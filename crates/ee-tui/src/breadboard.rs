//! Breadboard view — the circuit drawn as a solderless-breadboard layout
//! (`CP-UX-M2.2`).
//!
//! Where [`Schematic`](crate::Schematic) lists the circuit as a component table
//! and [`Overlay`](crate::Overlay) paints the operating point onto it, this view
//! lays the parts out on a **breadboard**: the `+`/`−` power rails top and bottom,
//! a numbered central field split by the trench, and each part plugged across its
//! tie-points. It is the same pure, dependency-free, render-to-`String` layer as
//! the other widgets — Unicode-width-correct (every measurement is on visible
//! `chars().count()`, never bytes) and golden-file testable before any terminal
//! event loop exists.
//!
//! **How parts and nets map to tie-points.** On a real breadboard the five holes
//! of a column form one electrically common node, so a column *is* a net. This
//! view makes that mapping literal: every distinct net the circuit touches becomes
//! one labelled column (`gnd`, `n1`, `n2`, …), ordered deterministically with
//! ground first and the rest ascending. Wired-together nets collapse through the
//! circuit's union-find to a single column — that shared column is the wire. Each
//! part gets one row in the upper bank; its leads plug into the columns of the
//! nets its terminals resolve to (`●`), its body spans between them (`─`) carrying
//! the standard designator letter (`R`, `C`, `Q`, …), and its kind and value are
//! annotated to the right. Parts touching the same net stack `●` down that
//! column, so the tie-point group reads at a glance.
//!
//! ```text
//! ┌─ breadboard ─────────────────────┐
//! │ +  ·    ·    ·                   │
//! │ -  ·    ·    ·                   │
//! │    gnd  n1   n2                  │
//! │ V1 ●─V──●    ·    V-source 5V    │
//! │ R1 ·    ●─R──●    Resistor 4.7kΩ │
//! │ C1 ●────C────●    Capacitor 1µF  │
//! │    ═════════════                 │
//! │    ·    ·    ·                   │
//! │    ·    ·    ·                   │
//! │    ·    ·    ·                   │
//! │ -  ·    ·    ·                   │
//! │ +  ·    ·    ·                   │
//! └──────────────────────────────────┘
//! ```

use crate::schematic::{fmt_opt_value, kind_label, net_name};
use crate::screen::top_border;
use ee_core::graph::{Circuit, NetId, Part, PartKind};
use std::collections::BTreeSet;

/// Cells per column: the hole plus the spacing/lead room to its right.
const COL_W: usize = 5;

/// The breadboard hole (an empty tie-point).
const HOLE: char = '·';
/// A plugged-in lead (a part terminal in that column).
const LEAD: char = '●';

/// A circuit rendered as a framed, solderless-breadboard layout.
///
/// Width is intrinsic: the frame grows to fit the columns and the longest part
/// annotation. [`min_width`](Breadboard::min_width) only pads a narrow board out
/// to a floor; it never clips (a clipped breadboard is unreadable). Every output
/// line is exactly the same number of character cells wide.
pub struct Breadboard<'a> {
    /// Heading shown in the frame's top border.
    pub title: &'a str,
    /// A floor for the frame width, in character cells; the board may render wider.
    pub min_width: usize,
}

impl<'a> Breadboard<'a> {
    /// A new breadboard view titled `title` (intrinsic width, no floor).
    pub fn new(title: &'a str) -> Self {
        Self {
            title,
            min_width: 0,
        }
    }

    /// Set a minimum frame width in character cells. The board renders at least
    /// this wide, padding on the right; it is never clipped narrower.
    pub fn min_width(mut self, min_width: usize) -> Self {
        self.min_width = min_width;
        self
    }

    /// Render the circuit to a framed, multi-line breadboard string.
    ///
    /// An empty circuit still renders a valid titled board (bare rails and trench
    /// with a `(no parts)` note), never a panic. Every line is exactly the frame
    /// width in character cells.
    pub fn render(&self, circuit: &Circuit) -> String {
        let rows = self.content_rows(circuit);
        self.frame(&rows).join("\n")
    }

    /// Render as exactly `height` lines clipped to `width` for use as a
    /// [`Screen`](crate::Screen) pane. Unlike [`render`](Breadboard::render) this
    /// honours a fixed pane size, so a board wider than the pane is clipped.
    pub(crate) fn pane_lines(&self, circuit: &Circuit, width: usize, height: usize) -> Vec<String> {
        let w = width.max(8);
        let h = height.max(2);
        let inner = w - 4; // '│ ' + text + ' │'
        let rows = self.content_rows(circuit);

        let mut out = Vec::with_capacity(h);
        out.push(top_border(self.title, w));
        for i in 0..h - 2 {
            let line = rows.get(i).map(String::as_str).unwrap_or("");
            out.push(format!("│ {} │", fit(line, inner)));
        }
        out.push(format!("└{}┘", "─".repeat(w - 2)));
        out
    }

