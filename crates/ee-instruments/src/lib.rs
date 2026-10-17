//! # ee-instruments
//!
//! Virtual instruments layered over the engines and presented as front panels
//! in the TUI: oscilloscope, multimeter, function generator, logic analyzer,
//! bench PSU, Bode/spectrum. Longer term, a LabVIEW-style **dataflow patcher**
//! composes these into custom virtual benches. See docs/FEATURES.md §4.
//!
//! Instruments *consume* signals and *control* sources; they hold no solver
//! state of their own.

mod fmt;

pub mod dmm;
pub mod funcgen;
pub mod oscilloscope;
pub mod psu;

pub use dmm::{measure, DmmMode, DmmReading, Multimeter};
pub use funcgen::{FuncGen, Waveform};
pub use oscilloscope::{CursorReadout, Cursors, Oscilloscope, Slope, Timebase, Trigger};
pub use psu::{BenchPsu, PsuMode, PsuOutput};

/// A named signal an instrument can display (a node voltage, a branch current,
/// an MCU pin, or a math channel).
#[derive(Debug, Clone)]
pub struct SignalRef(pub String);
