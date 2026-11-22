//! Modified Nodal Analysis: assemble `A·x = z` and solve it (`CP-SIM-M0.3`,
//! sparse path `CP-SIM-M2.4`).
//!
//! Unknowns are the node voltages (ground excluded, it is the reference) plus one
//! branch current per voltage source — the "modified" part of MNA. Each device
//! **stamps** its contribution into `A`/`z`; the solve is a Gaussian elimination
//! with partial pivoting. Partial pivoting is not optional here: a voltage
//! source's branch row has a zero on the diagonal, so a naive pivot-free
//! elimination would divide by zero.
//!
//! One [`System`], two backings behind one stamp API. The stamp methods and all
//! their callers are oblivious to which is in play; [`System::new`] picks on size:
//!
//! * **Dense** (below [`SPARSE_THRESHOLD`]) — a row-major `N×N` array solved by
//!   the original dense Gaussian elimination. Cache-friendly and byte-for-byte
//!   what every small circuit got before, so nothing small regresses.
//! * **Sparse** (at or above the threshold) — each row a sorted map of its
//!   nonzeros, solved by a right-looking sparse LU with partial pivoting. The
//!   matrix is *never* materialized dense, so assembly is `O(nnz)` and, on a
//!   sparse topology (a long resistor ladder) where fill-in stays bounded, the
//!   factorization is near-linear — the win over the dense cube.
//!
//! The sparse solve performs the *same* elimination the dense one does — the
//! same partial-pivot choice and the same floating-point row combinations, merely
//! skipping the structural zeros (dense's `a −= factor·0` is a no-op that changes
//! no bits) — so for a non-degenerate matrix (no exact pivot-magnitude tie) the
//! two agree to the last bit. The `sparse_matches_dense` correctness tests assert
//! that; the `sparse_vs_dense` bench measures the scaling.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

/// Unknown count at or above which [`System::new`] assembles into the sparse
/// backing. Chosen well above every hand-built test circuit so small, human-scale
/// solves keep the dense backing (and their exact prior results), while the large
/// ladders the sparse path targets clear it comfortably.
pub const SPARSE_THRESHOLD: usize = 64;

/// Unknown count at or above which the engine's repeated-solve hot loops route
/// through the fill-reducing, reusable ordered LU ([`Symbolic`] / [`Numeric`] via
/// [`ReusableSolver`], `CP-SIM-M5.1`) instead of the natural-order sparse solve.
///
/// **Why this exact value.** The natural-order dense and sparse backings are
/// bit-for-bit identical to each other (and the sparse one is what every circuit
/// in `[SPARSE_THRESHOLD, KLU_THRESHOLD)` gets); the ordered path reorders the
/// elimination and so matches them only to a few ULP (≤1e-12), not to the bit. The
/// golden determinism suite and `large_ladder_uses_sparse_and_matches_dense` pin
/// results bit-for-bit, and the largest circuit any of them runs through
/// `Engine::run` is a 200-rung ladder — MNA size 202. Setting the threshold at
/// **256** keeps every one of those below it (so they stay on the exact
/// natural-order path and do not move a bit), while genuinely large nets — the
/// thousands-of-node meshes and hubs where natural order fills catastrophically —
/// clear it and get the near-linear ordered solve with its reusable symbolic
/// factorization. Raising it only ever keeps more nets on the slower exact path;
/// lowering it below 203 would move a golden result and break the bit-for-bit gate.
pub const KLU_THRESHOLD: usize = 256;

/// The coefficient-matrix store behind a [`System`]: a dense array, or one sorted
/// nonzero map per row. The right-hand side `z` is always dense (length `size`).
#[derive(Clone)]
enum Backing {
    /// Row-major `size × size` array.
    Dense(Vec<f64>),
    /// Per-row sorted map `column → value` of the structural nonzeros.
    Sparse(Vec<BTreeMap<usize, f64>>),
}

/// A linear system `A·x = z` under construction via stamps. The matrix lives in a
/// [`Backing`] chosen by size; the right-hand side `z` is dense.
#[derive(Clone)]
pub struct System {
    backing: Backing,
    /// Right-hand side, length `size`.
    z: Vec<f64>,
    size: usize,
}

impl System {
    /// A zeroed `size × size` system, with the backing chosen by size: dense below
    /// [`SPARSE_THRESHOLD`], sparse at or above it. This is what the engine calls;
    /// [`dense`](Self::dense) / [`sparse`](Self::sparse) force a backing for tests
    /// and benches.
    pub fn new(size: usize) -> Self {
        if size >= SPARSE_THRESHOLD {
            Self::sparse(size)
        } else {
            Self::dense(size)
        }
    }

    /// A zeroed `size × size` system forced to the dense backing.
    pub fn dense(size: usize) -> Self {
        Self {
            backing: Backing::Dense(vec![0.0; size * size]),
            z: vec![0.0; size],
            size,
        }
    }

    /// A zeroed `size × size` system forced to the sparse backing.
    pub fn sparse(size: usize) -> Self {
        Self {
            backing: Backing::Sparse(vec![BTreeMap::new(); size]),
            z: vec![0.0; size],
            size,
        }
    }

    /// A mutable handle to the entry at (`r`, `c`), creating it (as `0.0`) if the
    /// sparse backing does not yet hold it — so every stamp reads and writes the
    /// same cell regardless of backing, and the two assemble bit-identical matrices.
    #[inline]
    fn at(&mut self, r: usize, c: usize) -> &mut f64 {
        let n = self.size;
        match &mut self.backing {
            Backing::Dense(a) => &mut a[r * n + c],
            Backing::Sparse(rows) => rows[r].entry(c).or_insert(0.0),
        }
    }

    /// Stamp a conductance `g` between optional nodes `i` and `j` (`None` = ground).
    pub fn stamp_conductance(&mut self, i: Option<usize>, j: Option<usize>, g: f64) {
        if let Some(i) = i {
            *self.at(i, i) += g;
        }
        if let Some(j) = j {
            *self.at(j, j) += g;
        }
        if let (Some(i), Some(j)) = (i, j) {
            *self.at(i, j) -= g;
            *self.at(j, i) -= g;
        }
    }

    /// Add `val` to the matrix entry at (`row`, `col`); a ground index (`None`)
    /// is the reference and is dropped. This is the raw, possibly *asymmetric*
    /// entry stamp a multiport nonlinear device uses for its Jacobian (a BJT or
    /// MOSFET transconductance is not symmetric); the two-terminal, symmetric
    /// [`stamp_conductance`](Self::stamp_conductance) is the special case.
    pub fn stamp_matrix(&mut self, row: Option<usize>, col: Option<usize>, val: f64) {
        if let (Some(r), Some(c)) = (row, col) {
            *self.at(r, c) += val;
        }
    }

