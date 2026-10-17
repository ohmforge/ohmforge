//! Function generator — a stimulus source that produces a [`Signal`]
//! (`CP-UX-M1.2`).
//!
//! The generator is the *signal source half* of the funcgen instrument: it
//! synthesises a sine / square / triangle / pulse waveform as an `ee-core`
//! [`Signal`], which any consumer — most usefully the
//! [`Oscilloscope`](crate::oscilloscope::Oscilloscope) — can then display.
//! Driving a live circuit source is a separate sim-core feature; this type does
//! not depend on it.
//!
//! The waveform is `offset + amplitude · shape(2π·f·t + φ)` sampled on a uniform
//! grid of `samples` points over `[0, span)`. For a sine that is exactly
//! `offset + amp·sin(2π f t + φ)`, which is the generator's defining contract
//! and the basis of its "shows on the scope" test.

use crate::fmt::{fit, fmt_eng, kv_line};
use ee_core::signal::Signal;
use ee_core::units::Unit;
use std::f64::consts::TAU;

/// The shape of the generated waveform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waveform {
    /// Sinusoid: `offset + amp·sin(2π f t + φ)`.
    Sine,
    /// Bipolar square: `offset + amp` for the first `duty` of each cycle, else
    /// `offset − amp`.
    Square,
    /// Symmetric triangle through `offset`, peaking at `offset ± amp`.
    Triangle,
    /// Unipolar pulse: `offset + amp` for the first `duty` of each cycle, else
    /// the `offset` baseline.
    Pulse,
}

impl Waveform {
    /// A short tag for the shape, shown on the panel.
    pub const fn tag(self) -> &'static str {
        match self {
            Waveform::Sine => "SINE",
            Waveform::Square => "SQ",
            Waveform::Triangle => "TRI",
            Waveform::Pulse => "PULSE",
        }
    }
}

/// A function-generator channel: a shape and its parameters, plus the output
/// grid (time span and sample count).
#[derive(Debug, Clone)]
pub struct FuncGen {
    name: String,
    unit: Unit,
    waveform: Waveform,
    amplitude: f64,
    frequency: f64,
    offset: f64,
    duty: f64,
    phase: f64,
    span: f64,
    samples: usize,
}

impl FuncGen {
    /// A new generator: 1 V, 1 kHz sine, no offset, 50 % duty, zero phase,
    /// 1 ms span sampled at 256 points. Tune with the builder methods.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            unit: Unit::Volt,
            waveform: Waveform::Sine,
            amplitude: 1.0,
            frequency: 1_000.0,
            offset: 0.0,
            duty: 0.5,
            phase: 0.0,
            span: 1e-3,
            samples: 256,
        }
    }

    /// Set the output waveform shape.
    pub fn waveform(mut self, waveform: Waveform) -> Self {
        self.waveform = waveform;
        self
    }

    /// Set the amplitude (peak, in the signal's unit).
    pub fn amplitude(mut self, amplitude: f64) -> Self {
        self.amplitude = amplitude;
        self
    }

    /// Set the frequency in hertz.
    pub fn frequency(mut self, frequency: f64) -> Self {
        self.frequency = frequency;
        self
    }

    /// Set the DC offset.
    pub fn offset(mut self, offset: f64) -> Self {
        self.offset = offset;
        self
    }

    /// Set the duty cycle (square/pulse), clamped to `[0, 1]`.
    pub fn duty(mut self, duty: f64) -> Self {
        self.duty = duty.clamp(0.0, 1.0);
        self
    }

    /// Set the phase in radians.
    pub fn phase(mut self, phase: f64) -> Self {
        self.phase = phase;
        self
    }

    /// Set the output unit (default [`Unit::Volt`]).
    pub fn unit(mut self, unit: Unit) -> Self {
        self.unit = unit;
        self
    }

    /// Set the time span (seconds) and sample count of the output grid.
    pub fn span(mut self, span: f64, samples: usize) -> Self {
        self.span = span;
        self.samples = samples;
        self
    }

    /// The sample value at time `t` (seconds), before offset is added — the raw
    /// shape scaled by amplitude.
    fn shape(&self, t: f64) -> f64 {
        let a = self.amplitude;
        match self.waveform {
            Waveform::Sine => a * (TAU * self.frequency * t + self.phase).sin(),
            Waveform::Square => {
                if cycle_phase(self.frequency, t, self.phase) < self.duty {
                    a
                } else {
                    -a
                }
            }
            Waveform::Triangle => a * triangle(cycle_phase(self.frequency, t, self.phase)),
            Waveform::Pulse => {
                if cycle_phase(self.frequency, t, self.phase) < self.duty {
                    a
                } else {
                    0.0
                }
            }
        }
    }

    /// Synthesise the waveform as a time-domain [`Signal`].
    ///
    /// The grid is `samples` points at `t_j = j·span/samples` for
    /// `j ∈ [0, samples)`. A zero sample count yields an empty signal.
    pub fn generate(&self) -> Signal {
        let n = self.samples;
        if n == 0 {
            return Signal::time(self.name.clone(), self.unit, vec![], vec![]);
        }
        let dt = self.span / n as f64;
        let x: Vec<f64> = (0..n).map(|j| j as f64 * dt).collect();
        let y: Vec<f64> = x.iter().map(|&t| self.offset + self.shape(t)).collect();
        Signal::time(self.name.clone(), self.unit, x, y)
    }

    /// Render a compact panel describing the configured stimulus.
    pub fn render(&self, width: usize) -> String {
        let width = width.max(16);
        let mut out = String::new();
        out.push_str(&fit(&format!("  FuncGen · {}", self.name), width));
        out.push('\n');
        out.push_str(&kv_line(
            &format!("  [{}]", self.waveform.tag()),
            &fmt_eng(self.frequency, "Hz"),
            width,
        ));
        out.push('\n');
        let line = format!(
            "  amp {}  off {}  φ {}",
            fmt_eng(self.amplitude, self.unit.symbol()),
            fmt_eng(self.offset, self.unit.symbol()),
            fmt_eng(self.phase, "rad"),
        );
        out.push_str(&fit(&line, width));
        out.push('\n');
        out
    }
}

