//! Shared braille canvas helper for the frequency-domain instrument views.
//!
//! The [`Plot`](../../ohmforge_tui/plot/struct.Plot.html) widget and the
//! [`oscilloscope`](crate::oscilloscope) each grew their own copy of the braille
//! pack-eight-dots-per-cell trick. The Bode and spectrum views share this module
//! so a new trace renders byte-for-byte like the scope's. It is deliberately
//! tiny and dependency-free, and renders a cell grid to plain `String` rows.

/// Dot bit-weights for the Braille Patterns block, indexed `DOTS[col][row]`.
/// Matches [`crate::oscilloscope`] so every trace looks the same.
const DOTS: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];

/// The base code point of the Braille Patterns block; add the dot byte to it.
const BRAILLE_BASE: u32 = 0x2800;

/// Set the dot at dot-canvas pixel `(px, py)` in a `cells_w`-wide cell grid.
fn set_dot(cells: &mut [u8], cells_w: usize, px: usize, py: usize) {
    let (cell_x, col) = (px / 2, px % 2);
    let (cell_y, row) = (py / 4, py % 4);
    cells[cell_y * cells_w + cell_x] |= DOTS[col][row];
}

/// A reusable braille dot canvas that **multiple** traces can be drawn onto
/// before it is emitted as rows.
///
/// [`trace_rows`] draws exactly one trace; the [`eye`](crate::eye) diagram folds
/// many unit intervals onto one frame, so it accumulates them here and reads the
/// rows out once. Each [`plot`](Canvas::plot) call sets dots the same way
/// [`trace_rows`] does — larger values sit nearer the top, non-finite values
/// break the connecting segment — so an overlay looks like the scope's traces.
pub(crate) struct Canvas {
    cells_w: usize,
    cells_h: usize,
    cells: Vec<u8>,
}

impl Canvas {
    /// A blank canvas of `cells_w × cells_h` character cells (`2·w × 4·h` dots).
    pub(crate) fn new(cells_w: usize, cells_h: usize) -> Self {
        Self {
            cells_w,
            cells_h,
            cells: vec![0u8; cells_w * cells_h],
        }
    }

    /// Draw one connected trace onto the canvas. `col_values[px]` is the value at
    /// dot-column `px`; values are scaled into `[ymin, ymax]` (inverted, like the
    /// scope) and consecutive columns are joined so steep segments stay unbroken.
    pub(crate) fn plot(&mut self, col_values: &[f64], ymin: f64, ymax: f64) {
        let ph = self.cells_h * 4;
        let range = ymax - ymin;
        let mut prev: Option<usize> = None;
        for (px, &yd) in col_values.iter().enumerate().take(self.cells_w * 2) {
            if !yd.is_finite() {
                prev = None;
                continue;
            }
            let frac = if range.abs() < f64::EPSILON {
                0.5
            } else {
                (ymax - yd) / range
            };
            let py = (frac * (ph - 1) as f64).round().clamp(0.0, (ph - 1) as f64) as usize;
            let (lo, hi) = match prev {
                Some(p) if p < py => (p, py),
                Some(p) => (py, p),
                None => (py, py),
            };
            for row_px in lo..=hi {
                set_dot(&mut self.cells, self.cells_w, px, row_px);
            }
            prev = Some(py);
        }
    }

    /// Whether the dot at dot-pixel `(px, py)` is set (used by the eye diagram to
    /// probe the opening).
    #[cfg(test)]
    pub(crate) fn dot_set(&self, px: usize, py: usize) -> bool {
        if px >= self.cells_w * 2 || py >= self.cells_h * 4 {
            return false;
        }
        let (cell_x, col) = (px / 2, px % 2);
        let (cell_y, row) = (py / 4, py % 4);
        self.cells[cell_y * self.cells_w + cell_x] & DOTS[col][row] != 0
    }

    /// Emit the accumulated canvas row by row as braille character strings.
    pub(crate) fn rows(&self) -> Vec<String> {
        (0..self.cells_h)
            .map(|cy| {
                (0..self.cells_w)
                    .map(|cx| {
                        char::from_u32(BRAILLE_BASE + self.cells[cy * self.cells_w + cx] as u32)
                            .unwrap_or(' ')
                    })
                    .collect::<String>()
            })
            .collect()
    }
}

/// Draw a connected trace into a braille cell grid and return it row by row.
///
/// `col_values[px]` is the data value at dot-column `px` (there are `plot_w * 2`
/// dot-columns and `plot_h * 4` dot-rows). A non-finite value leaves a gap and
/// breaks the connecting segment. The vertical axis is inverted — larger values
/// sit nearer the top row — exactly like the scope and the `Plot` widget. A
/// degenerate `[ymin, ymax]` range (a flat trace) is drawn mid-frame.
pub(crate) fn trace_rows(
    col_values: &[f64],
    ymin: f64,
    ymax: f64,
    plot_w: usize,
    plot_h: usize,
) -> Vec<String> {
    let ph = plot_h * 4;
    let mut cells = vec![0u8; plot_w * plot_h];
    let range = ymax - ymin;

    let mut prev: Option<usize> = None;
    for (px, &yd) in col_values.iter().enumerate().take(plot_w * 2) {
        if !yd.is_finite() {
            prev = None;
            continue;
        }
        let frac = if range.abs() < f64::EPSILON {
            0.5
        } else {
            (ymax - yd) / range
        };
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
