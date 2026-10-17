//! Live operating-point overlay — the schematic annotated with node voltages
//! (`CP-UX-M1.3`).
//!
//! Where [`Schematic`](crate::Schematic) lists the circuit as a static component
//! table, this view paints the **DC state onto it**: every net its parts touch is
//! labelled with its node voltage and colour-coded by level, so the operating
//! point is something you can *see* rather than read off a separate table.
//!
//! Like the rest of `ee-tui` this is a pure render-to-`String` layer with no
//! terminal event loop (that is `CP-UX-M1.4`) and — crucially — **no dependency on
//! `ee-sim`**. The op-point voltages come in as data: either the single-sample
//! [`Signal`](ee_core::Signal)s a DC solve emits (named `v(n<net>)`), via
//! [`NodeVoltages::from_signals`], or a map the caller builds by hand with
//! [`NodeVoltages::set`]. Ground is always 0 V and need not be supplied; an
//! unknown/unsolved net renders as `?` rather than panicking.
//!
//! ```text
//! ┌─ op-point ──────────────────────────────┐
//! │ V1  V-source  5V    [n1=5V, gnd=0V]      │
//! │ R1  Resistor  10kΩ  [n1=5V, n2=2.5V]     │
//! │ R2  Resistor  10kΩ  [n2=2.5V, gnd=0V]    │
//! └──────────────────────────────────────────┘
//! ```
//!
//! Colour is applied with ANSI escapes in the same house style as the binary's
//! prompt, but the width maths is done on *visible* characters only: each coloured
//! token is emitted as `<code><text><reset>`, the escapes carry zero width, and
//! every line is padded/clipped to its exact cell width regardless of colour. A
//! plain (no-escape) mode drives golden snapshots and width assertions.

use crate::schematic::{fmt_opt_value, fmt_value, kind_label, net_name};
use crate::screen::top_border;
use ee_core::graph::{Circuit, NetId};
use ee_core::units::Unit;
use ee_core::value::Value;
use std::collections::HashMap;

/// Voltages at or below this magnitude read as the "zero" bucket (ground level).
const ZERO_EPS: f64 = 1e-9;

/// ANSI reset — closes every coloured token so a pane to its right is unaffected.
const RESET: &str = "\x1b[0m";

/// Operating-point node voltages, keyed by **resolved** [`NetId`].
///
/// Keys are the representatives [`Circuit::resolve`] returns (wired nets collapse
/// to one), which is exactly the net id a DC solve names its `v(n<net>)` signals
/// after. Ground ([`NetId::GROUND`]) is implicitly 0 V and is never stored.
#[derive(Debug, Clone, Default)]
pub struct NodeVoltages {
    v: HashMap<NetId, f64>,
}

impl NodeVoltages {
    /// An empty map — every non-ground net reads as unsolved (`?`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from operating-point signals named `v(n<net>)`.
    ///
    /// Each signal's first real sample is taken as the node voltage; complex or
    /// empty signals and names that are not of the `v(n<net>)` form are skipped.
    /// A later signal for the same net wins. Decoupled from `ee-sim` by design: a
    /// DC solve's `Vec<Signal>` drops straight in.
    pub fn from_signals(signals: &[ee_core::signal::Signal]) -> Self {
        let mut v = HashMap::new();
        for s in signals {
            if let (Some(id), Some(&first)) =
                (parse_net_name(&s.name), s.real().and_then(|r| r.first()))
            {
                v.insert(NetId(id), first);
            }
        }
        Self { v }
    }

    /// Record (or overwrite) the voltage at a resolved net. Chainable.
    pub fn set(&mut self, net: NetId, volts: f64) -> &mut Self {
        self.v.insert(net, volts);
        self
    }

    /// The voltage at a resolved net: ground is always `Some(0.0)`; a stored net
    /// returns its value; an unknown net returns `None` (rendered as `?`).
    pub fn get(&self, net: NetId) -> Option<f64> {
        if net == NetId::GROUND {
            Some(0.0)
        } else {
            self.v.get(&net).copied()
        }
    }
}

