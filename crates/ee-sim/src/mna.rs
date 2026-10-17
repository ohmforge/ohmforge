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

use std::collections::BTreeMap;

/// Unknown count at or above which [`System::new`] assembles into the sparse
/// backing. Chosen well above every hand-built test circuit so small, human-scale
/// solves keep the dense backing (and their exact prior results), while the large
/// ladders the sparse path targets clear it comfortably.
pub const SPARSE_THRESHOLD: usize = 64;

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
}
