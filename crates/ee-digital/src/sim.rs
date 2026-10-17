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
use crate::seq::{FlipFlop, SeqElement};
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

/// A flip-flop (sequential element) instance, addressed by index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SeqId(pub usize);

/// Something that reads a net and must be re-evaluated when that net changes: a
/// combinational [`Gate`] or a sequential [`SeqElement`]. Stored per-net in
/// registration order so propagation stays deterministic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sink {
    Gate(GateId),
    Seq(SeqId),
}

/// The built-in combinational gate kinds. Each reduces its input levels with the
/// matching [`Logic`] truth table.
///
/// The reducing gates (`And`/`Nand`/`Or`/`Nor`/`Xor`/`Xnor`) fold over *all* their
/// inputs, so a two-input NAND and a three-input NAND are the same `GateKind` with
/// different fan-in. `Not` and `Buf` take exactly one input; `TriState` takes
/// exactly two (data, enable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateKind {
    /// Inverter (exactly one input).
    Not,
    /// Non-inverting buffer (exactly one input): passes its input level, with a
    /// floating (`Z`) input sensed as unknown (`X`).
    Buf,
    /// AND of all inputs.
    And,
    /// NAND — inverted AND of all inputs.
    Nand,
    /// OR of all inputs.
    Or,
    /// NOR — inverted OR of all inputs.
    Nor,
    /// XOR (parity) of all inputs.
    Xor,
    /// XNOR — inverted XOR (even-parity) of all inputs.
    Xnor,
    /// Tri-state buffer: inputs are `[data, enable]`. When `enable` is high the
    /// output follows `data`; when `enable` is low the output is high-impedance
    /// (`Z`) so other drivers on the net win; an unknown `enable` yields `X`.
    TriState,
}

