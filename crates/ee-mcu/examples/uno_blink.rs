//! Worked example: load a real `blink.hex` onto an Arduino Uno and watch D13
//! (the on-board LED, PB5) toggle at ~1 Hz.
//!
//! Run it with:
//!
//! ```text
//! cargo run -p ee-mcu --example uno_blink
//! ```
//!
//! The firmware is the committed Intel-HEX fixture used by the integration test
//! (`crates/ee-mcu/tests/blink.hex`); see `docs/guide/emulation.md` for the
//! assembly → HEX walkthrough. The Uno is clocked down to 1 MHz here so that the
//! `25 × 10000`-iteration delay (≈ 1,000,000 CPU cycles) lands at ~1 simulated
//! second per toggle while keeping the stepped-instruction count small.

use ee_mcu::avr::Uno;

fn main() {
    // The committed, hand-assembled blink image (Intel HEX).
    let hex = include_str!("../tests/blink.hex");

    let mut uno = Uno::new();
    uno.set_clock_hz(1_000_000.0); // 1 cycle = 1 µs → ~1 s per toggle
    uno.load_firmware(hex.as_bytes())
        .expect("blink.hex is a valid Intel HEX image");

    println!("Arduino Uno blink — D13 (PB5) toggling at ~1 Hz (clock = 1 MHz)\n");

    // Step the core, recording the elapsed cycle count at each D13 edge.
    let mut edges: Vec<u64> = Vec::new();
    let mut last = uno.digital_read(13);
    let max_steps = 8_000_000;
    for _ in 0..max_steps {
        uno.step();
        let now = uno.digital_read(13);
        if now != last {
            let cycles = uno.mcu().cycles();
            let seconds = cycles as f64 / 1_000_000.0;
            println!(
                "  edge: D13 -> {:<4}  at cycle {cycles:>9}  (t = {seconds:.3} s)",
                if now { "HIGH" } else { "low" }
            );
            edges.push(cycles);
            last = now;
            if edges.len() >= 3 {
                break;
            }
        }
    }

    assert!(edges.len() >= 2, "expected at least two D13 edges");
    let period_cycles = edges[1] - edges[0];
    let seconds = period_cycles as f64 / 1_000_000.0;
    println!(
        "\nMeasured toggle interval: {period_cycles} cycles = {seconds:.3} s (≈ 1 Hz, half-period)."
    );
    assert!(
        (seconds - 1.0).abs() < 0.2,
        "toggle interval should be ~1 s, got {seconds:.3} s"
    );
}
