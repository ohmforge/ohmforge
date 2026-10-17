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

use crate::{FirmwareError, McuCore};

use super::core::Avr;
use super::periph::Port;

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
}
