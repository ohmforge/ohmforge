//! # ee-sim
//!
//! The analog / mixed-signal simulation engine — the heart of the "emulator."
//!
//! Builds a Modified Nodal Analysis system from an [`ee_core::Circuit`], solves
//! it, and returns typed [`Signal`]s. See `docs/ARCHITECTURE.md` §
//! "The simulation core."
//!
//! Roadmap: **M0** (here) = linear MNA + DC operating point + a Backward-Euler
//! transient; M1 = nonlinear devices (Newton–Raphson) + AC/DC-sweep;
//! M2 = noise / Monte-Carlo. (docs/ROADMAP.md)
//!
//! This crate takes no clock, no filesystem, and no unseeded randomness, and
//! stays `wasm32`-clean — the same rules as `ee-core`.

mod mna;

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use ee_core::graph::{Circuit, NetId, Part, PartKind};
use ee_core::{Signal, Unit};
use mna::System;

/// The analyses the engine can run.
#[derive(Debug, Clone)]
pub enum Analysis {
    /// DC operating point (capacitors open, inductors shorted).
    OperatingPoint,
    /// Time-domain transient over `duration` seconds. `step` is the timestep;
    /// `None` picks `duration / 1000`.
    Transient { duration: f64, step: Option<f64> },
    /// Small-signal AC sweep (Bode). Not yet implemented (arrives at M1.4).
    Ac {
        f_start: f64,
        f_stop: f64,
        points_per_decade: u32,
    },
    /// Sweep an independent source's value across `[start, stop]` by `step`,
    /// recording the operating point at each point. (CP-SIM-M1.5.)
    DcSweep {
        source: String,
        start: f64,
        stop: f64,
        step: f64,
    },
    /// Repeat an inner analysis once per value of a source/parameter. (CP-SIM-M1.5.)
    Parametric {
        source: String,
        values: Vec<f64>,
        inner: Box<Analysis>,
    },
}

/// What can go wrong assembling or solving a circuit.
#[derive(Debug, Clone, PartialEq)]
pub enum SimError {
    /// A part that needs a value (R/C/V) has none.
    MissingValue(String),
    /// A part's value is out of range (e.g. a zero-ohm resistor).
    BadValue(String),
    /// A part does not have exactly two terminals.
    WrongTerminals(String),
    /// A device or analysis not modeled at this milestone.
    Unsupported(&'static str),
    /// The MNA matrix is singular — usually a floating node or missing ground.
    Singular,
}

impl fmt::Display for SimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SimError::MissingValue(r) => write!(f, "part {r} has no value"),
            SimError::BadValue(r) => write!(f, "part {r} has an invalid value"),
            SimError::WrongTerminals(r) => write!(f, "part {r} must have exactly two terminals"),
            SimError::Unsupported(what) => write!(f, "unsupported at this milestone: {what}"),
            SimError::Singular => {
                write!(
                    f,
                    "circuit is singular — a floating node or no path to ground?"
                )
            }
        }
    }
}

impl Error for SimError {}

/// The simulation engine. Stateless; hold one and call [`Engine::run`].
#[derive(Debug, Default, Clone, Copy)]
pub struct Engine;

impl Engine {
    pub fn new() -> Self {
        Engine
    }

    /// Run an [`Analysis`] against a circuit, returning one [`Signal`] per node.
    pub fn run(&self, circuit: &Circuit, analysis: &Analysis) -> Result<Vec<Signal>, SimError> {
        validate(circuit)?;
        let layout = Layout::build(circuit);
        match analysis {
            Analysis::OperatingPoint => {
                let x = solve_dc(circuit, &layout)?;
                Ok(op_signals(&layout, &x))
            }
            Analysis::Transient { duration, step } => transient(circuit, &layout, *duration, *step),
            Analysis::Ac { .. } => Err(SimError::Unsupported("AC analysis (CP-SIM-M1.4)")),
            Analysis::DcSweep { .. } => Err(SimError::Unsupported("DC sweep (CP-SIM-M1.5)")),
            Analysis::Parametric { .. } => {
                Err(SimError::Unsupported("parametric sweep (CP-SIM-M1.5)"))
            }
        }
    }
}

