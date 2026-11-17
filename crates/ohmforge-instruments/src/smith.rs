//! Smith chart — a reflection coefficient Γ (or S11) plotted on the Smith grid
//! (`CP-UX-M9.2`).
//!
//! The Smith chart is the RF engineer's polar plot of the complex reflection
//! coefficient `Γ = (Z − Z₀) / (Z + Z₀)` over the unit disk: the matched load
//! `Z = Z₀` (`Γ = 0`) sits dead centre, a short (`Z = 0`, `Γ = −1`) lands on the
//! far-left rim and an open (`Z = ∞`, `Γ = +1`) on the far-right rim, and the
//! whole passive world (`|Γ| ≤ 1`) fits inside the outer unit circle. Overlaid on
//! the disk are the familiar **constant-resistance** circles and
//! **constant-reactance** arcs, so a point's normalised impedance can be read off
//! directly.
//!
//! This panel renders from a **provided** `Γ` (or an array of `S11` samples, e.g.
//! a frequency sweep forming a locus) — it does **not** call into the engine's
//! S-parameter solve. Wiring `Engine::s_parameters` output into
//! [`SmithChart::render`] is a thin follow-up: that API returns `S11` as a
//! [`Complex`], which is exactly what this takes. Like the other panels it depends
//! only on `ohmforge-core`, renders to a deterministic `String`, and is
//! golden-file testable.

use crate::fmt::{fit, fmt_num};
use ohmforge_core::signal::Complex;

/// The minimum chart extent (so the grid math never divides by zero).
const MIN_EXTENT: usize = 9;

/// The glyph marking the plotted Γ point(s).
const POINT: char = '●';
/// The glyph tracing the constant-R / constant-X grid curves.
const GRID: char = '·';
/// The origin marker (`Γ = 0`, the matched-load centre), shown when no point
/// covers it.
const ORIGIN: char = '+';

/// A Smith chart: the unit-disk reflection-coefficient plane with the
/// constant-resistance / constant-reactance grid.
///
/// Build with [`new`](SmithChart::new), optionally [`size`](SmithChart::size) it,
/// then [`render`](SmithChart::render) one or more Γ / S11 points onto it. The
/// pure [`cell_of`](SmithChart::cell_of) / [`center`](SmithChart::center) mapping
/// is exposed so callers (and tests) can reason about where a coefficient lands
/// without parsing the rendered frame.
pub struct SmithChart<'a> {
    /// Heading shown above the frame.
    pub title: &'a str,
    /// Grid width, in character cells.
    pub width: usize,
    /// Grid height, in character cells.
    pub height: usize,
}

impl<'a> SmithChart<'a> {
    /// A sensible default chart: 33 cells wide, 17 tall (both odd, so `Γ = 0`
    /// maps to an exact centre cell).
    pub fn new(title: &'a str) -> Self {
        Self {
            title,
            width: 33,
            height: 17,
        }
    }

    /// Set the grid width and height, in character cells (each clamped to a sane
    /// minimum).
    pub fn size(mut self, width: usize, height: usize) -> Self {
        self.width = width.max(MIN_EXTENT);
        self.height = height.max(MIN_EXTENT);
        self
    }

    fn dims(&self) -> (usize, usize) {
        (self.width.max(MIN_EXTENT), self.height.max(MIN_EXTENT))
    }

    /// The cell holding the chart centre — the matched load `Γ = 0`.
    pub fn center(&self) -> (usize, usize) {
        self.cell_of(Complex::ZERO)
    }

    /// The `(col, row)` cell a reflection coefficient `gamma` maps to.
    ///
    /// The real axis runs left→right (`Γ.re = −1` at column 0, `+1` at the last
    /// column) and the imaginary axis runs bottom→top (`Γ.im = −1` at the last
    /// row, `+1` at row 0), so `Γ = 0` is the centre, a short (`−1`) the left rim
    /// and an open (`+1`) the right rim. Coefficients outside the unit disk
    /// (`|Γ| > 1`, i.e. active loads) are clamped onto the grid rather than
    /// dropped.
    pub fn cell_of(&self, gamma: Complex) -> (usize, usize) {
        let (w, h) = self.dims();
        let fx = ((gamma.re + 1.0) / 2.0 * (w - 1) as f64).round();
        let fy = ((1.0 - gamma.im) / 2.0 * (h - 1) as f64).round();
        let col = fx.clamp(0.0, (w - 1) as f64) as usize;
        let row = fy.clamp(0.0, (h - 1) as f64) as usize;
        (col, row)
    }

