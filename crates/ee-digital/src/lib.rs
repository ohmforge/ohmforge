//! # ee-digital
//!
//! Event-driven digital logic simulation. Signals change only at scheduled
//! events; gates schedule future events after their propagation delay. This is
//! far cheaper than analog MNA for pure logic and is what lets fast MCU pins
//! interact with slow analog subcircuits (via mixed-signal boundary elements).
//!
//! Roadmap: M1 = gates/flip-flops/counters + X/Z; M2 = memories/buses/tri-state
//! + protocol decode; M3 = HDL-lite. (docs/ROADMAP.md)

/// A four-valued logic level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Logic {
    Low,
    High,
    /// Unknown / undriven-contended.
    X,
    /// High-impedance.
    Z,
}
