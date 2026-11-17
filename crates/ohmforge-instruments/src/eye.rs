//! Eye diagram — a bitstream / periodic signal folded into overlaid unit
//! intervals (`CP-UX-M9.2`).
//!
//! A serial link's quality is read from its *eye*: slice the received waveform
//! into one-symbol-wide windows and overlay them, and the transitions trace out
//! the outline of an eye whose open centre is the margin a receiver samples in.
//! A wide-open eye is a clean link; a closed one is inter-symbol interference,
//! jitter or noise.
//!
//! This panel folds a **provided** signal — a `Vec<f64>` of uniformly-spaced
//! samples plus the symbol period in samples — it runs no simulation itself.
//! Feeding it a transient node voltage is a thin follow-up. Like the other panels
//! it depends only on `ohmforge-core`, reuses the shared braille canvas so an
//! overlaid trace looks like the scope's, and renders to a deterministic,
//! golden-file-testable `String`.

use crate::braille::Canvas;
use crate::fmt::{fit, fmt_num};

/// The minimum panel extent.
const MIN_W: usize = 16;
const MIN_H: usize = 4;

/// Measured eye-opening geometry, in the signal's own amplitude units.
///
/// All amplitudes are in the units of the folded samples;
/// [`decision_time`](EyeMetrics::decision_time) is a
/// fraction of the unit interval. Computed by [`EyeDiagram::metrics`] directly
/// from the folded traces, independent of the rendered grid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EyeMetrics {
    /// Vertical eye opening at the decision instant: the gap between the lowest
    /// "high" trace and the highest "low" trace. Positive ⇒ an open eye; `≤ 0` ⇒
    /// the eye is closed.
    pub opening_height: f64,
    /// The high rail level (max folded sample).
    pub level_high: f64,
    /// The low rail level (min folded sample).
    pub level_low: f64,
    /// The amplitude the rising/falling transitions cross at — the eye crossing
    /// level, the midpoint of the two rails and the ideal decision threshold.
    pub crossing_level: f64,
    /// Where in the unit interval the decision instant sits (the widest-opening
    /// phase), as a fraction `0..1`.
    pub decision_time: f64,
    /// How many unit intervals were overlaid.
    pub traces: usize,
    /// Samples per symbol (the fold period).
    pub samples_per_symbol: usize,
}

/// An eye diagram panel: folds a sampled signal into overlaid unit intervals.
///
/// Build with [`new`](EyeDiagram::new), optionally [`size`](EyeDiagram::size) it,
/// then [`render`](EyeDiagram::render) a signal, or read [`metrics`](EyeDiagram::metrics)
/// / [`is_open`](EyeDiagram::is_open) for the numeric opening.
pub struct EyeDiagram<'a> {
    /// Heading shown above the frame.
    pub title: &'a str,
    /// Frame width, in character cells.
    pub width: usize,
    /// Plot height, in character cells.
    pub height: usize,
}

impl<'a> EyeDiagram<'a> {
    /// A sensible default: 48 cells wide, an 8-cell-tall plot.
    pub fn new(title: &'a str) -> Self {
        Self {
            title,
            width: 48,
            height: 8,
        }
    }

    /// Set the frame width and plot height, in character cells.
    pub fn size(mut self, width: usize, height: usize) -> Self {
        self.width = width.max(MIN_W);
        self.height = height.max(MIN_H);
        self
    }

    /// Fold `samples` into one-symbol windows, each `sps + 1` samples long so both
    /// unit-interval boundaries are included. Consecutive windows step by one
    /// symbol, so every symbol transition is captured. Returns the overlaid
    /// traces; empty when there is less than one full window or `sps == 0`.
    fn fold(&self, samples: &[f64], sps: usize) -> Vec<Vec<f64>> {
        if sps == 0 {
            return Vec::new();
        }
        let mut traces = Vec::new();
        let mut start = 0;
        while start + sps < samples.len() {
            traces.push(samples[start..=start + sps].to_vec());
            start += sps;
        }
        traces
    }

