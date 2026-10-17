//! Schematic list view — the circuit as a readable component table.
//!
//! The live, dockable schematic canvas with mouse-driven placement is M1; this
//! is the static rendering layer underneath it. Like [`Plot`](crate::Plot) it is
//! dependency-free and renders to a `String`, which keeps it golden-file testable
//! and lets the shell and examples show a netlist before the `ratatui` event loop
//! exists.
//!
//! Each part becomes one row — reference, kind, value (magnitude + unit symbol in
//! engineering notation), and the nets its terminals connect to. Nets are
//! resolved through the circuit's union-find so wired-together nodes collapse to a
//! single canonical name: ground → `gnd`, everything else → `n<id>`.

use crate::screen::Pane;
use ee_core::graph::{Circuit, NetId, PartKind};
use ee_core::value::Value;

/// A circuit rendered as a framed, column-aligned component list.
///
/// The width is in character cells; the component rows are clipped to fit it.
pub struct Schematic<'a> {
    /// Heading shown in the frame's top border.
    pub title: &'a str,
    /// Total frame width, in character cells.
    pub width: usize,
}

impl<'a> Schematic<'a> {
    /// A sensible default: a 72-cell-wide frame.
    pub fn new(title: &'a str) -> Self {
        Self { title, width: 72 }
    }

    /// Set the frame width in character cells (clamped so the frame stays valid).
    pub fn width(mut self, width: usize) -> Self {
        self.width = width.max(8);
        self
    }

    /// Render the circuit to a framed, multi-line string.
    ///
    /// An empty circuit still renders a valid titled frame (with a `(no parts)`
    /// placeholder row), never a panic. Every output line is exactly `width`
    /// character cells wide.
    pub fn render(&self, circuit: &Circuit) -> String {
        let rows = self.rows(circuit);
        let body = if rows.is_empty() {
            "(no parts)".to_string()
        } else {
            rows.join("\n")
        };
        let height = rows.len().max(1) + 2; // content rows + top/bottom border
        Pane::new(self.title, &body)
            .render(self.width, height)
            .join("\n")
    }

    /// Build the unframed component rows, one `String` per part, columns aligned.
    ///
    /// Shared with [`Screen`](crate::Screen) so a schematic pane and the
    /// standalone widget format parts identically.
    pub(crate) fn rows(&self, circuit: &Circuit) -> Vec<String> {
        let parts = circuit.parts();
        let vals: Vec<String> = parts.iter().map(|p| fmt_opt_value(&p.value)).collect();

        // Column widths autoscale to the data (measured in chars, not bytes, so
        // multi-byte unit symbols like Ω and µ line up).
        let rw = parts
            .iter()
            .map(|p| p.reference.chars().count())
            .max()
            .unwrap_or(0);
        let kw = parts
            .iter()
            .map(|p| kind_label(&p.kind).chars().count())
            .max()
            .unwrap_or(0);
        let vw = vals.iter().map(|v| v.chars().count()).max().unwrap_or(0);

        parts
            .iter()
            .zip(&vals)
            .map(|(p, val)| {
                let nodes = p
                    .terminals
                    .iter()
                    .map(|t| net_name(circuit.resolve(*t)))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "{:<rw$}  {:<kw$}  {:<vw$}  [{nodes}]",
                    p.reference,
                    kind_label(&p.kind),
                    val
                )
            })
            .collect()
    }
}

/// The canonical display name for a resolved net: ground is `gnd`, else `n<id>`.
pub(crate) fn net_name(net: NetId) -> String {
    if net == NetId::GROUND {
        "gnd".to_string()
    } else {
        format!("n{}", net.0)
    }
}

/// A short, stable label for each part kind. Exhaustive over [`PartKind`].
pub(crate) fn kind_label(kind: &PartKind) -> &str {
    match kind {
        PartKind::Resistor => "Resistor",
        PartKind::Capacitor => "Capacitor",
        PartKind::Inductor => "Inductor",
        PartKind::VoltageSource => "V-source",
        PartKind::CurrentSource => "I-source",
        PartKind::Diode => "Diode",
        PartKind::Bjt(..) => "BJT",
        PartKind::Mosfet(..) => "MOSFET",
        PartKind::Other(name) => name,
    }
}

