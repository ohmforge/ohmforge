//! The time-ordered event queue that drives the simulation.
//!
//! Time is measured in **femtoseconds** as a `u64` tick ([`Femtos`]). Integer
//! time keeps event ordering exact — there is no float-equality hazard when two
//! events land on the same tick — which a compute core that must be
//! deterministic requires. At 1 fs resolution a `u64` still spans ~5 hours of
//! simulated time, far beyond any digital horizon we care about.
//!
//! The queue is a binary min-heap keyed by `(time, seq)`. `seq` is a
//! monotonically increasing insertion counter, so events scheduled for the same
//! tick pop in the order they were scheduled (FIFO). That tie-break is what makes
//! a run reproducible regardless of the heap's internal layout.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use crate::sim::DriverId;
use crate::Logic;

/// Simulation time in femtoseconds (1 fs = 1e-15 s).
pub type Femtos = u64;

/// A scheduled change: at `time`, driver `driver` begins driving `value`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    /// When the change takes effect.
    pub time: Femtos,
    /// Insertion order, the deterministic tie-breaker among equal `time`s.
    pub seq: u64,
    /// The driver whose output level changes.
    pub driver: DriverId,
    /// The level it drives from `time` onward.
    pub value: Logic,
}

impl Ord for Event {
    fn cmp(&self, other: &Self) -> Ordering {
        // `BinaryHeap` is a max-heap, so invert: the "greatest" event under this
        // order is the one to process next — earliest time, then lowest seq.
        other
            .time
            .cmp(&self.time)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

impl PartialOrd for Event {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A min-heap of [`Event`]s keyed by `(time, seq)`.
#[derive(Debug, Default)]
pub struct EventQueue {
    heap: BinaryHeap<Event>,
    next_seq: u64,
}

impl EventQueue {
    /// An empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Schedule `value` on `driver` at absolute time `time`, assigning the next
    /// sequence number so ties break in insertion order.
    pub fn schedule(&mut self, time: Femtos, driver: DriverId, value: Logic) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.heap.push(Event {
            time,
            seq,
            driver,
            value,
        });
    }

    /// The time of the earliest pending event, if any.
    pub fn peek_time(&self) -> Option<Femtos> {
        self.heap.peek().map(|e| e.time)
    }

    /// Pop the earliest event (lowest time, then earliest scheduled).
    pub fn pop(&mut self) -> Option<Event> {
        self.heap.pop()
    }

    /// Whether the queue has no pending events.
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    /// Number of pending events.
    pub fn len(&self) -> usize {
        self.heap.len()
    }
}

#[cfg(test)]
mod tests {
    use super::{EventQueue, Femtos};
    use crate::sim::DriverId;
    use crate::Logic;

    /// Drain the queue, returning each event's `(time, driver index)` in pop
    /// order.
    fn drain(q: &mut EventQueue) -> Vec<(Femtos, usize)> {
        let mut out = Vec::new();
        while let Some(e) = q.pop() {
            out.push((e.time, e.driver.0));
        }
        out
    }

    #[test]
    fn pops_in_time_order_regardless_of_insertion_order() {
        let mut q = EventQueue::new();
        q.schedule(30, DriverId(0), Logic::High);
        q.schedule(10, DriverId(1), Logic::Low);
        q.schedule(20, DriverId(2), Logic::X);
        assert_eq!(drain(&mut q), vec![(10, 1), (20, 2), (30, 0)]);
    }

    #[test]
    fn ties_break_by_insertion_order() {
        // Three events on the same tick must come out first-in-first-out.
        let mut q = EventQueue::new();
        q.schedule(5, DriverId(10), Logic::Low);
        q.schedule(5, DriverId(11), Logic::High);
        q.schedule(5, DriverId(12), Logic::X);
        assert_eq!(drain(&mut q), vec![(5, 10), (5, 11), (5, 12)]);
    }

    #[test]
    fn interleaved_times_and_ties_are_fully_deterministic() {
        let mut q = EventQueue::new();
        q.schedule(5, DriverId(0), Logic::Low);
        q.schedule(1, DriverId(1), Logic::Low);
        q.schedule(5, DriverId(2), Logic::Low); // same tick as DriverId(0), later
        q.schedule(1, DriverId(3), Logic::Low); // same tick as DriverId(1), later
        assert_eq!(q.len(), 4);
        assert_eq!(q.peek_time(), Some(1));
        assert_eq!(drain(&mut q), vec![(1, 1), (1, 3), (5, 0), (5, 2)]);
        assert!(q.is_empty());
    }
}
