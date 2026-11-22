//! # ohmforge-fmt
//!
//! File formats and ecosystem interop. The native project format is
//! human-readable and diff-friendly so circuits live happily in git. Also:
//! SPICE netlist import/export, KiCad netlist import, bill-of-materials (BOM)
//! export, and waveform export to CSV/VCD. See docs/FEATURES.md §9. (Roadmap M0
//! native; M1 SPICE; M2 VCD; M3 KiCad/BOM.)
//!
//! A format-by-format guide with runnable examples (incl. the SPICE `m`-vs-`meg`
//! gotcha) lives in `docs/guide/formats.md`, and real files in
//! `examples/formats/` are checked by `cargo test -p ohmforge-fmt --test examples`.
//!
//! At M3 the ecosystem reach widens: KiCad netlist import and BOM export
//! (`CP-AF-M3.1`) and a Fritzing-style sketch exporter ([`fritzing`],
//! `CP-AF-M3.4`) that writes the `.fz` XML describing parts and their wiring.
//!
//! At M10 the RF reach lands: a Touchstone `.sNp` exporter ([`touchstone`],
//! `CP-AF-M10.1`) that writes a swept 2-port S-parameter result as the `.s2p`
//! every RF tool reads — taking an [`ohmforge_core::Complex`] view so the solver
//! stays a dev-dependency — and SPICE `SINE`/`PULSE` source parsing in the
//! [`spice`] importer so an imported deck's time-varying sources carry a
//! [`Waveform`](ohmforge_core::Waveform).

pub mod bom;
pub mod fritzing;
pub mod kicad;
pub mod netlist;
pub mod project;
pub mod spice;
pub mod touchstone;
pub mod waveform;

pub use bom::to_csv as to_bom_csv;
pub use fritzing::export as export_fritzing;
pub use kicad::{import as import_kicad, KicadError};
pub use netlist::{parse as parse_netlist, NetlistError};
pub use project::{load as load_project, save as save_project, ProjectError};
pub use spice::{
    export as export_spice, import as import_spice, import_with_notes as import_spice_with_notes,
    ModelType, Note as SpiceNote, SpiceError, SpiceModel,
};
pub use touchstone::{
    export as export_touchstone, parse as parse_touchstone, Touchstone, TouchstoneError,
};
pub use waveform::{to_csv, to_vcd, WaveformError};
