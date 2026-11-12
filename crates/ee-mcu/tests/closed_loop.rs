//! CP-BRD-M6.1 gate: closed-loop MCU↔analog co-sim on the AVR.
//!
//! Each test plays the co-sim loop's half by hand through the engine-agnostic
//! [`PinIo`] exchange — exactly where the `ee` binary's lockstep loop would push
//! a solved node voltage in and read a pin drive back. The analog value only
//! ever crosses as a plain number (volts on a pin), so `ee-mcu` stays free of any
//! `ee-sim` dependency. A companion host test that wires a *real* `ee-sim`
//! divider node to this same surface lives in `crates/ee/tests/` (the one crate
//! that legitimately links both engines).
//!
//! 1. `adc_threshold_drives_a_gpio_*` — firmware starts an ADC conversion on A0,
//!    converts the injected analog voltage, compares it against a mid-scale
//!    threshold, and drives D8 HIGH above it / LOW below it. This closes the
//!    loop: analog node → ADC → firmware decision → GPIO.
//! 2. `rising_edge_fires_int0_isr` / `pin_change_isr_fires_on_either_edge` — an
//!    edge on an input pin vectors through the interrupt controller to a handler
//!    that runs (observably incrementing a register) and `RETI`s.

use ee_mcu::avr::{pin_id, Port, Uno};
use ee_mcu::{PinDrive, PinIo};

/// Firmware: continuously convert A0 (ADC channel 0) and mirror a mid-scale
/// threshold onto D8 (PB0) — HIGH when the 10-bit code reaches 512 (≈ half
/// reference), LOW below it.
///
/// ```text
///  0: SBI  DDRB,0          ; PB0 (D8) output                 0x9A20
/// loop(1):
///  1: LDI  r16, ADEN|ADSC  ; enable + start, prescaler /2     0xEC00
///  2: STS  ADCSRA, r16     ; (0x007A)                         0x9300
/// wait(4):
///  4: LDS  r16, ADCSRA     ; (0x007A)                         0x9100
///  6: SBRC r16, ADSC(6)    ; skip the jump once ADSC clears    0xFD06
///  7: RJMP wait            ; (-4)                              0xCFFC
///  8: LDS  r17, ADCH       ; (0x0079) high byte of the code    0x9110
/// 10: CPI  r17, 2          ; code >= 512  ⇔  ADCH >= 2         0x3012
/// 11: BRLO low             ; (+2) carry set ⇒ below threshold  0xF010
/// 12: SBI  PORTB,0         ; D8 HIGH                           0x9A28
/// 13: RJMP loop            ; (-13)                             0xCFF3
/// low(14):
/// 14: CBI  PORTB,0         ; D8 LOW                            0x9828
/// 15: RJMP loop            ; (-15)                             0xCFF1
/// ```
const ADC_THRESHOLD_FW: &[u16] = &[
    0x9A20, // 0  SBI DDRB,0
    0xEC00, // 1  LDI r16, ADEN|ADSC
    0x9300, 0x007A, // 2  STS ADCSRA, r16
    0x9100, 0x007A, // 4  LDS r16, ADCSRA
    0xFD06, // 6  SBRC r16,6
    0xCFFC, // 7  RJMP wait
    0x9110, 0x0079, // 8  LDS r17, ADCH
    0x3012, // 10 CPI r17,2
    0xF010, // 11 BRLO low
    0x9A28, // 12 SBI PORTB,0
    0xCFF3, // 13 RJMP loop
    0x9828, // 14 CBI PORTB,0
    0xCFF1, // 15 RJMP loop
];

/// Drive A0 at `volts`, run the firmware loop for `steps` instructions
/// (re-sampling the injected voltage every instruction), and report D8's drive.
fn run_adc_loop(volts: f64, steps: usize) -> PinDrive {
    let mut uno = Uno::new();
    uno.mcu_mut().load_program(ADC_THRESHOLD_FW);
    let mut io = uno.pin_io();
    io.set_input(Uno::analog_pin_id(0).unwrap(), volts);
    for _ in 0..steps {
        uno.step_with_io(&mut io);
    }
    io.drive(Uno::digital_pin_id(8).unwrap())
}

#[test]
fn adc_threshold_drives_a_gpio_high_above_the_threshold() {
    // 3.0 V on A0 → code 614 (ADCH = 2) → above the 2.5 V / code-512 threshold.
    assert_eq!(
        run_adc_loop(3.0, 400),
        PinDrive::Digital(true),
        "A0 above threshold → firmware drives D8 HIGH"
    );
}

#[test]
fn adc_threshold_drives_a_gpio_low_below_the_threshold() {
    // 1.0 V on A0 → code 204 (ADCH = 0) → below the threshold.
    assert_eq!(
        run_adc_loop(1.0, 400),
        PinDrive::Digital(false),
        "A0 below threshold → firmware drives D8 LOW"
    );
}

#[test]
fn adc_threshold_tracks_a_moving_analog_node() {
    // The same loop, one running instance, tracking a node that crosses the
    // threshold in both directions — the closed loop re-deciding each pass.
    let mut uno = Uno::new();
    uno.mcu_mut().load_program(ADC_THRESHOLD_FW);
    let mut io = uno.pin_io();
    let a0 = Uno::analog_pin_id(0).unwrap();
    let d8 = Uno::digital_pin_id(8).unwrap();

    let settle = |uno: &mut Uno, io: &mut PinIo| {
        for _ in 0..400 {
            uno.step_with_io(io);
        }
    };

    io.set_input(a0, 4.5); // well above
    settle(&mut uno, &mut io);
    assert_eq!(io.drive(d8), PinDrive::Digital(true));

    io.set_input(a0, 0.5); // well below
    settle(&mut uno, &mut io);
    assert_eq!(io.drive(d8), PinDrive::Digital(false));

    io.set_input(a0, 2.6); // just above half reference
    settle(&mut uno, &mut io);
    assert_eq!(io.drive(d8), PinDrive::Digital(true));
}

