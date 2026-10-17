//! Screen composition — several panes laid out into one framed terminal view.
//!
//! The live, dockable, resizable panel system (with a command palette and mouse)
//! is M1. This is the static rendering layer: a pure function from pane contents
//! to a single `String` of a given width and height, so a snapshot of the whole
//! UI is golden-file testable before any `ratatui`/`crossterm` event loop exists.
//!
//! The layout at M0 is fixed: a schematic pane and a scope pane sit side by side
//! across the top, with a full-width shell/log pane beneath them.
//!
//! ```text
//! ┌─ schematic ───────┐┌─ scope ───────────┐
//! │ V1  V-source  5V  ││  tran            ⡠⠊│
//! │ R1  Resistor 4.7kΩ││     ┤        ⡠⠔⠊⠁  │
//! └───────────────────┘└───────────────────┘
//! ┌─ shell ─────────────────────────────────┐
//! │ > run tran 1m                            │
//! └─────────────────────────────────────────┘
//! ```

use crate::schematic::Schematic;
use ee_core::graph::Circuit;

/// A titled box drawn around a block of text.
///
/// The box drawing characters match [`Plot`](crate::Plot)'s framing. The title is
/// written into the top border; the body is clipped (and padded) to exactly fill
/// the requested interior, so [`render`](Pane::render) always returns `height`
/// lines each exactly `width` character cells wide.
pub struct Pane<'a> {
    /// Heading shown in the top border.
    pub title: &'a str,
    /// Body text; each `\n`-separated line becomes one interior row.
    pub body: &'a str,
}

impl<'a> Pane<'a> {
    /// Build a pane from a title and a (possibly multi-line) body.
    pub fn new(title: &'a str, body: &'a str) -> Self {
        Self { title, body }
    }

    /// Render the pane as exactly `height` lines of exactly `width` cells each.
    ///
    /// `width` is clamped to at least 4 and `height` to at least 2 so the frame
    /// (two side borders, two end borders) always fits.
    pub fn render(&self, width: usize, height: usize) -> Vec<String> {
        let w = width.max(4);
        let h = height.max(2);
        let inner = w - 4; // interior text width: '│ ' + text + ' │'
        let rows = h - 2; // interior rows between the top and bottom borders

        let mut out = Vec::with_capacity(h);
        out.push(top_border(self.title, w));

        let mut lines = self.body.lines();
        for _ in 0..rows {
            let line = lines.next().unwrap_or("");
            out.push(format!("│ {} │", fit(line, inner)));
        }

        out.push(format!("└{}┘", "─".repeat(w - 2)));
        out
    }
}

/// Compose a schematic, a (pre-rendered) scope, and shell log lines into one view.
///
/// `scope` is any already-framed waveform render (typically [`Plot`](crate::Plot)
/// output); it is placed verbatim inside the scope pane. The result is a pure
/// `String`: exactly `height` lines, none wider than `width` cells.
pub struct Screen {
    /// Total view width, in character cells.
    pub width: usize,
    /// Total view height, in lines.
    pub height: usize,
}

impl Default for Screen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen {
    /// A sensible default: a 100×30 view.
    pub fn new() -> Self {
        Self {
            width: 100,
            height: 30,
        }
    }

    /// Set the total view size in character cells (clamped so every pane fits).
    pub fn size(mut self, width: usize, height: usize) -> Self {
        self.width = width.max(20);
        self.height = height.max(6);
        self
    }

    /// Lay out the three panes with the schematic pane replaced by the live
    /// operating-point [`Overlay`](crate::Overlay).
    ///
    /// Identical geometry to [`render`](Screen::render); the schematic pane shows
    /// each net's node voltage, colour-coded when `color` is set. The voltages are
    /// data the caller supplies (see [`NodeVoltages`](crate::overlay::NodeVoltages)),
    /// keeping `ee-tui` decoupled from the solver.
    pub fn render_overlay(
        &self,
        circuit: &Circuit,
        volts: &crate::overlay::NodeVoltages,
        scope: &str,
        shell: &[&str],
        color: bool,
    ) -> String {
        let w = self.width.max(20);
        let h = self.height.max(6);

        let left_w = w / 2;
        let right_w = w - left_w;
        let bottom_h = (h / 3).clamp(3, h - 3);
        let top_h = h - bottom_h;

        let left = crate::overlay::Overlay::new("op-point")
            .width(left_w)
            .color(color)
            .pane_lines(circuit, volts, top_h);
        let right = Pane::new("scope", scope).render(right_w, top_h);
        let shell_body = shell.join("\n");
        let bottom = Pane::new("shell", &shell_body).render(w, bottom_h);

        let mut out = String::new();
        for (l, r) in left.iter().zip(&right) {
            out.push_str(l);
            out.push_str(r);
            out.push('\n');
        }
        for line in &bottom {
            out.push_str(line);
            out.push('\n');
        }
        out
    }

