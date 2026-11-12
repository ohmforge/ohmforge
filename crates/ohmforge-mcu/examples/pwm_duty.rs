//! Worked example: Timer0 Fast-PWM on OC0A (PD6, the Arduino Uno's D6) and the
//! measured duty cycle on the pin.
//!
//! Run it with:
//!
//! ```text
//! cargo run -p ohmforge-mcu --example pwm_duty
//! ```
//!
//! The registers are written through the core's data space — exactly what an
//! `OUT`/`STS` from firmware does — then a spin loop advances the CPU cycle
//! counter, which drives the timer. We sample the OC0A pin (PD6) over several
//! PWM periods and report the fraction of the time it was high. With
//! `OCR0A = 64` and `TOP = 255` the non-inverting duty is `64 / 256 = 25 %`.

use ohmforge_mcu::avr::{Port, Uno};

// Data-space register addresses (I/O address + 0x20).
const DDRD: u16 = 0x2A;
const TCCR0A: u16 = 0x44;
const TCCR0B: u16 = 0x45;
const OCR0A: u16 = 0x47;

fn main() {
    let mut uno = Uno::new();
    // Firmware body: just spin, so stepping advances cycles and ticks the timer.
    //   RJMP .-1  (0xCFFF)
    uno.mcu_mut().load_program(&[0xCFFF]);
    let avr = uno.mcu_mut();

    // Configure Fast-PWM (WGM = 3), non-inverting on OC0A (COM0A1 = 1),
    // prescaler /1, 25 % duty (OCR0A = 64, TOP = 255), PD6 as output.
    avr.write_data(DDRD, 1 << 6); // OC0A = PD6 output
    avr.write_data(OCR0A, 64); // compare value → 25 % duty
    avr.write_data(TCCR0A, (2 << 6) | 0x03); // COM0A = 2 (non-inverting), WGM01:00 = 11
    avr.write_data(TCCR0B, 0x01); // WGM02 = 0, CS = /1

    println!("Timer0 Fast-PWM on OC0A (PD6): OCR0A = 64, TOP = 255 → expect 25 % duty\n");

    // Sample the pin once per CPU cycle across four full 256-count PWM periods.
    let periods = 4u32;
    let samples = periods * 256;
    let mut high = 0u32;
    let start = uno.mcu().cycles();
    while (uno.mcu().cycles() - start) < samples as u64 {
        uno.step(); // RJMP self = 2 cycles; the timer ticks each step
        if uno.mcu().pin(Port::D, 6) {
            high += 1;
        }
    }

    // `step` advances two cycles at a time, so we took ~samples/2 samples.
    let taken = (uno.mcu().cycles() - start) / 2;
    let duty = high as f64 / taken as f64;
    println!("  sampled {taken} points over {periods} PWM periods");
    println!("  pin high fraction (measured duty): {:.1} %", duty * 100.0);
    println!(
        "  reported duty via pin_drive:       {:.1} %",
        pwm_duty_from_drive(&uno) * 100.0
    );

    assert!(
        (duty - 0.25).abs() < 0.03,
        "measured duty should be ~25 %, got {:.3}",
        duty
    );
}

/// Read the duty the peripheral reports through the co-sim `PinDrive` surface.
fn pwm_duty_from_drive(uno: &Uno) -> f64 {
    match uno.mcu().peripherals().pin_drive(Port::D, 6) {
        ohmforge_mcu::PinDrive::Pwm { duty, .. } => duty,
        _ => 0.0,
    }
}
