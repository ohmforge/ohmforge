//! Scope math channels (`CP-UX-M2.3`).
//!
//! A *math channel* derives a new time-domain [`Signal`] from two existing ones
//! — the classic bench-scope `A−B`, `A+B`, `A·B`. The result is an ordinary
//! `Signal` that drops straight back into the [`oscilloscope`](crate::oscilloscope)
//! as another channel, so the difference of two probes plots exactly like a
//! measured trace.
//!
//! Channel `b` is linearly resampled onto channel `a`'s time grid before the
//! pointwise operation, so the inputs need not share sample points; when they do
//! (the common case) the resampling is exact and `A−B` is the plain elementwise
//! difference.

use ee_core::signal::{Signal, SignalData};
use ee_core::units::Unit;

/// The pointwise operation a math channel applies to its two inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MathOp {
    /// `a + b`.
    Add,
    /// `a − b`.
    Sub,
    /// `a · b`.
    Mul,
}

impl MathOp {
    fn apply(self, a: f64, b: f64) -> f64 {
        match self {
            MathOp::Add => a + b,
            MathOp::Sub => a - b,
            MathOp::Mul => a * b,
        }
    }

    /// The unit of the result. Sum/difference keep `a`'s unit; a product mixes
    /// dimensions, so it is reported dimensionless rather than mislabelled.
    fn result_unit(self, a: Unit) -> Unit {
        match self {
            MathOp::Add | MathOp::Sub => a,
            MathOp::Mul => Unit::None,
        }
    }
}

/// Build the math channel `op(a, b)` sampled on `a`'s time axis.
///
/// Both inputs must be real time-domain signals; a complex (frequency-domain)
/// input, or an empty `a`, yields an empty signal rather than a panic. `b` is
/// interpolated onto `a.x` (clamped at its ends), so matching grids give an
/// exact elementwise result.
pub fn math_channel(name: impl Into<String>, a: &Signal, b: &Signal, op: MathOp) -> Signal {
    let unit = op.result_unit(a.unit);
    let (ya, yb) = match (a.real(), b.real()) {
        (Some(ya), Some(yb)) => (ya, yb),
        _ => return Signal::time(name, unit, Vec::new(), Vec::new()),
    };

    let y: Vec<f64> =
        a.x.iter()
            .zip(ya)
            .map(|(&xi, &ai)| op.apply(ai, interp(&b.x, yb, xi)))
            .collect();
    Signal {
        name: name.into(),
        unit,
        domain: a.domain,
        x: a.x.clone(),
        y: SignalData::Real(y),
    }
}

/// Linearly interpolate `y` at `xd` over a non-decreasing `x`. Clamps at ends.
/// (Kept private here to keep the module free of cross-module coupling.)
fn interp(x: &[f64], y: &[f64], xd: f64) -> f64 {
    if x.is_empty() {
        return 0.0;
    }
    let i = x.partition_point(|&v| v < xd);
    if i == 0 {
        return y[0];
    }
    if i >= x.len() {
        return y[x.len() - 1];
    }
    let (x0, x1) = (x[i - 1], x[i]);
    let span = x1 - x0;
    if span.abs() < f64::EPSILON {
        return y[i];
    }
    let t = (xd - x0) / span;
    y[i - 1] + (y[i] - y[i - 1]) * t
}

#[cfg(test)]
mod tests {
    use super::*;
    use ee_core::signal::Complex;

    fn ch(name: &str, y: Vec<f64>) -> Signal {
        let x: Vec<f64> = (0..y.len()).map(|i| i as f64).collect();
        Signal::time(name, Unit::Volt, x, y)
    }

    #[test]
    fn subtract_is_elementwise_difference() {
        let a = ch("v(a)", vec![1.0, 2.0, 3.0, 4.0]);
        let b = ch("v(b)", vec![0.5, 0.5, 1.0, 2.0]);
        let d = math_channel("A-B", &a, &b, MathOp::Sub);
        assert_eq!(d.real().unwrap(), &[0.5, 1.5, 2.0, 2.0]);
        assert_eq!(d.name, "A-B");
        assert_eq!(d.unit, Unit::Volt);
        assert_eq!(d.x, a.x);
    }

    #[test]
    fn add_and_multiply() {
        let a = ch("a", vec![1.0, 2.0, 3.0]);
        let b = ch("b", vec![4.0, 5.0, 6.0]);
        assert_eq!(
            math_channel("sum", &a, &b, MathOp::Add).real().unwrap(),
            &[5.0, 7.0, 9.0]
        );
        let prod = math_channel("prod", &a, &b, MathOp::Mul);
        assert_eq!(prod.real().unwrap(), &[4.0, 10.0, 18.0]);
        // A product mixes dimensions → reported dimensionless.
        assert_eq!(prod.unit, Unit::None);
    }

    #[test]
    fn resamples_b_onto_a_grid() {
        // b sampled at 0,2,4 → interpolated onto a's 0,1,2,3,4 grid.
        let a = ch("a", vec![0.0, 0.0, 0.0, 0.0, 0.0]);
        let b = Signal::time("b", Unit::Volt, vec![0.0, 2.0, 4.0], vec![0.0, 2.0, 4.0]);
        // A is zero, so A−B is −B resampled = −[0,1,2,3,4].
        let d = math_channel("A-B", &a, &b, MathOp::Sub);
        assert_eq!(d.real().unwrap(), &[0.0, -1.0, -2.0, -3.0, -4.0]);
    }

    #[test]
    fn complex_or_empty_input_yields_empty_without_panic() {
        let a = ch("a", vec![1.0, 2.0]);
        let cplx = Signal::freq("h", Unit::None, vec![1.0], vec![Complex::ONE]);
        assert!(math_channel("x", &a, &cplx, MathOp::Sub).is_empty());

        let empty = Signal::time("e", Unit::Volt, Vec::new(), Vec::new());
        assert!(math_channel("x", &empty, &a, MathOp::Add).is_empty());
    }
}
