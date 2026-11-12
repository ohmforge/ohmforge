//! A small, self-contained braille waveform renderer for the browser front end.
//!
//! `ohmforge-tui` already has a polished `Plot` (see `crates/ohmforge-tui/src/plot.rs`), but
//! that crate pulls in `crossterm`, which does **not** compile for
//! `wasm32-unknown-unknown`. Rather than drag a terminal-I/O dependency into the
//! browser bundle (or edit another crate), `ohmforge-web` carries this compact,
//! dependency-free port of the same 2×4-dots-per-cell braille technique. It
//! renders an `(x, y)` series to a framed multi-line `String` that drops straight
//! into a `<pre>` on the page.

/// Dot bit-weights for the Unicode Braille Patterns block, indexed `DOTS[col][row]`.
const DOTS: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];
/// Base code point of the Braille Patterns block; add the dot byte to it.
const BRAILLE_BASE: u32 = 0x2800;

/// Render one `(x, y)` series to a titled braille frame, `width`×`height` cells.
///
/// `x` must be non-decreasing and the same length as `y`; fewer than two points
/// renders an empty frame. The y-range autoscales, padding a flat signal so a
/// constant line lands mid-frame. `y_unit`/`x_unit` are appended to the range
/// labels (either may be empty).
pub fn render(
    title: &str,
    x: &[f64],
    y: &[f64],
    width: usize,
    height: usize,
    y_unit: &str,
    x_unit: &str,
) -> String {
    let width = width.max(1);
    let height = height.max(1);
    let (pw, ph) = (width * 2, height * 4);
    let mut cells = vec![0u8; width * height];

    let plotted = x.len() >= 2 && x.len() == y.len();
    if plotted {
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
            if !yd.is_finite() {
                prev_py = None;
                continue;
            }
            let frac = (ymax - yd) / (ymax - ymin);
            let py = (frac * (ph - 1) as f64).round().clamp(0.0, (ph - 1) as f64) as usize;
            let (lo, hi) = match prev_py {
                Some(p) if p < py => (p, py),
                Some(p) => (py, p),
                None => (py, py),
            };
            for row_px in lo..=hi {
                set_dot(&mut cells, width, px, row_px);
            }
            prev_py = Some(py);
        }
    }

    frame(title, &cells, width, height, x, y, y_unit, x_unit)
}

#[allow(clippy::too_many_arguments)]
fn frame(
    title: &str,
    cells: &[u8],
    width: usize,
    height: usize,
    x: &[f64],
    y: &[f64],
    y_unit: &str,
    x_unit: &str,
) -> String {
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

    let top = format!("{}{}", fmt_num(ymax), y_unit);
    let bot = format!("{}{}", fmt_num(ymin), y_unit);
    let gutter = top.len().max(bot.len());

    let mut out = String::new();
    out.push_str("  ");
    out.push_str(title);
    out.push('\n');

    for cy in 0..height {
        let label = match cy {
            0 => &top,
            n if n == height - 1 => &bot,
            _ => "",
        };
        out.push_str(&format!("{label:>gutter$} \u{2524}"));
        for cx in 0..width {
            let ch = char::from_u32(BRAILLE_BASE + cells[cy * width + cx] as u32);
            out.push(ch.unwrap_or(' '));
        }
        out.push('\n');
    }

    out.push_str(&format!("{:>gutter$} \u{2514}", ""));
    out.push_str(&"\u{2500}".repeat(width));
    out.push('\n');
    if plotted {
        let xlo = format!("{}{}", fmt_num(x[0]), x_unit);
        let xhi = format!("{}{}", fmt_num(x[x.len() - 1]), x_unit);
        let pad = width.saturating_sub(xlo.len() + xhi.len()).max(1);
        out.push_str(&format!("{:>gutter$}  {xlo}{}{xhi}\n", "", " ".repeat(pad)));
    }
    out
}

fn set_dot(cells: &mut [u8], cells_w: usize, px: usize, py: usize) {
    let (cell_x, col) = (px / 2, px % 2);
    let (cell_y, row) = (py / 4, py % 4);
    cells[cell_y * cells_w + cell_x] |= DOTS[col][row];
}

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
