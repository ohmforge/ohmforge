//! Fuzz the native netlist importer: it must never panic on arbitrary input,
//! only return Ok or a NetlistError. (CP-PLAT-M10.2.)
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = ohmforge_fmt::parse_netlist(s);
    }
});