/// Parse a `v(n<digits>)` op-point signal name into its net id. Case-insensitive
/// on the leading `v`; returns `None` for any other shape.
fn parse_net_name(name: &str) -> Option<u32> {
    let rest = name
        .strip_prefix("v(n")
        .or_else(|| name.strip_prefix("V(n"))?;
    rest.strip_suffix(')')?.parse().ok()
}

/// Which colour band a net's voltage falls in.
///
/// `Zero` and `Negative` are their own bands (ground and sub-ground rails read at
/// a glance); positive voltages ramp `Low → Mid → High` as a fraction of the
/// largest magnitude present, so the palette scales to whatever circuit it draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bucket {
    /// No voltage provided for this net.
    Unknown,
    /// Within [`ZERO_EPS`] of 0 V (ground level).
    Zero,
    /// Below 0 V.
    Negative,
    /// Positive, in the lower third of the magnitude range.
    Low,
    /// Positive, in the middle third.
    Mid,
    /// Positive, in the upper third (up to and including the peak).
    High,
}

impl Bucket {
    /// Classify a net's voltage against the peak magnitude in the view.
    fn classify(volts: Option<f64>, vmax: f64) -> Self {
        let v = match volts {
            Some(v) => v,
            None => return Bucket::Unknown,
        };
        if v.abs() <= ZERO_EPS {
            return Bucket::Zero;
        }
        if v < 0.0 {
            return Bucket::Negative;
        }
        let frac = if vmax > 0.0 { v / vmax } else { 0.0 };
        if frac <= 1.0 / 3.0 {
            Bucket::Low
        } else if frac <= 2.0 / 3.0 {
            Bucket::Mid
        } else {
            Bucket::High
        }
    }

    /// The ANSI SGR code for this band (house style: `\x1b[..m`). A blue→red ramp
    /// for live nets; dim for ground and unknown.
    fn ansi(self) -> &'static str {
        match self {
            Bucket::Unknown => "\x1b[2m",   // dim
            Bucket::Zero => "\x1b[2m",      // dim (ground)
            Bucket::Negative => "\x1b[34m", // blue
            Bucket::Low => "\x1b[32m",      // green
            Bucket::Mid => "\x1b[33m",      // yellow
            Bucket::High => "\x1b[31m",     // red
        }
    }
}

/// One piece of a rendered row: text, optionally wrapped in a colour band.
struct Seg {
    text: String,
    color: Option<&'static str>,
}

impl Seg {
    fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            color: None,
        }
    }

    fn colored(text: impl Into<String>, color: &'static str) -> Self {
        Self {
            text: text.into(),
            color: Some(color),
        }
    }
}

/// Emit a row of segments into exactly `inner` visible cells.
///
/// Width is counted on visible characters only: a segment's text is clipped by
/// `chars().count()` *before* any escape is added, so an escape can never be split
/// and the colour codes contribute no width. The line is space-padded to fill
/// `inner` when the content is short.
fn emit(segs: &[Seg], inner: usize, color: bool) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for seg in segs {
        if used >= inner {
            break;
        }
        let remaining = inner - used;
        let n = seg.text.chars().count();
        let (txt, took) = if n <= remaining {
            (seg.text.as_str().to_string(), n)
        } else {
            (
                seg.text.chars().take(remaining).collect::<String>(),
                remaining,
            )
        };
        match (color, seg.color) {
            (true, Some(code)) => {
                out.push_str(code);
                out.push_str(&txt);
                out.push_str(RESET);
            }
            _ => out.push_str(&txt),
        }
        used += took;
    }
    if used < inner {
        out.push_str(&" ".repeat(inner - used));
    }
    out
}

