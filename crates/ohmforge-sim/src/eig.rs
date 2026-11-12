//! A compact, dependency-free dense linear-algebra kernel for the small-signal
//! transfer-function / pole-zero analysis (`CP-SIM-M7.1`).
//!
//! Two routines, both hand-rolled so the compute core keeps **no external
//! linear-algebra crate** (and stays `wasm32`-clean and deterministic):
//!
//! * [`solve_multi`] — Gaussian elimination with partial pivoting, applied to
//!   several right-hand sides at once. The pole/zero analysis uses it to form
//!   `G⁻¹·C` (one solve per column of `C`) and to read off the DC transfer gain
//!   and the input/output port impedances.
//! * [`eigenvalues`] — the eigenvalues of a general real (unsymmetric) matrix,
//!   via reduction to upper Hessenberg form ([`elmhes`], Gaussian elimination)
//!   followed by the **Francis double-shift QR iteration** ([`hqr`]). Real
//!   eigenvalues come back with a zero imaginary part; complex ones in conjugate
//!   pairs. This is the eigensolver the pencil `det(G + sC) = 0` is reduced to.
//!
//! **Scope / size bound.** These are dense `O(N³)` kernels with no balancing and
//! no eigenvectors — deliberately small and auditable, not a LAPACK replacement.
//! They are bounded to the modest system sizes a `.TF` port analysis produces
//! (the caller enforces `PZ_MAX_SYSTEM`); the large-matrix sparse path lives in
//! [`crate::mna`]. The QR iteration is capped at [`QR_MAX_ITERS`] sweeps per
//! eigenvalue and returns `None` if it fails to converge, so a pathological matrix
//! surfaces cleanly rather than looping.

use ohmforge_core::Complex;

/// Pivot-magnitude floor below which [`solve_multi`] declares the matrix singular
/// — the same threshold the real MNA solve ([`crate::mna`]) uses.
const SINGULAR_EPS: f64 = 1e-15;

/// Per-eigenvalue cap on QR sweeps before [`hqr`] gives up (returns `None`). Far
/// above what any well-formed modest-size pencil needs; it only trips on a
/// pathological matrix. (Numerical-Recipes `hqr` uses 30; the extra headroom costs
/// nothing on the tiny matrices this analysis produces.)
const QR_MAX_ITERS: usize = 60;

/// `|a|` carrying the sign of `b` (the Fortran/Numerical-Recipes `SIGN`).
#[inline]
fn copysign_nr(a: f64, b: f64) -> f64 {
    if b >= 0.0 {
        a.abs()
    } else {
        -a.abs()
    }
}

/// Solve `A·X = B` with partial pivoting, where `a` is the row-major `n×n` matrix
/// and each entry of `rhs` is one right-hand-side column (length `n`); returns the
/// corresponding solution columns, or `None` if `A` is singular.
///
/// A single LU factorization (in place on a working copy) is reused across every
/// right-hand side, so forming `G⁻¹·C` costs one factorization and `n` cheap
/// back-substitutions. Numerically identical in spirit to the dense MNA solve.
pub(crate) fn solve_multi(a: &[f64], n: usize, rhs: &[Vec<f64>]) -> Option<Vec<Vec<f64>>> {
    debug_assert_eq!(a.len(), n * n);
    if n == 0 {
        return Some(rhs.iter().map(|_| Vec::new()).collect());
    }
    let mut m = a.to_vec();
    let mut cols: Vec<Vec<f64>> = rhs.to_vec();

    // Forward elimination with partial pivoting, mirrored into every RHS column.
    for col in 0..n {
        let mut pivot = col;
        let mut best = m[col * n + col].abs();
        for r in (col + 1)..n {
            let v = m[r * n + col].abs();
            if v > best {
                best = v;
                pivot = r;
            }
        }
        if best < SINGULAR_EPS {
            return None;
        }
        if pivot != col {
            for c in 0..n {
                m.swap(pivot * n + c, col * n + c);
            }
            for b in &mut cols {
                b.swap(pivot, col);
            }
        }
        let piv = m[col * n + col];
        for r in (col + 1)..n {
            let factor = m[r * n + col] / piv;
            if factor == 0.0 {
                continue;
            }
            for c in col..n {
                m[r * n + c] -= factor * m[col * n + c];
            }
            for b in &mut cols {
                b[r] -= factor * b[col];
            }
        }
    }

    // Back-substitution per column.
    let mut out = Vec::with_capacity(cols.len());
    for b in &cols {
        let mut x = vec![0.0; n];
        for row in (0..n).rev() {
            let mut sum = b[row];
            for c in (row + 1)..n {
                sum -= m[row * n + c] * x[c];
            }
            x[row] = sum / m[row * n + row];
        }
        out.push(x);
    }
    Some(out)
}