    /// Stamp a current source of `amps` flowing from node `j` into node `i`.
    pub fn stamp_current(&mut self, i: Option<usize>, j: Option<usize>, amps: f64) {
        if let Some(i) = i {
            self.z[i] += amps;
        }
        if let Some(j) = j {
            self.z[j] -= amps;
        }
    }

    /// Stamp a voltage source of `volts` from `plus` to `minus`, using branch row
    /// `k` (an absolute index into the system, past the node block).
    pub fn stamp_vsource(
        &mut self,
        plus: Option<usize>,
        minus: Option<usize>,
        k: usize,
        volts: f64,
    ) {
        if let Some(i) = plus {
            *self.at(i, k) += 1.0;
            *self.at(k, i) += 1.0;
        }
        if let Some(j) = minus {
            *self.at(j, k) -= 1.0;
            *self.at(k, j) -= 1.0;
        }
        self.z[k] += volts;
    }

    /// A row-major `size × size` dense copy of the assembled coefficient matrix,
    /// materializing the sparse backing's structural nonzeros into a full array.
    /// The small-signal pole/zero analysis (`CP-SIM-M7.1`) uses it to pull the
    /// linearized conductance matrix `G` out through the ordinary stamp API rather
    /// than re-deriving every device's contribution by hand.
    pub(crate) fn dense_matrix(&self) -> Vec<f64> {
        match &self.backing {
            Backing::Dense(a) => a.clone(),
            Backing::Sparse(rows) => {
                let n = self.size;
                let mut m = vec![0.0; n * n];
                for (r, row) in rows.iter().enumerate() {
                    for (&c, &v) in row {
                        m[r * n + c] = v;
                    }
                }
                m
            }
        }
    }

    /// Solve `A·x = z`, consuming the system. Returns `None` if `A` is singular.
    ///
    /// Runs the algorithm matching the backing [`new`](Self::new) chose: dense
    /// Gaussian elimination for the dense store, the sparse LU for the sparse one.
    /// The two agree to the last bit on a non-degenerate matrix, so the backing is
    /// purely a speed choice — small systems stay exactly as they were.
    pub fn solve(self) -> Option<Vec<f64>> {
        match self.backing {
            Backing::Dense(a) => Self::solve_dense(a, self.z, self.size),
            Backing::Sparse(rows) => Self::solve_sparse(rows, self.z, self.size),
        }
    }

    /// Consume this assembled system into the `(SparseMatrix, rhs)` pair the
    /// fill-reducing ordered LU ([`Symbolic`] / [`Numeric`]) consumes — the bridge
    /// the engine's [`ReusableSolver`] crosses above [`KLU_THRESHOLD`] (`CP-SIM-M5.1`).
    /// A sparse backing hands its row maps straight over (a move, no rebuild); a
    /// dense backing (only reachable if something forced one at a KLU-scale size)
    /// is condensed to its structural nonzeros. The matrix structure is identical
    /// to what the natural-order solve would have eliminated, so the ordered result
    /// agrees with it to a few ULP.
    pub(crate) fn into_ordered_parts(self) -> (SparseMatrix, Vec<f64>) {
        let n = self.size;
        let rows = match self.backing {
            Backing::Sparse(rows) => rows,
            Backing::Dense(a) => {
                let mut rows = vec![BTreeMap::new(); n];
                for (r, row) in rows.iter_mut().enumerate() {
                    for c in 0..n {
                        let v = a[r * n + c];
                        if v != 0.0 {
                            row.insert(c, v);
                        }
                    }
                }
                rows
            }
        };
        (SparseMatrix { n, rows }, self.z)
    }

    /// Dense Gaussian elimination with partial pivoting — the original `O(N³)`
    /// solve. Returns `None` if `A` is singular.
    fn solve_dense(mut a: Vec<f64>, mut z: Vec<f64>, n: usize) -> Option<Vec<f64>> {
        if n == 0 {
            return Some(Vec::new());
        }
        // Forward elimination with partial pivoting.
        for col in 0..n {
            // Pick the pivot row: largest magnitude in this column at or below col.
            let mut pivot = col;
            let mut best = a[col * n + col].abs();
            for r in (col + 1)..n {
                let v = a[r * n + col].abs();
                if v > best {
                    best = v;
                    pivot = r;
                }
            }
            if best < 1e-15 {
                return None; // singular / underdetermined
            }
            if pivot != col {
                for c in 0..n {
                    a.swap(pivot * n + c, col * n + c);
                }
                z.swap(pivot, col);
            }
            // Eliminate below.
            let piv = a[col * n + col];
            for r in (col + 1)..n {
                let factor = a[r * n + col] / piv;
                if factor == 0.0 {
                    continue;
                }
                for c in col..n {
                    a[r * n + c] -= factor * a[col * n + c];
                }
                z[r] -= factor * z[col];
            }
        }
        // Back substitution.
        let mut x = vec![0.0; n];
        for row in (0..n).rev() {
            let base = row * n;
            let sum = z[row]
                - a[base + row + 1..base + n]
                    .iter()
                    .zip(&x[row + 1..n])
                    .map(|(a, xc)| a * xc)
                    .sum::<f64>();
            x[row] = sum / a[row * n + row];
        }
        Some(x)
    }