    /// Measure the eye opening of `samples` folded at `sps` samples per symbol.
    ///
    /// Scans every phase of the unit interval for the one with the widest vertical
    /// gap between the cluster of "high" traces and the cluster of "low" traces;
    /// that gap is the [`opening_height`](EyeMetrics::opening_height) and its phase
    /// the [`decision_time`](EyeMetrics::decision_time). The crossing level is the
    /// mid-level of the two rails. Degenerate input (no full window) yields a
    /// zero-opening report.
    pub fn metrics(&self, samples: &[f64], sps: usize) -> EyeMetrics {
        let traces = self.fold(samples, sps);
        let zero = EyeMetrics {
            opening_height: 0.0,
            level_high: 0.0,
            level_low: 0.0,
            crossing_level: 0.0,
            decision_time: 0.0,
            traces: traces.len(),
            samples_per_symbol: sps,
        };
        if traces.is_empty() {
            return zero;
        }
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for t in &traces {
            for &v in t {
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
        let mid = (lo + hi) / 2.0;

        // Find the phase (sample index within the UI) with the widest gap between
        // the lowest "high" trace and the highest "low" trace.
        let mut best_gap = f64::NEG_INFINITY;
        let mut best_phase = sps / 2;
        for phase in 0..=sps {
            let (mut hi_min, mut lo_max) = (f64::INFINITY, f64::NEG_INFINITY);
            let (mut n_hi, mut n_lo) = (0usize, 0usize);
            for t in &traces {
                let v = t[phase];
                if v >= mid {
                    hi_min = hi_min.min(v);
                    n_hi += 1;
                } else {
                    lo_max = lo_max.max(v);
                    n_lo += 1;
                }
            }
            if n_hi == 0 || n_lo == 0 {
                continue;
            }
            let gap = hi_min - lo_max;
            if gap > best_gap {
                best_gap = gap;
                best_phase = phase;
            }
        }
        let opening_height = if best_gap.is_finite() { best_gap } else { 0.0 };

        EyeMetrics {
            opening_height,
            level_high: hi,
            level_low: lo,
            // Symmetric two-level signaling crosses at the mid-level — the ideal
            // receiver decision threshold.
            crossing_level: mid,
            decision_time: best_phase as f64 / sps as f64,
            traces: traces.len(),
            samples_per_symbol: sps,
        }
    }

    /// Whether the eye is open: a positive [`opening_height`](EyeMetrics::opening_height).
    pub fn is_open(&self, samples: &[f64], sps: usize) -> bool {
        self.metrics(samples, sps).opening_height > 0.0
    }

    /// Fold and overlay the traces onto a shared braille [`Canvas`], returning it
    /// (and the amplitude range used) so both [`render`](EyeDiagram::render) and
    /// the tests can probe the same dots.
    fn canvas(&self, samples: &[f64], sps: usize) -> (Canvas, f64, f64) {
        let plot_w = self.width.max(MIN_W).saturating_sub(2).max(1);
        let plot_h = self.height.max(MIN_H);
        let mut canvas = Canvas::new(plot_w, plot_h);
        let traces = self.fold(samples, sps);
        if traces.is_empty() {
            return (canvas, 0.0, 1.0);
        }
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for t in &traces {
            for &v in t {
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
        // A small amplitude margin so the rails sit just inside the frame.
        let span = (hi - lo).max(f64::EPSILON);
        let (ymin, ymax) = (lo - 0.1 * span, hi + 0.1 * span);
        let dot_w = plot_w * 2;
        for t in &traces {
            let cols: Vec<f64> = (0..dot_w)
                .map(|px| {
                    let phase = px as f64 / (dot_w - 1).max(1) as f64 * sps as f64;
                    sample_at(t, phase)
                })
                .collect();
            canvas.plot(&cols, ymin, ymax);
        }
        (canvas, ymin, ymax)
    }

    /// Render the folded eye to a framed, multi-line string: a heading, a readout
    /// of the eye opening and crossing level, and the overlaid braille traces.
    pub fn render(&self, samples: &[f64], sps: usize) -> String {
        let width = self.width.max(MIN_W);
        let m = self.metrics(samples, sps);
        let (canvas, _, _) = self.canvas(samples, sps);

        let mut out = String::new();
        out.push_str("  ");
        out.push_str(&fit(self.title, width.saturating_sub(2)));
        out.push('\n');
        let readout = format!(
            "  eye {}  cross {}  {} UI",
            fmt_num(m.opening_height),
            fmt_num(m.crossing_level),
            m.traces
        );
        out.push_str(&fit(&readout, width));
        out.push('\n');
        for row in canvas.rows() {
            out.push_str(&fit(&row, width));
            out.push('\n');
        }
        out
    }

    /// The braille canvas the eye draws onto, for golden/opening probes in tests.
    #[cfg(test)]
    fn test_canvas(&self, samples: &[f64], sps: usize) -> Canvas {
        self.canvas(samples, sps).0
    }
}

/// Linearly interpolate trace `t` at fractional sample index `phase`.
fn sample_at(t: &[f64], phase: f64) -> f64 {
    if t.is_empty() {
        return f64::NAN;
    }
    let i = phase.floor() as usize;
    if i >= t.len() - 1 {
        return t[t.len() - 1];
    }
    let frac = phase - i as f64;
    t[i] * (1.0 - frac) + t[i + 1] * frac
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an NRZ waveform: each bit held flat for `sps` samples.
    fn nrz(bits: &[u8], sps: usize) -> Vec<f64> {
        let mut s = Vec::new();
        for &b in bits {
            for _ in 0..sps {
                s.push(b as f64);
            }
        }
        s
    }

    /// A clean alternating `0101…` pattern folds into a wide-open eye: a full-swing
    /// opening at the decision instant and a crossing level midway between the
    /// rails. The rendered grid has a clear (dot-free) band through the eye centre.
    #[test]
    fn eye_diagram_folds_a_known_pattern() {
        let sps = 8;
        let samples = nrz(&[0, 1, 0, 1, 0, 1, 0, 1, 0, 1], sps);
        let eye = EyeDiagram::new("eye");
        let m = eye.metrics(&samples, sps);

        assert!(m.traces >= 8, "most unit intervals overlaid: {}", m.traces);
        assert_eq!(m.level_low, 0.0);
        assert_eq!(m.level_high, 1.0);
        // Full-swing NRZ → the eye opens the whole way at the decision instant.
        assert!(
            (m.opening_height - 1.0).abs() < 1e-9,
            "opening {}",
            m.opening_height
        );
        // Balanced alternating data crosses dead centre.
        assert!(
            (m.crossing_level - 0.5).abs() < 1e-9,
            "crossing {}",
            m.crossing_level
        );
        assert!(eye.is_open(&samples, sps));

        // The rendered grid is open: at the decision column (UI centre), the dot
        // at mid-amplitude is clear (no trace passes through the eye).
        let canvas = eye.test_canvas(&samples, sps);
        let plot_w = eye.width - 2;
        let dot_w = plot_w * 2;
        let dot_h = eye.height * 4;
        let mid_px = dot_w / 2; // decision instant, centre of the UI
        let mid_py = dot_h / 2; // mid amplitude
                                // Scan a small vertical band around the centre: it must contain a clear dot.
        let open = (mid_py.saturating_sub(2)..=mid_py + 2).any(|py| !canvas.dot_set(mid_px, py));
        assert!(open, "the eye centre is open in the rendered grid");
    }

    /// A constant (never-transitioning) signal has no eye — one cluster only, so
    /// the opening is reported as zero rather than panicking.
    #[test]
    fn a_constant_signal_has_no_open_eye() {
        let eye = EyeDiagram::new("eye");
        let samples = vec![1.0; 64];
        let m = eye.metrics(&samples, 8);
        assert_eq!(m.opening_height, 0.0);
        assert!(!eye.is_open(&samples, 8));
    }

    /// Degenerate input (no full unit interval, or a zero period) is a clean,
    /// empty report.
    #[test]
    fn short_or_zero_period_input_is_clean() {
        let eye = EyeDiagram::new("eye");
        assert_eq!(eye.metrics(&[1.0, 2.0, 3.0], 8).traces, 0);
        assert_eq!(eye.metrics(&[1.0; 64], 0).traces, 0);
        // Render never panics on degenerate input.
        let _ = eye.render(&[], 8);
    }
}
