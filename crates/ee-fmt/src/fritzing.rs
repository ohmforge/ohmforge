//! Fritzing-style sketch export (`CP-AF-M3.4`) — write a [`Circuit`] to the
//! XML Fritzing stores inside a sketch, a sibling of [`crate::spice`],
//! [`crate::kicad`], and [`crate::bom`].
//!
//! Fritzing's native sketch is a zipped bundle (`.fzz`) whose central document is
//! an XML file (`.fz`): a `<module>` holding an `<instances>` list, one
//! `<instance>` per placed part, each carrying its part metadata and — through
//! per-connector `<connects>` lists — the wiring. This module emits that **`.fz`
//! XML** directly: no zip, no external part (`.fzp`) files, just a well-formed,
//! self-describing document. A zip crate would add a dependency for no modeling
//! gain, so it is deliberately omitted (the rule: `ee-fmt` depends on `ee-core`
//! alone).
//!
//! ```text
//! <?xml version="1.0" encoding="UTF-8"?>
//! <module fritzingVersion="0.9.6" moduleId="ee-assist:divider">
//!   <title>divider</title>
//!   <instances>
//!     <instance moduleIdRef="ee-assist:resistor" modelIndex="1" path="R1">
//!       <title>R1</title>
//!       <property name="family" value="resistor"/>
//!       <property name="value" value="1kΩ"/>
//!       <views>
//!         <schematicView layer="schematic">
//!           <connectors>
//!             <connector connectorId="connector0" net="n1">
//!               <connects>
//!                 <connect connectorId="connector0" modelIndex="0" layer="schematic"/>
//!               </connects>
//!             </connector>
//!             ...
//!           </connectors>
//!         </schematicView>
//!       </views>
//!     </instance>
//!     ...
//!   </instances>
//! </module>
//! ```
//!
//! ## The subset emitted
//!
//! * **Instances.** One `<instance>` per [`Part`](ee_core::graph::Part), in
//!   circuit order. `modelIndex` is the part's position (the stable handle a
//!   `<connect>` points back to), `path` and `<title>` carry the reference
//!   designator, `moduleIdRef` is `ee-assist:<kind-slug>` (Fritzing-style
//!   namespaced id), and `<property>` rows record the `family` and, when the part
//!   has one, its `value` (magnitude + unit symbol, e.g. `1kΩ` → written as the
//!   shortest round-trippable magnitude plus the SI symbol, matching the BOM).
//! * **Connectors & nets.** Each terminal becomes a `<connector connectorId=
//!   "connector<i>" net="n<id>">`, where the net id is the **resolved** net
//!   ([`Circuit::resolve`]) so wired-together nodes share one net — ground is
//!   `n0`. The connector's `<connects>` lists one `<connect>` per *other*
//!   terminal (on any instance) landing on that same resolved net, naming the
//!   peer's `modelIndex` and `connectorId`. That per-connector adjacency is
//!   exactly how Fritzing records a wire, and it fully encodes the netlist.
//!
//! The output is deterministic (circuit order throughout) and XML-escaped, so a
//! reference or `Other` name containing `<`, `&`, or a quote stays well-formed.
//! Export is infallible — it builds a [`String`] — like the SPICE and BOM
//! exporters.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use ee_core::graph::{Circuit, PartKind};
use ee_core::value::Value;

/// The Fritzing format version stamped into the `<module>` header.
const FRITZING_VERSION: &str = "0.9.6";

/// The layer every schematic connector/connection is tagged with.
const LAYER: &str = "schematic";

