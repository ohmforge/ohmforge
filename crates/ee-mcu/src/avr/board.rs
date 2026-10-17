//! The Arduino Uno board definition: an ATmega328P core plus the Uno's pin
//! mapping, so firmware that talks in "digital pin D13" lands on the right AVR
//! port bit.
//!
//! The Uno maps the Arduino headers onto the ATmega328P ports as:
//!
//! | Arduino pins | AVR port | notes                         |
//! |--------------|----------|-------------------------------|
//! | D0..D7       | PORTD 0..7 | D0/D1 are the USART RX/TX   |
//! | D8..D13      | PORTB 0..5 | **D13 → PB5** (the on-board LED) |
//! | A0..A5       | PORTC 0..5 | ADC channels 0..5           |
//!
//! [`Uno`] wraps an [`Avr`] clocked at the Uno's 16 MHz, loads firmware through
//! the existing Intel-HEX / raw loader, and resolves digital-pin reads to the
//! underlying GPIO pin.

use crate::{FirmwareError, McuCore, PinId, PinIo};

use super::core::Avr;
use super::periph::{pin_id, Port};
use super::variant::AvrVariant;

/// The Uno's CPU clock (16 MHz).
pub const UNO_F_CPU: f64 = 16_000_000.0;

/// An Arduino Uno: an ATmega328P with the Uno pin map.
pub struct Uno {
    mcu: Avr,
}

impl Default for Uno {
    fn default() -> Self {
        Self::new()
    }
}

impl Uno {
    /// A reset Uno clocked at 16 MHz.
    pub fn new() -> Self {
        let mut mcu = Avr::new();
        mcu.set_clock_hz(UNO_F_CPU);
        Self { mcu }
    }

    /// The underlying MCU core (registers, flash, peripherals).
    pub fn mcu(&self) -> &Avr {
        &self.mcu
    }
    /// Mutable access to the underlying MCU core.
    pub fn mcu_mut(&mut self) -> &mut Avr {
        &mut self.mcu
    }

    /// Set the CPU clock in Hz (tests may slow it to keep a run short).
    pub fn set_clock_hz(&mut self, hz: f64) {
        self.mcu.set_clock_hz(hz);
    }

    /// Load firmware (raw binary or Intel HEX) into flash and reset.
    pub fn load_firmware(&mut self, image: &[u8]) -> Result<(), FirmwareError> {
        self.mcu.load_firmware(image)
    }

    /// Execute one instruction.
    pub fn step(&mut self) {
        self.mcu.step();
    }
    /// Run until the simulated clock reaches `t` seconds.
    pub fn step_to(&mut self, t: f64) {
        self.mcu.step_to(t);
    }

    // --- Co-simulation bridge (forwards to the core; see [`McuCore`]) ---

    /// A [`PinIo`] exchange sized for this board's pins.
    pub fn pin_io(&self) -> PinIo {
        PinIo::new(self.mcu.pin_count())
    }

    /// The absolute cycle count at which the core next wants to run (see
    /// [`McuCore::next_event`]).
    pub fn next_event(&self) -> Option<u64> {
        self.mcu.next_event()
    }

    /// Advance to the cycle bound `until_cycle` while exchanging pin state (see
    /// [`McuCore::step_to_cycle`]).
    pub fn step_to_cycle(&mut self, until_cycle: u64, io: &mut PinIo) -> u64 {
        self.mcu.step_to_cycle(until_cycle, io)
    }

    /// A single co-sim step exchanging pin state (see [`Avr::step_with_io`]).
    pub fn step_with_io(&mut self, io: &mut PinIo) {
        self.mcu.step_with_io(io);
    }

    /// The [`PinId`] for Arduino digital pin `pin` (`0..=13`), for mapping it to
    /// a circuit node in the co-sim loop.
    pub fn digital_pin_id(pin: u8) -> Option<PinId> {
        Self::digital_pin(pin).map(|(port, bit)| pin_id(port, bit))
    }

    /// The [`PinId`] for Arduino analog pin `pin` (`0..=5`).
    pub fn analog_pin_id(pin: u8) -> Option<PinId> {
        Self::analog_pin(pin).map(|(port, bit)| pin_id(port, bit))
    }

    /// Map an Arduino digital pin number (`0..=13`) to its AVR `(port, bit)`.
    pub fn digital_pin(pin: u8) -> Option<(Port, u8)> {
        match pin {
            0..=7 => Some((Port::D, pin)),
            8..=13 => Some((Port::B, pin - 8)),
            _ => None,
        }
    }

    /// Map an Arduino analog pin number (`0..=5`) to its AVR `(port, bit)` and
    /// ADC channel (the bit and channel coincide on the Uno).
    pub fn analog_pin(pin: u8) -> Option<(Port, u8)> {
        match pin {
            0..=5 => Some((Port::C, pin)),
            _ => None,
        }
    }

    /// The logic level on Arduino digital pin `pin` (`false` for unknown pins).
    pub fn digital_read(&self, pin: u8) -> bool {
        match Self::digital_pin(pin) {
            Some((port, bit)) => self.mcu.pin(port, bit),
            None => false,
        }
    }
}

