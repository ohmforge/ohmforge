//! CP-PLAT-M10.2 — property tests for the text importers. Invariants asserted:
//!
//! - The **netlist** and **SPICE** importers **never panic** on arbitrary
//!   (multi-line) UTF-8 — they return `Ok` or a typed error.
//! - **Round-trip where defined:** a programmatically-built R/V circuit survives
//!   `spice::export` → `spice::import` with its part count and per-kind structure
//!   intact (`import ∘ export`).
//!
//! `proptest` is a dev-dependency only, so `ohmforge-fmt` stays dependency-free.

use ohmforge_core::graph::{Circuit, NetId, PartKind};
use ohmforge_core::{Unit, Value};
use ohmforge_fmt::{export_spice, import_spice, parse_netlist};
use proptest::prelude::*;

/// Build a small, structurally-valid R/V ladder: a 10 V-ish source at node 0,
/// then `rvals.len()` series resistors chaining fresh nodes to ground.
fn ladder(source_v: f64, rvals: &[f64]) -> Circuit {
    let mut c = Circuit::new("rt");
    let n0 = c.new_net();
    c.add_part(
        "V1",
        PartKind::VoltageSource,
        Some(Value::new(source_v, Unit::Volt)),
        vec![n0, NetId::GROUND],
    );
    let mut prev = n0;
    for (i, &r) in rvals.iter().enumerate() {
        let node = c.new_net();
        c.add_part(
            format!("R{i}"),
            PartKind::Resistor,
            Some(Value::new(r, Unit::Ohm)),
            vec![prev, node],
        );
        prev = node;
    }
    // A shunt resistor to ground so the last node is terminated.
    c.add_part(
        "Rterm",
        PartKind::Resistor,
        Some(Value::new(1_000.0, Unit::Ohm)),
        vec![prev, NetId::GROUND],
    );
    c
}

fn kind_tag(k: &PartKind) -> u8 {
    match k {
        PartKind::Resistor => 0,
        PartKind::VoltageSource => 1,
        _ => 2,
    }
}

proptest! {
    // Arbitrary multi-line text must never panic the native netlist importer.
    #[test]
    fn netlist_parse_never_panics(s in "(?s).{0,256}") {
        let _ = parse_netlist(&s);
    }

    // ...nor the SPICE importer.
    #[test]
    fn spice_import_never_panics(s in "(?s).{0,256}") {
        let _ = import_spice(&s);
    }

    // SPICE-shaped lines (element cards, `.model`/`.end` dot-cards, comments)
    // must never panic either.
    #[test]
    fn spice_shaped_lines_never_panic(
        lines in prop::collection::vec(
            "[*.]?[RVCLID][0-9]* ?[a-z0-9]* ?[a-z0-9]* ?[-+]?[0-9]{0,4}[kmunp]?",
            0..12,
        )
    ) {
        let deck = lines.join("\n");
        let _ = import_spice(&deck);
    }

    // Round-trip: export an R/V circuit to SPICE and re-import it; the part count
    // and per-kind structure must be preserved.
    #[test]
    fn spice_export_import_roundtrips(
        source_v in 1.0f64..24.0,
        rvals in prop::collection::vec(1.0f64..1e5, 1..6),
    ) {
        let original = ladder(source_v, &rvals);
        let deck = export_spice(&original);
        let reimported = import_spice(&deck)
            .unwrap_or_else(|err| panic!("exported deck failed to re-import: {err}\n{deck}"));

        prop_assert_eq!(
            reimported.parts().len(),
            original.parts().len(),
            "part count changed across round-trip\n{}", deck
        );

        let mut want: Vec<u8> = original.parts().iter().map(|p| kind_tag(&p.kind)).collect();
        let mut got: Vec<u8> = reimported.parts().iter().map(|p| kind_tag(&p.kind)).collect();
        want.sort_unstable();
        got.sort_unstable();
        prop_assert_eq!(want, got, "per-kind structure changed across round-trip\n{}", deck);
    }
}
