//! Spectrum view — magnitude (dB) vs **linear frequency** (`CP-UX-M2.3`).
//!
//! The companion to the [`Bode`](crate::bode::Bode) panel: where Bode plots an
//! AC transfer function against log frequency, the spectrum view renders the
//! one-sided magnitude spectrum produced by the scope's FFT channel
//! ([`magnitude_spectrum`](crate::fft::magnitude_spectrum)) against a linear
//! frequency axis, in decibels relative to the strongest bin. Zero bins (−∞ dB)
//! are floored so the display has a sensible dynamic range.
//!
//! Same contract as the other instrument views: `ee-core`-only, braille, and a
//! deterministic `String` render.

use crate::braille;
use crate::fft::peak_bin;
use crate::fmt::{fit, fmt_eng, fmt_num};
use ee_core::signal::{Domain, Signal, SignalData};

/// A magnitude spectrum display (dB vs linear frequency).
pub struct Spectrum<'a> {
    /// Heading shown above the frame.
    pub title: &'a str,
    /// Total frame width, in character cells.
    pub width: usize,
    /// Plot height, in character cells.
    pub height: usize,
    /// Displayed dynamic range below the peak, in dB (the noise floor).
    pub floor_db: f64,
}

impl<'a> Spectrum<'a> {
    /// A sensible default: 72×12 cells, 80 dB of dynamic range.
    pub fn new(title: &'a str) -> Self {
        Self {
            title,
            width: 72,
            height: 12,
            floor_db: 80.0,
        }
    }

    /// Set the frame width and plot height, in character cells.
    pub fn size(mut self, width: usize, height: usize) -> Self {
        self.width = width.max(16);
        self.height = height.max(2);
        self
    }

    /// Set the displayed dynamic range below the peak, in dB.
    pub fn floor(mut self, floor_db: f64) -> Self {
        self.floor_db = floor_db.max(1.0);
        self
    }

    /// Render a one-sided magnitude spectrum (a `Freq` complex `Signal`) to a
    /// framed, multi-line string. A non-`Freq`/non-complex or too-short input
    /// renders an empty (but framed) view rather than panicking.
    pub fn render(&self, sig: &Signal) -> String {
        let width = self.width.max(16);
        let plot_h = self.height.max(2);

        let bins = match (sig.domain, &sig.y) {
            (Domain::Freq, SignalData::Complex(c)) if c.len() >= 2 && sig.x.len() == c.len() => {
                Some(c)
            }
            _ => None,
        };

        // Peak level sets the top of the scale; the floor sits `floor_db` below.
        let peak = bins
            .map(|c| {
                c.iter()
                    .map(|z| z.magnitude_db())
                    .filter(|v| v.is_finite())
                    .fold(f64::NEG_INFINITY, f64::max)
            })
            .filter(|p| p.is_finite());
        let (ymax, ymin) = match peak {
            Some(p) => (p, p - self.floor_db),
            None => (0.0, 0.0),
        };

        let top = format!("{}dB", fmt_num(ymax));
        let bot = format!("{}dB", fmt_num(ymin));
        let gutter = top.chars().count().max(bot.chars().count()).max(1);
        let plot_w = width.saturating_sub(gutter + 2).max(1);
        let dot_w = plot_w * 2;

        let mut out = String::new();
        out.push_str("  ");
        out.push_str(&fit(self.title, width.saturating_sub(2)));
        out.push('\n');

        // Readout: the dominant bin.
        let readout = match peak_bin(sig) {
            Some((f, db)) => format!("  peak {} @ {}", fmt_num(db) + "dB", fmt_eng(f, "Hz")),
            None => "  peak --".to_string(),
        };
        out.push_str(&fit(&readout, width));
        out.push('\n');

        let (fmin, fmax) = bins
            .map(|_| (sig.x[0], sig.x[sig.x.len() - 1]))
            .unwrap_or((0.0, 0.0));

        let rows = match bins {
            Some(c) => {
                // Magnitude in dB per bin, floored so −∞ (zero bins) is drawable.
                let db: Vec<f64> = c
                    .iter()
                    .map(|z| {
                        let d = z.magnitude_db();
                        if d.is_finite() {
                            d.max(ymin)
                        } else {
                            ymin
                        }
                    })
                    .collect();
                let cols = sample_columns(&sig.x, &db, fmin, fmax, dot_w);
                braille::trace_rows(&cols, ymin, ymax, plot_w, plot_h)
            }
            None => braille::trace_rows(&[], 0.0, 1.0, plot_w, plot_h),
        };

        for (cy, row) in rows.iter().enumerate() {
            let label = if cy == 0 {
                top.as_str()
            } else if cy + 1 == rows.len() {
                bot.as_str()
            } else {
                ""
            };
            out.push_str(&format!("{label:>gutter$} \u{2524}{row}\n"));
        }

        // Linear frequency axis: a rule, then end labels.
        out.push_str(&format!("{:>gutter$} \u{2514}", ""));
        out.push_str(&"\u{2500}".repeat(plot_w));
        out.push('\n');
        if bins.is_some() {
            let xlo = fmt_eng(fmin, "Hz");
            let xhi = fmt_eng(fmax, "Hz");
            let pad = plot_w.saturating_sub(xlo.len() + xhi.len()).max(1);
            out.push_str(&fit(
                &format!("{:>gutter$}  {xlo}{}{xhi}", "", " ".repeat(pad)),
                width,
            ));
            out.push('\n');
        }
        out
    }
}

