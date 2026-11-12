//! An instruction-accurate ARM Cortex-M core covering ARMv6-M (Thumb, the
//! Cortex-M0+ as on the RP2040 / Raspberry Pi Pico) and the ARMv7-M / Thumb-2
//! additions (the Cortex-M3/M4, as on STM32-class parts).
//!
//! ARMv6-M is the Thumb instruction subset the Cortex-M0/M0+ implements: a flat
//! 32-bit memory, sixteen 32-bit registers (`r13`=SP, `r14`=LR, `r15`=PC), the
//! `xPSR` with its `N`/`Z`/`C`/`V` condition flags, and a simple nested vectored
//! interrupt controller. Almost every instruction is a single 16-bit halfword;
//! the one 32-bit form it needs is `BL`. The ARMv7-M superset adds the Thumb-2
//! **32-bit** encodings (data-processing with immediates and shifts, the
//! multiplies, bitfield ops, `MOVW`/`MOVT`, `CLZ`/`RBIT`, `LDRD`/`STRD`, wide
//! `LDM`/`STM`, `MRS`/`MSR`, and wider branches) plus the `IT` block, reusing the
//! same register file, flags, memory and exception mechanism.
//!
//! The pieces mirror the [`avr`](crate::avr) core so a Pico (or an STM32 board)
//! plugs into the same co-simulation loop as the Uno:
//! - [`isa`] — the decoded-instruction type ([`Instr`]) and operand shapes.
//! - [`decode`](mod@decode) — a table-driven decoder from Thumb / Thumb-2
//!   halfwords to [`Instr`].
//! - [`core`] — the register file, flat memory, NVIC-lite, and the step loop
//!   ([`CortexM`]), implementing the shared [`McuCore`](crate::McuCore) trait.
//! - [`periph`] — the RP2040 SIO GPIO block ([`Sio`]) and an STM32-class GPIO
//!   port ([`Stm32Gpio`]).
//! - [`board`] — board definitions: the Raspberry Pi [`Pico`] and an
//!   STM32-class [`Stm32`].
//!
//! It implements a correct **subset** of the ISA; unimplemented encodings decode
//! to [`Instr::Unknown`]. See [`decode`](mod@decode) for the exact coverage and
//! the documented TODOs.

pub mod board;
pub mod core;
pub mod decode;
pub mod isa;
pub mod periph;

pub use self::core::{Apsr, CortexM};
pub use board::{Pico, Stm32};
pub use decode::decode;
pub use isa::Instr;
pub use periph::{pin_id, stm32_pin_id, Sio, Stm32Gpio, PIN_COUNT, STM32_PIN_COUNT};
