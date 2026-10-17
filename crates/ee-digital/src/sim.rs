//! The event-driven simulator: nets, drivers, gates, and the run loop.
//!
//! The model is a small netlist. A [`Net`] is an electrically common node whose
//! level is the [resolution](Logic::resolve) of every [`DriverId`] attached to
//! it — a gate output, or an external stimulus. A [gate](GateKind) reads one or
//! more input nets and, a propagation `delay` after any of them changes, drives
//! its output net to the gate's newly computed level.
//!
//! Evaluation is purely event-driven: nothing is recomputed on a tick where no
//! net it depends on changed. Applying an event updates one driver, re-resolves
//! its net, and — only if the net's resolved level actually moved — re-evaluates
//! the gates that read it, scheduling their outputs `delay` into the future.
//! This is *transport* delay: a gate collapses to one scheduled transition per
//! genuine output change, so a stable circuit quiesces and the queue drains.
//!
//! # Example — a 2-input AND
//! ```
//! # use ee_digital::{Logic, Sim};
//! let mut sim = Sim::new();
//! let (a, b, y) = (sim.add_net(), sim.add_net(), sim.add_net());
//! let da = sim.add_input(a, Logic::High);
//! let _db = sim.add_input(b, Logic::High);
//! sim.add_gate(ee_digital::GateKind::And, &[a, b], y, 5, Logic::X);
//!
//! sim.run_until(4); // before the gate delay elapses
//! assert_eq!(sim.value(y), Logic::X);
//! sim.run_until(5); // delay elapsed: 1 AND 1 = 1
//! assert_eq!(sim.value(y), Logic::High);
//!
//! sim.set_input(da, Logic::Low, 20); // drive A low at t = 20
//! sim.run_until(25);                 // + 5 fs delay
//! assert_eq!(sim.value(y), Logic::Low);
//! ```

use crate::event::{EventQueue, Femtos};
use crate::Logic;

/// A signal net (an electrically common node), addressed by index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Net(pub usize);

/// A single driver contributing a level to a net (a gate output or an external
/// stimulus). A net with more than one driver resolves them via
/// [`Logic::resolve`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DriverId(pub usize);

/// A gate instance, addressed by index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GateId(pub usize);

/// The built-in combinational gate kinds. Each reduces its input levels with the
/// matching [`Logic`] truth table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateKind {
    /// Inverter (exactly one input).
    Not,
    /// AND of all inputs.
    And,
    /// OR of all inputs.
    Or,
    /// XOR (parity) of all inputs.
    Xor,
}

impl GateKind {
    /// Evaluate the gate over its (already resolved) input levels.
    fn eval(self, inputs: &[Logic]) -> Logic {
        match self {
            GateKind::Not => {
                debug_assert_eq!(inputs.len(), 1, "a NOT gate takes exactly one input");
                inputs[0].not()
            }
            GateKind::And => inputs.iter().copied().fold(Logic::High, Logic::and),
            GateKind::Or => inputs.iter().copied().fold(Logic::Low, Logic::or),
            GateKind::Xor => inputs.iter().copied().fold(Logic::Low, Logic::xor),
        }
    }
}

/// One driver's contribution to a net.
#[derive(Debug, Clone, Copy)]
struct Driver {
    net: Net,
    value: Logic,
}

/// A placed gate.
#[derive(Debug, Clone)]
struct Gate {
    kind: GateKind,
    inputs: Vec<Net>,
    output: DriverId,
    delay: Femtos,
    /// The last output level this gate scheduled (or its reset value). Comparing
    /// against it collapses re-evaluations to one event per genuine transition.
    last_output: Logic,
}

/// A net's current resolved level plus its connectivity.
#[derive(Debug, Clone, Default)]
struct NetState {
    value: Logic,
    drivers: Vec<DriverId>,
    /// Gates that read this net, in the order they were added — a fixed,
    /// deterministic propagation order.
    fanout: Vec<GateId>,
}

/// The event-driven digital simulator.
#[derive(Debug, Default)]
pub struct Sim {
    nets: Vec<NetState>,
    drivers: Vec<Driver>,
    gates: Vec<Gate>,
    queue: EventQueue,
    now: Femtos,
    started: bool,
}

impl Sim {
    /// A fresh, empty simulator at time zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// The current simulation time.
    pub fn now(&self) -> Femtos {
        self.now
    }

    /// Add a net, initially high-impedance (`Z`) until a driver is attached.
    pub fn add_net(&mut self) -> Net {
        let id = Net(self.nets.len());
        self.nets.push(NetState::default());
        id
    }

    /// Attach an external stimulus driver to `net` with an initial level. Drive
    /// it over time with [`set_input`](Sim::set_input). Call before
    /// [`run`](Sim::run_until)/[`step`](Sim::step).
    pub fn add_input(&mut self, net: Net, initial: Logic) -> DriverId {
        self.new_driver(net, initial)
    }