    /// Lay out a full-width [`Breadboard`](crate::Breadboard) above a full-width
    /// shell/log pane.
    ///
    /// The breadboard wants horizontal room, so unlike [`render`](Screen::render)
    /// it spans the whole width rather than sharing the top row with a scope. The
    /// board is clipped to fit if the view is narrower than its natural width.
    pub fn render_breadboard(&self, circuit: &Circuit, shell: &[&str]) -> String {
        let w = self.width.max(20);
        let h = self.height.max(6);
        let bottom_h = (h / 3).clamp(3, h - 3);
        let top_h = h - bottom_h;

        let top = crate::breadboard::Breadboard::new("breadboard").pane_lines(circuit, w, top_h);
        let shell_body = shell.join("\n");
        let bottom = Pane::new("shell", &shell_body).render(w, bottom_h);

        let mut out = String::new();
        for line in top.iter().chain(&bottom) {
            out.push_str(line);
            out.push('\n');
        }
        out
    }

    /// Lay out the three panes and return the composed frame.
    pub fn render(&self, circuit: &Circuit, scope: &str, shell: &[&str]) -> String {
        let w = self.width.max(20);
        let h = self.height.max(6);

        // Top row: schematic beside scope. Bottom: a full-width shell/log pane.
        let left_w = w / 2;
        let right_w = w - left_w;
        let bottom_h = (h / 3).clamp(3, h - 3);
        let top_h = h - bottom_h;

        let rows = Schematic::new("schematic").width(left_w).rows(circuit);
        let sch_body = if rows.is_empty() {
            "(no parts)".to_string()
        } else {
            rows.join("\n")
        };

        let left = Pane::new("schematic", &sch_body).render(left_w, top_h);
        let right = Pane::new("scope", scope).render(right_w, top_h);
        let shell_body = shell.join("\n");
        let bottom = Pane::new("shell", &shell_body).render(w, bottom_h);

        let mut out = String::new();
        for (l, r) in left.iter().zip(&right) {
            out.push_str(l);
            out.push_str(r);
            out.push('\n');
        }
        for line in &bottom {
            out.push_str(line);
            out.push('\n');
        }
        out
    }
}

/// The top border with the title inlined: `┌─ title ─────┐`, exactly `w` cells.
pub(crate) fn top_border(title: &str, w: usize) -> String {
    let cap = w - 2; // room between the two corner glyphs
    let raw = format!("─ {title} ");
    let mut inner: String = raw.chars().take(cap).collect();
    let used = inner.chars().count();
    inner.push_str(&"─".repeat(cap.saturating_sub(used)));
    format!("┌{inner}┐")
}