/// Firmware with an `INT0` (PD2) rising-edge handler that increments r20.
///
/// ```text
///  0: RJMP main           ; (+3)                              0xC003
///  1: NOP                                                     0x0000
/// INT0 vector (word 2):
///  2: INC  r20                                                0x9543
///  3: RETI                                                    0x9518
/// main(4):
///  4: SEI                                                     0x9478
///  5: LDI  r16, 0x03      ; ISC01:00 = 11 → rising edge       0xE003
///  6: STS  EICRA, r16     ; (0x0069)                          0x9300
///  8: SBI  EIMSK, 0       ; enable INT0 (I/O 0x1D)            0x9AE8
/// spin(9):
///  9: RJMP spin                                               0xCFFF
/// ```
const INT0_FW: &[u16] = &[
    0xC003, // 0 RJMP main
    0x0000, // 1 NOP
    0x9543, // 2 INC r20   (INT0 vector)
    0x9518, // 3 RETI
    0x9478, // 4 SEI
    0xE003, // 5 LDI r16,0x03
    0x9300, 0x0069, // 6 STS EICRA, r16
    0x9AE8, // 8 SBI EIMSK,0
    0xCFFF, // 9 RJMP spin
];

#[test]
fn rising_edge_fires_int0_isr() {
    let mut uno = Uno::new();
    uno.mcu_mut().load_program(INT0_FW);
    let mut io = uno.pin_io();
    let d2 = Uno::digital_pin_id(2).unwrap(); // PD2 = INT0

    // Run the setup; PD2 starts low. No edge, no interrupt.
    io.set_input(d2, 0.0);
    for _ in 0..16 {
        uno.step_with_io(&mut io);
    }
    assert_eq!(uno.mcu().reg(20), 0, "no edge yet → ISR never ran");

    // A rising edge on PD2 vectors to the handler exactly once.
    io.set_input(d2, 5.0);
    for _ in 0..16 {
        uno.step_with_io(&mut io);
    }
    assert_eq!(uno.mcu().reg(20), 1, "rising edge ran the INT0 ISR once");

    // A falling edge must not fire a rising-edge interrupt.
    io.set_input(d2, 0.0);
    for _ in 0..16 {
        uno.step_with_io(&mut io);
    }
    assert_eq!(uno.mcu().reg(20), 1, "falling edge does not fire");

    // A second rising edge fires again.
    io.set_input(d2, 5.0);
    for _ in 0..16 {
        uno.step_with_io(&mut io);
    }
    assert_eq!(uno.mcu().reg(20), 2, "second rising edge ran the ISR again");
}

/// Firmware with a `PCINT2` (Port D bank) handler on PD4 that increments r21.
///
/// ```text
///  0: RJMP main           ; (+11)                             0xC00B
///  1..9: NOP fillers                                          0x0000
/// PCINT2 vector (word 10):
/// 10: INC  r21                                                0x9553
/// 11: RETI                                                    0x9518
/// main(12):
/// 12: SEI                                                     0x9478
/// 13: LDI  r16, 0x10      ; PCINT20 mask (PD4)                0xE100
/// 14: STS  PCMSK2, r16    ; (0x006D)                          0x9300
/// 16: LDI  r16, 0x04      ; PCIE2                             0xE004
/// 17: STS  PCICR, r16     ; (0x0068)                          0x9300
/// spin(19):
/// 19: RJMP spin                                               0xCFFF
/// ```
const PCINT_FW: &[u16] = &[
    0xC00B, // 0  RJMP main
    0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, // 1..9 NOP
    0x9553, // 10 INC r21   (PCINT2 vector)
    0x9518, // 11 RETI
    0x9478, // 12 SEI
    0xE100, // 13 LDI r16,0x10
    0x9300, 0x006D, // 14 STS PCMSK2, r16
    0xE004, // 16 LDI r16,0x04
    0x9300, 0x0068, // 17 STS PCICR, r16
    0xCFFF, // 19 RJMP spin
];

#[test]
fn pin_change_isr_fires_on_either_edge() {
    let mut uno = Uno::new();
    uno.mcu_mut().load_program(PCINT_FW);
    let mut io = uno.pin_io();
    let d4 = pin_id(Port::D, 4); // PCINT20

    io.set_input(d4, 0.0);
    for _ in 0..24 {
        uno.step_with_io(&mut io);
    }
    assert_eq!(uno.mcu().reg(21), 0, "no edge yet");

    // Pin-change fires on *any* edge — rising…
    io.set_input(d4, 5.0);
    for _ in 0..16 {
        uno.step_with_io(&mut io);
    }
    assert_eq!(uno.mcu().reg(21), 1, "rising edge ran the PCINT2 ISR");

    // …and falling.
    io.set_input(d4, 0.0);
    for _ in 0..16 {
        uno.step_with_io(&mut io);
    }
    assert_eq!(uno.mcu().reg(21), 2, "falling edge ran it again");
}
