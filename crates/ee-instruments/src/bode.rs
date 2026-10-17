//! Bode panel — magnitude & phase vs **log frequency** (`CP-UX-M2.3`).
//!
//! AC analysis emits a `Domain::Freq` complex [`Signal`]: one
//! [`Complex`](ee_core::signal::Complex) transfer value per frequency. The Bode
//! view renders it the way an engineer reads a frequency response — magnitude in
//! decibels and phase in degrees, both plotted against a **logarithmic** frequency
//! axis with decade gridlines and labels. The two sub-plots stack, magnitude
//! over phase, sharing one x-axis.
//!
//! Like the [`oscilloscope`](crate::oscilloscope) it reuses the braille canvas
//! and the SI number formatting, depends only on `ee-core`, and renders to a
//! deterministic `String` — no live terminal, golden-file testable.

use crate::braille;
use crate::fmt::{fit, fmt_eng, fmt_num};
use ee_core::signal::{Domain, Signal, SignalData};

/// A Bode plot: a magnitude (dB) sub-plot over a phase (°) sub-plot, both vs
/// log frequency.
pub struct Bode<'a> {
    /// Heading shown above the frame.
    pub title: &'a str,
    /// Total frame width, in character cells.
    pub width: usize,
    /// Height of **each** sub-plot, in character cells.
    pub height: usize,
}

impl<'a> Bode<'a> {
    /// A sensible default: 72 cells wide, 8-cell-tall sub-plots.
    pub fn new(title: &'a str) -> Self {
        Self {
            title,
            width: 72,
            height: 8,
        }
    }

    /// Set the frame width and per-sub-plot height, in character cells.
    pub fn size(mut self, width: usize, height: usize) -> Self {
        self.width = width.max(16);
        self.height = height.max(2);
        self
    }

    /// Render the Bode view of a frequency-domain complex `Signal` to a framed,
    /// multi-line string. A non-`Freq`/non-complex input, or one with fewer than
    /// two positive-frequency samples, renders an empty (but framed) view rather
    /// than panicking.
    pub fn render(&self, sig: &Signal) -> String {
        let width = self.width.max(16);
        let ph_h = self.height.max(2);
        let curves = curves(sig);

        let (mag_lo, mag_hi, pha_lo, pha_hi) = match &curves {
            Some(c) => {
                let (ml, mh) = bounds(&c.mag_db);
                let (pl, ph) = bounds(&c.phase_deg);
                (ml, mh, pl, ph)
            }
            None => (0.0, 0.0, 0.0, 0.0),
        };

        let mag_top = format!("{}dB", fmt_num(mag_hi));
        let mag_bot = format!("{}dB", fmt_num(mag_lo));
        let pha_top = format!("{}°", fmt_num(pha_hi));
        let pha_bot = format!("{}°", fmt_num(pha_lo));
        let gutter = [&mag_top, &mag_bot, &pha_top, &pha_bot]
            .iter()
            .map(|s| s.chars().count())
            .max()
            .unwrap_or(1)
            .max(1);
        let plot_w = width.saturating_sub(gutter + 2).max(1);
        let dot_w = plot_w * 2;

        let mut out = String::new();
        out.push_str("  ");
        out.push_str(&fit(self.title, width.saturating_sub(2)));
        out.push('\n');

        // Readout: the −3 dB corner and the passband peak.
        let corner = corner_3db(sig)
            .map(|f| fmt_eng(f, "Hz"))
            .unwrap_or_else(|| "--".to_string());
        let peak = curves
            .as_ref()
            .map(|c| {
                c.mag_db
                    .iter()
                    .copied()
                    .filter(|v| v.is_finite())
                    .fold(f64::NEG_INFINITY, f64::max)
            })
            .filter(|p| p.is_finite())
            .map(|p| format!("{}dB", fmt_num(p)))
            .unwrap_or_else(|| "--".to_string());
        out.push_str(&fit(&format!("  f(-3dB) {corner}   peak {peak}"), width));
        out.push('\n');

        let (lmin, lmax) = curves
            .as_ref()
            .map(|c| (c.log_f[0], c.log_f[c.log_f.len() - 1]))
            .unwrap_or((0.0, 0.0));

        // Magnitude sub-plot.
        let mag_rows = match &curves {
            Some(c) => braille::trace_rows(
                &sample_columns(&c.log_f, &c.mag_db, lmin, lmax, dot_w),
                mag_lo,
                mag_hi,
                plot_w,
                ph_h,
            ),
            None => braille::trace_rows(&[], 0.0, 1.0, plot_w, ph_h),
        };
        push_block(
            &mut out, "|H| (dB)", &mag_top, &mag_bot, gutter, &mag_rows, width,
        );

        // Phase sub-plot.
        let pha_rows = match &curves {
            Some(c) => braille::trace_rows(
                &sample_columns(&c.log_f, &c.phase_deg, lmin, lmax, dot_w),
                pha_lo,
                pha_hi,
                plot_w,
                ph_h,
            ),
            None => braille::trace_rows(&[], 0.0, 1.0, plot_w, ph_h),
        };
        push_block(
            &mut out,
            "∠H (°)",
            &pha_top,
            &pha_bot,
            gutter,
            &pha_rows,
            width,
        );

        // Shared log-frequency axis with decade ticks and labels.
        let (rule, labels) = decade_axis(lmin, lmax, plot_w, gutter);
        out.push_str(&fit(&rule, width));
        out.push('\n');
        if curves.is_some() {
            out.push_str(&fit(&labels, width));
            out.push('\n');
        }
        out
    }
}

