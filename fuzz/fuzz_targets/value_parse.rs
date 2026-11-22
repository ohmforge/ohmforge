//! Fuzz the engineering-notation value parser: it must never panic on arbitrary
//! bytes, only return Ok or a ParseError. (CP-PLAT-M10.2.)
#![no_main]

use libfuzzer_sys::fuzz_target;
use ohmforge_core::{Unit, Value};

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // A few units so the prefix/suffix stripping sees different symbols.
        for unit in [Unit::Ohm, Unit::Farad, Unit::Volt, Unit::None] {
            let _ = Value::parse(s, unit);
        }
    }
});
