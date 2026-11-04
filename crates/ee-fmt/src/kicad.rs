//! KiCad netlist import (`CP-AF-M3.1`) — read the s-expression netlist KiCad's
//! *Tools → Generate Netlist* emits (the `(export (components …) (nets …))`
//! form) into an [`ee_core::Circuit`], a sibling of [`crate::spice`] and
//! [`crate::netlist`].
//!
//! ```text
//! (export (version "E")
//!   (components
//!     (comp (ref "V1") (value "10"))
//!     (comp (ref "R1") (value "1k"))
//!     (comp (ref "R2") (value "1k")))
//!   (nets
//!     (net (code "1") (name "/in")
//!       (node (ref "V1") (pin "1"))
//!       (node (ref "R1") (pin "1")))
//!     (net (code "2") (name "/out")
//!       (node (ref "R1") (pin "2"))
//!       (node (ref "R2") (pin "1")))
//!     (net (code "3") (name "GND")
//!       (node (ref "V1") (pin "2"))
//!       (node (ref "R2") (pin "2")))))
//! ```
//!
//! ## The subset read at M3
//!
//! * **Components** are taken from the `(components …)` block: each `(comp (ref
//!   …) (value …) …)` becomes a [`Part`](ee_core::graph::Part). The kind comes
//!   from the reference's first letter, exactly like the other importers — `R`
//!   (ohms), `C` (farads), `L` (henries), `V` (volts), `I` (amps), `D`
//!   (diode) — and anything else becomes [`PartKind::Other`] carrying the KiCad
//!   `value` string (a `Q`/`U`/… the engine doesn't model natively). Everything
//!   past `ref`/`value` in a `comp` (footprint, libsource, …) is ignored.
//! * **Nets** are taken from the `(nets …)` block. Each `(net …)` is one
//!   electrical node; its `(node (ref …) (pin …))` entries say which component
//!   pins land on it. A net whose `name` is a ground alias (`GND`, `GNDA`,
//!   `GNDD`, `AGND`, `DGND`, `0`, `GROUND`, `EARTH`), or whose `code` is `0`,
//!   collapses onto [`NetId::GROUND`].
//! * **Terminals** are ordered by ascending pin — numeric pins first (`1`, `2`,
//!   `10`), then the rest lexically — so a two-pin passive's pin 1/2 map to
//!   terminals 0/1, and a diode's pin 1 (anode) / pin 2 (cathode) land in the
//!   `[anode, cathode]` order the engine expects.
//!
//! ## Values
//!
//! A component value is parsed with the engineering notation `ee-core` already
//! understands (`1k`, `4k7`, `100n`), the unit fixed by the kind. Source values
//! accept a leading `DC`/`AC` keyword (`(value "DC 10")` → 10 V). A value that
//! is absent or not a magnitude (a diode's `"LED"`, a transistor's `"2N3904"`)
//! leaves the part valued `None` rather than failing the import — the part is
//! still placed and wired.
//!
//! ## Errors
//!
//! Unlike the line-oriented parsers, this reader works on a parsed s-expression
//! *tree*, so a [`KicadError`] names the offending construct (unbalanced parens,
//! a root that is not `(export …)`, a duplicate reference) rather than a source
//! line. Zero dependencies beyond `ee-core`; the s-expression reader is
//! hand-written like its siblings.

use std::collections::HashMap;
use std::error::Error;
use std::fmt;

use ee_core::graph::{Circuit, DiodeModel, NetId, PartKind};
use ee_core::units::Unit;
use ee_core::value::Value;

/// A KiCad-netlist import failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KicadError {
    /// The s-expression could not be tokenized/parsed (unbalanced parens, an
    /// unterminated quoted string, or trailing junk). Carries a reason.
    Syntax(String),
    /// The top-level form was not `(export …)` — this is not a KiCad netlist.
    NotKicadNetlist,
    /// Two `(comp …)` entries share a reference designator.
    DuplicateComponent(String),
}

impl fmt::Display for KicadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KicadError::Syntax(why) => write!(f, "malformed KiCad netlist: {why}"),
            KicadError::NotKicadNetlist => {
                write!(f, "not a KiCad netlist (expected a top-level `(export …)`)")
            }
            KicadError::DuplicateComponent(r) => {
                write!(f, "duplicate component reference {r:?}")
            }
        }
    }
}

impl Error for KicadError {}