/// Export `circuit` as Fritzing-style `.fz` sketch XML. See the module docs for
/// the exact subset emitted. Always well-formed: an empty circuit yields a
/// `<module>` with an empty `<instances/>`.
pub fn export(circuit: &Circuit) -> String {
    // Net → the (model index, connector index) occurrences landing on it, so each
    // connector can list its peers. BTreeMap + circuit order ⇒ deterministic.
    let mut net_members: BTreeMap<u32, Vec<(usize, usize)>> = BTreeMap::new();
    for (idx, part) in circuit.parts().iter().enumerate() {
        for (conn, &terminal) in part.terminals.iter().enumerate() {
            let net = circuit.resolve(terminal).0;
            net_members.entry(net).or_default().push((idx, conn));
        }
    }

    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let module_id = format!("ee-assist:{}", slug(&circuit.name, "circuit"));
    let _ = writeln!(
        out,
        "<module fritzingVersion=\"{FRITZING_VERSION}\" moduleId=\"{}\">",
        xml_attr(&module_id)
    );
    let title = if circuit.name.is_empty() {
        "ee-assist circuit"
    } else {
        circuit.name.as_str()
    };
    let _ = writeln!(out, "  <title>{}</title>", xml_text(title));

    if circuit.parts().is_empty() {
        out.push_str("  <instances/>\n");
        out.push_str("</module>\n");
        return out;
    }

    out.push_str("  <instances>\n");
    for (idx, part) in circuit.parts().iter().enumerate() {
        let _ = writeln!(
            out,
            "    <instance moduleIdRef=\"{}\" modelIndex=\"{idx}\" path=\"{}\">",
            xml_attr(&module_id_ref(&part.kind)),
            xml_attr(&part.reference),
        );
        let _ = writeln!(out, "      <title>{}</title>", xml_text(&part.reference));
        let _ = writeln!(
            out,
            "      <property name=\"family\" value=\"{}\"/>",
            xml_attr(family(&part.kind)),
        );
        if let Some(label) = value_label(part.value.as_ref()) {
            let _ = writeln!(
                out,
                "      <property name=\"value\" value=\"{}\"/>",
                xml_attr(&label),
            );
        }

        out.push_str("      <views>\n");
        let _ = writeln!(out, "        <schematicView layer=\"{LAYER}\">");
        out.push_str("          <connectors>\n");
        for (conn, &terminal) in part.terminals.iter().enumerate() {
            let net = circuit.resolve(terminal).0;
            let _ = writeln!(
                out,
                "            <connector connectorId=\"connector{conn}\" net=\"n{net}\">",
            );
            out.push_str("              <connects>\n");
            for &(peer_idx, peer_conn) in net_members.get(&net).into_iter().flatten() {
                if peer_idx == idx && peer_conn == conn {
                    continue; // a connector does not connect to itself
                }
                let _ = writeln!(
                    out,
                    "                <connect connectorId=\"connector{peer_conn}\" modelIndex=\"{peer_idx}\" layer=\"{LAYER}\"/>",
                );
            }
            out.push_str("              </connects>\n");
            out.push_str("            </connector>\n");
        }
        out.push_str("          </connectors>\n");
        out.push_str("        </schematicView>\n");
        out.push_str("      </views>\n");
        out.push_str("    </instance>\n");
    }
    out.push_str("  </instances>\n");
    out.push_str("</module>\n");
    out
}

/// The Fritzing-style namespaced part id for a kind, `ee-assist:<slug>`.
fn module_id_ref(kind: &PartKind) -> String {
    let slug = match kind {
        PartKind::Resistor => "resistor",
        PartKind::Capacitor => "capacitor",
        PartKind::Inductor => "inductor",
        PartKind::VoltageSource => "voltage-source",
        PartKind::CurrentSource => "current-source",
        PartKind::Diode(_) => "diode",
        PartKind::Bjt(..) => "bjt",
        PartKind::Mosfet(..) => "mosfet",
        PartKind::Vcvs(_) => "vcvs",
        PartKind::Vccs(_) => "vccs",
        PartKind::Ccvs(..) => "ccvs",
        PartKind::Cccs(..) => "cccs",
        PartKind::Other(name) => return format!("ee-assist:other:{}", slug(name, "part")),
    };
    format!("ee-assist:{slug}")
}

/// The `family` property value — the broad part family a librarian groups by.
fn family(kind: &PartKind) -> &str {
    match kind {
        PartKind::Resistor => "resistor",
        PartKind::Capacitor => "capacitor",
        PartKind::Inductor => "inductor",
        PartKind::VoltageSource => "voltage source",
        PartKind::CurrentSource => "current source",
        PartKind::Diode(_) => "diode",
        PartKind::Bjt(..) => "transistor (bjt)",
        PartKind::Mosfet(..) => "transistor (mosfet)",
        PartKind::Vcvs(_) | PartKind::Vccs(_) | PartKind::Ccvs(..) | PartKind::Cccs(..) => {
            "controlled source"
        }
        PartKind::Other(_) => "other",
    }
}

