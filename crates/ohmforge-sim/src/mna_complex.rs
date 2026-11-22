//! Complex-valued Modified Nodal Analysis for AC small-signal and noise analysis
//! (`CP-SIM-M1.4`, reusable ordered path `CP-SIM-M10.1`).
//!
//! This mirrors [`crate::mna::System`] entry-for-entry, but over
//! [`ohmforge_core::Complex`] instead of `f64`: node voltages and branch currents are
//! phasors, and each reactive device contributes a complex admittance (`jωC` for
//! a capacitor, `1/jωL` for an inductor). Nonlinear devices enter as their
//! small-signal conductance about the DC operating point, so they stamp as a
//! purely real admittance here.
//!
//! Like the real side, one [`ComplexSystem`] carries **two backings** behind one
//! stamp API, chosen by size in [`ComplexSystem::new`]:
//!
//! * **Dense** (below [`COMPLEX_SPARSE_THRESHOLD`]) — a row-major `N×N` array solved
//!   by Gaussian elimination with partial pivoting (pivoting on complex magnitude,
//!   just as essential as on the real side: a voltage source's branch row has a zero
//!   diagonal). This is byte-for-byte what every small circuit got before, so no
//!   golden AC/noise fixture moves a bit.
//! * **Sparse** (at or above the threshold) — one sorted nonzero map per row, solved
//!   by a right-looking sparse LU performing the *same* elimination as the dense one
//!   (merely skipping the structural zeros), so the two agree to the last bit absent
//!   an exact pivot-magnitude tie.
//!
//! On top of those, a swept AC or noise analysis solves the **same structure** at
//! every frequency — only the `jωC` / `1/jωL` numbers change. So, exactly as the
//! real engine's `CP-SIM-M4.2` KLU path does, [`ComplexSymbolic`] computes a
//! fill-reducing ordering once, [`ComplexSymbolic::factor`] records the pivot
//! sequence and factors, [`ComplexNumeric::refactor`] reuses that symbolic structure
//! to recompute only the numbers at each frequency, and [`ComplexNumeric::solve`]
//! back-solves any right-hand side — the multi-RHS win a noise analysis (one solve
//! per noise source per frequency) especially wants. [`ComplexReusableSolver`] wraps
//! the strategy choice: a bit-for-bit pass-through below [`COMPLEX_KLU_THRESHOLD`],
//! the ordered reuse at or above it (matching the dense solve to a few ULP).

use std::collections::{BTreeMap, BTreeSet};

use ohmforge_core::Complex;

use crate::mna::min_degree_eliminate;

/// Unknown count at or above which [`ComplexSystem::new`] assembles into the sparse
/// backing instead of the dense array — the complex twin of
/// [`crate::mna::SPARSE_THRESHOLD`]. Chosen well above every hand-built AC/noise
/// fixture so they keep the dense backing (and their exact prior phasors).
pub const COMPLEX_SPARSE_THRESHOLD: usize = 64;

/// Unknown count at or above which a swept complex solve routes through the
/// fill-reducing, reusable ordered LU ([`ComplexSymbolic`] / [`ComplexNumeric`] via
/// [`ComplexReusableSolver`]) rather than the natural-order sparse solve — the
/// complex twin of [`crate::mna::KLU_THRESHOLD`], and set to the same value for the
/// same reason: the ordered path reorders the elimination and so matches the exact
/// natural-order/dense solve only to a few ULP (≤1e-12), not bit-for-bit, while the
/// golden AC/noise fixtures are far smaller than this and so never leave the exact
/// dense path.
pub const COMPLEX_KLU_THRESHOLD: usize = 256;

/// The coefficient-matrix store behind a [`ComplexSystem`]: a dense array, or one
/// sorted nonzero map per row. The right-hand side `z` is always dense.
#[derive(Clone)]
enum Backing {
    /// Row-major `size × size` array.
    Dense(Vec<Complex>),
    /// Per-row sorted map `column → value` of the structural nonzeros.
    Sparse(Vec<BTreeMap<usize, Complex>>),
}

/// A complex linear system `A·x = z` under construction via stamps. The matrix lives
/// in a [`Backing`] chosen by size; the right-hand side `z` is dense.
pub struct ComplexSystem {
    backing: Backing,
    /// Right-hand side, length `size`.
    z: Vec<Complex>,
    size: usize,
}