    /// Render the chart with each `points` coefficient plotted onto the grid.
    ///
    /// An empty slice renders the bare Smith grid. The heading carries a readout
    /// of the first point: its `Γ` (magnitude ∠ phase) and the standing-wave ratio
    /// it implies.
    pub fn render(&self, points: &[Complex]) -> String {
        let (w, h) = self.dims();
        let mut grid = vec![vec![' '; w]; h];

        // The Smith grid: the outer unit circle (constant R = 0), a few
        // constant-resistance circles and constant-reactance arcs, and the real
        // axis. Drawn first so plotted points sit on top.
        self.draw_circle(&mut grid, 0.0, 0.0, 1.0); // unit circle, |Γ| = 1
        for &r in &[0.2, 0.5, 1.0, 2.0, 5.0] {
            // Constant resistance: centre (r/(1+r), 0), radius 1/(1+r).
            self.draw_circle(&mut grid, r / (1.0 + r), 0.0, 1.0 / (1.0 + r));
        }
        for &x in &[0.2, 0.5, 1.0, 2.0, 5.0] {
            // Constant reactance ±x: centre (1, ±1/x), radius 1/x.
            self.draw_circle(&mut grid, 1.0, 1.0 / x, 1.0 / x);
            self.draw_circle(&mut grid, 1.0, -1.0 / x, 1.0 / x);
        }
        self.draw_real_axis(&mut grid);

        // The origin marker, unless a point will cover it.
        let (cx, cy) = self.center();
        grid[cy][cx] = ORIGIN;

        for &g in points {
            let (col, row) = self.cell_of(g);
            grid[row][col] = POINT;
        }

        let mut out = String::new();
        out.push_str("  ");
        out.push_str(&fit(self.title, w.saturating_sub(2)));
        out.push('\n');
        out.push_str(&fit(&self.readout(points), w));
        out.push('\n');
        for row in grid {
            out.push_str(&row.into_iter().collect::<String>());
            out.push('\n');
        }
        out
    }

    /// The one-line Γ / VSWR readout for the first plotted point.
    fn readout(&self, points: &[Complex]) -> String {
        match points.first() {
            Some(g) => {
                let mag = g.magnitude();
                let vswr = if mag < 1.0 {
                    fmt_num((1.0 + mag) / (1.0 - mag))
                } else {
                    "∞".to_string()
                };
                format!(
                    "  Γ={}∠{}°  VSWR {vswr}",
                    fmt_num(mag),
                    fmt_num(g.phase_deg())
                )
            }
            None => "  (no point)".to_string(),
        }
    }

    /// Rasterise a circle of radius `rad` centred at impedance-plane point
    /// `(ox, oy)` onto the grid, marking the cells it passes through that lie
    /// inside the unit disk. Blank cells only, so stronger glyphs (the point
    /// marker) are never overwritten.
    fn draw_circle(&self, grid: &mut [Vec<char>], ox: f64, oy: f64, rad: f64) {
        const STEPS: usize = 1440;
        for i in 0..STEPS {
            let theta = i as f64 / STEPS as f64 * std::f64::consts::TAU;
            let x = ox + rad * theta.cos();
            let y = oy + rad * theta.sin();
            // Keep only the arc inside the (closed) unit disk.
            if x * x + y * y > 1.0 + 1e-9 {
                continue;
            }
            let (col, row) = self.cell_of(Complex::new(x, y));
            if grid[row][col] == ' ' {
                grid[row][col] = GRID;
            }
        }
    }

    /// Mark the real axis (`Γ.im = 0`) across the unit disk.
    fn draw_real_axis(&self, grid: &mut [Vec<char>]) {
        let (w, _) = self.dims();
        for col in 0..w {
            let g = Complex::new(col as f64 / (w - 1) as f64 * 2.0 - 1.0, 0.0);
            if g.re * g.re > 1.0 + 1e-9 {
                continue;
            }
            let (c, row) = self.cell_of(g);
            if grid[row][c] == ' ' {
                grid[row][c] = GRID;
            }
        }
    }
}