/// Import a KiCad s-expression netlist into a [`Circuit`] ready for the engine.
/// See the module docs for the exact subset understood.
pub fn import(src: &str) -> Result<Circuit, KicadError> {
    let root = parse_sexp(src)?;

    // The top-level form must be `(export …)`.
    let items = root.as_list().ok_or(KicadError::NotKicadNetlist)?;
    if items.first().and_then(Sexp::as_atom) != Some("export") {
        return Err(KicadError::NotKicadNetlist);
    }

    let mut circuit = Circuit::new("");

    // ── Components: ref → value string, in declaration order. ────────────────
    let mut comp_order: Vec<String> = Vec::new();
    let mut comp_value: HashMap<String, Option<String>> = HashMap::new();
    if let Some(components) = child_list(items, "components") {
        for comp in children_named(components, "comp") {
            let Some(reference) = field(comp, "ref") else {
                continue;
            };
            if comp_value.contains_key(&reference) {
                return Err(KicadError::DuplicateComponent(reference));
            }
            let value = field(comp, "value");
            comp_order.push(reference.clone());
            comp_value.insert(reference, value);
        }
    }

    // ── Nets: allocate a NetId per net, collect each component pin's net. ─────
    // `pins[ref]` = list of (pin-name, net) for that component.
    let mut pins: HashMap<String, Vec<(String, NetId)>> = HashMap::new();
    if let Some(nets) = child_list(items, "nets") {
        for net in children_named(nets, "net") {
            let name = field(net, "name").unwrap_or_default();
            let code = field(net, "code").unwrap_or_default();
            let id = if is_ground_net(&name, &code) {
                NetId::GROUND
            } else {
                circuit.new_net()
            };
            for node in children_named(net, "node") {
                let (Some(reference), Some(pin)) = (field(node, "ref"), field(node, "pin")) else {
                    continue;
                };
                pins.entry(reference).or_default().push((pin, id));
            }
        }
    }

    // ── Build each component in declaration order (deterministic). ────────────
    for reference in &comp_order {
        let (kind, unit) = classify(reference);
        let raw = comp_value.get(reference).and_then(|v| v.as_deref());
        let value = raw.and_then(|raw| parse_value(&kind, unit, raw));
        // An unmodeled part keeps its KiCad `value` as the `Other` name.
        let kind = other_name(kind, raw);

        let mut terminals = pins.remove(reference).unwrap_or_default();
        terminals.sort_by_key(|(pin, _)| pin_key(pin));
        let terminals: Vec<NetId> = terminals.into_iter().map(|(_, net)| net).collect();

        circuit.add_part(reference.clone(), kind, value, terminals);
    }

    Ok(circuit)
}

/// Map a reference's leading letter to a part kind and its value's unit, like the
/// SPICE/netlist importers. Unknown prefixes become [`PartKind::Other`] (the
/// value string is attached when the part is built).
fn classify(reference: &str) -> (PartKind, Unit) {
    match reference.chars().next().map(|c| c.to_ascii_uppercase()) {
        Some('R') => (PartKind::Resistor, Unit::Ohm),
        Some('C') => (PartKind::Capacitor, Unit::Farad),
        Some('L') => (PartKind::Inductor, Unit::Henry),
        Some('V') => (PartKind::VoltageSource, Unit::Volt),
        Some('I') => (PartKind::CurrentSource, Unit::Ampere),
        Some('D') => (PartKind::Diode(DiodeModel::default()), Unit::Ampere),
        // Transistors, ICs, connectors, … aren't modeled natively; keep the name.
        _ => (PartKind::Other(String::new()), Unit::None),
    }
}

/// Parse a component value for its kind, or `None` when it is absent or not a
/// magnitude. `Other` parts keep the raw string as their name instead.
fn parse_value(kind: &PartKind, unit: Unit, raw: &str) -> Option<Value> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    match kind {
        // A diode's value is a part number ("LED", "1N4148"), never a magnitude.
        PartKind::Diode(_) | PartKind::Other(_) => None,
        PartKind::VoltageSource | PartKind::CurrentSource => {
            // Sources may carry a leading `DC`/`AC` keyword (`DC 10`).
            let tok = raw
                .split_whitespace()
                .find(|t| !t.eq_ignore_ascii_case("DC") && !t.eq_ignore_ascii_case("AC"))?;
            Value::parse(tok, unit).ok()
        }
        _ => Value::parse(raw, unit).ok(),
    }
}

