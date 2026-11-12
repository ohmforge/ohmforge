//! Reusable logic building blocks assembled from the kernel's primitives.
//!
//! Where [`GateKind`] and the flip-flop constructors on [`Sim`]
//! are the primitive cells, this module composes them into the small structural
//! blocks a board actually uses: multiplexers (built from gates) and a ripple
//! counter (built from toggle flip-flops). Each builder wires nets and cells into
//! an existing [`Sim`] and hands back the handles you read or drive.

use crate::event::Femtos;
use crate::sim::{DriverId, GateKind, Net, Sim};
use crate::Logic;

/// Add a **2:1 multiplexer**: `y = a` when `sel` is low, `y = b` when `sel` is
/// high. Built as `y = (a AND ¬sel) OR (b AND sel)`, so it costs an inverter and
/// two ANDs feeding an OR, each with propagation `delay`. Returns the output net.
pub fn add_mux2(sim: &mut Sim, sel: Net, a: Net, b: Net, delay: Femtos) -> Net {
    let nsel = sim.add_net();
    let a_sel = sim.add_net();
    let b_sel = sim.add_net();
    let y = sim.add_net();

    sim.add_gate(GateKind::Not, &[sel], nsel, delay, Logic::X);
    sim.add_gate(GateKind::And, &[a, nsel], a_sel, delay, Logic::X);
    sim.add_gate(GateKind::And, &[b, sel], b_sel, delay, Logic::X);
    sim.add_gate(GateKind::Or, &[a_sel, b_sel], y, delay, Logic::X);
    y
}

/// Add a **4:1 multiplexer** selecting `d[0..4]` by the two select lines, with
/// `sel0` the low-order bit: the output is `d[sel1 * 2 + sel0]`. Composed from
/// three [`add_mux2`] cells. Returns the output net.
pub fn add_mux4(sim: &mut Sim, sel0: Net, sel1: Net, d: [Net; 4], delay: Femtos) -> Net {
    let lo = add_mux2(sim, sel0, d[0], d[1], delay); // sel0 picks d0/d1
    let hi = add_mux2(sim, sel0, d[2], d[3], delay); // sel0 picks d2/d3
    add_mux2(sim, sel1, lo, hi, delay) // sel1 picks the pair
}

/// An asynchronous (ripple) binary up-counter built from toggle flip-flops.
///
/// Drive [`clk`](Self::clk) with a `0 → 1 → 0` pulse per count; each rising edge
/// increments the value by one, wrapping at `2^width`. Assert
/// [`reset`](Self::reset) (hold it longer than the counter's ripple delay, i.e.
/// `width * delay`) to clear every stage to zero.
pub struct RippleCounter {
    /// External clock driver — pulse it `0 → 1 → 0` to advance the count.
    pub clk: DriverId,
    /// Asynchronous active-high clear driver.
    pub reset: DriverId,
    /// Stage outputs, least-significant bit first (`q[0]` = bit 0).
    pub q: Vec<Net>,
}

/// One driver on a tri-state bus: a `data` net gated by an active-high `enable`.
///
/// When `enable` is high the driver contributes `data` to the bus; when low it
/// releases the bus (high-impedance `Z`); an unknown `enable` contributes `X`.
#[derive(Debug, Clone, Copy)]
pub struct TriStateDriver {
    /// The level this driver would put on the bus when enabled.
    pub data: Net,
    /// Active-high output-enable.
    pub enable: Net,
}

/// Add a **tri-state bus**: wire one [`TriState`](GateKind::TriState) buffer per
/// `drivers` entry onto a single shared net, each with propagation `delay`, and
/// return that bus net.
///
/// The bus level is the [resolution](Logic::resolve) of every enabled driver, so
/// it behaves exactly as hardware wired-together tri-state outputs do: with one
/// driver enabled the bus takes its value; with several enabled it takes their
/// agreed value, or `X` where they conflict; with none enabled the bus floats
/// (`Z`). This is the `CP-BRD-M3.1` bus acceptance gate.
pub fn add_tristate_bus(sim: &mut Sim, drivers: &[TriStateDriver], delay: Femtos) -> Net {
    let bus = sim.add_net();
    for d in drivers {
        sim.add_gate(
            GateKind::TriState,
            &[d.data, d.enable],
            bus,
            delay,
            Logic::Z,
        );
    }
    bus
}

