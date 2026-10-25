//! # ee-instruments
//!
//! Virtual instruments layered over the engines and presented as front panels
//! in the TUI: oscilloscope, multimeter, function generator, logic analyzer,
//! bench PSU, Bode/spectrum. Longer term, a LabVIEW-style **dataflow patcher**
//! composes these into custom virtual benches. See docs/FEATURES.md §4.
//!
//! Instruments *consume* signals and *control* sources; they hold no solver
//! state of their own.

mod braille;
mod fmt;

pub mod bode;
pub mod dmm;
pub mod fft;
pub mod funcgen;
pub mod logic_analyzer;
pub mod math;
pub mod oscilloscope;
pub mod patch;
pub mod psu;
pub mod spectrum;

pub use bode::{corner_3db, Bode};
pub use dmm::{measure, DmmMode, DmmReading, Multimeter};
pub use fft::{fft, magnitude_spectrum, peak_bin, Window};
pub use funcgen::{FuncGen, Waveform};
pub use logic_analyzer::{
    decode_i2c, decode_spi, decode_uart, DigitalChannel, I2cEvent, Level, LogicAnalyzer, Parity,
    SpiConfig, UartConfig, UartFrame,
};
pub use math::{math_channel, MathOp};
pub use oscilloscope::{CursorReadout, Cursors, Oscilloscope, Slope, Timebase, Trigger};
pub use patch::{
    Block, Bound, Check, CheckResult, Filter, FilterKind, Patch, PatchError, PatchRun,
    PluginHandle, Port, ProcessingBlock, TestReport, TestSequence, VirtualInstrument, Wire,
};
pub use psu::{BenchPsu, PsuMode, PsuOutput};
pub use spectrum::Spectrum;

/// A named signal an instrument can display (a node voltage, a branch current,
/// an MCU pin, or a math channel).
#[derive(Debug, Clone)]
pub struct SignalRef(pub String);