/// Keep the raw KiCad `value` as an [`PartKind::Other`] part's name, so a
/// `2N3904` transistor or a `74HC00` IC survives import as `other(2N3904)`.
fn other_name(kind: PartKind, raw: Option<&str>) -> PartKind {
    match kind {
        PartKind::Other(_) => PartKind::Other(raw.unwrap_or("").trim().to_string()),
        other => other,
    }
}

/// Whether a net name/code is a ground reference.
fn is_ground_net(name: &str, code: &str) -> bool {
    if code.trim() == "0" {
        return true;
    }
    // KiCad net names are often hierarchical (`/sheet/GND`); match the last leg.
    let leaf = name.rsplit('/').next().unwrap_or(name).trim();
    matches!(
        leaf.to_ascii_uppercase().as_str(),
        "GND" | "GNDA" | "GNDD" | "AGND" | "DGND" | "0" | "GROUND" | "EARTH" | "VSS"
    )
}

/// A sort key for a pin name: numeric pins (`1`, `2`, `10`) sort first and in
/// numeric order; everything else sorts after, lexically. Returned as a tuple so
/// `Ord` does the work.
fn pin_key(pin: &str) -> (u8, u64, String) {
    match pin.trim().parse::<u64>() {
        Ok(n) => (0, n, String::new()),
        Err(_) => (1, 0, pin.trim().to_string()),
    }
}

// ── A tiny s-expression reader (hand-rolled, zero-dep) ───────────────────────

/// A parsed s-expression node: a bare/quoted atom, or a parenthesized list.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Sexp {
    Atom(String),
    List(Vec<Sexp>),
}

impl Sexp {
    fn as_atom(&self) -> Option<&str> {
        match self {
            Sexp::Atom(s) => Some(s.as_str()),
            Sexp::List(_) => None,
        }
    }

    fn as_list(&self) -> Option<&[Sexp]> {
        match self {
            Sexp::List(items) => Some(items),
            Sexp::Atom(_) => None,
        }
    }
}

/// Parse exactly one top-level s-expression from `src` (trailing whitespace and
/// `;`-comments allowed). Errors on unbalanced parens, an unterminated string, or
/// trailing junk after the first form.
fn parse_sexp(src: &str) -> Result<Sexp, KicadError> {
    let tokens = tokenize(src)?;
    let mut pos = 0;
    let node = parse_node(&tokens, &mut pos)?;
    if pos != tokens.len() {
        return Err(KicadError::Syntax(
            "unexpected trailing data after the top-level form".to_string(),
        ));
    }
    Ok(node)
}

/// One lexical token of an s-expression.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Open,
    Close,
    Atom(String),
}

/// Split s-expression text into tokens. Handles `(`/`)`, double-quoted strings
/// (with `\` escapes), bare atoms, and `;`-to-end-of-line comments.
fn tokenize(src: &str) -> Result<Vec<Token>, KicadError> {
    let mut tokens = Vec::new();
    let mut chars = src.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            '(' => {
                chars.next();
                tokens.push(Token::Open);
            }
            ')' => {
                chars.next();
                tokens.push(Token::Close);
            }
            ';' => {
                // Comment to end of line.
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '"' => {
                chars.next(); // opening quote
                let mut s = String::new();
                let mut closed = false;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => {
                            closed = true;
                            break;
                        }
                        '\\' => {
                            if let Some(esc) = chars.next() {
                                s.push(esc);
                            }
                        }
                        _ => s.push(c),
                    }
                }
                if !closed {
                    return Err(KicadError::Syntax("unterminated quoted string".to_string()));
                }
                tokens.push(Token::Atom(s));
            }
            c if c.is_whitespace() => {
                chars.next();
            }
            _ => {
                let mut s = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() || c == '(' || c == ')' || c == '"' || c == ';' {
                        break;
                    }
                    s.push(c);
                    chars.next();
                }
                tokens.push(Token::Atom(s));
            }
        }
    }
    Ok(tokens)
}