/// Clip or space-pad a string to exactly `width` character cells.
fn fit(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n >= width {
        s.chars().take(width).collect()
    } else {
        let mut out = String::with_capacity(s.len() + (width - n));
        out.push_str(s);
        out.push_str(&" ".repeat(width - n));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Plot;
    use ee_core::graph::{NetId, PartKind};
    use ee_core::units::Unit;
    use ee_core::value::Value;

    fn rc_circuit() -> Circuit {
        let mut c = Circuit::new("rc");
        let vin = c.new_net();
        let out = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            Some(Value::new(5.0, Unit::Volt)),
            vec![vin, NetId::GROUND],
        );
        c.add_part(
            "R1",
            PartKind::Resistor,
            Some(Value::new(4700.0, Unit::Ohm)),
            vec![vin, out],
        );
        c
    }

    fn scope() -> String {
        let x: Vec<f64> = (0..32).map(|i| i as f64).collect();
        let y: Vec<f64> = x.iter().map(|t| (t / 5.0).sin()).collect();
        Plot::new("tran").size(16, 6).render(&x, &y)
    }

    #[test]
    fn pane_is_exactly_width_by_height() {
        let pane = Pane::new("log", "one\ntwo\nthree");
        let lines = pane.render(20, 4);
        assert_eq!(lines.len(), 4, "exactly height lines");
        for line in &lines {
            assert_eq!(line.chars().count(), 20, "exactly width cells: {line:?}");
        }
        assert!(lines[0].contains("log"), "title in the top border");
    }

    #[test]
    fn composition_contains_every_pane_and_fits() {
        let width = 80;
        let height = 20;
        let out = Screen::new().size(width, height).render(
            &rc_circuit(),
            &scope(),
            &["> run tran 1m", "sim: ok"],
        );

        // A part reference from the schematic pane.
        assert!(out.contains("R1"), "schematic pane:\n{out}");
        // A braille glyph from the scope pane (anything above the blank U+2800).
        assert!(
            out.chars().any(|c| ('\u{2801}'..='\u{28FF}').contains(&c)),
            "scope pane braille:\n{out}"
        );
        // A shell log line.
        assert!(out.contains("sim: ok"), "shell pane:\n{out}");

        // Exactly the requested height, and no line wider than the requested width.
        assert_eq!(out.lines().count(), height, "line count == height");
        for line in out.lines() {
            assert!(
                line.chars().count() <= width,
                "line exceeds width {width}: {line:?}"
            );
        }
    }

    #[test]
    fn overlay_pane_composes_with_voltages_and_fits() {
        use crate::overlay::NodeVoltages;
        let c = rc_circuit();
        // Annotate the live net; ground is implicit 0 V.
        let mut v = NodeVoltages::new();
        v.set(c.resolve(NetId(1)), 5.0);

        let width = 80;
        let height = 20;
        let out = Screen::new().size(width, height).render_overlay(
            &c,
            &v,
            &scope(),
            &["> op", "sim: ok"],
            false,
        );

        // The overlay pane carries a net=voltage annotation and ground at 0 V.
        assert!(out.contains("n1=5V"), "overlay voltage annotation:\n{out}");
        assert!(out.contains("gnd=0V"), "ground reference:\n{out}");
        // Scope and shell panes still compose in.
        assert!(
            out.chars()
                .any(|ch| ('\u{2801}'..='\u{28FF}').contains(&ch)),
            "scope pane braille:\n{out}"
        );
        assert!(out.contains("sim: ok"), "shell pane:\n{out}");

        assert_eq!(out.lines().count(), height, "line count == height");
        for line in out.lines() {
            assert!(
                line.chars().count() <= width,
                "line exceeds width {width}: {line:?}"
            );
        }
    }

    #[test]
    fn breadboard_composition_spans_full_width_and_fits() {
        let width = 80;
        let height = 20;
        let out = Screen::new()
            .size(width, height)
            .render_breadboard(&rc_circuit(), &["> place R1", "ok"]);

        // The breadboard pane: a part reference, the trench, and tie-point holes.
        assert!(out.contains("R1"), "breadboard pane:\n{out}");
        assert!(out.contains('═'), "trench:\n{out}");
        assert!(out.contains('·'), "tie-point holes:\n{out}");
        // The shell pane still composes beneath it.
        assert!(out.contains("ok"), "shell pane:\n{out}");

        assert_eq!(out.lines().count(), height, "line count == height");
        for line in out.lines() {
            assert_eq!(
                line.chars().count(),
                width,
                "line is not exactly {width} cells: {line:?}"
            );
        }
    }

    #[test]
    fn empty_circuit_composes_without_panicking() {
        let out = Screen::new()
            .size(60, 12)
            .render(&Circuit::new("empty"), &scope(), &["booting"]);
        assert!(
            out.contains("(no parts)"),
            "empty schematic placeholder:\n{out}"
        );
        assert_eq!(out.lines().count(), 12);
        for line in out.lines() {
            assert!(line.chars().count() <= 60);
        }
    }
}