    /// Wrap the (already uniform-width) content rows in a titled box.
    fn frame(&self, rows: &[String]) -> Vec<String> {
        let inner = rows.first().map(|r| r.chars().count()).unwrap_or(0);
        // Frame must clear the title ('┌─ title ─┐') and any floor width.
        let title_min = self.title.chars().count() + 6;
        let w = (inner + 4).max(self.min_width).max(title_min);
        let inner = w - 4;

        let mut out = Vec::with_capacity(rows.len() + 2);
        out.push(top_border(self.title, w));
        for row in rows {
            out.push(format!("│ {} │", fit(row, inner)));
        }
        out.push(format!("└{}┘", "─".repeat(w - 2)));
        out
    }

    /// Build the breadboard's body as uniform-width, unframed rows.
    fn content_rows(&self, circuit: &Circuit) -> Vec<String> {
        let parts = circuit.parts();

        // Columns: one per distinct resolved net. A BTreeSet gives a deterministic
        // ascending order, so ground (net 0) is always the first column.
        let nets: Vec<NetId> = parts
            .iter()
            .flat_map(|p| p.terminals.iter().map(|t| circuit.resolve(*t)))
            .map(|n| n.0)
            .collect::<BTreeSet<u32>>()
            .into_iter()
            .map(NetId)
            .collect();

        // Gutter holds the row label: a part reference, or a rail's '+'/'−'.
        let gutter = parts
            .iter()
            .map(|p| p.reference.chars().count())
            .max()
            .unwrap_or(0)
            .max(1);

        if nets.is_empty() {
            return self.empty_rows(gutter);
        }

        let col_of: std::collections::HashMap<u32, usize> =
            nets.iter().enumerate().map(|(i, n)| (n.0, i)).collect();
        // The field is wide enough for every hole plus the longest net label.
        let tail = nets
            .iter()
            .map(|n| net_name(*n).chars().count())
            .max()
            .unwrap_or(1)
            .max(1);
        let field_w = (nets.len() - 1) * COL_W + tail;

        let banks = parts.len(); // upper bank = one row per part
        let lower = banks.clamp(2, 5); // a bounded strip of free holes below

        let mut rows: Vec<String> = Vec::new();
        // Top power rails.
        rows.push(row(gutter, "+", &hole_field(&nets, field_w), ""));
        rows.push(row(gutter, "-", &hole_field(&nets, field_w), ""));
        // Column header: the net each column carries.
        rows.push(row(gutter, "", &header_field(&nets, field_w, &col_of), ""));
        // Upper bank: one part per row, plugged across its tie-points.
        for p in parts {
            let (field, annot) = self.part_row(p, circuit, field_w, &col_of);
            rows.push(row(gutter, &p.reference, &field, &annot));
        }
        // The trench, then the free lower bank, then the bottom rails.
        rows.push(row(gutter, "", &trench_field(field_w), ""));
        for _ in 0..lower {
            rows.push(row(gutter, "", &hole_field(&nets, field_w), ""));
        }
        rows.push(row(gutter, "-", &hole_field(&nets, field_w), ""));
        rows.push(row(gutter, "+", &hole_field(&nets, field_w), ""));

        pad_uniform(rows)
    }

    /// A bare board for a circuit with no parts: rails and a trench over a few
    /// placeholder columns, plus a `(no parts)` note.
    fn empty_rows(&self, gutter: usize) -> Vec<String> {
        let placeholder: Vec<NetId> = (0..3).map(NetId).collect();
        let field_w = (placeholder.len() - 1) * COL_W + 1;
        let rows = vec![
            row(gutter, "+", &hole_field(&placeholder, field_w), ""),
            row(gutter, "-", &hole_field(&placeholder, field_w), ""),
            row(
                gutter,
                "",
                &hole_field(&placeholder, field_w),
                "  (no parts)",
            ),
            row(gutter, "", &trench_field(field_w), ""),
            row(gutter, "-", &hole_field(&placeholder, field_w), ""),
            row(gutter, "+", &hole_field(&placeholder, field_w), ""),
        ];
        pad_uniform(rows)
    }