/// Format an optional value; a part with no value renders a blank value column.
pub(crate) fn fmt_opt_value(value: &Option<Value>) -> String {
    match value {
        Some(v) => fmt_value(v),
        None => String::new(),
    }
}

/// Format a value in engineering notation with its SI prefix and unit symbol,
/// e.g. `4700 Ω` → `4.7kΩ`, `1e-6 F` → `1µF`, `5 V` → `5V`.
pub(crate) fn fmt_value(value: &Value) -> String {
    let sym = value.unit.symbol();
    let m = value.magnitude;
    if m == 0.0 {
        return format!("0{sym}");
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
    let a = m.abs();
    for &(factor, prefix) in PREFIXES {
        if a >= factor {
            return format!("{}{prefix}{sym}", fmt_num(m / factor));
        }
    }
    format!("{}p{sym}", fmt_num(m / 1e-12))
}

/// Compact number formatting: three decimals, trailing zeros trimmed, no `-0`.
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
    use ee_core::units::Unit;

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
    fn rc_circuit_lists_every_part() {
        let out = Schematic::new("rc").width(48).render(&rc_circuit());
        // Each reference, a couple of kind labels, and the resolved node names.
        for needle in ["V1", "R1", "C1", "Resistor", "Capacitor", "n1", "gnd"] {
            assert!(out.contains(needle), "render is missing {needle:?}:\n{out}");
        }
        // Engineering-notation values with their unit symbols.
        assert!(out.contains("4.7kΩ"), "resistor value:\n{out}");
        assert!(out.contains("1µF"), "capacitor value:\n{out}");
        // A box frame consistent with Plot's framing characters.
        for border in ['┌', '┐', '└', '┘', '│'] {
            assert!(out.contains(border), "missing border {border}:\n{out}");
        }
    }

    #[test]
    fn every_line_fits_the_configured_width() {
        let width = 40;
        let out = Schematic::new("rc").width(width).render(&rc_circuit());
        for line in out.lines() {
            assert_eq!(
                line.chars().count(),
                width,
                "line is not exactly {width} cells: {line:?}"
            );
        }
    }

    #[test]
    fn empty_circuit_renders_a_valid_titled_frame() {
        let out = Schematic::new("empty")
            .width(24)
            .render(&Circuit::new("empty"));
        assert!(out.contains("empty"), "title shows in the frame:\n{out}");
        assert!(out.contains("(no parts)"), "placeholder row:\n{out}");
        assert!(out.contains('┌') && out.contains('┘'), "framed:\n{out}");
        for line in out.lines() {
            assert_eq!(line.chars().count(), 24);
        }
    }

    #[test]
    fn wired_nets_resolve_to_one_canonical_name() {
        // Two raw nets wired together must render as a single node name.
        let mut c = Circuit::new("w");
        let a = c.new_net();
        let b = c.new_net();
        c.wire(a, b);
        c.add_part("R1", PartKind::Resistor, None, vec![a, b]);
        let rows = Schematic::new("w").rows(&c);
        assert_eq!(rows.len(), 1);
        // Both terminals collapse to the lower id (n1), so n2 never appears.
        assert!(rows[0].contains("n1"), "{rows:?}");
        assert!(
            !rows[0].contains("n2"),
            "wired nets must collapse: {rows:?}"
        );
    }

    #[test]
    fn current_source_and_other_kinds_are_labeled() {
        let mut c = Circuit::new("k");
        let n = c.new_net();
        c.add_part(
            "I1",
            PartKind::CurrentSource,
            Some(Value::new(0.02, Unit::Ampere)),
            vec![n, NetId::GROUND],
        );
        c.add_part(
            "D1",
            PartKind::Other("Diode".to_string()),
            None,
            vec![n, NetId::GROUND],
        );
        let out = Schematic::new("k").render(&c);
        assert!(out.contains("I-source"), "current source label:\n{out}");
        assert!(out.contains("Diode"), "Other(name) label:\n{out}");
    }
}