/// The −3 dB corner frequency: where the magnitude first falls 3 dB below its
/// low-frequency (passband) level. Interpolated in log-frequency space. `None`
/// for a non-frequency input or a response that never drops 3 dB.
pub fn corner_3db(sig: &Signal) -> Option<f64> {
    let c = curves(sig)?;
    let reference = c.mag_db[0];
    let target = reference - 3.0;
    for i in 1..c.mag_db.len() {
        let (m0, m1) = (c.mag_db[i - 1], c.mag_db[i]);
        if m0 > target && m1 <= target {
            let dm = m1 - m0;
            let lf = if dm.abs() < f64::EPSILON {
                c.log_f[i]
            } else {
                c.log_f[i - 1] + (c.log_f[i] - c.log_f[i - 1]) * ((target - m0) / dm)
            };
            return Some(10f64.powf(lf));
        }
    }
    None
}

/// The positive-frequency curves pulled out of a frequency-domain signal.
struct Curves {
    log_f: Vec<f64>,
    mag_db: Vec<f64>,
    phase_deg: Vec<f64>,
}

/// Extract `log10(f)`, magnitude-dB and phase-degree arrays for every
/// positive, finite frequency. `None` unless the signal is `Freq` + complex
/// with at least two such points (log frequency needs `f > 0`).
fn curves(sig: &Signal) -> Option<Curves> {
    let c = match (sig.domain, &sig.y) {
        (Domain::Freq, SignalData::Complex(c)) => c,
        _ => return None,
    };
    let mut log_f = Vec::new();
    let mut mag_db = Vec::new();
    let mut phase_deg = Vec::new();
    for (&f, z) in sig.x.iter().zip(c) {
        if f > 0.0 && f.is_finite() {
            log_f.push(f.log10());
            mag_db.push(z.magnitude_db());
            phase_deg.push(z.phase_deg());
        }
    }
    if log_f.len() < 2 {
        return None;
    }
    Some(Curves {
        log_f,
        mag_db,
        phase_deg,
    })
}