/// The `value` property text: the shortest round-trippable magnitude plus the
/// unit symbol (`1000Ω`, `5V`), matching the BOM/project spelling. `None` ⇒ no
/// value property is written.
fn value_label(value: Option<&Value>) -> Option<String> {
    value.map(|v| format!("{}{}", v.magnitude, v.unit.symbol()))
}

/// Lowercase, hyphenated slug of `s` for an id segment: alphanumerics kept, every
/// other run collapsed to a single `-`. Falls back to `fallback` when empty.
fn slug(s: &str, fallback: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

/// Escape text content for an XML element body (`<`, `>`, `&`).
fn xml_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

/// Escape a value for a double-quoted XML attribute (text escapes plus `"`).
fn xml_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ee_core::graph::{Circuit, NetId, PartKind};
    use ee_core::units::Unit;

    /// A 5 V → divider: V1 (in→gnd), R1 (in→out) 1 kΩ, R2 (out→gnd) 2 kΩ.
    fn divider() -> Circuit {
        let mut c = Circuit::new("divider");
        let n_in = c.new_net();
        let n_out = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            Some(Value::new(5.0, Unit::Volt)),
            vec![n_in, NetId::GROUND],
        );
        c.add_part(
            "R1",
            PartKind::Resistor,
            Some(Value::new(1000.0, Unit::Ohm)),
            vec![n_in, n_out],
        );
        c.add_part(
            "R2",
            PartKind::Resistor,
            Some(Value::new(2000.0, Unit::Ohm)),
            vec![n_out, NetId::GROUND],
        );
        c
    }

    // ── A minimal well-formedness checker (zero-dep, test-only) ─────────────────

    /// Walk the document and return `Err` on the first structural fault: a
    /// mismatched or unclosed tag, or stray markup. Understands the XML
    /// declaration, self-closing tags (`<x/>`), and attributes with quoted values.
    /// Enough to assert the export is well-formed XML without pulling in a parser.
    fn check_well_formed(xml: &str) -> Result<(), String> {
        let bytes = xml.as_bytes();
        let mut i = 0;
        let mut stack: Vec<String> = Vec::new();
        let mut saw_root = false;
        while i < bytes.len() {
            if bytes[i] != b'<' {
                i += 1;
                continue;
            }
            // Processing instruction `<?...?>` — skip wholesale.
            if xml[i..].starts_with("<?") {
                let end = xml[i..].find("?>").ok_or("unterminated <? ?>")?;
                i += end + 2;
                continue;
            }
            // Find the matching `>`, honoring quoted attribute values.
            let mut j = i + 1;
            let mut in_quote = false;
            while j < bytes.len() {
                match bytes[j] {
                    b'"' => in_quote = !in_quote,
                    b'>' if !in_quote => break,
                    _ => {}
                }
                j += 1;
            }
            if j >= bytes.len() {
                return Err("unterminated tag".to_string());
            }
            let inner = xml[i + 1..j].trim();
            if let Some(close) = inner.strip_prefix('/') {
                // Closing tag: must match the top of the stack.
                let name = close.trim();
                match stack.pop() {
                    Some(open) if open == name => {}
                    Some(open) => return Err(format!("</{name}> closes <{open}>")),
                    None => return Err(format!("</{name}> with no open tag")),
                }
            } else if inner.ends_with('/') {
                // Self-closing: opens and closes at once, still needs a root.
                saw_root = true;
            } else {
                let name = inner.split_whitespace().next().unwrap_or("").to_string();
                if name.is_empty() {
                    return Err("empty tag name".to_string());
                }
                stack.push(name);
                saw_root = true;
            }
            i = j + 1;
        }
        if !stack.is_empty() {
            return Err(format!("unclosed tags: {stack:?}"));
        }
        if !saw_root {
            return Err("no elements".to_string());
        }
        Ok(())
    }

    #[test]
    fn export_is_well_formed_with_a_module_and_instances() {
        let xml = export(&divider());
        check_well_formed(&xml).expect("well-formed XML");
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(xml.contains("<module fritzingVersion=\"0.9.6\""));
        assert!(xml.contains("moduleId=\"ee-assist:divider\""));
        assert!(xml.contains("<title>divider</title>"));
    }

    #[test]
    fn every_part_is_an_instance_with_its_reference_and_kind() {
        let xml = export(&divider());
        // One instance per part, carrying the reference as path + title.
        assert_eq!(xml.matches("<instance ").count(), 3);
        for reference in ["V1", "R1", "R2"] {
            assert!(
                xml.contains(&format!("path=\"{reference}\"")),
                "{reference}: {xml}"
            );
            assert!(
                xml.contains(&format!("<title>{reference}</title>")),
                "{reference}"
            );
        }
        // Kinds map to namespaced module ids and families.
        assert!(xml.contains("moduleIdRef=\"ee-assist:voltage-source\""));
        assert!(xml.contains("moduleIdRef=\"ee-assist:resistor\""));
        assert!(xml.contains("<property name=\"family\" value=\"resistor\"/>"));
        // Values are written with the unit symbol.
        assert!(xml.contains("<property name=\"value\" value=\"5V\"/>"));
        assert!(xml.contains("<property name=\"value\" value=\"1000Ω\"/>"));
        assert!(xml.contains("<property name=\"value\" value=\"2000Ω\"/>"));
    }

    #[test]
    fn connectors_encode_the_shared_nets() {
        let xml = export(&divider());
        // The output node `out` is shared by R1.connector1 and R2.connector0, and
        // nothing else. R1 is modelIndex 1, R2 is modelIndex 2.
        // R1's second connector connects to R2's first.
        assert!(
            xml.contains(
                "<connect connectorId=\"connector0\" modelIndex=\"2\" layer=\"schematic\"/>"
            ),
            "R1.out -> R2: {xml}"
        );
        // Ground (n0) is shared by V1.connector1 and R2.connector1.
        assert!(
            xml.contains("net=\"n0\""),
            "ground connector present: {xml}"
        );
        // Every terminal became a connector: V1(2) + R1(2) + R2(2) = 6.
        assert_eq!(xml.matches("<connector connectorId=").count(), 6);
    }

    #[test]
    fn shared_net_connections_are_symmetric_and_grounded() {
        // Two resistors both from a node `a` to ground: `a` (n1) is shared by
        // R1.c0 and R2.c0; ground (n0) by R1.c1 and R2.c1.
        let mut c = Circuit::new("pair");
        let a = c.new_net();
        c.add_part(
            "R1",
            PartKind::Resistor,
            Some(Value::new(1000.0, Unit::Ohm)),
            vec![a, NetId::GROUND],
        );
        c.add_part(
            "R2",
            PartKind::Resistor,
            Some(Value::new(1000.0, Unit::Ohm)),
            vec![a, NetId::GROUND],
        );
        let xml = export(&c);
        check_well_formed(&xml).unwrap();
        // R1 (index 0) connector0 connects to R2 (index 1) connector0 on net a.
        assert!(xml.contains(
            "<connect connectorId=\"connector0\" modelIndex=\"1\" layer=\"schematic\"/>"
        ));
        // And the reverse: R2.c0 -> R1 (index 0) c0.
        assert!(xml.contains(
            "<connect connectorId=\"connector0\" modelIndex=\"0\" layer=\"schematic\"/>"
        ));
        // A floating/standalone connector (none here) would have an empty
        // <connects></connects>; both nets have two members, so each connector
        // lists exactly one peer.
        assert_eq!(xml.matches("<connect ").count(), 4);
    }

    #[test]
    fn empty_circuit_is_well_formed_with_no_instances() {
        let xml = export(&Circuit::new("empty"));
        check_well_formed(&xml).expect("well-formed");
        assert!(xml.contains("<instances/>"));
        assert!(!xml.contains("<instance "));
    }

    #[test]
    fn other_parts_and_special_characters_stay_well_formed() {
        let mut c = Circuit::new("sketch <1> & \"more\"");
        let n = c.new_net();
        c.add_part(
            "U<1>",
            PartKind::Other("op-amp & buffer".into()),
            None,
            vec![n, NetId::GROUND],
        );
        let xml = export(&c);
        check_well_formed(&xml).expect("escaped markup stays well-formed");
        // The title text and the attribute are escaped.
        assert!(xml.contains("<title>U&lt;1&gt;</title>"), "{xml}");
        assert!(xml.contains("&amp;"), "ampersand escaped: {xml}");
        // The Other name is slugged into the module id.
        assert!(xml.contains("ee-assist:other:op-amp-buffer"), "{xml}");
        // No value property for a valueless part.
        assert!(!xml.contains("name=\"value\""), "no value row: {xml}");
    }

    #[test]
    fn export_is_deterministic() {
        assert_eq!(export(&divider()), export(&divider()));
    }
}
