//! Bill-of-materials export (`CP-AF-M3.1`) — turn a [`Circuit`] into the CSV a
//! purchaser or assembler reads: one line per distinct part *type*, grouped by
//! kind and value, with the reference designators that share it and a quantity.
//!
//! A sibling of [`crate::waveform`]: same house rules — a hand-rolled,
//! zero-dependency (beyond `ohmforge-core`) CSV writer with RFC-4180 quoting and
//! deterministic, byte-stable output.
//!
//! ```text
//! kind,value,quantity,references
//! Resistor,1kΩ,2,R1 R2
//! Resistor,2.2kΩ,1,R3
//! Capacitor,0.0000001F,1,C1
//! Voltage source,5V,1,V1
//! ```
//!
//! ## Grouping
//!
//! Two parts share a BOM line when they have the **same kind and the same
//! value** (magnitude *and* unit, compared on the shortest round-trippable
//! decimal — so `1000Ω` and `1000Ω` group but `1000Ω` and `1001Ω` do not). A
//! part with no value groups with the other valueless parts of its kind (shown
//! as `-`). [`PartKind::Other`] keeps its name, so an `other(led)` and an
//! `other(buzzer)` stay on separate lines.
//!
//! ## Ordering
//!
//! Lines sort by kind (in a fixed, schematic-friendly order: sources, then
//! R/L/C, then semiconductors, …), then by value magnitude, then by the group's
//! first reference. Reference designators inside a line sort *naturally*
//! (`R1 R2 R10`, not `R1 R10 R2`). The output is therefore identical for
//! identical input, every run.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use ohmforge_core::graph::{Circuit, PartKind};
use ohmforge_core::value::Value;

/// Export `circuit` as a bill-of-materials CSV. See the module docs for the
/// grouping and ordering rules. The output always carries the header row, so an
/// empty circuit yields a header and nothing else.
pub fn to_csv(circuit: &Circuit) -> String {
    let mut groups: BTreeMap<GroupKey, Vec<String>> = BTreeMap::new();
    for part in circuit.parts() {
        let key = GroupKey::new(&part.kind, part.value);
        groups.entry(key).or_default().push(part.reference.clone());
    }

    let mut out = String::from("kind,value,quantity,references\n");
    for (key, mut refs) in groups {
        refs.sort_by_key(|r| ref_key(r));
        let quantity = refs.len();
        let references = refs.join(" ");
        let _ = writeln!(
            out,
            "{},{},{quantity},{}",
            csv_field(&key.kind_label()),
            csv_field(&key.value_label()),
            csv_field(&references),
        );
    }
    out
}

/// A line's grouping identity: the part kind and its value. Ordering is defined
/// by the derived `Ord` over the fields below, which is exactly the line order
/// the module documents (kind order, then value, then kind name tiebreak).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct GroupKey {
    /// A stable rank fixing the kind sort order (sources first, then passives…).
    kind_rank: u8,
    /// The kind's display name — distinguishes `other(led)` from `other(buzzer)`
    /// and breaks ties between two kinds that share a rank.
    kind_name: String,
    /// The value, as a sort/equality-friendly encoding: `None` sorts first, then
    /// by magnitude bits, then unit symbol. (See [`ValueKey`].)
    value: ValueKey,
}

impl GroupKey {
    fn new(kind: &PartKind, value: Option<Value>) -> Self {
        GroupKey {
            kind_rank: kind_rank(kind),
            kind_name: kind_label(kind),
            value: ValueKey::new(value),
        }
    }

    fn kind_label(&self) -> String {
        self.kind_name.clone()
    }

    fn value_label(&self) -> String {
        self.value.label()
    }
}

/// A total-order key for an optional [`Value`]: `None` sorts before any value;
/// values sort by magnitude (via its IEEE-754 total-order bits) then unit symbol.
/// Storing the magnitude's bits keeps equal magnitudes in one group and makes the
/// ordering total and deterministic (no `f64` `PartialOrd` pitfalls).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ValueKey {
    present: bool,
    magnitude_bits: u64,
    unit_symbol: &'static str,
    /// The displayed magnitude text, carried so the label matches what was keyed.
    text: String,
}

