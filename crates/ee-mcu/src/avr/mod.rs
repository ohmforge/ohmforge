//! An instruction-accurate AVR core, modelled on the ATmega328P (Arduino
//! Uno/Nano).
//!
//! The AVR is an 8-bit Harvard-architecture RISC MCU: a 16-bit flash program
//! space addressed in words, a separate byte-addressed data space (holding the
//! register file, the I/O registers and SRAM), and a status register ([`Sreg`])
//! whose flags drive the conditional branches.
//!
//! The pieces:
//! - [`sreg`] — the status register and its eight flags.
//! - [`isa`] — the decoded-instruction type ([`Instr`]) and operand shapes.
//! - [`decode`] — a table-driven decoder from flash words to [`Instr`].
//! - [`core`] — the register file, memory model, interrupt controller, and the
//!   fetch/decode/execute step loop ([`Avr`]).
//!
//! It implements the supported-opcode **subset** correctly rather than the whole
//! ISA; unimplemented encodings decode to [`Instr::Unknown`]. See [`decode`] for
//! the exact coverage and the documented TODOs.

pub mod core;
pub mod decode;
pub mod isa;
pub mod sreg;

pub use self::core::Avr;
pub use decode::decode;
pub use isa::{Instr, Ptr, PtrMode};
pub use sreg::Sreg;
