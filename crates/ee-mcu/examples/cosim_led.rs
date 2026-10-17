//! Worked example: the co-simulation bridge — real AVR firmware drives a pin,
//! and the mixed-signal boundary turns that drive into a measurable result in a
//! simulated LED circuit.
//!
//! Run it with:
//!
//! ```text
//! cargo run -p ee-mcu --example cosim_led
//! ```
//!
//! This mirrors the `ee cosim` demo in the `ee` binary. The binary wires the pin
//! to the full nonlinear analog solver (`ee-sim`); here — because `ee-mcu` must
//! not depend on `ee-sim` — we keep the whole example inside `ee-mcu` by solving
//! a deliberately simple load by hand through the [`boundary`](ee_mcu::boundary)
//! module. The structure is the real lockstep loop:
//!
//! 1. sample the circuit's node voltages into the core (`sample_inputs`),
//! 2. run the core (one instruction here, via `step_with_io`),
//! 3. publish the pin's drive back out,
//! 4. turn the drive into a [`Thevenin`] source and solve the circuit,
//! 5. feed the solved node voltage back in on the next iteration.
//!
//! The circuit is the Uno's D13 pin → a 220 Ω series resistor → an LED (modelled
//! here as a constant 2.0 V forward drop while conducting) → ground.

use ee_mcu::avr::Uno;
use ee_mcu::boundary::OutputDrive;
use ee_mcu::PinDrive;

/// Series resistor between the pin and the LED, in ohms.
const R_SERIES: f64 = 220.0;
/// Approximate LED forward drop while conducting, in volts.
const LED_VF: f64 = 2.0;

fn main() {
    // Tiny firmware: make D13 (PB5) an output and drive it high, then spin.
    //   SBI DDRB,5 (0x9A25) · SBI PORTB,5 (0x9A2D) · RJMP .-1 (0xCFFF)
    // Raw little-endian flash bytes (low byte first per 16-bit word).
    let firmware: [u8; 6] = [0x25, 0x9A, 0x2D, 0x9A, 0xFF, 0xCF];

    let mut uno = Uno::new();
    uno.load_firmware(&firmware).expect("firmware loads");

    // A realistic (non-ideal) output driver: 5 V rail behind 40 Ω of source
    // resistance, so the delivered voltage sags a little under load.
    uno.mcu_mut()
        .peripherals_mut()
        .set_output_drive(OutputDrive::push_pull(5.0, 40.0));

    let d13 = Uno::digital_pin_id(13).expect("D13 exists");
    let mut io = uno.pin_io();

    println!("Co-simulation: AVR firmware drives D13 → a simulated LED.");
    println!(
        "  driver: 5 V push-pull, 40 Ω source R;  load: {R_SERIES:.0} Ω + LED ({LED_VF:.1} V)\n"
    );

    for step in 1..=4 {
        // One lockstep iteration: sample inputs, run an instruction, publish drive.
        uno.step_with_io(&mut io);
        let drive = io.drive(d13);

        // Solve the little resistive circuit for this step's pin drive.
        let (state, v_pin, i_ma) = match drive {
            PinDrive::Digital(true) | PinDrive::Pwm { level: true, .. } => {
                // The pin is a Thévenin source (v_oc, r_src). Current flows when
                // the open-circuit voltage clears the LED drop:
                //   I = (v_oc − Vf) / (r_src + R_series)
                //   v(d13) = v_oc − I·r_src
                let th = uno
                    .mcu()
                    .peripherals()
                    .pin_thevenin(ee_mcu::avr::Port::B, 5)
                    .expect("a driven pin presents a source");
                let i = ((th.v_oc - LED_VF) / (th.r + R_SERIES)).max(0.0);
                let v_pin = th.v_oc - i * th.r;
                ("HIGH → LED lit", v_pin, i * 1e3)
            }
            _ => ("low  → LED off", 0.0, 0.0),
        };

        println!("  step {step}: D13 {state}   v(d13) = {v_pin:.2} V, I(LED) = {i_ma:.1} mA");

        if i_ma > 0.0 {
            println!(
                "\n  digitalWrite(13, HIGH) in firmware → {i_ma:.1} mA through the simulated LED."
            );
            // Sanity: with 5 V, 40 Ω source, 220 Ω series and a 2 V LED,
            // I = (5 − 2) / 260 ≈ 11.5 mA.
            assert!(
                (i_ma - 11.5).abs() < 0.5,
                "expected ~11.5 mA, got {i_ma:.1} mA"
            );
            return;
        }
    }
}
