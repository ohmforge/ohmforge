//! Multimeter (DMM) — reads a measurement off a [`Signal`] (`CP-UX-M1.2`).
//!
//! Like the [`Oscilloscope`](crate::oscilloscope::Oscilloscope) the DMM is
//! dependency-light (`ee-core` only) and renders to a `String`, so it stays
//! deterministic and unit-testable with no live terminal.
//!
//! A single [`measure`](Multimeter::measure) pass computes every reading from
//! one signal: the **DC** value (the mean — which for a one-sample operating
//! point *is* the node voltage), the **RMS**, the **peak-to-peak** swing and the
//! **frequency** recovered from mean-crossings. The selected [`DmmMode`] chooses
//! which one the front panel shows; the others are still available on the
//! returned [`DmmReading`].

use crate::fmt::{fit, fmt_eng, kv_line};
use ee_core::signal::Signal;
use ee_core::units::Unit;

/// Which measurement the multimeter display shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmmMode {
    /// DC voltage: the mean of the samples (the operating-point value).
    DcVolts,
    /// True-RMS value, `sqrt(mean(y²))`.
    Rms,
    /// Peak-to-peak swing, `max − min`.
    PeakToPeak,
    /// Fundamental frequency, recovered from rising mean-crossings.
    Frequency,
}

impl DmmMode {
    /// A short tag for the mode, shown on the panel.
    pub const fn tag(self) -> &'static str {
        match self {
            DmmMode::DcVolts => "DC",
            DmmMode::Rms => "RMS",
            DmmMode::PeakToPeak => "Vpp",
            DmmMode::Frequency => "FREQ",
        }
    }
}

/// Every reading a single [`measure`](Multimeter::measure) pass produces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DmmReading {
    /// DC / mean value — the operating-point reading for a one-sample signal.
    pub dc: f64,
    /// True-RMS value.
    pub rms: f64,
    /// Peak-to-peak swing (`max − min`); `0` for a flat or single-sample signal.
    pub vpp: f64,
    /// Frequency in hertz from rising mean-crossings; `0` when undeterminable
    /// (fewer than two crossings, non-increasing time axis, DC input).
    pub freq: f64,
    /// The unit of `dc`/`rms`/`vpp` (copied from the measured signal).
    pub unit: Unit,
}

impl DmmReading {
    /// The reading selected by `mode`, paired with the unit symbol to print.
    pub fn value(&self, mode: DmmMode) -> (f64, &'static str) {
        match mode {
            DmmMode::DcVolts => (self.dc, self.unit.symbol()),
            DmmMode::Rms => (self.rms, self.unit.symbol()),
            DmmMode::PeakToPeak => (self.vpp, self.unit.symbol()),
            DmmMode::Frequency => (self.freq, Unit::Hertz.symbol()),
        }
    }
}

/// Compute every DMM reading from a real-valued signal.
///
/// A complex (AC) signal, or one with no samples, yields an all-zero reading
/// rather than a panic. For a single-sample operating point the DC reading is
/// that sample and the swing/frequency are zero.
pub fn measure(signal: &Signal) -> DmmReading {
    let y = signal.real().unwrap_or(&[]);
    let n = y.len();
    if n == 0 {
        return DmmReading {
            dc: 0.0,
            rms: 0.0,
            vpp: 0.0,
            freq: 0.0,
            unit: signal.unit,
        };
    }

    let sum: f64 = y.iter().copied().filter(|v| v.is_finite()).sum();
    let finite = y.iter().filter(|v| v.is_finite()).count().max(1);
    let dc = sum / finite as f64;

    let sq: f64 = y
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .map(|v| v * v)
        .sum();
    let rms = (sq / finite as f64).sqrt();

    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for &v in y {
        if v.is_finite() {
            lo = lo.min(v);
            hi = hi.max(v);
        }
    }
    let vpp = if lo.is_finite() && hi.is_finite() {
        hi - lo
    } else {
        0.0
    };

    let freq = frequency(&signal.x, y, dc);

    DmmReading {
        dc,
        rms,
        vpp,
        freq,
        unit: signal.unit,
    }
}

/// Recover a fundamental frequency from the signal's rising mean-crossings.
///
/// The mean is removed first so a DC-offset waveform still triggers; the period
/// is the average spacing between successive rising crossings (interpolated to
/// sub-sample time), giving a stable estimate over many cycles.
fn frequency(x: &[f64], y: &[f64], mean: f64) -> f64 {
    if x.len() != y.len() || y.len() < 2 {
        return 0.0;
    }
    let mut crossings: Vec<f64> = Vec::new();
    for i in 1..y.len() {
        let (a, b) = (y[i - 1] - mean, y[i] - mean);
        if a < 0.0 && b >= 0.0 {
            let dy = b - a;
            let t = if dy.abs() < f64::EPSILON {
                x[i]
            } else {
                x[i - 1] + (x[i] - x[i - 1]) * (-a / dy)
            };
            crossings.push(t);
        }
    }
    if crossings.len() < 2 {
        return 0.0;
    }
    let span = crossings[crossings.len() - 1] - crossings[0];
    let periods = (crossings.len() - 1) as f64;
    if span > 0.0 {
        periods / span
    } else {
        0.0
    }
}

/// A multimeter front panel: a title, a selected mode and a display width.
pub struct Multimeter {
    title: String,
    mode: DmmMode,
    width: usize,
}

