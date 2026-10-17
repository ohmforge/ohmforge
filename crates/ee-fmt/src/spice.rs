//! SPICE netlist import/export (`CP-AF-M1.1`) — the industry-standard "deck"
//! front door, a sibling of [`crate::netlist`] and [`crate::project`].
//!
//! ```text
//! * a resistive divider          <- line 1 is always the title (per tradition)
//! V1 in 0 DC 10                   ; 10 V source from `in` to ground (node 0)
//! R1 in  mid 1k                   ; 1 kΩ
//! R2 mid 0   1k
//! .op                             <- dot-directives are parsed and ignored (M1)
//! .end
//! ```
//!
//! ## Subset supported at M1
//!
//! * **Elements**, keyed by the reference's first letter: `R` (ohms), `C`
//!   (farads), `L` (henries), `V` (volt source), `I` (current source). Each is a
//!   two-terminal device written `Ref node1 node2 value`; sources also accept an
//!   optional `DC`/`AC` keyword before the value (`Vxxx n+ n- [DC] value`).
//! * **Ground** is node `0` (the SPICE convention); `gnd`/`ground` are also
//!   accepted. Every other node name allocates a net, and reusing a name wires
//!   parts together (as in the friendly netlist).
//! * **Comments**: a line beginning with `*`, or anything after an inline `;`.
//! * **Title**: the first physical line, kept verbatim as the circuit name.
//! * **Dot-directives** (`.end`, `.ends`, `.tran`, `.op`, `.model`, …) are
//!   recognized and skipped — they never cause an error at M1, but nothing in
//!   them is modeled yet.
//!
//! ## Value suffixes (the `m`-vs-`meg` footgun)
//!
//! SPICE scale factors are **case-insensitive**, which is exactly where they bite:
//! `M` means *milli*, not mega — mega is spelled `MEG`. ee-core's own parser uses
//! the opposite convention (`M` = mega), so this module parses SPICE magnitudes
//! itself rather than deferring to [`Value::parse`]. The recognized factors, any
//! trailing letters after them ignored (`10kohm` == `10k`):
//!
//! | suffix        | factor  | | suffix | factor |
//! |---------------|---------|-|--------|--------|
//! | `t`           | 1e12    | | `m`    | 1e-3   |
//! | `g`           | 1e9     | | `u`/`µ`| 1e-6   |
//! | `meg`         | 1e6     | | `n`    | 1e-9   |
//! | `k`           | 1e3     | | `p`    | 1e-12  |
//! | `mil`         | 25.4e-6 | | `f`    | 1e-15  |
//!
//! `meg` and `mil` are matched before the single-letter `m` so neither is read as
//! milli. Plain scientific notation (`1e-6`) is accepted too.
//!
//! Export writes a valid deck: a title line, one element line per part (ground as
//! `0`, every other net as `n<id>` via [`Circuit::resolve`]), and a trailing
//! `.end`. Zero dependencies beyond `ee-core`; the parser is hand-written like its
//! siblings.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt::{self, Write as _};

use ee_core::graph::{Circuit, NetId, PartKind};
use ee_core::units::Unit;
use ee_core::value::Value;

/// An import failure, tagged with the 1-based source line it occurred on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpiceError {
    pub line: usize,
    pub kind: ErrorKind,
}

/// What went wrong on a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    /// An element line had fewer than `Ref node1 node2 value` (4 fields).
    TooFewFields { got: usize },
    /// The reference designator was empty.
    EmptyReference,
    /// The reference's first letter is not a known element type.
    UnknownElement(char),
    /// A source line had no numeric value after its nodes (and `DC`/`AC` keyword).
    MissingValue,
    /// The value field could not be parsed as a SPICE magnitude.
    BadValue(String),
    /// Two parts share a reference designator.
    DuplicateReference(String),
}