    /// Build one part's field row (holes + plugged leads + body span) and its
    /// trailing `kind value` annotation.
    fn part_row(
        &self,
        part: &Part,
        circuit: &Circuit,
        field_w: usize,
        col_of: &std::collections::HashMap<u32, usize>,
    ) -> (String, String) {
        let mut cells = vec![' '; field_w];
        // Every column shows its hole first; leads and the body paint over them.
        for c in 0..=(field_w.saturating_sub(1) / COL_W) {
            let x = c * COL_W;
            if x < field_w {
                cells[x] = HOLE;
            }
        }

        let cols: Vec<usize> = part
            .terminals
            .iter()
            .filter_map(|t| col_of.get(&circuit.resolve(*t).0).copied())
            .collect();

        if let (Some(&lo), Some(&hi)) = (cols.iter().min(), cols.iter().max()) {
            let (x0, x1) = (lo * COL_W, hi * COL_W);
            // The body: a lead-to-lead line carrying the designator letter.
            for cell in cells.iter_mut().take(x1 + 1).skip(x0) {
                *cell = '─';
            }
            let mid = (x0 + x1) / 2;
            if mid > x0 && mid < x1 {
                cells[mid] = designator(&part.kind);
            }
            // Plug the leads in last so they sit on top of the body line.
            for &c in &cols {
                cells[c * COL_W] = LEAD;
            }
        }

        let val = fmt_opt_value(&part.value);
        let annot = if val.is_empty() {
            format!("  {}", kind_label(&part.kind))
        } else {
            format!("  {} {}", kind_label(&part.kind), val)
        };
        (cells.into_iter().collect(), annot)
    }
}

/// The standard reference-designator letter for a part kind — the glyph its body
/// carries on the board. Exhaustive over [`PartKind`].
fn designator(kind: &PartKind) -> char {
    match kind {
        PartKind::Resistor => 'R',
        PartKind::Capacitor => 'C',
        PartKind::Inductor => 'L',
        PartKind::VoltageSource => 'V',
        PartKind::CurrentSource => 'I',
        PartKind::Diode(_) => 'D',
        PartKind::Bjt(..) => 'Q',
        PartKind::Mosfet(..) => 'M',
        PartKind::Vcvs(_) => 'E',
        PartKind::Vccs(_) => 'G',
        PartKind::Ccvs(..) => 'H',
        PartKind::Cccs(..) => 'F',
        PartKind::Other(_) => 'U',
    }
}

/// A field of empty holes, one per column.
fn hole_field(nets: &[NetId], field_w: usize) -> String {
    let mut cells = vec![' '; field_w];
    for c in 0..nets.len() {
        let x = c * COL_W;
        if x < field_w {
            cells[x] = HOLE;
        }
    }
    cells.into_iter().collect()
}

/// The column header: each column labelled with the net it carries.
fn header_field(
    nets: &[NetId],
    field_w: usize,
    col_of: &std::collections::HashMap<u32, usize>,
) -> String {
    let mut cells = vec![' '; field_w];
    for n in nets {
        let x = col_of[&n.0] * COL_W;
        for (i, ch) in net_name(*n).chars().enumerate() {
            if x + i < field_w {
                cells[x + i] = ch;
            }
        }
    }
    cells.into_iter().collect()
}

/// The trench: the groove splitting the central field, drawn with doubled rule so
/// it never reads as a part's body line.
fn trench_field(field_w: usize) -> String {
    "═".repeat(field_w)
}

/// Assemble `gutter` + field + trailing annotation into one row.
fn row(gutter: usize, label: &str, field: &str, annot: &str) -> String {
    let lbl = fit(label, gutter);
    format!("{lbl} {field}{annot}")
}