    /// Right-looking sparse LU with partial pivoting. Returns `None` if `A` is
    /// singular. Numerically this is the *same* elimination as
    /// [`solve_dense`](Self::solve_dense) — the same pivot choice (largest column
    /// magnitude among rows not yet used as pivots) and the same row
    /// combinations `row_r −= (a[r][c]/pivot)·row_pivot` — but it stores each row
    /// as a sorted list of its nonzeros, so it touches only the structural
    /// nonzeros and leaves the (implicit) zeros alone. Dense's `a[r][c] −=
    /// factor·0` is a no-op that changes nothing, so skipping it changes no bits:
    /// absent an exact pivot-magnitude tie the two solvers return identical `x`.
    ///
    /// The row permutation from pivoting is kept implicitly in `pivot_of_col`
    /// (which row is the pivot for each column) rather than by physically moving
    /// rows, and `col_rows` indexes, per column, the rows that currently hold a
    /// nonzero there — so a pivot search scans only the handful of rows actually
    /// present in the column instead of the whole matrix. On a bounded-fill
    /// topology (a resistor ladder, where elimination creates `O(1)` fill per
    /// column) the factorization is near-linear.
    ///
    /// The matrix is never materialized dense: assembly is `O(nnz)` and the whole
    /// solve is near-linear on a sparse topology, against the dense `O(N³)` (or
    /// `O(N²)` once dense's zero-skip kicks in on a sparse matrix) — the scaling
    /// win the bench measures.
    fn solve_sparse(
        sparse_rows: Vec<BTreeMap<usize, f64>>,
        mut z: Vec<f64>,
        n: usize,
    ) -> Option<Vec<f64>> {
        if n == 0 {
            return Some(Vec::new());
        }

        // Each row's nonzeros as a column-ascending list (BTreeMap iterates sorted),
        // the shape the elimination wants.
        let mut rows: Vec<Vec<(usize, f64)>> = sparse_rows
            .into_iter()
            .map(|m| m.into_iter().collect())
            .collect();

        // col_rows[c]: every row that currently holds a nonzero in column c. Each
        // (row, col) pair appears at most once — fill only ever pushes a row the
        // first time it gains an entry in that column.
        let mut col_rows: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (r, row) in rows.iter().enumerate() {
            for &(c, _) in row {
                col_rows[c].push(r);
            }
        }

        // Which rows have already served as a pivot (so are frozen), and the
        // pivot row chosen for each column — the implicit row permutation.
        let mut used = vec![false; n];
        let mut pivot_of_col = vec![usize::MAX; n];

        // The value a row currently holds at a column (0.0 if absent).
        let value_at = |rows: &[Vec<(usize, f64)>], r: usize, c: usize| -> f64 {
            match rows[r].binary_search_by(|&(cc, _)| cc.cmp(&c)) {
                Ok(i) => rows[r][i].1,
                Err(_) => 0.0,
            }
        };

        // Forward elimination, column by column.
        for c in 0..n {
            // Partial pivot: the unused row with the largest magnitude at column c.
            // Strict `>` keeps the earliest such row on a tie, as the dense scan does.
            let mut pivot_row = usize::MAX;
            let mut best = 0.0_f64;
            for &r in &col_rows[c] {
                if used[r] {
                    continue;
                }
                let v = value_at(&rows, r, c).abs();
                if v > best {
                    best = v;
                    pivot_row = r;
                }
            }
            if best < 1e-15 || pivot_row == usize::MAX {
                return None; // singular / underdetermined
            }
            used[pivot_row] = true;
            pivot_of_col[c] = pivot_row;

            let pivot_val = value_at(&rows, pivot_row, c);
            let pivot_z = z[pivot_row];
            // The pivot row's upper part (columns past c) drives every elimination
            // below; snapshot it so the borrow of `rows` is released for mutation.
            let pivot_upper: Vec<(usize, f64)> = rows[pivot_row]
                .iter()
                .copied()
                .filter(|&(cc, _)| cc > c)
                .collect();

            // Rows to eliminate: the other unused rows with a nonzero at column c.
            let targets: Vec<usize> = col_rows[c]
                .iter()
                .copied()
                .filter(|&r| r != pivot_row && !used[r])
                .collect();

            for r in targets {
                let Ok(idx_c) = rows[r].binary_search_by(|&(cc, _)| cc.cmp(&c)) else {
                    continue; // no live entry at c (a cancelled fill) — nothing to do
                };
                let factor = rows[r][idx_c].1 / pivot_val;
                // The (r, c) entry is eliminated to zero — drop it.
                rows[r].remove(idx_c);
                if factor == 0.0 {
                    continue;
                }
                // row_r −= factor · pivot_row, over the pivot's upper columns.
                for &(cj, pv) in &pivot_upper {
                    match rows[r].binary_search_by(|&(cc, _)| cc.cmp(&cj)) {
                        Ok(i) => rows[r][i].1 -= factor * pv,
                        Err(i) => {
                            rows[r].insert(i, (cj, -factor * pv));
                            col_rows[cj].push(r); // new fill-in
                        }
                    }
                }
                z[r] -= factor * pivot_z;
            }
        }

        // Back substitution. The unknown for column c is x[c]; its equation is the
        // frozen pivot row `pivot_of_col[c]`, whose leading column is exactly c.
        //
        // To stay bit-identical to the dense solver, reduce in its exact order:
        // accumulate the product sum `Σ a[c][j]·x[j]` over `j > c` in ascending
        // column order (the stored order), *then* subtract once from `z` — not
        // `z` minus each product in turn, which would round differently.
        let mut x = vec![0.0; n];
        for c in (0..n).rev() {
            let p = pivot_of_col[c];
            let mut diag = 0.0;
            let mut acc = 0.0;
            for &(cc, v) in &rows[p] {
                match cc.cmp(&c) {
                    std::cmp::Ordering::Equal => diag = v,
                    std::cmp::Ordering::Greater => acc += v * x[cc],
                    std::cmp::Ordering::Less => {}
                }
            }
            x[c] = (z[p] - acc) / diag;
        }
        Some(x)
    }
}

// ===========================================================================
// KLU-class sparse: fill-reducing ordering + reusable symbolic factorization
// (CP-SIM-M4.2)
// ===========================================================================
//
// The `System::solve_sparse` above eliminates in *natural* column order and folds
// the right-hand side into one pass — correct and bit-for-bit matching the dense
// solve, but on a badly-numbered matrix (a shared rail / "arrow" whose hub is
// eliminated first, a 2-D grid) that order creates catastrophic fill, so the
// factorization degrades toward the dense cube. It also throws the factors away:
// a transient or sweep that solves the *same structure* with new numbers every
// step pays the full pivot search and symbolic bookkeeping again and again.
//
// This section adds the KLU-class answer. [`SparseMatrix`] holds the coefficient
// pattern; [`Symbolic::analyze`] computes a **fill-reducing minimum-degree
// ordering** once (so the hub is eliminated last, the grid cut cheaply);
// [`Symbolic::factor`] runs a right-looking LU with partial pivoting *in that
// order*, recording the pivot sequence, the triangular factors L and U, and the
// fill pattern into a [`Numeric`]. [`Numeric::solve`] is then a cheap forward/back
// substitution for any right-hand side, and [`Numeric::refactor`] **reuses the
// symbolic factorization** — the ordering, the pivot sequence, and the fill — to
// recompute only the numbers for a matrix of the same structure, which is exactly
// what a transient step or a sweep point needs (a numeric refactor, no re-analysis
// and no pivot search). It is numerically the same LU as the dense solve, merely
// reordered, so it agrees with it to a few ULP (the `ordered_matches_dense` tests
// hold it to ≤1e-12); the `ordered_vs_natural` bench measures the scaling win.

/// A sparse coefficient matrix in row-map form — the input to the reusable ordered
/// LU ([`Symbolic`] / [`Numeric`], `CP-SIM-M4.2`). Mirrors the stamp API of the
/// engine's `System` (conductance / raw entry / voltage-source coupling) so a
/// caller assembles it exactly as it would the engine's system; the right-hand
/// side is
/// kept separately and handed to [`Numeric::solve`].
pub struct SparseMatrix {
    n: usize,
    rows: Vec<BTreeMap<usize, f64>>,
}