/// The schematic drawn as a framed, colour-coded operating-point view.
///
/// Width is in character cells; rows are clipped to fit. [`color`](Overlay::color)
/// toggles the ANSI palette — off for golden snapshots and plain terminals.
pub struct Overlay<'a> {
    /// Heading shown in the frame's top border.
    pub title: &'a str,
    /// Total frame width, in character cells.
    pub width: usize,
    /// Whether to apply the ANSI colour palette.
    pub color: bool,
}

impl<'a> Overlay<'a> {
    /// A sensible default: a 72-cell-wide, coloured frame.
    pub fn new(title: &'a str) -> Self {
        Self {
            title,
            width: 72,
            color: true,
        }
    }

    /// Set the frame width in character cells (clamped so the frame stays valid).
    pub fn width(mut self, width: usize) -> Self {
        self.width = width.max(8);
        self
    }

    /// Enable or disable the ANSI colour palette (disable it for snapshots).
    pub fn color(mut self, color: bool) -> Self {
        self.color = color;
        self
    }

    /// Render the annotated schematic to a framed, multi-line string.
    ///
    /// An empty circuit still renders a valid titled frame with a `(no parts)`
    /// placeholder; a net with no supplied voltage renders `?`. Every line is
    /// exactly `width` *visible* cells wide.
    pub fn render(&self, circuit: &Circuit, volts: &NodeVoltages) -> String {
        let rows = self.content_rows(circuit, volts);
        let height = rows.len().max(1) + 2;
        self.frame(&rows, height).join("\n")
    }

    /// Render as exactly `height` lines for use as a [`Screen`](crate::Screen) pane.
    pub(crate) fn pane_lines(
        &self,
        circuit: &Circuit,
        volts: &NodeVoltages,
        height: usize,
    ) -> Vec<String> {
        let rows = self.content_rows(circuit, volts);
        self.frame(&rows, height)
    }

    /// Wrap the content rows in a titled box of exactly `height` lines.
    fn frame(&self, rows: &[Vec<Seg>], height: usize) -> Vec<String> {
        let w = self.width.max(8);
        let h = height.max(2);
        let inner = w - 4; // '│ ' + text + ' │'
        let content_h = h - 2;
        let placeholder = [Seg::plain("(no parts)")];

        let mut out = Vec::with_capacity(h);
        out.push(top_border(self.title, w));
        for i in 0..content_h {
            let segs: &[Seg] = match rows.get(i) {
                Some(r) => r,
                None if rows.is_empty() && i == 0 => &placeholder,
                None => &[],
            };
            out.push(format!("│ {} │", emit(segs, inner, self.color)));
        }
        out.push(format!("└{}┘", "─".repeat(w - 2)));
        out
    }

    /// Build the per-part segment rows: the aligned `ref / kind / value` columns
    /// (shared with [`Schematic`](crate::Schematic)) followed by a bracketed list
    /// of colour-coded `net=voltage` annotations.
    fn content_rows(&self, circuit: &Circuit, volts: &NodeVoltages) -> Vec<Vec<Seg>> {
        let parts = circuit.parts();
        let vals: Vec<String> = parts.iter().map(|p| fmt_opt_value(&p.value)).collect();

        // Column widths autoscale to the data, measured in chars (not bytes) so
        // multi-byte unit symbols line up.
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

        // Peak magnitude across every touched net — the ramp scales to it.
        let mut vmax = 0.0f64;
        for p in parts {
            for &t in &p.terminals {
                if let Some(v) = volts.get(circuit.resolve(t)) {
                    vmax = vmax.max(v.abs());
                }
            }
        }

        parts
            .iter()
            .zip(&vals)
            .map(|(p, val)| {
                let mut segs = vec![Seg::plain(format!(
                    "{:<rw$}  {:<kw$}  {:<vw$}  [",
                    p.reference,
                    kind_label(&p.kind),
                    val
                ))];
                for (i, &t) in p.terminals.iter().enumerate() {
                    if i > 0 {
                        segs.push(Seg::plain(", "));
                    }
                    let net = circuit.resolve(t);
                    let v = volts.get(net);
                    let token = format!("{}={}", net_name(net), fmt_voltage(v));
                    segs.push(Seg::colored(token, Bucket::classify(v, vmax).ansi()));
                }
                segs.push(Seg::plain("]"));
                segs
            })
            .collect()
    }
}