/// Reject devices not modeled at M0 so a part never gets silently dropped.
fn validate(circuit: &Circuit) -> Result<(), SimError> {
    for part in circuit.parts() {
        match &part.kind {
            PartKind::Resistor
            | PartKind::Capacitor
            | PartKind::Inductor
            | PartKind::VoltageSource
            | PartKind::CurrentSource => {}
            PartKind::Other(_) => {
                return Err(SimError::Unsupported("nonlinear/unknown device (M1)"))
            }
        }
    }
    Ok(())
}

/// Dense-index mapping from resolved nets to MNA rows.
struct Layout {
    /// Resolved raw net id → dense node index (ground excluded).
    index: BTreeMap<u32, usize>,
    /// Dense node index → resolved raw net id (for naming signals).
    inv: Vec<u32>,
    /// Part indices of the voltage sources (each adds a branch-current row).
    vsources: Vec<usize>,
    /// Part indices of the inductors. At the DC operating point each is a short
    /// (a 0 V source) and so adds its own branch-current row, after the vsources.
    inductors: Vec<usize>,
}

impl Layout {
    fn build(circuit: &Circuit) -> Self {
        let mut index = BTreeMap::new();
        let mut inv = Vec::new();
        for part in circuit.parts() {
            for &t in &part.terminals {
                let r = circuit.resolve(t).0;
                if r != NetId::GROUND.0 && !index.contains_key(&r) {
                    index.insert(r, inv.len());
                    inv.push(r);
                }
            }
        }
        let kind_indices = |want: fn(&PartKind) -> bool| {
            circuit
                .parts()
                .iter()
                .enumerate()
                .filter(|(_, p)| want(&p.kind))
                .map(|(i, _)| i)
                .collect::<Vec<_>>()
        };
        let vsources = kind_indices(|k| matches!(k, PartKind::VoltageSource));
        let inductors = kind_indices(|k| matches!(k, PartKind::Inductor));
        Self {
            index,
            inv,
            vsources,
            inductors,
        }
    }

    fn n_nodes(&self) -> usize {
        self.inv.len()
    }

    /// Dense index of a net, or `None` for ground (the reference).
    fn node_of(&self, circuit: &Circuit, net: NetId) -> Option<usize> {
        let r = circuit.resolve(net).0;
        if r == NetId::GROUND.0 {
            None
        } else {
            self.index.get(&r).copied()
        }
    }
}

/// The stored magnitude of a part's value (base SI units), or an error.
fn magnitude(part: &Part) -> Result<f64, SimError> {
    part.value
        .as_ref()
        .map(|v| v.magnitude)
        .ok_or_else(|| SimError::MissingValue(part.reference.clone()))
}

/// The two nets a two-terminal part connects, or an error.
fn terminals2(part: &Part) -> Result<(NetId, NetId), SimError> {
    match part.terminals.as_slice() {
        [a, b] => Ok((*a, *b)),
        _ => Err(SimError::WrongTerminals(part.reference.clone())),
    }
}

/// Stamp every resistor as a conductance between its nodes.
fn stamp_resistors(sys: &mut System, circuit: &Circuit, layout: &Layout) -> Result<(), SimError> {
    for part in circuit.parts() {
        if matches!(part.kind, PartKind::Resistor) {
            let r = magnitude(part)?;
            if r <= 0.0 {
                return Err(SimError::BadValue(part.reference.clone()));
            }
            let (a, b) = terminals2(part)?;
            sys.stamp_conductance(
                layout.node_of(circuit, a),
                layout.node_of(circuit, b),
                1.0 / r,
            );
        }
    }
    Ok(())
}

/// Stamp every voltage source, each using its own branch-current row.
fn stamp_vsources(sys: &mut System, circuit: &Circuit, layout: &Layout) -> Result<(), SimError> {
    for (branch, &pi) in layout.vsources.iter().enumerate() {
        let part = &circuit.parts()[pi];
        let volts = magnitude(part)?;
        let (plus, minus) = terminals2(part)?;
        let k = layout.n_nodes() + branch;
        sys.stamp_vsource(
            layout.node_of(circuit, plus),
            layout.node_of(circuit, minus),
            k,
            volts,
        );
    }
    Ok(())
}