/// Sample `vals` at each dot-column over a linear frequency axis `[fmin, fmax]`.
fn sample_columns(freqs: &[f64], vals: &[f64], fmin: f64, fmax: f64, dot_w: usize) -> Vec<f64> {
    (0..dot_w)
        .map(|px| {
            let t = if dot_w > 1 {
                px as f64 / (dot_w - 1) as f64
            } else {
                0.0
            };
            interp(freqs, vals, fmin + (fmax - fmin) * t)
        })
        .collect()
}

/// Linearly interpolate `y` at `xd` over a non-decreasing `x`. Clamps at ends.
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
    use crate::fft::{magnitude_spectrum, Window};
    use ee_core::signal::Complex;
    use ee_core::units::Unit;
    use std::f64::consts::TAU;

    fn sine_spectrum(f0: f64, fs: f64, n: usize) -> Signal {
        let dt = 1.0 / fs;
        let x: Vec<f64> = (0..n).map(|i| i as f64 * dt).collect();
        let y: Vec<f64> = (0..n).map(|i| (TAU * f0 * i as f64 * dt).sin()).collect();
        magnitude_spectrum(
            &Signal::time("v(in)", Unit::Volt, x, y),
            Window::Rectangular,
        )
    }

    #[test]
    fn spectrum_renders_peak_and_fits_width() {
        let spec = sine_spectrum(8.0, 64.0, 64);
        let width = 60;
        let view = Spectrum::new("FFT: v(in)").size(width, 8);
        let a = view.render(&spec);
        let b = view.render(&spec);
        assert_eq!(a, b, "render must be deterministic");
        // The peak readout names the 8 Hz tone.
        assert!(a.contains("peak"), "readout:\n{a}");
        assert!(a.contains("8Hz"), "peak frequency in readout:\n{a}");
        assert!(a.contains("dB"), "dB axis:\n{a}");
        assert!(a.chars().any(|c| ('\u{2801}'..='\u{28FF}').contains(&c)));
        for line in a.lines() {
            assert!(
                line.chars().count() <= width,
                "line exceeds width {width}: {line:?}"
            );
        }
    }

    #[test]
    fn empty_and_non_frequency_input_do_not_panic() {
        let empty = Signal::freq("e", Unit::None, Vec::new(), Vec::new());
        assert!(!Spectrum::new("x").size(32, 4).render(&empty).is_empty());

        let t = Signal::time("v", Unit::Volt, vec![0.0, 1.0], vec![0.0, 1.0]);
        let out = Spectrum::new("x").size(32, 4).render(&t);
        assert!(out.contains('\u{2524}')); // still framed

        let one = Signal::freq("h", Unit::None, vec![1.0], vec![Complex::ONE]);
        assert!(!Spectrum::new("x").size(24, 3).render(&one).is_empty());
    }

    #[test]
    fn length_mismatched_spectrum_renders_without_panicking() {
        use ee_core::signal::{Domain, SignalData};
        // A complex signal whose `x` is shorter than its bins. `render` takes its
        // empty-view path (the bins guard fails) but still reads the peak bin for
        // the readout, which must not index past the frequency axis.
        let bad = Signal {
            name: "h".to_string(),
            unit: Unit::None,
            domain: Domain::Freq,
            x: vec![0.0],
            y: SignalData::Complex(vec![Complex::ONE, Complex::ONE, Complex::ONE]),
        };
        let out = Spectrum::new("x").size(32, 4).render(&bad);
        assert!(out.contains('\u{2524}'), "still a framed view:\n{out}");
    }
}
