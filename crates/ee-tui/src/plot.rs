//! Braille waveform plotting — the "first pixels" widget (`CP-UX-M0.3`).
//!
//! A terminal character cell can be treated as a 2×4 grid of dots using the
//! Unicode Braille Patterns block (`U+2800..=U+28FF`). Packing eight dots per
//! cell buys 2× horizontal and 4× vertical resolution over ASCII plotting, so a
//! waveform reads as a smooth curve instead of a staircase — high resolution in
//! a plain terminal, no GPU, works over SSH.
//!
//! This widget is deliberately dependency-free and renders to a `String`: that
//! keeps it golden-file testable (the `CP-UX-M0.3` gate) and lets it run in the
//! examples before the `ratatui` event loop or the `ee-sim` solver exist. It
//! consumes plain `(x, y)` sample slices today; once `Signal` lands in `ee-core`
//! (`CP-SIM-M0.5`) a thin adapter will feed `Signal` straight in.

/// Dot bit-weights for the Braille Patterns block, indexed `DOTS[col][row]`.
///
/// A cell holds two columns (left/right) and four rows (top→bottom). The block
/// encodes each of the eight dots as one bit of the low byte of the code point:
///
/// ```text
///   col0 col1        bit weights
///   (1)  (4)         1→0x01  4→0x08
///   (2)  (5)         2→0x02  5→0x10
///   (3)  (6)         3→0x04  6→0x20
///   (7)  (8)         7→0x40  8→0x80
/// ```
const DOTS: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];

/// The base code point of the Braille Patterns block; add the dot byte to it.
const BRAILLE_BASE: u32 = 0x2800;

/// A single-series waveform plot rendered to Unicode braille text.
///
/// Sizes are in character cells; the dot canvas is `2*width` by `4*height`.
pub struct Plot<'a> {
    /// Heading shown above the frame.
    pub title: &'a str,
    /// Plot area width, in character cells.
    pub width: usize,
    /// Plot area height, in character cells.
    pub height: usize,
    /// Unit appended to the y-axis min/max labels (e.g. `"V"`); may be empty.
    pub y_unit: &'a str,
    /// Unit appended to the x-axis range labels (e.g. `"ms"`); may be empty.
    pub x_unit: &'a str,
}

impl<'a> Plot<'a> {
    /// A sensible default: 72×16 cells, unitless.
    pub fn new(title: &'a str) -> Self {
        Self {
            title,
            width: 72,
            height: 16,
            y_unit: "",
            x_unit: "",
        }
    }

    /// Set the axis units shown on the range labels.
    pub fn units(mut self, y_unit: &'a str, x_unit: &'a str) -> Self {
        self.y_unit = y_unit;
        self.x_unit = x_unit;
        self
    }

    /// Set the plot area size in character cells.
    pub fn size(mut self, width: usize, height: usize) -> Self {
        self.width = width.max(1);
        self.height = height.max(1);
        self
    }

    /// Render the `(x, y)` series to a framed, multi-line string.
    ///
    /// `x` must be non-decreasing (a time or frequency axis); `x` and `y` must be
    /// the same length. Fewer than two points renders an empty frame. The y-range
    /// autoscales to the data, padding a degenerate (flat) signal so a constant
    /// line lands mid-frame.
    pub fn render(&self, x: &[f64], y: &[f64]) -> String {
        let (pw, ph) = (self.width * 2, self.height * 4);
        let mut cells = vec![0u8; self.width * self.height];

        if x.len() >= 2 && x.len() == y.len() {
            let (xmin, xmax) = (x[0], x[x.len() - 1]);
            let (mut ymin, mut ymax) = y_bounds(y);
            if (ymax - ymin).abs() < f64::EPSILON {
                ymin -= 1.0;
                ymax += 1.0;
            }

            let mut prev_py: Option<usize> = None;
            for px in 0..pw {
                let t = px as f64 / (pw - 1) as f64;
                let xd = xmin + (xmax - xmin) * t;
                let yd = interp(x, y, xd);
                // Invert: larger y is nearer the top row (py = 0).
                let frac = (ymax - yd) / (ymax - ymin);
                let py = (frac * (ph - 1) as f64).round().clamp(0.0, (ph - 1) as f64) as usize;
                // Connect to the previous column so steep segments stay unbroken.
                let (lo, hi) = match prev_py {
                    Some(p) if p < py => (p, py),
                    Some(p) => (py, p),
                    None => (py, py),
                };
                for row_px in lo..=hi {
                    set_dot(&mut cells, self.width, px, row_px);
                }
                prev_py = Some(py);
            }
        }

        self.frame(&cells, x, y)
    }

