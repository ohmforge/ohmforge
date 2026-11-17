//! CP-BRD-M9.1 co-sim gate: an MCU PWM pin driving a servo actuator model
//! through the [`PinIo`] bridge.
//!
//! This is the cross-engine wiring the checkpoint asks for, done in a crate that
//! legitimately links the MCU core and an `ohmforge-devices` part model — **not**
//! by adding `ohmforge-sim` to `ohmforge-mcu`. The servo is a *behavioural* model
//! ([`ohmforge_devices::modules::Servo`]: pulse width → shaft angle), so no analog
//! engine is involved; the only thing that crosses the boundary is the pin's PWM
//! drive, read back through [`PinIo`] exactly as the lockstep co-sim loop would.
//!
//! The AVR runs Timer0 in Fast-PWM on OC0A (PD6 = Arduino D6). The co-sim step
//! samples the pin's [`PinDrive::Pwm`] duty; interpreting that duty as the high
//! time of a 20 ms (50 Hz) servo frame gives the control pulse width, which the
//! servo model maps to an angle. A larger compare value → a wider pulse → a larger
//! commanded angle: the firmware pin genuinely steers the model.

use ohmforge_devices::modules::Servo;
use ohmforge_mcu::avr::Uno;
use ohmforge_mcu::PinDrive;

// Data-space register addresses (I/O address + 0x20), matching the `pwm_duty`
// example.
const DDRD: u16 = 0x2A;
const TCCR0A: u16 = 0x44;
const TCCR0B: u16 = 0x45;
const OCR0A: u16 = 0x47;

/// A standard RC servo frame: 50 Hz → a 20 ms period, in microseconds.
const SERVO_FRAME_US: f64 = 20_000.0;

/// Configure Timer0 Fast-PWM on OC0A at the given compare value, run the co-sim
/// bridge for a few steps, and return the PWM duty the pin reports through
/// [`PinIo`].
fn pwm_duty_for(compare: u8) -> f64 {
    let mut uno = Uno::new();
    // Firmware body: spin, so each co-sim step advances cycles and ticks the timer.
    uno.mcu_mut().load_program(&[0xCFFF]); // RJMP .-1
    {
        let avr = uno.mcu_mut();
        avr.write_data(DDRD, 1 << 6); // OC0A = PD6 (D6) output
        avr.write_data(OCR0A, compare); // duty = compare / 256
        avr.write_data(TCCR0A, (2 << 6) | 0x03); // COM0A = 2 (non-inverting), WGM01:00 = 11
        avr.write_data(TCCR0B, 0x01); // WGM02 = 0, prescaler /1 → Fast-PWM, TOP = 255
    }

    let mut io = uno.pin_io();
    let d6 = Uno::digital_pin_id(6).expect("D6 maps to a pin");
    // A handful of co-sim steps: the loop pushes/reads the pin bank each step.
    for _ in 0..64 {
        uno.step_with_io(&mut io);
    }
    match io.drive(d6) {
        PinDrive::Pwm { duty, .. } => duty,
        other => panic!("D6 should report a PWM drive, got {other:?}"),
    }
}

/// The servo angle commanded by a PWM duty read off the pin, treating the duty as
/// the high fraction of a 20 ms servo frame.
fn angle_from_duty(servo: &Servo, duty: f64) -> f64 {
    servo.angle(duty * SERVO_FRAME_US)
}

#[test]
fn mcu_pwm_pin_drives_the_servo_model_through_the_bridge() {
    let servo = Servo::default(); // 1000 µs → 0°, 2000 µs → 180°

    // OCR0A = 19 → duty 19/256 ≈ 7.42 % → 1484 µs high → ~87° on a 20 ms frame.
    let duty_mid = pwm_duty_for(19);
    assert!(
        (duty_mid - 19.0 / 256.0).abs() < 1e-9,
        "pin reports the exact Fast-PWM duty, got {duty_mid}"
    );
    let angle_mid = angle_from_duty(&servo, duty_mid);
    // The pin drives the model to a sane mid-range angle.
    assert!(
        (angle_mid - 87.1).abs() < 1.0,
        "a ~7.4 % duty commands roughly the mid-travel angle, got {angle_mid}"
    );
    // The angle is exactly what the servo model predicts for the bridged pulse —
    // i.e. the MCU pin is what drives the actuator.
    assert!((angle_mid - servo.angle(duty_mid * SERVO_FRAME_US)).abs() < 1e-12);

    // A smaller compare value → narrower pulse → smaller angle: the pin steers it.
    let duty_low = pwm_duty_for(13); // ≈ 5.08 % → 1016 µs → ~2.9°
    let angle_low = angle_from_duty(&servo, duty_low);
    assert!(
        angle_low < angle_mid,
        "a narrower PWM pulse commands a smaller angle ({angle_low} < {angle_mid})"
    );
    assert!(
        (0.0..=180.0).contains(&angle_low) && (0.0..=180.0).contains(&angle_mid),
        "both commanded angles are within the servo's travel"
    );
}
