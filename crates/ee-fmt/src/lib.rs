//! # ee-fmt
//!
//! File formats and ecosystem interop. The native project format is
//! human-readable and diff-friendly so circuits live happily in git. Also:
//! SPICE netlist import/export, KiCad netlist read, and waveform export to
//! CSV/VCD. See docs/FEATURES.md §9. (Roadmap M0 native; M1 SPICE; M2 KiCad/VCD.)

pub mod netlist;

pub use netlist::{parse as parse_netlist, NetlistError};
