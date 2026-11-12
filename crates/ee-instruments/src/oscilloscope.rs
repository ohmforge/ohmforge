//! Oscilloscope — multi-channel, triggered, cursor-measured scope (`CP-UX-M1.1`).
//!
//! The scope *consumes* [`Signal`]s (one per channel) and *produces a view*: a
//! framed, multi-line braille render plus a set of cursor measurements. Like the
//! [`Plot`](../../ee_tui/plot/struct.Plot.html) widget it is deliberately
//! dependency-light (it depends only on `ee-core`) and renders to a `String`,
//! which keeps it deterministic and golden-file testable with no live terminal.
//!
//! ## Trigger — the lock mechanism
//!
//! A free-running scope starts each frame at an arbitrary capture phase, so a
//! periodic waveform appears to drift. A **trigger** fixes that: it finds the
//! first point where a chosen *source* channel crosses a *level* with a given
//! *slope* (rising/falling), interpolates the sub-sample crossing **time**, and
//! anchors the displayed window there. The window's samples — and the x-axis
//! labels — are then expressed *relative to the trigger*, so two captures of the
//! same periodic signal at different phases render **byte-for-byte identically**.
//! That phase-independence is the `CP-UX-M1.1` stability gate.
//!
//! ## Timebase & cursors
//!
//! The [`Timebase`] gives the visible time span and a horizontal offset from the
//! trigger. Two time [`Cursors`] report `Δt`, `Δv` (on a chosen channel) and the
//! implied frequency `1/Δt` as a [`CursorReadout`].

use ee_core::signal::Signal;

/// Dot bit-weights for the Braille Patterns block, indexed `DOTS[col][row]`.
/// Matches [`ee_tui::plot`](../../ee_tui/plot/index.html) so traces look the same.
const DOTS: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];

/// The base code point of the Braille Patterns block; add the dot byte to it.
const BRAILLE_BASE: u32 = 0x2800;

/// Which edge of a level crossing arms the trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slope {
    /// Crossing the level from below (the signal is increasing).
    Rising,
    /// Crossing the level from above (the signal is decreasing).
    Falling,
}

/// How the scope decides where the displayed window begins.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Trigger {
    /// No trigger: the window starts at the beginning of the capture. A periodic
    /// signal will drift frame-to-frame with the capture phase.
    FreeRun,
    /// Edge/level trigger: lock to the first `slope` crossing of `level` on the
    /// `source` channel. The window is anchored to the (interpolated) crossing.
    Edge {
        /// Index of the channel the trigger watches.
        source: usize,
        /// Threshold the source channel must cross.
        level: f64,
        /// Which crossing direction arms the trigger.
        slope: Slope,
    },
}

/// The visible horizontal window: a span and an offset from the trigger point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timebase {
    /// Visible time span, in seconds (the width of the window).
    pub span: f64,
    /// Offset of the window start relative to the trigger point, in seconds.
    /// Negative shows pre-trigger samples; zero starts the window at the trigger.
    pub offset: f64,
}

impl Timebase {
    /// A window of `span` seconds anchored exactly at the trigger point.
    pub fn new(span: f64) -> Self {
        Self { span, offset: 0.0 }
    }

    /// Set the horizontal offset from the trigger (seconds).
    pub fn offset(mut self, offset: f64) -> Self {
        self.offset = offset;
        self
    }
}

/// Two time cursors placed on the (trigger-relative) timebase.
///
/// Cursor times are measured **from the window start** (the trigger point), so
/// they are phase-independent just like the render.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cursors {
    /// First cursor time, relative to the window start (seconds).
    pub t1: f64,
    /// Second cursor time, relative to the window start (seconds).
    pub t2: f64,
    /// Channel the voltage delta `Δv` is measured on.
    pub source: usize,
}

impl Cursors {
    /// Two time cursors at `t1`/`t2` (relative to trigger), measuring `source`.
    pub fn new(t1: f64, t2: f64, source: usize) -> Self {
        Self { t1, t2, source }
    }
}