/// Stamp every independent current source. `I` from terminal `a` to terminal `b`
/// drives that current from `a` into `b` (no new unknown — a pure RHS term), so it
/// is identical in the operating point and in every transient step.
fn stamp_current_sources(
    sys: &mut System,
    circuit: &Circuit,
    layout: &Layout,
) -> Result<(), SimError> {
    for part in circuit.parts() {
        if matches!(part.kind, PartKind::CurrentSource) {
            let amps = magnitude(part)?;
            let (a, b) = terminals2(part)?;
            // `stamp_current(i, j, amps)` injects `amps` flowing from `j` into `i`,
            // so `(b, a)` reads as "amps from a into b" — the source's own direction.
            sys.stamp_current(layout.node_of(circuit, b), layout.node_of(circuit, a), amps);
        }
    }
    Ok(())
}

/// Stamp every inductor as a short — a 0 V voltage source on its own branch row,
/// one per entry in `layout.inductors` (placed after the voltage-source branches).
/// This is the DC operating point: a de-energized inductor is a wire.
fn stamp_inductor_shorts(
    sys: &mut System,
    circuit: &Circuit,
    layout: &Layout,
) -> Result<(), SimError> {
    let base = layout.n_nodes() + layout.vsources.len();
    for (branch, &pi) in layout.inductors.iter().enumerate() {
        let part = &circuit.parts()[pi];
        let (a, b) = terminals2(part)?;
        sys.stamp_vsource(
            layout.node_of(circuit, a),
            layout.node_of(circuit, b),
            base + branch,
            0.0,
        );
    }
    Ok(())
}

/// Solve the DC operating point: resistors + sources, capacitors open, inductors
/// shorted.
fn solve_dc(circuit: &Circuit, layout: &Layout) -> Result<Vec<f64>, SimError> {
    let size = layout.n_nodes() + layout.vsources.len() + layout.inductors.len();
    let mut sys = System::new(size);
    stamp_resistors(&mut sys, circuit, layout)?;
    stamp_vsources(&mut sys, circuit, layout)?;
    stamp_current_sources(&mut sys, circuit, layout)?;
    stamp_inductor_shorts(&mut sys, circuit, layout)?;
    sys.solve().ok_or(SimError::Singular)
}

/// Voltage across a two-terminal device (`v(plus) - v(minus)`), ground = 0 V.
fn across(x: &[f64], plus: Option<usize>, minus: Option<usize>) -> f64 {
    plus.map_or(0.0, |i| x[i]) - minus.map_or(0.0, |i| x[i])
}