/// Format a node voltage in the same engineering notation as part values; an
/// unsolved net (`None`) renders as `?`.
fn fmt_voltage(volts: Option<f64>) -> String {
    match volts {
        Some(v) => fmt_value(&Value::new(v, Unit::Volt)),
        None => "?".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ee_core::graph::PartKind;
    use ee_core::signal::Signal;

    /// Count visible cells, skipping any `\x1b[..m` SGR escape sequences.
    fn visible_width(s: &str) -> usize {
        let mut n = 0;
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // Consume up to and including the terminating 'm'.
                for e in chars.by_ref() {
                    if e == 'm' {
                        break;
                    }
                }
            } else {
                n += 1;
            }
        }
        n
    }

    /// A resistor divider: V1 = 5 V at n1, R1 n1→n2, R2 n2→gnd. With equal
    /// resistors the DC op-point is n1 = 5 V, n2 = 2.5 V.
    fn divider() -> (Circuit, NodeVoltages) {
        let mut c = Circuit::new("div");
        let n1 = c.new_net();
        let n2 = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            Some(Value::new(5.0, Unit::Volt)),
            vec![n1, NetId::GROUND],
        );
        c.add_part(
            "R1",
            PartKind::Resistor,
            Some(Value::new(10_000.0, Unit::Ohm)),
            vec![n1, n2],
        );
        c.add_part(
            "R2",
            PartKind::Resistor,
            Some(Value::new(10_000.0, Unit::Ohm)),
            vec![n2, NetId::GROUND],
        );
        let mut v = NodeVoltages::new();
        v.set(c.resolve(n1), 5.0).set(c.resolve(n2), 2.5);
        (c, v)
    }

    #[test]
    fn visible_width_strips_escapes() {
        assert_eq!(visible_width("abc"), 3);
        assert_eq!(visible_width("\x1b[31mab\x1b[0m"), 2);
    }

    // The acceptance gate: every net is annotated with its op-point voltage next
    // to the right part, and ground reads 0 V.
    #[test]
    fn nets_are_annotated_with_their_voltage_and_ground_reads_zero() {
        let (c, v) = divider();
        let out = Overlay::new("op").width(60).color(false).render(&c, &v);
        for needle in [
            "V1", "R1", "R2",      // every part reference
            "n1=5V",   // source node
            "n2=2.5V", // divider midpoint
            "gnd=0V",  // ground reference
        ] {
            assert!(out.contains(needle), "missing {needle:?}:\n{out}");
        }
        // The annotation sits on the correct row: R1 touches n1 and n2.
        let r1 = out.lines().find(|l| l.contains("R1")).unwrap();
        assert!(
            r1.contains("n1=5V") && r1.contains("n2=2.5V"),
            "R1 row must carry both its nets:\n{r1}"
        );
        // R2 bridges the midpoint to ground.
        let r2 = out.lines().find(|l| l.contains("R2")).unwrap();
        assert!(
            r2.contains("n2=2.5V") && r2.contains("gnd=0V"),
            "R2 row must carry n2 and ground:\n{r2}"
        );
    }

    #[test]
    fn op_point_voltages_come_from_signals() {
        // The same divider, but voltages arrive as single-sample DC signals the
        // way an `ee-sim` op-point solve names them.
        let (c, _) = divider();
        let (n1, n2) = (c.resolve(NetId(1)), c.resolve(NetId(2)));
        let sigs = vec![
            Signal::time(format!("v(n{})", n1.0), Unit::Volt, vec![0.0], vec![5.0]),
            Signal::time(format!("v(n{})", n2.0), Unit::Volt, vec![0.0], vec![2.5]),
        ];
        let v = NodeVoltages::from_signals(&sigs);
        let out = Overlay::new("op").width(60).color(false).render(&c, &v);
        assert!(out.contains("n1=5V") && out.contains("n2=2.5V"), "{out}");
        assert!(out.contains("gnd=0V"), "ground is implicit 0 V:\n{out}");
    }

    #[test]
    fn plain_mode_matches_golden_snapshot() {
        let (c, v) = divider();
        let out = Overlay::new("op-point")
            .width(44)
            .color(false)
            .render(&c, &v);
        let expected = "\
┌─ op-point ───────────────────────────────┐
│ V1  V-source  5V    [n1=5V, gnd=0V]      │
│ R1  Resistor  10kΩ  [n1=5V, n2=2.5V]     │
│ R2  Resistor  10kΩ  [n2=2.5V, gnd=0V]    │
└──────────────────────────────────────────┘";
        assert_eq!(out, expected, "golden mismatch:\n{out}");
    }

    #[test]
    fn colored_mode_bands_high_and_low_nets() {
        // n1 = 5 V is the peak (High → red); n2 = 1 V is a fifth of it (Low → green).
        let mut c = Circuit::new("ramp");
        let n1 = c.new_net();
        let n2 = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            Some(Value::new(5.0, Unit::Volt)),
            vec![n1, NetId::GROUND],
        );
        c.add_part("R1", PartKind::Resistor, None, vec![n1, n2]);
        let mut v = NodeVoltages::new();
        v.set(c.resolve(n1), 5.0).set(c.resolve(n2), 1.0);

        let out = Overlay::new("op").width(60).color(true).render(&c, &v);
        assert!(out.contains("\x1b[31m"), "high net should be red:\n{out}");
        assert!(out.contains("\x1b[32m"), "low net should be green:\n{out}");
        assert!(out.contains(RESET), "every token must be reset:\n{out}");
    }

    #[test]
    fn no_line_exceeds_the_configured_width_in_either_mode() {
        let (c, v) = divider();
        let width = 40;
        let plain = Overlay::new("op").width(width).color(false).render(&c, &v);
        for line in plain.lines() {
            assert_eq!(
                line.chars().count(),
                width,
                "plain line is not exactly {width} cells: {line:?}"
            );
        }
        let colored = Overlay::new("op").width(width).color(true).render(&c, &v);
        for line in colored.lines() {
            assert_eq!(
                visible_width(line),
                width,
                "coloured line's visible width is not {width}: {line:?}"
            );
        }
    }

    #[test]
    fn unsolved_net_renders_question_mark_without_panic() {
        // Only n1 is supplied; n2 is left unsolved.
        let mut c = Circuit::new("part");
        let n1 = c.new_net();
        let n2 = c.new_net();
        c.add_part("R1", PartKind::Resistor, None, vec![n1, n2]);
        let mut v = NodeVoltages::new();
        v.set(c.resolve(n1), 3.3);

        let out = Overlay::new("op").width(48).color(false).render(&c, &v);
        assert!(out.contains("n1=3.3V"), "solved net:\n{out}");
        assert!(out.contains("n2=?"), "unsolved net shows '?':\n{out}");
    }

    #[test]
    fn empty_circuit_renders_a_valid_titled_frame() {
        let out = Overlay::new("empty")
            .width(24)
            .color(false)
            .render(&Circuit::new("empty"), &NodeVoltages::new());
        assert!(out.contains("empty"), "title in frame:\n{out}");
        assert!(out.contains("(no parts)"), "placeholder row:\n{out}");
        assert!(out.contains('┌') && out.contains('┘'), "framed:\n{out}");
        for line in out.lines() {
            assert_eq!(line.chars().count(), 24);
        }
    }

    #[test]
    fn signal_name_parsing_is_strict() {
        assert_eq!(parse_net_name("v(n3)"), Some(3));
        assert_eq!(parse_net_name("V(n0)"), Some(0));
        assert_eq!(parse_net_name("i(R1)"), None);
        assert_eq!(parse_net_name("v(out)"), None);
        assert_eq!(parse_net_name("v(n)"), None);
    }
}
