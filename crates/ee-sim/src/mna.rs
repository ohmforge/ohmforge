//! Modified Nodal Analysis: assemble `A·x = z` and solve it (`CP-SIM-M0.3`).
//!
//! Unknowns are the node voltages (ground excluded, it is the reference) plus one
//! branch current per voltage source — the "modified" part of MNA. Each device
//! **stamps** its contribution into `A`/`z`; the solve is a dense Gaussian
//! elimination with partial pivoting. Partial pivoting is not optional here: a
//! voltage source's branch row has a zero on the diagonal, so a naive pivot-free
//! elimination would divide by zero. A sparse (KLU-style) path replaces this for
//! large circuits at `CP-SIM-M2.4`; dense is correct and clear for M0.

/// A dense linear system `A·x = z` under construction via stamps.
pub struct System {
    /// Row-major `size × size` coefficient matrix.
    a: Vec<f64>,
    /// Right-hand side, length `size`.
    z: Vec<f64>,
    size: usize,
}

impl System {
    /// A zeroed `size × size` system.
    pub fn new(size: usize) -> Self {
        Self {
            a: vec![0.0; size * size],
            z: vec![0.0; size],
            size,
        }
    }

    #[inline]
    fn at(&mut self, r: usize, c: usize) -> &mut f64 {
        &mut self.a[r * self.size + c]
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
    pub fn solve(mut self) -> Option<Vec<f64>> {
        let n = self.size;
        if n == 0 {
            return Some(Vec::new());
        }
        // Forward elimination with partial pivoting.
        for col in 0..n {
            // Pick the pivot row: largest magnitude in this column at or below col.
            let mut pivot = col;
            let mut best = self.a[col * n + col].abs();
            for r in (col + 1)..n {
                let v = self.a[r * n + col].abs();
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
                if factor == 0.0 {
                    continue;
                }
                for c in col..n {
                    self.a[r * n + c] -= factor * self.a[col * n + c];
                }
                self.z[r] -= factor * self.z[col];
            }
        }
        // Back substitution.
        let mut x = vec![0.0; n];
        for row in (0..n).rev() {
            let base = row * n;
            let sum = self.z[row]
                - self.a[base + row + 1..base + n]
                    .iter()
                    .zip(&x[row + 1..n])
                    .map(|(a, xc)| a * xc)
                    .sum::<f64>();
            x[row] = sum / self.a[row * n + row];
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
}
