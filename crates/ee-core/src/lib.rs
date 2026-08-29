//! # ee-core
//!
//! The shared data model for `ee-assist`: the circuit graph, physical units and
//! quantities, simulation time, and the event queue. This crate is **pure data +
//! logic** — no I/O, no UI, and intended to stay `no_std`-friendly so the same
//! model runs in the TUI, headless, and in WebAssembly.
//!
//! See `docs/ARCHITECTURE.md` for how this fits the rest of the workspace.

pub mod graph;
pub mod time;
pub mod units;
pub mod value;

pub use graph::{Circuit, NetId, PartId};
pub use units::Unit;
pub use value::Value;
