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
mod mna_complex;

use std::collections::BTreeMap;
use std::error::Error;
use std::f64::consts::TAU;
use std::fmt;

use ee_core::graph::{Circuit, NetId, Part, PartKind, Waveform};
use ee_core::{Complex, Signal, Unit};
use mna::System;
use mna_complex::ComplexSystem;

/// The analyses the engine can run.
#[derive(Debug, Clone)]
pub enum Analysis {
    /// DC operating point (capacitors open, inductors shorted).
    OperatingPoint,
    /// Time-domain transient over `duration` seconds. `step` is the timestep;
    /// `None` picks `duration / 1000`.
    Transient { duration: f64, step: Option<f64> },
    /// Small-signal AC sweep (Bode) over `[f_start, f_stop]` hertz, log-spaced at
    /// `points_per_decade`. The circuit is linearized about its DC operating point
    /// and driven by a 1∠0 stimulus on every source marked as an AC drive (a
    /// source carrying a [`Waveform::Sine`]). (`CP-SIM-M1.4`.)
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
    /// Newton–Raphson did not reach the voltage/current tolerance within the
    /// iteration cap. Carries the cap that was hit.
    NonConvergence(usize),
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
            SimError::NonConvergence(iters) => {
                write!(
                    f,
                    "nonlinear solve did not converge within {iters} Newton–Raphson iterations"
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
            Analysis::Ac {
                f_start,
                f_stop,
                points_per_decade,
            } => ac(circuit, &layout, *f_start, *f_stop, *points_per_decade),
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
            | PartKind::CurrentSource
            | PartKind::Diode => {}
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

/// The value an independent source presents at transient time `t`.
///
/// `t = None` asks for the DC / operating-point bias — the constant for
/// [`Waveform::Dc`], the `offset` of a sine, the `v1` of a pulse — which is also
/// what the `t = 0` transient sample uses. `t = Some(secs)` evaluates the
/// waveform formula at that time. A `Dc` source needs a [`Value`]; a `Sine` or
/// `Pulse` carries its own levels and needs none.
fn source_value(part: &Part, t: Option<f64>) -> Result<f64, SimError> {
    Ok(match &part.waveform {
        Waveform::Dc => magnitude(part)?,
        Waveform::Sine {
            amplitude,
            freq,
            offset,
            phase_deg,
        } => match t {
            None => *offset,
            Some(t) => offset + amplitude * (TAU * freq * t + phase_deg.to_radians()).sin(),
        },
        Waveform::Pulse {
            v1,
            v2,
            delay,
            width,
            period,
        } => match t {
            None => *v1,
            Some(t) => pulse_value(*v1, *v2, *delay, *width, *period, t),
        },
    })
}

/// A single/repeating pulse level at time `t` (no rise/fall time): `v1` before
/// `delay`, `v2` for `width` after it, `v1` for the rest of each `period`. A
/// `period <= 0` makes the pulse fire once and stay at `v1` afterwards.
fn pulse_value(v1: f64, v2: f64, delay: f64, width: f64, period: f64, t: f64) -> f64 {
    if t < delay {
        return v1;
    }
    let mut local = t - delay;
    if period > 0.0 {
        local %= period;
    }
    if local < width {
        v2
    } else {
        v1
    }
}

/// Whether a source is the AC small-signal drive: a source carrying a sine
/// waveform. Such sources are stamped with a 1∠0 stimulus in AC analysis; every
/// other source is quiet (a short for V, an open for I), as in SPICE.
fn is_ac_drive(part: &Part) -> bool {
    matches!(part.waveform, Waveform::Sine { .. })
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

/// Stamp every voltage source, each using its own branch-current row. `t` selects
/// the source level: `None` for the operating-point bias, `Some(secs)` for the
/// waveform value at that transient time.
fn stamp_vsources(
    sys: &mut System,
    circuit: &Circuit,
    layout: &Layout,
    t: Option<f64>,
) -> Result<(), SimError> {
    for (branch, &pi) in layout.vsources.iter().enumerate() {
        let part = &circuit.parts()[pi];
        let volts = source_value(part, t)?;
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
/// drives that current from `a` into `b` (no new unknown — a pure RHS term). `t`
/// selects the level: `None` for the operating-point bias, `Some(secs)` for the
/// waveform value at that transient time (a DC source is the same either way).
fn stamp_current_sources(
    sys: &mut System,
    circuit: &Circuit,
    layout: &Layout,
    t: Option<f64>,
) -> Result<(), SimError> {
    for part in circuit.parts() {
        if matches!(part.kind, PartKind::CurrentSource) {
            let amps = source_value(part, t)?;
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

// ---------------------------------------------------------------------------
// Nonlinear devices + Newton–Raphson
// ---------------------------------------------------------------------------

/// Newton–Raphson iteration cap. Hitting it is reported as
/// [`SimError::NonConvergence`]. Convergence aids (gmin / source stepping) that
/// let far more circuits reach this cap arrive at `CP-SIM-M1.3`.
const NR_MAX_ITERS: usize = 100;
/// Converged when the largest node-voltage change between iterations is below
/// this, in volts.
const NR_V_TOL: f64 = 1e-9;
/// …and the largest device-current change between iterations is below this, in
/// amperes. Both must hold at once.
const NR_I_TOL: f64 = 1e-12;

/// The contract a nonlinear two-terminal device fulfils for the Newton–Raphson
/// loop. Given the branch voltage `v = v(a) − v(b)` it reports the current it
/// carries and its small-signal conductance `g = dI/dV` at that bias, plus a
/// limiting rule that keeps the per-iteration voltage step sane. The solver
/// stamps the device's Norton companion (a conductance `g` in parallel with a
/// current source `I_eq = I(v) − g·v`) and iterates. New device types (BJT /
/// MOSFET at `CP-SIM-M1.2`) implement these three methods and drop straight in.
trait NonlinearDevice {
    /// Terminal nets `(a, b)`; the branch voltage is `v(a) − v(b)`.
    fn terminals(&self) -> (NetId, NetId);
    /// Linearize about branch voltage `v`, returning `(I(v), dI/dV)`.
    fn evaluate(&self, v: f64) -> (f64, f64);
    /// Junction limiting: given the raw branch voltage from the last solve and
    /// the previous iteration's (already limited) value, return a limited
    /// voltage so `exp` cannot run away between iterations.
    fn limit(&self, v_new: f64, v_old: f64) -> f64;
}

/// Shockley-diode model parameters. Defaults are a generic small-signal silicon
/// diode at 300 K.
#[derive(Debug, Clone, Copy)]
struct DiodeModel {
    /// Saturation current `I_s` (A).
    is: f64,
    /// Emission coefficient `n` (dimensionless).
    n: f64,
    /// Thermal voltage `V_t = kT/q` (V).
    vt: f64,
}

impl Default for DiodeModel {
    fn default() -> Self {
        Self {
            is: 1e-14,
            n: 1.0,
            vt: 0.025_852,
        }
    }
}

/// A junction diode across `(a, b)` = `(anode, cathode)`.
struct Diode {
    a: NetId,
    b: NetId,
    model: DiodeModel,
}

impl NonlinearDevice for Diode {
    fn terminals(&self) -> (NetId, NetId) {
        (self.a, self.b)
    }

    fn evaluate(&self, v: f64) -> (f64, f64) {
        // Shockley: I = Is·(exp(V/(n·Vt)) − 1), and g = dI/dV = (Is/(n·Vt))·exp(V/(n·Vt)).
        let vte = self.model.n * self.model.vt;
        let ex = (v / vte).exp();
        let i = self.model.is * (ex - 1.0);
        let g = (self.model.is / vte) * ex;
        (i, g)
    }

    fn limit(&self, v_new: f64, v_old: f64) -> f64 {
        // Standard SPICE pn-junction limiting (`pnjlim`): once the forward
        // voltage is past `vcrit` and the step exceeds a thermal voltage, map the
        // raw Newton step through a logarithm so `exp` cannot overflow. Reverse
        // and small steps pass through untouched. Fuller aids are `CP-SIM-M1.3`.
        let vte = self.model.n * self.model.vt;
        let vcrit = vte * (vte / (std::f64::consts::SQRT_2 * self.model.is)).ln();
        if v_new > vcrit && (v_new - v_old).abs() > vte {
            if v_old > 0.0 {
                let arg = 1.0 + (v_new - v_old) / vte;
                if arg > 0.0 {
                    v_old + vte * arg.ln()
                } else {
                    vcrit
                }
            } else if v_new > 0.0 {
                vte * (v_new / vte).ln()
            } else {
                v_new
            }
        } else {
            v_new
        }
    }
}

/// Build the diode model for a part: defaults, with `I_s` overridden by the
/// part's value (amperes) when a sensible positive magnitude is present.
fn diode_model(part: &Part) -> DiodeModel {
    let mut m = DiodeModel::default();
    if let Some(v) = &part.value {
        if v.magnitude > 0.0 && v.magnitude.is_finite() {
            m.is = v.magnitude;
        }
    }
    m
}

/// Collect every nonlinear device in the circuit as a boxed [`NonlinearDevice`].
/// This is the one place a new device kind is wired into the solver.
fn collect_nonlinear(circuit: &Circuit) -> Result<Vec<Box<dyn NonlinearDevice>>, SimError> {
    let mut devices: Vec<Box<dyn NonlinearDevice>> = Vec::new();
    for part in circuit.parts() {
        if matches!(part.kind, PartKind::Diode) {
            let (a, b) = terminals2(part)?;
            devices.push(Box::new(Diode {
                a,
                b,
                model: diode_model(part),
            }));
        }
    }
    Ok(devices)
}

/// Newton–Raphson solve of an MNA system that may contain nonlinear devices.
///
/// `stamp_linear` stamps every *linear* contribution (R / V / I, and the C / L
/// companions during a transient step) into a fresh [`System`] — exactly the
/// system the M0 solver built. Each iteration rebuilds that system, linearizes
/// every nonlinear device about the running guess and stamps its Norton
/// companion, solves, applies junction limiting, and tests convergence on both
/// the node-voltage step and the device-current step.
///
/// With **no** nonlinear devices the routine stamps and solves exactly once,
/// reproducing the linear M0 result bit-for-bit — so the existing linear tests
/// do not move.
fn solve_nr(
    circuit: &Circuit,
    layout: &Layout,
    size: usize,
    devices: &[Box<dyn NonlinearDevice>],
    stamp_linear: &dyn Fn(&mut System) -> Result<(), SimError>,
) -> Result<Vec<f64>, SimError> {
    // Purely linear: one stamp-and-solve is exact (and identical to M0).
    if devices.is_empty() {
        let mut sys = System::new(size);
        stamp_linear(&mut sys)?;
        return sys.solve().ok_or(SimError::Singular);
    }

    let mut x = vec![0.0; size]; // initial guess: every node at 0 V
    let mut v_prev = vec![0.0; devices.len()]; // last (limited) branch voltages
    let mut i_prev = vec![0.0; devices.len()]; // last device currents

    for _ in 0..NR_MAX_ITERS {
        let mut sys = System::new(size);
        stamp_linear(&mut sys)?;

        let mut i_now = vec![0.0; devices.len()];
        for (k, dev) in devices.iter().enumerate() {
            let (a, b) = dev.terminals();
            let (na, nb) = (layout.node_of(circuit, a), layout.node_of(circuit, b));
            let v = dev.limit(across(&x, na, nb), v_prev[k]);
            v_prev[k] = v;
            let (i, g) = dev.evaluate(v);
            i_now[k] = i;
            // Norton companion: conductance `g` in parallel with a current source
            // `I_eq = i − g·v`. The constant part moves to the RHS exactly as the
            // C / L companions do — `stamp_current(a, b, g·v − i)`.
            sys.stamp_conductance(na, nb, g);
            sys.stamp_current(na, nb, g * v - i);
        }

        let x_new = sys.solve().ok_or(SimError::Singular)?;
        // A diverging solution (overflow → inf/NaN) must never read as converged.
        // `f64::max` ignores NaN, so test finiteness explicitly before the tol
        // check rather than letting a NaN step fold to a spurious zero.
        let finite = x_new.iter().chain(&i_now).all(|v| v.is_finite());
        let dv = x_new
            .iter()
            .zip(&x)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f64::max);
        let di = i_now
            .iter()
            .zip(&i_prev)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f64::max);
        x = x_new;
        i_prev = i_now;
        if finite && dv < NR_V_TOL && di < NR_I_TOL {
            return Ok(x);
        }
    }
    Err(SimError::NonConvergence(NR_MAX_ITERS))
}

/// Solve the DC operating point: resistors + sources, capacitors open, inductors
/// shorted, and any nonlinear device driven to steady state by Newton–Raphson.
fn solve_dc(circuit: &Circuit, layout: &Layout) -> Result<Vec<f64>, SimError> {
    let size = layout.n_nodes() + layout.vsources.len() + layout.inductors.len();
    let devices = collect_nonlinear(circuit)?;
    solve_nr(circuit, layout, size, &devices, &|sys| {
        stamp_resistors(sys, circuit, layout)?;
        // Operating point: sources at their bias (`None`).
        stamp_vsources(sys, circuit, layout, None)?;
        stamp_current_sources(sys, circuit, layout, None)?;
        stamp_inductor_shorts(sys, circuit, layout)?;
        Ok(())
    })
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

    // Nonlinear devices are re-linearized inside the per-step Newton–Raphson
    // loop, on top of the C / L companions.
    let diodes = collect_nonlinear(circuit)?;

    let mut times: Vec<f64> = Vec::with_capacity(steps + 1);
    let mut traces: Vec<Vec<f64>> = vec![Vec::with_capacity(steps + 1); n];
    let mut cap_prev = vec![0.0; caps.len()]; // 0 V initial condition (UIC)
    let mut ind_prev = vec![0.0; inds.len()]; // 0 A initial condition (de-energized)

    // t = 0: hold each capacitor at its initial voltage (a 0 V source), solve the
    // rest, and record. This is the true starting point of every curve.
    {
        let stamp = |sys: &mut System| -> Result<(), SimError> {
            stamp_resistors(sys, circuit, layout)?;
            // t = 0 uses the waveform bias (sine → offset, pulse → v1).
            stamp_vsources(sys, circuit, layout, None)?;
            stamp_current_sources(sys, circuit, layout, None)?;
            // Inductors are open at t = 0 (i(0) = 0), so they are not stamped.
            for (ci, (a, b, _)) in caps.iter().enumerate() {
                let k = n + m + ci;
                sys.stamp_vsource(
                    layout.node_of(circuit, *a),
                    layout.node_of(circuit, *b),
                    k,
                    0.0,
                );
            }
            Ok(())
        };
        let x0 = solve_nr(circuit, layout, n + m + caps.len(), &diodes, &stamp)?;
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

    // t = h, 2h, … : companion-stamp the reactive devices, Newton-solve, advance.
    for k in 1..=steps {
        let t = k as f64 * h;
        let stamp = |sys: &mut System| -> Result<(), SimError> {
            stamp_resistors(sys, circuit, layout)?;
            // Sources evaluated at the current transient time.
            stamp_vsources(sys, circuit, layout, Some(t))?;
            stamp_current_sources(sys, circuit, layout, Some(t))?;
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
                // History source `i_prev` oriented a → b: current flows a into b.
                sys.stamp_current(ib, ia, ind_prev[li]);
            }
            Ok(())
        };
        let x = solve_nr(circuit, layout, n + m, &diodes, &stamp)?;
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

// ---------------------------------------------------------------------------
// AC small-signal analysis (CP-SIM-M1.4)
// ---------------------------------------------------------------------------

/// Log-spaced sweep frequencies from `f_start` to `f_stop` at `points_per_decade`.
/// Endpoints are included (the last point lands on `f_stop`).
fn log_frequencies(f_start: f64, f_stop: f64, ppd: u32) -> Vec<f64> {
    let decades = (f_stop / f_start).log10();
    let n = (decades * ppd as f64).round().max(1.0) as usize;
    let mut out = Vec::with_capacity(n + 1);
    for i in 0..=n {
        out.push(f_start * 10f64.powf(i as f64 / ppd as f64));
    }
    // Pin the last sample exactly on f_stop (float drift in the exponent aside).
    if let Some(last) = out.last_mut() {
        *last = f_stop;
    }
    out
}

/// Small-signal AC analysis: linearize about the DC operating point, then solve a
/// complex MNA system at each log-spaced frequency and return one complex
/// [`Signal`] per node (the node-voltage phasor vs. frequency).
///
/// Reactive devices enter by their admittance — a capacitor as `jωC`, an inductor
/// as `1/(jωL)` — so, unlike the DC operating point, inductors are **not** branch
/// shorts here and add no branch row; the only branch rows are the voltage
/// sources'. Nonlinear devices (the diode) are replaced by their small-signal
/// conductance `g = dI/dV` evaluated at the DC bias, reusing the Newton–Raphson
/// operating point. The stimulus is a 1∠0 phasor on every AC-drive source
/// ([`is_ac_drive`]); all other sources are quiet (V → short, I → open).
fn ac(
    circuit: &Circuit,
    layout: &Layout,
    f_start: f64,
    f_stop: f64,
    points_per_decade: u32,
) -> Result<Vec<Signal>, SimError> {
    if !f_start.is_finite()
        || !f_stop.is_finite()
        || f_start <= 0.0
        || f_stop <= f_start
        || points_per_decade == 0
    {
        return Err(SimError::BadValue("AC sweep range/points".into()));
    }

    // Bias point for linearizing nonlinear devices (and verifying the topology).
    let x_dc = solve_dc(circuit, layout)?;

    let n = layout.n_nodes();
    let m = layout.vsources.len();
    let size = n + m; // inductors are admittances in AC — no branch rows.

    // Precompute every frequency-independent stamp once.
    // Resistors → real conductance G.
    let mut conductances: Vec<(Option<usize>, Option<usize>, f64)> = Vec::new();
    // Capacitors → jωC; inductors → 1/(jωL). Stored as (nodes, value) in F / H.
    let mut caps: Vec<(Option<usize>, Option<usize>, f64)> = Vec::new();
    let mut inds: Vec<(Option<usize>, Option<usize>, f64)> = Vec::new();
    for part in circuit.parts() {
        match part.kind {
            PartKind::Resistor => {
                let r = magnitude(part)?;
                if r <= 0.0 {
                    return Err(SimError::BadValue(part.reference.clone()));
                }
                let (a, b) = terminals2(part)?;
                conductances.push((
                    layout.node_of(circuit, a),
                    layout.node_of(circuit, b),
                    1.0 / r,
                ));
            }
            PartKind::Capacitor => {
                let (a, b) = terminals2(part)?;
                caps.push((
                    layout.node_of(circuit, a),
                    layout.node_of(circuit, b),
                    magnitude(part)?,
                ));
            }
            PartKind::Inductor => {
                let l = magnitude(part)?;
                if l <= 0.0 {
                    return Err(SimError::BadValue(part.reference.clone()));
                }
                let (a, b) = terminals2(part)?;
                inds.push((layout.node_of(circuit, a), layout.node_of(circuit, b), l));
            }
            _ => {}
        }
    }

    // Diodes → small-signal conductance g at the DC bias (real admittance).
    let devices = collect_nonlinear(circuit)?;
    let diode_g: Vec<(Option<usize>, Option<usize>, f64)> = devices
        .iter()
        .map(|dev| {
            let (a, b) = dev.terminals();
            let (na, nb) = (layout.node_of(circuit, a), layout.node_of(circuit, b));
            let v = across(&x_dc, na, nb);
            let (_, g) = dev.evaluate(v);
            (na, nb, g)
        })
        .collect();

    // Voltage sources → branch row k with a 1∠0 drive (else a 0 V short).
    let vsource_stamps: Vec<(Option<usize>, Option<usize>, usize, Complex)> = layout
        .vsources
        .iter()
        .enumerate()
        .map(|(branch, &pi)| {
            let part = &circuit.parts()[pi];
            let (plus, minus) = terminals2(part)?;
            let drive = if is_ac_drive(part) {
                Complex::ONE
            } else {
                Complex::ZERO
            };
            Ok::<_, SimError>((
                layout.node_of(circuit, plus),
                layout.node_of(circuit, minus),
                n + branch,
                drive,
            ))
        })
        .collect::<Result<_, _>>()?;

    // AC-drive current sources → a 1∠0 injection a → b (quiet ones are open).
    let mut csource_drives: Vec<(Option<usize>, Option<usize>)> = Vec::new();
    for part in circuit.parts() {
        if matches!(part.kind, PartKind::CurrentSource) && is_ac_drive(part) {
            let (a, b) = terminals2(part)?;
            csource_drives.push((layout.node_of(circuit, a), layout.node_of(circuit, b)));
        }
    }

    let freqs = log_frequencies(f_start, f_stop, points_per_decade);
    let mut per_node: Vec<Vec<Complex>> = vec![Vec::with_capacity(freqs.len()); n];

    for &f in &freqs {
        let w = TAU * f;
        let mut sys = ComplexSystem::new(size);
        for &(a, b, g) in &conductances {
            sys.stamp_admittance(a, b, Complex::new(g, 0.0));
        }
        for &(a, b, c) in &caps {
            sys.stamp_admittance(a, b, Complex::new(0.0, w * c)); // jωC
        }
        for &(a, b, l) in &inds {
            sys.stamp_admittance(a, b, Complex::new(0.0, -1.0 / (w * l))); // 1/(jωL)
        }
        for &(a, b, g) in &diode_g {
            sys.stamp_admittance(a, b, Complex::new(g, 0.0));
        }
        for &(plus, minus, k, drive) in &vsource_stamps {
            sys.stamp_vsource(plus, minus, k, drive);
        }
        for &(a, b) in &csource_drives {
            // 1∠0 from a into b — same direction convention as the DC stamp.
            sys.stamp_current(b, a, Complex::ONE);
        }

        let x = sys.solve().ok_or(SimError::Singular)?;
        for (d, col) in per_node.iter_mut().enumerate() {
            col.push(x[d]);
        }
    }

    Ok(per_node
        .into_iter()
        .enumerate()
        .map(|(d, col)| {
            Signal::freq(
                format!("v(n{})", layout.inv[d]),
                Unit::Volt,
                freqs.clone(),
                col,
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

    // --- Nonlinear (diode) — CP-SIM-M1.1 -----------------------------------

    /// Model constants the tests check against (must match [`DiodeModel`] defaults).
    const IS: f64 = 1e-14;
    const VTE: f64 = 1.0 * 0.025_852; // n·Vt with n = 1

    #[test]
    fn diode_iv() {
        // Force a known forward current with an ideal current source and read the
        // junction voltage back: it must equal the inverse-Shockley prediction
        // V = n·Vt·ln(I/Is + 1), and the model's own I(V) at that point equals I.
        for &i in &[1e-9, 1e-6, 1e-3, 1e-2, 0.1_f64] {
            let mut c = Circuit::new("diode_iv");
            let node = c.new_net();
            // I injected from ground into `node`; the diode (anode = node,
            // cathode = gnd) carries it forward to ground.
            c.add_part(
                "I1",
                PartKind::CurrentSource,
                v(&format!("{i}"), Unit::Ampere),
                vec![NetId::GROUND, node],
            );
            c.add_part("D1", PartKind::Diode, None, vec![node, NetId::GROUND]);

            let layout = Layout::build(&c);
            let x = solve_dc(&c, &layout).unwrap();
            let vd = x[layout.node_of(&c, node).unwrap()];

            let expected = VTE * (i / IS + 1.0).ln();
            assert!(
                (vd - expected).abs() <= 1e-7,
                "I={i}: vd={vd}, expected {expected}, err {}",
                (vd - expected).abs()
            );
            let i_model = IS * ((vd / VTE).exp() - 1.0);
            assert!(
                (i_model - i).abs() <= 1e-9 + 1e-6 * i,
                "I={i}: model current {i_model} (rel err {})",
                (i_model - i).abs() / i
            );
        }
    }

    #[test]
    fn diode_resistor_op_converges() {
        // 5 V ─ 1k ─ diode ─ gnd. The operating point must settle to a sane
        // junction drop whose Shockley current equals the resistor current
        // (KCL at the midpoint — the real convergence check).
        let mut c = Circuit::new("diode_r");
        let vin = c.new_net();
        let mid = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            v("5", Unit::Volt),
            vec![vin, NetId::GROUND],
        );
        c.add_part("R1", PartKind::Resistor, v("1k", Unit::Ohm), vec![vin, mid]);
        c.add_part("D1", PartKind::Diode, None, vec![mid, NetId::GROUND]);

        let layout = Layout::build(&c);
        let x = solve_dc(&c, &layout).unwrap();
        let vd = x[layout.node_of(&c, mid).unwrap()];

        // Silicon junction drop lands in the usual window.
        assert!(
            (0.6..0.8).contains(&vd),
            "junction drop {vd} V out of the expected window"
        );
        let i_r = (5.0 - vd) / 1000.0; // resistor current
        assert!(i_r > 0.0, "diode not forward-biased");
        // KCL at `mid`: the diode's own I(vd) equals the resistor current.
        let i_d = IS * ((vd / VTE).exp() - 1.0);
        assert!(
            (i_d - i_r).abs() <= 1e-9 + 1e-6 * i_r,
            "I_d={i_d} vs I_R={i_r} (rel err {})",
            (i_d - i_r).abs() / i_r
        );
    }

    #[test]
    fn nonconvergence_is_reported() {
        // Pathological: an absurd 1e300 A forced through a lone diode drives the
        // junction past the floating-point exp range, so Newton-Raphson can never
        // settle. The solver must hit the cap and report it — no panic, no hang.
        let mut c = Circuit::new("runaway");
        let node = c.new_net();
        c.add_part(
            "I1",
            PartKind::CurrentSource,
            v("1e300", Unit::Ampere),
            vec![NetId::GROUND, node],
        );
        c.add_part("D1", PartKind::Diode, None, vec![node, NetId::GROUND]);

        let err = Engine::new()
            .run(&c, &Analysis::OperatingPoint)
            .unwrap_err();
        assert!(
            matches!(err, SimError::NonConvergence(_)),
            "expected non-convergence, got {err:?}"
        );
    }

    #[test]
    fn linear_circuit_is_unaffected_by_the_nr_path() {
        // A circuit with no nonlinear devices must take exactly one solve and give
        // the same answer as the old linear path (regression guard for the divider).
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
        assert!((x[layout.node_of(&c, mid).unwrap()] - 5.0).abs() < 1e-12);
    }

    // --- Time-varying sources — CP-SIM-M1 ----------------------------------

    #[test]
    fn sine_source_reproduces_its_formula_in_transient() {
        // An unloaded sine voltage source (1 MΩ bleed to ground so the node is not
        // floating) must trace offset + A·sin(2πf t + φ) at every sampled time.
        let (amp, freq, offset, phase_deg) = (2.0, 1_000.0, 1.0, 30.0_f64);
        let mut c = Circuit::new("sine");
        let out = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            None, // a sine carries its own levels — no DC Value needed
            vec![out, NetId::GROUND],
        );
        c.add_part(
            "R1",
            PartKind::Resistor,
            v("1meg", Unit::Ohm),
            vec![out, NetId::GROUND],
        );
        assert!(c.set_waveform(
            "V1",
            Waveform::Sine {
                amplitude: amp,
                freq,
                offset,
                phase_deg,
            },
        ));

        let h = 2e-6; // 500 steps per 1 kHz cycle
        let sigs = Engine::new()
            .run(
                &c,
                &Analysis::Transient {
                    duration: 2e-3,
                    step: Some(h),
                },
            )
            .unwrap();
        let name = format!("v(n{})", c.resolve(out).0);
        let sig = sigs.iter().find(|s| s.name == name).unwrap();

        // t = 0 holds the DC bias (the offset), per the waveform contract.
        let y = sig.real().unwrap();
        assert!((y[0] - offset).abs() < 1e-9, "t=0 bias {} != offset", y[0]);
        // Every t > 0 sample matches the analytic sine within tight tolerance.
        for (&t, &val) in sig.x.iter().zip(y).skip(1) {
            let expected = offset + amp * (TAU * freq * t + phase_deg.to_radians()).sin();
            assert!(
                (val - expected).abs() <= 1e-6,
                "t={t}: got {val}, expected {expected}"
            );
        }
    }

    #[test]
    fn pulse_source_steps_v1_to_v2_at_the_right_times() {
        // PULSE(v1=0, v2=5, delay=1ms, width=1ms, period=4ms): low until 1 ms,
        // high for [1,2) ms, low again, then it repeats one period later.
        let mut c = Circuit::new("pulse");
        let out = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            None,
            vec![out, NetId::GROUND],
        );
        c.add_part(
            "R1",
            PartKind::Resistor,
            v("1meg", Unit::Ohm),
            vec![out, NetId::GROUND],
        );
        assert!(c.set_waveform(
            "V1",
            Waveform::Pulse {
                v1: 0.0,
                v2: 5.0,
                delay: 1e-3,
                width: 1e-3,
                period: 4e-3,
            },
        ));

        let h = 1e-5;
        let sigs = Engine::new()
            .run(
                &c,
                &Analysis::Transient {
                    duration: 6e-3,
                    step: Some(h),
                },
            )
            .unwrap();
        let name = format!("v(n{})", c.resolve(out).0);
        let sig = sigs.iter().find(|s| s.name == name).unwrap();

        // Sample the trace at representative instants and check the level.
        let level_at = |target: f64| -> f64 {
            let i = sig
                .x
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    (*a - target)
                        .abs()
                        .partial_cmp(&(*b - target).abs())
                        .unwrap()
                })
                .unwrap()
                .0;
            sig.real().unwrap()[i]
        };
        assert!(level_at(0.5e-3).abs() < 1e-9, "before delay should be v1");
        assert!(
            (level_at(1.5e-3) - 5.0).abs() < 1e-9,
            "in pulse should be v2"
        );
        assert!(level_at(3.0e-3).abs() < 1e-9, "after width should be v1");
        // One period later (delay + period = 5 ms) the pulse is high again.
        assert!(
            (level_at(5.5e-3) - 5.0).abs() < 1e-9,
            "next period should be v2"
        );
    }

    // --- AC small-signal analysis — CP-SIM-M1.4 ----------------------------

    #[test]
    fn rc_ac_bode() {
        // RC low-pass: V1(AC) ─ R ─•─ C ─ gnd, output v(out). H = 1/(1+jωRC),
        // so |H| is −3 dB and ∠H is −45° at fc = 1/(2πRC); ≈0 dB far below,
        // −20 dB/decade far above.
        let (r, cap) = (1_000.0_f64, 159.154_943e-9_f64); // fc ≈ 1 kHz
        let fc = 1.0 / (TAU * r * cap);

        let mut c = Circuit::new("rc_ac");
        let inp = c.new_net();
        let out = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            None,
            vec![inp, NetId::GROUND],
        );
        c.add_part("R1", PartKind::Resistor, v("1k", Unit::Ohm), vec![inp, out]);
        c.add_part(
            "C1",
            PartKind::Capacitor,
            Some(Value::new(cap, Unit::Farad)),
            vec![out, NetId::GROUND],
        );
        // Mark V1 as the AC drive (a sine stimulus → 1∠0 in AC).
        assert!(c.set_waveform(
            "V1",
            Waveform::Sine {
                amplitude: 1.0,
                freq: fc,
                offset: 0.0,
                phase_deg: 0.0,
            },
        ));

        let sigs = Engine::new()
            .run(
                &c,
                &Analysis::Ac {
                    f_start: fc / 100.0,
                    f_stop: fc * 100.0,
                    points_per_decade: 20,
                },
            )
            .unwrap();

        let name = format!("v(n{})", c.resolve(out).0);
        let sig = sigs.iter().find(|s| s.name == name).unwrap();
        let (freqs, z) = (&sig.x, sig.complex().unwrap());

        // Helper: the sample whose frequency is closest to a target.
        let nearest = |target: f64| -> usize {
            freqs
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    (*a - target)
                        .abs()
                        .partial_cmp(&(*b - target).abs())
                        .unwrap()
                })
                .unwrap()
                .0
        };

        // At fc: −3 dB (±0.1 dB) and −45° (±1°).
        let ic = nearest(fc);
        assert!(
            (freqs[ic] - fc).abs() / fc < 0.02,
            "no sample near fc (got {} Hz)",
            freqs[ic]
        );
        let db_fc = z[ic].magnitude_db();
        let ph_fc = z[ic].phase_deg();
        assert!((db_fc + 3.0103).abs() <= 0.1, "fc gain {db_fc} dB");
        assert!((ph_fc + 45.0).abs() <= 1.0, "fc phase {ph_fc}°");

        // Two decades below fc: essentially unity gain (≈ 0 dB).
        let db_low = z[nearest(fc / 100.0)].magnitude_db();
        assert!(db_low.abs() <= 0.1, "low-freq gain {db_low} dB");

        // Far above fc the slope is −20 dB/decade: compare one decade apart.
        let db_10fc = z[nearest(fc * 10.0)].magnitude_db();
        let db_100fc = z[nearest(fc * 100.0)].magnitude_db();
        let slope = db_100fc - db_10fc; // dB per decade
        assert!(
            (slope + 20.0).abs() <= 0.5,
            "high-freq slope {slope} dB/decade (points {db_10fc}, {db_100fc})"
        );
        // And the input node is the 1∠0 reference (0 dB).
        let in_name = format!("v(n{})", c.resolve(inp).0);
        let in_sig = sigs.iter().find(|s| s.name == in_name).unwrap();
        assert!(in_sig.complex().unwrap()[ic].magnitude_db().abs() < 1e-9);
    }

    #[test]
    fn ac_rejects_a_bad_sweep_range() {
        let mut c = Circuit::new("rc_ac");
        let inp = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            None,
            vec![inp, NetId::GROUND],
        );
        c.add_part(
            "R1",
            PartKind::Resistor,
            v("1k", Unit::Ohm),
            vec![inp, NetId::GROUND],
        );
        assert!(c.set_waveform(
            "V1",
            Waveform::Sine {
                amplitude: 1.0,
                freq: 1_000.0,
                offset: 0.0,
                phase_deg: 0.0,
            },
        ));
        let err = Engine::new()
            .run(
                &c,
                &Analysis::Ac {
                    f_start: 0.0, // non-positive start is rejected
                    f_stop: 1_000.0,
                    points_per_decade: 10,
                },
            )
            .unwrap_err();
        assert!(matches!(err, SimError::BadValue(_)), "got {err:?}");
    }
}
