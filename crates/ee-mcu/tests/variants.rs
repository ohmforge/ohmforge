//! (B) CP-BRD-M2.4 gate: the AVR variant **peripheral-map** test.
//!
//! Each variant — ATmega328P, ATmega2560, ATtiny85 — must expose the right
//! memory size, port/timer set and pin count, and a tiny program must run on it.
//! All three reuse the shared [`Avr`] core; only the
//! [`AvrVariant`](ee_mcu::avr::AvrVariant) descriptor differs. See
//! `crates/ee-mcu/src/avr/variant.rs`.

use ee_mcu::avr::{Avr, AvrVariant, Port};
use ee_mcu::McuCore;

/// A tiny program that toggles PB5 high, then verify it ran on `avr`.
///
/// `SBI DDRB,5` (PB5 output) ; `LDI r16,0x20` ; `OUT PINB,r16` (write-1-to-PINx
/// toggles PORTB bit 5). PORTB is modelled on every variant, so this exercises
/// the shared core on each part's memory map.
fn tiny_gpio_program_runs(avr: &mut Avr) {
    avr.load_program(&[0x9A25, 0xE200, 0xB903]);
    avr.step(); // SBI DDRB,5 → output
    assert!(!avr.pin(Port::B, 5), "PB5 starts low");
    avr.step(); // LDI r16,0x20
    avr.step(); // OUT PINB,r16 → toggle PB5 high
    assert!(avr.pin(Port::B, 5), "tiny program toggled PB5 high");
}

#[test]
fn atmega328p_map_and_tiny_program() {
    let v = AvrVariant::atmega328p();
    let mut avr = Avr::with_variant(v);

    // Memory size.
    assert_eq!(v.flash_bytes(), 32 * 1024, "32 KiB flash");
    assert_eq!(v.sram_bytes, 2 * 1024, "2 KiB SRAM");
    assert_eq!(avr.sp(), 0x08FF, "stack parks at RAMEND");
    // Port + timer set.
    assert_eq!(v.port_count, 3, "ports B, C, D");
    assert_eq!(v.timer_count(), 3, "Timer0/1/2");
    // Pin count (co-sim exchange size).
    assert_eq!(avr.pin_count(), 24);

    tiny_gpio_program_runs(&mut avr);
}

#[test]
fn atmega2560_map_and_tiny_program() {
    let v = AvrVariant::atmega2560();
    let mut avr = Avr::with_variant(v);

    // Memory size: an order of magnitude more flash and 4× the SRAM of the 328P.
    assert_eq!(v.flash_bytes(), 256 * 1024, "256 KiB flash");
    assert_eq!(v.sram_bytes, 8 * 1024, "8 KiB SRAM");
    assert_eq!(avr.sp(), 0x21FF, "stack parks at the larger RAMEND");
    // Port + timer set: eleven ports (A–L), six timers.
    assert_eq!(v.port_count, 11);
    assert_eq!(v.timer_count(), 6);
    // Pin count.
    assert_eq!(avr.pin_count(), 88);

    // The bigger memory map is real: a byte near the top of the 2560's 8 KiB
    // SRAM round-trips, where the same address is out of range on the 328P.
    avr.write_data(0x2000, 0xA5);
    assert_eq!(avr.read_data(0x2000), 0xA5, "high SRAM is addressable");
    let mut small = Avr::with_variant(AvrVariant::atmega328p());
    small.write_data(0x2000, 0xA5);
    assert_eq!(
        small.read_data(0x2000),
        0x00,
        "beyond the 328P's RAMEND reads 0"
    );

    tiny_gpio_program_runs(&mut avr);
}

#[test]
fn attiny85_map_and_tiny_program() {
    let v = AvrVariant::attiny85();
    let mut avr = Avr::with_variant(v);

    // Memory size: a small part — 8 KiB flash, 512 B SRAM starting at 0x60.
    assert_eq!(v.flash_bytes(), 8 * 1024, "8 KiB flash");
    assert_eq!(v.sram_bytes, 512, "512 B SRAM");
    assert_eq!(
        v.sram_start, 0x0060,
        "SRAM follows the I/O window (no ext-I/O)"
    );
    assert_eq!(avr.sp(), 0x025F, "stack parks at RAMEND");
    // Port + timer set: a single port, two 8-bit timers.
    assert_eq!(v.port_count, 1, "port B only");
    assert_eq!(v.timer_count(), 2);
    assert_eq!(v.vector_size_words, 1, "RJMP vector table");
    // Pin count.
    assert_eq!(avr.pin_count(), 8);

    // SRAM starts right after the I/O window at 0x60.
    avr.write_data(0x0060, 0x3C);
    assert_eq!(avr.read_data(0x0060), 0x3C, "first SRAM byte round-trips");

    tiny_gpio_program_runs(&mut avr);
}

#[test]
fn variants_are_meaningfully_distinct() {
    let parts = [
        AvrVariant::atmega328p(),
        AvrVariant::atmega2560(),
        AvrVariant::attiny85(),
    ];
    // Flash, SRAM, pin count and timer count all differ across the three.
    let flash: Vec<_> = parts.iter().map(|p| p.flash_bytes()).collect();
    let pins: Vec<_> = parts.iter().map(|p| p.pin_count()).collect();
    assert_eq!(flash, vec![32 * 1024, 256 * 1024, 8 * 1024]);
    assert_eq!(pins, vec![24, 88, 8]);
}
