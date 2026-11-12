//! An instruction-accurate Xtensa core, modelled on the **ESP32**'s Xtensa LX6.
//!
//! Xtensa is a 32-bit load/store RISC with sixteen address registers and
//! **variable-length** instructions: the core encodings (RRR, RRI8, RI16, CALL,
//! BRI12) are three bytes, and the Code-Density option adds two-byte "narrow"
//! forms. Instructions are little-endian in memory. The architecture's defining
//! feature is its **register windows** — a 64-entry physical register file that
//! rotates on windowed calls — but this core deliberately models the simpler,
//! fully documented **CALL0 (windowless) ABI**: a flat sixteen-entry register
//! file with `a0` as the link register. The windowed call machinery is not
//! modelled (see [`decode`](mod@decode)).
//!
//! The pieces mirror the [`avr`](crate::avr) and [`cortexm`](crate::cortexm)
//! cores so an [`Esp32`] plugs into the same co-simulation loop as the Uno and
//! the Pico:
//! - [`isa`] — the decoded-instruction type ([`Instr`]) and operand shapes.
//! - [`decode`](mod@decode) — a table-driven decoder from instruction bytes to
//!   [`Instr`], with the exact covered-opcode set documented.
//! - [`core`] — the register file, flat memory with the GPIO block decoded as
//!   MMIO, and the fetch/decode/execute step loop ([`Xtensa`]), implementing the
//!   shared [`McuCore`](crate::McuCore) trait.
//! - [`periph`] — the ESP32 GPIO block ([`Esp32Gpio`]).
//! - [`board`] — the [`Esp32`] board tying the core to the GPIO map.
//! - [`variant`] — per-part descriptors ([`XtensaVariant`]).
//!
//! It implements a correct **subset** of the ISA; unimplemented encodings decode
//! to [`Instr::Unknown`]. See [`decode`](mod@decode) for the exact coverage and
//! the documented TODOs.

pub mod board;
pub mod core;
pub mod decode;
pub mod isa;
pub mod periph;
pub mod variant;

pub use self::core::Xtensa;
pub use board::{Esp32, ESP32_F_CPU, LED_GPIO};
pub use decode::decode;
pub use isa::{Instr, Size};
pub use periph::{pin_id, Esp32Gpio, GPIO_BASE, PIN_COUNT};
pub use variant::XtensaVariant;
