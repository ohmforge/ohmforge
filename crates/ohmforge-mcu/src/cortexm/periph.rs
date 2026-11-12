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
use ohmforge_digital::Logic;

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

// --- STM32-class GPIO peripheral ---

/// Base address of the modelled STM32 GPIO port (STM32F4 `GPIOC`, AHB1).
pub const STM32_GPIO_BASE: u32 = 0x4002_0800;

/// Number of pins on the modelled STM32 GPIO port (one 16-bit port).
pub const STM32_PIN_COUNT: usize = 16;

/// The flat [`PinId`] base for the STM32 port, placed above the SIO pins so the
/// two GPIO blocks occupy disjoint co-sim pin ids and never overwrite each
/// other's drives.
pub const STM32_PIN_BASE: u16 = PIN_COUNT as u16;

// STM32F4 GPIO register offsets from the port base.
const MODER: u32 = 0x00;
const OTYPER: u32 = 0x04;
const OSPEEDR: u32 = 0x08;
const PUPDR: u32 = 0x0C;
const IDR: u32 = 0x10;
const ODR: u32 = 0x14;
const BSRR: u32 = 0x18;

/// The flat [`PinId`] for STM32 port bit `n`.
pub fn stm32_pin_id(bit: u8) -> PinId {
    PinId::new(STM32_PIN_BASE + bit as u16)
}

/// A single STM32F4-style GPIO port: `MODER` (two bits per pin), `ODR`, the
/// atomic `BSRR` set/reset window, and `IDR`. A pin drives its `ODR` bit when
/// `MODER` selects general-purpose output (`01`); otherwise it is an input.
///
/// This models just the GPIO a blink needs; `OTYPER`/`OSPEEDR`/`PUPDR` and the
/// alternate-function registers are accepted and stored but do not affect the
/// co-sim boundary (which owns drive strength and thresholds).
#[derive(Debug, Clone, Default)]
pub struct Stm32Gpio {
    /// `MODER`: two bits per pin (`01` = output).
    moder: u32,
    /// `ODR`: the output data latch (low 16 bits).
    odr: u32,
    /// The resolved external input levels (low 16 bits).
    input: u32,
    /// Digital-input boundary (thresholds + hysteresis).
    thresholds: InputThresholds,
}

impl Stm32Gpio {
    /// A reset GPIO port (all pins inputs, output latch clear).
    pub fn new() -> Self {
        Self {
            thresholds: InputThresholds::default(),
            ..Default::default()
        }
    }

    /// Whether `addr` falls in this port's register window.
    pub fn claims(addr: u32) -> bool {
        (STM32_GPIO_BASE..STM32_GPIO_BASE + 0x400).contains(&addr)
    }

    /// Whether pin `bit` is configured as a general-purpose output.
    fn is_output(&self, bit: u8) -> bool {
        (self.moder >> (2 * bit as u32)) & 0b11 == 0b01
    }

    /// The resolved logic level on `bit`: the `ODR` latch if it is an output,
    /// else the externally driven input level.
    pub fn pin_level(&self, bit: u8) -> bool {
        let mask = 1u32 << bit;
        if self.is_output(bit) {
            self.odr & mask != 0
        } else {
            self.input & mask != 0
        }
    }

    /// What `bit` drives onto its node: its `ODR` bit if an output, else high-Z.
    pub fn pin_drive(&self, bit: u8) -> PinDrive {
        if self.is_output(bit) {
            PinDrive::Digital(self.odr & (1 << bit) != 0)
        } else {
            PinDrive::HighZ
        }
    }

    /// Configure the digital-input thresholds.
    pub fn set_input_thresholds(&mut self, thresholds: InputThresholds) {
        self.thresholds = thresholds;
    }

    /// Sample the co-sim exchange's input voltages into the input register.
    pub fn sample_inputs(&mut self, io: &PinIo) {
        for bit in 0..STM32_PIN_COUNT as u8 {
            let mask = 1u32 << bit;
            let previous = if self.input & mask != 0 {
                Logic::High
            } else {
                Logic::Low
            };
            let v = io.input(stm32_pin_id(bit));
            match self.thresholds.resolve(v, previous) {
                Logic::High => self.input |= mask,
                Logic::Low => self.input &= !mask,
                _ => {}
            }
        }
    }

    /// Publish each pin's drive into the co-sim exchange's *out* half.
    pub fn publish_drives(&self, io: &mut PinIo) {
        for bit in 0..STM32_PIN_COUNT as u8 {
            io.set_drive(stm32_pin_id(bit), self.pin_drive(bit));
        }
    }

    /// Read a GPIO register (word access). Unmodelled offsets read as zero.
    pub fn read(&self, addr: u32) -> u32 {
        match addr - STM32_GPIO_BASE {
            MODER => self.moder,
            IDR => self.input & 0xFFFF,
            ODR => self.odr & 0xFFFF,
            _ => 0,
        }
    }

    /// Write a GPIO register (word access), honouring the `BSRR` set/reset window.
    pub fn write(&mut self, addr: u32, val: u32) {
        match addr - STM32_GPIO_BASE {
            MODER => self.moder = val,
            ODR => self.odr = val & 0xFFFF,
            BSRR => {
                // Bits [15:0] set the matching ODR bit, [31:16] reset it; a reset
                // and set of the same bit in one write leaves the bit set.
                self.odr &= !((val >> 16) & 0xFFFF);
                self.odr |= val & 0xFFFF;
            }
            // Accepted but not modelled at the co-sim boundary.
            OTYPER | OSPEEDR | PUPDR => {}
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

    #[test]
    fn stm32_moder_gates_output_and_bsrr_toggles() {
        let mut g = Stm32Gpio::new();
        // Before MODER selects output, PC13 is high-Z.
        g.write(STM32_GPIO_BASE + BSRR, 1 << 13); // BS13
        assert_eq!(g.pin_drive(13), PinDrive::HighZ, "input until MODER set");
        // MODER: PC13 → output (01 in bits [27:26]).
        g.write(STM32_GPIO_BASE + MODER, 0b01 << 26);
        assert_eq!(g.pin_drive(13), PinDrive::Digital(true), "BS13 drove high");
        // BSRR reset (BR13 in the high half) clears it again.
        g.write(STM32_GPIO_BASE + BSRR, 1 << (16 + 13));
        assert_eq!(g.pin_drive(13), PinDrive::Digital(false), "BR13 cleared it");
        // ODR reads back the latch.
        assert_eq!(g.read(STM32_GPIO_BASE + ODR), 0);
    }

    #[test]
    fn stm32_claims_disjoint_window_from_sio() {
        assert!(Stm32Gpio::claims(STM32_GPIO_BASE + MODER));
        assert!(!Stm32Gpio::claims(SIO_BASE));
        assert!(!Sio::claims(STM32_GPIO_BASE));
        // STM32 pin ids sit above the SIO pins.
        assert_eq!(stm32_pin_id(0).index(), PIN_COUNT as u16);
    }
}
