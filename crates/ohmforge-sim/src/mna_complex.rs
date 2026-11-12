//! Complex-valued Modified Nodal Analysis for AC small-signal analysis
//! (`CP-SIM-M1.4`).
//!
//! This mirrors [`crate::mna::System`] entry-for-entry, but over
//! [`ohmforge_core::Complex`] instead of `f64`: node voltages and branch currents are
//! phasors, and each reactive device contributes a complex admittance (`jωC` for
//! a capacitor, `1/(jωL)` for an inductor). Nonlinear devices enter as their
//! small-signal conductance about the DC operating point, so they stamp as a
//! purely real admittance here.
//!
//! The solve is the same dense Gaussian elimination with partial pivoting as the
//! real system — pivoting on complex magnitude, which is just as essential: a
//! voltage source's branch row still has a zero on the diagonal, so a pivot-free
//! elimination would divide by zero. A sparse path is future work, as in `mna`.

use ohmforge_core::Complex;

/// A dense complex linear system `A·x = z` under construction via stamps.
pub struct ComplexSystem {
    /// Row-major `size × size` coefficient matrix.
    a: Vec<Complex>,
    /// Right-hand side, length `size`.
    z: Vec<Complex>,
    size: usize,
}

impl ComplexSystem {
    /// A zeroed `size × size` system.
    pub fn new(size: usize) -> Self {
        Self {
            a: vec![Complex::ZERO; size * size],
            z: vec![Complex::ZERO; size],
            size,
        }
    }

    #[inline]
    fn at(&mut self, r: usize, c: usize) -> &mut Complex {
        &mut self.a[r * self.size + c]
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
    ///
    /// Gaussian elimination with partial pivoting, pivoting on the largest-
    /// magnitude entry in the column — the complex twin of the real solver.
    pub fn solve(mut self) -> Option<Vec<Complex>> {
        let n = self.size;
        if n == 0 {
            return Some(Vec::new());
        }
        // Forward elimination with partial pivoting.
        for col in 0..n {
            // Pick the pivot row: largest magnitude in this column at or below col.
            let mut pivot = col;
            let mut best = self.a[col * n + col].magnitude();
            for r in (col + 1)..n {
                let v = self.a[r * n + col].magnitude();
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
                    self.a.swap(pivot * n + c, col * n + c);
                }
                self.z.swap(pivot, col);
            }
            // Eliminate below.
            let piv = self.a[col * n + col];
            for r in (col + 1)..n {
                let factor = self.a[r * n + col] / piv;
                if factor == Complex::ZERO {
                    continue;
                }
                for c in col..n {
                    self.a[r * n + c] = self.a[r * n + c] - factor * self.a[col * n + c];
                }
                self.z[r] = self.z[r] - factor * self.z[col];
            }
        }
        // Back substitution.
        let mut x = vec![Complex::ZERO; n];
        for row in (0..n).rev() {
            let base = row * n;
            let mut sum = self.z[row];
            for (c, xc) in x.iter().enumerate().take(n).skip(row + 1) {
                sum = sum - self.a[base + c] * *xc;
            }
            x[row] = sum / self.a[row * n + row];
        }
        Some(x)
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
}
