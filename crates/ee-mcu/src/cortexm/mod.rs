//! An instruction-accurate ARM Cortex-M0+ core (ARMv6-M / Thumb), modelled on
//! the RP2040 (Raspberry Pi Pico).
//!
//! ARMv6-M is the Thumb instruction subset the Cortex-M0/M0+ implements: a flat
//! 32-bit memory, sixteen 32-bit registers (`r13`=SP, `r14`=LR, `r15`=PC), the
//! `xPSR` with its `N`/`Z`/`C`/`V` condition flags, and a simple nested vectored
//! interrupt controller. Almost every instruction is a single 16-bit halfword;
//! the one 32-bit form modelled here is `BL`.
//!
//! The pieces mirror the [`avr`](crate::avr) core so a Pico plugs into the same
//! co-simulation loop as the Uno:
//! - [`isa`] — the decoded-instruction type ([`Instr`]) and operand shapes.
//! - [`decode`](mod@decode) — a table-driven decoder from Thumb halfwords to [`Instr`].
//! - [`core`] — the register file, flat memory, NVIC-lite, and the step loop
//!   ([`CortexM`]), implementing the shared [`McuCore`](crate::McuCore) trait.
//! - [`periph`] — the RP2040 SIO GPIO block ([`Sio`]).
//! - [`board`] — board definitions, starting with the Raspberry Pi [`Pico`].
//!
//! It implements a correct **subset** of ARMv6-M; unimplemented encodings decode
//! to [`Instr::Unknown`]. See [`decode`](mod@decode) for the exact coverage and
//! the documented TODOs.

pub mod board;
pub mod core;
pub mod decode;
pub mod isa;
pub mod periph;

pub use self::core::{Apsr, CortexM};
pub use board::Pico;
pub use decode::decode;
pub use isa::Instr;
pub use periph::{pin_id, Sio, PIN_COUNT};
