//! `ee` — the ee-assist binary.
//!
//! At M0 this becomes the full app: it owns session/project state and runs the
//! TUI event loop wiring the shell, engines, instruments, and AI copilot
//! together. For now it's an end-to-end smoke test of the M0 compute path —
//! parse a netlist, build the circuit, simulate a transient, and draw the
//! result in the terminal — proving every crate links and the pieces fit.
//! See docs/ROADMAP.md.

use ee_fmt::netlist;
use ee_sim::{Analysis, Engine};
use ee_tui::Plot;

/// The circuit, as you would type it. An RC low-pass: a 5 V step through 1 kΩ
/// into 1 µF, so τ = R·C = 1 ms.
const NETLIST: &str = "\
.title rc-lowpass
V1 in  gnd 5
R1 in  out 1k
C1 out gnd 1u
";

fn main() {
    println!("ee-assist — AI-assisted EE workbench (scaffold)\n");
    println!("netlist:");
    for line in NETLIST.lines() {
        println!("  {line}");
    }
    println!();

    // ── Parse: text → circuit. ──
    let circuit = match netlist::parse(NETLIST) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("netlist error, {e}");
            std::process::exit(1);
        }
    };

    // ── Simulate: transient over 5 τ, 20 µs steps. ──
    let analysis = Analysis::Transient {
        duration: 5e-3,
        step: Some(20e-6),
    };
    let signals = match Engine::new().run(&circuit, &analysis) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("simulation failed: {e}");
            std::process::exit(1);
        }
    };

    // ── Draw v(out) in the braille scope. `out` is R1's second terminal. ──
    let out = circuit.part_by_ref("R1").expect("R1 exists").terminals[1];
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
