//! Edge-triggered sequential elements: D and JK flip-flops.
//!
//! Gates ([`GateKind`](crate::GateKind)) are purely combinational — their output
//! is a function of their inputs *now*. A flip-flop is different: it carries
//! state and only samples its data inputs on the **rising edge** of its clock.
//! That edge detection, plus the stored bit, is what lives here; [`Sim`] owns a
//! `Vec<SeqElement>` and feeds each one the resolved levels of the nets it reads.
//!
//! Edge detection is a strict `0 → 1` transition on the clock net
//! (`last_clk == Low && clk == High`). Requiring a definite low *before* the high
//! means an `X → 1` or `Z → 1` transition is **not** mistaken for a clock edge —
//! the element only triggers on a clean rising edge, matching real hardware and
//! keeping start-up (`last_clk` seeded to `X`) glitch-free.
//!
//! Both flip-flops carry optional **asynchronous**, active-high `set` and `reset`
//! inputs. These are level-sensitive and dominate the clock: while `reset` is high
//! the output is forced low regardless of the clock, and `set` forces it high
//! (`reset` wins if both are asserted). This models the usual `PRE`/`CLR` pins.

use crate::event::Femtos;
use crate::sim::{DriverId, Net};
use crate::Logic;

/// Which flip-flop a [`SeqElement`] implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlipFlop {
    /// D-type: on a rising clock edge, `Q` takes the value of the `D` input.
    D,
    /// JK-type: on a rising clock edge, `(J, K)` selects hold (`00`), clear
    /// (`01`), set (`10`), or toggle (`11`).
    Jk,
}

/// One flip-flop instance and its stored state.
///
/// Kept `pub(crate)`: user code builds flip-flops through the `Sim::add_*`
/// constructors, which wire the nets and own the scheduling. This type is just
/// the state the kernel steps.
#[derive(Debug, Clone)]
pub(crate) struct SeqElement {
    kind: FlipFlop,
    /// Clock net (edge-sensitive).
    pub(crate) clk: Net,
    /// Data nets: `[D]` for a D-FF, `[J, K]` for a JK-FF.
    pub(crate) data: Vec<Net>,
    /// Asynchronous active-high preset, if wired.
    pub(crate) set: Option<Net>,
    /// Asynchronous active-high clear, if wired.
    pub(crate) reset: Option<Net>,
    /// Driver for the `Q` output.
    pub(crate) q: DriverId,
    /// Driver for the complementary `Q̄` output.
    pub(crate) qbar: DriverId,
    /// Propagation delay from trigger to output change.
    pub(crate) delay: Femtos,
    /// The clock level seen on the previous evaluation (for edge detection).
    last_clk: Logic,
    /// The currently stored bit (`High`/`Low`, or `X` if indeterminate).
    state: Logic,
    /// The last `Q` level actually scheduled, so re-evaluations that don't move
    /// the output schedule nothing (matching the gate memoization in [`Sim`]).
    last_q: Logic,
}

impl SeqElement {
    /// Build a flip-flop. `initial` is the power-on `Q` level (`Q̄` is its
    /// inverse). `last_clk` starts at `X` so the first clean `0 → 1` on the clock
    /// is the first trigger.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        kind: FlipFlop,
        clk: Net,
        data: Vec<Net>,
        set: Option<Net>,
        reset: Option<Net>,
        q: DriverId,
        qbar: DriverId,
        delay: Femtos,
        initial: Logic,
    ) -> Self {
        Self {
            kind,
            clk,
            data,
            set,
            reset,
            q,
            qbar,
            delay,
            last_clk: Logic::X,
            state: initial,
            last_q: initial,
        }
    }

    /// Re-evaluate given the resolved levels of the element's nets. Updates the
    /// stored state and, if the `Q` output moved since it was last scheduled,
    /// returns the new `(Q, Q̄)` pair for the caller to schedule after `delay`.
    ///
    /// `set`/`reset` default to `Low` (not asserted) when the pin isn't wired.
    pub(crate) fn update(
        &mut self,
        clk: Logic,
        data: &[Logic],
        set: Logic,
        reset: Logic,
    ) -> Option<(Logic, Logic)> {
        // Strict rising edge: a definite low followed by a definite high.
        let rising = self.last_clk == Logic::Low && clk == Logic::High;
        self.last_clk = clk;

        let next = if reset == Logic::High {
            Logic::Low // asynchronous clear dominates everything
        } else if set == Logic::High {
            Logic::High // asynchronous preset
        } else if rising {
            match self.kind {
                FlipFlop::D => data[0].sensed(),
                FlipFlop::Jk => jk_next(self.state, data[0], data[1]),
            }
        } else {
            self.state // no trigger: hold
        };

        self.state = next;
        if next != self.last_q {
            self.last_q = next;
            Some((next, next.not()))
        } else {
            None
        }
    }
}

