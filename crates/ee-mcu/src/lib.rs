//! # ee-mcu
//!
//! Instruction-accurate microcontroller cores, their peripherals, and the
//! **co-simulation bridge** that maps MCU pins onto circuit nodes so real
//! firmware runs against a simulated circuit. See `docs/ARCHITECTURE.md` §
//! "Board & MCU emulation."
//!
//! Planned cores (docs/ROADMAP.md):
//! - `avr`     — ATmega328P (Arduino Uno/Nano), ATmega2560, ATtiny  (M1/M2)
//! - `cortexm` — RP2040 (Raspberry Pi Pico), STM32-class            (M2/M3)
//! - `riscv`   — RV32I(MC) for ESP32-class                          (M3)
//!
//! The cores are **pure computation** — no wall-clock, no randomness, no I/O —
//! so a run is deterministic and the crate builds for `wasm32-unknown-unknown`.
//! Co-simulation wiring (pins ↔ circuit nodes) is M2; M1 establishes the
//! [`McuCore`] trait and an instruction-stepping [`avr`] core.

pub mod avr;

/// A microcontroller core the emulator can run.
///
/// The core executes whole instructions ("instruction-accurate"): cycle counts
/// are tracked so [`step_to`](McuCore::step_to) can map simulated time onto the
/// instruction stream, but sub-instruction timing is not yet modelled.
pub trait McuCore {
    /// Execute exactly one instruction (or service one pending interrupt),
    /// advancing the program counter and the core's cycle counter.
    fn step(&mut self);

    /// Advance execution until the core's elapsed time reaches (at most) `t`
    /// seconds, executing whole instructions. Because stepping is
    /// instruction-accurate, the core may stop a fraction of an instruction past
    /// `t`.
    fn step_to(&mut self, t: f64);

    /// Load firmware from an image already in memory. Raw binary images are
    /// copied into flash verbatim (little-endian 16-bit words); an image that
    /// begins with `:` is parsed as Intel HEX. ELF is not yet supported.
    fn load_firmware(&mut self, image: &[u8]) -> Result<(), FirmwareError>;
}

/// Why a firmware image could not be loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirmwareError {
    /// The image format is recognised but not yet supported (e.g. ELF).
    UnsupportedFormat,
    /// The image is malformed or does not fit the target's flash.
    BadImage,
}

impl core::fmt::Display for FirmwareError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FirmwareError::UnsupportedFormat => f.write_str(
                "unsupported firmware format (only raw binary and Intel HEX are supported)",
            ),
            FirmwareError::BadImage => {
                f.write_str("malformed firmware image, or it does not fit the target flash")
            }
        }
    }
}

impl std::error::Error for FirmwareError {}