/// Add a `width`-bit ripple counter with per-flip-flop propagation `delay`.
///
/// Each stage is a JK flip-flop with `J = K = 1` (a toggle flip-flop). Stage 0 is
/// clocked by the external clock; every later stage is clocked by the previous
/// stage's `Q̄`, so a stage toggles when the one below it rolls `1 → 0` — ordinary
/// binary ripple counting. All stages share one asynchronous clear.
pub fn add_ripple_counter(sim: &mut Sim, width: usize, delay: Femtos) -> RippleCounter {
    assert!(width > 0, "a counter needs at least one bit");

    let clk_net = sim.add_net();
    let clk = sim.add_input(clk_net, Logic::Low);
    let reset_net = sim.add_net();
    let reset = sim.add_input(reset_net, Logic::Low);

    // A constant-high net tied to every stage's J and K inputs (toggle mode).
    let high = sim.add_net();
    sim.add_input(high, Logic::High);

    let mut q = Vec::with_capacity(width);
    let mut prev_qbar = clk_net; // stage 0 is clocked by the external clock
    for _ in 0..width {
        let q_net = sim.add_net();
        let qbar_net = sim.add_net();
        sim.add_jk_flipflop_sr(
            prev_qbar,
            high,
            high,
            None,
            Some(reset_net),
            q_net,
            qbar_net,
            delay,
            Logic::Low,
        );
        q.push(q_net);
        prev_qbar = qbar_net;
    }

    RippleCounter { clk, reset, q }
}

#[cfg(test)]
mod tests {
    use super::{add_mux2, add_mux4, add_ripple_counter, add_tristate_bus, TriStateDriver};
    use crate::{Logic, Net, Sim};

    const D: u64 = 10; // per-cell propagation delay, in fs

    /// Read a counter's stage nets as an integer, LSB first.
    fn count(sim: &Sim, q: &[Net]) -> u64 {
        let mut v = 0u64;
        for (bit, &net) in q.iter().enumerate() {
            match sim.value(net) {
                Logic::High => v |= 1 << bit,
                Logic::Low => {}
                other => panic!("counter bit {bit} is not a definite level: {other:?}"),
            }
        }
        v
    }

    #[test]
    fn mux2_selects_between_its_inputs() {
        let mut sim = Sim::new();
        let (sel, a, b) = (sim.add_net(), sim.add_net(), sim.add_net());
        let dsel = sim.add_input(sel, Logic::Low);
        sim.add_input(a, Logic::Low);
        sim.add_input(b, Logic::High);
        let y = add_mux2(&mut sim, sel, a, b, D);

        sim.run_until(3 * D); // sel = 0 → y = a = 0
        assert_eq!(sim.value(y), Logic::Low);

        sim.set_input(dsel, Logic::High, 10 * D); // sel = 1 → y = b = 1
        sim.run_until(20 * D);
        assert_eq!(sim.value(y), Logic::High);
    }

    #[test]
    fn mux4_selects_the_addressed_input() {
        let mut sim = Sim::new();
        let s0 = sim.add_net();
        let s1 = sim.add_net();
        let d = [sim.add_net(), sim.add_net(), sim.add_net(), sim.add_net()];
        let ds0 = sim.add_input(s0, Logic::Low);
        let ds1 = sim.add_input(s1, Logic::Low);
        // Distinct levels so each selection is unambiguous: d0=1,d1=0,d2=0,d3=1.
        sim.add_input(d[0], Logic::High);
        sim.add_input(d[1], Logic::Low);
        sim.add_input(d[2], Logic::Low);
        sim.add_input(d[3], Logic::High);
        let y = add_mux4(&mut sim, s0, s1, d, D);

        sim.run_until(10 * D); // (s1,s0)=00 → d0 = 1
        assert_eq!(sim.value(y), Logic::High);

        sim.set_input(ds0, Logic::High, 20 * D); // 01 → d1 = 0
        sim.run_until(40 * D);
        assert_eq!(sim.value(y), Logic::Low);

        sim.set_input(ds0, Logic::Low, 50 * D); // 10 → d2 = 0
        sim.set_input(ds1, Logic::High, 50 * D);
        sim.run_until(70 * D);
        assert_eq!(sim.value(y), Logic::Low);

        sim.set_input(ds0, Logic::High, 80 * D); // 11 → d3 = 1
        sim.run_until(100 * D);
        assert_eq!(sim.value(y), Logic::High);
    }