/// Backward-Euler transient.
///
/// Capacitors start uncharged (0 V initial condition) and discretize into a
/// companion model — a conductance `C/h` in parallel with a current source
/// `(C/h)·v_prev`, refreshed each timestep.
///
/// Inductors start de-energized (`i(0) = 0`). Backward Euler gives
/// `i_n = i_prev + (h/L)·(v_a − v_b)`, whose Norton companion is a conductance
/// `g_eq = h/L` between the terminals in parallel with a history current source
/// of value `i_prev` oriented `a → b`. After each solved step `i_prev` is refreshed
/// to `i_prev + g_eq·(v_a − v_b)` from the new node voltages. At `t = 0` the
/// inductor is left open (not stamped) so the recorded start matches `i(0) = 0`.
fn transient(
    circuit: &Circuit,
    layout: &Layout,
    duration: f64,
    step: Option<f64>,
) -> Result<Vec<Signal>, SimError> {
    let h = step.unwrap_or(duration / 1000.0);
    if h <= 0.0 || duration <= 0.0 {
        return Err(SimError::BadValue("transient duration/step".into()));
    }
    let steps = (duration / h).round().max(1.0) as usize;
    let n = layout.n_nodes();
    let m = layout.vsources.len();

    // Capacitors: (plus, minus, farads).
    let caps: Vec<(NetId, NetId, f64)> = circuit
        .parts()
        .iter()
        .filter(|p| matches!(p.kind, PartKind::Capacitor))
        .map(|p| {
            let (a, b) = terminals2(p)?;
            Ok::<_, SimError>((a, b, magnitude(p)?))
        })
        .collect::<Result<_, _>>()?;

    // Inductors: (a, b, henries). A non-positive inductance would blow up `h/L`.
    let inds: Vec<(NetId, NetId, f64)> = circuit
        .parts()
        .iter()
        .filter(|p| matches!(p.kind, PartKind::Inductor))
        .map(|p| {
            let (a, b) = terminals2(p)?;
            let l = magnitude(p)?;
            if l <= 0.0 {
                return Err(SimError::BadValue(p.reference.clone()));
            }
            Ok::<_, SimError>((a, b, l))
        })
        .collect::<Result<_, _>>()?;

    let mut times: Vec<f64> = Vec::with_capacity(steps + 1);
    let mut traces: Vec<Vec<f64>> = vec![Vec::with_capacity(steps + 1); n];
    let mut cap_prev = vec![0.0; caps.len()]; // 0 V initial condition (UIC)
    let mut ind_prev = vec![0.0; inds.len()]; // 0 A initial condition (de-energized)

    // t = 0: hold each capacitor at its initial voltage (a 0 V source), solve the
    // rest, and record. This is the true starting point of every curve.
    {
        let mut sys = System::new(n + m + caps.len());
        stamp_resistors(&mut sys, circuit, layout)?;
        stamp_vsources(&mut sys, circuit, layout)?;
        stamp_current_sources(&mut sys, circuit, layout)?;
        // Inductors are open at t = 0 (i(0) = 0), so they are not stamped at all.
        for (ci, (a, b, _)) in caps.iter().enumerate() {
            let k = n + m + ci;
            sys.stamp_vsource(
                layout.node_of(circuit, *a),
                layout.node_of(circuit, *b),
                k,
                0.0,
            );
        }
        let x0 = sys.solve().ok_or(SimError::Singular)?;
        times.push(0.0);
        for (d, trace) in traces.iter_mut().enumerate() {
            trace.push(x0[d]);
        }
        for (ci, (a, b, _)) in caps.iter().enumerate() {
            cap_prev[ci] = across(
                &x0,
                layout.node_of(circuit, *a),
                layout.node_of(circuit, *b),
            );
        }
    }

    // t = h, 2h, … : companion-stamp the capacitors and advance.
    for k in 1..=steps {
        let mut sys = System::new(n + m);
        stamp_resistors(&mut sys, circuit, layout)?;
        stamp_vsources(&mut sys, circuit, layout)?;
        stamp_current_sources(&mut sys, circuit, layout)?;
        for (ci, (a, b, c)) in caps.iter().enumerate() {
            let g_eq = c / h;
            let (ia, ib) = (layout.node_of(circuit, *a), layout.node_of(circuit, *b));
            sys.stamp_conductance(ia, ib, g_eq);
            sys.stamp_current(ia, ib, g_eq * cap_prev[ci]);
        }
        for (li, (a, b, l)) in inds.iter().enumerate() {
            let g_eq = h / l;
            let (ia, ib) = (layout.node_of(circuit, *a), layout.node_of(circuit, *b));
            sys.stamp_conductance(ia, ib, g_eq);
            // History source `i_prev` oriented a → b: current flows from a into b.
            sys.stamp_current(ib, ia, ind_prev[li]);
        }
        let x = sys.solve().ok_or(SimError::Singular)?;
        times.push(k as f64 * h);
        for (d, trace) in traces.iter_mut().enumerate() {
            trace.push(x[d]);
        }
        for (ci, (a, b, _)) in caps.iter().enumerate() {
            cap_prev[ci] = across(&x, layout.node_of(circuit, *a), layout.node_of(circuit, *b));
        }
        for (li, (a, b, l)) in inds.iter().enumerate() {
            let g_eq = h / l;
            ind_prev[li] +=
                g_eq * across(&x, layout.node_of(circuit, *a), layout.node_of(circuit, *b));
        }
    }

    Ok(traces
        .into_iter()
        .enumerate()
        .map(|(d, y)| {
            Signal::time(
                format!("v(n{})", layout.inv[d]),
                Unit::Volt,
                times.clone(),
                y,
            )
        })
        .collect())
}

