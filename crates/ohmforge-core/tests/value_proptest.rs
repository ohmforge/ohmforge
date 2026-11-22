//! CP-PLAT-M10.2 — property tests for the engineering-notation value parser
//! ([`Value::parse`]), the hot text path hit on every circuit load and shell
//! edit. Invariants asserted:
//!
//! - **Never panics** on arbitrary UTF-8, or on structured engineering-notation
//!   tokens that reach the SI-prefix / embedded-decimal / unit-suffix branches.
//! - **Round-trips where defined:** a finite decimal rendered with `Display`
//!   re-parses to the same magnitude (the plain-number path, end to end).
//!
//! `proptest` is a dev-dependency only, so `ohmforge-core` stays dependency-free.

use ohmforge_core::{Unit, Value};
use proptest::prelude::*;

fn any_unit() -> impl Strategy<Value = Unit> {
    prop_oneof![
        Just(Unit::Volt),
        Just(Unit::Ampere),
        Just(Unit::Ohm),
        Just(Unit::Farad),
        Just(Unit::Henry),
        Just(Unit::Hertz),
        Just(Unit::Second),
        Just(Unit::Watt),
        Just(Unit::None),
    ]
}

proptest! {
    // Arbitrary text must never panic — it may only return Ok or a ParseError.
    #[test]
    fn parse_never_panics(s in "(?s).{0,64}", unit in any_unit()) {
        let _ = Value::parse(&s, unit);
    }

    // Structured tokens that deliberately exercise sign / SI prefix (`p`..`T`,
    // `meg`) / embedded-decimal (`4k7`) / stripped-unit-suffix branches — still
    // only asserting "no panic".
    #[test]
    fn parse_eng_tokens_never_panic(
        s in "[-+]?[0-9]{0,4}(meg|[pnumµkKMGTRr])?[0-9]{0,4}(Hz|Ohm|ohm|Ω|[VAFHsW])?",
        unit in any_unit(),
    ) {
        let _ = Value::parse(&s, unit);
    }

    // Round-trip (where defined): Rust's `Display` for f64 never uses exponent
    // form and round-trips exactly, so a plain decimal must parse back to the
    // same magnitude via the no-prefix branch.
    #[test]
    fn plain_number_roundtrips(x in -1e12f64..1e12f64, unit in any_unit()) {
        let s = format!("{x}");
        let parsed = Value::parse(&s, unit).expect("a plain decimal must parse");
        prop_assert!(
            (parsed.magnitude - x).abs() <= 1e-9 * x.abs().max(1.0),
            "parsed {} != {}", parsed.magnitude, x
        );
        prop_assert_eq!(parsed.unit, unit);
    }
}
