//! The RP2040 **SIO** (single-cycle I/O) GPIO block — the minimal peripheral a
//! Pico blink needs.
//!
//! On the RP2040 the processor drives GPIO through the SIO, a set of registers
//! in the single-cycle I/O space at `0xD000_0000`. Firmware sets a pin's output
//! enable (`GPIO_OE`) and then its output level (`GPIO_OUT`), and reads pin
//! levels back through `GPIO_IN`. The `*_SET` / `*_CLR` / `*_XOR` aliases are the
//! atomic read-modify-write windows real firmware uses (a blink typically
//! `GPIO_OUT_XOR`s the LED bit).
//!
//! This models the GPIO subset of the SIO: the output latch, the output enable,
//! and the input register, plus the set/clear/xor aliases for `OUT` and `OE`.
//! The pad/bank0 function-select and pull configuration are not modelled — the
//! co-sim boundary ([`crate::boundary`]) owns drive strength and thresholds — so
//! a pin reads as driven whenever its `OE` bit is set.

use crate::boundary::InputThresholds;
use crate::pin::{PinDrive, PinId, PinIo};
use ee_digital::Logic;

/// Base address of the SIO block in the single-cycle I/O space.
pub const SIO_BASE: u32 = 0xD000_0000;

/// Number of GPIO pins modelled (RP2040 has 30 user GPIOs, `GPIO0..GPIO29`).
pub const PIN_COUNT: usize = 30;

// Register offsets from [`SIO_BASE`].
const CPUID: u32 = 0x000;
const GPIO_IN: u32 = 0x004;
const GPIO_OUT: u32 = 0x010;
const GPIO_OUT_SET: u32 = 0x014;
const GPIO_OUT_CLR: u32 = 0x018;
const GPIO_OUT_XOR: u32 = 0x01c;
const GPIO_OE: u32 = 0x020;
const GPIO_OE_SET: u32 = 0x024;
const GPIO_OE_CLR: u32 = 0x028;
const GPIO_OE_XOR: u32 = 0x02c;

/// Mask of the modelled GPIO bits (`GPIO0..GPIO29`).
const GPIO_MASK: u32 = (1 << PIN_COUNT) - 1;

/// The flat [`PinId`] for `GPIO<n>` — the pin index is the GPIO number.
pub fn pin_id(gpio: u8) -> PinId {
    PinId::new(gpio as u16)
}

/// The SIO GPIO registers.
#[derive(Debug, Clone, Default)]
pub struct Sio {
    /// `GPIO_OUT`: the output latch.
    out: u32,
    /// `GPIO_OE`: per-pin output enable (`1` ⇒ the pin drives `out`).
    oe: u32,
    /// `GPIO_IN`: the resolved external input levels.
    input: u32,
    /// Digital-input boundary (thresholds + hysteresis).
    thresholds: InputThresholds,
}

impl Sio {
    /// A reset SIO block (all pins inputs, output latch clear).
    pub fn new() -> Self {
        Self {
            thresholds: InputThresholds::default(),
            ..Default::default()
        }
    }

    /// Whether `addr` falls in the SIO window this peripheral claims.
    pub fn claims(addr: u32) -> bool {
        (SIO_BASE..SIO_BASE + 0x1000).contains(&addr)
    }

    /// The resolved logic level on `gpio`: the output latch if the pin is an
    /// output, else the externally driven input level.
    pub fn pin_level(&self, gpio: u8) -> bool {
        let mask = 1u32 << gpio;
        if self.oe & mask != 0 {
            self.out & mask != 0
        } else {
            self.input & mask != 0
        }
    }

    /// What `gpio` drives onto its node: the output latch if enabled, else
    /// high-impedance (an input).
    pub fn pin_drive(&self, gpio: u8) -> PinDrive {
        let mask = 1u32 << gpio;
        if self.oe & mask == 0 {
            PinDrive::HighZ
        } else {
            PinDrive::Digital(self.out & mask != 0)
        }
    }

    /// Configure the digital-input thresholds.
    pub fn set_input_thresholds(&mut self, thresholds: InputThresholds) {
        self.thresholds = thresholds;
    }

    /// Sample the co-sim exchange's input voltages into `GPIO_IN`, resolving each
    /// through the input thresholds (with hysteresis against the current level).
    pub fn sample_inputs(&mut self, io: &PinIo) {
        for gpio in 0..PIN_COUNT as u8 {
            let mask = 1u32 << gpio;
            let previous = if self.input & mask != 0 {
                Logic::High
            } else {
                Logic::Low
            };
            let v = io.input(pin_id(gpio));
            match self.thresholds.resolve(v, previous) {
                Logic::High => self.input |= mask,
                Logic::Low => self.input &= !mask,
                _ => {} // in-band with no history: hold
            }
        }
    }

    /// Publish each pin's drive into the co-sim exchange's *out* half.
    pub fn publish_drives(&self, io: &mut PinIo) {
        for gpio in 0..PIN_COUNT as u8 {
            io.set_drive(pin_id(gpio), self.pin_drive(gpio));
        }
    }

    /// Read a SIO register (word access). Unmodelled offsets read as zero.
    pub fn read(&self, addr: u32) -> u32 {
        match addr - SIO_BASE {
            CPUID => 0, // core 0
            GPIO_IN => self.input & GPIO_MASK,
            GPIO_OUT | GPIO_OUT_SET | GPIO_OUT_CLR | GPIO_OUT_XOR => self.out & GPIO_MASK,
            GPIO_OE | GPIO_OE_SET | GPIO_OE_CLR | GPIO_OE_XOR => self.oe & GPIO_MASK,
            _ => 0,
        }
    }

    /// Write a SIO register (word access), honouring the set/clear/xor aliases.
    pub fn write(&mut self, addr: u32, val: u32) {
        let val = val & GPIO_MASK;
        match addr - SIO_BASE {
            GPIO_OUT => self.out = val,
            GPIO_OUT_SET => self.out |= val,
            GPIO_OUT_CLR => self.out &= !val,
            GPIO_OUT_XOR => self.out ^= val,
            GPIO_OE => self.oe = val,
            GPIO_OE_SET => self.oe |= val,
            GPIO_OE_CLR => self.oe &= !val,
            GPIO_OE_XOR => self.oe ^= val,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oe_gates_whether_a_pin_drives() {
        let mut sio = Sio::new();
        // Output latch high but OE clear → still an input (high-Z).
        sio.write(SIO_BASE + GPIO_OUT, 1 << 25);
        assert_eq!(sio.pin_drive(25), PinDrive::HighZ);
        // Enable the output → now driving high.
        sio.write(SIO_BASE + GPIO_OE_SET, 1 << 25);
        assert_eq!(sio.pin_drive(25), PinDrive::Digital(true));
    }

    #[test]
    fn out_xor_toggles_a_pin() {
        let mut sio = Sio::new();
        sio.write(SIO_BASE + GPIO_OE_SET, 1 << 25);
        assert!(!sio.pin_level(25));
        sio.write(SIO_BASE + GPIO_OUT_XOR, 1 << 25);
        assert!(sio.pin_level(25), "xor set the bit");
        sio.write(SIO_BASE + GPIO_OUT_XOR, 1 << 25);
        assert!(!sio.pin_level(25), "xor cleared it again");
    }

    #[test]
    fn input_register_reflects_sampled_levels() {
        let mut sio = Sio::new();
        let mut io = PinIo::new(PIN_COUNT);
        io.set_input(pin_id(10), 3.3);
        sio.sample_inputs(&io);
        assert_eq!(sio.read(SIO_BASE + GPIO_IN) & (1 << 10), 1 << 10);
    }
}