/// The JK next-state function for a rising edge. Any unknown control input makes
/// the next state unknown, except that this is already captured by `not()`/hold
/// semantics on definite inputs.
fn jk_next(q: Logic, j: Logic, k: Logic) -> Logic {
    match (j.sensed(), k.sensed()) {
        (Logic::Low, Logic::Low) => q,            // 00 → hold
        (Logic::Low, Logic::High) => Logic::Low,  // 01 → clear
        (Logic::High, Logic::Low) => Logic::High, // 10 → set
        (Logic::High, Logic::High) => q.not(),    // 11 → toggle
        _ => Logic::X,                            // an unknown control bit
    }
}

#[cfg(test)]
mod tests {
    use super::{jk_next, FlipFlop, SeqElement};
    use crate::sim::{DriverId, Net};
    use crate::Logic::{High, Low, X};

    #[test]
    fn jk_truth_table() {
        assert_eq!(jk_next(Low, Low, Low), Low); // hold 0
        assert_eq!(jk_next(High, Low, Low), High); // hold 1
        assert_eq!(jk_next(High, Low, High), Low); // clear
        assert_eq!(jk_next(Low, High, Low), High); // set
        assert_eq!(jk_next(Low, High, High), High); // toggle 0→1
        assert_eq!(jk_next(High, High, High), Low); // toggle 1→0
        assert_eq!(jk_next(Low, X, Low), X); // unknown control → X
    }

    #[test]
    fn d_flipflop_samples_only_on_rising_edge() {
        let mut ff = SeqElement::new(
            FlipFlop::D,
            Net(0),
            vec![Net(1)],
            None,
            None,
            DriverId(0),
            DriverId(1),
            10,
            Low,
        );
        // D high, but clock still low: no change.
        assert_eq!(ff.update(Low, &[High], Low, Low), None);
        // Rising edge samples D = 1 → Q goes high.
        assert_eq!(ff.update(High, &[High], Low, Low), Some((High, Low)));
        // Clock stays high, D drops: Q holds (edge already consumed).
        assert_eq!(ff.update(High, &[Low], Low, Low), None);
        // Falling edge: still holds.
        assert_eq!(ff.update(Low, &[Low], Low, Low), None);
        // Next rising edge samples D = 0 → Q goes low.
        assert_eq!(ff.update(High, &[Low], Low, Low), Some((Low, High)));
    }

    #[test]
    fn async_reset_dominates_and_holds() {
        let mut ff = SeqElement::new(
            FlipFlop::D,
            Net(0),
            vec![Net(1)],
            None,
            Some(Net(2)),
            DriverId(0),
            DriverId(1),
            10,
            High,
        );
        // Reset asserted forces Q low even with D high and a rising edge.
        assert_eq!(ff.update(High, &[High], Low, High), Some((Low, High)));
        // Reset still asserted: stays low.
        assert_eq!(ff.update(Low, &[High], Low, High), None);
        // Reset released on a non-edge: holds the cleared value.
        assert_eq!(ff.update(Low, &[High], Low, Low), None);
    }
}