/// The Arduino Mega's CPU clock (16 MHz).
pub const MEGA_F_CPU: f64 = 16_000_000.0;

/// An Arduino Mega: an [`ATmega2560`](AvrVariant::atmega2560) with the Mega's
/// pin map. The 2560 is the same AVR ISA as the Uno's 328P with a larger memory
/// map and more ports/timers, so it reuses the shared [`Avr`] core.
///
/// Only the pins that land on the modelled GPIO ports (B/C/D) are mapped here —
/// notably the on-board LED on **D13 → PB7** and the rest of the PORTB header
/// (D10..D13 → PB4..PB7). The Mega's full header (PORTA/E/F/G/H/J/K/L) is part
/// of the declared port set (see [`AvrVariant::atmega2560`]) but beyond the
/// register-decoded B/C/D subset the emulator models.
pub struct Mega {
    mcu: Avr,
}

impl Default for Mega {
    fn default() -> Self {
        Self::new()
    }
}

impl Mega {
    /// A reset Mega (ATmega2560) clocked at 16 MHz.
    pub fn new() -> Self {
        let mut mcu = Avr::with_variant(AvrVariant::atmega2560());
        mcu.set_clock_hz(MEGA_F_CPU);
        Self { mcu }
    }

    /// The underlying MCU core (registers, flash, peripherals).
    pub fn mcu(&self) -> &Avr {
        &self.mcu
    }
    /// Mutable access to the underlying MCU core.
    pub fn mcu_mut(&mut self) -> &mut Avr {
        &mut self.mcu
    }

    /// Set the CPU clock in Hz (tests may slow it to keep a run short).
    pub fn set_clock_hz(&mut self, hz: f64) {
        self.mcu.set_clock_hz(hz);
    }

    /// Load firmware (raw binary or Intel HEX) into flash and reset.
    pub fn load_firmware(&mut self, image: &[u8]) -> Result<(), FirmwareError> {
        self.mcu.load_firmware(image)
    }

    /// Execute one instruction.
    pub fn step(&mut self) {
        self.mcu.step();
    }
    /// Run until the simulated clock reaches `t` seconds.
    pub fn step_to(&mut self, t: f64) {
        self.mcu.step_to(t);
    }

    /// A [`PinIo`] exchange sized for this board's pins.
    pub fn pin_io(&self) -> PinIo {
        PinIo::new(self.mcu.pin_count())
    }

    /// Map a Mega digital pin to its AVR `(port, bit)`, for the pins on the
    /// modelled PORTB header: D10..D13 → PB4..PB7 (D13 is the on-board LED).
    /// Other header pins live on ports the emulator does not register-decode and
    /// return `None`.
    pub fn digital_pin(pin: u8) -> Option<(Port, u8)> {
        match pin {
            10..=13 => Some((Port::B, pin - 6)), // D10→PB4 … D13→PB7
            _ => None,
        }
    }

    /// The [`PinId`] for Mega digital pin `pin` (modelled PORTB header pins).
    pub fn digital_pin_id(pin: u8) -> Option<PinId> {
        Self::digital_pin(pin).map(|(port, bit)| pin_id(port, bit))
    }

    /// The logic level on Mega digital pin `pin` (`false` for unmapped pins).
    pub fn digital_read(&self, pin: u8) -> bool {
        match Self::digital_pin(pin) {
            Some((port, bit)) => self.mcu.pin(port, bit),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uno_pin_map_places_the_led_on_pb5() {
        assert_eq!(Uno::digital_pin(13), Some((Port::B, 5)));
        assert_eq!(Uno::digital_pin(0), Some((Port::D, 0)));
        assert_eq!(Uno::digital_pin(7), Some((Port::D, 7)));
        assert_eq!(Uno::digital_pin(8), Some((Port::B, 0)));
        assert_eq!(Uno::digital_pin(14), None);
        assert_eq!(Uno::analog_pin(0), Some((Port::C, 0)));
    }

    #[test]
    fn mega_places_the_led_on_pb7_and_runs_a_tiny_program() {
        assert_eq!(Mega::digital_pin(13), Some((Port::B, 7)));
        assert_eq!(Mega::digital_pin(10), Some((Port::B, 4)));

        // The Mega carries the ATmega2560's larger memory map.
        let mut mega = Mega::new();
        assert_eq!(mega.mcu().variant().name, "ATmega2560");
        assert_eq!(mega.mcu().pin_count(), 88);

        // A tiny program toggling the LED (PB7): SBI DDRB,7 ; LDI r16,0x80 ;
        // OUT PINB,r16 (write-1-to-PINx toggles PORTB bit 7).
        mega.mcu_mut().load_program(&[0x9A27, 0xE800, 0xB903]);
        mega.step(); // SBI DDRB,7 → output
        assert!(!mega.digital_read(13), "LED starts low");
        mega.step(); // LDI r16,0x80
        mega.step(); // OUT PINB,r16 → toggle PB7 high
        assert!(mega.digital_read(13), "LED (D13 → PB7) toggled high");
    }
}
