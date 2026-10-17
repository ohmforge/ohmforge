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
//! - [`decode`](mod@decode) — a table-driven decoder from flash words to [`Instr`].
//! - [`core`] — the register file, memory model, interrupt controller, and the
//!   fetch/decode/execute step loop ([`Avr`]).
//! - [`periph`] — the memory-mapped peripherals ([`Peripherals`]): GPIO, the
//!   Timer0/Timer1 counters with PWM, the ADC, and the USART transmitter.
//! - [`board`] — board definitions tying the core + peripherals to a pinout,
//!   starting with the Arduino [`Uno`] (and [`Mega`] on the ATmega2560).
//! - [`variant`] — per-part descriptors ([`AvrVariant`]) that size the shared
//!   core into an ATmega328P, ATmega2560 or ATtiny85.
//!
//! It implements the supported-opcode **subset** correctly rather than the whole
//! ISA; unimplemented encodings decode to [`Instr::Unknown`]. See [`decode`](mod@decode) for
//! the exact coverage and the documented TODOs.

pub mod board;
pub mod core;
pub mod decode;
pub mod isa;
pub mod periph;
pub mod sreg;
pub mod variant;

pub use self::core::Avr;
pub use board::{Mega, Uno};
pub use decode::decode;
pub use isa::{Instr, Ptr, PtrMode};
pub use periph::{pin_id, pin_location, Peripherals, Port, PIN_COUNT};
pub use sreg::Sreg;
pub use variant::{AvrVariant, TimerSpec};
