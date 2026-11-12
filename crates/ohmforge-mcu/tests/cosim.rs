//! (B) CP-BRD-M2.1 gate: exercise the co-simulation **bridge surface** — the
//! [`PinIo`] exchange and the [`McuCore`] co-sim methods — without any analog
//! engine. Each test plays the co-sim loop's half by hand: set input voltages,
//! step the core to a cycle bound, then read the pin drives it asserted.
//!
//! The real lockstep loop lives in the `of` binary; here we only prove the
//! surface it drives behaves. See `docs/plan/track-boards.md` § "Co-simulation
//! bridge."

use ohmforge_mcu::avr::{pin_id, Avr, Port, Uno};
use ohmforge_mcu::{McuCore, PinDrive, PinIo};

/// A reset core holding a hand-assembled program.
fn core_with(words: &[u16]) -> Avr {
    let mut avr = Avr::new();
    avr.load_program(words);
    avr
}

#[test]
fn digital_write_drives_the_mapped_pin() {
    // SBI DDRB,5 (PB5 output) ; SBI PORTB,5 (drive it high). PB5 is Arduino D13.
    let mut avr = core_with(&[0x9A25, 0x9A2D]);
    let mut io = PinIo::new(avr.pin_count());

    // Two SBIs at 2 cycles each: step the co-sim surface past both, exchanging
    // pin state.
    avr.step_to_cycle(4, &mut io);

    assert_eq!(
        io.drive(pin_id(Port::B, 5)),
        PinDrive::Digital(true),
        "DDRB5 output + PORTB5 set → D13 drives high in PinIo"
    );
    // A pin the firmware never configured stays high-impedance.
    assert_eq!(io.drive(pin_id(Port::D, 7)), PinDrive::HighZ);
}

#[test]
fn analog_read_yields_the_expected_adc_code() {
    // Firmware: start an ADC conversion on channel 0 (A0) then spin.
    //   LDI  r16, ADEN|ADSC|ADPS0   ; 0xC1  (enable, start, prescaler /2)
    //   STS  ADCSRA(0x7A), r16
    //   RJMP self
    // ADMUX resets to 0, selecting channel 0 = A0 (Port C bit 0).
    let mut avr = core_with(&[0xEC01, 0x9300, 0x007A, 0xCFFF]);
    let mut io = PinIo::new(avr.pin_count());

    // Present half the 5 V reference on A0; the loop would get this from the
    // solved divider. 2.5 V / 5 V * 1024 = 512.
    io.set_input(Uno::analog_pin_id(0).unwrap(), 2.5);

    // LDI + STS + a 13·2-clock conversion all finish well inside 100 cycles.
    avr.step_to_cycle(100, &mut io);

    // Read the 10-bit result straight out of ADCL/ADCH (data addrs 0x78/0x79).
    let code = avr.read_data(0x78) as u16 | (avr.read_data(0x79) as u16) << 8;
    assert_eq!(code, 512, "half-reference input converts to mid-scale code");
}

#[test]
fn next_event_reports_a_configured_timer_compare() {
    let mut avr = Avr::new();
    // Idle core: nothing scheduled.
    assert_eq!(avr.next_event(), None);

    // Configure Timer0: Normal mode, prescaler /1, compare A at 100. The next
    // event the loop must not step past is that compare match, 100 cycles out.
    avr.write_data(0x47, 100); // OCR0A
    avr.write_data(0x45, 0x01); // TCCR0B: CS = /1
    assert_eq!(
        avr.next_event(),
        Some(100),
        "next wake is the compare match at count 100 (before the 256 overflow)"
    );
}

#[test]
fn firmware_reads_a_pin_and_conditionally_drives_another() {
    // PD2 (D2) is an input; PD3 (D3) mirrors it as an output.
    //   SBI  DDRD,3            ; PD3 output
    // loop:
    //   SBIC PIND,2            ; skip the set if PD2 is low
    //   SBI  PORTD,3           ; PD2 high  → drive PD3 high
    //   SBIS PIND,2            ; skip the clear if PD2 is high
    //   CBI  PORTD,3           ; PD2 low   → drive PD3 low
    //   RJMP loop
    let mut uno = Uno::new();
    uno.mcu_mut()
        .load_program(&[0x9A53, 0x994A, 0x9A5B, 0x9B4A, 0x985B, 0xCFFB]);
    let mut io = uno.pin_io();

    let d2 = Uno::digital_pin_id(2).unwrap();
    let d3 = Uno::digital_pin_id(3).unwrap();

    // Drive D2 high (above the 2.5 V threshold); after a few loop passes D3
    // follows high. step_with_io re-samples the input every instruction.
    io.set_input(d2, 5.0);
    for _ in 0..24 {
        uno.step_with_io(&mut io);
    }
    assert_eq!(
        io.drive(d3),
        PinDrive::Digital(true),
        "D2 high → D3 driven high"
    );

    // Now pull D2 low; D3 follows low.
    io.set_input(d2, 0.0);
    for _ in 0..24 {
        uno.step_with_io(&mut io);
    }
    assert_eq!(
        io.drive(d3),
        PinDrive::Digital(false),
        "D2 low → D3 driven low"
    );
}

#[test]
fn step_to_cycle_is_a_no_op_but_still_exchanges() {
    // With until_cycle already reached, no instructions run, yet the exchange
    // still samples inputs and publishes the current drives.
    let mut avr = core_with(&[0x9A25, 0x9A2D]); // SBI DDRB,5 ; SBI PORTB,5
    let mut io = PinIo::new(avr.pin_count());
    avr.step_to_cycle(4, &mut io); // run both instructions
    let cycles = avr.cycles();

    // A second call that asks for no further progress returns the same cycle
    // count and re-publishes the (unchanged) drives.
    let reached = avr.step_to_cycle(0, &mut io);
    assert_eq!(reached, cycles, "no further instructions executed");
    assert_eq!(io.drive(pin_id(Port::B, 5)), PinDrive::Digital(true));
}
