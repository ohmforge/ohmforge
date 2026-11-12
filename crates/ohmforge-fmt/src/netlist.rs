//! A small, friendly netlist text format (`CP-SIM-M0.2`) — the "type a circuit"
//! front door.
//!
//! One part per line, whitespace-separated:
//!
//! ```text
//! # an RC low-pass (comments start with # or //)
//! .title rc-lowpass        // optional directive
//! V1 in  gnd 5             // 5 V source from `in` to ground
//! R1 in  out 1k            // 1 kΩ
//! C1 out gnd 1u            // 1 µF
//! ```
//!
//! Fields are `REF  NODE_A  NODE_B  VALUE`. The part type comes from the first
//! letter of the reference (`R`/`C`/`L`/`V`), which also fixes the value's unit.
//! Node names are arbitrary; reusing a name is how you wire parts together, and
//! `gnd`, `ground`, and `0` all mean the ground reference. Values accept the
//! engineering notation `ohmforge-core` already understands (`4k7`, `100n`, `1meg`).
//!
//! This is deliberately not full SPICE — it is the minimum that feeds the M0
//! engine. SPICE/KiCad import land at M1/M2 (docs/ROADMAP.md).

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;

use ohmforge_core::graph::{Circuit, NetId, PartKind};
use ohmforge_core::units::Unit;
use ohmforge_core::value::{ParseError, Value};

/// A parse failure, tagged with the 1-based source line it occurred on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetlistError {
    pub line: usize,
    pub kind: ErrorKind,
}

/// What went wrong on a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    /// A part line did not have exactly `REF A B VALUE`.
    WrongFieldCount { got: usize },
    /// The reference designator was empty.
    EmptyReference,
    /// The reference's first letter is not a known part type.
    UnknownPrefix(char),
    /// The value field could not be parsed.
    BadValue(ParseError),
    /// Two parts share a reference designator.
    DuplicateReference(String),
    /// A `.directive` that the format does not recognize.
    UnknownDirective(String),
}

impl fmt::Display for NetlistError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.kind)
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorKind::WrongFieldCount { got } => {
                write!(f, "expected 4 fields (REF A B VALUE), found {got}")
            }
            ErrorKind::EmptyReference => write!(f, "empty part reference"),
            ErrorKind::UnknownPrefix(c) => {
                write!(f, "unknown part type '{c}' (expected R, C, L, or V)")
            }
            ErrorKind::BadValue(e) => {
                let why = match e {
                    ParseError::Empty => "value is empty",
                    ParseError::NotANumber => "not a valid number",
                };
                write!(f, "bad value: {why}")
            }
            ErrorKind::DuplicateReference(r) => write!(f, "duplicate part reference {r:?}"),
            ErrorKind::UnknownDirective(d) => write!(f, "unknown directive .{d}"),
        }
    }
}

impl Error for NetlistError {}

/// Parse netlist source into a [`Circuit`] ready for the engine.
pub fn parse(src: &str) -> Result<Circuit, NetlistError> {
    let mut circuit = Circuit::new("netlist");
    let mut nets: HashMap<String, NetId> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();

    for (i, raw) in src.lines().enumerate() {
        let line = i + 1;
        let text = strip_comment(raw).trim();
        if text.is_empty() {
            continue;
        }

        if let Some(rest) = text.strip_prefix('.') {
            apply_directive(&mut circuit, rest, line)?;
            continue;
        }

        let fields: Vec<&str> = text.split_whitespace().collect();
        if fields.len() != 4 {
            return Err(err(line, ErrorKind::WrongFieldCount { got: fields.len() }));
        }
        let reference = fields[0];
        let (kind, unit) = classify(reference).map_err(|k| err(line, k))?;
        if !seen.insert(reference.to_string()) {
            return Err(err(
                line,
                ErrorKind::DuplicateReference(reference.to_string()),
            ));
        }
        let a = net_for(&mut circuit, &mut nets, fields[1]);
        let b = net_for(&mut circuit, &mut nets, fields[2]);
        let value = Value::parse(fields[3], unit).map_err(|e| err(line, ErrorKind::BadValue(e)))?;
        circuit.add_part(reference, kind, Some(value), vec![a, b]);
    }

    Ok(circuit)
}

fn err(line: usize, kind: ErrorKind) -> NetlistError {
    NetlistError { line, kind }
}

/// Everything before the first `#` or `//` comment marker.
fn strip_comment(line: &str) -> &str {
    let hash = line.find('#').unwrap_or(line.len());
    let slashes = line.find("//").unwrap_or(line.len());
    &line[..hash.min(slashes)]
}