impl fmt::Display for SpiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.kind)
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorKind::TooFewFields { got } => {
                write!(
                    f,
                    "expected at least 4 fields (Ref N1 N2 VALUE), found {got}"
                )
            }
            ErrorKind::EmptyReference => write!(f, "empty element reference"),
            ErrorKind::UnknownElement(c) => {
                write!(f, "unknown element type '{c}' (expected R, C, L, V, or I)")
            }
            ErrorKind::MissingValue => write!(f, "missing value after element nodes"),
            ErrorKind::BadValue(tok) => write!(f, "bad value {tok:?}"),
            ErrorKind::DuplicateReference(r) => write!(f, "duplicate element reference {r:?}"),
        }
    }
}

impl Error for SpiceError {}

/// Import a SPICE deck into a [`Circuit`] ready for the engine. See the module
/// docs for the exact subset understood at M1.
pub fn import(src: &str) -> Result<Circuit, SpiceError> {
    let mut circuit = Circuit::new("");
    let mut nets: HashMap<String, NetId> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();

    for (i, raw) in src.lines().enumerate() {
        let line = i + 1;

        // SPICE tradition: the very first physical line is the title, kept
        // verbatim (comment markers and all) as the circuit's name.
        if i == 0 {
            circuit.name = raw.trim().to_string();
            continue;
        }

        // `*` opens a full-line comment; `;` opens an inline one.
        if raw.trim_start().starts_with('*') {
            continue;
        }
        let text = strip_comment(raw).trim();
        if text.is_empty() {
            continue;
        }

        // Dot-directives (`.end`, `.tran`, `.model`, …) are noted and skipped.
        if text.starts_with('.') {
            continue;
        }

        parse_element(&mut circuit, &mut nets, &mut seen, text, line)?;
    }

    Ok(circuit)
}

fn err(line: usize, kind: ErrorKind) -> SpiceError {
    SpiceError { line, kind }
}

/// Everything before the first inline `;` comment marker.
fn strip_comment(line: &str) -> &str {
    match line.find(';') {
        Some(i) => &line[..i],
        None => line,
    }
}

/// Parse one `Ref N1 N2 [DC|AC] value` element line and add it to the circuit.
fn parse_element(
    circuit: &mut Circuit,
    nets: &mut HashMap<String, NetId>,
    seen: &mut HashSet<String>,
    text: &str,
    line: usize,
) -> Result<(), SpiceError> {
    let fields: Vec<&str> = text.split_whitespace().collect();
    if fields.len() < 4 {
        return Err(err(line, ErrorKind::TooFewFields { got: fields.len() }));
    }
    let reference = fields[0];
    let (kind, unit) = classify(reference).map_err(|k| err(line, k))?;
    if !seen.insert(reference.to_string()) {
        return Err(err(
            line,
            ErrorKind::DuplicateReference(reference.to_string()),
        ));
    }

    let a = net_for(circuit, nets, fields[1]);
    let b = net_for(circuit, nets, fields[2]);

    // Everything after the two nodes is the value, past an optional `DC`/`AC`
    // keyword that sources may carry (`V1 n+ n- DC 10`).
    let mut rest = &fields[3..];
    if let Some(first) = rest.first() {
        if first.eq_ignore_ascii_case("DC") || first.eq_ignore_ascii_case("AC") {
            rest = &rest[1..];
        }
    }
    let value_tok = rest
        .first()
        .ok_or_else(|| err(line, ErrorKind::MissingValue))?;
    let magnitude = spice_magnitude(value_tok)
        .ok_or_else(|| err(line, ErrorKind::BadValue(value_tok.to_string())))?;

    circuit.add_part(
        reference,
        kind,
        Some(Value::new(magnitude, unit)),
        vec![a, b],
    );
    Ok(())
}

