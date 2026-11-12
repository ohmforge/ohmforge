//! # ee-core
//!
//! The shared data model for `ee-assist`: the circuit graph, physical units and
//! quantities, simulation time, the event queue, and the [`Signal`] result type.
//! This crate is **pure data + logic** — no I/O, no UI — and stays WASM-clean so
//! the same model runs in the TUI, headless, and in the browser.
//!
//! See `docs/ARCHITECTURE.md` for how this fits the rest of the workspace.

pub mod graph;
pub mod signal;
pub mod time;
pub mod units;
pub mod value;

pub use graph::{
    BjtModel, Circuit, DiodeModel, Line, LineKind, MosfetModel, NetId, PartId, Polarity, Waveform,
};
pub use signal::{Complex, Domain, Signal, SignalData};
pub use units::Unit;
pub use value::Value;