/// Parse one node (atom or list) from `tokens` at `*pos`, advancing `*pos`.
fn parse_node(tokens: &[Token], pos: &mut usize) -> Result<Sexp, KicadError> {
    match tokens.get(*pos) {
        None => Err(KicadError::Syntax("unexpected end of input".to_string())),
        Some(Token::Close) => Err(KicadError::Syntax("unexpected `)`".to_string())),
        Some(Token::Atom(s)) => {
            let atom = Sexp::Atom(s.clone());
            *pos += 1;
            Ok(atom)
        }
        Some(Token::Open) => {
            *pos += 1;
            let mut items = Vec::new();
            loop {
                match tokens.get(*pos) {
                    None => {
                        return Err(KicadError::Syntax(
                            "unbalanced `(` — missing `)`".to_string(),
                        ))
                    }
                    Some(Token::Close) => {
                        *pos += 1;
                        return Ok(Sexp::List(items));
                    }
                    Some(_) => items.push(parse_node(tokens, pos)?),
                }
            }
        }
    }
}

/// The first direct child list of `items` whose head atom is `head`, as its own
/// items (`(head …rest)` → `…rest`).
fn child_list<'a>(items: &'a [Sexp], head: &str) -> Option<&'a [Sexp]> {
    items.iter().find_map(|node| {
        let list = node.as_list()?;
        (list.first()?.as_atom()? == head).then_some(list)
    })
}

/// Every direct child list of `items` whose head atom is `name`.
fn children_named<'a>(items: &'a [Sexp], name: &str) -> impl Iterator<Item = &'a [Sexp]> {
    // `name` is cloned into the closure so the returned iterator borrows only
    // `items` (not the caller's `name`), keeping its lifetime to `'a` alone.
    let name = name.to_string();
    items.iter().filter_map(move |node| {
        let list = node.as_list()?;
        (list.first()?.as_atom()? == name).then_some(list)
    })
}