/// Map a reference's leading letter to an element kind and its value's unit.
fn classify(reference: &str) -> Result<(PartKind, Unit), ErrorKind> {
    let c = reference.chars().next().ok_or(ErrorKind::EmptyReference)?;
    Ok(match c.to_ascii_uppercase() {
        'R' => (PartKind::Resistor, Unit::Ohm),
        'C' => (PartKind::Capacitor, Unit::Farad),
        'L' => (PartKind::Inductor, Unit::Henry),
        'V' => (PartKind::VoltageSource, Unit::Volt),
        'I' => (PartKind::CurrentSource, Unit::Ampere),
        other => return Err(ErrorKind::UnknownElement(other)),
    })
}

/// The net for a node name, allocating a fresh one the first time it is seen.
/// `0`/`gnd`/`ground` always resolve to [`NetId::GROUND`].
fn net_for(circuit: &mut Circuit, nets: &mut HashMap<String, NetId>, name: &str) -> NetId {
    if is_ground(name) {
        return NetId::GROUND;
    }
    if let Some(&id) = nets.get(name) {
        return id;
    }
    let id = circuit.new_net();
    nets.insert(name.to_string(), id);
    id
}

fn is_ground(name: &str) -> bool {
    matches!(name.to_ascii_lowercase().as_str(), "0" | "gnd" | "ground")
}

/// Parse a SPICE magnitude: a number (optionally with an exponent) followed by an
/// optional case-insensitive scale factor, with any trailing unit text ignored
/// (`10kohm` == `10k`). Returns the base-SI magnitude, or `None` if not numeric.
fn spice_magnitude(tok: &str) -> Option<f64> {
    let (number, suffix) = split_number(tok);
    let mantissa: f64 = number.parse().ok()?;
    Some(mantissa * scale_factor(suffix))
}

/// Split a token into its leading numeric text (sign, digits, decimal point, and
/// an optional `e`/`E` exponent) and the trailing suffix (the scale factor plus
/// any ignored unit letters).
fn split_number(s: &str) -> (&str, &str) {
    let bytes = s.as_bytes();
    let mut i = 0;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
        i += 1;
    }
    // An exponent is only part of the number if it is actually `e[sign]digits`;
    // otherwise the `e` belongs to a trailing suffix.
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        let mut j = i + 1;
        if j < bytes.len() && (bytes[j] == b'+' || bytes[j] == b'-') {
            j += 1;
        }
        if j < bytes.len() && bytes[j].is_ascii_digit() {
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            i = j;
        }
    }
    (&s[..i], &s[i..])
}

/// The SPICE scale factor named by a suffix (1.0 when it names none). `meg` and
/// `mil` are checked before the single-letter `m` (milli) so neither is misread.
fn scale_factor(suffix: &str) -> f64 {
    let low = suffix.to_ascii_lowercase();
    if low.starts_with("meg") {
        return 1e6;
    }
    if low.starts_with("mil") {
        return 25.4e-6;
    }
    match suffix.chars().next() {
        Some('t') | Some('T') => 1e12,
        Some('g') | Some('G') => 1e9,
        Some('k') | Some('K') => 1e3,
        Some('m') | Some('M') => 1e-3,
        Some('u') | Some('U') | Some('µ') => 1e-6,
        Some('n') | Some('N') => 1e-9,
        Some('p') | Some('P') => 1e-12,
        Some('f') | Some('F') => 1e-15,
        _ => 1.0,
    }
}

/// Export a [`Circuit`] as a SPICE deck. Round-trips through [`import`]: each
/// part's reference/kind/value and the shared-net topology survive (see the
/// module docs). Parts whose kind is outside the M1 subset are emitted as a
/// comment rather than a bogus element.
pub fn export(circuit: &Circuit) -> String {
    let mut out = String::new();
    // Writing to a String is infallible, so these `write!`s cannot fail.
    // The title line is mandatory in SPICE; fall back to a stable default so the
    // deck stays valid even for an unnamed circuit.
    let title = if circuit.name.is_empty() {
        "ee-assist circuit"
    } else {
        circuit.name.as_str()
    };
    let _ = writeln!(out, "{title}");

    for part in circuit.parts() {
        if !is_exportable(&part.kind) {
            let _ = writeln!(
                out,
                "* {} skipped (unsupported in SPICE M1)",
                part.reference
            );
            continue;
        }
        let _ = write!(out, "{}", part.reference);
        for &terminal in &part.terminals {
            let _ = write!(out, " {}", node_name(circuit.resolve(terminal)));
        }
        let _ = writeln!(out, " {}", export_value(part.value.as_ref()));
    }

    let _ = writeln!(out, ".end");
    out
}

