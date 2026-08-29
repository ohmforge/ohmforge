//! # ee-devices
//!
//! Concrete device models and their stamping/behavior, plus **board
//! definitions** (Arduino Uno/Nano/Mega, Raspberry Pi Pico, ESP32-class, and
//! user-defined custom boards) and a searchable **parts library**.
//!
//! A board is a manifest: MCU, pinout, and onboard parts. Custom boards drop in
//! by writing a manifest — see docs/FEATURES.md §3. (Roadmap M1–M3.)

/// A board the user can `board add`.
#[derive(Debug, Clone)]
pub struct BoardDef {
    pub model: String,
    /// e.g. "atmega328p".
    pub mcu: String,
    /// Named pins → their role/net mapping. TODO(M1): fill for Uno.
    pub pins: Vec<String>,
}