/// The position within the current cycle, in `[0, 1)`, for frequency `f` at
/// time `t` with phase offset `phase` (radians).
fn cycle_phase(f: f64, t: f64, phase: f64) -> f64 {
    let theta = f * t + phase / TAU;
    theta - theta.floor()
}

/// A symmetric triangle in `[-1, 1]` over a unit cycle phase, starting at `0`
/// and rising (so it tracks a sine): `0 → +1 → 0 → −1 → 0`.
fn triangle(phase: f64) -> f64 {
    if phase < 0.25 {
        4.0 * phase
    } else if phase < 0.75 {
        2.0 - 4.0 * phase
    } else {
        4.0 * phase - 4.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oscilloscope::{Oscilloscope, Slope, Timebase, Trigger};

    #[test]
    fn sine_samples_match_the_formula() {
        let (amp, freq, off, phase) = (2.0, 1_000.0, 0.5, 0.3);
        let fg = FuncGen::new("v(stim)")
            .waveform(Waveform::Sine)
            .amplitude(amp)
            .frequency(freq)
            .offset(off)
            .phase(phase)
            .span(2e-3, 512);
        let sig = fg.generate();
        assert_eq!(sig.len(), 512);
        let y = sig.real().unwrap();
        for (j, (&t, &v)) in sig.x.iter().zip(y).enumerate() {
            let want = off + amp * (TAU * freq * t + phase).sin();
            assert!(
                (v - want).abs() < 1e-12,
                "sample {j}: t={t} got {v}, want {want}"
            );
        }
    }

    #[test]
    fn square_has_two_levels_and_requested_duty() {
        let fg = FuncGen::new("sq")
            .waveform(Waveform::Square)
            .amplitude(1.0)
            .frequency(1_000.0)
            .duty(0.25)
            .span(1e-3, 1000); // exactly one period, 1000 samples
        let y = fg.generate();
        let s = y.real().unwrap();
        let high = s.iter().filter(|&&v| v > 0.0).count();
        let low = s.iter().filter(|&&v| v < 0.0).count();
        assert_eq!(high + low, 1000);
        // 25 % duty over one period → ~250 high samples.
        assert!((high as i64 - 250).abs() <= 1, "high = {high}");
        assert!((low as i64 - 750).abs() <= 1, "low = {low}");
        // Only the two bipolar levels appear.
        assert!(s
            .iter()
            .all(|&v| (v - 1.0).abs() < 1e-12 || (v + 1.0).abs() < 1e-12));
    }

    #[test]
    fn triangle_peaks_at_plus_minus_amplitude_through_zero() {
        let fg = FuncGen::new("tri")
            .waveform(Waveform::Triangle)
            .amplitude(3.0)
            .frequency(1_000.0)
            .span(1e-3, 4000);
        let s = fg.generate();
        let y = s.real().unwrap();
        let max = y.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let min = y.iter().copied().fold(f64::INFINITY, f64::min);
        assert!((max - 3.0).abs() < 1e-2, "peak = {max}");
        assert!((min + 3.0).abs() < 1e-2, "trough = {min}");
        assert!(y[0].abs() < 1e-9, "triangle starts at 0, got {}", y[0]);
    }

    #[test]
    fn pulse_is_unipolar_with_requested_duty() {
        let fg = FuncGen::new("pulse")
            .waveform(Waveform::Pulse)
            .amplitude(5.0)
            .offset(0.0)
            .frequency(1_000.0)
            .duty(0.1)
            .span(1e-3, 1000);
        let s = fg.generate();
        let y = s.real().unwrap();
        let high = y.iter().filter(|&&v| v > 2.5).count();
        assert!((high as i64 - 100).abs() <= 1, "high = {high}");
        // Unipolar: baseline is the offset (0), peak is offset+amp (5).
        assert!(y
            .iter()
            .all(|&v| (v - 5.0).abs() < 1e-12 || v.abs() < 1e-12));
    }

    #[test]
    fn generated_sine_shows_on_the_scope_as_a_stable_frame() {
        // Six periods of a 1 kHz sine, generated by the funcgen.
        let fg = FuncGen::new("v(sig)")
            .waveform(Waveform::Sine)
            .amplitude(1.0)
            .frequency(1_000.0)
            .span(6e-3, 600);
        let mk = || {
            Oscilloscope::new("scope")
                .size(48, 6)
                .timebase(Timebase::new(2e-3))
                .trigger(Trigger::Edge {
                    source: 0,
                    level: 0.0,
                    slope: Slope::Rising,
                })
                .channel(fg.generate())
                .render()
        };
        let frame = mk();
        // It is a valid, non-empty render with braille trace glyphs drawn.
        assert!(frame.contains("v(sig)"));
        assert!(frame
            .chars()
            .any(|c| ('\u{2801}'..='\u{28FF}').contains(&c)));
        // Deterministic: the triggered frame is stable across captures.
        assert_eq!(frame, mk(), "triggered frame must be stable");
    }

    #[test]
    fn zero_sample_count_does_not_panic() {
        let s = FuncGen::new("v").span(1e-3, 0).generate();
        assert!(s.is_empty());
        let _ = FuncGen::new("v").render(24);
    }
}
