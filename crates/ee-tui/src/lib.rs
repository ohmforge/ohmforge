//! # ee-tui
//!
//! The terminal UI (`ratatui` + `crossterm`): schematic rendering with a live
//! voltage/current overlay, high-resolution **braille** waveform plots, dockable
//! panels (schematic, scope, shell, AI chat, parts palette, instruments), a
//! command palette, and mouse-driven part placement. Works over SSH.
//!
//! Rendering is decoupled from simulation via snapshots, so a slow redraw never
//! stalls the solver. See docs/ARCHITECTURE.md § "The TUI." (Roadmap M0+.)

pub mod plot;

pub use plot::Plot;