impl GateKind {
    /// Evaluate the gate over its (already resolved) input levels.
    fn eval(self, inputs: &[Logic]) -> Logic {
        match self {
            GateKind::Not => {
                debug_assert_eq!(inputs.len(), 1, "a NOT gate takes exactly one input");
                inputs[0].not()
            }
            GateKind::Buf => {
                debug_assert_eq!(inputs.len(), 1, "a buffer takes exactly one input");
                inputs[0].sensed()
            }
            GateKind::And => inputs.iter().copied().fold(Logic::High, Logic::and),
            GateKind::Nand => inputs.iter().copied().fold(Logic::High, Logic::and).not(),
            GateKind::Or => inputs.iter().copied().fold(Logic::Low, Logic::or),
            GateKind::Nor => inputs.iter().copied().fold(Logic::Low, Logic::or).not(),
            GateKind::Xor => inputs.iter().copied().fold(Logic::Low, Logic::xor),
            GateKind::Xnor => inputs.iter().copied().fold(Logic::Low, Logic::xor).not(),
            GateKind::TriState => {
                debug_assert_eq!(inputs.len(), 2, "a tri-state buffer takes [data, enable]");
                match inputs[1].sensed() {
                    Logic::High => inputs[0].sensed(), // enabled: drive the data level
                    Logic::Low => Logic::Z,            // disabled: release the net
                    _ => Logic::X,                     // unknown enable
                }
            }
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
    /// Sinks (gates and flip-flops) that read this net, in the order they were
    /// added — a fixed, deterministic propagation order.
    fanout: Vec<Sink>,
}

/// The event-driven digital simulator.
#[derive(Debug, Default)]
pub struct Sim {
    nets: Vec<NetState>,
    drivers: Vec<Driver>,
    gates: Vec<Gate>,
    seq: Vec<SeqElement>,
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
            self.nets[net.0].fanout.push(Sink::Gate(gid));
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

    /// Add an edge-triggered **D flip-flop**: on each rising edge of `clk`, `q`
    /// takes the level of `d` (and `qbar` its complement) after `delay`
    /// femtoseconds. `initial` is the power-on `q` level. No asynchronous
    /// set/reset — see [`add_d_flipflop_sr`](Sim::add_d_flipflop_sr) for those.
    pub fn add_d_flipflop(
        &mut self,
        clk: Net,
        d: Net,
        q: Net,
        qbar: Net,
        delay: Femtos,
        initial: Logic,
    ) -> SeqId {
        self.add_d_flipflop_sr(clk, d, None, None, q, qbar, delay, initial)
    }

    /// Add a D flip-flop with optional asynchronous active-high `set` (preset)
    /// and `reset` (clear) nets. Asserting a level-sensitive async pin forces the
    /// output regardless of the clock; `reset` dominates `set`.
    #[allow(clippy::too_many_arguments)]
    pub fn add_d_flipflop_sr(
        &mut self,
        clk: Net,
        d: Net,
        set: Option<Net>,
        reset: Option<Net>,
        q: Net,
        qbar: Net,
        delay: Femtos,
        initial: Logic,
    ) -> SeqId {
        self.add_flipflop(FlipFlop::D, clk, &[d], set, reset, q, qbar, delay, initial)
    }

    /// Add an edge-triggered **JK flip-flop**: on each rising edge of `clk`,
    /// `(j, k)` selects hold (`00`), clear (`01`), set (`10`), or toggle (`11`).
    /// Tying `j` and `k` high makes a toggle (T) flip-flop — the building block of
    /// the ripple counter. No asynchronous set/reset.
    #[allow(clippy::too_many_arguments)]
    pub fn add_jk_flipflop(
        &mut self,
        clk: Net,
        j: Net,
        k: Net,
        q: Net,
        qbar: Net,
        delay: Femtos,
        initial: Logic,
    ) -> SeqId {
        self.add_jk_flipflop_sr(clk, j, k, None, None, q, qbar, delay, initial)
    }

    /// Add a JK flip-flop with optional asynchronous active-high `set`/`reset`
    /// nets (see [`add_d_flipflop_sr`](Sim::add_d_flipflop_sr)).
    #[allow(clippy::too_many_arguments)]
    pub fn add_jk_flipflop_sr(
        &mut self,
        clk: Net,
        j: Net,
        k: Net,
        set: Option<Net>,
        reset: Option<Net>,
        q: Net,
        qbar: Net,
        delay: Femtos,
        initial: Logic,
    ) -> SeqId {
        self.add_flipflop(
            FlipFlop::Jk,
            clk,
            &[j, k],
            set,
            reset,
            q,
            qbar,
            delay,
            initial,
        )
    }

    /// Shared flip-flop wiring: create the `q`/`qbar` drivers, register the
    /// element in the fan-out of every net it reads (clock, data, async pins) in
    /// a fixed order, and store it.
    #[allow(clippy::too_many_arguments)]
    fn add_flipflop(
        &mut self,
        kind: FlipFlop,
        clk: Net,
        data: &[Net],
        set: Option<Net>,
        reset: Option<Net>,
        q: Net,
        qbar: Net,
        delay: Femtos,
        initial: Logic,
    ) -> SeqId {
        let q_driver = self.new_driver(q, initial);
        let qbar_driver = self.new_driver(qbar, initial.not());
        let sid = SeqId(self.seq.len());

        // Register in the fan-out of every input net so a change on any of them
        // re-evaluates this element. Order is fixed for determinism.
        self.nets[clk.0].fanout.push(Sink::Seq(sid));
        for &net in data {
            self.nets[net.0].fanout.push(Sink::Seq(sid));
        }
        for net in set.iter().chain(reset.iter()) {
            self.nets[net.0].fanout.push(Sink::Seq(sid));
        }

        self.seq.push(SeqElement::new(
            kind,
            clk,
            data.to_vec(),
            set,
            reset,
            q_driver,
            qbar_driver,
            delay,
            initial,
        ));
        sid
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
        // Seed each flip-flop's clock reference and apply any asserted async
        // pins, scheduling the output if its reset level needs correcting.
        for i in 0..self.seq.len() {
            self.reeval_seq(SeqId(i));
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

    /// Re-evaluate one flip-flop over its inputs' current resolved levels; if its
    /// `q` output moved, schedule the new `q`/`qbar` pair `delay` into the future.
    fn reeval_seq(&mut self, sid: SeqId) {
        let e = &self.seq[sid.0];
        let clk = self.nets[e.clk.0].value;
        let data: Vec<Logic> = e.data.iter().map(|&n| self.nets[n.0].value).collect();
        let set = e.set.map_or(Logic::Low, |n| self.nets[n.0].value);
        let reset = e.reset.map_or(Logic::Low, |n| self.nets[n.0].value);

        let e = &mut self.seq[sid.0];
        if let Some((q, qbar)) = e.update(clk, &data, set, reset) {
            let (q_driver, qbar_driver, delay) = (e.q, e.qbar, e.delay);
            let at = self.now + delay;
            self.queue.schedule(at, q_driver, q);
            self.queue.schedule(at, qbar_driver, qbar);
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
        for sink in fanout {
            match sink {
                Sink::Gate(gid) => self.reeval_gate(gid),
                Sink::Seq(sid) => self.reeval_seq(sid),
            }
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
    use crate::event::Femtos;
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

    /// Drive a two-input gate's inputs to `(a, b)` and read its settled output.
    fn two_input(kind: GateKind, a: Logic, b: Logic) -> Logic {
        let mut sim = Sim::new();
        let (na, nb, ny) = (sim.add_net(), sim.add_net(), sim.add_net());
        sim.add_input(na, a);
        sim.add_input(nb, b);
        sim.add_gate(kind, &[na, nb], ny, 5, Logic::X);
        sim.run_until(5);
        sim.value(ny)
    }

    #[test]
    fn nand_nor_xnor_truth_tables() {
        use Logic::{High as H, Low as L};
        // (a, b, NAND, NOR, XNOR)
        let cases = [
            (L, L, H, H, H),
            (L, H, H, L, L),
            (H, L, H, L, L),
            (H, H, L, L, H),
        ];
        for (a, b, nand, nor, xnor) in cases {
            assert_eq!(two_input(GateKind::Nand, a, b), nand, "NAND({a:?},{b:?})");
            assert_eq!(two_input(GateKind::Nor, a, b), nor, "NOR({a:?},{b:?})");
            assert_eq!(two_input(GateKind::Xnor, a, b), xnor, "XNOR({a:?},{b:?})");
        }
    }

    #[test]
    fn buffer_passes_its_input() {
        let mut sim = Sim::new();
        let (a, y) = (sim.add_net(), sim.add_net());
        let da = sim.add_input(a, Logic::Low);
        sim.add_gate(GateKind::Buf, &[a], y, 5, Logic::X);
        sim.run_until(5);
        assert_eq!(sim.value(y), Logic::Low);
        sim.set_input(da, Logic::High, 10);
        sim.run_until(15);
        assert_eq!(sim.value(y), Logic::High);
    }

    #[test]
    fn tristate_buffer_releases_the_net_when_disabled() {
        // A tri-state buffer shares a net with a plain input. When the buffer is
        // disabled it floats (Z) and the other driver wins; when enabled it
        // drives, conflicting with a disagreeing driver → X.
        let mut sim = Sim::new();
        let (data, en, bus) = (sim.add_net(), sim.add_net(), sim.add_net());
        sim.add_input(data, Logic::High);
        let den = sim.add_input(en, Logic::Low);
        let dbus = sim.add_input(bus, Logic::Low); // the other driver on the bus
        sim.add_gate(GateKind::TriState, &[data, en], bus, 5, Logic::Z);

        sim.run_until(5);
        assert_eq!(
            sim.value(bus),
            Logic::Low,
            "disabled buffer floats; input wins"
        );

        sim.set_input(den, Logic::High, 10); // enable: buffer drives High vs input Low
        sim.run_until(15);
        assert_eq!(
            sim.value(bus),
            Logic::X,
            "enabled buffer conflicts with the input"
        );

        sim.set_input(dbus, Logic::Z, 20); // other driver releases the bus
        sim.run_until(25);
        assert_eq!(
            sim.value(bus),
            Logic::High,
            "buffer alone now drives the bus"
        );
    }

    #[test]
    fn d_flipflop_latches_d_on_the_rising_edge() {
        const DLY: Femtos = 5;
        let mut sim = Sim::new();
        let (clk, d, q, qbar) = (sim.add_net(), sim.add_net(), sim.add_net(), sim.add_net());
        let dclk = sim.add_input(clk, Logic::Low);
        let dd = sim.add_input(d, Logic::Low);
        sim.add_d_flipflop(clk, d, q, qbar, DLY, Logic::Low);

        sim.run_until(0);
        assert_eq!(sim.value(q), Logic::Low);
        assert_eq!(sim.value(qbar), Logic::High);

        // Set D high well before the edge; the edge then latches a 1.
        sim.set_input(dd, Logic::High, 10);
        sim.set_input(dclk, Logic::High, 20);
        sim.run_until(20 + DLY - 1);
        assert_eq!(
            sim.value(q),
            Logic::Low,
            "output changes only after the delay"
        );
        sim.run_until(20 + DLY);
        assert_eq!(sim.value(q), Logic::High, "D=1 latched on the rising edge");
        assert_eq!(sim.value(qbar), Logic::Low);

        // Dropping D while the clock is high must not disturb Q.
        sim.set_input(dd, Logic::Low, 30);
        sim.run_until(40);
        assert_eq!(sim.value(q), Logic::High, "Q holds between edges");

        // Clock low (no effect), then a fresh rising edge latches the new D=0.
        sim.set_input(dclk, Logic::Low, 50);
        sim.set_input(dclk, Logic::High, 60);
        sim.run_until(60 + DLY);
        assert_eq!(sim.value(q), Logic::Low, "D=0 latched on the next edge");
        assert_eq!(sim.value(qbar), Logic::High);
    }

    #[test]
    fn d_flipflop_async_reset_overrides_the_clock() {
        const DLY: Femtos = 5;
        let mut sim = Sim::new();
        let (clk, d, rst, q, qbar) = (
            sim.add_net(),
            sim.add_net(),
            sim.add_net(),
            sim.add_net(),
            sim.add_net(),
        );
        let dclk = sim.add_input(clk, Logic::Low);
        sim.add_input(d, Logic::High); // D is held high throughout
        let drst = sim.add_input(rst, Logic::High); // start asserted
        sim.add_d_flipflop_sr(clk, d, None, Some(rst), q, qbar, DLY, Logic::Low);

        // Reset asserted pins Q low even across a rising edge.
        sim.set_input(dclk, Logic::High, 10);
        sim.run_until(10 + DLY);
        assert_eq!(sim.value(q), Logic::Low, "async reset dominates the clock");

        // Release reset, drop the clock, then clock again: now D=1 latches.
        sim.set_input(drst, Logic::Low, 20);
        sim.set_input(dclk, Logic::Low, 30);
        sim.set_input(dclk, Logic::High, 40);
        sim.run_until(40 + DLY);
        assert_eq!(sim.value(q), Logic::High, "latches once reset is released");
    }

    #[test]
    fn jk_flipflop_set_clear_hold_and_toggle() {
        const DLY: Femtos = 5;
        let mut sim = Sim::new();
        let (clk, j, k, q, qbar) = (
            sim.add_net(),
            sim.add_net(),
            sim.add_net(),
            sim.add_net(),
            sim.add_net(),
        );
        let dclk = sim.add_input(clk, Logic::Low);
        let dj = sim.add_input(j, Logic::Low);
        let dk = sim.add_input(k, Logic::Low);
        sim.add_jk_flipflop(clk, j, k, q, qbar, DLY, Logic::Low);

        // Pulse the clock 0→1→0 at `base`, returning the time just after the edge
        // has propagated. Inputs must be set before `base`.
        let mut base = 10u64;
        let pulse = |sim: &mut Sim, base: u64| {
            sim.set_input(dclk, Logic::High, base);
            sim.set_input(dclk, Logic::Low, base + 20);
            sim.run_until(base + 20 + DLY);
        };

        // J=1,K=0 → set.
        sim.set_input(dj, Logic::High, base - 2);
        pulse(&mut sim, base);
        assert_eq!(sim.value(q), Logic::High, "J=1,K=0 sets Q");
        base += 40;

        // J=0,K=1 → clear.
        sim.set_input(dj, Logic::Low, base - 2);
        sim.set_input(dk, Logic::High, base - 2);
        pulse(&mut sim, base);
        assert_eq!(sim.value(q), Logic::Low, "J=0,K=1 clears Q");
        base += 40;

        // J=0,K=0 → hold (Q stays low).
        sim.set_input(dk, Logic::Low, base - 2);
        pulse(&mut sim, base);
        assert_eq!(sim.value(q), Logic::Low, "J=0,K=0 holds Q");
        base += 40;

        // J=1,K=1 → toggle on each edge: Low→High→Low.
        sim.set_input(dj, Logic::High, base - 2);
        sim.set_input(dk, Logic::High, base - 2);
        pulse(&mut sim, base);
        assert_eq!(sim.value(q), Logic::High, "toggle 0→1");
        assert_eq!(sim.value(qbar), Logic::Low);
        base += 40;
        pulse(&mut sim, base);
        assert_eq!(sim.value(q), Logic::Low, "toggle 1→0");
        assert_eq!(sim.value(qbar), Logic::High);
    }
}