    #[test]
    fn ripple_counter_counts_0_to_15_and_wraps() {
        const WIDTH: usize = 4;
        let mut sim = Sim::new();
        let c = add_ripple_counter(&mut sim, WIDTH, D);

        // Settle time per clock must exceed the worst-case ripple through all
        // stages. One clock "tick" is a full low→high→low pulse.
        let tick = 100 * D;
        let mut t = 0u64;

        // Clear to a known zero, holding reset well past the ripple delay.
        sim.set_input(c.reset, Logic::High, t);
        sim.set_input(c.reset, Logic::Low, t + tick);
        t += 2 * tick;
        sim.run_until(t);
        assert_eq!(count(&sim, &c.q), 0, "reset did not clear the counter");

        // Two full laps (0..16 twice) exercises every carry and the wrap.
        for lap in 0..2 {
            for expected in 0..(1u64 << WIDTH) {
                assert_eq!(
                    count(&sim, &c.q),
                    expected,
                    "before tick (lap {lap}, expected {expected})"
                );
                // Rising edge advances the count; falling edge does nothing.
                sim.set_input(c.clk, Logic::High, t);
                sim.set_input(c.clk, Logic::Low, t + tick / 2);
                t += tick;
                sim.run_until(t);
            }
            // After 2^WIDTH ticks the counter has wrapped back to zero.
            assert_eq!(count(&sim, &c.q), 0, "counter did not wrap (lap {lap})");
        }
    }

    #[test]
    fn tristate_bus_resolves_two_drivers() {
        // Two drivers with opposing data values share a bus via tri-state
        // buffers. The acceptance gate: one enabled → its value; both enabled →
        // X (conflict); neither → Z (floating).
        let mut sim = Sim::new();
        let (d0, e0) = (sim.add_net(), sim.add_net());
        let (d1, e1) = (sim.add_net(), sim.add_net());
        sim.add_input(d0, Logic::High); // driver 0 would drive High
        sim.add_input(d1, Logic::Low); // driver 1 would drive Low
        let de0 = sim.add_input(e0, Logic::Low);
        let de1 = sim.add_input(e1, Logic::Low);
        let bus = add_tristate_bus(
            &mut sim,
            &[
                TriStateDriver {
                    data: d0,
                    enable: e0,
                },
                TriStateDriver {
                    data: d1,
                    enable: e1,
                },
            ],
            D,
        );

        // Neither enabled → the bus floats.
        sim.run_until(2 * D);
        assert_eq!(sim.value(bus), Logic::Z, "no driver enabled → Z");

        // Enable driver 0 only → bus takes its High.
        sim.set_input(de0, Logic::High, 10 * D);
        sim.run_until(12 * D);
        assert_eq!(sim.value(bus), Logic::High, "driver 0 alone drives High");

        // Enable driver 1 too → High vs Low conflict → X.
        sim.set_input(de1, Logic::High, 20 * D);
        sim.run_until(22 * D);
        assert_eq!(sim.value(bus), Logic::X, "both drivers conflict → X");

        // Disable driver 0, leaving driver 1 → bus takes its Low.
        sim.set_input(de0, Logic::Low, 30 * D);
        sim.run_until(32 * D);
        assert_eq!(sim.value(bus), Logic::Low, "driver 1 alone drives Low");

        // Disable both again → back to floating.
        sim.set_input(de1, Logic::Low, 40 * D);
        sim.run_until(42 * D);
        assert_eq!(sim.value(bus), Logic::Z, "both released → Z");
    }

    #[test]
    fn ripple_counter_async_reset_clears_mid_count() {
        const WIDTH: usize = 4;
        let mut sim = Sim::new();
        let c = add_ripple_counter(&mut sim, WIDTH, D);
        let tick = 100 * D;
        let mut t = 0u64;

        // Count up to 5.
        for _ in 0..5 {
            sim.set_input(c.clk, Logic::High, t);
            sim.set_input(c.clk, Logic::Low, t + tick / 2);
            t += tick;
            sim.run_until(t);
        }
        assert_eq!(count(&sim, &c.q), 5);

        // Async clear mid-count returns to zero.
        sim.set_input(c.reset, Logic::High, t);
        sim.set_input(c.reset, Logic::Low, t + tick);
        t += 2 * tick;
        sim.run_until(t);
        assert_eq!(count(&sim, &c.q), 0);
    }
}