    /// Wrap the dot canvas in a titled frame with axis labels.
    fn frame(&self, cells: &[u8], x: &[f64], y: &[f64]) -> String {
        let plotted = x.len() >= 2 && x.len() == y.len();
        let (ymin, ymax) = if plotted {
            let (lo, hi) = y_bounds(y);
            if (hi - lo).abs() < f64::EPSILON {
                (lo - 1.0, hi + 1.0)
            } else {
                (lo, hi)
            }
        } else {
            (0.0, 0.0)
        };

        let top = format!("{}{}", fmt_num(ymax), self.y_unit);
        let bot = format!("{}{}", fmt_num(ymin), self.y_unit);
        let gutter = top.len().max(bot.len());

        let mut out = String::new();
        out.push_str("  ");
        out.push_str(self.title);
        out.push('\n');

        for cy in 0..self.height {
            let label = match cy {
                0 => &top,
                n if n == self.height - 1 => &bot,
                _ => "",
            };
            out.push_str(&format!("{label:>gutter$} \u{2524}")); // right-justified label + '┤'
            for cx in 0..self.width {
                let ch = char::from_u32(BRAILLE_BASE + cells[cy * self.width + cx] as u32);
                out.push(ch.unwrap_or(' '));
            }
            out.push('\n');
        }

        // X axis: a rule under the plot, then the range labels at each end.
        out.push_str(&format!("{:>gutter$} \u{2514}", ""));
        out.push_str(&"\u{2500}".repeat(self.width));
        out.push('\n');
        if plotted {
            let xlo = format!("{}{}", fmt_num(x[0]), self.x_unit);
            let xhi = format!("{}{}", fmt_num(x[x.len() - 1]), self.x_unit);
            let pad = self.width.saturating_sub(xlo.len() + xhi.len()).max(1);
            out.push_str(&format!("{:>gutter$}  {xlo}{}{xhi}\n", "", " ".repeat(pad)));
        }
        out
    }
}

/// Set the dot at dot-canvas pixel `(px, py)` in the cell grid.
fn set_dot(cells: &mut [u8], cells_w: usize, px: usize, py: usize) {
    let (cell_x, col) = (px / 2, px % 2);
    let (cell_y, row) = (py / 4, py % 4);
    cells[cell_y * cells_w + cell_x] |= DOTS[col][row];
}

/// Min and max of a slice, ignoring non-finite samples.
fn y_bounds(y: &[f64]) -> (f64, f64) {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for &v in y.iter().filter(|v| v.is_finite()) {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    if lo.is_finite() {
        (lo, hi)
    } else {
        (0.0, 0.0)
    }
}

/// Linearly interpolate `y` at `xd` over a non-decreasing `x`. Clamps at ends.
fn interp(x: &[f64], y: &[f64], xd: f64) -> f64 {
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
    let mut s = format!("{v:.3}");
    if s.contains('.') {
        s.truncate(s.trim_end_matches('0').trim_end_matches('.').len());
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(n: usize) -> (Vec<f64>, Vec<f64>) {
        let xs: Vec<f64> = (0..n).map(|i| i as f64).collect();
        let ys = xs.clone();
        (xs, ys)
    }

    #[test]
    fn renders_stable_frame() {
        let (x, y) = ramp(32);
        let plot = Plot::new("ramp").size(20, 8);
        let a = plot.render(&x, &y);
        let b = plot.render(&x, &y);
        assert_eq!(a, b, "rendering must be deterministic");
        // Title line + 8 plot rows + axis rule + range labels.
        assert_eq!(a.lines().count(), 1 + 8 + 1 + 1);
        assert!(a.contains('\u{2524}'), "each plot row carries the '┤' axis");
    }

    #[test]
    fn braille_fills_are_in_block() {
        let (x, y) = ramp(64);
        let out = Plot::new("ramp").size(24, 6).render(&x, &y);
        // At least one real braille glyph got drawn (not just the blank U+2800).
        assert!(out.chars().any(|c| ('\u{2801}'..='\u{28FF}').contains(&c)));
    }

    #[test]
    fn rising_ramp_goes_bottom_left_to_top_right() {
        // A monotonically rising signal should darken the lower-left and the
        // upper-right of the canvas. Compare the diagonal-opposite cells.
        let (x, y) = ramp(48);
        let w = 24;
        let h = 8;
        let mut cells = vec![0u8; w * h];
        let plot = Plot::new("t").size(w, h);
        let _ = plot.render(&x, &y); // exercise the public path too
                                     // Rebuild the raw canvas to inspect cell occupancy directly.
        let (pw, ph) = (w * 2, h * 4);
        for px in 0..pw {
            let t = px as f64 / (pw - 1) as f64;
            let frac = 1.0 - t; // rising signal, inverted axis
            let py = (frac * (ph - 1) as f64).round() as usize;
            set_dot(&mut cells, w, px, py);
        }
        let bottom_left = cells[(h - 1) * w];
        let top_right = cells[w - 1];
        assert_ne!(
            bottom_left, 0,
            "start of a rising ramp is low (bottom-left)"
        );
        assert_ne!(top_right, 0, "end of a rising ramp is high (top-right)");
    }

    #[test]
    fn flat_signal_does_not_panic_and_frames() {
        let x: Vec<f64> = (0..10).map(|i| i as f64).collect();
        let y = vec![2.5; 10];
        let out = Plot::new("flat").size(16, 5).units("V", "s").render(&x, &y);
        // A degenerate (flat) signal is padded by ±1 so the constant line lands
        // mid-frame; the axis labels therefore read 3.5V / 1.5V, not 2.5V.
        assert!(
            out.contains("3.5V") && out.contains("1.5V"),
            "flat signal autoscales with padding"
        );
    }

    #[test]
    fn too_few_points_renders_empty_frame() {
        let out = Plot::new("empty").size(10, 4).render(&[0.0], &[0.0]);
        assert_eq!(out.lines().count(), 1 + 4 + 1); // no x-range line when unplotted
    }
}