impl SparseMatrix {
    /// A zeroed `n × n` sparse matrix.
    pub fn new(n: usize) -> Self {
        Self {
            n,
            rows: vec![BTreeMap::new(); n],
        }
    }

    /// Accumulate `val` into entry (`r`, `c`).
    pub fn add(&mut self, r: usize, c: usize, val: f64) {
        *self.rows[r].entry(c).or_insert(0.0) += val;
    }

    /// Stamp a conductance `g` between optional nodes `i`/`j` (`None` = ground),
    /// identical to `System::stamp_conductance`.
    pub fn stamp_conductance(&mut self, i: Option<usize>, j: Option<usize>, g: f64) {
        if let Some(i) = i {
            self.add(i, i, g);
        }
        if let Some(j) = j {
            self.add(j, j, g);
        }
        if let (Some(i), Some(j)) = (i, j) {
            self.add(i, j, -g);
            self.add(j, i, -g);
        }
    }

    /// Stamp the matrix coupling of a voltage source from `plus`/`minus` onto
    /// branch row `k` (the `volts` right-hand side is the caller's, as in
    /// `System::stamp_vsource`).
    pub fn stamp_vsource(&mut self, plus: Option<usize>, minus: Option<usize>, k: usize) {
        if let Some(i) = plus {
            self.add(i, k, 1.0);
            self.add(k, i, 1.0);
        }
        if let Some(j) = minus {
            self.add(j, k, -1.0);
            self.add(k, j, -1.0);
        }
    }
}

/// The reusable symbolic factorization (`CP-SIM-M4.2`): a fill-reducing ordering
/// of a matrix's structure, computed once and reused across every numeric
/// factorization of a matrix with that same pattern.
pub struct Symbolic {
    n: usize,
    /// `perm[new] = old` — the fill-reducing elimination order, applied
    /// symmetrically (rows and columns both reordered by it).
    perm: Vec<usize>,
    /// `iperm[old] = new` — the inverse of `perm`.
    iperm: Vec<usize>,
}

impl Symbolic {
    /// Analyze `mat`'s nonzero structure and compute a minimum-degree fill-reducing
    /// ordering. Depends only on the pattern, so the result is reused for every
    /// numeric factorization of a same-structured matrix.
    pub fn analyze(mat: &SparseMatrix) -> Symbolic {
        let perm = min_degree_order(&mat.rows, mat.n);
        let mut iperm = vec![0usize; mat.n];
        for (newi, &old) in perm.iter().enumerate() {
            iperm[old] = newi;
        }
        Symbolic {
            n: mat.n,
            perm,
            iperm,
        }
    }

    /// Numerically factor `mat` (which must share this ordering's structure) with
    /// partial pivoting *within the fill-reducing order*, returning the reusable
    /// [`Numeric`] factors, or `None` if the matrix is singular.
    pub fn factor(&self, mat: &SparseMatrix) -> Option<Numeric> {
        self.run(mat, None)
    }

    /// The shared right-looking elimination. `forced = None` searches for the
    /// partial pivot and records it; `forced = Some(seq)` reuses a recorded pivot
    /// sequence (the refactor path), failing with `None` if a reused pivot has
    /// collapsed to ~0 so the caller can fall back to a fresh [`factor`](Self::factor).
    fn run(&self, mat: &SparseMatrix, forced: Option<&[usize]>) -> Option<Numeric> {
        let n = self.n;
        // The matrix in permuted coordinates: new row i is old row perm[i], its
        // columns remapped through iperm, kept column-ascending.
        let mut rows: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
        for (newi, row) in rows.iter_mut().enumerate() {
            let old = self.perm[newi];
            let mut v: Vec<(usize, f64)> = mat.rows[old]
                .iter()
                .map(|(&oc, &val)| (self.iperm[oc], val))
                .collect();
            v.sort_by_key(|&(c, _)| c);
            *row = v;
        }

        let mut col_rows: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (r, row) in rows.iter().enumerate() {
            for &(c, _) in row {
                col_rows[c].push(r);
            }
        }
        let mut used = vec![false; n];
        let mut pivot_of_col = vec![usize::MAX; n];
        let mut l: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
        let mut u: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];

        for c in 0..n {
            // Pick the pivot: a recorded one for refactor, else the largest column
            // magnitude among unused rows (partial pivoting, as the dense solve).
            let pivot_row = match forced {
                Some(seq) => {
                    let p = seq[c];
                    if p >= n || used[p] || value_at(&rows, p, c).abs() < 1e-15 {
                        return None; // a reused pivot collapsed — re-analyze/factor
                    }
                    p
                }
                None => {
                    let mut pr = usize::MAX;
                    let mut best = 0.0_f64;
                    for &r in &col_rows[c] {
                        if used[r] {
                            continue;
                        }
                        let v = value_at(&rows, r, c).abs();
                        if v > best {
                            best = v;
                            pr = r;
                        }
                    }
                    if best < 1e-15 || pr == usize::MAX {
                        return None; // singular / underdetermined
                    }
                    pr
                }
            };
            used[pivot_row] = true;
            pivot_of_col[c] = pivot_row;

            let pivot_val = value_at(&rows, pivot_row, c);
            // U row c: the pivot row's entries at columns ≥ c (diagonal + upper).
            let urow: Vec<(usize, f64)> = rows[pivot_row]
                .iter()
                .copied()
                .filter(|&(cc, _)| cc >= c)
                .collect();
            let pivot_upper: Vec<(usize, f64)> =
                urow.iter().copied().filter(|&(cc, _)| cc > c).collect();
            u[c] = urow;

            let targets: Vec<usize> = col_rows[c]
                .iter()
                .copied()
                .filter(|&r| r != pivot_row && !used[r])
                .collect();
            let mut lcol: Vec<(usize, f64)> = Vec::with_capacity(targets.len());
            for r in targets {
                let Ok(idx_c) = rows[r].binary_search_by(|&(cc, _)| cc.cmp(&c)) else {
                    continue; // a cancelled fill left no live entry at c
                };
                let factor = rows[r][idx_c].1 / pivot_val;
                rows[r].remove(idx_c);
                lcol.push((r, factor)); // L multiplier, recorded for reuse
                if factor == 0.0 {
                    continue;
                }
                for &(cj, pv) in &pivot_upper {
                    match rows[r].binary_search_by(|&(cc, _)| cc.cmp(&cj)) {
                        Ok(i) => rows[r][i].1 -= factor * pv,
                        Err(i) => {
                            rows[r].insert(i, (cj, -factor * pv));
                            col_rows[cj].push(r); // new fill
                        }
                    }
                }
            }
            l[c] = lcol;
        }