impl ComplexSystem {
    /// A zeroed `size × size` system, with the backing chosen by size: dense below
    /// [`COMPLEX_SPARSE_THRESHOLD`], sparse at or above it. This is what the AC /
    /// noise drivers call; [`dense`](Self::dense) / [`sparse`](Self::sparse) force a
    /// backing for tests and benches.
    pub fn new(size: usize) -> Self {
        if size >= COMPLEX_SPARSE_THRESHOLD {
            Self::sparse(size)
        } else {
            Self::dense(size)
        }
    }

    /// A zeroed `size × size` system forced to the dense backing.
    pub fn dense(size: usize) -> Self {
        Self {
            backing: Backing::Dense(vec![Complex::ZERO; size * size]),
            z: vec![Complex::ZERO; size],
            size,
        }
    }

    /// A zeroed `size × size` system forced to the sparse backing.
    pub fn sparse(size: usize) -> Self {
        Self {
            backing: Backing::Sparse(vec![BTreeMap::new(); size]),
            z: vec![Complex::ZERO; size],
            size,
        }
    }

    /// A mutable handle to the entry at (`r`, `c`), creating it (as `0`) if the
    /// sparse backing does not yet hold it — so every stamp reads and writes the same
    /// cell regardless of backing, and the two assemble bit-identical matrices.
    #[inline]
    fn at(&mut self, r: usize, c: usize) -> &mut Complex {
        let n = self.size;
        match &mut self.backing {
            Backing::Dense(a) => &mut a[r * n + c],
            Backing::Sparse(rows) => rows[r].entry(c).or_insert(Complex::ZERO),
        }
    }

    /// Stamp an admittance `y` between optional nodes `i` and `j` (`None` = ground).
    /// Resistor `G`, capacitor `jωC`, inductor `1/(jωL)`, and linearized device
    /// conductances all enter here — the complex analogue of
    /// [`stamp_conductance`](crate::mna::System::stamp_conductance).
    pub fn stamp_admittance(&mut self, i: Option<usize>, j: Option<usize>, y: Complex) {
        if let Some(i) = i {
            *self.at(i, i) = *self.at(i, i) + y;
        }
        if let Some(j) = j {
            *self.at(j, j) = *self.at(j, j) + y;
        }
        if let (Some(i), Some(j)) = (i, j) {
            *self.at(i, j) = *self.at(i, j) - y;
            *self.at(j, i) = *self.at(j, i) - y;
        }
    }

    /// Add `y` to the matrix entry at (`row`, `col`); a ground index (`None`) is
    /// the reference and is dropped. The complex twin of
    /// [`stamp_matrix`](crate::mna::System::stamp_matrix): the raw, possibly
    /// asymmetric entry stamp a linearized multiport device (BJT / MOSFET
    /// small-signal admittance) contributes in AC analysis.
    pub fn stamp_matrix(&mut self, row: Option<usize>, col: Option<usize>, y: Complex) {
        if let (Some(r), Some(c)) = (row, col) {
            *self.at(r, c) = *self.at(r, c) + y;
        }
    }

    /// Stamp a phasor current of `amps` flowing from node `j` into node `i`.
    pub fn stamp_current(&mut self, i: Option<usize>, j: Option<usize>, amps: Complex) {
        if let Some(i) = i {
            self.z[i] = self.z[i] + amps;
        }
        if let Some(j) = j {
            self.z[j] = self.z[j] - amps;
        }
    }

    /// Stamp a voltage source of `volts` (a phasor) from `plus` to `minus`, using
    /// branch row `k` (an absolute index past the node block).
    pub fn stamp_vsource(
        &mut self,
        plus: Option<usize>,
        minus: Option<usize>,
        k: usize,
        volts: Complex,
    ) {
        if let Some(i) = plus {
            *self.at(i, k) = *self.at(i, k) + Complex::ONE;
            *self.at(k, i) = *self.at(k, i) + Complex::ONE;
        }
        if let Some(j) = minus {
            *self.at(j, k) = *self.at(j, k) - Complex::ONE;
            *self.at(k, j) = *self.at(k, j) - Complex::ONE;
        }
        self.z[k] = self.z[k] + volts;
    }