/// Operating-point node voltages as single-sample signals.
fn op_signals(layout: &Layout, x: &[f64]) -> Vec<Signal> {
    (0..layout.n_nodes())
        .map(|d| {
            Signal::time(
                format!("v(n{})", layout.inv[d]),
                Unit::Volt,
                vec![0.0],
                vec![x[d]],
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ee_core::value::Value;

    fn v(s: &str, u: Unit) -> Option<Value> {
        Some(Value::parse(s, u).unwrap())
    }

    #[test]
    fn resistive_divider_operating_point() {
        // 10 V across two equal 1k resistors → 5 V at the midpoint.
        let mut c = Circuit::new("divider");
        let vin = c.new_net();
        let mid = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            v("10", Unit::Volt),
            vec![vin, NetId::GROUND],
        );
        c.add_part("R1", PartKind::Resistor, v("1k", Unit::Ohm), vec![vin, mid]);
        c.add_part(
            "R2",
            PartKind::Resistor,
            v("1k", Unit::Ohm),
            vec![mid, NetId::GROUND],
        );

        let layout = Layout::build(&c);
        let x = solve_dc(&c, &layout).unwrap();
        let mid_idx = layout.node_of(&c, mid).unwrap();
        assert!((x[mid_idx] - 5.0).abs() < 1e-9, "midpoint = {}", x[mid_idx]);
    }

    #[test]
    fn rc_step_transient_matches_analytic() {
        // 5 V step into a 1k / 1µF low-pass → v(out) = 5(1 - e^(-t/RC)), RC = 1 ms.
        let mut c = Circuit::new("rc");
        let vin = c.new_net();
        let out = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            v("5", Unit::Volt),
            vec![vin, NetId::GROUND],
        );
        c.add_part("R1", PartKind::Resistor, v("1k", Unit::Ohm), vec![vin, out]);
        c.add_part(
            "C1",
            PartKind::Capacitor,
            v("1u", Unit::Farad),
            vec![out, NetId::GROUND],
        );

        let sigs = Engine::new()
            .run(
                &c,
                &Analysis::Transient {
                    duration: 5e-3,
                    step: Some(5e-6),
                },
            )
            .unwrap();

        let name = format!("v(n{})", c.resolve(out).0);
        let sig = sigs
            .iter()
            .find(|s| s.name == name)
            .expect("v(out) present");
        let rc = 1e-3;
        for (&t, &val) in sig.x.iter().zip(sig.real().unwrap()) {
            let expected = 5.0 * (1.0 - (-t / rc).exp());
            // Within 1% of full scale (5 V) across the whole curve.
            assert!(
                (val - expected).abs() <= 0.05,
                "t={t}: got {val}, expected {expected}"
            );
        }
        // Sanity: starts uncharged, ends near the rail.
        let y = sig.real().unwrap();
        assert!(y[0].abs() < 1e-9);
        assert!(*y.last().unwrap() > 4.9);
    }

    #[test]
    fn current_source_into_resistor_operating_point() {
        // 1 mA pushed from ground up into node `n`, 1 kΩ from `n` to ground.
        // KCL at `n`: the injected current leaves through R, so v = I·R = 1 V exactly.
        let mut c = Circuit::new("isrc");
        let n = c.new_net();
        c.add_part(
            "I1",
            PartKind::CurrentSource,
            v("1m", Unit::Ampere),
            vec![NetId::GROUND, n],
        );
        c.add_part(
            "R1",
            PartKind::Resistor,
            v("1k", Unit::Ohm),
            vec![n, NetId::GROUND],
        );

        let layout = Layout::build(&c);
        let x = solve_dc(&c, &layout).unwrap();
        let n_idx = layout.node_of(&c, n).unwrap();
        assert!((x[n_idx] - 1.0).abs() < 1e-9, "v(n) = {}", x[n_idx]);
    }

    #[test]
    fn rl_step_transient_matches_analytic() {
        // 5 V step into R = 100 Ω, L = 0.1 H in series to ground → τ = L/R = 1 ms.
        // The series current (= resistor current) follows i(t) = (V/R)(1 − e^(−t/τ)).
        let mut c = Circuit::new("rl");
        let vin = c.new_net();
        let mid = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            v("5", Unit::Volt),
            vec![vin, NetId::GROUND],
        );
        c.add_part(
            "R1",
            PartKind::Resistor,
            v("100", Unit::Ohm),
            vec![vin, mid],
        );
        c.add_part(
            "L1",
            PartKind::Inductor,
            v("0.1", Unit::Henry),
            vec![mid, NetId::GROUND],
        );

        let sigs = Engine::new()
            .run(
                &c,
                &Analysis::Transient {
                    duration: 5e-3,
                    step: Some(1e-5), // h/τ = 0.01
                },
            )
            .unwrap();

        let name = format!("v(n{})", c.resolve(mid).0);
        let sig = sigs
            .iter()
            .find(|s| s.name == name)
            .expect("v(mid) present");
        let (vsrc, r, tau) = (5.0, 100.0, 1e-3);
        let i_final = vsrc / r; // 50 mA
        for (&t, &vmid) in sig.x.iter().zip(sig.real().unwrap()) {
            let i = (vsrc - vmid) / r; // series: resistor current == inductor current
            let expected = i_final * (1.0 - (-t / tau).exp());
            // Within 1% of full scale (50 mA).
            assert!(
                (i - expected).abs() <= 0.01 * i_final,
                "t={t}: i={i}, expected={expected}"
            );
        }
        // Sanity: starts de-energized, ends at the resistive limit.
        let vmid = sig.real().unwrap();
        let i0 = (vsrc - vmid[0]) / r;
        let i_last = (vsrc - *vmid.last().unwrap()) / r;
        assert!(i0.abs() < 1e-9, "i(0) = {i0}");
        assert!(i_last > 0.99 * i_final, "i(end) = {i_last}");
    }

    #[test]
    fn series_rlc_rings_at_the_damped_frequency() {
        // Underdamped series RLC step: V1 — R — L — C — gnd, output v(C).
        // R = 10 Ω, L = 1 mH, C = 1 µF ⇒ ω0 = 1/√(LC), α = R/(2L),
        // ω_d = √(ω0² − α²). The step response overshoots and its first peak
        // sits at t_p = π/ω_d — a frequency we can read off the trace.
        let mut c = Circuit::new("rlc");
        let vin = c.new_net();
        let a = c.new_net();
        let out = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            v("5", Unit::Volt),
            vec![vin, NetId::GROUND],
        );
        c.add_part("R1", PartKind::Resistor, v("10", Unit::Ohm), vec![vin, a]);
        c.add_part("L1", PartKind::Inductor, v("1m", Unit::Henry), vec![a, out]);
        c.add_part(
            "C1",
            PartKind::Capacitor,
            v("1u", Unit::Farad),
            vec![out, NetId::GROUND],
        );

        let (l, cap, r): (f64, f64, f64) = (1e-3, 1e-6, 10.0);
        let w0 = 1.0 / (l * cap).sqrt();
        let alpha = r / (2.0 * l);
        let w_d = (w0 * w0 - alpha * alpha).sqrt();
        let t_peak = std::f64::consts::PI / w_d; // ≈ 100.6 µs

        let h = 2e-8; // ≈ 5000 steps to the first peak keeps Backward-Euler honest
        let sigs = Engine::new()
            .run(
                &c,
                &Analysis::Transient {
                    duration: 3.0 * t_peak,
                    step: Some(h),
                },
            )
            .unwrap();

        let name = format!("v(n{})", c.resolve(out).0);
        let sig = sigs
            .iter()
            .find(|s| s.name == name)
            .expect("v(out) present");
        let (xs, ys) = (&sig.x, sig.real().unwrap());

        // Locate the first local maximum of v(C) — the overshoot peak.
        let mut peak_i = 0usize;
        for i in 1..ys.len() - 1 {
            if ys[i] >= ys[i - 1] && ys[i] > ys[i + 1] {
                peak_i = i;
                break;
            }
        }
        assert!(peak_i > 0, "no overshoot peak found — not underdamped?");

        // Underdamped ⇒ the cap voltage overshoots its 5 V DC final value.
        assert!(
            ys[peak_i] > 5.0,
            "peak {} did not overshoot 5 V",
            ys[peak_i]
        );
        // The first peak lands at π/ω_d, within 2% (Backward-Euler adds slight damping).
        let rel = (xs[peak_i] - t_peak).abs() / t_peak;
        assert!(
            rel <= 0.02,
            "first peak at {} s, expected {t_peak} s (rel err {rel})",
            xs[peak_i]
        );
    }

    #[test]
    fn missing_ground_is_reported_clearly() {
        // A source and resistor with no ground reference → singular, not a panic.
        let mut c = Circuit::new("floating");
        let a = c.new_net();
        let b = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            v("5", Unit::Volt),
            vec![a, b],
        );
        c.add_part("R1", PartKind::Resistor, v("1k", Unit::Ohm), vec![a, b]);
        let err = Engine::new()
            .run(&c, &Analysis::OperatingPoint)
            .unwrap_err();
        assert_eq!(err, SimError::Singular);
    }
}
