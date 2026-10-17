//! # ee-fmt
//!
//! File formats and ecosystem interop. The native project format is
//! human-readable and diff-friendly so circuits live happily in git. Also:
//! SPICE netlist import/export, KiCad netlist read, and waveform export to
//! CSV/VCD. See docs/FEATURES.md §9. (Roadmap M0 native; M1 SPICE; M2 KiCad/VCD.)

pub mod netlist;
pub mod project;
pub mod spice;
pub mod waveform;

pub use netlist::{parse as parse_netlist, NetlistError};
pub use project::{load as load_project, save as save_project, ProjectError};
pub use spice::{export as export_spice, import as import_spice, SpiceError};
pub use waveform::{to_csv, to_vcd, WaveformError};
