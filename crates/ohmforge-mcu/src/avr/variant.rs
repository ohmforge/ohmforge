//! AVR device variants: the per-chip parameters that turn the shared
//! [`Avr`](super::core::Avr) core into a specific part.
//!
//! All AVR parts in this family share one ISA, one decoder, and one
//! fetch/decode/execute loop — so the core itself is reused verbatim. What
//! differs between an ATmega328P, an ATmega2560 and an ATtiny85 is their
//! **memory map** and their **peripheral complement**:
//!
//! | parameter        | ATmega328P | ATmega2560 | ATtiny85 |
//! |------------------|-----------:|-----------:|---------:|
//! | flash (bytes)    |      32 Ki |     256 Ki |     8 Ki |
//! | SRAM (bytes)     |       2 Ki |       8 Ki |      512 |
//! | SRAM start       |     0x0100 |     0x0200 |   0x0060 |
//! | GPIO ports       |    3 (B–D) |   11 (A–L) |   1 (B)  |
//! | modelled pins    |         24 |         88 |        8 |
//! | timers           |          3 |          6 |        2 |
//! | vector stride    |    2 words |    2 words |   1 word |
//!
//! An [`AvrVariant`] is a small `Copy` descriptor carrying exactly these
//! numbers. [`Avr::with_variant`](super::core::Avr::with_variant) sizes flash,
//! SRAM and the I/O window from it, parks the stack at `RAMEND`, and sizes the
//! [`PinIo`](crate::PinIo) exchange to `pin_count()`.
//!
//! # Modelled subset
//!
//! The memory sizes, pin count and the declared port/timer **set** are
//! parameterised per part and are what callers (and the variant gate test)
//! observe. The live, register-decoded peripheral file — GPIO ports B/C/D and
//! Timer0/Timer1 at their ATmega328P I/O addresses — is the shared subset reused
//! across every variant (see [`periph`](super::periph)). A variant that declares
//! more ports or timers exposes the larger memory and pin budget and the correct
//! counts, while GPIO/timer activity is modelled on the common B/C/D + Timer0/1
//! registers. This keeps the core honest about "same ISA, bigger map" without
//! duplicating a full register layout per part.

/// A timer/counter a variant provides, for the declared peripheral set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimerSpec {
    /// Datasheet name, e.g. `"Timer0"`.
    pub name: &'static str,
    /// Counter width in bits (8 or 16).
    pub bits: u8,
}

impl TimerSpec {
    /// A timer spec from a name and width.
    pub const fn new(name: &'static str, bits: u8) -> Self {
        Self { name, bits }
    }
}

// --- Declared timer sets ---

const TIMERS_328P: &[TimerSpec] = &[
    TimerSpec::new("Timer0", 8),
    TimerSpec::new("Timer1", 16),
    TimerSpec::new("Timer2", 8),
];

const TIMERS_2560: &[TimerSpec] = &[
    TimerSpec::new("Timer0", 8),
    TimerSpec::new("Timer1", 16),
    TimerSpec::new("Timer2", 8),
    TimerSpec::new("Timer3", 16),
    TimerSpec::new("Timer4", 16),
    TimerSpec::new("Timer5", 16),
];

const TIMERS_TINY85: &[TimerSpec] = &[TimerSpec::new("Timer0", 8), TimerSpec::new("Timer1", 8)];

/// The per-chip parameters that specialise the shared AVR core into a part.
///
/// Construct one with [`atmega328p`](AvrVariant::atmega328p),
/// [`atmega2560`](AvrVariant::atmega2560) or [`attiny85`](AvrVariant::attiny85),
/// then build a core with
/// [`Avr::with_variant`](super::core::Avr::with_variant).
#[derive(Debug, Clone, Copy)]
pub struct AvrVariant {
    /// The part name, e.g. `"ATmega328P"`.
    pub name: &'static str,
    /// Flash program memory in 16-bit instruction words.
    pub flash_words: usize,
    /// Internal SRAM in bytes.
    pub sram_bytes: usize,
    /// First data-space address of SRAM (I/O + extended-I/O sit below it).
    pub sram_start: u16,
    /// Number of 8-bit GPIO ports the part bonds out.
    pub port_count: usize,
    /// The timers the part provides (declared set; see the module docs).
    pub timers: &'static [TimerSpec],
    /// Interrupt-vector stride in flash words (1 for parts whose table uses
    /// `RJMP`, 2 for parts using `JMP`).
    pub vector_size_words: u16,
    /// Default CPU clock in Hz.
    pub f_cpu: f64,
}