impl ValueKey {
    fn new(value: Option<Value>) -> Self {
        match value {
            None => ValueKey {
                present: false,
                magnitude_bits: 0,
                unit_symbol: "",
                text: String::new(),
            },
            Some(v) => ValueKey {
                present: true,
                magnitude_bits: total_order_bits(v.magnitude),
                unit_symbol: v.unit.symbol(),
                text: format!("{}", v.magnitude),
            },
        }
    }

    /// The `value` column text: `-` for no value, else magnitude + unit symbol
    /// (the same spelling the project format writes).
    fn label(&self) -> String {
        if !self.present {
            "-".to_string()
        } else {
            format!("{}{}", self.text, self.unit_symbol)
        }
    }
}

/// Map an `f64` to a `u64` that sorts in the same order as the float (IEEE-754
/// total order): flip the sign bit for positives, flip all bits for negatives.
/// NaNs sort to one end but never compare equal to a real magnitude's bits.
fn total_order_bits(x: f64) -> u64 {
    let bits = x.to_bits();
    if bits & (1 << 63) == 0 {
        bits | (1 << 63)
    } else {
        !bits
    }
}

/// A stable rank per kind, giving the BOM a schematic-friendly line order:
/// sources, then R/L/C passives, then semiconductors, then controlled sources,
/// then anything else.
fn kind_rank(kind: &PartKind) -> u8 {
    match kind {
        PartKind::VoltageSource => 0,
        PartKind::CurrentSource => 1,
        PartKind::Resistor => 2,
        PartKind::Inductor => 3,
        PartKind::Capacitor => 4,
        PartKind::Diode(_) => 5,
        PartKind::Bjt(..) => 6,
        PartKind::Mosfet(..) => 7,
        PartKind::Vcvs(_) | PartKind::Vccs(_) | PartKind::Ccvs(..) | PartKind::Cccs(..) => 8,
        PartKind::Other(_) => 9,
    }
}

/// A human-readable kind name for the `kind` column.
fn kind_label(kind: &PartKind) -> String {
    match kind {
        PartKind::Resistor => "Resistor".to_string(),
        PartKind::Capacitor => "Capacitor".to_string(),
        PartKind::Inductor => "Inductor".to_string(),
        PartKind::VoltageSource => "Voltage source".to_string(),
        PartKind::CurrentSource => "Current source".to_string(),
        PartKind::Diode(_) => "Diode".to_string(),
        PartKind::Bjt(..) => "BJT".to_string(),
        PartKind::Mosfet(..) => "MOSFET".to_string(),
        PartKind::Vcvs(_) => "VCVS".to_string(),
        PartKind::Vccs(_) => "VCCS".to_string(),
        PartKind::Ccvs(..) => "CCVS".to_string(),
        PartKind::Cccs(..) => "CCCS".to_string(),
        PartKind::Other(name) => {
            if name.is_empty() {
                "Other".to_string()
            } else {
                format!("Other ({name})")
            }
        }
    }
}

/// A natural-order sort key for a reference designator: its leading letters, then
/// its trailing number as an integer, so `R2` sorts before `R10`. A ref with no
/// trailing number sorts by its text with the number slot empty.
fn ref_key(reference: &str) -> (String, u64, String) {
    let digits_start = reference
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_digit())
        .last()
        .map(|(i, _)| i);
    match digits_start {
        Some(i) => {
            let (prefix, num) = reference.split_at(i);
            match num.parse::<u64>() {
                Ok(n) => (prefix.to_string(), n, String::new()),
                Err(_) => (reference.to_string(), 0, reference.to_string()),
            }
        }
        None => (reference.to_string(), 0, reference.to_string()),
    }
}

