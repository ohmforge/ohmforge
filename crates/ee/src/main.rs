//! `ee` — the ee-assist binary.
//!
//! At M0 this becomes the full app: it owns session/project state and runs the
//! TUI event loop wiring the shell, engines, instruments, and AI copilot
//! together. For now it's an end-to-end smoke test of the M0 compute path —
//! parse component values, build a circuit, simulate a transient, and draw the
//! result in the terminal — proving every crate links and the pieces fit.
//! See docs/ROADMAP.md.

use ee_core::graph::{Circuit, NetId, PartKind};
use ee_core::units::Unit;
use ee_core::value::Value;
use ee_sim::{Analysis, Engine};
use ee_tui::Plot;

fn main() {
    println!("ee-assist — AI-assisted EE workbench (scaffold)\n");

    // ── Build an RC low-pass by hand: a 5 V step through 1 kΩ into 1 µF. ──
    //
    //   V1(5V) ──[ R1 1k ]──┬── out
    //                       ─┴─ C1 1µ
    //                        │
    //                       gnd
    let mut circuit = Circuit::new("rc-lowpass");
    let vin = circuit.new_net();
    let out = circuit.new_net();
    circuit.add_part(
        "V1",
        PartKind::VoltageSource,
        val("5", Unit::Volt),
        vec![vin, NetId::GROUND],
    );
    circuit.add_part(
        "R1",
        PartKind::Resistor,
        val("1k", Unit::Ohm),
        vec![vin, out],
    );
    circuit.add_part(
        "C1",
        PartKind::Capacitor,
        val("1u", Unit::Farad),
        vec![out, NetId::GROUND],
    );

    println!("circuit \"{}\":", circuit.name);
    for part in circuit.parts() {
        let value = part
            .value
            .as_ref()
            .map(|v| format!("{:>9.3e} {}", v.magnitude, v.unit.symbol()))
            .unwrap_or_default();
        println!("  {:<3} {}", part.reference, value);
    }
    println!("  τ = R·C = 1 ms\n");

    // ── Simulate: transient over 5 τ, 20 µs steps. ──
    let engine = Engine::new();
    let analysis = Analysis::Transient {
        duration: 5e-3,
        step: Some(20e-6),
    };
    let signals = match engine.run(&circuit, &analysis) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("simulation failed: {e}");
            std::process::exit(1);
        }
    };

    // ── Draw v(out) in the braille scope. ──
    let out_name = format!("v(n{})", circuit.resolve(out).0);
    let Some(sig) = signals.iter().find(|s| s.name == out_name) else {
        eprintln!("no signal for the output node");
        std::process::exit(1);
    };
    let Some(y) = sig.real() else { return };

    // Time axis in milliseconds so the labels read nicely.
    let x_ms: Vec<f64> = sig.x.iter().map(|t| t * 1e3).collect();
    let plot = Plot::new("v(out): RC step response")
        .units("V", "ms")
        .size(72, 16);
    println!("{}", plot.render(&x_ms, y));

    // A couple of measured points against the textbook RC curve.
    let sample_at = |t_target: f64| -> f64 {
        let i = (0..sig.x.len())
            .min_by(|&a, &b| {
                let (da, db) = ((sig.x[a] - t_target).abs(), (sig.x[b] - t_target).abs());
                da.total_cmp(&db)
            })
            .unwrap_or(0);
        y[i]
    };
    println!(
        "\n  v(out) @ 1τ (1 ms) = {:.3} V   (textbook 63.2% → 3.161 V)",
        sample_at(1e-3)
    );
    println!(
        "  v(out) @ 5τ (5 ms) = {:.3} V   (textbook 99.3% → 4.966 V)",
        sample_at(5e-3)
    );
    println!(
        "\ntype a circuit → simulate → see it. next: the interactive shell (docs/ROADMAP.md)."
    );
}

/// Parse an engineering-notation value, e.g. `"1k"` → 1000 Ω.
fn val(text: &str, unit: Unit) -> Option<Value> {
    match Value::parse(text, unit) {
        Ok(v) => Some(v),
        Err(e) => {
            eprintln!("bad value {text:?}: {e:?}");
            None
        }
    }
}