        Some(Numeric {
            n,
            perm: self.perm.clone(),
            pivot_of_col,
            l,
            u,
        })
    }
}

/// The reusable numeric LU factors of one matrix in a [`Symbolic`] ordering
/// (`CP-SIM-M4.2`). [`solve`](Self::solve) applies them to any right-hand side;
/// [`refactor`](Self::refactor) recomputes them for a same-structured matrix
/// without re-analyzing or re-searching pivots.
pub struct Numeric {
    n: usize,
    /// `perm[new] = old`, carried so [`solve`](Self::solve) can (un)permute.
    perm: Vec<usize>,
    /// The pivot (permuted) row chosen for each permuted column — the reusable
    /// pivot sequence.
    pivot_of_col: Vec<usize>,
    /// Per column: the `(row, multiplier)` entries of L below the pivot.
    l: Vec<Vec<(usize, f64)>>,
    /// Per column: the pivot row's entries at columns ≥ that column (U, diagonal
    /// first in sort order).
    u: Vec<Vec<(usize, f64)>>,
}

impl Numeric {
    /// Solve `A·x = rhs` from the stored factors — permute the right-hand side,
    /// forward-substitute through L, back-substitute through U, unpermute. The same
    /// reduction order the dense solver uses, so the result matches it to a few ULP.
    pub fn solve(&self, rhs: &[f64]) -> Vec<f64> {
        let n = self.n;
        let mut bp = vec![0.0; n];
        for i in 0..n {
            bp[i] = rhs[self.perm[i]];
        }
        // Forward substitution: apply L column by column (the elimination order).
        for c in 0..n {
            let piv_b = bp[self.pivot_of_col[c]];
            for &(r, factor) in &self.l[c] {
                bp[r] -= factor * piv_b;
            }
        }
        // Back substitution: x[c] = (b[pivot] − Σ_{j>c} U[c][j]·x[j]) / diag.
        let mut x = vec![0.0; n];
        for c in (0..n).rev() {
            let mut diag = 0.0;
            let mut acc = 0.0;
            for &(cc, val) in &self.u[c] {
                if cc == c {
                    diag = val;
                } else {
                    acc += val * x[cc];
                }
            }
            x[c] = (bp[self.pivot_of_col[c]] - acc) / diag;
        }
        let mut out = vec![0.0; n];
        for i in 0..n {
            out[self.perm[i]] = x[i];
        }
        out
    }

    /// Reuse this factorization's symbolic structure — ordering, pivot sequence,
    /// fill — to recompute the numbers for `mat`, which must share the original
    /// structure. Only the numeric refactor runs: no re-analysis, no pivot search.
    /// Returns `None` if a reused pivot has collapsed (the caller then re-analyzes
    /// and factors afresh).
    pub fn refactor(&mut self, sym: &Symbolic, mat: &SparseMatrix) -> Option<()> {
        let fresh = sym.run(mat, Some(&self.pivot_of_col))?;
        *self = fresh;
        Some(())
    }
}

/// The engine-facing linear solver for a repeated-structure hot loop — the
/// Newton–Raphson iterations of one solve, the steps of a transient, the points of
/// a sweep (`CP-SIM-M5.1`). It owns the strategy choice and the reuse:
///
/// * **Below [`KLU_THRESHOLD`]** it is a pass-through — each assembled [`System`] is
///   solved by its own backing exactly as before ([`System::solve`]), so every
///   small / human-scale circuit keeps its current result *bit-for-bit*. This is
///   the overwhelming common case and costs nothing.
/// * **At or above [`KLU_THRESHOLD`]** the first solve runs the fill-reducing
///   [`Symbolic::analyze`] once and [`Symbolic::factor`], caching both; every later
///   solve of the *same structure* only [`Numeric::refactor`]s (no re-ordering, no
///   pivot search) and back-solves. A collapsed pivot (a reused pivot gone ~0 as
///   the numbers moved) transparently re-analyzes and re-factors, so correctness
///   never depends on the reuse holding.
///
/// All matrices a single solver is fed **must share one structure** — the caller
/// guarantees it by giving each structurally-distinct context (a gmin-stepping
/// continuation, a transient's `t = 0` solve vs. its stepping solves) its own
/// solver. That is exactly how the engine drives it.
pub(crate) struct ReusableSolver {
    /// Whether to take the ordered KLU path (size ≥ [`KLU_THRESHOLD`]).
    ordered: bool,
    /// The cached fill-reducing ordering, once the first ordered solve computed it.
    sym: Option<Symbolic>,
    /// The cached numeric factors, refactored in place on every later solve.
    num: Option<Numeric>,
}

impl ReusableSolver {
    /// A solver for systems of `size` unknowns: the ordered reusable path at or
    /// above [`KLU_THRESHOLD`], else a bit-for-bit pass-through to [`System::solve`].
    pub(crate) fn new(size: usize) -> Self {
        Self {
            ordered: size >= KLU_THRESHOLD,
            sym: None,
            num: None,
        }
    }

    /// Solve one assembled system, consuming it. On the ordered path this reuses
    /// the cached symbolic factorization (analyzing only on the first call or after
    /// a pivot collapse). Returns `None` if the matrix is singular.
    pub(crate) fn solve(&mut self, sys: System) -> Option<Vec<f64>> {
        if !self.ordered {
            return sys.solve();
        }
        let (mat, rhs) = sys.into_ordered_parts();
        // Reuse the cached ordering + pivot sequence when we have one and it still
        // holds for these numbers; otherwise analyze + factor afresh.
        if let (Some(sym), Some(num)) = (self.sym.as_ref(), self.num.as_mut()) {
            if num.refactor(sym, &mat).is_some() {
                return Some(num.solve(&rhs));
            }
        }
        let sym = Symbolic::analyze(&mat);
        let num = sym.factor(&mat)?;
        let x = num.solve(&rhs);
        self.sym = Some(sym);
        self.num = Some(num);
        Some(x)
    }
}

/// The value a permuted row currently holds at a column (0.0 if absent) — the
/// elimination's structural lookup.
fn value_at(rows: &[Vec<(usize, f64)>], r: usize, c: usize) -> f64 {
    match rows[r].binary_search_by(|&(cc, _)| cc.cmp(&c)) {
        Ok(i) => rows[r][i].1,
        Err(_) => 0.0,
    }
}