/// The single-atom argument of a `(key value)` child of `items` — e.g. `ref` in
/// `(comp (ref "R1") …)` yields `"R1"`. `None` if the key is absent or its value
/// is not a lone atom.
fn field(items: &[Sexp], key: &str) -> Option<String> {
    let list = child_list(items, key)?;
    match list {
        [_, Sexp::Atom(v)] => Some(v.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIVIDER: &str = r#"
(export (version "E")
  (components
    (comp (ref "V1") (value "DC 10") (footprint "ee:V"))
    (comp (ref "R1") (value "1k"))
    (comp (ref "R2") (value "1k")))
  (nets
    (net (code "1") (name "/in")
      (node (ref "V1") (pin "1"))
      (node (ref "R1") (pin "1")))
    (net (code "2") (name "/out")
      (node (ref "R1") (pin "2"))
      (node (ref "R2") (pin "1")))
    (net (code "3") (name "GND")
      (node (ref "V1") (pin "2"))
      (node (ref "R2") (pin "2")))))
"#;

    #[test]
    fn imports_components_kinds_and_values() {
        let c = import(DIVIDER).unwrap();
        assert_eq!(c.parts().len(), 3);

        let v1 = c.part_by_ref("V1").unwrap();
        assert!(matches!(v1.kind, PartKind::VoltageSource));
        assert_eq!(v1.value.as_ref().unwrap().magnitude, 10.0);

        let r1 = c.part_by_ref("R1").unwrap();
        assert!(matches!(r1.kind, PartKind::Resistor));
        assert_eq!(r1.value.as_ref().unwrap().magnitude, 1000.0);
    }

    #[test]
    fn nets_wire_pins_together_and_ground_collapses() {
        let c = import(DIVIDER).unwrap();
        let v1 = c.part_by_ref("V1").unwrap();
        let r1 = c.part_by_ref("R1").unwrap();
        let r2 = c.part_by_ref("R2").unwrap();

        // `/in` is shared by V1.pin1 and R1.pin1 (both terminal 0).
        assert_eq!(c.resolve(v1.terminals[0]), c.resolve(r1.terminals[0]));
        // `/out` is shared by R1.pin2 and R2.pin1.
        assert_eq!(c.resolve(r1.terminals[1]), c.resolve(r2.terminals[0]));
        // The `GND` net collapses to ground for V1.pin2 and R2.pin2.
        assert_eq!(c.resolve(v1.terminals[1]), NetId::GROUND);
        assert_eq!(c.resolve(r2.terminals[1]), NetId::GROUND);
    }

    #[test]
    fn imported_divider_simulates_to_the_midpoint() {
        // The gate: import → solve → the midpoint sits at half the 10 V rail.
        use ee_sim::{Analysis, Engine};
        let c = import(DIVIDER).unwrap();
        let mid = c.part_by_ref("R1").unwrap().terminals[1];
        let sigs = Engine::new().run(&c, &Analysis::OperatingPoint).unwrap();
        let name = format!("v(n{})", c.resolve(mid).0);
        let v = sigs
            .iter()
            .find(|s| s.name == name)
            .expect("v(out) present")
            .real()
            .unwrap()[0];
        assert!((v - 5.0).abs() < 1e-9, "midpoint = {v}");
    }

    #[test]
    fn unquoted_atoms_and_hierarchical_ground_are_accepted() {
        // Older exports leave ref/value unquoted; a hierarchical `/sheet/GND`
        // name still reads as ground on its last leg.
        let src = r#"(export
            (components (comp (ref R1) (value 2k)) (comp (ref R2) (value 2k)))
            (nets
              (net (code 1) (name /sheet/GND) (node (ref R1) (pin 2)) (node (ref R2) (pin 2)))
              (net (code 2) (name top) (node (ref R1) (pin 1)) (node (ref R2) (pin 1)))))"#;
        let c = import(src).unwrap();
        let r1 = c.part_by_ref("R1").unwrap();
        let r2 = c.part_by_ref("R2").unwrap();
        assert_eq!(c.resolve(r1.terminals[1]), NetId::GROUND);
        assert_eq!(c.resolve(r1.terminals[0]), c.resolve(r2.terminals[0]));
    }

    #[test]
    fn code_zero_is_ground_even_without_a_name() {
        let src = r#"(export (components (comp (ref R1) (value 1k)))
            (nets
              (net (code 0) (name "") (node (ref R1) (pin 2)))
              (net (code 1) (name a) (node (ref R1) (pin 1)))))"#;
        let c = import(src).unwrap();
        assert_eq!(c.part_by_ref("R1").unwrap().terminals[1], NetId::GROUND);
    }

    #[test]
    fn unmodeled_parts_keep_their_value_as_other() {
        let src = r#"(export (components (comp (ref Q1) (value "2N3904")) (comp (ref D1) (value "LED")))
            (nets (net (code 1) (name a) (node (ref Q1) (pin 1)) (node (ref D1) (pin 1)))))"#;
        let c = import(src).unwrap();
        let q1 = &c.part_by_ref("Q1").unwrap().kind;
        assert!(matches!(q1, PartKind::Other(n) if n == "2N3904"), "{q1:?}");
        // A diode is modeled as a Diode with no magnitude value.
        assert!(matches!(
            c.part_by_ref("D1").unwrap().kind,
            PartKind::Diode(_)
        ));
        assert_eq!(c.part_by_ref("D1").unwrap().value, None);
    }

    #[test]
    fn terminals_follow_ascending_pin_order() {
        // Pins given out of order still map pin1→terminal0, pin2→terminal1.
        let src = r#"(export (components (comp (ref R1) (value 1k)))
            (nets
              (net (code 1) (name hi) (node (ref R1) (pin 2)))
              (net (code 2) (name lo) (node (ref R1) (pin 1)))))"#;
        let c = import(src).unwrap();
        let r1 = c.part_by_ref("R1").unwrap();
        // terminal 0 is the `lo` net (pin 1), terminal 1 is `hi` (pin 2).
        assert_ne!(c.resolve(r1.terminals[0]), c.resolve(r1.terminals[1]));
        assert_eq!(r1.terminals.len(), 2);
    }

    #[test]
    fn rejects_non_export_and_malformed_input() {
        assert_eq!(
            import("(netlist (foo))").unwrap_err(),
            KicadError::NotKicadNetlist
        );
        assert!(matches!(
            import("(export (components").unwrap_err(),
            KicadError::Syntax(_)
        ));
        assert!(matches!(
            import(r#"(export (components (comp (ref "R1") (value "1k"#).unwrap_err(),
            KicadError::Syntax(_)
        ));
    }

    #[test]
    fn duplicate_component_is_rejected() {
        let src = r#"(export (components (comp (ref R1) (value 1k)) (comp (ref R1) (value 2k))))"#;
        assert_eq!(
            import(src).unwrap_err(),
            KicadError::DuplicateComponent("R1".to_string())
        );
    }

    #[test]
    fn display_is_friendly() {
        assert!(KicadError::NotKicadNetlist
            .to_string()
            .contains("KiCad netlist"));
    }
}