impl AvrVariant {
    /// `RAMEND`: the highest valid data address, and the stack pointer's reset
    /// value.
    pub const fn ramend(&self) -> u16 {
        self.sram_start + self.sram_bytes as u16 - 1
    }

    /// Modelled GPIO pins: eight per port.
    pub const fn pin_count(&self) -> usize {
        self.port_count * 8
    }

    /// Flash size in bytes (two per word).
    pub const fn flash_bytes(&self) -> usize {
        self.flash_words * 2
    }

    /// The number of timers in the declared set.
    pub const fn timer_count(&self) -> usize {
        self.timers.len()
    }

    /// The **ATmega328P** (Arduino Uno/Nano): 32 KiB flash, 2 KiB SRAM, ports
    /// B/C/D, three timers, 16 MHz.
    pub const fn atmega328p() -> Self {
        Self {
            name: "ATmega328P",
            flash_words: 16 * 1024, // 32 KiB
            sram_bytes: 2 * 1024,   // 2 KiB
            sram_start: 0x0100,
            port_count: 3, // B, C, D
            timers: TIMERS_328P,
            vector_size_words: 2,
            f_cpu: 16_000_000.0,
        }
    }

    /// The **ATmega2560** (Arduino Mega): 256 KiB flash, 8 KiB SRAM, eleven
    /// ports (A–L, no I), six timers, 16 MHz. The same AVR ISA as the 328P with
    /// an extended memory and peripheral map.
    pub const fn atmega2560() -> Self {
        Self {
            name: "ATmega2560",
            flash_words: 128 * 1024, // 256 KiB
            sram_bytes: 8 * 1024,    // 8 KiB
            sram_start: 0x0200,      // extended-I/O window pushes SRAM up
            port_count: 11,          // A,B,C,D,E,F,G,H,J,K,L
            timers: TIMERS_2560,
            vector_size_words: 2,
            f_cpu: 16_000_000.0,
        }
    }

    /// The **ATtiny85**: 8 KiB flash, 512 B SRAM, a single port (B), two 8-bit
    /// timers, 8 MHz internal oscillator. A small-pin-count part on the same ISA.
    pub const fn attiny85() -> Self {
        Self {
            name: "ATtiny85",
            flash_words: 4 * 1024, // 8 KiB
            sram_bytes: 512,
            sram_start: 0x0060, // no extended-I/O: SRAM follows the I/O window
            port_count: 1,      // B only
            timers: TIMERS_TINY85,
            vector_size_words: 1, // small flash ⇒ RJMP vector table
            f_cpu: 8_000_000.0,
        }
    }
}

impl Default for AvrVariant {
    fn default() -> Self {
        Self::atmega328p()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_report_distinct_memory_maps() {
        let m328 = AvrVariant::atmega328p();
        assert_eq!(m328.flash_bytes(), 32 * 1024);
        assert_eq!(m328.sram_bytes, 2 * 1024);
        assert_eq!(m328.ramend(), 0x08FF);
        assert_eq!(m328.pin_count(), 24);
        assert_eq!(m328.timer_count(), 3);

        let m2560 = AvrVariant::atmega2560();
        assert_eq!(m2560.flash_bytes(), 256 * 1024);
        assert_eq!(m2560.sram_bytes, 8 * 1024);
        assert_eq!(m2560.ramend(), 0x21FF);
        assert_eq!(m2560.pin_count(), 88);
        assert_eq!(m2560.timer_count(), 6);

        let tiny = AvrVariant::attiny85();
        assert_eq!(tiny.flash_bytes(), 8 * 1024);
        assert_eq!(tiny.sram_bytes, 512);
        assert_eq!(tiny.ramend(), 0x025F);
        assert_eq!(tiny.pin_count(), 8);
        assert_eq!(tiny.timer_count(), 2);
        assert_eq!(tiny.vector_size_words, 1);
    }
}