/// A minimum-degree fill-reducing ordering of a matrix's (symmetrized) nonzero
/// structure — the AMD-style heart of [`Symbolic::analyze`] (`CP-SIM-M4.2`).
///
/// The matrix graph has an edge between `i` and `j` wherever `A[i][j]` or `A[j][i]`
/// is structurally present (`i ≠ j`). The classic greedy heuristic repeatedly
/// eliminates the lowest-degree node — eliminating a node turns its neighbors into
/// a clique (the fill it creates) — so a hub shared by everything (the pathological
/// case for natural order) is eliminated *last*, when its neighbors are already
/// gone, and a long chain / ladder keeps its already-good order. A lazy
/// binary-heap keyed by current degree drives the selection; stale entries (a node
/// whose degree has since changed) are skipped on pop.
fn min_degree_order(rows: &[BTreeMap<usize, f64>], n: usize) -> Vec<usize> {
    let mut adj: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n];
    for (r, row) in rows.iter().enumerate() {
        for &c in row.keys() {
            if c != r {
                adj[r].insert(c);
                adj[c].insert(r);
            }
        }
    }
    min_degree_eliminate(adj)
}

/// The greedy minimum-degree elimination over a prebuilt symmetric adjacency — the
/// pattern-only heart of [`min_degree_order`], shared with the complex AC/noise
/// solver (`CP-SIM-M10.1`), whose adjacency is built from [`ohmforge_core::Complex`]
/// row maps. Taking the adjacency (not the valued rows) keeps the ordering purely
/// structural, so the real and complex paths share one proven heuristic. `adj[i]`
/// holds `i`'s neighbors (`j ≠ i` wherever the matrix couples them, symmetrized);
/// the result is the fill-reducing elimination order `perm[new] = old`.
pub(crate) fn min_degree_eliminate(mut adj: Vec<BTreeSet<usize>>) -> Vec<usize> {
    let n = adj.len();
    let mut eliminated = vec![false; n];
    let mut order: Vec<usize> = Vec::with_capacity(n);
    let mut heap: BinaryHeap<Reverse<(usize, usize)>> = BinaryHeap::new();
    for (v, a) in adj.iter().enumerate() {
        heap.push(Reverse((a.len(), v)));
    }

    while let Some(Reverse((d, v))) = heap.pop() {
        if eliminated[v] || d != adj[v].len() {
            continue; // already gone, or a stale (superseded) degree entry
        }
        eliminated[v] = true;
        order.push(v);

        let nbrs: Vec<usize> = adj[v].iter().copied().filter(|&u| !eliminated[u]).collect();
        for &u in &nbrs {
            adj[u].remove(&v);
        }
        // Eliminating v makes its surviving neighbors a clique (the induced fill).
        for i in 0..nbrs.len() {
            for j in (i + 1)..nbrs.len() {
                let (a, b) = (nbrs[i], nbrs[j]);
                adj[a].insert(b);
                adj[b].insert(a);
            }
        }
        for &u in &nbrs {
            heap.push(Reverse((adj[u].len(), u))); // re-key with the new degree
        }
    }
    // Any node never reached (fully isolated) trails in natural order.
    for (v, &e) in eliminated.iter().enumerate() {
        if !e {
            order.push(v);
        }
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solves_a_2x2() {
        // [2 1; 1 3] x = [3; 5]  ->  x = [4/5, 7/5]
        let mut s = System::new(2);
        *s.at(0, 0) = 2.0;
        *s.at(0, 1) = 1.0;
        *s.at(1, 0) = 1.0;
        *s.at(1, 1) = 3.0;
        s.z[0] = 3.0;
        s.z[1] = 5.0;
        let x = s.solve().unwrap();
        assert!((x[0] - 0.8).abs() < 1e-12);
        assert!((x[1] - 1.4).abs() < 1e-12);
    }

    #[test]
    fn pivots_through_a_zero_diagonal() {
        // [0 1; 1 0] x = [2; 3]  ->  x = [3; 2]  (needs a row swap)
        let mut s = System::new(2);
        *s.at(0, 1) = 1.0;
        *s.at(1, 0) = 1.0;
        s.z[0] = 2.0;
        s.z[1] = 3.0;
        let x = s.solve().unwrap();
        assert!((x[0] - 3.0).abs() < 1e-12);
        assert!((x[1] - 2.0).abs() < 1e-12);
    }

    #[test]
    fn reports_singular() {
        let mut s = System::new(2);
        *s.at(0, 0) = 1.0;
        *s.at(0, 1) = 1.0;
        *s.at(1, 0) = 1.0;
        *s.at(1, 1) = 1.0; // rank-deficient
        s.z[0] = 1.0;
        s.z[1] = 2.0;
        assert!(s.solve().is_none());
    }

    // --- Sparse solver equivalence + scaling — CP-SIM-M2.4 ------------------

    /// The MNA system of an `n`-rung resistor ladder: a 10 V source at node 0,
    /// then a series 1 kΩ into each rung node with a 1 kΩ shunt to ground. Size
    /// is `n + 2` (ladder nodes `0..=n` plus the source branch row). This is the
    /// canonical *sparse* topology — tridiagonal node block plus one branch row —
    /// the sparse solver is built to exploit.
    fn ladder_system(n: usize, make: fn(usize) -> System) -> System {
        let mut s = make(n + 2);
        let branch = n + 1;
        s.stamp_vsource(Some(0), None, branch, 10.0);
        let mut prev = 0usize;
        for i in 0..n {
            let node = i + 1;
            s.stamp_conductance(Some(prev), Some(node), 1e-3);
            s.stamp_conductance(Some(node), None, 1e-3);
            prev = node;
        }
        s
    }

    /// A small deterministic LCG, so the "dense random" fixtures are reproducible
    /// with no external dependency (keeps the crate deterministic + wasm-clean).
    struct Lcg(u64);
    impl Lcg {
        fn next_f64(&mut self) -> f64 {
            // Numerical-Recipes constants; take the top bits for a value in [-1, 1).
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
        }
    }

    /// A dense, strictly diagonally dominant `n × n` system (always solvable
    /// without pivoting) with a fixed seed — exercises the solve on a *fully
    /// populated* matrix, where every elimination fills, so the two backings' row
    /// combinations must still agree entry for entry.
    fn dense_random_system(n: usize, seed: u64, make: fn(usize) -> System) -> System {
        let mut s = make(n);
        let mut rng = Lcg(seed);
        for r in 0..n {
            let mut rowsum = 0.0;
            for c in 0..n {
                if r != c {
                    let v = rng.next_f64();
                    *s.at(r, c) = v;
                    rowsum += v.abs();
                }
            }
            // Diagonal dominates the row's off-diagonal mass → nonsingular.
            *s.at(r, r) = rowsum + 1.0 + rng.next_f64().abs();
            s.z[r] = rng.next_f64();
        }
        s
    }

    /// The acceptance correctness gate: the sparse backing solves identically to
    /// the dense one, bit for bit, across several matrices — resistor ladders at a
    /// few sizes (the sparse topology, each needing the branch-row pivot), fully
    /// dense random systems, and a zero-diagonal system that forces row swaps.
    #[test]
    fn sparse_matches_dense_bit_for_bit() {
        // Resistor ladders of several sizes — the sparse topology.
        for &n in &[70usize, 200, 500] {
            let dense = ladder_system(n, System::dense)
                .solve()
                .expect("dense ladder solves");
            let sparse = ladder_system(n, System::sparse)
                .solve()
                .expect("sparse ladder solves");
            assert_eq!(dense, sparse, "ladder n={n}: sparse diverged from dense");
            // Sanity: the source pins node 0 to exactly 10 V, and every rung node
            // is a positive, strictly attenuated voltage.
            assert_eq!(sparse[0], 10.0, "ladder n={n}: v(vin) not 10 V");
            for w in sparse[1..=n].windows(2) {
                assert!(w[0] > w[1] && w[1] > 0.0, "ladder n={n} not monotone");
            }
        }

        // Fully dense random systems: every row fills during elimination.
        for &seed in &[1u64, 12345, 0xDEAD_BEEF] {
            let dense = dense_random_system(80, seed, System::dense)
                .solve()
                .unwrap();
            let sparse = dense_random_system(80, seed, System::sparse)
                .solve()
                .unwrap();
            assert_eq!(dense, sparse, "dense_random seed={seed}: mismatch");
        }
    }

    /// The sparse backing reproduces the dense solver's partial pivoting on a
    /// large zero-diagonal system — block-diagonal `[0 1; 1 0]` pairs, each
    /// needing a row swap — above the dispatch threshold so a real circuit hitting
    /// it is covered too.
    #[test]
    fn sparse_handles_pivoting_at_scale() {
        let pairs = 40; // size 80 ≥ SPARSE_THRESHOLD
        let build = |make: fn(usize) -> System| {
            let mut s = make(2 * pairs);
            for p in 0..pairs {
                let (i, j) = (2 * p, 2 * p + 1);
                *s.at(i, j) = 1.0;
                *s.at(j, i) = 1.0;
                s.z[i] = (p as f64) + 2.0;
                s.z[j] = (p as f64) + 3.0;
            }
            s
        };
        let dense = build(System::dense).solve().unwrap();
        let sparse = build(System::sparse).solve().unwrap();
        assert_eq!(dense, sparse, "zero-diagonal pivoting diverged");
        // Each pair swaps its two unknowns: x[i] = z[j], x[j] = z[i].
        for p in 0..pairs {
            assert_eq!(sparse[2 * p], (p as f64) + 3.0);
            assert_eq!(sparse[2 * p + 1], (p as f64) + 2.0);
        }
    }

    /// A singular large system is reported `None` by the sparse backing just as
    /// the dense one does — here a 100-rung ladder with one extra, wholly detached
    /// unknown (an all-zero row and column) leaves a pivotless column.
    #[test]
    fn sparse_reports_singular() {
        let n = 100;
        let build = |make: fn(usize) -> System| {
            // One unknown past the ladder's `n + 2` is never stamped, so its row
            // and column stay all-zero → no pivot → singular.
            let mut s = make(n + 3);
            let branch = n + 1;
            s.stamp_vsource(Some(0), None, branch, 10.0);
            let mut prev = 0usize;
            for i in 0..n {
                let node = i + 1;
                s.stamp_conductance(Some(prev), Some(node), 1e-3);
                s.stamp_conductance(Some(node), None, 1e-3);
                prev = node;
            }
            s
        };
        assert!(
            build(System::dense).solve().is_none(),
            "dense should flag singular"
        );
        assert!(
            build(System::sparse).solve().is_none(),
            "sparse should flag singular"
        );
    }

    /// The sparse backing also works below the dispatch threshold (so the
    /// equivalence is a property of the algorithm, not the size): forced sparse,
    /// it reproduces the pivoting 2×2 the dense solver handles.
    #[test]
    fn sparse_solves_small_pivoting_case() {
        let mut s = System::sparse(2);
        *s.at(0, 1) = 1.0;
        *s.at(1, 0) = 1.0;
        s.z[0] = 2.0;
        s.z[1] = 3.0;
        let x = s.solve().unwrap();
        assert_eq!(x, vec![3.0, 2.0]);
    }

    // --- Fill-reducing ordered reusable LU — CP-SIM-M4.2 --------------------

    /// A conductance list — `(node_i, node_j, g)`, `None` = ground — the fixtures
    /// below stamp into both a dense reference and a [`SparseMatrix`].
    type Conds = Vec<(Option<usize>, Option<usize>, f64)>;

    /// Conductance list + right-hand side for an "arrow"/hub network: node 0 is a
    /// hub joined by `g` to each of `n` leaves (nodes `1..=n`), every leaf also tied
    /// to ground by `g`, and a probe current injected at the hub. Numbering the hub
    /// first is the worst case for natural-order fill; min-degree eliminates it last.
    fn arrow(n: usize) -> (Conds, Vec<f64>, usize) {
        let size = n + 1;
        let g = 1e-3;
        let mut conds = Vec::new();
        for leaf in 1..=n {
            conds.push((Some(0usize), Some(leaf), g)); // hub ↔ leaf
            conds.push((Some(leaf), None, g)); // leaf ↔ ground
        }
        let mut rhs = vec![0.0; size];
        rhs[0] = 1e-3; // inject 1 mA at the hub
        (conds, rhs, size)
    }

    /// Conductance list + RHS for an `m × m` resistor grid: each node joined to its
    /// right and down neighbor by `g`, every node leaked to ground by `g` (so the
    /// system is nonsingular), and a probe current at a corner. The 2-D structure is
    /// the case where a fill-reducing order beats natural row-major numbering.
    fn grid(m: usize) -> (Conds, Vec<f64>, usize) {
        let size = m * m;
        let g = 1e-3;
        let idx = |r: usize, c: usize| r * m + c;
        let mut conds = Vec::new();
        for r in 0..m {
            for c in 0..m {
                let node = idx(r, c);
                conds.push((Some(node), None, g)); // leak to ground
                if c + 1 < m {
                    conds.push((Some(node), Some(idx(r, c + 1)), g));
                }
                if r + 1 < m {
                    conds.push((Some(node), Some(idx(r + 1, c)), g));
                }
            }
        }
        let mut rhs = vec![0.0; size];
        rhs[0] = 1e-3;
        (conds, rhs, size)
    }

    /// Solve a conductance list + RHS with the dense reference solver.
    fn dense_ref(conds: &Conds, rhs: &[f64], n: usize) -> Vec<f64> {
        let mut s = System::dense(n);
        for &(i, j, g) in conds {
            s.stamp_conductance(i, j, g);
        }
        s.z.copy_from_slice(rhs);
        s.solve().expect("dense reference solves")
    }

    /// Assemble a [`SparseMatrix`] from a conductance list.
    fn sparse_mat(conds: &Conds, n: usize) -> SparseMatrix {
        let mut m = SparseMatrix::new(n);
        for &(i, j, g) in conds {
            m.stamp_conductance(i, j, g);
        }
        m
    }

    fn max_abs_diff(a: &[f64], b: &[f64]) -> f64 {
        a.iter()
            .zip(b)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0, f64::max)
    }

    /// The `CP-SIM-M4.2` correctness gate: the fill-reducing ordered LU solves the
    /// same systems as the dense reference to ≤1e-12 (it is the same LU, merely
    /// reordered). Covers the arrow (worst natural-order fill), a 2-D grid, and a
    /// resistor ladder.
    #[test]
    fn ordered_matches_dense() {
        let cases: Vec<(Conds, Vec<f64>, usize)> = vec![arrow(500), grid(16), arrow(2000)];
        for (conds, rhs, n) in cases {
            let reference = dense_ref(&conds, &rhs, n);
            let mat = sparse_mat(&conds, n);
            let sym = Symbolic::analyze(&mat);
            let num = sym.factor(&mat).expect("ordered factor solves");
            let x = num.solve(&rhs);
            let err = max_abs_diff(&x, &reference);
            assert!(err <= 1e-12, "ordered vs dense max |Δ| = {err} (n = {n})");
        }

        // A ladder (with its voltage-source branch row) too, to exercise the pivot
        // on a zero-diagonal branch under the ordering.
        let n = 300;
        let mut mat = SparseMatrix::new(n + 2);
        let mut sref = System::dense(n + 2);
        let branch = n + 1;
        mat.stamp_vsource(Some(0), None, branch);
        sref.stamp_vsource(Some(0), None, branch, 10.0);
        let mut prev = 0usize;
        for i in 0..n {
            let node = i + 1;
            mat.stamp_conductance(Some(prev), Some(node), 1e-3);
            mat.stamp_conductance(Some(node), None, 1e-3);
            sref.stamp_conductance(Some(prev), Some(node), 1e-3);
            sref.stamp_conductance(Some(node), None, 1e-3);
            prev = node;
        }
        let mut rhs = vec![0.0; n + 2];
        rhs[branch] = 10.0;
        let reference = sref.solve().unwrap();
        let sym = Symbolic::analyze(&mat);
        let x = sym.factor(&mat).unwrap().solve(&rhs);
        assert!(
            max_abs_diff(&x, &reference) <= 1e-12,
            "ordered ladder vs dense diverged"
        );
        assert_eq!(x[0], 10.0, "source-pinned hub voltage");
    }

    /// Reusing the symbolic factorization (`refactor`) for a same-structured matrix
    /// gives the same answer as factoring it afresh — the KLU reuse property. The
    /// second matrix scales every conductance (and uses a different RHS), so only
    /// the numbers change, not the structure; `refactor` must track it to ≤1e-12.
    #[test]
    fn refactor_reuses_symbolic_and_matches_fresh() {
        let (conds, rhs0, n) = arrow(800);
        let mat0 = sparse_mat(&conds, n);
        let sym = Symbolic::analyze(&mat0);
        let mut num = sym.factor(&mat0).unwrap();
        assert!(max_abs_diff(&num.solve(&rhs0), &dense_ref(&conds, &rhs0, n)) <= 1e-12);

        // A structurally identical matrix with every conductance ×3.7, new RHS.
        let conds2: Vec<_> = conds.iter().map(|&(i, j, g)| (i, j, g * 3.7)).collect();
        let mut rhs1 = vec![0.0; n];
        rhs1[0] = 2e-3;
        rhs1[n / 2] = -1e-3;
        let mat2 = sparse_mat(&conds2, n);

        // Refactor reuses the ordering + pivot sequence; no re-analysis.
        num.refactor(&sym, &mat2).expect("refactor succeeds");
        let reused = num.solve(&rhs1);

        // A fresh analyze+factor of the scaled matrix is the ground truth.
        let fresh = Symbolic::analyze(&mat2).factor(&mat2).unwrap().solve(&rhs1);
        let reference = dense_ref(&conds2, &rhs1, n);
        assert!(
            max_abs_diff(&reused, &fresh) <= 1e-12,
            "refactor diverged from a fresh factor"
        );
        assert!(
            max_abs_diff(&reused, &reference) <= 1e-12,
            "refactor diverged from the dense reference"
        );
    }

    /// The min-degree ordering actually cuts fill on the arrow: eliminating the hub
    /// last keeps the factor nonzero count O(n), where natural order (hub first)
    /// would fill the whole leaf block to O(n²). We check the ordering puts the
    /// hub (node 0) last and the resulting L/U stay near-linear in size.
    #[test]
    fn ordering_cuts_fill_on_the_arrow() {
        let (conds, _rhs, size) = arrow(1000); // size = 1001 (hub + 1000 leaves)
        let mat = sparse_mat(&conds, size);
        let sym = Symbolic::analyze(&mat);
        // The hub (node 0) is eliminated among the very last nodes — the whole point
        // of min-degree here (it ties at degree 1 with the final leaf, so it lands
        // second-to-last rather than dead last). Natural order would take it first.
        assert!(
            sym.iperm[0] >= size - 2,
            "min-degree should eliminate the hub (node 0) last, got order {}",
            sym.iperm[0]
        );
        let n = size;
        let num = sym.factor(&mat).unwrap();
        let fill: usize = num.l.iter().map(|c| c.len()).sum::<usize>()
            + num.u.iter().map(|c| c.len()).sum::<usize>();
        // Near-linear: bounded by a small constant × size, nowhere near n²/2.
        assert!(
            fill < 8 * n,
            "ordered factor fill {fill} is not near-linear for n = {n}"
        );
    }

    /// A singular sparse matrix (an all-zero row/column) is reported `None` by the
    /// ordered factorization, exactly as the dense and natural-order paths do.
    #[test]
    fn ordered_reports_singular() {
        let (mut conds, _rhs, _n) = arrow(50);
        // Add one wholly detached unknown (index 51): no entries → pivotless column.
        let size = 52;
        conds.retain(|_| true);
        let mat = sparse_mat(&conds, size);
        let sym = Symbolic::analyze(&mat);
        assert!(
            sym.factor(&mat).is_none(),
            "a detached unknown must make the ordered factor singular"
        );
    }
}