    /// Add a gate reading `inputs`, driving `output` through `delay`
    /// femtoseconds, with `initial` as the output's reset level (what it drives
    /// before the first evaluated transition). Returns the gate's id.
    ///
    /// `initial` lets a sequential loop (a ring oscillator, a latch) start from a
    /// known state instead of being stuck at `X`.
    pub fn add_gate(
        &mut self,
        kind: GateKind,
        inputs: &[Net],
        output: Net,
        delay: Femtos,
        initial: Logic,
    ) -> GateId {
        let out_driver = self.new_driver(output, initial);
        let gid = GateId(self.gates.len());
        for &net in inputs {
            self.nets[net.0].fanout.push(gid);
        }
        self.gates.push(Gate {
            kind,
            inputs: inputs.to_vec(),
            output: out_driver,
            delay,
            last_output: initial,
        });
        gid
    }

    /// Register a new driver on `net` and fold its initial level into the net's
    /// resolved value. Used for both external inputs and gate outputs.
    fn new_driver(&mut self, net: Net, initial: Logic) -> DriverId {
        let id = DriverId(self.drivers.len());
        self.drivers.push(Driver {
            net,
            value: initial,
        });
        self.nets[net.0].drivers.push(id);
        self.nets[net.0].value = self.resolved(net);
        id
    }

    /// Schedule `driver` to begin driving `value` at absolute time `at`. Scheduling
    /// in the past is clamped to now.
    pub fn set_input(&mut self, driver: DriverId, value: Logic, at: Femtos) {
        self.queue.schedule(at.max(self.now), driver, value);
    }

    /// The current resolved level of `net`.
    pub fn value(&self, net: Net) -> Logic {
        self.nets[net.0].value
    }

    /// The time of the next pending event, if any.
    pub fn next_event_time(&self) -> Option<Femtos> {
        self.queue.peek_time()
    }

    /// The resolution of every driver on `net` (`Z` if undriven).
    fn resolved(&self, net: Net) -> Logic {
        self.nets[net.0]
            .drivers
            .iter()
            .fold(Logic::Z, |acc, &d| acc.resolve(self.drivers[d.0].value))
    }

    /// Evaluate a gate over its inputs' current resolved levels.
    fn eval_gate(&self, gid: GateId) -> Logic {
        let g = &self.gates[gid.0];
        let vals: Vec<Logic> = g.inputs.iter().map(|&n| self.nets[n.0].value).collect();
        g.kind.eval(&vals)
    }

    /// Perform the initial evaluation pass, scheduling the first transition of
    /// any gate whose reset output disagrees with its inputs. Idempotent.
    fn start(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        for i in 0..self.gates.len() {
            self.reeval_gate(GateId(i));
        }
    }

    /// Re-evaluate one gate; if its output changed from the last value it
    /// scheduled, schedule the new level `delay` into the future.
    fn reeval_gate(&mut self, gid: GateId) {
        let out = self.eval_gate(gid);
        let g = &mut self.gates[gid.0];
        if out != g.last_output {
            g.last_output = out;
            let (driver, delay) = (g.output, g.delay);
            self.queue.schedule(self.now + delay, driver, out);
        }
    }

    /// Apply one event: update its driver, re-resolve the net, and — only if the
    /// net's level actually moved — re-evaluate the gates that read it.
    fn apply(&mut self, driver: DriverId, value: Logic) {
        let d = &mut self.drivers[driver.0];
        if d.value == value {
            return; // this driver already drives that level
        }
        d.value = value;
        let net = d.net;

        let resolved = self.resolved(net);
        if self.nets[net.0].value == resolved {
            return; // another driver still pins the net; no net-level change
        }
        self.nets[net.0].value = resolved;

        // Fanout order is fixed (registration order), so propagation is
        // deterministic. Clone the small list to release the borrow on `nets`.
        let fanout = self.nets[net.0].fanout.clone();
        for gid in fanout {
            self.reeval_gate(gid);
        }
    }

    /// Process the single earliest pending event, advancing time to it. Returns
    /// that event's time, or `None` if the queue is empty.
    pub fn step(&mut self) -> Option<Femtos> {
        if !self.started {
            self.start();
        }
        let ev = self.queue.pop()?;
        self.now = ev.time;
        self.apply(ev.driver, ev.value);
        Some(ev.time)
    }