    /// Solve `A·x = z`, consuming the system. Returns `None` if `A` is singular.
    /// Runs the algorithm matching the backing [`new`](Self::new) chose; the two
    /// agree to the last bit on a non-degenerate matrix.
    pub fn solve(self) -> Option<Vec<Complex>> {
        match self.backing {
            Backing::Dense(a) => Self::solve_dense(a, self.z, self.size),
            Backing::Sparse(rows) => Self::solve_sparse(rows, self.z, self.size),
        }
    }

    /// Consume this assembled system into the `(ComplexSparseMatrix, rhs)` pair the
    /// reusable ordered LU consumes — the bridge [`ComplexReusableSolver`] crosses at
    /// or above [`COMPLEX_KLU_THRESHOLD`]. A sparse backing hands its row maps over
    /// (a move); a dense backing is condensed to its structural nonzeros.
    pub(crate) fn into_ordered_parts(self) -> (ComplexSparseMatrix, Vec<Complex>) {
        let n = self.size;
        let rows = match self.backing {
            Backing::Sparse(rows) => rows,
            Backing::Dense(a) => {
                let mut rows = vec![BTreeMap::new(); n];
                for (r, row) in rows.iter_mut().enumerate() {
                    for c in 0..n {
                        let v = a[r * n + c];
                        if v != Complex::ZERO {
                            row.insert(c, v);
                        }
                    }
                }
                rows
            }
        };
        (ComplexSparseMatrix { n, rows }, self.z)
    }

    /// Dense Gaussian elimination with partial pivoting (on complex magnitude) — the
    /// original `O(N³)` solve. Returns `None` if `A` is singular.
    fn solve_dense(mut a: Vec<Complex>, mut z: Vec<Complex>, n: usize) -> Option<Vec<Complex>> {
        if n == 0 {
            return Some(Vec::new());
        }
        // Forward elimination with partial pivoting.
        for col in 0..n {
            // Pick the pivot row: largest magnitude in this column at or below col.
            let mut pivot = col;
            let mut best = a[col * n + col].magnitude();
            for r in (col + 1)..n {
                let v = a[r * n + col].magnitude();
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
                if factor == Complex::ZERO {
                    continue;
                }
                for c in col..n {
                    a[r * n + c] = a[r * n + c] - factor * a[col * n + c];
                }
                z[r] = z[r] - factor * z[col];
            }
        }
        // Back substitution.
        let mut x = vec![Complex::ZERO; n];
        for row in (0..n).rev() {
            let base = row * n;
            let mut sum = z[row];
            for (c, xc) in x.iter().enumerate().take(n).skip(row + 1) {
                sum = sum - a[base + c] * *xc;
            }
            x[row] = sum / a[row * n + row];
        }
        Some(x)
    }