/// The reflection coefficient of a load impedance `z` referenced to a real
/// system impedance `z0`: `Γ = (z − z0) / (z + z0)`.
///
/// This is the inverse of [`impedance_from_gamma`]. A convenience for callers
/// that hold an impedance rather than a scattering parameter; the engine's
/// S-parameter solve returns `S11 = Γ` directly, so the panel itself never needs
/// it.
pub fn reflection_coefficient(z: Complex, z0: f64) -> Complex {
    let z0 = Complex::new(z0, 0.0);
    (z - z0) / (z + z0)
}

/// The load impedance a reflection coefficient `gamma` implies, referenced to a
/// real system impedance `z0`: `Z = z0·(1 + Γ) / (1 − Γ)`.
///
/// A perfect open (`Γ = +1`) returns a non-finite (infinite) impedance, exactly
/// as the formula's division by zero does.
pub fn impedance_from_gamma(gamma: Complex, z0: f64) -> Complex {
    let z0 = Complex::new(z0, 0.0);
    z0 * (Complex::ONE + gamma) / (Complex::ONE - gamma)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Γ = 0` (a matched load) plots at the chart centre; a short (`Γ = −1`) and
    /// an open (`Γ = +1`) land on the left and right rim, all on the real axis.
    #[test]
    fn smith_chart_places_known_reflection_coefficients() {
        let chart = SmithChart::new("S11");
        let (w, h) = chart.dims();
        let center = chart.center();

        // Matched load: dead centre.
        assert_eq!(chart.cell_of(Complex::ZERO), center);
        // A 50 Ω load on a 50 Ω line is matched → Γ = 0 → centre.
        let matched = reflection_coefficient(Complex::new(50.0, 0.0), 50.0);
        assert_eq!(chart.cell_of(matched), center);

        // Short (Z = 0): Γ = −1 → left rim, same row as the centre.
        let short = reflection_coefficient(Complex::new(0.0, 0.0), 50.0);
        assert_eq!(short, Complex::new(-1.0, 0.0));
        let (sc, sr) = chart.cell_of(short);
        assert_eq!(sc, 0, "short on the left rim");
        assert_eq!(sr, center.1, "short on the real axis");

        // Open (Z = ∞, approximated by a huge R): Γ → +1 → right rim.
        let open = reflection_coefficient(Complex::new(1e12, 0.0), 50.0);
        let (oc, or) = chart.cell_of(open);
        assert_eq!(oc, w - 1, "open on the right rim");
        assert_eq!(or, center.1, "open on the real axis");

        // The rendered frame carries the point glyphs where they belong.
        let frame = chart.render(&[Complex::ZERO, short, Complex::new(1.0, 0.0)]);
        let rows: Vec<&str> = frame.lines().collect();
        // Two header rows precede the grid rows.
        let grid: Vec<Vec<char>> = rows[2..].iter().map(|r| r.chars().collect()).collect();
        assert_eq!(grid.len(), h);
        assert_eq!(grid[center.1][center.0], POINT, "Γ=0 marked at centre");
        assert_eq!(grid[sr][0], POINT, "short marked on the left rim");
        assert_eq!(grid[or][w - 1], POINT, "open marked on the right rim");
    }

    /// A purely reactive load sits on the unit rim (|Γ| = 1), above or below the
    /// real axis by the sign of the reactance.
    #[test]
    fn reactive_loads_land_on_the_rim() {
        let chart = SmithChart::new("S11");
        let center = chart.center();
        // +j50 on a 50 Ω line → Γ = (j50−50)/(j50+50) = j → top rim.
        let inductive = reflection_coefficient(Complex::new(0.0, 50.0), 50.0);
        assert!((inductive.magnitude() - 1.0).abs() < 1e-9);
        let (_, row) = chart.cell_of(inductive);
        assert!(row < center.1, "inductive reactance plots above the axis");
        // −j50 → Γ = −j → bottom rim.
        let capacitive = reflection_coefficient(Complex::new(0.0, -50.0), 50.0);
        let (_, crow) = chart.cell_of(capacitive);
        assert!(crow > center.1, "capacitive reactance plots below the axis");
    }

    /// Γ ↔ Z round-trips through the two conversion helpers.
    #[test]
    fn gamma_and_impedance_round_trip() {
        let z = Complex::new(75.0, -30.0);
        let g = reflection_coefficient(z, 50.0);
        let back = impedance_from_gamma(g, 50.0);
        assert!((back.re - z.re).abs() < 1e-9 && (back.im - z.im).abs() < 1e-9);
    }
}