    /// Run until simulation time `until`, draining every event scheduled at or
    /// before it in nondecreasing time order. Time advances to `until` even if
    /// the queue empties earlier.
    pub fn run_until(&mut self, until: Femtos) {
        if !self.started {
            self.start();
        }
        while let Some(t) = self.queue.peek_time() {
            if t > until {
                break;
            }
            let ev = self.queue.pop().expect("peek_time just reported an event");
            self.now = ev.time;
            self.apply(ev.driver, ev.value);
        }
        if until > self.now {
            self.now = until;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GateKind, Sim};
    use crate::Logic;

    /// An odd ring of inverters oscillates with period `2 · N · d`. This is the
    /// `CP-BRD-M1.1` acceptance gate.
    #[test]
    fn ring_oscillator_period_is_two_n_d() {
        const N: usize = 3;
        const D: u64 = 10; // per-gate propagation delay, in fs
        let expected_period = 2 * N as u64 * D; // = 60 fs

        let mut sim = Sim::new();
        let nets: Vec<_> = (0..N).map(|_| sim.add_net()).collect();

        // inv_i reads net_i and drives net_{i+1 mod N}. Seed the outputs in an
        // alternating 1,0,1,0,… pattern; because N is odd the loop cannot sit
        // static, leaving exactly one edge to travel around the ring.
        for i in 0..N {
            let input = nets[i];
            let output = nets[(i + 1) % N];
            let seed = if i % 2 == 0 { Logic::High } else { Logic::Low };
            sim.add_gate(GateKind::Not, &[input], output, D, seed);
        }

        let watch = nets[0];
        sim.start(); // force the initial pass so we can observe scheduled events

        // Collect rising edges of the watched net over several periods.
        let horizon = 4 * expected_period;
        let mut rises = Vec::new();
        let mut saw_low = false;
        let mut prev = sim.value(watch);
        while let Some(t) = sim.next_event_time() {
            if t > horizon {
                break;
            }
            sim.step();
            let now = sim.value(watch);
            if prev != Logic::High && now == Logic::High {
                rises.push(sim.now());
            }
            if now == Logic::Low {
                saw_low = true;
            }
            prev = now;
        }

        // It must actually toggle between definite levels — never stuck at X.
        assert!(
            rises.len() >= 2,
            "expected at least two rising edges, got {rises:?}"
        );
        assert!(saw_low, "watched net never reached a definite low — stuck?");
        assert!(matches!(sim.value(watch), Logic::Low | Logic::High));

        // Consecutive rising edges are one full period apart, every time.
        for pair in rises.windows(2) {
            assert_eq!(
                pair[1] - pair[0],
                expected_period,
                "measured period {} != expected {expected_period} (rises {rises:?})",
                pair[1] - pair[0],
            );
        }
    }

    #[test]
    fn and_gate_switches_after_its_delay() {
        let mut sim = Sim::new();
        let (a, b, y) = (sim.add_net(), sim.add_net(), sim.add_net());
        sim.add_input(a, Logic::High);
        sim.add_input(b, Logic::High);
        sim.add_gate(GateKind::And, &[a, b], y, 5, Logic::X);

        // Output holds its reset value until the delay elapses.
        sim.run_until(4);
        assert_eq!(sim.value(y), Logic::X);
        sim.run_until(5);
        assert_eq!(sim.value(y), Logic::High, "1 AND 1 = 1 after the delay");
    }

    #[test]
    fn or_gate_tracks_a_changing_input() {
        let mut sim = Sim::new();
        let (a, b, y) = (sim.add_net(), sim.add_net(), sim.add_net());
        let da = sim.add_input(a, Logic::Low);
        sim.add_input(b, Logic::Low);
        sim.add_gate(GateKind::Or, &[a, b], y, 3, Logic::X);

        sim.run_until(3);
        assert_eq!(sim.value(y), Logic::Low, "0 OR 0 = 0");

        sim.set_input(da, Logic::High, 10);
        sim.run_until(12);
        assert_eq!(sim.value(y), Logic::Low, "input change not yet propagated");
        sim.run_until(13);
        assert_eq!(sim.value(y), Logic::High, "1 OR 0 = 1 after the delay");
    }

    #[test]
    fn two_agreeing_drivers_resolve_and_conflicts_go_x() {
        // A net driven by two external inputs resolves via Logic::resolve.
        let mut sim = Sim::new();
        let n = sim.add_net();
        let d0 = sim.add_input(n, Logic::High);
        let d1 = sim.add_input(n, Logic::High);
        sim.run_until(0);
        assert_eq!(sim.value(n), Logic::High, "both drive high → high");

        sim.set_input(d1, Logic::Low, 5); // now they conflict
        sim.run_until(5);
        assert_eq!(sim.value(n), Logic::X, "conflicting drivers → X");

        sim.set_input(d0, Logic::Low, 10); // agree again, low
        sim.run_until(10);
        assert_eq!(sim.value(n), Logic::Low, "both drive low → low");
    }
}