/// What the cursors measure: the time, voltage and frequency deltas.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CursorReadout {
    /// `Δt = |t2 - t1|`, in seconds.
    pub dt: f64,
    /// `Δv`, the source channel's value at `t2` minus its value at `t1`.
    pub dv: f64,
    /// `1/Δt`, the frequency implied by the cursor spacing (hertz); infinite if
    /// the cursors coincide.
    pub freq: f64,
}

/// A multi-channel oscilloscope: channels + trigger + timebase + cursors.
///
/// Build it, add channels, choose a trigger and timebase, and call
/// [`render`](Oscilloscope::render) for the view or [`measure`](Oscilloscope::measure)
/// for the cursor readout.
pub struct Oscilloscope {
    /// Heading shown above the frame.
    title: String,
    /// The displayed channels, drawn top-to-bottom in order.
    channels: Vec<Signal>,
    /// How the window is anchored.
    trigger: Trigger,
    /// The visible time window.
    timebase: Timebase,
    /// Optional time cursors.
    cursors: Option<Cursors>,
    /// Total frame width, in character cells.
    width: usize,
    /// Per-channel plot height, in character cells.
    channel_height: usize,
}

impl Oscilloscope {
    /// A new, empty scope: free-running, 72 cells wide, 6-cell-tall channels.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            channels: Vec::new(),
            trigger: Trigger::FreeRun,
            timebase: Timebase::new(1.0),
            cursors: None,
            width: 72,
            channel_height: 6,
        }
    }

    /// Add a channel (consumes the [`Signal`]). Channels draw in insertion order.
    pub fn channel(mut self, signal: Signal) -> Self {
        self.channels.push(signal);
        self
    }

    /// Set the trigger mode.
    pub fn trigger(mut self, trigger: Trigger) -> Self {
        self.trigger = trigger;
        self
    }

    /// Set the visible time window.
    pub fn timebase(mut self, timebase: Timebase) -> Self {
        self.timebase = timebase;
        self
    }

    /// Place two time cursors.
    pub fn cursors(mut self, cursors: Cursors) -> Self {
        self.cursors = Some(cursors);
        self
    }

    /// Set the frame width and per-channel height, in character cells.
    pub fn size(mut self, width: usize, channel_height: usize) -> Self {
        self.width = width.max(16);
        self.channel_height = channel_height.max(2);
        self
    }

    /// Number of channels.
    pub fn len(&self) -> usize {
        self.channels.len()
    }

    /// Whether the scope has no channels.
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    /// The resolved trigger time, in the source channel's own (absolute) axis.
    ///
    /// `None` for [`Trigger::FreeRun`], for a missing/complex/too-short source
    /// channel, or when the level is never crossed with the requested slope — in
    /// all of which the window falls back to the start of the capture.
    pub fn trigger_time(&self) -> Option<f64> {
        match self.trigger {
            Trigger::FreeRun => None,
            Trigger::Edge {
                source,
                level,
                slope,
            } => {
                let ch = self.channels.get(source)?;
                let y = ch.real()?;
                find_edge(&ch.x, y, level, slope)
            }
        }
    }

    /// The absolute time at which the displayed window begins.
    fn window_start(&self) -> f64 {
        let base = self
            .trigger_time()
            .or_else(|| self.channels.iter().find_map(|c| c.x.first().copied()))
            .unwrap_or(0.0);
        base + self.timebase.offset
    }

    /// Measure the two time cursors. `None` if no cursors are set or the source
    /// channel is absent/complex.
    pub fn measure(&self) -> Option<CursorReadout> {
        let c = self.cursors?;
        let ch = self.channels.get(c.source)?;
        let y = ch.real()?;
        let start = self.window_start();
        let dt = (c.t2 - c.t1).abs();
        let v1 = interp(&ch.x, y, start + c.t1);
        let v2 = interp(&ch.x, y, start + c.t2);
        let freq = if dt > 0.0 { 1.0 / dt } else { f64::INFINITY };
        Some(CursorReadout {
            dt,
            dv: v2 - v1,
            freq,
        })
    }

    /// Render the scope to a framed, multi-line string.
    ///
    /// The result begins with the title (and a cursor-readout line when cursors
    /// are set), then one labeled braille sub-plot per channel stacked top to
    /// bottom, and a shared trigger-relative x-axis beneath them. Every line is
    /// at most `width` character cells wide.
    pub fn render(&self) -> String {
        let start = self.window_start();
        let span = if self.timebase.span.is_finite() && self.timebase.span > 0.0 {
            self.timebase.span
        } else {
            1.0
        };
        let end = start + span;

        // Pass 1: per-channel vertical bounds (independent of horizontal cells),
        // so the y-axis gutter can be sized before any braille is drawn.
        let bounds: Vec<(f64, f64)> = self
            .channels
            .iter()
            .map(|ch| padded_bounds(ch, start, end))
            .collect();

        let gutter = self
            .channels
            .iter()
            .zip(&bounds)
            .flat_map(|(ch, &(lo, hi))| {
                let sym = ch.unit.symbol();
                // Count display columns (chars), not bytes: `µ`/`Ω` unit symbols
                // are multi-byte in UTF-8, so `.len()` would over-size the gutter.
                let w = sym.chars().count();
                [
                    fmt_num(hi).chars().count() + w,
                    fmt_num(lo).chars().count() + w,
                ]
            })
            .max()
            .unwrap_or(1)
            .max(1);

        // Interior braille columns: total width minus the gutter, a space and '┤'.
        let plot_w = self.width.saturating_sub(gutter + 2).max(1);

        let mut out = String::new();
        out.push_str("  ");
        out.push_str(&fit(&self.title, self.width.saturating_sub(2)));
        out.push('\n');

        if let Some(readout) = self.measure() {
            let unit = self
                .cursors
                .and_then(|c| self.channels.get(c.source))
                .map(|ch| ch.unit.symbol())
                .unwrap_or("");
            let line = format!(
                "  Δt={}  1/Δt={}  Δv={}",
                fmt_eng(readout.dt, "s"),
                fmt_eng(readout.freq, "Hz"),
                fmt_eng(readout.dv, unit),
            );
            out.push_str(&fit(&line, self.width));
            out.push('\n');
        }

        for (ch, &(ymin, ymax)) in self.channels.iter().zip(&bounds) {
            let sym = ch.unit.symbol();
            let top = format!("{}{sym}", fmt_num(ymax));
            let bot = format!("{}{sym}", fmt_num(ymin));

            // Channel label row.
            let label = if sym.is_empty() {
                format!("  {}", ch.name)
            } else {
                format!("  {} ({sym})", ch.name)
            };
            out.push_str(&fit(&label, self.width));
            out.push('\n');

            let rows = trace_rows(ch, start, end, ymin, ymax, plot_w, self.channel_height);
            for (cy, row) in rows.iter().enumerate() {
                let axis_label = match cy {
                    0 => top.as_str(),
                    n if n == self.channel_height - 1 => bot.as_str(),
                    _ => "",
                };
                out.push_str(&format!("{axis_label:>gutter$} \u{2524}{row}\n"));
            }
        }

        // Shared x axis: a rule, then trigger-relative time labels (0 .. span).
        out.push_str(&format!("{:>gutter$} \u{2514}", ""));
        out.push_str(&"\u{2500}".repeat(plot_w));
        out.push('\n');
        let xlo = fmt_eng(0.0, "s");
        let xhi = fmt_eng(span, "s");
        // Count chars, not bytes: a `µs`/`ms` label is multi-byte, so a byte count
        // would under-pad and push the right label past the plot edge.
        let pad = plot_w
            .saturating_sub(xlo.chars().count() + xhi.chars().count())
            .max(1);
        out.push_str(&fit(
            &format!("{:>gutter$}  {xlo}{}{xhi}", "", " ".repeat(pad)),
            self.width,
        ));
        out.push('\n');
        out
    }
}

