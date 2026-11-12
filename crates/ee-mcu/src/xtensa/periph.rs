//! The ESP32 **GPIO** block — the minimal peripheral an ESP32 blink needs.
//!
//! On the ESP32 the processor drives GPIO through a register bank based at
//! `0x3FF4_4000`. Firmware sets a pin's output enable (`GPIO_ENABLE`) and then
//! its output level (`GPIO_OUT`), and reads pin levels back through `GPIO_IN`.
//! Real firmware uses the atomic **write-1-to-set** / **write-1-to-clear**
//! windows (`*_W1TS` / `*_W1TC`) rather than read-modify-write, so this models
//! them alongside the direct registers — the same pattern as the RP2040
//! [`Sio`](crate::cortexm::Sio) set/clear aliases.
//!
//! This models the low GPIO bank (`GPIO0..GPIO31`): the output latch, the output
//! enable, and the input register, plus the set/clear windows for `OUT` and
//! `ENABLE`. The IO-MUX function-select and pull configuration are not modelled —
//! the co-sim boundary ([`crate::boundary`]) owns drive strength and thresholds —
//! so a pin reads as driven whenever its `ENABLE` bit is set. The high bank
//! (`GPIO32..GPIO39`, the `*_1` registers) is not modelled.

use crate::boundary::InputThresholds;
use crate::pin::{PinDrive, PinId, PinIo};
use ee_digital::Logic;

/// Base address of the ESP32 GPIO register bank.
pub const GPIO_BASE: u32 = 0x3FF4_4000;

/// Number of GPIO pins modelled (the low `GPIO0..GPIO31` bank).
pub const PIN_COUNT: usize = 32;

// Register offsets from [`GPIO_BASE`] (low bank).
const GPIO_OUT: u32 = 0x04;
const GPIO_OUT_W1TS: u32 = 0x08;
const GPIO_OUT_W1TC: u32 = 0x0C;
const GPIO_ENABLE: u32 = 0x20;
const GPIO_ENABLE_W1TS: u32 = 0x24;
const GPIO_ENABLE_W1TC: u32 = 0x28;
const GPIO_IN: u32 = 0x3C;

/// The flat [`PinId`] for `GPIO<n>` — the pin index is the GPIO number.
pub fn pin_id(gpio: u8) -> PinId {
    PinId::new(gpio as u16)
}

/// The ESP32 GPIO registers (low bank).
#[derive(Debug, Clone, Default)]
pub struct Esp32Gpio {
    /// `GPIO_OUT`: the output latch.
    out: u32,
    /// `GPIO_ENABLE`: per-pin output enable (`1` ⇒ the pin drives `out`).
    enable: u32,
    /// `GPIO_IN`: the resolved external input levels.
    input: u32,
    /// Digital-input boundary (thresholds + hysteresis).
    thresholds: InputThresholds,
}

impl Esp32Gpio {
    /// A reset GPIO block (all pins inputs, output latch clear).
    pub fn new() -> Self {
        Self {
            thresholds: InputThresholds::default(),
            ..Default::default()
        }
    }

    /// Whether `addr` falls in the GPIO register window this peripheral claims.
    pub fn claims(addr: u32) -> bool {
        (GPIO_BASE..GPIO_BASE + 0x1000).contains(&addr)
    }

    /// The resolved logic level on `gpio`: the output latch if the pin is an
    /// output, else the externally driven input level.
    pub fn pin_level(&self, gpio: u8) -> bool {
        let mask = 1u32 << gpio;
        if self.enable & mask != 0 {
            self.out & mask != 0
        } else {
            self.input & mask != 0
        }
    }

    /// What `gpio` drives onto its node: the output latch if enabled, else
    /// high-impedance (an input).
    pub fn pin_drive(&self, gpio: u8) -> PinDrive {
        let mask = 1u32 << gpio;
        if self.enable & mask == 0 {
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

    /// Read a GPIO register (word access). Unmodelled offsets read as zero.
    pub fn read(&self, addr: u32) -> u32 {
        match addr - GPIO_BASE {
            GPIO_OUT | GPIO_OUT_W1TS | GPIO_OUT_W1TC => self.out,
            GPIO_ENABLE | GPIO_ENABLE_W1TS | GPIO_ENABLE_W1TC => self.enable,
            GPIO_IN => self.input,
            _ => 0,
        }
    }

    /// Write a GPIO register (word access), honouring the write-1-to-set and
    /// write-1-to-clear windows.
    pub fn write(&mut self, addr: u32, val: u32) {
        match addr - GPIO_BASE {
            GPIO_OUT => self.out = val,
            GPIO_OUT_W1TS => self.out |= val,
            GPIO_OUT_W1TC => self.out &= !val,
            GPIO_ENABLE => self.enable = val,
            GPIO_ENABLE_W1TS => self.enable |= val,
            GPIO_ENABLE_W1TC => self.enable &= !val,
            _ => {} // GPIO_IN and unmodelled offsets are read-only here
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enable_gates_whether_a_pin_drives() {
        let mut g = Esp32Gpio::new();
        // Output latch high but ENABLE clear → still an input (high-Z).
        g.write(GPIO_BASE + GPIO_OUT, 1 << 2);
        assert_eq!(g.pin_drive(2), PinDrive::HighZ);
        // Enable the output → now driving high.
        g.write(GPIO_BASE + GPIO_ENABLE_W1TS, 1 << 2);
        assert_eq!(g.pin_drive(2), PinDrive::Digital(true));
    }

    #[test]
    fn w1ts_and_w1tc_set_and_clear_a_pin() {
        let mut g = Esp32Gpio::new();
        g.write(GPIO_BASE + GPIO_ENABLE_W1TS, 1 << 2);
        assert!(!g.pin_level(2));
        g.write(GPIO_BASE + GPIO_OUT_W1TS, 1 << 2);
        assert!(g.pin_level(2), "W1TS set the bit high");
        g.write(GPIO_BASE + GPIO_OUT_W1TC, 1 << 2);
        assert!(!g.pin_level(2), "W1TC cleared it low");
    }

    #[test]
    fn input_register_reflects_sampled_levels() {
        let mut g = Esp32Gpio::new();
        let mut io = PinIo::new(PIN_COUNT);
        io.set_input(pin_id(4), 3.3);
        g.sample_inputs(&io);
        assert_eq!(g.read(GPIO_BASE + GPIO_IN) & (1 << 4), 1 << 4);
    }
}
