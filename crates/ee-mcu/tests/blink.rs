//! (B) CP-BRD-M1.5 gate: load a real `blink.hex` onto an Arduino Uno and show
//! that D13 (PB5) toggles at ~1 Hz.
//!
//! # How `blink.hex` was produced
//!
//! The fixture `blink.hex` (next to this file) is a hand-assembled Intel-HEX
//! image for the ATmega328P. Its program, by flash word address:
//!
//! ```text
//!  0: SBI  DDRB, 5        ; 0x9A25 — PB5 (Arduino D13) as an output
//!  1: LDI  r16, 0x20      ; 0xE200 — bit mask for PB5
//!  2: (loop) OUT PINB,r16 ; 0xB903 — write-1-to-PINx toggles PORTB bit 5
//!  3: LDI  r18, 25        ; 0xE129 — outer delay count
//!  4: (outer) LDI r24,0x10; 0xE180 — inner count = 10000 (0x2710), low byte
//!  5: LDI  r25, 0x27      ; 0xE297 — inner count, high byte
//!  6: (inner) SBIW r24, 1 ; 0x9701
//!  7: BRNE inner          ; 0xF7F1 (offset -2)
//!  8: DEC  r18            ; 0x952A
//!  9: BRNE outer          ; 0xF7D1 (offset -6)
//! 10: RJMP loop           ; 0xCFF7 (offset -9)
//! ```
//!
//! The delay is `25 × 10000` SBIW iterations at 4 cycles each ≈ 1,000,000 CPU
//! cycles between toggles. The test clocks the Uno at **1 MHz** (via
//! [`Uno::set_clock_hz`]) so that one toggle ≈ one simulated second — i.e. D13
//! toggles at ~1 Hz — while keeping the number of stepped instructions small.
//!
//! The same word list is emitted to Intel HEX by [`words_to_ihex`] below and
//! asserted equal to the committed fixture, so the checked-in `blink.hex` is
//! exactly this program.

use ee_mcu::avr::{Port, Uno};

/// The flash words of the blink program (see the module comment).
const BLINK_WORDS: [u16; 11] = [
    0x9A25, 0xE200, 0xB903, 0xE129, 0xE180, 0xE297, 0x9701, 0xF7F1, 0x952A, 0xF7D1, 0xCFF7,
];

/// Emit `words` as a single-record Intel HEX image (little-endian), the way the
/// fixture was generated.
fn words_to_ihex(words: &[u16]) -> String {
    let mut bytes = Vec::with_capacity(words.len() * 2);
    for &w in words {
        bytes.push((w & 0xFF) as u8);
        bytes.push((w >> 8) as u8);
    }
    let mut rec = String::from(":");
    let mut sum = 0u8;
    let mut emit = |rec: &mut String, b: u8| {
        rec.push_str(&format!("{b:02X}"));
        sum = sum.wrapping_add(b);
    };
    emit(&mut rec, bytes.len() as u8);
    emit(&mut rec, 0x00); // address high
    emit(&mut rec, 0x00); // address low
    emit(&mut rec, 0x00); // record type: data
    for b in &bytes {
        emit(&mut rec, *b);
    }
    let checksum = (!sum).wrapping_add(1);
    rec.push_str(&format!("{checksum:02X}"));
    format!("{rec}\n:00000001FF\n")
}

#[test]
fn committed_blink_hex_matches_the_assembly() {
    let fixture = include_str!("blink.hex");
    assert_eq!(
        fixture,
        words_to_ihex(&BLINK_WORDS),
        "committed blink.hex is the hand-assembled program"
    );
}

#[test]
fn uno_blinks_d13_at_about_1hz() {
    let hex = include_str!("blink.hex");

    let mut uno = Uno::new();
    // 1 MHz → 1 cycle = 1 µs, so the ~1,000,000-cycle delay is ~1 s per toggle
    // and the run stays a few million instruction steps.
    uno.set_clock_hz(1_000_000.0);
    uno.load_firmware(hex.as_bytes()).expect("load blink.hex");

    // Step until we have seen a few D13 edges, recording the elapsed cycles at
    // each one.
    let mut edges: Vec<u64> = Vec::new();
    let mut last = uno.digital_read(13);
    let max_steps = 8_000_000;
    for _ in 0..max_steps {
        uno.step();
        let now = uno.digital_read(13);
        if now != last {
            edges.push(uno.mcu().cycles());
            last = now;
            if edges.len() >= 3 {
                break;
            }
        }
    }

    assert!(
        edges.len() >= 2,
        "D13 should toggle at least twice, saw {} edge(s)",
        edges.len()
    );

    // The interval between consecutive toggles, in simulated seconds.
    let period_cycles = edges[1] - edges[0];
    let seconds = period_cycles as f64 / 1_000_000.0;
    assert!(
        (seconds - 1.0).abs() < 0.2,
        "D13 toggle interval should be ~1 s, measured {seconds:.3} s ({period_cycles} cycles)"
    );

    // And D13 really is PB5.
    assert_eq!(Uno::digital_pin(13), Some((Port::B, 5)));
}