/// Find the first `slope` crossing of `level` and interpolate its time.
fn find_edge(x: &[f64], y: &[f64], level: f64, slope: Slope) -> Option<f64> {
    if x.len() != y.len() {
        return None;
    }
    for i in 1..y.len() {
        let (a, b) = (y[i - 1], y[i]);
        let crossed = match slope {
            Slope::Rising => a < level && b >= level,
            Slope::Falling => a > level && b <= level,
        };
        if crossed {
            let dy = b - a;
            let t = if dy.abs() < f64::EPSILON {
                x[i]
            } else {
                x[i - 1] + (x[i] - x[i - 1]) * ((level - a) / dy)
            };
            return Some(t);
        }
    }
    None
}

/// Vertical bounds of a channel over the window `[start, end]`, padded so a flat
/// signal still lands mid-frame (matching `ee_tui::plot`'s behaviour).
fn padded_bounds(ch: &Signal, start: f64, end: f64) -> (f64, f64) {
    let y = ch.real().unwrap_or(&[]);
    let x = &ch.x;
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for (xi, yi) in x.iter().zip(y) {
        if *xi >= start && *xi <= end && yi.is_finite() {
            lo = lo.min(*yi);
            hi = hi.max(*yi);
        }
    }
    // Always include the interpolated window endpoints so the trace never leaves
    // the frame even when no raw sample sits inside the window.
    if !y.is_empty() {
        for v in [interp(x, y, start), interp(x, y, end)] {
            if v.is_finite() {
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
    }
    if !lo.is_finite() {
        lo = 0.0;
        hi = 0.0;
    }
    if (hi - lo).abs() < f64::EPSILON {
        lo -= 1.0;
        hi += 1.0;
    }
    (lo, hi)
}

/// Draw one channel's trace into a braille cell grid and return it row by row.
fn trace_rows(
    ch: &Signal,
    start: f64,
    end: f64,
    ymin: f64,
    ymax: f64,
    plot_w: usize,
    plot_h: usize,
) -> Vec<String> {
    let (pw, ph) = (plot_w * 2, plot_h * 4);
    let mut cells = vec![0u8; plot_w * plot_h];
    let y = ch.real().unwrap_or(&[]);
    let x = &ch.x;

    if !y.is_empty() {
        let samples: Vec<f64> = (0..pw)
            .map(|px| {
                let t = if pw > 1 {
                    px as f64 / (pw - 1) as f64
                } else {
                    0.0
                };
                interp(x, y, start + (end - start) * t)
            })
            .collect();

        let mut prev: Option<usize> = None;
        for (px, &yd) in samples.iter().enumerate() {
            // A non-finite sample (an inf/NaN from a diverged solve) leaves a gap
            // and breaks the connecting segment, matching `braille::trace_rows` —
            // rather than clamping to a spurious full-height spike.
            if !yd.is_finite() {
                prev = None;
                continue;
            }
            let frac = (ymax - yd) / (ymax - ymin);
            let py = (frac * (ph - 1) as f64).round().clamp(0.0, (ph - 1) as f64) as usize;
            // Connect to the previous column so steep segments stay unbroken.
            let (lo, hi) = match prev {
                Some(p) if p < py => (p, py),
                Some(p) => (py, p),
                None => (py, py),
            };
            for row_px in lo..=hi {
                set_dot(&mut cells, plot_w, px, row_px);
            }
            prev = Some(py);
        }
    }

    (0..plot_h)
        .map(|cy| {
            (0..plot_w)
                .map(|cx| {
                    char::from_u32(BRAILLE_BASE + cells[cy * plot_w + cx] as u32).unwrap_or(' ')
                })
                .collect::<String>()
        })
        .collect()
}

/// Set the dot at dot-canvas pixel `(px, py)` in the cell grid.
fn set_dot(cells: &mut [u8], cells_w: usize, px: usize, py: usize) {
    let (cell_x, col) = (px / 2, px % 2);
    let (cell_y, row) = (py / 4, py % 4);
    cells[cell_y * cells_w + cell_x] |= DOTS[col][row];
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

/// Compact number formatting for axis labels: trims trailing zeros, avoids `-0`.
fn fmt_num(v: f64) -> String {
    if v == 0.0 {
        return "0".to_string();
    }
    if !v.is_finite() {
        return if v > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    let mut s = format!("{v:.3}");
    if s.contains('.') {
        s.truncate(s.trim_end_matches('0').trim_end_matches('.').len());
    }
    s
}

/// Format a value in engineering notation with an SI prefix and unit, e.g.
/// `0.001 s` → `1ms`, `1000 Hz` → `1kHz`. Used for the readout and x labels.
fn fmt_eng(v: f64, unit: &str) -> String {
    if v == 0.0 {
        return format!("0{unit}");
    }
    if !v.is_finite() {
        return format!("{}{unit}", if v > 0.0 { "inf" } else { "-inf" });
    }
    const PREFIXES: &[(f64, &str)] = &[
        (1e12, "T"),
        (1e9, "G"),
        (1e6, "M"),
        (1e3, "k"),
        (1.0, ""),
        (1e-3, "m"),
        (1e-6, "µ"),
        (1e-9, "n"),
        (1e-12, "p"),
    ];
    let a = v.abs();
    for &(factor, prefix) in PREFIXES {
        if a >= factor {
            return format!("{}{prefix}{unit}", fmt_num(v / factor));
        }
    }
    format!("{}p{unit}", fmt_num(v / 1e-12))
}

/// Clip or space-pad a string to at most `width` character cells. Over-long
/// input is truncated; shorter input is returned unpadded (callers that need an
/// exact width pad separately).
fn fit(s: &str, width: usize) -> String {
    if s.chars().count() > width {
        s.chars().take(width).collect()
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ee_core::signal::Signal;
    use ee_core::units::Unit;
    use std::f64::consts::TAU;

    /// A sine of frequency `f` (Hz) sampled on a uniform grid, phase-shifted by
    /// `phase_samples` sample intervals — i.e. a different *capture phase* of the
    /// same periodic waveform.
    fn sine(name: &str, f: f64, dt: f64, n: usize, phase_samples: f64) -> Signal {
        let w = TAU * f;
        let x: Vec<f64> = (0..n).map(|j| j as f64 * dt).collect();
        let y: Vec<f64> = (0..n)
            .map(|j| (w * (j as f64 + phase_samples) * dt).sin())
            .collect();
        Signal::time(name, Unit::Volt, x, y)
    }

    fn scope_of(sig: Signal) -> Oscilloscope {
        Oscilloscope::new("scope")
            .size(48, 6)
            .timebase(Timebase::new(2e-3)) // two periods of a 1 kHz signal
            .trigger(Trigger::Edge {
                source: 0,
                level: 0.0,
                slope: Slope::Rising,
            })
            .channel(sig)
    }

    #[test]
    fn trigger_locks_a_periodic_signal_regardless_of_phase() {
        // 1 kHz sine, 100 samples/period, 6 periods captured.
        let (f, dt, n) = (1_000.0, 1e-5, 600);
        let a = scope_of(sine("v(sig)", f, dt, n, 0.0)).render();
        let b = scope_of(sine("v(sig)", f, dt, n, 37.0)).render();
        let c = scope_of(sine("v(sig)", f, dt, n, 183.0)).render();
        assert_eq!(a, b, "trigger must remove capture phase:\nA:\n{a}\nB:\n{b}");
        assert_eq!(a, c, "trigger must remove capture phase:\nA:\n{a}\nC:\n{c}");

        // Golden frame — the stable, trigger-locked view.
        assert_eq!(a, GOLDEN_TRIGGERED, "golden scope frame drifted:\n{a}");
    }

    #[test]
    fn free_run_drifts_with_phase_but_trigger_does_not() {
        let (f, dt, n) = (1_000.0, 1e-5, 600);
        let mk = |phase| {
            Oscilloscope::new("scope")
                .size(48, 6)
                .timebase(Timebase::new(2e-3))
                .trigger(Trigger::FreeRun)
                .channel(sine("v(sig)", f, dt, n, phase))
                .render()
        };
        // Different capture phases produce different free-run frames...
        assert_ne!(mk(0.0), mk(37.0), "free-run should drift with phase");
        // ...which is exactly what the trigger exists to fix (asserted above).
    }

    #[test]
    fn cursors_report_dt_and_frequency() {
        // One period of a 1 kHz sine is 1 ms; cursors a period apart read 1 kHz.
        let (f, dt, n) = (1_000.0, 1e-5, 600);
        let scope = scope_of(sine("v(sig)", f, dt, n, 0.0)).cursors(Cursors::new(0.0, 1.0 / f, 0));
        let r = scope.measure().expect("cursors set");
        assert!((r.dt - 1e-3).abs() < 1e-12, "Δt = {}", r.dt);
        assert!((r.freq - f).abs() < 1e-6, "1/Δt = {} Hz, want {f}", r.freq);
        // A full period of a sine returns to the same value → Δv ≈ 0.
        assert!(r.dv.abs() < 1e-9, "Δv over one period = {}", r.dv);

        // Half a period apart on a sine starting at 0: Δv spans the full swing.
        let half = scope_of(sine("v(sig)", f, dt, n, 0.0))
            .cursors(Cursors::new(0.0, 0.5 / f, 0))
            .measure()
            .unwrap();
        assert!(
            (half.freq - 2.0 * f).abs() < 1e-6,
            "half-period cursors read 2 kHz, got {}",
            half.freq
        );
    }

    #[test]
    fn multi_channel_renders_both_labels_and_fits_width() {
        let (f, dt, n) = (1_000.0, 1e-5, 600);
        let width = 60;
        let scope = Oscilloscope::new("scope")
            .size(width, 5)
            .timebase(Timebase::new(2e-3))
            .trigger(Trigger::Edge {
                source: 0,
                level: 0.0,
                slope: Slope::Rising,
            })
            .channel(sine("v(a)", f, dt, n, 0.0))
            .channel(sine("v(b)", f, dt, n, 25.0));
        assert_eq!(scope.len(), 2);
        let out = scope.render();
        assert!(out.contains("v(a)"), "channel A label:\n{out}");
        assert!(out.contains("v(b)"), "channel B label:\n{out}");
        // A braille glyph for each trace was actually drawn.
        assert!(out.chars().any(|c| ('\u{2801}'..='\u{28FF}').contains(&c)));
        for line in out.lines() {
            assert!(
                line.chars().count() <= width,
                "line exceeds width {width}: {line:?}"
            );
        }
    }

    #[test]
    fn flat_signal_does_not_panic() {
        let x: Vec<f64> = (0..50).map(|i| i as f64 * 1e-4).collect();
        let y = vec![2.5; 50];
        let out = Oscilloscope::new("flat")
            .size(40, 5)
            .timebase(Timebase::new(2e-3))
            .trigger(Trigger::Edge {
                source: 0,
                level: 0.0,
                slope: Slope::Rising,
            })
            .channel(Signal::time("v(flat)", Unit::Volt, x, y))
            .render();
        // No rising crossing of 0 exists → free-run fallback, padded ±1 bounds.
        assert!(out.contains("3.5V") && out.contains("1.5V"), "{out}");
    }

    #[test]
    fn single_sample_signal_does_not_panic() {
        let out = Oscilloscope::new("one")
            .size(32, 4)
            .timebase(Timebase::new(1e-3))
            .channel(Signal::time("v(x)", Unit::Volt, vec![0.0], vec![1.0]))
            .render();
        assert!(!out.is_empty());
    }

    #[test]
    fn non_finite_sample_leaves_a_gap_not_a_spike() {
        // A flat 0 V trace with one inf sample. The finite trace lands mid-frame,
        // so the top braille row must stay blank: the inf must gap, not clamp to a
        // full-height spike reaching the top row.
        let n = 60;
        let x: Vec<f64> = (0..n).map(|i| i as f64 * 1e-4).collect();
        let mut y = vec![0.0; n];
        y[n / 2] = f64::INFINITY;
        let out = Oscilloscope::new("inf")
            .size(48, 6)
            .timebase(Timebase::new(n as f64 * 1e-4))
            .channel(Signal::time("v(x)", Unit::Volt, x, y))
            .render();
        // The first braille row (nearest the top) is the first line carrying '┤'.
        let top = out
            .lines()
            .find(|l| l.contains('\u{2524}'))
            .expect("a braille row");
        assert!(
            !top.chars().any(|c| ('\u{2801}'..='\u{28FF}').contains(&c)),
            "an inf sample spiked to the top row instead of gapping:\n{out}"
        );
    }

    #[test]
    fn empty_scope_renders_without_panicking() {
        let out = Oscilloscope::new("empty").size(24, 4).render();
        assert!(out.contains("empty"));
    }

    /// Regression (`CP-UX-M8.3`): a `µs`-scale timebase produces a multi-byte `µ`
    /// in the x-axis label. The axis width math must count display *chars*, not
    /// UTF-8 *bytes*, or the right-hand time label falls short of the rule's right
    /// edge. The x-label line and the axis rule line must span the same number of
    /// cells (before this fix the label line was one char short per `µ`).
    #[test]
    fn micro_timebase_axis_labels_align_to_the_rule() {
        // 40 µs span → `fmt_eng` renders "40µs" (a multi-byte `µ`).
        let (f, dt, n) = (100_000.0, 1e-7, 600);
        let out = Oscilloscope::new("scope")
            .size(48, 6)
            .timebase(Timebase::new(40e-6))
            .trigger(Trigger::FreeRun)
            .channel(sine("v(sig)", f, dt, n, 0.0))
            .render();
        let lines: Vec<&str> = out.lines().collect();
        let label = *lines.last().expect("an x-label line");
        let rule = lines[lines.len() - 2];
        assert!(label.contains('µ'), "the span label uses µs: {label:?}");
        assert!(rule.contains('\u{2514}'), "the rule row uses └: {rule:?}");
        assert_eq!(
            label.chars().count(),
            rule.chars().count(),
            "x-label must span the same cells as the rule:\n{rule}\n{label}"
        );
    }

    /// Regression (`CP-UX-M8.3`): a channel whose unit symbol is multi-byte (`Ω`)
    /// must not over-size the y-axis gutter. The gutter width is measured in chars,
    /// so an `Ω` channel and an identical `V` channel leave the same plot width —
    /// i.e. the braille plot interior is the same number of cells wide.
    #[test]
    fn ohm_unit_gutter_matches_an_ascii_unit() {
        let (f, dt, n) = (1_000.0, 1e-5, 600);
        let plot_cols = |unit: Unit| {
            let x: Vec<f64> = (0..n).map(|j| j as f64 * dt).collect();
            let y: Vec<f64> = (0..n).map(|j| (TAU * f * j as f64 * dt).sin()).collect();
            let out = Oscilloscope::new("scope")
                .size(48, 6)
                .timebase(Timebase::new(2e-3))
                .trigger(Trigger::FreeRun)
                .channel(Signal::time("v(sig)", unit, x, y))
                .render();
            // The axis rule row's ─ run is the plot interior width.
            out.lines()
                .find(|l| l.contains('\u{2514}'))
                .map(|l| l.chars().filter(|&c| c == '\u{2500}').count())
                .unwrap()
        };
        assert_eq!(
            plot_cols(Unit::Ohm),
            plot_cols(Unit::Volt),
            "a multi-byte unit symbol must not steal plot columns"
        );
    }

    // Captured from a passing run; regenerate by printing `a` in the trigger test.
    const GOLDEN_TRIGGERED: &str = "  scope\n  v(sig) (V)\n 1V ┤⠀⠀⠀⣠⠞⠉⠙⠲⣄⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⣠⠖⠋⠉⠳⣄⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀\n    ┤⠀⢀⡞⠁⠀⠀⠀⠀⠘⢦⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢀⡼⠁⠀⠀⠀⠀⠈⢳⡀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀\n    ┤⢠⠏⠀⠀⠀⠀⠀⠀⠀⠈⢳⡀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢀⡞⠀⠀⠀⠀⠀⠀⠀⠀⠳⡄⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀\n    ┤⠉⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠳⡄⠀⠀⠀⠀⠀⠀⠀⢠⠏⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠹⣄⠀⠀⠀⠀⠀⠀⠀⢀⡞\n    ┤⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠹⣄⠀⠀⠀⠀⠀⣰⠋⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠘⢦⠀⠀⠀⠀⠀⣠⠏⠀\n-1V ┤⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠈⠳⣄⣀⡤⠞⠁⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠈⠳⢤⣀⣠⠞⠁⠀⠀\n    └───────────────────────────────────────────\n     0s                                      2ms\n";
}
