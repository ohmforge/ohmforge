//! Fuzz the boolean Expr parser: it must never panic on arbitrary input, only
//! return Ok or an ExprError. (CP-PLAT-M10.2.)
#![no_main]

use libfuzzer_sys::fuzz_target;
use ohmforge_digital::hdl::Expr;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = Expr::parse(s, &["a", "b", "c", "d"]);
    }
});
