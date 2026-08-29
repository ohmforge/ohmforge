//! `ee` — the ee-assist binary.
//!
//! At M0 this becomes the full app: it owns session/project state and runs the
//! TUI event loop wiring the shell, engines, instruments, and AI copilot
//! together. For now it's a smoke-test entry point proving the workspace builds
//! and the crates link. See docs/ROADMAP.md.

use ee_core::{units::Unit, value::Value};

fn main() {
    println!("ee-assist — AI-assisted EE workbench (scaffold)");
    println!("See docs/ROADMAP.md for what lands next.\n");

    // Prove the core links and the engineering-notation parser works.
    let demo = [
        ("4k7", Unit::Ohm),
        ("100n", Unit::Farad),
        ("1meg", Unit::Ohm),
    ];
    for (text, unit) in demo {
        match Value::parse(text, unit) {
            Ok(v) => println!("  parsed {text:>5}  = {:e} {}", v.magnitude, unit.symbol()),
            Err(e) => println!("  parse error on {text:?}: {e:?}"),
        }
    }
}
