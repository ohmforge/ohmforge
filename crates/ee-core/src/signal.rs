//! [`Signal`] — the universal result type (`CP-SIM-M0.5`).
//!
//! Every analysis emits `Signal`s and every consumer (the TUI scope, the
//! instruments, the AI, the file layer) reads them **without depending on
//! `ee-sim`**. That is precisely why the type lives here in `ee-core`: results
//! can cross the render snapshot channel and be serialized by `ee-fmt` with no
//! solver in the dependency graph.
//!
//! The M0 shape is a finished, owned buffer (no borrows into the solver).
//! Streaming variants for long transients come later; AC will add a complex
//! `SignalData::Complex` variant (`CP-SIM-M1.4`) alongside `Real`.

use crate::units::Unit;

/// The independent axis a [`Signal`] is sampled over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Domain {
    /// Time domain (transient); x is seconds.
    Time,
    /// Frequency domain (AC/noise); x is hertz.
    Freq,
}

/// A complex sample (AC analysis). Hand-rolled — no `num-complex` dependency, so
/// the compute core stays dependency-free and `wasm32`-clean.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Complex {
    pub re: f64,
    pub im: f64,
}

impl Complex {
    /// The additive identity `0 + 0j`.
    pub const ZERO: Complex = Complex { re: 0.0, im: 0.0 };
    /// The multiplicative identity `1 + 0j`.
    pub const ONE: Complex = Complex { re: 1.0, im: 0.0 };

    pub const fn new(re: f64, im: f64) -> Self {
        Self { re, im }
    }

    /// Magnitude `|z|`.
    pub fn magnitude(&self) -> f64 {
        self.re.hypot(self.im)
    }

    /// Magnitude in decibels, `20·log10|z|`.
    pub fn magnitude_db(&self) -> f64 {
        20.0 * self.magnitude().log10()
    }

    /// Phase in degrees, `atan2(im, re)`.
    pub fn phase_deg(&self) -> f64 {
        self.im.atan2(self.re).to_degrees()
    }

    /// Complex conjugate `re − im·j`.
    pub fn conj(&self) -> Complex {
        Complex::new(self.re, -self.im)
    }

    /// The reciprocal `1 / z`. Returns a non-finite value for `z = 0`, exactly as
    /// `f64` division does — callers that pivot (the AC solver) guard against it.
    pub fn inv(&self) -> Complex {
        let d = self.re * self.re + self.im * self.im;
        Complex::new(self.re / d, -self.im / d)
    }
}

// Hand-rolled complex arithmetic so the AC solver reads like the real one, with
// no `num-complex` dependency (keeps `ee-core` dependency-free and wasm32-clean).
impl core::ops::Add for Complex {
    type Output = Complex;
    fn add(self, rhs: Complex) -> Complex {
        Complex::new(self.re + rhs.re, self.im + rhs.im)
    }
}

impl core::ops::Sub for Complex {
    type Output = Complex;
    fn sub(self, rhs: Complex) -> Complex {
        Complex::new(self.re - rhs.re, self.im - rhs.im)
    }
}

impl core::ops::Mul for Complex {
    type Output = Complex;
    fn mul(self, rhs: Complex) -> Complex {
        Complex::new(
            self.re * rhs.re - self.im * rhs.im,
            self.re * rhs.im + self.im * rhs.re,
        )
    }
}

impl core::ops::Div for Complex {
    type Output = Complex;
    // `a / b == a · (1/b)` — the multiply is the division, not a misplaced op.
    #[allow(clippy::suspicious_arithmetic_impl)]
    fn div(self, rhs: Complex) -> Complex {
        self * rhs.inv()
    }
}

impl core::ops::Neg for Complex {
    type Output = Complex;
    fn neg(self) -> Complex {
        Complex::new(-self.re, -self.im)
    }
}

/// The dependent samples: real for time-domain / DC, complex for AC.
#[derive(Debug, Clone, PartialEq)]
pub enum SignalData {
    /// Real-valued samples (transient voltages/currents, DC sweeps).
    Real(Vec<f64>),
    /// Complex-valued samples (AC small-signal: magnitude/phase per frequency).
    Complex(Vec<Complex>),
}

