//! # ee-mcu
//!
//! Instruction-accurate microcontroller cores, their peripherals, and the
//! **co-simulation bridge** that maps MCU pins onto circuit nodes so real
//! firmware runs against a simulated circuit. See `docs/ARCHITECTURE.md` §
//! "Board & MCU emulation."
//!
//! Planned cores (docs/ROADMAP.md):
//! - `avr`     — ATmega328P (Arduino Uno/Nano), ATmega2560, ATtiny  [M1/M2]
//! - `cortexm` — RP2040 (Raspberry Pi Pico), STM32-class            [M2/M3]
//! - `riscv`   — RV32I(MC) for ESP32-class                          [M3]

/// A microcontroller core the emulator can run.
pub trait McuCore {
    /// Advance execution to (at most) the given simulation time in seconds,
    /// executing instructions and updating peripheral/pin state.
    fn step_to(&mut self, t: f64);
    /// Load firmware from a `.hex`/`.elf` image already in memory.
    fn load_firmware(&mut self, image: &[u8]) -> Result<(), FirmwareError>;
}

#[derive(Debug)]
pub enum FirmwareError {
    UnsupportedFormat,
    BadImage,
}