/// The eigenvalues of a general real `n×n` matrix (row-major `a`), as `n` complex
/// numbers — real eigenvalues with a zero imaginary part, complex ones in
/// conjugate pairs. `None` if the QR iteration fails to converge within
/// [`QR_MAX_ITERS`] sweeps on some eigenvalue.
///
/// Reduces `a` to upper Hessenberg form with [`elmhes`] and then runs the
/// double-shift QR iteration [`hqr`]. Neither eigenvectors nor balancing are
/// computed — only the spectrum, which is all the pole/zero extraction needs.
pub(crate) fn eigenvalues(a: &[f64], n: usize) -> Option<Vec<Complex>> {
    debug_assert_eq!(a.len(), n * n);
    if n == 0 {
        return Some(Vec::new());
    }
    if n == 1 {
        return Some(vec![Complex::new(a[0], 0.0)]);
    }
    // 1-indexed working matrix `h[1..=n][1..=n]` (index 0 is padding), so the
    // transcription of the classic Fortran kernels below stays index-for-index.
    let mut h = vec![vec![0.0f64; n + 1]; n + 1];
    for i in 0..n {
        for j in 0..n {
            h[i + 1][j + 1] = a[i * n + j];
        }
    }
    elmhes(&mut h, n);
    hqr(&mut h, n)
}

/// Reduce a real matrix to upper Hessenberg form by the stabilized elimination of
/// Numerical Recipes' `elmhes` (1-indexed in `a[1..=n][1..=n]`). Only the spectrum
/// is wanted, so the elimination multipliers left below the subdiagonal are zeroed
/// afterwards, leaving a clean Hessenberg matrix for [`hqr`].
// Index-walking loops are intrinsic to this 1-indexed pivot/elimination kernel;
// rewriting them as iterators would obscure the transcription, not clarify it.
#[allow(clippy::needless_range_loop)]
fn elmhes(a: &mut [Vec<f64>], n: usize) {
    if n < 3 {
        return; // already (trivially) Hessenberg
    }
    for m in 2..=(n - 1) {
        // Pivot: the largest-magnitude entry in column m-1 at or below row m.
        let mut x: f64 = 0.0;
        let mut i = m;
        for j in m..=n {
            if a[j][m - 1].abs() >= x.abs() {
                x = a[j][m - 1];
                i = j;
            }
        }
        if i != m {
            // Interchange rows i, m (columns m-1..=n) and columns i, m (all rows).
            for j in (m - 1)..=n {
                let t = a[i][j];
                a[i][j] = a[m][j];
                a[m][j] = t;
            }
            for row in a.iter_mut().take(n + 1).skip(1) {
                row.swap(i, m);
            }
        }
        if x != 0.0 {
            for ii in (m + 1)..=n {
                let mut y = a[ii][m - 1];
                if y != 0.0 {
                    y /= x;
                    a[ii][m - 1] = y;
                    for j in m..=n {
                        a[ii][j] -= y * a[m][j];
                    }
                    for row in a.iter_mut().take(n + 1).skip(1) {
                        row[m] += y * row[ii];
                    }
                }
            }
        }
    }
    // Zero the strictly-below-subdiagonal multipliers elmhes leaves behind.
    for i in 3..=n {
        for j in 1..=(i - 2) {
            a[i][j] = 0.0;
        }
    }
}

