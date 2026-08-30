//! `cargo run -p ee-tui --example scope`
//!
//! The "first pixels" demo: braille waveform plots rendered to the terminal,
//! ahead of the solver and the event loop. The signals here come from a tiny
//! stub generator (a sine and an RC charge curve) — exactly the stand-in the
//! `ui-shell` track uses until `ee-sim` produces real `Signal`s. See the
//! `CP-UX-M0.3` gate in docs/plan/CHECKPOINTS.md.

use ee_tui::Plot;

fn main() {
    // --- stub signal 1: a 5 V, 2-cycle sine over 4 ms --------------------
    let n = 400;
    let t_end_ms = 4.0;
    let freq_hz = 500.0;
    let mut t = Vec::with_capacity(n);
    let mut sine = Vec::with_capacity(n);
    for i in 0..n {
        let ms = t_end_ms * i as f64 / (n - 1) as f64;
        t.push(ms);
        sine.push(5.0 * (2.0 * std::f64::consts::PI * freq_hz * (ms / 1000.0)).sin());
    }

    // --- stub signal 2: RC charge, 3.3 V rail, tau = 1 ms ----------------
    // v(t) = V*(1 - e^(-t/RC)) — the same curve CP-SIM-M0.4 will produce for real.
    let vcc = 3.3;
    let tau_ms = 1.0;
    let rc: Vec<f64> = t
        .iter()
        .map(|&ms| vcc * (1.0 - (-ms / tau_ms).exp()))
        .collect();

    println!();
    println!("  ee-assist — braille scope demo (stub signals, no solver yet)\n");

    print!(
        "{}",
        Plot::new("v(sine)  — 5 V @ 500 Hz")
            .size(72, 14)
            .units("V", "ms")
            .render(&t, &sine)
    );
    println!();
    print!(
        "{}",
        Plot::new("v(out)   — RC charge, Vcc=3.3 V, \u{3c4}=1 ms")
            .size(72, 12)
            .units("V", "ms")
            .render(&t, &rc)
    );
    println!();
    println!("  Both curves are drawn with Unicode braille (2\u{d7}4 dots/cell).");
    println!("  Next: CP-SIM-M0.4 feeds a real transient into this same widget.");
}