impl Multimeter {
    /// A new multimeter in DC-volts mode, 28 cells wide.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            mode: DmmMode::DcVolts,
            width: 28,
        }
    }

    /// Select which reading the display shows.
    pub fn mode(mut self, mode: DmmMode) -> Self {
        self.mode = mode;
        self
    }

    /// Set the panel width in character cells (minimum 16).
    pub fn size(mut self, width: usize) -> Self {
        self.width = width.max(16);
        self
    }

    /// The currently selected mode.
    pub fn selected(&self) -> DmmMode {
        self.mode
    }

    /// Measure `signal` (a convenience wrapper over [`measure`]).
    pub fn measure(&self, signal: &Signal) -> DmmReading {
        measure(signal)
    }

    /// Render the panel: title, mode tag, the selected reading large, and the
    /// remaining readings on a summary line. Every line fits `width` cells.
    pub fn render(&self, signal: &Signal) -> String {
        let r = self.measure(signal);
        let (val, unit) = r.value(self.mode);

        let mut out = String::new();
        out.push_str(&fit(&format!("  {}", self.title), self.width));
        out.push('\n');
        out.push_str(&kv_line(
            &format!("  [{}]", self.mode.tag()),
            &fmt_eng(val, unit),
            self.width,
        ));
        out.push('\n');

        let sym = r.unit.symbol();
        let summary = format!(
            "  DC {}  RMS {}  Vpp {}  {}",
            fmt_eng(r.dc, sym),
            fmt_eng(r.rms, sym),
            fmt_eng(r.vpp, sym),
            fmt_eng(r.freq, "Hz"),
        );
        out.push_str(&fit(&summary, self.width));
        out.push('\n');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ee_core::units::Unit;
    use std::f64::consts::{SQRT_2, TAU};

    /// A sine of frequency `f` (Hz), `n` samples at step `dt`, amp `a`, offset
    /// `off`.
    fn sine(f: f64, dt: f64, n: usize, a: f64, off: f64) -> Signal {
        let w = TAU * f;
        let x: Vec<f64> = (0..n).map(|j| j as f64 * dt).collect();
        let y: Vec<f64> = (0..n)
            .map(|j| off + a * (w * j as f64 * dt).sin())
            .collect();
        Signal::time("v(sig)", Unit::Volt, x, y)
    }

    #[test]
    fn dc_reads_a_single_sample_node_voltage() {
        // Operating point: one sample. The DC reading IS the node voltage.
        let op = Signal::time("v(node)", Unit::Volt, vec![0.0], vec![3.3]);
        let r = measure(&op);
        assert!((r.dc - 3.3).abs() < 1e-12, "DC = {}", r.dc);
        assert_eq!(r.vpp, 0.0, "single sample has no swing");
        assert_eq!(r.freq, 0.0, "single sample has no frequency");
        assert!((r.rms - 3.3).abs() < 1e-12, "RMS of a constant = |v|");
    }

    #[test]
    fn rms_of_a_sine_is_amp_over_root_two() {
        // 1 kHz, 1000 samples/period across 10 periods, amp 2 V.
        let s = sine(1_000.0, 1e-6, 10_000, 2.0, 0.0);
        let r = measure(&s);
        assert!(
            (r.rms - 2.0 / SQRT_2).abs() < 1e-3,
            "RMS = {}, want {}",
            r.rms,
            2.0 / SQRT_2
        );
    }

    #[test]
    fn vpp_of_a_sine_is_twice_the_amplitude() {
        let s = sine(1_000.0, 1e-6, 10_000, 2.0, 1.0);
        let r = measure(&s);
        assert!((r.vpp - 4.0).abs() < 1e-2, "Vpp = {}, want 4", r.vpp);
        // DC offset recovered as the mean.
        assert!((r.dc - 1.0).abs() < 1e-2, "DC = {}, want 1", r.dc);
    }

    #[test]
    fn frequency_recovered_from_zero_crossings() {
        let f = 1_000.0;
        let s = sine(f, 1e-6, 10_000, 1.0, 0.0);
        let r = measure(&s);
        assert!((r.freq - f).abs() / f < 1e-3, "freq = {}, want {f}", r.freq);

        // A DC-offset sine still recovers the fundamental (mean removed first).
        let off = sine(f, 1e-6, 10_000, 1.0, 5.0);
        assert!((measure(&off).freq - f).abs() / f < 1e-3);
    }

    #[test]
    fn render_shows_selected_mode_and_fits_width() {
        let width = 32;
        let s = sine(1_000.0, 1e-6, 10_000, 2.0, 0.0);
        let out = Multimeter::new("DMM")
            .mode(DmmMode::Rms)
            .size(width)
            .render(&s);
        assert!(out.contains("[RMS]"), "mode tag:\n{out}");
        for line in out.lines() {
            assert!(
                line.chars().count() <= width,
                "line exceeds width {width}: {line:?}"
            );
        }
    }

    #[test]
    fn degenerate_inputs_do_not_panic() {
        let empty = Signal::time("v", Unit::Volt, vec![], vec![]);
        let r = measure(&empty);
        assert_eq!((r.dc, r.rms, r.vpp, r.freq), (0.0, 0.0, 0.0, 0.0));
        let _ = Multimeter::new("DMM").render(&empty);

        // Flat signal: no crossings → 0 Hz, 0 Vpp, DC = the level.
        let flat = Signal::time("v", Unit::Volt, vec![0.0, 1.0, 2.0], vec![2.5; 3]);
        let rf = measure(&flat);
        assert!((rf.dc - 2.5).abs() < 1e-12);
        assert_eq!(rf.freq, 0.0);
        assert_eq!(rf.vpp, 0.0);
    }
}