/// Eigenvalues of a real upper Hessenberg matrix by the Francis double-shift QR
/// iteration — a direct transcription of Numerical Recipes' `hqr` (1-indexed in
/// `a[1..=n][1..=n]`). Returns the `n` eigenvalues, or `None` if any eigenvalue
/// needs more than [`QR_MAX_ITERS`] sweeps.
#[allow(clippy::needless_range_loop)]
fn hqr(a: &mut [Vec<f64>], n: usize) -> Option<Vec<Complex>> {
    let mut wr = vec![0.0f64; n + 1];
    let mut wi = vec![0.0f64; n + 1];

    // One-norm-ish scale for the convergence test.
    let mut anorm = 0.0;
    for i in 1..=n {
        let start = if i >= 2 { i - 1 } else { 1 };
        for j in start..=n {
            anorm += a[i][j].abs();
        }
    }

    let mut nn = n;
    let mut t = 0.0;
    while nn >= 1 {
        let mut its = 0usize;
        loop {
            // Locate a single negligible subdiagonal element at or above nn.
            let mut l = nn;
            while l >= 2 {
                let mut s = a[l - 1][l - 1].abs() + a[l][l].abs();
                if s == 0.0 {
                    s = anorm;
                }
                if a[l][l - 1].abs() + s == s {
                    a[l][l - 1] = 0.0;
                    break;
                }
                l -= 1;
            }
            let mut x = a[nn][nn];
            if l == nn {
                // One real root.
                wr[nn] = x + t;
                wi[nn] = 0.0;
                nn -= 1;
            } else {
                let mut y = a[nn - 1][nn - 1];
                let mut w = a[nn][nn - 1] * a[nn - 1][nn];
                if l == nn - 1 {
                    // Two roots from the trailing 2×2 block.
                    let p = 0.5 * (y - x);
                    let q = p * p + w;
                    let mut z = q.abs().sqrt();
                    x += t;
                    if q >= 0.0 {
                        z = p + copysign_nr(z, p);
                        wr[nn - 1] = x + z;
                        wr[nn] = wr[nn - 1];
                        if z != 0.0 {
                            wr[nn] = x - w / z;
                        }
                        wi[nn - 1] = 0.0;
                        wi[nn] = 0.0;
                    } else {
                        wr[nn - 1] = x + p;
                        wr[nn] = x + p;
                        wi[nn - 1] = -z;
                        wi[nn] = z;
                    }
                    nn -= 2;
                } else {
                    // No roots isolated yet — a double-shift QR sweep.
                    if its == QR_MAX_ITERS {
                        return None;
                    }
                    if its == 10 || its == 20 {
                        // Exceptional (ad-hoc) shift to break a stall.
                        t += x;
                        for i in 1..=nn {
                            a[i][i] -= x;
                        }
                        let s = a[nn][nn - 1].abs() + a[nn - 1][nn - 2].abs();
                        y = 0.75 * s;
                        x = y;
                        w = -0.4375 * s * s;
                    }
                    its += 1;

                    // Look for two consecutive small subdiagonal elements.
                    let mut p;
                    let mut q;
                    let mut r;
                    let mut m = nn - 2;
                    loop {
                        let z = a[m][m];
                        r = x - z;
                        let s2 = y - z;
                        p = (r * s2 - w) / a[m + 1][m] + a[m][m + 1];
                        q = a[m + 1][m + 1] - z - r - s2;
                        r = a[m + 2][m + 1];
                        let s = p.abs() + q.abs() + r.abs();
                        p /= s;
                        q /= s;
                        r /= s;
                        if m == l {
                            break;
                        }
                        let u = a[m][m - 1].abs() * (q.abs() + r.abs());
                        let v = p.abs() * (a[m - 1][m - 1].abs() + z.abs() + a[m + 1][m + 1].abs());
                        if u + v == v {
                            break;
                        }
                        m -= 1;
                    }

                    for i in (m + 2)..=nn {
                        a[i][i - 2] = 0.0;
                        if i != m + 2 {
                            a[i][i - 3] = 0.0;
                        }
                    }

                    // The double QR sweep over rows/columns m..=nn.
                    let mut k = m;
                    while k < nn {
                        if k != m {
                            p = a[k][k - 1];
                            q = a[k + 1][k - 1];
                            r = 0.0;
                            if k != nn - 1 {
                                r = a[k + 2][k - 1];
                            }
                            x = p.abs() + q.abs() + r.abs();
                            if x != 0.0 {
                                p /= x;
                                q /= x;
                                r /= x;
                            }
                        }
                        let s = copysign_nr((p * p + q * q + r * r).sqrt(), p);
                        if s != 0.0 {
                            if k == m {
                                if l != m {
                                    a[k][k - 1] = -a[k][k - 1];
                                }
                            } else {
                                a[k][k - 1] = -s * x;
                            }
                            p += s;
                            let xx = p / s;
                            let yy = q / s;
                            let zz = r / s;
                            q /= p;
                            r /= p;
                            // Row modification.
                            for j in k..=nn {
                                p = a[k][j] + q * a[k + 1][j];
                                if k != nn - 1 {
                                    p += r * a[k + 2][j];
                                    a[k + 2][j] -= p * zz;
                                }
                                a[k + 1][j] -= p * yy;
                                a[k][j] -= p * xx;
                            }
                            let mmin = if nn < k + 3 { nn } else { k + 3 };
                            // Column modification.
                            for i in l..=mmin {
                                p = xx * a[i][k] + yy * a[i][k + 1];
                                if k != nn - 1 {
                                    p += zz * a[i][k + 2];
                                    a[i][k + 2] -= p * r;
                                }
                                a[i][k + 1] -= p * q;
                                a[i][k] -= p;
                            }
                        }
                        k += 1;
                    }
                }
            }
            // Deflation loop continues until a 1×1 or 2×2 block is isolated.
            // (Signed compare so a just-decremented `nn` cannot underflow.)
            if (l as isize) >= (nn as isize) - 1 {
                break;
            }
        }
    }

    Some((1..=n).map(|i| Complex::new(wr[i], wi[i])).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    /// Match `got` eigenvalues to `want` (order-independent) within tolerance.
    fn spectrum_matches(mut got: Vec<Complex>, want: &[Complex]) {
        assert_eq!(got.len(), want.len(), "eigenvalue count");
        for w in want {
            let pos = got
                .iter()
                .position(|g| close(g.re, w.re) && close(g.im, w.im))
                .unwrap_or_else(|| panic!("missing eigenvalue {w:?} in {got:?}"));
            got.remove(pos);
        }
    }

    #[test]
    fn solve_multi_matches_hand_solution() {
        // [2 1; 1 3] with RHS columns [3;5] and [1;0].
        let a = vec![2.0, 1.0, 1.0, 3.0];
        let x = solve_multi(&a, 2, &[vec![3.0, 5.0], vec![1.0, 0.0]]).unwrap();
        assert!(close(x[0][0], 0.8) && close(x[0][1], 1.4));
        // Inverse first column: [2 1;1 3]⁻¹ = 1/5 [3 -1; -1 2].
        assert!(close(x[1][0], 0.6) && close(x[1][1], -0.2));
    }

    #[test]
    fn solve_multi_reports_singular() {
        let a = vec![1.0, 1.0, 1.0, 1.0];
        assert!(solve_multi(&a, 2, &[vec![1.0, 2.0]]).is_none());
    }

    #[test]
    fn eigenvalues_of_a_diagonal_matrix() {
        let a = vec![2.0, 0.0, 0.0, 0.0, 3.0, 0.0, 0.0, 0.0, -5.0];
        spectrum_matches(
            eigenvalues(&a, 3).unwrap(),
            &[
                Complex::new(2.0, 0.0),
                Complex::new(3.0, 0.0),
                Complex::new(-5.0, 0.0),
            ],
        );
    }

    #[test]
    fn eigenvalues_of_a_rotation_are_a_conjugate_pair() {
        // [0 -1; 1 0] → ±i.
        let a = vec![0.0, -1.0, 1.0, 0.0];
        spectrum_matches(
            eigenvalues(&a, 2).unwrap(),
            &[Complex::new(0.0, 1.0), Complex::new(0.0, -1.0)],
        );
    }

    #[test]
    fn eigenvalues_complex_pair_with_real_part() {
        // [1 -1; 1 1] → 1 ± i.
        let a = vec![1.0, -1.0, 1.0, 1.0];
        spectrum_matches(
            eigenvalues(&a, 2).unwrap(),
            &[Complex::new(1.0, 1.0), Complex::new(1.0, -1.0)],
        );
    }

    #[test]
    fn eigenvalues_of_a_companion_matrix() {
        // Companion of (λ-1)(λ-2)(λ-3) = λ³ - 6λ² + 11λ - 6: roots 1, 2, 3.
        // Row-major companion (top-row form).
        let a = vec![
            6.0, -11.0, 6.0, //
            1.0, 0.0, 0.0, //
            0.0, 1.0, 0.0,
        ];
        spectrum_matches(
            eigenvalues(&a, 3).unwrap(),
            &[
                Complex::new(1.0, 0.0),
                Complex::new(2.0, 0.0),
                Complex::new(3.0, 0.0),
            ],
        );
    }

    #[test]
    fn eigenvalues_upper_triangular_reads_the_diagonal() {
        // A 4×4 upper-triangular matrix: eigenvalues are its diagonal.
        let a = vec![
            1.0, 2.0, 3.0, 4.0, //
            0.0, 5.0, 6.0, 7.0, //
            0.0, 0.0, -2.0, 8.0, //
            0.0, 0.0, 0.0, 9.0,
        ];
        spectrum_matches(
            eigenvalues(&a, 4).unwrap(),
            &[
                Complex::new(1.0, 0.0),
                Complex::new(5.0, 0.0),
                Complex::new(-2.0, 0.0),
                Complex::new(9.0, 0.0),
            ],
        );
    }
}