/// Whether a part kind maps onto the R/C/L/V/I SPICE subset. Diodes and
/// transistors need a `.model` card (a later checkpoint), so they're skipped
/// here rather than emitted as an unparseable line.
fn is_exportable(kind: &PartKind) -> bool {
    !matches!(
        kind,
        PartKind::Other(_) | PartKind::Diode | PartKind::Bjt(..) | PartKind::Mosfet(..)
    )
}

/// The node name for a resolved net: `0` for ground, else `n<id>`.
fn node_name(net: NetId) -> String {
    if net == NetId::GROUND {
        "0".to_string()
    } else {
        format!("n{}", net.0)
    }
}

/// The value field for an element: the base-SI magnitude via `f64`'s default
/// `Display` (the shortest string that parses back to the same bits), so a plain
/// re-[`import`] recovers it exactly. `None` is written as a neutral `0`.
fn export_value(value: Option<&Value>) -> String {
    match value {
        Some(v) => format!("{}", v.magnitude),
        None => "0".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIVIDER: &str = "\
divider demo
* a 10 V source across two equal resistors
V1 in 0 DC 10
R1 in  mid 1k    ; the top leg
R2 mid 0   1k
.op
.end
";

    #[test]
    fn imports_kinds_values_and_title() {
        let c = import(DIVIDER).unwrap();
        assert_eq!(c.name, "divider demo");
        assert_eq!(c.parts().len(), 3);

        let v1 = c.part_by_ref("V1").unwrap();
        assert!(matches!(v1.kind, PartKind::VoltageSource));
        assert_eq!(v1.value.as_ref().unwrap().magnitude, 10.0);

        let r1 = c.part_by_ref("R1").unwrap();
        assert!(matches!(r1.kind, PartKind::Resistor));
        assert_eq!(r1.value.as_ref().unwrap().magnitude, 1000.0);
    }

    #[test]
    fn shared_node_names_wire_parts_together() {
        let c = import(DIVIDER).unwrap();
        let v1 = c.part_by_ref("V1").unwrap();
        let r1 = c.part_by_ref("R1").unwrap();
        let r2 = c.part_by_ref("R2").unwrap();
        // `in` is shared by V1 and R1; `mid` by R1 and R2.
        assert_eq!(v1.terminals[0], r1.terminals[0], "node `in` is one net");
        assert_eq!(r1.terminals[1], r2.terminals[0], "node `mid` is one net");
        // Node `0` collapses to ground everywhere.
        assert_eq!(v1.terminals[1], NetId::GROUND);
        assert_eq!(r2.terminals[1], NetId::GROUND);
    }

    #[test]
    fn ground_aliases_are_all_ground() {
        let c = import("title\nR1 a 0 1k\nR2 a gnd 1k\nR3 a ground 1k\n.end").unwrap();
        for r in ["R1", "R2", "R3"] {
            assert_eq!(c.part_by_ref(r).unwrap().terminals[1], NetId::GROUND);
        }
    }

    #[test]
    fn imported_divider_simulates_to_the_midpoint() {
        // The gate's "sim matches" half: import → solve → the midpoint sits at
        // half the rail (two equal resistors, 10 V → 5 V).
        use ee_sim::{Analysis, Engine};
        let c = import(DIVIDER).unwrap();
        let mid = c.part_by_ref("R1").unwrap().terminals[1]; // the `mid` net
        let sigs = Engine::new().run(&c, &Analysis::OperatingPoint).unwrap();
        let name = format!("v(n{})", c.resolve(mid).0);
        let sig = sigs
            .iter()
            .find(|s| s.name == name)
            .expect("v(mid) present");
        let v = sig.real().unwrap();
        assert!((v[0] - 5.0).abs() < 1e-9, "midpoint = {}", v[0]);
    }

    #[test]
    fn suffix_magnitudes_including_m_versus_meg() {
        let cases = [
            ("1k", 1e3),
            ("1meg", 1e6),
            ("1MEG", 1e6),
            ("1m", 1e-3), // milli — NOT mega
            ("1M", 1e-3), // case-insensitive: still milli in SPICE
            ("1u", 1e-6),
            ("1µ", 1e-6),
            ("1n", 1e-9),
            ("1p", 1e-12),
            ("1f", 1e-15),
            ("1g", 1e9),
            ("1t", 1e12),
            ("4.7k", 4700.0),
            ("1mil", 25.4e-6),
            ("10kohm", 1e4),    // trailing unit text ignored
            ("2.2e-9", 2.2e-9), // plain scientific notation
            ("-5", -5.0),
        ];
        for (tok, want) in cases {
            let got = spice_magnitude(tok).unwrap_or_else(|| panic!("{tok} did not parse"));
            assert!(
                (got - want).abs() <= want.abs() * 1e-9 + 1e-18,
                "{tok}: got {got}, want {want}"
            );
        }
        // Non-numeric tokens (e.g. a stray keyword) are rejected.
        assert!(spice_magnitude("DC").is_none());
        assert!(spice_magnitude("").is_none());
    }

    /// Build a circuit exercising every exportable kind plus a shared node and a
    /// `wire()`-merged net, to prove topology survives the round-trip.
    fn sample() -> Circuit {
        let mut c = Circuit::new("roundtrip rc");
        let n_in = c.new_net();
        let n_mid = c.new_net();
        let n_out = c.new_net();
        let n_alias = c.new_net();
        c.wire(n_out, n_alias); // one electrical node spelled two ways

        c.add_part(
            "V1",
            PartKind::VoltageSource,
            Some(Value::new(5.0, Unit::Volt)),
            vec![n_in, NetId::GROUND],
        );
        c.add_part(
            "R1",
            PartKind::Resistor,
            Some(Value::new(2200.0, Unit::Ohm)),
            vec![n_in, n_mid],
        );
        c.add_part(
            "C1",
            PartKind::Capacitor,
            Some(Value::new(2.2e-9, Unit::Farad)),
            vec![n_mid, NetId::GROUND],
        );
        c.add_part(
            "L1",
            PartKind::Inductor,
            Some(Value::new(1e-4, Unit::Henry)),
            vec![n_mid, n_alias],
        );
        c.add_part(
            "I1",
            PartKind::CurrentSource,
            Some(Value::new(1e-3, Unit::Ampere)),
            vec![n_out, NetId::GROUND],
        );
        c
    }

    #[test]
    fn round_trip_preserves_parts_and_topology() {
        let orig = sample();
        let deck = export(&orig);
        let back = import(&deck).unwrap();

        assert_eq!(back.name, "roundtrip rc");
        assert_eq!(back.parts().len(), orig.parts().len());

        // Reference, kind, and value survive exactly (f64 equality).
        for op in orig.parts() {
            let bp = back.part_by_ref(&op.reference).unwrap();
            assert_eq!(kind_tag(&bp.kind), kind_tag(&op.kind), "{}", op.reference);
            assert_eq!(bp.value, op.value, "value of {}", op.reference);
        }

        // Connectivity: parts that shared a net still resolve to one net.
        let v1 = back.part_by_ref("V1").unwrap();
        let r1 = back.part_by_ref("R1").unwrap();
        let c1 = back.part_by_ref("C1").unwrap();
        let l1 = back.part_by_ref("L1").unwrap();
        let i1 = back.part_by_ref("I1").unwrap();
        // `n_in` shared by V1 and R1.
        assert_eq!(back.resolve(v1.terminals[0]), back.resolve(r1.terminals[0]));
        // `n_mid` shared by R1, C1, L1.
        assert_eq!(back.resolve(r1.terminals[1]), back.resolve(c1.terminals[0]));
        assert_eq!(back.resolve(r1.terminals[1]), back.resolve(l1.terminals[0]));
        // The wire()-merged pair (L1's alias and I1's node) is one net.
        assert_eq!(back.resolve(l1.terminals[1]), back.resolve(i1.terminals[0]));
        // Ground stayed ground.
        assert_eq!(back.resolve(v1.terminals[1]), NetId::GROUND);
        assert_eq!(back.resolve(c1.terminals[1]), NetId::GROUND);

        // Idempotent: re-exporting the reloaded circuit is byte-identical.
        assert_eq!(export(&back), deck);
    }

    #[test]
    fn export_output_is_stable_and_readable() {
        let deck = export(&sample());
        let lines: Vec<&str> = deck.lines().collect();

        // Title first, `.end` last.
        assert_eq!(lines[0], "roundtrip rc");
        assert_eq!(*lines.last().unwrap(), ".end");

        // Specific element lines are present and exact (ground as `0`).
        assert!(deck.contains("\nV1 n1 0 5\n"), "{deck}");
        assert!(deck.contains("\nR1 n1 n2 2200\n"), "{deck}");
        assert!(deck.contains("\nC1 n2 0 0.0000000022\n"), "{deck}");
    }

    #[test]
    fn unsupported_kinds_export_as_a_comment() {
        let mut c = Circuit::new("has-other");
        let n = c.new_net();
        c.add_part(
            "D1",
            PartKind::Other("led".into()),
            None,
            vec![n, NetId::GROUND],
        );
        let deck = export(&c);
        assert!(
            deck.contains("* D1 skipped (unsupported in SPICE M1)"),
            "{deck}"
        );
        // And re-importing the deck simply drops it (comment line).
        assert_eq!(import(&deck).unwrap().parts().len(), 0);
    }

    #[test]
    fn errors_point_at_the_offending_line() {
        // Line 1 is the title; the bad element is on line 2.
        let few = import("title\nR1 a b").unwrap_err();
        assert_eq!(few, err(2, ErrorKind::TooFewFields { got: 3 }));

        let unknown = import("title\nR1 a b 1k\nX9 a b 2k").unwrap_err();
        assert_eq!(unknown, err(3, ErrorKind::UnknownElement('X')));

        let bad_value = import("title\n\nC1 a b nope").unwrap_err();
        assert_eq!(bad_value.line, 3);
        assert_eq!(bad_value.kind, ErrorKind::BadValue("nope".to_string()));

        let missing = import("title\nV1 a b DC").unwrap_err();
        assert_eq!(missing, err(2, ErrorKind::MissingValue));

        let dup = import("title\nR1 a b 1k\nR1 b c 2k").unwrap_err();
        assert_eq!(dup, err(3, ErrorKind::DuplicateReference("R1".into())));
    }

    #[test]
    fn display_includes_line_number() {
        let e = err(7, ErrorKind::MissingValue);
        assert!(e.to_string().starts_with("line 7:"), "{e}");
    }

    /// A kind discriminant string for equality checks in tests.
    fn kind_tag(kind: &PartKind) -> String {
        match kind {
            PartKind::Resistor => "R".into(),
            PartKind::Capacitor => "C".into(),
            PartKind::Inductor => "L".into(),
            PartKind::VoltageSource => "V".into(),
            PartKind::CurrentSource => "I".into(),
            PartKind::Diode => "D".into(),
            PartKind::Bjt(..) => "Q".into(),
            PartKind::Mosfet(..) => "M".into(),
            PartKind::Other(n) => format!("other:{n}"),
        }
    }
}
