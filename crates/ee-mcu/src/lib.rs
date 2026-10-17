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
pub mod boundary;
pub mod cortexm;
pub mod pin;

/// External-toolchain compile orchestration (`.ino`/`.c` → HEX → emulator).
///
/// Opt-in behind the `toolchain` cargo feature and excluded on `wasm32`, so the
/// default build and tests need no external compiler. See the module docs.
#[cfg(all(feature = "toolchain", not(target_arch = "wasm32")))]
pub mod toolchain;

pub use boundary::{InputThresholds, OutputDrive, Thevenin};
pub use pin::{PinDrive, PinId, PinIo};

/// A microcontroller core the emulator can run.
///
/// The core executes whole instructions ("instruction-accurate"): cycle counts
/// are tracked so [`step_to`](McuCore::step_to) can map simulated time onto the
/// instruction stream, but sub-instruction timing is not yet modelled.
///
/// # Co-simulation
///
/// Beyond standalone stepping, the trait carries the **bridge surface** the
/// lockstep co-sim loop (in the `ee` binary) needs: [`next_event`](McuCore::next_event)
/// tells the loop when the core next wants to run, [`step_to_cycle`](McuCore::step_to_cycle)
/// advances it to a cycle bound while exchanging pin state through a [`PinIo`],
/// and [`pin_count`](McuCore::pin_count) sizes that exchange. The core itself
/// knows nothing about the analog engine — it only reads input voltages and
/// writes pin drives; the loop reconciles those with the circuit graph.
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

    /// The number of pins in this core's [`PinIo`] exchange, so the co-sim loop
    /// can size one with [`PinIo::new`].
    fn pin_count(&self) -> usize;

    /// The absolute cycle count at which the core next has something scheduled
    /// that the loop should not step past — a timer compare/overflow (and so a
    /// possible PWM edge or interrupt), a USART bit boundary, or an ADC
    /// conversion completing — or `None` if nothing is pending and the core would
    /// simply free-run.
    ///
    /// The loop takes `t_next = min(core.next_event(), sim.next_event(), …)` and
    /// advances both engines to it, so an early (but never late) wake only costs
    /// a little efficiency, never correctness.
    fn next_event(&self) -> Option<u64>;

    /// Co-simulation step: sample `io`'s input voltages (feeding the ADC and the
    /// digital-input thresholds), execute whole instructions until the elapsed
    /// cycle count reaches at least `until_cycle`, then publish the resulting pin
    /// drives back into `io`. Returns the cycle count actually reached.
    ///
    /// Inputs are sampled once at entry: within a lockstep interval the circuit's
    /// node voltages are held constant, so this matches the loop's contract.
    /// Calling with `until_cycle <= cycles` runs no instructions but still
    /// performs the exchange.
    fn step_to_cycle(&mut self, until_cycle: u64, io: &mut PinIo) -> u64;
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