/// Pad every row to the width of the widest, measured in visible characters.
fn pad_uniform(rows: Vec<String>) -> Vec<String> {
    let w = rows.iter().map(|r| r.chars().count()).max().unwrap_or(0);
    rows.into_iter().map(|r| fit(&r, w)).collect()
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
    use ee_core::graph::{BjtModel, MosfetModel, Polarity};
    use ee_core::units::Unit;
    use ee_core::value::Value;

    /// A minimal RC circuit: a source, a resistor, and a capacitor to ground.
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
        c.add_part(
            "C1",
            PartKind::Capacitor,
            Some(Value::new(1e-6, Unit::Farad)),
            vec![out, NetId::GROUND],
        );
        c
    }

    #[test]
    fn renders_a_recognizable_breadboard() {
        let out = Breadboard::new("breadboard").render(&rc_circuit());
        // Power rails top and bottom.
        assert!(
            out.contains('+') && out.contains('-'),
            "power rails:\n{out}"
        );
        // The trench between the two banks.
        assert!(out.contains('═'), "trench:\n{out}");
        // Tie-point holes and plugged leads.
        assert!(out.contains(HOLE), "empty holes:\n{out}");
        assert!(out.contains(LEAD), "plugged leads:\n{out}");
        // Net columns are labelled, ground first.
        assert!(
            out.contains("gnd") && out.contains("n1"),
            "net columns:\n{out}"
        );
        // Every part is placed with its designator and value.
        for needle in ["V1", "R1", "C1", "4.7kΩ", "1µF", "Resistor"] {
            assert!(out.contains(needle), "missing {needle:?}:\n{out}");
        }
        // The frame matches the house box-drawing style.
        for border in ['┌', '┐', '└', '┘', '│'] {
            assert!(out.contains(border), "missing border {border}:\n{out}");
        }
    }

    #[test]
    fn every_line_is_the_same_width() {
        let out = Breadboard::new("rc").render(&rc_circuit());
        let w = out.lines().next().unwrap().chars().count();
        for line in out.lines() {
            assert_eq!(
                line.chars().count(),
                w,
                "line is not {w} cells (char-measured): {line:?}"
            );
        }
    }

    #[test]
    fn min_width_pads_but_never_clips() {
        let narrow = Breadboard::new("rc").render(&rc_circuit());
        let intrinsic = narrow.lines().next().unwrap().chars().count();
        // A floor wider than intrinsic pads the board out to it.
        let wide = Breadboard::new("rc")
            .min_width(intrinsic + 20)
            .render(&rc_circuit());
        assert_eq!(wide.lines().next().unwrap().chars().count(), intrinsic + 20);
        // A floor narrower than intrinsic is ignored (no clipping).
        let floored = Breadboard::new("rc").min_width(4).render(&rc_circuit());
        assert_eq!(floored.lines().next().unwrap().chars().count(), intrinsic);
    }

    #[test]
    fn wired_nets_collapse_to_one_column() {
        // Two raw nets wired together must share a single tie-point column.
        let mut c = Circuit::new("w");
        let a = c.new_net();
        let b = c.new_net();
        c.wire(a, b);
        c.add_part("R1", PartKind::Resistor, None, vec![a, b]);
        let out = Breadboard::new("w").render(&c);
        // Both terminals collapse to n1, so n2 is never a column.
        assert!(out.contains("n1"), "the wire's shared column:\n{out}");
        assert!(!out.contains("n2"), "wired nets must collapse:\n{out}");
    }

    #[test]
    fn empty_circuit_renders_a_bare_board_without_panicking() {
        let out = Breadboard::new("empty").render(&Circuit::new("empty"));
        assert!(out.contains("empty"), "title in frame:\n{out}");
        assert!(out.contains("(no parts)"), "placeholder note:\n{out}");
        assert!(out.contains('═'), "trench still drawn:\n{out}");
        assert!(out.contains('┌') && out.contains('┘'), "framed:\n{out}");
        let w = out.lines().next().unwrap().chars().count();
        for line in out.lines() {
            assert_eq!(line.chars().count(), w);
        }
    }

    #[test]
    fn multi_terminal_parts_get_their_designator_letter() {
        // One of every multi-terminal active device; each must place its leads and
        // carry its standard designator letter on the board.
        let mut c = Circuit::new("act");
        let (a, b, d, e) = (c.new_net(), c.new_net(), c.new_net(), c.new_net());
        c.add_part(
            "Q1",
            PartKind::Bjt(Polarity::N, BjtModel::default()),
            None,
            vec![a, b, NetId::GROUND],
        );
        c.add_part(
            "M1",
            PartKind::Mosfet(Polarity::N, MosfetModel::default()),
            None,
            vec![d, e, NetId::GROUND],
        );
        c.add_part("E1", PartKind::Vcvs(100.0), None, vec![a, b, d, e]);
        let out = Breadboard::new("act").render(&c);
        for (designator, label) in [('Q', "BJT"), ('M', "MOSFET"), ('E', "VCVS")] {
            assert!(out.contains(designator), "designator {designator}:\n{out}");
            assert!(out.contains(label), "kind label {label}:\n{out}");
        }
    }

    #[test]
    fn plain_render_matches_golden_snapshot() {
        let out = Breadboard::new("breadboard").render(&rc_circuit());
        let expected = "\
┌─ breadboard ─────────────────────┐
│ +  ·    ·    ·                   │
│ -  ·    ·    ·                   │
│    gnd  n1   n2                  │
│ V1 ●─V──●    ·    V-source 5V    │
│ R1 ·    ●─R──●    Resistor 4.7kΩ │
│ C1 ●────C────●    Capacitor 1µF  │
│    ═════════════                 │
│    ·    ·    ·                   │
│    ·    ·    ·                   │
│    ·    ·    ·                   │
│ -  ·    ·    ·                   │
│ +  ·    ·    ·                   │
└──────────────────────────────────┘";
        assert_eq!(out, expected, "golden mismatch:\n{out}");
    }
}