    /// Right-looking sparse LU with partial pivoting — the same elimination as
    /// [`solve_dense`](Self::solve_dense) (the same pivot choice and row
    /// combinations, skipping the structural zeros), so absent an exact
    /// pivot-magnitude tie it returns an identical `x`. The structure mirrors the
    /// real [`crate::mna::System`]'s sparse solve, with one deliberate difference in
    /// the back-substitution order: the complex dense solve subtracts each product
    /// *in turn* from the running sum (`((z − p₁) − p₂) …`), so this path does too
    /// (ascending column order), where the real dense solve accumulates then
    /// subtracts once — matching the dense partner bit-for-bit each way.
    fn solve_sparse(
        sparse_rows: Vec<BTreeMap<usize, Complex>>,
        mut z: Vec<Complex>,
        n: usize,
    ) -> Option<Vec<Complex>> {
        if n == 0 {
            return Some(Vec::new());
        }
        let mut rows: Vec<Vec<(usize, Complex)>> = sparse_rows
            .into_iter()
            .map(|m| m.into_iter().collect())
            .collect();

        let mut col_rows: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (r, row) in rows.iter().enumerate() {
            for &(c, _) in row {
                col_rows[c].push(r);
            }
        }

        let mut used = vec![false; n];
        let mut pivot_of_col = vec![usize::MAX; n];

        // Forward elimination, column by column.
        for c in 0..n {
            let mut pivot_row = usize::MAX;
            let mut best = 0.0_f64;
            for &r in &col_rows[c] {
                if used[r] {
                    continue;
                }
                let v = value_at(&rows, r, c).magnitude();
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
            let pivot_upper: Vec<(usize, Complex)> = rows[pivot_row]
                .iter()
                .copied()
                .filter(|&(cc, _)| cc > c)
                .collect();

            let targets: Vec<usize> = col_rows[c]
                .iter()
                .copied()
                .filter(|&r| r != pivot_row && !used[r])
                .collect();

            for r in targets {
                let Ok(idx_c) = rows[r].binary_search_by(|&(cc, _)| cc.cmp(&c)) else {
                    continue;
                };
                let factor = rows[r][idx_c].1 / pivot_val;
                rows[r].remove(idx_c);
                if factor == Complex::ZERO {
                    continue;
                }
                for &(cj, pv) in &pivot_upper {
                    match rows[r].binary_search_by(|&(cc, _)| cc.cmp(&cj)) {
                        Ok(i) => rows[r][i].1 = rows[r][i].1 - factor * pv,
                        Err(i) => {
                            rows[r].insert(i, (cj, Complex::ZERO - factor * pv));
                            col_rows[cj].push(r); // new fill-in
                        }
                    }
                }
                z[r] = z[r] - factor * pivot_z;
            }
        }

        // Back substitution: subtract each upper product in turn, ascending column
        // order, to match the dense complex solve bit-for-bit.
        let mut x = vec![Complex::ZERO; n];
        for c in (0..n).rev() {
            let p = pivot_of_col[c];
            let mut diag = Complex::ZERO;
            let mut val = z[p];
            for &(cc, v) in &rows[p] {
                match cc.cmp(&c) {
                    std::cmp::Ordering::Equal => diag = v,
                    std::cmp::Ordering::Greater => val = val - v * x[cc],
                    std::cmp::Ordering::Less => {}
                }
            }
            x[c] = val / diag;
        }
        Some(x)
    }
}

// ===========================================================================
// KLU-class complex sparse: fill-reducing ordering + reusable factorization
// (CP-SIM-M10.1) — a direct port of the real mna.rs reuse (CP-SIM-M4.2).
// ===========================================================================

/// A complex sparse coefficient matrix in row-map form — the input to the reusable
/// ordered LU ([`ComplexSymbolic`] / [`ComplexNumeric`]).
pub struct ComplexSparseMatrix {
    n: usize,
    rows: Vec<BTreeMap<usize, Complex>>,
}

impl ComplexSparseMatrix {
    /// A zeroed `n × n` complex sparse matrix.
    pub fn new(n: usize) -> Self {
        Self {
            n,
            rows: vec![BTreeMap::new(); n],
        }
    }

    /// Accumulate `val` into entry (`r`, `c`).
    pub fn add(&mut self, r: usize, c: usize, val: Complex) {
        let e = self.rows[r].entry(c).or_insert(Complex::ZERO);
        *e = *e + val;
    }

    /// Stamp an admittance `y` between optional nodes `i`/`j` (`None` = ground),
    /// identical to [`ComplexSystem::stamp_admittance`].
    pub fn stamp_admittance(&mut self, i: Option<usize>, j: Option<usize>, y: Complex) {
        if let Some(i) = i {
            self.add(i, i, y);
        }
        if let Some(j) = j {
            self.add(j, j, y);
        }
        if let (Some(i), Some(j)) = (i, j) {
            self.add(i, j, Complex::ZERO - y);
            self.add(j, i, Complex::ZERO - y);
        }
    }
}

/// The reusable symbolic factorization of a complex matrix's structure: a
/// fill-reducing ordering computed once and reused across every numeric factorization
/// of a matrix with that same pattern (`CP-SIM-M10.1`).
pub struct ComplexSymbolic {
    n: usize,
    /// `perm[new] = old` — the fill-reducing elimination order.
    perm: Vec<usize>,
    /// `iperm[old] = new` — the inverse of `perm`.
    iperm: Vec<usize>,
}

impl ComplexSymbolic {
    /// Analyze `mat`'s nonzero structure and compute a minimum-degree fill-reducing
    /// ordering (the same structural heuristic the real side uses, via
    /// [`min_degree_eliminate`]). Depends only on the pattern, so the result is
    /// reused for every numeric factorization of a same-structured matrix.
    pub fn analyze(mat: &ComplexSparseMatrix) -> ComplexSymbolic {
        let n = mat.n;
        let mut adj: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n];
        for (r, row) in mat.rows.iter().enumerate() {
            for &c in row.keys() {
                if c != r {
                    adj[r].insert(c);
                    adj[c].insert(r);
                }
            }
        }
        let perm = min_degree_eliminate(adj);
        let mut iperm = vec![0usize; n];
        for (newi, &old) in perm.iter().enumerate() {
            iperm[old] = newi;
        }
        ComplexSymbolic { n, perm, iperm }
    }

