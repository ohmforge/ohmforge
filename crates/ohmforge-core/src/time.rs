//! Simulation time. Kept as `f64` seconds for now; a fixed-point femtosecond
//! representation may replace this if event-queue determinism demands it.

/// A point in simulation time, in seconds.
pub type Seconds = f64;

/// A scheduled simulation event (used by the event-driven digital path and the
/// MCU co-simulation bridge). `TODO(M1)`: back this with a binary heap.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Event {
    pub at: Seconds,
}