/// Finite min/max, padding a flat series by ±1 so it lands mid-frame.
fn bounds(vals: &[f64]) -> (f64, f64) {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for &v in vals.iter().filter(|v| v.is_finite()) {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    if !lo.is_finite() {
        return (0.0, 0.0);
    }
    if (hi - lo).abs() < f64::EPSILON {
        (lo - 1.0, hi + 1.0)
    } else {
        (lo, hi)
    }
}

/// Sample `vals` at each dot-column, interpolating in log-frequency space.
fn sample_columns(log_f: &[f64], vals: &[f64], lmin: f64, lmax: f64, dot_w: usize) -> Vec<f64> {
    (0..dot_w)
        .map(|px| {
            let t = if dot_w > 1 {
                px as f64 / (dot_w - 1) as f64
            } else {
                0.0
            };
            interp(log_f, vals, lmin + (lmax - lmin) * t)
        })
        .collect()
}

/// Append one sub-plot: a section title, then `rows` with gutter axis labels.
fn push_block(
    out: &mut String,
    section: &str,
    top: &str,
    bot: &str,
    gutter: usize,
    rows: &[String],
    width: usize,
) {
    out.push_str(&fit(&format!("  {section}"), width));
    out.push('\n');
    let h = rows.len();
    for (cy, row) in rows.iter().enumerate() {
        let label = if cy == 0 {
            top
        } else if cy + 1 == h {
            bot
        } else {
            ""
        };
        out.push_str(&format!("{label:>gutter$} \u{2524}{row}\n"));
    }
}

/// Build the decade-ruled x-axis: a rule line with `┬` at each power-of-ten and
/// a label line placing `1Hz`, `10Hz`, `1kHz`… greedily without overlap.
fn decade_axis(lmin: f64, lmax: f64, plot_w: usize, gutter: usize) -> (String, String) {
    let mut rule: Vec<char> = vec!['\u{2500}'; plot_w];
    let mut labels: Vec<char> = vec![' '; plot_w];
    let span = lmax - lmin;
    if span > 0.0 && plot_w >= 1 {
        let mut next_free = 0usize;
        let e0 = lmin.ceil() as i64;
        let e1 = lmax.floor() as i64;
        for e in e0..=e1 {
            let t = (e as f64 - lmin) / span;
            let col = ((t * (plot_w.saturating_sub(1)) as f64).round() as usize).min(plot_w - 1);
            rule[col] = '\u{252C}'; // ┬
            let label: Vec<char> = fmt_eng(10f64.powf(e as f64), "Hz").chars().collect();
            if label.len() <= plot_w {
                let start = col.min(plot_w - label.len());
                if start >= next_free {
                    for (k, ch) in label.iter().enumerate() {
                        labels[start + k] = *ch;
                    }
                    next_free = start + label.len() + 1;
                }
            }
        }
    }
    let rule_line = format!(
        "{:>gutter$} \u{2514}{}",
        "",
        rule.into_iter().collect::<String>()
    );
    let label_str: String = labels.into_iter().collect();
    let label_line = format!("{}{}", " ".repeat(gutter + 2), label_str.trim_end());
    (rule_line, label_line)
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
    use ee_core::signal::Complex;
    use ee_core::units::Unit;

    /// A first-order RC low-pass response `H(f) = 1 / (1 + j f/fc)` sampled on a
    /// log-spaced grid from 1 Hz to 1 MHz (10 points per decade).
    fn rc_lowpass(fc: f64) -> Signal {
        let freqs: Vec<f64> = (0..=60).map(|i| 10f64.powf(i as f64 / 10.0)).collect();
        let data: Vec<Complex> = freqs
            .iter()
            .map(|&f| Complex::ONE / Complex::new(1.0, f / fc))
            .collect();
        Signal::freq("H", Unit::None, freqs, data)
    }

    #[test]
    fn rc_response_renders_magnitude_and_phase() {
        let fc = 1_000.0;
        let sig = rc_lowpass(fc);
        let width = 80;
        let bode = Bode::new("AC: v(out)/v(in)").size(width, 6);
        let a = bode.render(&sig);
        let b = bode.render(&sig);
        assert_eq!(a, b, "render must be deterministic");

        // Both sub-plots are present and labelled.
        assert!(a.contains("|H| (dB)"), "magnitude sub-plot:\n{a}");
        assert!(a.contains("∠H (°)"), "phase sub-plot:\n{a}");
        assert!(a.contains("f(-3dB)"), "corner readout:\n{a}");
        // Decade gridline + labels on the log axis.
        assert!(a.contains('\u{252C}'), "decade tick marks:\n{a}");
        assert!(
            a.contains("1kHz") && a.contains("1MHz"),
            "decade labels:\n{a}"
        );
        // Braille traces were actually drawn.
        assert!(a.chars().any(|c| ('\u{2801}'..='\u{28FF}').contains(&c)));
        // Every line fits the frame width.
        for line in a.lines() {
            assert!(
                line.chars().count() <= width,
                "line exceeds width {width}: {line:?}"
            );
        }
    }

    #[test]
    fn corner_is_near_fc_with_passband_and_rolloff() {
        let fc = 1_000.0;
        let sig = rc_lowpass(fc);
        // The −3 dB corner sits within a few percent of fc.
        let corner = corner_3db(&sig).expect("a corner");
        assert!(
            (corner - fc).abs() / fc < 0.05,
            "corner {corner} Hz vs fc {fc}"
        );

        // Passband is ~0 dB and phase swings from ~0° toward −90°.
        let c = sig.complex().unwrap();
        let peak = c
            .iter()
            .map(|z| z.magnitude_db())
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(peak.abs() < 0.01, "passband peak {peak} dB");
        let min_phase = c
            .iter()
            .map(|z| z.phase_deg())
            .fold(f64::INFINITY, f64::min);
        assert!(
            min_phase < -89.0,
            "phase should approach −90°, got {min_phase}"
        );
        // Roll-off is ~−20 dB/decade: from 10·fc to 100·fc the magnitude drops ~20 dB.
        let db_at = |f: f64| (Complex::ONE / Complex::new(1.0, f / fc)).magnitude_db();
        let slope = db_at(100.0 * fc) - db_at(10.0 * fc);
        assert!((slope + 20.0).abs() < 0.2, "roll-off {slope} dB/decade");
    }

    #[test]
    fn non_frequency_or_tiny_input_renders_without_panic() {
        // A time-domain signal is not a Bode input → empty (but framed) view.
        let t = Signal::time("v", Unit::Volt, vec![0.0, 1.0], vec![0.0, 1.0]);
        let out = Bode::new("x").size(40, 4).render(&t);
        assert!(out.contains("|H| (dB)") && out.contains("∠H (°)"));
        assert!(corner_3db(&t).is_none());

        // A single-point frequency signal is too short for a log axis.
        let one = Signal::freq("h", Unit::None, vec![1.0], vec![Complex::ONE]);
        assert!(!Bode::new("x").size(32, 3).render(&one).is_empty());
    }
}