/// Map a reference's leading letter to a part kind and its value's unit.
fn classify(reference: &str) -> Result<(PartKind, Unit), ErrorKind> {
    let c = reference.chars().next().ok_or(ErrorKind::EmptyReference)?;
    Ok(match c.to_ascii_uppercase() {
        'R' => (PartKind::Resistor, Unit::Ohm),
        'C' => (PartKind::Capacitor, Unit::Farad),
        'L' => (PartKind::Inductor, Unit::Henry),
        'V' => (PartKind::VoltageSource, Unit::Volt),
        other => return Err(ErrorKind::UnknownPrefix(other)),
    })
}

/// The net for a node name, allocating a fresh one the first time a name is seen.
/// `gnd`/`ground`/`0` always resolve to [`NetId::GROUND`].
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
    matches!(name.to_ascii_lowercase().as_str(), "gnd" | "ground" | "0")
}

/// Handle a `.directive` line.
fn apply_directive(circuit: &mut Circuit, rest: &str, line: usize) -> Result<(), NetlistError> {
    let mut it = rest.splitn(2, char::is_whitespace);
    let name = it.next().unwrap_or("");
    let arg = it.next().unwrap_or("").trim();
    match name.to_ascii_lowercase().as_str() {
        "title" => {
            circuit.name = arg.to_string();
            Ok(())
        }
        other => Err(err(line, ErrorKind::UnknownDirective(other.to_string()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RC: &str = "\
# an RC low-pass
.title rc-lowpass
V1 in  gnd 5
R1 in  out 1k   // the series resistor
C1 out gnd 1u
";

    #[test]
    fn parses_parts_kinds_and_values() {
        let c = parse(RC).unwrap();
        assert_eq!(c.name, "rc-lowpass");
        assert_eq!(c.parts().len(), 3);

        let r1 = c.part_by_ref("R1").unwrap();
        assert!(matches!(r1.kind, PartKind::Resistor));
        assert_eq!(r1.value.as_ref().unwrap().magnitude, 1000.0);

        let c1 = c.part_by_ref("C1").unwrap();
        assert!(matches!(c1.kind, PartKind::Capacitor));
        assert_eq!(c1.value.as_ref().unwrap().magnitude, 1e-6);
    }

    #[test]
    fn shared_node_names_wire_parts_together() {
        let c = parse(RC).unwrap();
        let v1 = c.part_by_ref("V1").unwrap();
        let r1 = c.part_by_ref("R1").unwrap();
        let c1 = c.part_by_ref("C1").unwrap();
        // `in` is shared by V1 and R1; `out` by R1 and C1.
        assert_eq!(v1.terminals[0], r1.terminals[0], "node `in` is one net");
        assert_eq!(r1.terminals[1], c1.terminals[0], "node `out` is one net");
        // ...and the three ground references all collapse to NetId::GROUND.
        assert_eq!(v1.terminals[1], NetId::GROUND);
        assert_eq!(c1.terminals[1], NetId::GROUND);
    }

    #[test]
    fn ground_aliases_are_all_ground() {
        let c = parse("R1 a gnd 1k\nR2 a 0 1k\nR3 a ground 1k").unwrap();
        for r in ["R1", "R2", "R3"] {
            assert_eq!(c.part_by_ref(r).unwrap().terminals[1], NetId::GROUND);
        }
    }

    #[test]
    fn errors_point_at_the_offending_line() {
        let unknown = parse("R1 a b 1k\nX9 a b 2k").unwrap_err();
        assert_eq!(unknown, err(2, ErrorKind::UnknownPrefix('X')));

        let bad_value = parse("R1 a b 1k\n\nC1 a b nope").unwrap_err();
        assert_eq!(bad_value.line, 3);
        assert!(matches!(bad_value.kind, ErrorKind::BadValue(_)));

        let few = parse("R1 a b").unwrap_err();
        assert_eq!(few, err(1, ErrorKind::WrongFieldCount { got: 3 }));

        let dup = parse("R1 a b 1k\nR1 b c 2k").unwrap_err();
        assert_eq!(dup, err(2, ErrorKind::DuplicateReference("R1".into())));

        let dir = parse(".nope foo").unwrap_err();
        assert_eq!(dir, err(1, ErrorKind::UnknownDirective("nope".into())));
    }

    #[test]
    fn parsed_rc_actually_simulates() {
        // The real proof: parse → simulate → the output charges toward the rail.
        use ohmforge_sim::{Analysis, Engine};
        let c = parse(RC).unwrap();
        let out = c.part_by_ref("R1").unwrap().terminals[1]; // the `out` net
        let sigs = Engine::new()
            .run(
                &c,
                &Analysis::Transient {
                    duration: 5e-3,
                    step: Some(5e-6),
                },
            )
            .unwrap();
        let name = format!("v(n{})", c.resolve(out).0);
        let sig = sigs
            .iter()
            .find(|s| s.name == name)
            .expect("v(out) present");
        let y = sig.real().unwrap();
        assert!(y[0].abs() < 1e-9, "starts uncharged");
        assert!(*y.last().unwrap() > 4.9, "ends near the 5 V rail");
    }
}
