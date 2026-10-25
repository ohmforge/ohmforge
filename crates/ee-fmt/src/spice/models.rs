//! A tiny built-in catalog of real vendor parts and their SPICE `.model`
//! parameters (`CP-AF-M4.1`), resolvable by name.
//!
//! When a SPICE deck references a model name that no `.model` card in the deck
//! defines, [`crate::spice::import`] falls back to this catalog, so a bare
//! `D1 a b 1N4148` imports as a 1N4148-flavoured diode with no `.model` line in
//! sight. Lookup is case-insensitive.
//!
//! ## Parameter sources
//!
//! The values are representative, datasheet-/vendor-SPICE-flavoured
//! *approximations* — enough to make a part behave recognizably (a small-signal
//! switching diode vs. a power rectifier; a high-β small-signal NPN), not a
//! substitute for the manufacturer's qualified model. Each entry notes where its
//! numbers come from. Only the parameters the engine can apply are consumed on
//! import (diode `IS`; BJT `IS`/`BF`/`BR`; MOSFET `VTO`/`KP`/`LAMBDA`); the rest
//! are carried for fidelity and surfaced as [`crate::spice::Note`]s.
//!
//! | part   | type | role                               | source basis                         |
//! |--------|------|------------------------------------|---------------------------------------|
//! | 1N4148 | D    | small-signal Si switching diode    | common Vishay/Philips SPICE model     |
//! | 1N4007 | D    | 1 kV, 1 A general-purpose rectifier | common rectifier SPICE model          |
//! | 2N3904 | NPN  | small-signal NPN (β ≈ 300)         | common ON-Semi/Philips SPICE model    |
//! | 2N2222 | NPN  | small-signal switching NPN         | common 2N2222A SPICE model            |
//! | 2N7000 | NMOS | small-signal logic-level MOSFET    | datasheet-derived level-1 approximation |

use super::{ModelType, SpiceModel};

/// One catalog entry: a name, a device type, and its parameters as `&'static`
/// data (SPICE parameter names, base-SI magnitudes), so the whole catalog is a
/// `const` with no allocation or lazy initialization.
struct Entry {
    name: &'static str,
    model_type: ModelType,
    params: &'static [(&'static str, f64)],
}

/// The built-in catalog. Order is stable so [`names`] is deterministic.
const CATALOG: &[Entry] = &[
    // 1N4148 — the classic small-signal silicon switching diode. A tiny IS and a
    // soft emission coefficient give the familiar ~0.6–0.7 V drop at a few mA.
    Entry {
        name: "1N4148",
        model_type: ModelType::Diode,
        params: &[
            ("IS", 2.52e-9),
            ("N", 1.752),
            ("RS", 0.568),
            ("CJO", 4.0e-12),
            ("BV", 100.0),
            ("IBV", 1.0e-4),
        ],
    },
    // 1N4007 — a 1 kV / 1 A general-purpose rectifier: a larger junction (bigger
    // IS and capacitance) and a high reverse-breakdown voltage.
    Entry {
        name: "1N4007",
        model_type: ModelType::Diode,
        params: &[
            ("IS", 7.02e-9),
            ("N", 1.8),
            ("RS", 0.0341),
            ("CJO", 2.6e-11),
            ("BV", 1000.0),
            ("IBV", 5.0e-6),
        ],
    },
    // 2N3904 — a ubiquitous small-signal NPN with a high forward β.
    Entry {
        name: "2N3904",
        model_type: ModelType::Npn,
        params: &[
            ("IS", 6.734e-15),
            ("BF", 416.4),
            ("BR", 0.7371),
            ("VAF", 74.03),
            ("RB", 10.0),
            ("RC", 1.0),
        ],
    },
    // 2N2222 — the classic switching/amplifier NPN (2N2222A-class parameters).
    Entry {
        name: "2N2222",
        model_type: ModelType::Npn,
        params: &[
            ("IS", 1.434e-14),
            ("BF", 255.9),
            ("BR", 6.092),
            ("VAF", 74.03),
            ("RB", 10.0),
            ("RC", 1.0),
        ],
    },
    // 2N7000 — a small-signal logic-level N-channel MOSFET. A level-1 square-law
    // approximation: ~2.1 V threshold, a modest transconductance, mild channel-
    // length modulation. (The engine ties the body to the source.)
    Entry {
        name: "2N7000",
        model_type: ModelType::Nmos,
        params: &[("VTO", 2.1), ("KP", 0.12), ("LAMBDA", 0.01), ("GAMMA", 0.0)],
    },
];

/// Look up a vendor part by name (case-insensitive), building an owned
/// [`SpiceModel`] from the catalog entry. Returns `None` for an unknown name.
///
/// ```
/// # use ee_fmt::spice::{models, ModelType};
/// let d = models::lookup("1n4148").expect("1N4148 is catalogued");
/// assert_eq!(d.model_type, ModelType::Diode);
/// assert!((d.param("IS").unwrap() - 2.52e-9).abs() < 1e-18);
/// assert!(models::lookup("nope").is_none());
/// ```
pub fn lookup(name: &str) -> Option<SpiceModel> {
    CATALOG
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case(name))
        .map(|e| SpiceModel {
            name: e.name.to_string(),
            model_type: e.model_type,
            params: e.params.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        })
}

/// Every catalogued part name, in a stable order — handy for listing what the
/// built-in library can resolve.
pub fn names() -> impl Iterator<Item = &'static str> {
    CATALOG.iter().map(|e| e.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_the_catalogued_parts_case_insensitively() {
        for name in ["1N4148", "1N4007", "2N3904", "2N2222", "2N7000"] {
            assert!(lookup(name).is_some(), "{name} should be catalogued");
            assert!(
                lookup(&name.to_ascii_lowercase()).is_some(),
                "{name} lookup is case-insensitive"
            );
        }
        assert!(
            lookup("TL072").is_none(),
            "uncatalogued part resolves to None"
        );
    }

    #[test]
    fn catalogued_types_and_key_params_are_right() {
        let d = lookup("1N4148").unwrap();
        assert_eq!(d.model_type, ModelType::Diode);
        assert!((d.param("IS").unwrap() - 2.52e-9).abs() < 1e-18);

        let q = lookup("2N3904").unwrap();
        assert_eq!(q.model_type, ModelType::Npn);
        assert!((q.param("BF").unwrap() - 416.4).abs() < 1e-9);

        let m = lookup("2N7000").unwrap();
        assert_eq!(m.model_type, ModelType::Nmos);
        assert!((m.param("VTO").unwrap() - 2.1).abs() < 1e-9);
    }

    #[test]
    fn names_lists_every_entry() {
        let listed: Vec<&str> = names().collect();
        assert_eq!(listed.len(), CATALOG.len());
        assert!(listed.contains(&"2N2222"));
    }
}