    /// Numerically factor `mat` (which must share this ordering's structure) with
    /// partial pivoting *within the fill-reducing order*, or `None` if singular.
    pub fn factor(&self, mat: &ComplexSparseMatrix) -> Option<ComplexNumeric> {
        self.run(mat, None)
    }

    /// The shared right-looking elimination. `forced = None` searches for the partial
    /// pivot and records it; `forced = Some(seq)` reuses a recorded pivot sequence
    /// (the refactor path), failing with `None` if a reused pivot has collapsed.
    fn run(&self, mat: &ComplexSparseMatrix, forced: Option<&[usize]>) -> Option<ComplexNumeric> {
        let n = self.n;
        let mut rows: Vec<Vec<(usize, Complex)>> = vec![Vec::new(); n];
        for (newi, row) in rows.iter_mut().enumerate() {
            let old = self.perm[newi];
            let mut v: Vec<(usize, Complex)> = mat.rows[old]
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
        let mut l: Vec<Vec<(usize, Complex)>> = vec![Vec::new(); n];
        let mut u: Vec<Vec<(usize, Complex)>> = vec![Vec::new(); n];

        for c in 0..n {
            let pivot_row = match forced {
                Some(seq) => {
                    let p = seq[c];
                    if p >= n || used[p] || value_at(&rows, p, c).magnitude() < 1e-15 {
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
                        let v = value_at(&rows, r, c).magnitude();
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
            let urow: Vec<(usize, Complex)> = rows[pivot_row]
                .iter()
                .copied()
                .filter(|&(cc, _)| cc >= c)
                .collect();
            let pivot_upper: Vec<(usize, Complex)> =
                urow.iter().copied().filter(|&(cc, _)| cc > c).collect();
            u[c] = urow;

            let targets: Vec<usize> = col_rows[c]
                .iter()
                .copied()
                .filter(|&r| r != pivot_row && !used[r])
                .collect();
            let mut lcol: Vec<(usize, Complex)> = Vec::with_capacity(targets.len());
            for r in targets {
                let Ok(idx_c) = rows[r].binary_search_by(|&(cc, _)| cc.cmp(&c)) else {
                    continue;
                };
                let factor = rows[r][idx_c].1 / pivot_val;
                rows[r].remove(idx_c);
                lcol.push((r, factor));
                if factor == Complex::ZERO {
                    continue;
                }
                for &(cj, pv) in &pivot_upper {
                    match rows[r].binary_search_by(|&(cc, _)| cc.cmp(&cj)) {
                        Ok(i) => rows[r][i].1 = rows[r][i].1 - factor * pv,
                        Err(i) => {
                            rows[r].insert(i, (cj, Complex::ZERO - factor * pv));
                            col_rows[cj].push(r);
                        }
                    }
                }
            }
            l[c] = lcol;
        }

        Some(ComplexNumeric {
            n,
            perm: self.perm.clone(),
            pivot_of_col,
            l,
            u,
        })
    }
}

/// The reusable numeric LU factors of one complex matrix in a [`ComplexSymbolic`]
/// ordering (`CP-SIM-M10.1`). [`solve`](Self::solve) applies them to any right-hand
/// side; [`refactor`](Self::refactor) recomputes them for a same-structured matrix
/// without re-analyzing or re-searching pivots.
pub struct ComplexNumeric {
    n: usize,
    perm: Vec<usize>,
    pivot_of_col: Vec<usize>,
    l: Vec<Vec<(usize, Complex)>>,
    u: Vec<Vec<(usize, Complex)>>,
}

impl ComplexNumeric {
    /// Solve `A·x = rhs` from the stored factors — permute the right-hand side,
    /// forward-substitute through L, back-substitute through U, unpermute.
    pub fn solve(&self, rhs: &[Complex]) -> Vec<Complex> {
        let n = self.n;
        let mut bp = vec![Complex::ZERO; n];
        for i in 0..n {
            bp[i] = rhs[self.perm[i]];
        }
        for c in 0..n {
            let piv_b = bp[self.pivot_of_col[c]];
            for &(r, factor) in &self.l[c] {
                bp[r] = bp[r] - factor * piv_b;
            }
        }
        let mut x = vec![Complex::ZERO; n];
        for c in (0..n).rev() {
            let mut diag = Complex::ZERO;
            let mut acc = Complex::ZERO;
            for &(cc, val) in &self.u[c] {
                if cc == c {
                    diag = val;
                } else {
                    acc = acc + val * x[cc];
                }
            }
            x[c] = (bp[self.pivot_of_col[c]] - acc) / diag;
        }
        let mut out = vec![Complex::ZERO; n];
        for i in 0..n {
            out[self.perm[i]] = x[i];
        }
        out
    }

    /// Reuse this factorization's symbolic structure — ordering, pivot sequence,
    /// fill — to recompute the numbers for `mat`, which must share the original
    /// structure. Returns `None` if a reused pivot has collapsed (the caller then
    /// re-analyzes and factors afresh).
    pub fn refactor(&mut self, sym: &ComplexSymbolic, mat: &ComplexSparseMatrix) -> Option<()> {
        let fresh = sym.run(mat, Some(&self.pivot_of_col))?;
        *self = fresh;
        Some(())
    }
}

/// The driver-facing complex linear solver for a swept-structure loop (the points of
/// an AC or noise sweep, `CP-SIM-M10.1`). Mirrors the real engine's
/// `ReusableSolver`:
///
/// * **Below [`COMPLEX_KLU_THRESHOLD`]** it is a pass-through — each assembled
///   [`ComplexSystem`] is solved by its own backing exactly as before, so every small
///   AC/noise circuit keeps its phasors *bit-for-bit*.
/// * **At or above [`COMPLEX_KLU_THRESHOLD`]** the first solve runs
///   [`ComplexSymbolic::analyze`] once and [`ComplexSymbolic::factor`], caching both;
///   every later solve of the same structure only [`ComplexNumeric::refactor`]s and
///   back-solves. A collapsed pivot transparently re-analyzes and re-factors.
///
/// All systems a single solver is fed **must share one structure** — an AC/noise
/// sweep guarantees it (only the `jωC` / `1/jωL` numbers change across frequency).
pub struct ComplexReusableSolver {
    ordered: bool,
    sym: Option<ComplexSymbolic>,
    num: Option<ComplexNumeric>,
}

impl ComplexReusableSolver {
    /// A solver for systems of `size` unknowns: the ordered reusable path at or above
    /// [`COMPLEX_KLU_THRESHOLD`], else a bit-for-bit pass-through to
    /// [`ComplexSystem::solve`].
    pub fn new(size: usize) -> Self {
        Self {
            ordered: size >= COMPLEX_KLU_THRESHOLD,
            sym: None,
            num: None,
        }
    }

    /// Solve one assembled system, consuming it. On the ordered path this reuses the
    /// cached symbolic factorization (analyzing only on the first call or after a
    /// pivot collapse). Returns `None` if the matrix is singular.
    pub fn solve(&mut self, sys: ComplexSystem) -> Option<Vec<Complex>> {
        if !self.ordered {
            return sys.solve();
        }
        let (mat, rhs) = sys.into_ordered_parts();
        if let (Some(sym), Some(num)) = (self.sym.as_ref(), self.num.as_mut()) {
            if num.refactor(sym, &mat).is_some() {
                return Some(num.solve(&rhs));
            }
        }
        let sym = ComplexSymbolic::analyze(&mat);
        let num = sym.factor(&mat)?;
        let x = num.solve(&rhs);
        self.sym = Some(sym);
        self.num = Some(num);
        Some(x)
    }
}

/// The value a row currently holds at a column (0 if absent) — the elimination's
/// structural lookup, over column-ascending `(col, value)` rows.
fn value_at(rows: &[Vec<(usize, Complex)>], r: usize, c: usize) -> Complex {
    match rows[r].binary_search_by(|&(cc, _)| cc.cmp(&c)) {
        Ok(i) => rows[r][i].1,
        Err(_) => Complex::ZERO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solves_a_complex_2x2() {
        // [ (1+0j)  (0+1j) ] x = [ 1 ]   -> solve and check A·x == z.
        // [ (0-1j)  (2+0j) ]       [ 0 ]
        let mut s = ComplexSystem::new(2);
        *s.at(0, 0) = Complex::new(1.0, 0.0);
        *s.at(0, 1) = Complex::new(0.0, 1.0);
        *s.at(1, 0) = Complex::new(0.0, -1.0);
        *s.at(1, 1) = Complex::new(2.0, 0.0);
        s.z[0] = Complex::new(1.0, 0.0);
        s.z[1] = Complex::new(0.0, 0.0);

        // Residual check against a fresh copy of the same matrix.
        let (a00, a01, a10, a11) = (
            Complex::new(1.0, 0.0),
            Complex::new(0.0, 1.0),
            Complex::new(0.0, -1.0),
            Complex::new(2.0, 0.0),
        );
        let x = s.solve().unwrap();
        let r0 = a00 * x[0] + a01 * x[1] - Complex::new(1.0, 0.0);
        let r1 = a10 * x[0] + a11 * x[1] - Complex::ZERO;
        assert!(r0.magnitude() < 1e-12 && r1.magnitude() < 1e-12);
    }

    #[test]
    fn pivots_through_a_zero_diagonal() {
        // [0 1; 1 0] x = [2; 3] -> x = [3; 2], needs a row swap exactly like the
        // real solver's branch-row case.
        let mut s = ComplexSystem::new(2);
        *s.at(0, 1) = Complex::ONE;
        *s.at(1, 0) = Complex::ONE;
        s.z[0] = Complex::new(2.0, 0.0);
        s.z[1] = Complex::new(3.0, 0.0);
        let x = s.solve().unwrap();
        assert!((x[0] - Complex::new(3.0, 0.0)).magnitude() < 1e-12);
        assert!((x[1] - Complex::new(2.0, 0.0)).magnitude() < 1e-12);
    }

    #[test]
    fn reports_singular() {
        let mut s = ComplexSystem::new(2);
        *s.at(0, 0) = Complex::ONE;
        *s.at(0, 1) = Complex::ONE;
        *s.at(1, 0) = Complex::ONE;
        *s.at(1, 1) = Complex::ONE; // rank-deficient
        s.z[0] = Complex::ONE;
        s.z[1] = Complex::new(2.0, 0.0);
        assert!(s.solve().is_none());
    }

    // --- Complex sparse / ordered equivalence — CP-SIM-M10.1 ----------------

    /// A deterministic complex LCG for reproducible "dense random" fixtures, with no
    /// external dependency (keeps the crate deterministic + wasm-clean).
    struct Lcg(u64);
    impl Lcg {
        fn next_f64(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
        }
        fn next_c(&mut self) -> Complex {
            Complex::new(self.next_f64(), self.next_f64())
        }
    }

    /// A complex, diagonally dominant `n × n` system with a fixed seed — stamped into
    /// a system forced to `make`'s backing.
    fn dense_random_system(n: usize, seed: u64, make: fn(usize) -> ComplexSystem) -> ComplexSystem {
        let mut s = make(n);
        let mut rng = Lcg(seed);
        for r in 0..n {
            let mut rowmag = 0.0;
            for c in 0..n {
                if r != c {
                    let v = rng.next_c();
                    *s.at(r, c) = v;
                    rowmag += v.magnitude();
                }
            }
            // Diagonal dominates the row → nonsingular regardless of pivoting.
            *s.at(r, r) = Complex::new(rowmag + 1.0 + rng.next_f64().abs(), rng.next_f64());
            s.z[r] = rng.next_c();
        }
        s
    }

    /// The correctness gate for the sparse backing: it solves a complex system
    /// bit-for-bit identically to the dense one, across several fixtures.
    #[test]
    fn complex_sparse_matches_dense_bit_for_bit() {
        for &seed in &[1u64, 12345, 0xDEAD_BEEF] {
            let dense = dense_random_system(70, seed, ComplexSystem::dense)
                .solve()
                .unwrap();
            let sparse = dense_random_system(70, seed, ComplexSystem::sparse)
                .solve()
                .unwrap();
            assert_eq!(dense, sparse, "complex sparse diverged (seed={seed})");
        }
    }

    /// Assemble a [`ComplexSparseMatrix`] + RHS for an RC/RLC-flavored "arrow": a hub
    /// joined to each leaf by a complex admittance, every leaf shunted to ground, a
    /// 1∠0 probe at the hub. Structure fixed; only the numbers change with `scale`.
    fn arrow(n: usize, scale: Complex) -> (ComplexSparseMatrix, Vec<Complex>) {
        let size = n + 1;
        let y = Complex::new(1e-3, 5e-4) * scale;
        let mut m = ComplexSparseMatrix::new(size);
        for leaf in 1..=n {
            m.stamp_admittance(Some(0), Some(leaf), y);
            m.stamp_admittance(Some(leaf), None, y);
        }
        let mut rhs = vec![Complex::ZERO; size];
        rhs[0] = Complex::ONE;
        (m, rhs)
    }

    fn dense_arrow_solve(n: usize, scale: Complex, rhs: &[Complex]) -> Vec<Complex> {
        let size = n + 1;
        let y = Complex::new(1e-3, 5e-4) * scale;
        let mut s = ComplexSystem::dense(size);
        for leaf in 1..=n {
            s.stamp_admittance(Some(0), Some(leaf), y);
            s.stamp_admittance(Some(leaf), None, y);
        }
        s.z.copy_from_slice(rhs);
        s.solve().expect("dense arrow solves")
    }

    fn max_abs_diff(a: &[Complex], b: &[Complex]) -> f64 {
        a.iter()
            .zip(b)
            .map(|(x, y)| (*x - *y).magnitude())
            .fold(0.0, f64::max)
    }

    /// The ordered reusable complex LU matches the dense reference on a small,
    /// well-conditioned arrow (≤1e-12), and `refactor` tracks a same-structured matrix
    /// with new numbers — the KLU reuse property — reproducing a *fresh* ordered
    /// factor of the scaled matrix to ≤1e-12. (Ordered-vs-dense at scale is proven on
    /// a well-conditioned ladder by the engine-level `swept_ac_ordered_matches_dense`;
    /// a large natural-order dense arrow is the pathological-fill case and so makes a
    /// poor reference here.)
    #[test]
    fn complex_ordered_and_refactor_match_dense() {
        let (mat0, rhs0) = arrow(60, Complex::ONE);
        let sym = ComplexSymbolic::analyze(&mat0);
        let mut num = sym.factor(&mat0).expect("ordered factor solves");
        let ref0 = dense_arrow_solve(60, Complex::ONE, &rhs0);
        assert!(
            max_abs_diff(&num.solve(&rhs0), &ref0) <= 1e-12,
            "ordered factor diverged from dense"
        );

        // A structurally identical matrix, every admittance × (3 + 2j), new RHS.
        let scale = Complex::new(3.0, 2.0);
        let (mat1, _r) = arrow(60, scale);
        let mut rhs1 = vec![Complex::ZERO; 61];
        rhs1[0] = Complex::new(2.0, 0.0);
        rhs1[30] = Complex::new(-1.0, 0.5);

        // Refactor reuses the ordering + pivot sequence; no re-analysis.
        num.refactor(&sym, &mat1).expect("refactor succeeds");
        let reused = num.solve(&rhs1);
        // A fresh analyze+factor of the scaled matrix is the ground truth for reuse.
        let fresh = ComplexSymbolic::analyze(&mat1)
            .factor(&mat1)
            .unwrap()
            .solve(&rhs1);
        assert!(
            max_abs_diff(&reused, &fresh) <= 1e-12,
            "complex refactor diverged from a fresh factor"
        );
        // And it still tracks the dense reference on this well-conditioned size.
        let ref1 = dense_arrow_solve(60, scale, &rhs1);
        assert!(
            max_abs_diff(&reused, &ref1) <= 1e-12,
            "complex refactor diverged from dense"
        );
    }

    /// A singular complex matrix (a detached unknown) is reported `None` by the
    /// ordered factorization, as the dense path is.
    #[test]
    fn complex_ordered_reports_singular() {
        let (_m, _r) = arrow(10, Complex::ONE);
        // A matrix with one wholly detached unknown → pivotless column.
        let mut m = ComplexSparseMatrix::new(5);
        m.stamp_admittance(Some(0), Some(1), Complex::ONE);
        m.stamp_admittance(Some(1), None, Complex::ONE);
        // node 2,3,4 detached
        let sym = ComplexSymbolic::analyze(&m);
        assert!(sym.factor(&m).is_none());
    }
}