/// Quote a CSV field per RFC 4180 when it contains a comma, quote, or newline;
/// otherwise return it unchanged. (The same policy as the waveform CSV writer.)
fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for c in s.chars() {
            if c == '"' {
                out.push('"'); // RFC 4180 doubles an embedded quote
            }
            out.push(c);
        }
        out.push('"');
        out
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ohmforge_core::graph::{Circuit, NetId};
    use ohmforge_core::units::Unit;

    /// A small mixed circuit: a 5 V source, two 1 kΩ resistors, one 2.2 kΩ, a
    /// 100 nF cap, and an LED (`Other`), to exercise grouping and ordering.
    fn sample() -> Circuit {
        let mut c = Circuit::new("bom demo");
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
            Some(Value::new(1000.0, Unit::Ohm)),
            vec![n1, n2],
        );
        c.add_part(
            "R2",
            PartKind::Resistor,
            Some(Value::new(1000.0, Unit::Ohm)),
            vec![n2, NetId::GROUND],
        );
        c.add_part(
            "R10",
            PartKind::Resistor,
            Some(Value::new(2200.0, Unit::Ohm)),
            vec![n1, n2],
        );
        c.add_part(
            "C1",
            PartKind::Capacitor,
            Some(Value::new(1e-7, Unit::Farad)),
            vec![n2, NetId::GROUND],
        );
        c.add_part(
            "D1",
            PartKind::Other("led".into()),
            None,
            vec![n2, NetId::GROUND],
        );
        c
    }

    #[test]
    fn groups_by_kind_and_value_with_quantities() {
        let csv = to_csv(&sample());
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines[0], "kind,value,quantity,references");

        // The two 1 kΩ resistors collapse to one line, qty 2, refs naturally
        // ordered (R2 before R10).
        assert!(
            csv.contains("Resistor,1000Ω,2,R1 R2\n"),
            "grouped 1k resistors: {csv}"
        );
        // The lone 2.2 kΩ is its own line.
        assert!(csv.contains("Resistor,2200Ω,1,R10\n"), "{csv}");
        // The cap, the source, and the LED each appear.
        assert!(csv.contains("Capacitor,0.0000001F,1,C1\n"), "{csv}");
        assert!(csv.contains("Voltage source,5V,1,V1\n"), "{csv}");
        assert!(csv.contains("Other (led),-,1,D1\n"), "{csv}");
    }

    #[test]
    fn every_part_appears_exactly_once() {
        let circuit = sample();
        let csv = to_csv(&circuit);
        // Sum of quantities equals the part count (the BOM lists every part).
        let total: usize = csv
            .lines()
            .skip(1)
            .map(|l| l.split(',').nth(2).unwrap().parse::<usize>().unwrap())
            .sum();
        assert_eq!(total, circuit.parts().len(), "every part is accounted for");

        // Every reference designator shows up in the references column.
        for p in circuit.parts() {
            assert!(
                csv.contains(&p.reference),
                "{} missing from BOM: {csv}",
                p.reference
            );
        }
    }

    #[test]
    fn line_order_is_sources_then_passives_and_byte_stable() {
        let csv = to_csv(&sample());
        let body: Vec<&str> = csv.lines().skip(1).collect();
        // Source line comes before any resistor line.
        let v_at = body.iter().position(|l| l.starts_with("Voltage")).unwrap();
        let r_at = body.iter().position(|l| l.starts_with("Resistor")).unwrap();
        assert!(v_at < r_at, "sources precede passives: {body:?}");
        // Deterministic: same circuit, identical bytes.
        assert_eq!(to_csv(&sample()), csv);
    }

    #[test]
    fn valueless_parts_of_a_kind_group_together() {
        let mut c = Circuit::new("no values");
        let n = c.new_net();
        c.add_part("R1", PartKind::Resistor, None, vec![n, NetId::GROUND]);
        c.add_part("R2", PartKind::Resistor, None, vec![n, NetId::GROUND]);
        let csv = to_csv(&c);
        assert!(csv.contains("Resistor,-,2,R1 R2\n"), "{csv}");
    }

    #[test]
    fn empty_circuit_is_just_the_header() {
        let csv = to_csv(&Circuit::new("empty"));
        assert_eq!(csv, "kind,value,quantity,references\n");
    }

    #[test]
    fn a_comma_in_an_other_name_is_quoted() {
        let mut c = Circuit::new("q");
        let n = c.new_net();
        c.add_part(
            "U1",
            PartKind::Other("op-amp, dual".into()),
            None,
            vec![n, NetId::GROUND],
        );
        let csv = to_csv(&c);
        assert!(csv.contains("\"Other (op-amp, dual)\",-,1,U1\n"), "{csv}");
    }
}