/// A named, unit-carrying waveform: `y` sampled at each `x`.
#[derive(Debug, Clone, PartialEq)]
pub struct Signal {
    /// Human label, e.g. `"v(out)"` or `"i(R1)"`.
    pub name: String,
    /// Physical unit of the `y` samples.
    pub unit: Unit,
    /// Whether `x` is time or frequency.
    pub domain: Domain,
    /// Sample points along the independent axis (t or f).
    pub x: Vec<f64>,
    /// Sample values.
    pub y: SignalData,
}

impl Signal {
    /// Build a real-valued time-domain signal (the transient common case).
    pub fn time(name: impl Into<String>, unit: Unit, x: Vec<f64>, y: Vec<f64>) -> Self {
        debug_assert_eq!(x.len(), y.len(), "x and y must be the same length");
        Self {
            name: name.into(),
            unit,
            domain: Domain::Time,
            x,
            y: SignalData::Real(y),
        }
    }

    /// Build a complex-valued frequency-domain signal (the AC common case);
    /// `x` is frequency in hertz.
    pub fn freq(name: impl Into<String>, unit: Unit, x: Vec<f64>, y: Vec<Complex>) -> Self {
        debug_assert_eq!(x.len(), y.len(), "x and y must be the same length");
        Self {
            name: name.into(),
            unit,
            domain: Domain::Freq,
            x,
            y: SignalData::Complex(y),
        }
    }

    /// Number of samples.
    pub fn len(&self) -> usize {
        self.x.len()
    }

    /// Whether the signal carries no samples.
    pub fn is_empty(&self) -> bool {
        self.x.is_empty()
    }

    /// The real `y` samples, if this is real-valued data.
    pub fn real(&self) -> Option<&[f64]> {
        match &self.y {
            SignalData::Real(v) => Some(v),
            SignalData::Complex(_) => None,
        }
    }

    /// The complex `y` samples, if this is complex-valued data.
    pub fn complex(&self) -> Option<&[Complex]> {
        match &self.y {
            SignalData::Complex(v) => Some(v),
            SignalData::Real(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_signal_roundtrips() {
        let s = Signal::time(
            "v(out)",
            Unit::Volt,
            vec![0.0, 1.0, 2.0],
            vec![0.0, 0.5, 0.75],
        );
        assert_eq!(s.len(), 3);
        assert!(!s.is_empty());
        assert_eq!(s.domain, Domain::Time);
        assert_eq!(s.real().unwrap(), &[0.0, 0.5, 0.75]);
        assert!(s.complex().is_none());
    }

    #[test]
    fn freq_signal_carries_complex_samples() {
        // A unit vector at +90°: magnitude 1, phase 90°, 0 dB.
        let s = Signal::freq(
            "v(out)",
            Unit::Volt,
            vec![1_000.0],
            vec![Complex::new(0.0, 1.0)],
        );
        assert_eq!(s.domain, Domain::Freq);
        assert!(s.real().is_none());
        let z = s.complex().unwrap()[0];
        assert!((z.magnitude() - 1.0).abs() < 1e-12);
        assert!((z.phase_deg() - 90.0).abs() < 1e-9);
        assert!(z.magnitude_db().abs() < 1e-9);
    }

    #[test]
    fn complex_arithmetic_matches_by_hand() {
        let a = Complex::new(1.0, 2.0);
        let b = Complex::new(3.0, -1.0);
        assert_eq!(a + b, Complex::new(4.0, 1.0));
        assert_eq!(a - b, Complex::new(-2.0, 3.0));
        // (1+2j)(3−j) = 3 − j + 6j − 2j² = 5 + 5j.
        assert_eq!(a * b, Complex::new(5.0, 5.0));
        // z·(1/z) == 1, and z/z == 1.
        let q = a * a.inv();
        assert!((q.re - 1.0).abs() < 1e-12 && q.im.abs() < 1e-12);
        let r = (a / b) * b;
        assert!((r.re - a.re).abs() < 1e-12 && (r.im - a.im).abs() < 1e-12);
        assert_eq!(-a, Complex::new(-1.0, -2.0));
        assert_eq!(a.conj(), Complex::new(1.0, -2.0));
        assert_eq!(Complex::ZERO + a, a);
        assert_eq!(Complex::ONE * a, a);
    }
}
