//! # ohmforge-digital
//!
//! Event-driven digital logic simulation. Signals change only at scheduled
//! events; gates schedule future events after their propagation delay. This is
//! far cheaper than analog MNA for pure logic and is what lets fast MCU pins
//! interact with slow analog subcircuits (via mixed-signal boundary elements).
//!
//! The kernel has three pieces:
//! - [`Logic`] — four-valued logic (`0`/`1`/`X`/`Z`) with `X`-propagating truth
//!   tables and multi-driver resolution.
//! - [`EventQueue`] — a deterministic min-heap of timed events, keyed in integer
//!   femtoseconds ([`Femtos`]) so there are no float-equality ordering hazards.
//! - [`Sim`] — the netlist (nets, drivers, gates) and the run loop that drains
//!   due events, re-resolves nets, and re-evaluates driven gates.
//!
//! The crate is pure compute: no clock, no randomness, no I/O, and it builds for
//! `wasm32-unknown-unknown`, matching the determinism rules the rest of the
//! compute core follows.
//!
//! Roadmap: M1 = gates/flip-flops/counters + X/Z; M2 = memories/buses/tri-state
//! + protocol decode; M3 = HDL-lite. (docs/ROADMAP.md)

pub mod components;
pub mod event;
pub mod hdl;
pub mod logic;
pub mod seq;
pub mod sim;

pub use components::{add_tristate_bus, RippleCounter, TriStateDriver};
pub use event::{Event, EventQueue, Femtos};
pub use hdl::{Expr, ExprError, Ram, Rom, TruthTable};
pub use logic::Logic;
pub use seq::FlipFlop;
pub use sim::{BlockId, DriverId, GateId, GateKind, Net, SeqId, Sim};
