//! # ohmforge-sim
//!
//! The analog / mixed-signal simulation engine — the heart of the "emulator."
//!
//! Builds a Modified Nodal Analysis system from an [`ohmforge_core::Circuit`], solves
//! it, and returns typed [`Signal`]s. See `docs/ARCHITECTURE.md` §
//! "The simulation core."
//!
//! Roadmap: **M0** (here) = linear MNA + DC operating point + a Backward-Euler
//! transient; M1 = nonlinear devices (Newton–Raphson) + AC/DC-sweep;
//! M2 = noise / Monte-Carlo. (docs/ROADMAP.md)
//!
//! This crate takes no clock, no filesystem, and no unseeded randomness, and
//! stays `wasm32`-clean — the same rules as `ohmforge-core`.

mod eig;
mod expr;
mod mna;
mod mna_complex;

use std::collections::BTreeMap;
use std::error::Error;
use std::f64::consts::TAU;
use std::fmt;

use mna::{ReusableSolver, System};
use mna_complex::{ComplexReusableSolver, ComplexSymbolic, ComplexSystem, COMPLEX_KLU_THRESHOLD};
use ohmforge_core::graph::{
    BjtModel, Circuit, DiodeModel, Line, LineKind, MosfetModel, NetId, Part, PartKind, Polarity,
    Switch, Waveform, NOMINAL_TEMP_C,
};
use ohmforge_core::value::Value;
use ohmforge_core::{Complex, Domain, Signal, SignalData, Unit};

/// The time-integration method for a transient analysis (`CP-SIM-M10.2`).
///
/// Backward Euler is the **default** everywhere — [`Analysis::Transient`] always uses
/// it, so every existing transient result is byte-identical. The opt-in
/// [`Trapezoidal`](IntegrationMethod::Trapezoidal) method is selected only through
/// [`Engine::run_transient`], an additive surface that leaves the [`Analysis`] enum
/// (and its many construction sites) untouched.
///
/// * **Backward Euler** — first order, unconditionally stable, but heavily and
///   artificially damped: it smears LC ringing and under-predicts overshoot on
///   resonant / lightly-damped circuits.
/// * **Trapezoidal** — second order and (for an ideal reactance) energy-conserving,
///   so it sustains oscillation where Backward Euler decays it, at the cost of the
///   classic trapezoidal "ringing" on a hard-switched step. Applied to the
///   two-terminal capacitor and (isolated) inductor companions; magnetically-coupled
///   inductors, transmission lines, and controlled switches keep their Backward-Euler
///   companions regardless, and the method needs a fixed timestep (the adaptive
///   driver's step-doubling error control is first-order Backward Euler by design).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IntegrationMethod {
    /// First-order Backward Euler — the default, matching every pre-`CP-SIM-M10.2`
    /// transient bit-for-bit.
    #[default]
    BackwardEuler,
    /// Second-order trapezoidal integration (opt-in via [`Engine::run_transient`]).
    Trapezoidal,
}

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
    /// Small-signal **noise** analysis (`CP-SIM-M2.1`). Propagate every component's
    /// noise generator to the `output` node over `[f_start, f_stop]` (log-spaced at
    /// `points_per_decade`) and return the output-referred noise power spectral
    /// density. At M2.1 the modeled generators are resistor thermal noise, current
    /// PSD `4kT/R` (A²/Hz); the result is one frequency-domain [`Signal`] of the
    /// output voltage PSD in **V²/Hz**. Reuses the complex-MNA transimpedance from
    /// each noise source to the output, exactly as the AC path linearizes.
    Noise {
        output: NetId,
        f_start: f64,
        f_stop: f64,
        points_per_decade: u32,
    },
    /// **Monte-Carlo** tolerance analysis (`CP-SIM-M2.2`). Run `inner` `runs` times,
    /// each time perturbing every part that carries a [`Circuit::tolerance`] within
    /// its ±tolerance band using a seeded in-crate PRNG, and record the measured
    /// `output`-node value at sample index `sample` from each run. Returns one
    /// [`Signal`] of the per-run samples (the x axis is the run index), from which
    /// a spread (std-dev, min/max) follows. A fixed `seed` reproduces the samples
    /// bit-for-bit; a different seed gives a different-but-deterministic draw.
    MonteCarlo {
        seed: u64,
        runs: usize,
        output: NetId,
        sample: usize,
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
    /// A part is wired with the wrong number of terminals for its kind.
    WrongTerminals(String),
    /// A device or analysis not modeled at this milestone.
    Unsupported(&'static str),
    /// The MNA matrix is singular — usually a floating node or missing ground.
    Singular,
    /// Newton–Raphson did not reach the voltage/current tolerance within the
    /// iteration cap. Carries the cap that was hit.
    NonConvergence(usize),
    /// A sweep names a part reference that is not in the circuit.
    UnknownPart(String),
    /// A current-controlled source (CCVS/CCCS) names a controlling element that is
    /// absent, or is not a branch-carrying voltage source whose current we can
    /// sense. Carries the offending reference.
    BadControl(String),
    /// A source carries a [`Waveform::Expr`] whose expression could not be parsed
    /// or evaluated. Carries the source's reference. (`CP-SIM-M3.1`.)
    BadExpr(String),
}

impl fmt::Display for SimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SimError::MissingValue(r) => write!(f, "part {r} has no value"),
            SimError::BadValue(r) => write!(f, "part {r} has an invalid value"),
            SimError::WrongTerminals(r) => {
                write!(f, "part {r} is wired with the wrong number of terminals")
            }
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
            SimError::UnknownPart(r) => write!(f, "no part named {r} to sweep"),
            SimError::BadControl(r) => write!(
                f,
                "controlled source references {r}, which is not a current-sensing voltage source"
            ),
            SimError::BadExpr(r) => write!(f, "source {r} has an invalid time expression"),
        }
    }
}

impl Error for SimError {}

/// The simulation engine. Holds an optional [`DeviceRegistry`] of plugin device
/// models (`CP-SIM-M4.1`); [`Engine::new`] starts empty (built-in devices only)
/// and [`Engine::with_device`] / [`Engine::register_device`] add external models.
/// Cheap to clone — registered factories are shared behind an `Arc`.
#[derive(Debug, Default, Clone)]
pub struct Engine {
    registry: DeviceRegistry,
}

impl Engine {
    /// A new engine with no plugin devices registered — built-ins only.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an external [`NonlinearDevice`] model, matched to parts carrying
    /// [`PartKind::Other`] with the given `name`, and return the engine
    /// (builder-style). `Engine::run` then stamps that device in every analysis.
    pub fn with_device<F>(mut self, name: impl Into<String>, factory: F) -> Self
    where
        F: Fn(&Part) -> Result<Box<dyn NonlinearDevice>, SimError> + Send + Sync + 'static,
    {
        self.registry.register(name, factory);
        self
    }

    /// Register an external [`NonlinearDevice`] model in place. The `name` matches
    /// parts whose kind is [`PartKind::Other`] carrying that string.
    pub fn register_device<F>(&mut self, name: impl Into<String>, factory: F)
    where
        F: Fn(&Part) -> Result<Box<dyn NonlinearDevice>, SimError> + Send + Sync + 'static,
    {
        self.registry.register(name, factory);
    }

    /// Run an [`Analysis`] against a circuit, returning one [`Signal`] per node.
    pub fn run(&self, circuit: &Circuit, analysis: &Analysis) -> Result<Vec<Signal>, SimError> {
        let reg = &self.registry;
        validate(circuit, reg)?;
        let layout = Layout::build(circuit);
        match analysis {
            Analysis::OperatingPoint => {
                let x = solve_dc(circuit, &layout, reg)?;
                Ok(op_signals(&layout, &x))
            }
            Analysis::Transient { duration, step } => transient(
                circuit,
                &layout,
                *duration,
                *step,
                reg,
                IntegrationMethod::BackwardEuler,
            ),
            Analysis::Ac {
                f_start,
                f_stop,
                points_per_decade,
            } => ac(circuit, &layout, *f_start, *f_stop, *points_per_decade, reg),
            Analysis::DcSweep {
                source,
                start,
                stop,
                step,
            } => dc_sweep(circuit, source, *start, *stop, *step, reg),
            Analysis::Parametric {
                source,
                values,
                inner,
            } => self.parametric(circuit, source, values, inner),
            Analysis::Noise {
                output,
                f_start,
                f_stop,
                points_per_decade,
            } => noise(
                circuit,
                &layout,
                *output,
                *f_start,
                *f_stop,
                *points_per_decade,
                reg,
            ),
            Analysis::MonteCarlo {
                seed,
                runs,
                output,
                sample,
                inner,
            } => self.monte_carlo(circuit, *seed, *runs, *output, *sample, inner),
        }
    }

    /// Run a fixed-step transient with an explicit [`IntegrationMethod`] — the opt-in
    /// surface for second-order (trapezoidal) integration (`CP-SIM-M10.2`).
    ///
    /// This is **purely additive**: [`Engine::run`] with [`Analysis::Transient`] is
    /// unchanged and always Backward Euler, so every existing result stays
    /// byte-identical; `run_transient` is the only way to select another integrator,
    /// and does not touch the [`Analysis`] enum. It requires an explicit `step`
    /// (`> 0`): a non-Backward-Euler method is incompatible with the adaptive driver's
    /// first-order error control, and `method = BackwardEuler` here is exactly the
    /// fixed-step `Analysis::Transient { step: Some(step) }` path. Returns one
    /// [`Signal`] per node, as [`Engine::run`] does.
    ///
    /// Trapezoidal sustains the ringing of a lightly-damped resonator (an LC tank)
    /// that Backward Euler artificially damps; it applies to the two-terminal
    /// capacitor and isolated-inductor companions, while coupled inductors,
    /// transmission lines, and controlled switches keep Backward Euler.
    pub fn run_transient(
        &self,
        circuit: &Circuit,
        duration: f64,
        step: f64,
        method: IntegrationMethod,
    ) -> Result<Vec<Signal>, SimError> {
        validate(circuit, &self.registry)?;
        let layout = Layout::build(circuit);
        transient(
            circuit,
            &layout,
            duration,
            Some(step),
            &self.registry,
            method,
        )
    }
}

/// Reject devices not modeled so a part never gets silently dropped. A
/// [`PartKind::Other`] part is accepted only when `registry` carries a plugin
/// factory for its name (`CP-SIM-M4.1`); otherwise it is still unsupported.
fn validate(circuit: &Circuit, registry: &DeviceRegistry) -> Result<(), SimError> {
    for part in circuit.parts() {
        match &part.kind {
            PartKind::Resistor
            | PartKind::Capacitor
            | PartKind::Inductor
            | PartKind::VoltageSource
            | PartKind::CurrentSource
            | PartKind::Diode(..)
            | PartKind::Bjt(..)
            | PartKind::Mosfet(..)
            | PartKind::Vcvs(..)
            | PartKind::Vccs(..)
            | PartKind::Ccvs(..)
            | PartKind::Cccs(..) => {}
            PartKind::Other(name) if registry.contains(name) => {}
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
    /// Part indices of the VCVS parts. Each, like a voltage source, adds a
    /// branch-current row — placed right after the voltage-source branches so the
    /// row layout is identical in the DC, transient, and AC systems.
    vcvs: Vec<usize>,
    /// Part indices of the CCVS parts. Each adds a branch-current row, after the
    /// VCVS branches.
    ccvs: Vec<usize>,
    /// Part indices of the inductors. At the DC operating point each is a short
    /// (a 0 V source) and so adds its own branch-current row, after the
    /// controlled-source branches.
    inductors: Vec<usize>,
}

impl Layout {
    fn build(circuit: &Circuit) -> Self {
        let mut index = BTreeMap::new();
        let mut inv = Vec::new();
        let see = |index: &mut BTreeMap<u32, usize>, inv: &mut Vec<u32>, t: NetId| {
            let r = circuit.resolve(t).0;
            if r != NetId::GROUND.0 && !index.contains_key(&r) {
                index.insert(r, inv.len());
                inv.push(r);
            }
        };
        for part in circuit.parts() {
            for &t in &part.terminals {
                see(&mut index, &mut inv, t);
            }
        }
        // Transmission-line ports are nets too, even if only a line touches them,
        // so they get a node row (`CP-SIM-M7.2`). No lines ⇒ this loop is empty and
        // the node numbering is exactly as before.
        for line in circuit.lines() {
            for t in [line.a1, line.a2, line.b1, line.b2] {
                see(&mut index, &mut inv, t);
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
        let vcvs = kind_indices(|k| matches!(k, PartKind::Vcvs(_)));
        let ccvs = kind_indices(|k| matches!(k, PartKind::Ccvs(..)));
        let inductors = kind_indices(|k| matches!(k, PartKind::Inductor));
        Self {
            index,
            inv,
            vsources,
            vcvs,
            ccvs,
            inductors,
        }
    }

    fn n_nodes(&self) -> usize {
        self.inv.len()
    }

    /// Absolute row index of the first controlled-source branch — right after the
    /// node block and the voltage-source branches. VCVS branch `e` sits at
    /// `controlled_base() + e`; CCVS branch `c` at `controlled_base() +
    /// vcvs.len() + c`. This offset is the same in the DC, transient-step, and AC
    /// systems, so a controlled source stamps identically in all three.
    fn controlled_base(&self) -> usize {
        self.n_nodes() + self.vsources.len()
    }

    /// Number of branch rows the controlled sources add (one per VCVS and CCVS;
    /// VCCS/CCCS introduce no new unknown).
    fn n_controlled_branches(&self) -> usize {
        self.vcvs.len() + self.ccvs.len()
    }

    /// Absolute branch-row index of the voltage source named `reference`, used as
    /// the current-sense branch by a CCVS/CCCS. `None` if no voltage source in the
    /// circuit carries that reference.
    fn sense_branch(&self, circuit: &Circuit, reference: &str) -> Option<usize> {
        self.vsources
            .iter()
            .position(|&pi| circuit.parts()[pi].reference == reference)
            .map(|b| self.n_nodes() + b)
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

/// A resistor's effective resistance at the circuit's temperature:
/// `R0·(1 + tc·(T − 27 °C))`, where `R0` is its nominal value, `tc` the part's
/// [`Circuit::tempco`] (0 when it carries none), and `T` the
/// [`Circuit::temperature`]. The reference temperature is the nominal
/// [`NOMINAL_TEMP_C`], so a circuit left at the default temperature returns `R0`
/// unchanged — the temperature-independent result, bit-for-bit. A non-positive
/// nominal *or* effective resistance is a [`SimError::BadValue`], exactly as the
/// plain resistor stamp treated a non-positive value. (`CP-SIM-M3.1`.)
fn resistor_value(circuit: &Circuit, part: &Part) -> Result<f64, SimError> {
    let r0 = magnitude(part)?;
    if r0 <= 0.0 {
        return Err(SimError::BadValue(part.reference.clone()));
    }
    let tc = circuit.tempco(&part.reference).unwrap_or(0.0);
    let r = r0 * (1.0 + tc * (circuit.temperature() - NOMINAL_TEMP_C));
    if r <= 0.0 {
        return Err(SimError::BadValue(part.reference.clone()));
    }
    Ok(r)
}

/// The value an independent source presents at transient time `t`.
///
/// `t = None` asks for the DC / operating-point bias — the constant for
/// [`Waveform::Dc`], the `offset` of a sine, the `v1` of a pulse — which is also
/// what the `t = 0` transient sample uses. `t = Some(secs)` evaluates the
/// waveform formula at that time. A `Dc` source needs a [`Value`]; a `Sine`,
/// `Pulse`, or [`Expr`](Waveform::Expr) carries its own levels and needs none. A
/// malformed `Expr` surfaces as [`SimError::BadExpr`] rather than a panic.
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
        // Behavioral source: evaluate the expression at the current time. The
        // operating point / `t = 0` sample (`t = None`) uses its value at `t = 0`.
        Waveform::Expr(src) => expr::eval(src, t.unwrap_or(0.0))
            .map_err(|_| SimError::BadExpr(part.reference.clone()))?,
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

/// The three nets a transistor connects — `[collector/drain, base/gate,
/// emitter/source]` — or an error if it is not wired with exactly three.
fn terminals3(part: &Part) -> Result<[NetId; 3], SimError> {
    match part.terminals.as_slice() {
        [a, b, c] => Ok([*a, *b, *c]),
        _ => Err(SimError::WrongTerminals(part.reference.clone())),
    }
}

/// The four nets a voltage/current-controlled source connects —
/// `[out+, out−, ctrl+, ctrl−]` — or an error if it is not wired with exactly four.
fn terminals4(part: &Part) -> Result<[NetId; 4], SimError> {
    match part.terminals.as_slice() {
        [a, b, c, d] => Ok([*a, *b, *c, *d]),
        _ => Err(SimError::WrongTerminals(part.reference.clone())),
    }
}

/// Stamp every resistor as a conductance between its nodes.
fn stamp_resistors(sys: &mut System, circuit: &Circuit, layout: &Layout) -> Result<(), SimError> {
    for part in circuit.parts() {
        if matches!(part.kind, PartKind::Resistor) {
            let r = resistor_value(circuit, part)?;
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
/// waveform value at that transient time. `scale` multiplies the level — `1.0` for
/// the real circuit, a ramp in `[0, 1]` during source stepping ([`source_stepping`]).
fn stamp_vsources(
    sys: &mut System,
    circuit: &Circuit,
    layout: &Layout,
    t: Option<f64>,
    scale: f64,
) -> Result<(), SimError> {
    for (branch, &pi) in layout.vsources.iter().enumerate() {
        let part = &circuit.parts()[pi];
        let volts = source_value(part, t)? * scale;
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
/// `scale` multiplies the level, as in [`stamp_vsources`] (source stepping).
fn stamp_current_sources(
    sys: &mut System,
    circuit: &Circuit,
    layout: &Layout,
    t: Option<f64>,
    scale: f64,
) -> Result<(), SimError> {
    for part in circuit.parts() {
        if matches!(part.kind, PartKind::CurrentSource) {
            let amps = source_value(part, t)? * scale;
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
    let base = layout.controlled_base() + layout.n_controlled_branches();
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

/// Stamp every linear controlled (dependent) source into the real system
/// (`CP-SIM-M2.3`). These are frequency-independent and carry no state, so the
/// same stamp serves the operating point and each transient step; the AC path has
/// its complex twin, [`stamp_controlled_sources_complex`].
///
/// * **VCVS** (gain `E`): a voltage source on its own branch row `k` constrained
///   to `V(out+) − V(out−) = E·(V(ctrl+) − V(ctrl−))`. The branch row couples the
///   control-node columns with `∓E`, on top of the ordinary voltage-source output
///   coupling.
/// * **VCCS** (transconductance `G`): four Jacobian entries, no new unknown —
///   `I = G·(V(ctrl+) − V(ctrl−))` flowing `out+ → out−`.
/// * **CCVS** (transresistance `R`): like a VCVS, but the branch row couples the
///   sensed voltage source's branch-current column with `−R`.
/// * **CCCS** (gain `F`): couples the sensed branch-current column into the output
///   node rows with `±F`; no new unknown.
fn stamp_controlled_sources(
    sys: &mut System,
    circuit: &Circuit,
    layout: &Layout,
) -> Result<(), SimError> {
    let base = layout.controlled_base();
    let node = |net| layout.node_of(circuit, net);

    // VCVS: output branch like a voltage source, plus the control coupling.
    for (e, &pi) in layout.vcvs.iter().enumerate() {
        let part = &circuit.parts()[pi];
        let PartKind::Vcvs(gain) = part.kind else {
            unreachable!("layout.vcvs holds only VCVS parts")
        };
        let [outp, outm, cp, cm] = terminals4(part)?;
        let k = base + e;
        sys.stamp_vsource(node(outp), node(outm), k, 0.0);
        sys.stamp_matrix(Some(k), node(cp), -gain);
        sys.stamp_matrix(Some(k), node(cm), gain);
    }

    // CCVS: output branch, control coupling onto the sensed branch-current column.
    for (c, &pi) in layout.ccvs.iter().enumerate() {
        let part = &circuit.parts()[pi];
        let PartKind::Ccvs(gain, ctrl) = &part.kind else {
            unreachable!("layout.ccvs holds only CCVS parts")
        };
        let (outp, outm) = terminals2(part)?;
        let k = base + layout.vcvs.len() + c;
        let kc = layout
            .sense_branch(circuit, ctrl)
            .ok_or_else(|| SimError::BadControl(ctrl.clone()))?;
        sys.stamp_vsource(node(outp), node(outm), k, 0.0);
        sys.stamp_matrix(Some(k), Some(kc), -gain);
    }

    // VCCS and CCCS add no branch row — pure (trans)conductance / coupling entries.
    for part in circuit.parts() {
        match &part.kind {
            PartKind::Vccs(g) => {
                let [outp, outm, cp, cm] = terminals4(part)?;
                sys.stamp_matrix(node(outp), node(cp), *g);
                sys.stamp_matrix(node(outp), node(cm), -*g);
                sys.stamp_matrix(node(outm), node(cp), -*g);
                sys.stamp_matrix(node(outm), node(cm), *g);
            }
            PartKind::Cccs(f, ctrl) => {
                let (outp, outm) = terminals2(part)?;
                let kc = layout
                    .sense_branch(circuit, ctrl)
                    .ok_or_else(|| SimError::BadControl(ctrl.clone()))?;
                sys.stamp_matrix(node(outp), Some(kc), *f);
                sys.stamp_matrix(node(outm), Some(kc), -*f);
            }
            _ => {}
        }
    }
    Ok(())
}

/// Stamp every controlled source into the complex AC system — the frequency-
/// independent twin of [`stamp_controlled_sources`]. Each real (trans)conductance
/// or gain enters as a purely real admittance `g + 0j`, so the dependent sources
/// are flat with frequency. The branch-row layout matches the real system: in AC
/// there are no inductor branches, but the controlled branches still sit right
/// after the voltage-source branches, so [`Layout::controlled_base`] and
/// [`Layout::sense_branch`] give the same indices here.
fn stamp_controlled_sources_complex(
    sys: &mut ComplexSystem,
    circuit: &Circuit,
    layout: &Layout,
) -> Result<(), SimError> {
    let base = layout.controlled_base();
    let node = |net| layout.node_of(circuit, net);
    let re = |x: f64| Complex::new(x, 0.0);

    for (e, &pi) in layout.vcvs.iter().enumerate() {
        let part = &circuit.parts()[pi];
        let PartKind::Vcvs(gain) = part.kind else {
            unreachable!("layout.vcvs holds only VCVS parts")
        };
        let [outp, outm, cp, cm] = terminals4(part)?;
        let k = base + e;
        sys.stamp_vsource(node(outp), node(outm), k, Complex::ZERO);
        sys.stamp_matrix(Some(k), node(cp), re(-gain));
        sys.stamp_matrix(Some(k), node(cm), re(gain));
    }

    for (c, &pi) in layout.ccvs.iter().enumerate() {
        let part = &circuit.parts()[pi];
        let PartKind::Ccvs(gain, ctrl) = &part.kind else {
            unreachable!("layout.ccvs holds only CCVS parts")
        };
        let (outp, outm) = terminals2(part)?;
        let k = base + layout.vcvs.len() + c;
        let kc = layout
            .sense_branch(circuit, ctrl)
            .ok_or_else(|| SimError::BadControl(ctrl.clone()))?;
        sys.stamp_vsource(node(outp), node(outm), k, Complex::ZERO);
        sys.stamp_matrix(Some(k), Some(kc), re(-gain));
    }

    for part in circuit.parts() {
        match &part.kind {
            PartKind::Vccs(g) => {
                let [outp, outm, cp, cm] = terminals4(part)?;
                sys.stamp_matrix(node(outp), node(cp), re(*g));
                sys.stamp_matrix(node(outp), node(cm), re(-*g));
                sys.stamp_matrix(node(outm), node(cp), re(-*g));
                sys.stamp_matrix(node(outm), node(cm), re(*g));
            }
            PartKind::Cccs(f, ctrl) => {
                let (outp, outm) = terminals2(part)?;
                let kc = layout
                    .sense_branch(circuit, ctrl)
                    .ok_or_else(|| SimError::BadControl(ctrl.clone()))?;
                sys.stamp_matrix(node(outp), Some(kc), re(*f));
                sys.stamp_matrix(node(outm), Some(kc), re(-*f));
            }
            _ => {}
        }
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

/// The contract an N-terminal nonlinear device fulfils for the Newton–Raphson
/// loop. Each iteration the solver reads the device's terminal voltages at the
/// running guess and asks it to [`linearize`](NonlinearDevice::linearize): report
/// the current flowing into every terminal and the Jacobian `∂I_i/∂V_j` of those
/// currents, bundled with the RHS equivalent-current sources as a [`Companion`]
/// that the engine writes into the MNA system. A diode is the
/// two-terminal case; a BJT and a MOSFET stamp a 3×3 block. (`CP-SIM-M1.2`.)
///
/// This trait is the **public plugin contract** (`CP-SIM-M4.1`): an external
/// crate can implement it for its own device and register a factory with
/// [`Engine::with_device`] / [`DeviceRegistry`], after which [`Engine::run`]
/// stamps that device in every analysis — operating point, transient, DC/AC —
/// exactly like a built-in. The built-in `Diode`, `Bjt`, and `Mosfet` are
/// ordinary implementors of it.
pub trait NonlinearDevice {
    /// The device's terminal nets, in the order [`linearize`](Self::linearize)
    /// reads voltages and reports currents. The internal controlling/branch
    /// voltages are formed from these inside the model.
    fn terminals(&self) -> &[NetId];

    /// Length of the limiting state the solver carries for this device across
    /// iterations — its previously limited controlling voltages. `0` for a model
    /// that never limits (the square-law MOSFET).
    fn state_len(&self) -> usize;

    /// Linearize about the terminal voltages `v` (volts, one per terminal in
    /// [`terminals`](Self::terminals) order, ground = 0 V), returning the
    /// [`Companion`] to stamp.
    ///
    /// When `limit` is set the model may damp the Newton step through its
    /// junctions, reading and updating `state` (its previously limited
    /// controlling voltages); AC analysis passes `limit = false` to linearize
    /// exactly at the converged bias.
    fn linearize(&self, v: &[f64], state: &mut [f64], limit: bool) -> Companion;
}

/// A nonlinear device's linearized multiport companion — the output of
/// [`NonlinearDevice::linearize`]. Every vector is in the device's terminal order.
/// Public so external device models (`CP-SIM-M4.1`) can construct one; the fields
/// are the same multiport Norton equivalent the built-in devices return.
pub struct Companion {
    /// Terminal currents `I_i` (the current flowing *from node `i` into the
    /// device*) at the linearization point — the solver's current-convergence
    /// probe.
    pub i: Vec<f64>,
    /// RHS equivalent-current sources: `i_eq[a]` is injected into node `a`'s row,
    /// equal to `Σ_j g[a][j]·V*_j − I_a` at the (possibly limited) linearization
    /// point. The model precomputes it so junction limiting stays internal and
    /// the stamped companion is self-consistent — the multiport generalization of
    /// the two-terminal Norton source `I_eq = I(v) − g·v`.
    pub i_eq: Vec<f64>,
    /// Jacobian `g[i][j] = ∂I_i/∂V_j`, a dense `n × n` block in terminal order
    /// (possibly asymmetric — a transconductance is not reciprocal).
    pub g: Vec<Vec<f64>>,
}

/// A factory that builds a plugin [`NonlinearDevice`] from the [`Part`] that
/// carries it — the value stored in a [`DeviceRegistry`]. Shared behind an `Arc`
/// so an [`Engine`] stays cheap to clone and `Send + Sync` (required for the
/// native `parallel` feature). The factory reads the part's terminals, value, and
/// any model data, and returns the boxed device (or a [`SimError`] if the part is
/// malformed — e.g. wrong terminal count).
pub type DeviceFactory =
    std::sync::Arc<dyn Fn(&Part) -> Result<Box<dyn NonlinearDevice>, SimError> + Send + Sync>;

/// A registry of plugin nonlinear-device models, keyed by the name a part carries
/// in [`PartKind::Other`] (`CP-SIM-M4.1`). The engine consults it when it meets an
/// `Other(name)` part: a registered factory builds the device, which then stamps
/// in Newton–Raphson / operating-point / transient / AC exactly like a built-in;
/// an unregistered name is still rejected as [`SimError::Unsupported`]. Cloning is
/// cheap — the factories are shared behind `Arc`.
#[derive(Default, Clone)]
pub struct DeviceRegistry {
    factories: BTreeMap<String, DeviceFactory>,
}

impl DeviceRegistry {
    /// An empty registry — built-in devices only.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `factory` under `name`; a part of kind [`PartKind::Other`] carrying
    /// that name is then built and stamped by it. Re-registering a name replaces it.
    pub fn register<F>(&mut self, name: impl Into<String>, factory: F)
    where
        F: Fn(&Part) -> Result<Box<dyn NonlinearDevice>, SimError> + Send + Sync + 'static,
    {
        self.factories
            .insert(name.into(), std::sync::Arc::new(factory));
    }

    /// The factory registered for `name`, if any.
    fn get(&self, name: &str) -> Option<&DeviceFactory> {
        self.factories.get(name)
    }

    /// Whether a factory is registered for `name`.
    fn contains(&self, name: &str) -> bool {
        self.factories.contains_key(name)
    }
}

impl fmt::Debug for DeviceRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceRegistry")
            .field("devices", &self.factories.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Stamp an N-terminal device's linearized [`Companion`] into the system: the
/// Jacobian block as the (possibly asymmetric) conductance matrix, and each
/// equivalent-current source onto its node's RHS row. `nodes[i]` is terminal
/// `i`'s MNA index (`None` = ground, dropped). This is the two-terminal Norton
/// stamp generalized to a port matrix.
fn stamp_companion(sys: &mut System, nodes: &[Option<usize>], c: &Companion) {
    for (a, &node_a) in nodes.iter().enumerate() {
        // `stamp_current(node_a, None, ·)` is a bare RHS add into node a.
        sys.stamp_current(node_a, None, c.i_eq[a]);
        for (b, &node_b) in nodes.iter().enumerate() {
            sys.stamp_matrix(node_a, node_b, c.g[a][b]);
        }
    }
}

/// SPICE pn-junction limiting (`pnjlim`). Given the raw Newton voltage `v_new`
/// across a junction and the previous iteration's limited value `v_old`, return a
/// limited voltage so `exp(v/vte)` cannot run away between iterations: past the
/// critical voltage a step beyond a thermal voltage `vte` is mapped through a
/// logarithm. Reverse bias and sub-thermal steps pass through untouched. Shared
/// by the diode and both BJT junctions. `vte` is the junction's emission-scaled
/// thermal voltage, `is` its saturation current.
fn pnjlim(v_new: f64, v_old: f64, vte: f64, is: f64) -> f64 {
    let vcrit = vte * (vte / (std::f64::consts::SQRT_2 * is)).ln();
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

/// The junction thermal voltage `V_t = kT/q` (V) at the nominal temperature — the
/// fixed solver constant a diode's emission coefficient scales to `n·V_t`. (It is a
/// constant rather than a [`DiodeModel`] field because the model exposes only the
/// SPICE `.model` knobs `I_s`/`n`/`R_s`.)
const DIODE_VT: f64 = 0.025_852;

/// Local-Newton iteration cap for resolving a diode's internal junction voltage
/// when it carries a series resistance `R_s > 0` (`CP-SIM-M5.2`). A handful of
/// steps converges the scalar `V = V_j + R_s·I(V_j)` from a warm start; the cap is
/// only a guard against a pathological start.
const DIODE_RS_ITERS: usize = 50;

/// A junction diode. Terminals `[anode, cathode]`; the branch voltage is
/// `v(anode) − v(cathode)` and forward current flows anode → cathode. Carries its
/// [`DiodeModel`] (`I_s`, `n`, `R_s`); the thermal voltage is [`DIODE_VT`].
struct Diode {
    nets: [NetId; 2],
    model: DiodeModel,
}

impl NonlinearDevice for Diode {
    fn terminals(&self) -> &[NetId] {
        &self.nets
    }

    fn state_len(&self) -> usize {
        1 // the single (junction) voltage carried for limiting / warm-starting
    }

    fn linearize(&self, v: &[f64], state: &mut [f64], limit: bool) -> Companion {
        let is = self.model.is;
        let vte = self.model.n * DIODE_VT; // emission-scaled thermal voltage, n·Vt
        let v_term = v[0] - v[1];

        let (i, g, i_eq) = if self.model.rs == 0.0 {
            // Ideal junction — the exact pre-model path, bit-for-bit. Shockley:
            // I = Is·(exp(V/(n·Vt)) − 1), g = dI/dV = (Is/(n·Vt))·exp(V/(n·Vt)).
            let mut vd = v_term;
            if limit {
                vd = pnjlim(vd, state[0], vte, is);
                state[0] = vd;
            }
            let ex = (vd / vte).exp();
            let i = is * (ex - 1.0);
            let g = (is / vte) * ex;
            (i, g, g * vd - i)
        } else {
            // Series resistance: the terminal voltage splits across the ohmic R_s
            // and the junction, `V = V_j + R_s·I(V_j)`. Resolve the internal V_j for
            // the present terminal voltage by a damped local Newton (warm-started
            // from the stored V_j), then present the composite's small-signal
            // conductance g_eff = g_j/(1 + R_s·g_j) — the series combination of the
            // junction conductance with R_s — as a single two-terminal companion, so
            // no internal MNA node is needed.
            let rs = self.model.rs;
            let mut vj = state[0]; // previous junction voltage (0 on a cold start)
            for _ in 0..DIODE_RS_ITERS {
                let ex = (vj / vte).exp();
                let ij = is * (ex - 1.0);
                let gj = (is / vte) * ex;
                // f(Vj) = Vj + Rs·I(Vj) − V_term, f'(Vj) = 1 + Rs·gj.
                let f = vj + rs * ij - v_term;
                let step = f / (1.0 + rs * gj);
                let vj_next = pnjlim(vj - step, vj, vte, is); // damp the junction step
                let done = (vj_next - vj).abs() < 1e-14 * (1.0 + vj_next.abs());
                vj = vj_next;
                if done {
                    break;
                }
            }
            if limit {
                state[0] = vj;
            }
            let ex = (vj / vte).exp();
            let i = is * (ex - 1.0);
            let gj = (is / vte) * ex;
            let g = gj / (1.0 + rs * gj); // composite (junction + Rs) conductance
                                          // Norton source referenced to the terminal voltage: i_eq = g·V_term − I.
            (i, g, g * v_term - i)
        };
        // Norton equivalent current into the anode; the cathode mirrors it.
        Companion {
            i: vec![i, -i],
            i_eq: vec![i_eq, -i_eq],
            g: vec![vec![g, -g], vec![-g, g]],
        }
    }
}

/// Ebers–Moll bipolar transistor. Terminals `[collector, base, emitter]`; the
/// [`Polarity`] folds PNP onto the NPN equations by flipping every controlling
/// voltage and terminal current.
struct Bjt {
    nets: [NetId; 3],
    polarity: Polarity,
    model: BjtModel,
}

impl NonlinearDevice for Bjt {
    fn terminals(&self) -> &[NetId] {
        &self.nets
    }

    fn state_len(&self) -> usize {
        2 // the internal (Vbe, Vbc) junction voltages
    }

    fn linearize(&self, v: &[f64], state: &mut [f64], limit: bool) -> Companion {
        let BjtModel { is, bf, br, vt } = self.model;
        let s = self.polarity.sign();
        let (vc, vb, ve) = (v[0], v[1], v[2]);
        // Internal NPN-oriented junction voltages; polarity folds PNP onto NPN.
        let mut vbe = s * (vb - ve);
        let mut vbc = s * (vb - vc);
        if limit {
            vbe = pnjlim(vbe, state[0], vt, is);
            vbc = pnjlim(vbc, state[1], vt, is);
            state[0] = vbe;
            state[1] = vbc;
        }
        let (evbe, evbc) = ((vbe / vt).exp(), (vbc / vt).exp());
        // Ebers–Moll transport form: two junction diodes plus the transport source.
        let ibe = is / bf * (evbe - 1.0); // base–emitter diode (base-current share)
        let ibc = is / br * (evbc - 1.0); // base–collector diode (base-current share)
        let it = is * (evbe - evbc); // transport current, collector → emitter
                                     // Internal (NPN) terminal currents into [C, B, E] — they sum to zero.
        let (ic0, ib0, ie0) = (it - ibc, ibe + ibc, -it - ibe);
        // Junction/transport (trans)conductances at this bias.
        let gpi = is / (bf * vt) * evbe; //  ∂ibe/∂Vbe
        let gmu = is / (br * vt) * evbc; //  ∂ibc/∂Vbc
        let gif = is / vt * evbe; //  ∂it/∂Vbe
        let gir = is / vt * evbc; // −∂it/∂Vbc
                                  // Each internal current's partials wrt the internal (Vbe, Vbc).
        let (c_be, c_bc) = (gif, -gir - gmu);
        let (b_be, b_bc) = (gpi, gmu);
        let (e_be, e_bc) = (-gif - gpi, gir);
        // Jacobian in node coordinates (vc, vb, ve). Because Vbe/Vbc each carry
        // the polarity factor and so does the current, `s²` cancels and the block
        // is polarity-independent.
        let g = vec![
            vec![gir + gmu, gif - gir - gmu, -gif], // ∂Ic/∂(vc, vb, ve)
            vec![-gmu, gpi + gmu, -gpi],            // ∂Ib/∂(vc, vb, ve)
            vec![-gir, gir - gif - gpi, gif + gpi], // ∂Ie/∂(vc, vb, ve)
        ];
        // Terminal currents carry the polarity sign; so do the Norton sources,
        // formed from the internal partials at the limited (Vbe, Vbc).
        let i = vec![s * ic0, s * ib0, s * ie0];
        let i_eq = vec![
            s * (c_be * vbe + c_bc * vbc - ic0),
            s * (b_be * vbe + b_bc * vbc - ib0),
            s * (e_be * vbe + e_bc * vbc - ie0),
        ];
        Companion { i, i_eq, g }
    }
}

/// Level-1 (square-law) MOSFET with the body tied to the source. Terminals
/// `[drain, gate, source]`; the [`Polarity`] folds PMOS onto the NMOS equations.
struct Mosfet {
    nets: [NetId; 3],
    polarity: Polarity,
    model: MosfetModel,
}

impl NonlinearDevice for Mosfet {
    fn terminals(&self) -> &[NetId] {
        &self.nets
    }

    fn state_len(&self) -> usize {
        0 // square law — no junction exp, so no limiting needed
    }

    fn linearize(&self, v: &[f64], _state: &mut [f64], _limit: bool) -> Companion {
        let s = self.polarity.sign();
        // Internal n-channel orientation; `s²` cancels in the Jacobian so `mos_core`
        // already returns the node-coordinate block.
        let (i0, g) = mos_core(s * v[0], s * v[1], s * v[2], &self.model);
        let i = vec![s * i0[0], s * i0[1], s * i0[2]];
        let g: Vec<Vec<f64>> = g.iter().map(|row| row.to_vec()).collect();
        // No limiting: the linearization point is the raw node voltages, so the
        // Norton source follows straight from the Jacobian.
        let i_eq = (0..3)
            .map(|a| (0..3).map(|b| g[a][b] * v[b]).sum::<f64>() - i[a])
            .collect();
        Companion { i, i_eq, g }
    }
}

/// Level-1 NMOS channel in internal (n-channel) coordinates: terminal currents
/// into `[drain, gate, source]` and the 3×3 Jacobian `∂I_i/∂V_j`. The channel is
/// symmetric about drain/source, so whichever terminal sits higher acts as the
/// drain (keeping `Vds ≥ 0`); the gate draws no DC current.
fn mos_core(vd: f64, vg: f64, vs: f64, m: &MosfetModel) -> ([f64; 3], [[f64; 3]; 3]) {
    if vd >= vs {
        let (id, gm, gds) = mos_chan(vg - vs, vd - vs, m);
        (
            [id, 0.0, -id],
            [
                [gds, gm, -(gm + gds)],
                [0.0, 0.0, 0.0],
                [-gds, -gm, gm + gds],
            ],
        )
    } else {
        // Source and drain swap roles: the physical source is the channel drain.
        let (id, gm, gds) = mos_chan(vg - vd, vs - vd, m);
        (
            [-id, 0.0, id],
            [
                [gm + gds, -gm, -gds],
                [0.0, 0.0, 0.0],
                [-(gm + gds), gm, gds],
            ],
        )
    }
}

/// The channel current `Id` (drain → source, `Vds ≥ 0`) and its derivatives
/// `gm = ∂Id/∂Vgs`, `gds = ∂Id/∂Vds` across the three level-1 regions — cutoff,
/// triode, saturation — with channel-length modulation `(1 + λ·Vds)`. `kp` is
/// `K = ½·µ·Cox·W/L`.
fn mos_chan(vgs: f64, vds: f64, m: &MosfetModel) -> (f64, f64, f64) {
    let vov = vgs - m.vth; // gate overdrive
    if vov <= 0.0 {
        return (0.0, 0.0, 0.0); // cutoff
    }
    let clm = 1.0 + m.lambda * vds;
    if vds < vov {
        // Triode.
        let id = m.kp * (2.0 * vov - vds) * vds * clm;
        let gm = 2.0 * m.kp * vds * clm;
        let gds = m.kp * ((2.0 * vov - 2.0 * vds) * clm + (2.0 * vov - vds) * vds * m.lambda);
        (id, gm, gds)
    } else {
        // Saturation.
        let id = m.kp * vov * vov * clm;
        let gm = 2.0 * m.kp * vov * clm;
        let gds = m.kp * vov * vov * m.lambda;
        (id, gm, gds)
    }
}

/// Collect every nonlinear device in the circuit as a boxed [`NonlinearDevice`].
/// This is the one place a new device kind is wired into the solver: the built-in
/// diode / BJT / MOSFET, plus any [`PartKind::Other`] part whose name has a plugin
/// factory in `registry` (`CP-SIM-M4.1`). An `Other` part with no registered
/// factory is skipped here — [`validate`] has already rejected it, so this stays a
/// pure collector.
fn collect_nonlinear(
    circuit: &Circuit,
    registry: &DeviceRegistry,
) -> Result<Vec<Box<dyn NonlinearDevice>>, SimError> {
    let mut devices: Vec<Box<dyn NonlinearDevice>> = Vec::new();
    for part in circuit.parts() {
        match &part.kind {
            PartKind::Diode(model) => {
                let (a, b) = terminals2(part)?;
                devices.push(Box::new(Diode {
                    nets: [a, b],
                    model: *model,
                }));
            }
            PartKind::Bjt(polarity, model) => {
                devices.push(Box::new(Bjt {
                    nets: terminals3(part)?,
                    polarity: *polarity,
                    model: *model,
                }));
            }
            PartKind::Mosfet(polarity, model) => {
                devices.push(Box::new(Mosfet {
                    nets: terminals3(part)?,
                    polarity: *polarity,
                    model: *model,
                }));
            }
            PartKind::Other(name) => {
                if let Some(factory) = registry.get(name) {
                    devices.push(factory(part)?);
                }
            }
            _ => {}
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
///
/// `x_init` is the starting guess (`None` = every node at 0 V); the convergence
/// aids ([`gmin_stepping`] / [`source_stepping`]) warm-start each continuation
/// step from the previous one. `gmin` adds a conductance from every node to
/// ground on every iteration — the knob gmin stepping ramps from large → 0; pass
/// `0.0` for the unaided solve. `solver` carries the linear-solve strategy and its
/// reusable ordered factorization across the iterations (`CP-SIM-M5.1`).
#[allow(clippy::too_many_arguments)] // one cohesive Newton solve: system + aids + solver
fn solve_nr(
    circuit: &Circuit,
    layout: &Layout,
    size: usize,
    devices: &[Box<dyn NonlinearDevice>],
    stamp_linear: &dyn Fn(&mut System) -> Result<(), SimError>,
    x_init: Option<&[f64]>,
    gmin: f64,
    solver: &mut ReusableSolver,
) -> Result<Vec<f64>, SimError> {
    // Purely linear: one stamp-and-solve is exact (and identical to M0). A linear
    // circuit always solves in one shot, so the convergence aids never reach here.
    // The `solver` routes the large-net solve through the ordered KLU path (and,
    // across a transient's steps, reuses its symbolic factorization); below
    // `KLU_THRESHOLD` it is a bit-for-bit pass-through, so small circuits are exact.
    if devices.is_empty() {
        let mut sys = System::new(size);
        stamp_linear(&mut sys)?;
        return solver.solve(sys).ok_or(SimError::Singular);
    }

    // Initial guess: the warm start if given, else every node at 0 V.
    let mut x = match x_init {
        Some(x0) => x0.to_vec(),
        None => vec![0.0; size],
    };
    // Per-device limiting memory (previous limited controlling voltages) and the
    // previous iteration's terminal currents, for the current-convergence test.
    let mut states: Vec<Vec<f64>> = devices.iter().map(|d| vec![0.0; d.state_len()]).collect();
    let mut i_prev: Vec<Vec<f64>> = devices
        .iter()
        .map(|d| vec![0.0; d.terminals().len()])
        .collect();

    for _ in 0..NR_MAX_ITERS {
        let mut sys = System::new(size);
        stamp_linear(&mut sys)?;
        // gmin stepping: a tiny conductance from every node to ground props up an
        // otherwise ill-conditioned Jacobian; it is ramped to 0 across outer steps.
        if gmin > 0.0 {
            for i in 0..layout.n_nodes() {
                sys.stamp_conductance(Some(i), None, gmin);
            }
        }

        let mut i_now: Vec<Vec<f64>> = Vec::with_capacity(devices.len());
        for (k, dev) in devices.iter().enumerate() {
            let nodes: Vec<Option<usize>> = dev
                .terminals()
                .iter()
                .map(|&t| layout.node_of(circuit, t))
                .collect();
            let v: Vec<f64> = nodes.iter().map(|&nd| nd.map_or(0.0, |i| x[i])).collect();
            // Linearize about the running guess (with junction limiting) and stamp
            // the device's multiport Norton companion.
            let comp = dev.linearize(&v, &mut states[k], true);
            stamp_companion(&mut sys, &nodes, &comp);
            i_now.push(comp.i);
        }

        let x_new = solver.solve(sys).ok_or(SimError::Singular)?;
        // A diverging solution (overflow → inf/NaN) must never read as converged.
        // `f64::max` ignores NaN, so test finiteness explicitly before the tol
        // check rather than letting a NaN step fold to a spurious zero.
        let finite =
            x_new.iter().all(|v| v.is_finite()) && i_now.iter().flatten().all(|v| v.is_finite());
        let dv = x_new
            .iter()
            .zip(&x)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f64::max);
        let di = i_now
            .iter()
            .flatten()
            .zip(i_prev.iter().flatten())
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

/// Number of continuation steps in gmin and source stepping: the aid ramps its
/// knob across this many solves, warm-starting each from the last.
const STEPPING_STEPS: usize = 16;
/// Largest node-to-ground conductance gmin stepping starts from (1 kΩ to ground
/// on every node — enough to tame almost any ill-conditioned Jacobian).
const GMIN_START: f64 = 1e-3;
/// Smallest non-zero gmin before the final gmin = 0 solve on the real circuit.
const GMIN_FLOOR: f64 = 1e-12;
/// Residual SPICE-style GMIN: a tiny node-to-ground conductance stamped into the DC
/// bias only as a *fallback*, when the plain + aided ladder leaves the system
/// singular because a node floats at DC (a reactance-only node — e.g. a capacitive
/// divider's mid-node — that no resistive path pins). It conditions that node so the
/// bias returns a gmin-pinned point instead of erroring [`SimError::Singular`]. It is
/// never present on a circuit that already solves, so it cannot move a golden result.
const GMIN_RESIDUAL: f64 = 1e-12;

/// Solve a (possibly nonlinear) DC-like system with the full convergence-aid
/// ladder, shared by the DC operating point and the transient's `t = 0` initial
/// solve so both are equally robust.
///
/// `stamp_at(sys, alpha)` stamps every linear contribution with the independent
/// sources scaled by `alpha` (`1.0` = the real circuit); source stepping ramps
/// `alpha` 0 → 1.
///
/// Tries plain NR first (the fast path — a plain-convergent or purely linear
/// circuit takes exactly this and nothing more). Only if NR fails — hitting its
/// iteration cap, *or* returning a singular Jacobian — does it fall back to the
/// convergence aids: gmin stepping, then source stepping. A singular plain solve
/// is routed in too, because an isolated node at the cold-start guess (a
/// cutoff-at-0 V MOSFET channel, say) is exactly what gmin stepping props up and
/// resolves; a node that stays floating at the real circuit (gmin's final step at
/// gmin = 0) still surfaces as [`SimError::Singular`], and only a genuinely
/// hopeless nonlinear solve surfaces as [`SimError::NonConvergence`].
fn solve_aided(
    circuit: &Circuit,
    layout: &Layout,
    size: usize,
    devices: &[Box<dyn NonlinearDevice>],
    stamp_at: &dyn Fn(&mut System, f64) -> Result<(), SimError>,
    solver: &mut ReusableSolver,
) -> Result<Vec<f64>, SimError> {
    // 1. Plain Newton–Raphson, full sources, no gmin, cold start. This is the only
    // path that reuses the caller's `solver` — its structure (full sources, gmin 0)
    // is the one a transient repeats step after step, so the symbolic factorization
    // carries over. The gmin / source-stepping fallbacks below perturb the structure
    // (gmin adds node-to-ground stamps), so each gets its own throwaway solver.
    match solve_nr(
        circuit,
        layout,
        size,
        devices,
        &|sys| stamp_at(sys, 1.0),
        None,
        0.0,
        solver,
    ) {
        Ok(x) => return Ok(x),
        // Hit the iteration cap or a singular guess — the aids may still get there.
        Err(SimError::NonConvergence(_)) | Err(SimError::Singular) => {}
        Err(e) => return Err(e), // Bad*/Missing* are data faults no aid can cure
    }

    // 2. gmin stepping, then 3. source stepping. Each returns `Ok(None)` when a
    // continuation step fails to converge; a real fault (still singular at the
    // final gmin = 0 — a truly floating node) propagates straight out.
    if let Some(x) = gmin_stepping(circuit, layout, size, devices, stamp_at)? {
        return Ok(x);
    }
    if let Some(x) = source_stepping(circuit, layout, size, devices, stamp_at)? {
        return Ok(x);
    }
    Err(SimError::NonConvergence(NR_MAX_ITERS))
}

/// Solve the DC operating point: resistors + sources, capacitors open, inductors
/// shorted, and any nonlinear device driven to steady state by Newton–Raphson
/// with the shared convergence-aid ladder ([`solve_aided`]).
fn solve_dc(
    circuit: &Circuit,
    layout: &Layout,
    registry: &DeviceRegistry,
) -> Result<Vec<f64>, SimError> {
    solve_dc_conditioned(circuit, layout, registry, 0.0)
}

/// [`solve_dc`] with a residual node-to-ground conductance `gmin` added to every
/// bias stamp. `gmin = 0.0` is the plain operating point (what [`solve_dc`] runs, so
/// every existing caller is unchanged to the bit). A small positive `gmin` — the
/// [`GMIN_RESIDUAL`] fallback [`dc_sweep`] takes on a singular point — conditions a
/// node that floats at DC so the solve returns a gmin-pinned point instead of
/// erroring [`SimError::Singular`].
fn solve_dc_conditioned(
    circuit: &Circuit,
    layout: &Layout,
    registry: &DeviceRegistry,
    gmin: f64,
) -> Result<Vec<f64>, SimError> {
    let size = layout.n_nodes()
        + layout.vsources.len()
        + layout.n_controlled_branches()
        + layout.inductors.len();
    let devices = collect_nonlinear(circuit, registry)?;
    // The linear stamp at source scale `alpha` (1.0 = the real circuit).
    let stamp_at = |sys: &mut System, alpha: f64| -> Result<(), SimError> {
        stamp_resistors(sys, circuit, layout)?;
        // Operating point: sources at their bias (`None`), scaled for source stepping.
        stamp_vsources(sys, circuit, layout, None, alpha)?;
        stamp_current_sources(sys, circuit, layout, None, alpha)?;
        stamp_controlled_sources(sys, circuit, layout)?;
        stamp_inductor_shorts(sys, circuit, layout)?;
        // Residual GMIN (fallback only): a tiny conductance from every node to ground,
        // baked into the stamp so it survives even the linear one-shot path and gmin
        // stepping's final gmin = 0 step. Left out entirely when `gmin == 0.0`.
        if gmin > 0.0 {
            for i in 0..layout.n_nodes() {
                sys.stamp_conductance(Some(i), None, gmin);
            }
        }
        Ok(())
    };
    // A one-shot operating point: its own solver reuses the symbolic factorization
    // across this solve's Newton iterations (and, on a large net, takes the ordered
    // KLU path); a sweep/Monte-Carlo point owns one per point, so parallel fan-out
    // stays thread-local.
    let mut solver = ReusableSolver::new(size);
    solve_aided(circuit, layout, size, &devices, &stamp_at, &mut solver)
}

/// The DC bias a small-signal analysis (AC / noise / `.TF`) linearizes about. A
/// circuit with **no** nonlinear device has no operating-point dependence — its
/// small-signal stamps (`G`, `jωC`, `1/jωL`) are constant — so it needs no bias at
/// all, and we skip the DC solve and return a zero vector. This is what unblocks a
/// node that floats *only* at DC: a capacitive divider's mid-node, an AC-coupled
/// subnode, or a source-matched open-ended line all have a perfectly well-conditioned
/// small-signal matrix, yet the DC gate used to reject them as [`SimError::Singular`].
/// A genuinely singular small-signal system (no AC path to ground) still surfaces at
/// the complex solve. The returned vector is never indexed on this path (the
/// device-linearization loops are empty), and for a circuit that *does* carry
/// nonlinear devices the real operating point is solved and returned exactly as
/// before — so every previously-solving analysis is bit-for-bit unchanged.
fn small_signal_bias(
    circuit: &Circuit,
    layout: &Layout,
    registry: &DeviceRegistry,
) -> Result<Vec<f64>, SimError> {
    if collect_nonlinear(circuit, registry)?.is_empty() {
        let size = layout.n_nodes()
            + layout.vsources.len()
            + layout.n_controlled_branches()
            + layout.inductors.len();
        Ok(vec![0.0; size])
    } else {
        solve_dc(circuit, layout, registry)
    }
}

/// gmin stepping: solve with a large node-to-ground conductance first (which
/// makes the Jacobian diagonally dominant and trivially solvable), then ramp that
/// conductance geometrically down to zero, warm-starting each solve from the last.
/// The final solve sits at gmin = 0 — the true circuit — so the returned point
/// carries no residual-conductance error. `Ok(None)` means a continuation step
/// failed to converge (the caller then tries source stepping); a real error (a
/// singular system) propagates.
fn gmin_stepping(
    circuit: &Circuit,
    layout: &Layout,
    size: usize,
    devices: &[Box<dyn NonlinearDevice>],
    stamp_at: &dyn Fn(&mut System, f64) -> Result<(), SimError>,
) -> Result<Option<Vec<f64>>, SimError> {
    let mut x = vec![0.0; size];
    let ratio = GMIN_FLOOR / GMIN_START; // < 1
    for k in 0..=STEPPING_STEPS {
        let gmin = if k == STEPPING_STEPS {
            0.0 // the last step is the real circuit
        } else {
            GMIN_START * ratio.powf(k as f64 / STEPPING_STEPS as f64)
        };
        // Each continuation step's gmin changes the stamped structure, so it gets a
        // fresh solver (reuse holds only within its own Newton iterations).
        let mut solver = ReusableSolver::new(size);
        match solve_nr(
            circuit,
            layout,
            size,
            devices,
            &|sys| stamp_at(sys, 1.0),
            Some(&x),
            gmin,
            &mut solver,
        ) {
            Ok(sol) => x = sol,
            Err(SimError::NonConvergence(_)) => return Ok(None),
            Err(e) => return Err(e),
        }
    }
    Ok(Some(x))
}

/// Source stepping: ramp every independent source from 0 → 100% of its value
/// across the continuation, warm-starting each solve from the last. At 0% the
/// circuit is dead and the solution is the origin; each small increment moves the
/// operating point only a little, keeping every Newton solve inside its basin of
/// attraction. `Ok(None)` means a step failed to converge; a real error propagates.
fn source_stepping(
    circuit: &Circuit,
    layout: &Layout,
    size: usize,
    devices: &[Box<dyn NonlinearDevice>],
    stamp_at: &dyn Fn(&mut System, f64) -> Result<(), SimError>,
) -> Result<Option<Vec<f64>>, SimError> {
    let mut x = vec![0.0; size];
    for k in 0..=STEPPING_STEPS {
        let alpha = k as f64 / STEPPING_STEPS as f64; // 0 → 1
                                                      // Source scale changes only right-hand-side numbers, not structure, but a
                                                      // fresh per-step solver keeps the stepping path independent of the plain one.
        let mut solver = ReusableSolver::new(size);
        match solve_nr(
            circuit,
            layout,
            size,
            devices,
            &|sys| stamp_at(sys, alpha),
            Some(&x),
            0.0,
            &mut solver,
        ) {
            Ok(sol) => x = sol,
            Err(SimError::NonConvergence(_)) => return Ok(None),
            Err(e) => return Err(e),
        }
    }
    Ok(Some(x))
}

/// Voltage across a two-terminal device (`v(plus) - v(minus)`), ground = 0 V.
fn across(x: &[f64], plus: Option<usize>, minus: Option<usize>) -> f64 {
    plus.map_or(0.0, |i| x[i]) - minus.map_or(0.0, |i| x[i])
}

/// Backward-Euler timestep bounds and local-truncation-error controls for the
/// adaptive transient (`CP-SIM-M1.6`). The error estimate is a step-doubling
/// (Richardson) comparison of one step of `h` against two of `h/2`; since
/// Backward Euler is first order, that difference *is* the finer solution's LTE.
/// A step is accepted when the normalized error is ≤ 1 and the next step is
/// `safety·err^(−1/2)` larger (the first-order exponent), clamped so it never
/// grows or shrinks too violently.
const LTE_RELTOL: f64 = 1e-4;
const LTE_ABSTOL: f64 = 1e-6;
const LTE_SAFETY: f64 = 0.9;
const LTE_GROW_MAX: f64 = 5.0;
const LTE_SHRINK_MIN: f64 = 0.2;
/// The adaptive step is never larger than `duration / MIN_ADAPTIVE_STEPS` (so
/// even a lazy waveform is sampled) nor smaller than `duration · H_MIN_FRAC`
/// (the floor at which a step is force-accepted to guarantee progress).
const MIN_ADAPTIVE_STEPS: f64 = 20.0;
const H_MIN_FRAC: f64 = 1e-6;
/// A safety net on the adaptive driver's accept/reject iterations — far above any
/// real run, it only trips on a pathological limit cycle.
const ADAPTIVE_ITER_CAP: usize = 5_000_000;

/// A transient assembled once: the reactive devices (capacitors, inductors), the
/// nonlinear devices, and the MNA dimensions. Both the fixed-step and the
/// adaptive drivers advance it through the identical Backward-Euler [`step`].
///
/// Capacitors discretize into a companion model — a conductance `C/h` in parallel
/// with a current source `(C/h)·v_prev`, refreshed each step. Inductors give
/// `i_n = i_prev + (h/L)·(v_a − v_b)`: a conductance `g_eq = h/L` in parallel with
/// a history source `i_prev` oriented `a → b`, with `i_prev` refreshed from the
/// solved node voltages. At `t = 0` capacitors are held at their initial voltage
/// (a 0 V source) and inductors are open (`i(0) = 0`), per [`initial`](Self::initial).
/// One Backward-Euler step's outcome — the node solution and the refreshed
/// reactive and transmission-line state the next step reads.
struct StepOut {
    /// The node (and branch) solution at the step's end time.
    x: Vec<f64>,
    /// Refreshed capacitor voltages.
    caps: Vec<f64>,
    /// Refreshed inductor currents.
    inds: Vec<f64>,
    /// Refreshed lossless-line port samples `[v1, i1, v2, i2]` (lossless-line
    /// order); empty when the circuit has no lossless lines. (`CP-SIM-M7.2`.)
    lossless: Vec<[f64; 4]>,
    /// Refreshed lossy-line companion state `[series_i, cap_a_v, cap_b_v]`
    /// (lossy-line order); empty when the circuit has no lossy lines.
    lossy: Vec<[f64; 3]>,
    /// Refreshed capacitor branch currents `i_C` — the extra history the trapezoidal
    /// companion carries (`CP-SIM-M10.2`). **Empty under Backward Euler**, which needs
    /// no current history, so the default path threads nothing and stays bit-for-bit.
    cap_i: Vec<f64>,
    /// Refreshed isolated-inductor branch voltages `v_L` — the trapezoidal companion's
    /// extra history. Empty under Backward Euler.
    ind_v: Vec<f64>,
}

// ---------------------------------------------------------------------------
// Mutual inductance / coupled inductors (CP-SIM-M6.1)
// ---------------------------------------------------------------------------

/// A group of magnetically-coupled inductors and the dense inductance matrix
/// (henries) relating their branch voltages and currents: `lmat[i][i]` is member
/// `i`'s self-inductance `L`, and `lmat[i][j]` (`i ≠ j`) the mutual inductance
/// `M = k·√(Lᵢ·Lⱼ)` from a [`Circuit::couple`] relation (`0` when the two are not
/// directly coupled).
///
/// Coupled inductors are solved with a **branch-current formulation** — each gets
/// its own MNA branch row whose equation is `v = Σⱼ (Lᵢⱼ/h)·(iⱼ − iⱼ_prev)` in a
/// Backward-Euler transient (and `v = Σⱼ jωLᵢⱼ·iⱼ` in AC). That is the robust way
/// to stamp mutual coupling: the alternative admittance/Norton companion would need
/// the *inverse* inductance matrix, which is singular exactly at the ideal `k = 1`
/// transformer the gate requires. An **isolated** inductor keeps its original
/// single conductance companion (admittance in AC), so nothing uncoupled moves a
/// bit.
struct CoupledGroup {
    /// Indices (into the caller's inductor list) of this group's members, ascending.
    members: Vec<usize>,
    /// Dense inductance matrix in `members` order (henries), symmetric.
    lmat: Vec<Vec<f64>>,
}

/// The coupled-inductor structure of a circuit: which inductors are magnetically
/// coupled (and so need an extra MNA branch row) and the per-group inductance
/// matrices. Built by [`couplings_of`]; a circuit with no [`Circuit::couple`]
/// relations yields `count == 0` and all-`None` `branch_of`, i.e. the pre-M6 path
/// unchanged.
struct Couplings {
    /// Per inductor (caller's order): its 0-based offset among the coupled branch
    /// rows, or `None` if it is uncoupled (keeps the isolated-inductor companion).
    branch_of: Vec<Option<usize>>,
    /// Total coupled inductors = number of branch rows the coupling adds.
    count: usize,
    /// The coupled groups (each with ≥ 2 members).
    groups: Vec<CoupledGroup>,
}

/// Union-find root with path halving, over a flat parent slice.
fn uf_find(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

/// Union two union-find sets, keeping the smaller index as the root (so grouping
/// is deterministic regardless of the order edges are added).
fn uf_union(parent: &mut [usize], a: usize, b: usize) {
    let (ra, rb) = (uf_find(parent, a), uf_find(parent, b));
    if ra != rb {
        parent[ra.max(rb)] = ra.min(rb);
    }
}

/// Partition a circuit's inductors — given in `ind_refs` / `ind_l` order (matching
/// how the transient and AC paths enumerate inductor parts) — into magnetically-
/// coupled groups per the circuit's [`Circuit::couple`] relations, and assign each
/// coupled inductor a branch-row offset (`CP-SIM-M6.1`).
///
/// A [`Coupling`](ohmforge_core::graph::Coupling) naming two of these inductors adds an
/// edge between them; connected components of size ≥ 2 become coupled groups, each
/// with a dense inductance matrix (`L` on the diagonal, `M = k·√(Lᵢ·Lⱼ)` for a
/// directly-coupled pair). Isolated inductors get `branch_of = None` and keep their
/// exact single-inductor companion, so a circuit with no couplings is bit-for-bit
/// unchanged. Grouping is deterministic (union-find to the smaller root; members
/// and groups in ascending index order).
fn couplings_of(circuit: &Circuit, ind_refs: &[String], ind_l: &[f64]) -> Couplings {
    let n = ind_refs.len();
    let mut branch_of = vec![None; n];
    if circuit.couplings().is_empty() || n == 0 {
        return Couplings {
            branch_of,
            count: 0,
            groups: Vec::new(),
        };
    }

    let index_of = |r: &str| ind_refs.iter().position(|s| s == r);
    let mut parent: Vec<usize> = (0..n).collect();
    // Directly-coupled pairs → their coefficient `k`, keyed by the ordered index
    // pair so the mutual term is looked up unambiguously below.
    let mut pair_k: BTreeMap<(usize, usize), f64> = BTreeMap::new();
    for cpl in circuit.couplings() {
        if let (Some(i), Some(j)) = (index_of(&cpl.a), index_of(&cpl.b)) {
            if i != j {
                pair_k.insert((i.min(j), i.max(j)), cpl.k);
                uf_union(&mut parent, i, j);
            }
        }
    }

    // Collect members per connected component (BTreeMap keeps roots ascending, and
    // members are pushed in ascending index order).
    let mut by_root: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let r = uf_find(&mut parent, i);
        by_root.entry(r).or_default().push(i);
    }

    let mut groups = Vec::new();
    let mut offset = 0usize;
    for members in by_root.into_values() {
        if members.len() < 2 {
            continue; // an isolated inductor keeps its single-inductor companion
        }
        let sz = members.len();
        let mut lmat = vec![vec![0.0; sz]; sz];
        for (li, &gi) in members.iter().enumerate() {
            lmat[li][li] = ind_l[gi];
            for (lj, &gj) in members.iter().enumerate().skip(li + 1) {
                // members ascending ⇒ gi < gj, so the key is already ordered.
                if let Some(&k) = pair_k.get(&(gi, gj)) {
                    let m = k * (ind_l[gi] * ind_l[gj]).sqrt();
                    lmat[li][lj] = m;
                    lmat[lj][li] = m;
                }
            }
        }
        for &gi in &members {
            branch_of[gi] = Some(offset);
            offset += 1;
        }
        groups.push(CoupledGroup { members, lmat });
    }

    Couplings {
        branch_of,
        count: offset,
        groups,
    }
}

/// A transmission line resolved for the **transient** engine (`CP-SIM-M7.2`).
///
/// A lossless line is Branin's travelling-wave companion: each port is a resistor
/// `Z₀` in series with a history voltage source `E` read from the *other* port one
/// delay ago, so it takes two branch rows (`branch = Some(first row)`) and a
/// history buffer. A lossy line is a lumped single-π — a series `R + L`
/// Norton companion between the `+` nodes and a shunt `C/2 + G/2` at each port
/// (`L = Z₀·T`, `C = T/Z₀`) — carrying its own running state, no branch row
/// (`branch = None`).
struct TLine {
    a1: Option<usize>,
    a2: Option<usize>,
    b1: Option<usize>,
    b2: Option<usize>,
    z0: f64,
    delay: f64,
    /// `None` = lossless; `Some((r, g, l, c))` = lumped-LC lossy.
    loss: Option<(f64, f64, f64, f64)>,
    /// For a lossless line, the first of its two branch rows; `None` for a lossy
    /// line (which adds no branch row).
    branch: Option<usize>,
}

/// The committed transmission-line state the fixed-step transient driver threads
/// across steps (`CP-SIM-M7.2`). A lossless line reads its delayed port history
/// from here; a lossy line reads its previous companion state.
struct LineState {
    /// Committed sample times, ascending, with `times[0] = 0`.
    times: Vec<f64>,
    /// `lossless[s][i] = [v1, i1, v2, i2]` of lossless line `i` (in lossless-line
    /// order) at committed sample `s`. `i`/`i…` are the branch currents *into* the
    /// line at each port.
    lossless: Vec<Vec<[f64; 4]>>,
    /// `lossy[i] = [series_current, cap_a_v, cap_b_v]` — lossy line `i`'s running
    /// companion state (previous step only, in lossy-line order).
    lossy: Vec<[f64; 3]>,
}

impl LineState {
    /// The port state `[v1, i1, v2, i2]` of lossless line `i` interpolated at time
    /// `tq`, or all-zero for `tq` before the first sample (the quiescent line before
    /// a wave has had time to arrive). Linear interpolation between the committed
    /// samples bracketing `tq`; clamped to the last sample for `tq` past the end.
    fn port_at(&self, i: usize, tq: f64) -> [f64; 4] {
        if tq < self.times[0] {
            return [0.0; 4];
        }
        let last = *self.times.last().expect("≥1 committed sample");
        if tq >= last {
            return self.lossless.last().expect("≥1 sample")[i];
        }
        let mut hi = 1;
        while self.times[hi] < tq {
            hi += 1;
        }
        let lo = hi - 1;
        let (t0, t1) = (self.times[lo], self.times[hi]);
        let f = if t1 > t0 { (tq - t0) / (t1 - t0) } else { 0.0 };
        let (a, b) = (self.lossless[lo][i], self.lossless[hi][i]);
        let mut out = [0.0; 4];
        for k in 0..4 {
            out[k] = a[k] + f * (b[k] - a[k]);
        }
        out
    }
}

/// Stamp a lossless transmission line's two Branin companion branches into the
/// real transient system: port A on branch `ka` with history voltage `e1`, port B
/// on branch `kb` with `e2`. Branch currents are the currents *into the line*, so
/// the port equation is `v − Z₀·i = e` with the node coupling `−i` at the `+`
/// node (the current leaves it into the line). (`CP-SIM-M7.2`.)
fn stamp_lossless_line(sys: &mut System, ln: &TLine, ka: usize, kb: usize, e1: f64, e2: f64) {
    sys.stamp_matrix(Some(ka), ln.a1, 1.0);
    sys.stamp_matrix(Some(ka), ln.a2, -1.0);
    sys.stamp_matrix(Some(ka), Some(ka), -ln.z0);
    // The branch current `i1` leaves node a1 into the line (returns at a2), exactly
    // a voltage source's node coupling (+1 at the `+` node).
    sys.stamp_matrix(ln.a1, Some(ka), 1.0);
    sys.stamp_matrix(ln.a2, Some(ka), -1.0);
    sys.stamp_current(Some(ka), None, e1);

    sys.stamp_matrix(Some(kb), ln.b1, 1.0);
    sys.stamp_matrix(Some(kb), ln.b2, -1.0);
    sys.stamp_matrix(Some(kb), Some(kb), -ln.z0);
    sys.stamp_matrix(ln.b1, Some(kb), 1.0);
    sys.stamp_matrix(ln.b2, Some(kb), -1.0);
    sys.stamp_current(Some(kb), None, e2);
}

/// Stamp a lossy (lumped single-π) line's Backward-Euler companions into the real
/// transient step system: a series `R + L` Norton between the `+` nodes
/// (conductance `1/(R + L/h)` plus the inductive history current) and a shunt
/// `C/2 + G/2` at each port, from the previous state `[series_i, cap_a_v, cap_b_v]`.
/// (`CP-SIM-M7.2`.)
fn stamp_lossy_line(sys: &mut System, ln: &TLine, h: f64, state: [f64; 3]) {
    let (r, g, l, c) = ln.loss.expect("lossy line carries its RLGC");
    let [is_prev, ca_prev, cb_prev] = state;
    let lh = l / h;
    let g_eq = 1.0 / (r + lh);
    sys.stamp_conductance(ln.a1, ln.b1, g_eq);
    // Inductive history current, flowing a1 → b1.
    sys.stamp_current(ln.b1, ln.a1, g_eq * lh * is_prev);
    let g_c = 0.5 * c / h;
    let g_sh = 0.5 * g;
    sys.stamp_conductance(ln.a1, ln.a2, g_c + g_sh);
    sys.stamp_current(ln.a1, ln.a2, g_c * ca_prev);
    sys.stamp_conductance(ln.b1, ln.b2, g_c + g_sh);
    sys.stamp_current(ln.b1, ln.b2, g_c * cb_prev);
}

/// A controlled switch resolved for the **transient** engine (`CP-SIM-M9.1`).
///
/// It is a plain conductance between nodes `a` and `b` whose value snaps between
/// `1/ron` (closed) and `1/roff` (open) according to the control node's voltage.
/// The control voltage is read from the *previous* committed step's solution — a
/// one-timestep lag that is standard for a control-driven switch and negligible at
/// a timestep fine against the PWM period; the switch adds **no** branch row, so a
/// circuit with no switches assembles exactly as before.
struct TSwitch {
    ctrl: Option<usize>,
    a: Option<usize>,
    b: Option<usize>,
    ron: f64,
    roff: f64,
    threshold: f64,
}

impl TSwitch {
    fn resolve(circuit: &Circuit, layout: &Layout, sw: &Switch) -> Self {
        let node = |t| layout.node_of(circuit, t);
        Self {
            ctrl: node(sw.ctrl),
            a: node(sw.a),
            b: node(sw.b),
            ron: sw.ron,
            roff: sw.roff,
            threshold: sw.threshold,
        }
    }

    /// The conductance to stamp given the control voltage read from `node_prev`:
    /// `1/ron` when `V(ctrl) ≥ threshold`, else `1/roff`.
    fn conductance(&self, node_prev: &[f64]) -> f64 {
        let vctrl = across(node_prev, self.ctrl, None);
        if vctrl >= self.threshold {
            1.0 / self.ron
        } else {
            1.0 / self.roff
        }
    }
}

struct TransientModel<'a> {
    circuit: &'a Circuit,
    layout: &'a Layout,
    /// Capacitors as `(plus, minus, farads)`.
    caps: Vec<(NetId, NetId, f64)>,
    /// Inductors as `(a, b, henries)`.
    inds: Vec<(NetId, NetId, f64)>,
    /// Magnetic-coupling structure over `inds` (`CP-SIM-M6.1`). Coupled inductors
    /// are stamped with a branch row each (`coupling.count` of them, right after the
    /// controlled-source branches); isolated inductors keep the conductance
    /// companion, so a circuit with no couplings is unchanged.
    coupling: Couplings,
    /// Transmission lines (`CP-SIM-M7.2`), resolved. Empty ⇒ the pre-M7 path,
    /// bit-for-bit. Lossless lines take two branch rows each (after the
    /// coupled-inductor branches); lossy lines take none.
    lines: Vec<TLine>,
    /// Total branch rows the lossless lines add (`2 ×` their count).
    line_branches: usize,
    /// Controlled switches (`CP-SIM-M9.1`), resolved to node indices. Empty ⇒ the
    /// pre-M9 path, bit-for-bit. Each stamps a bare conductance (no branch row), so
    /// they never change the system size.
    switches: Vec<TSwitch>,
    /// Nonlinear devices, re-linearized inside each step's Newton loop.
    devices: Vec<Box<dyn NonlinearDevice>>,
    /// Node count and voltage-source branch count. The step system is
    /// `n + m + cb + coupling.count + line_branches`, with `cb` the
    /// controlled-source (VCVS/CCVS) branch rows, `coupling.count` the
    /// coupled-inductor branch rows, and `line_branches` the lossless-line rows.
    n: usize,
    m: usize,
    cb: usize,
    /// Time-integration method (`CP-SIM-M10.2`). [`IntegrationMethod::BackwardEuler`]
    /// (the default) reproduces every pre-M10 transient bit-for-bit; the opt-in
    /// trapezoidal method changes only the two-terminal reactive companions.
    method: IntegrationMethod,
}

impl<'a> TransientModel<'a> {
    fn build(
        circuit: &'a Circuit,
        layout: &'a Layout,
        registry: &DeviceRegistry,
        method: IntegrationMethod,
    ) -> Result<Self, SimError> {
        let caps: Vec<(NetId, NetId, f64)> = circuit
            .parts()
            .iter()
            .filter(|p| matches!(p.kind, PartKind::Capacitor))
            .map(|p| {
                let (a, b) = terminals2(p)?;
                // A non-positive capacitance gives a negative/zero companion
                // conductance `C/h`, which silently poisons the solve — reject it
                // as a bad value, exactly as a non-positive resistance/inductance is.
                let c = magnitude(p)?;
                if c <= 0.0 {
                    return Err(SimError::BadValue(p.reference.clone()));
                }
                Ok::<_, SimError>((a, b, c))
            })
            .collect::<Result<_, _>>()?;

        // A non-positive inductance would blow up `h/L`. Collect the references and
        // values alongside, in the same order, so the coupling structure can map a
        // `Circuit::couple(l1, l2, …)` relation onto these inductors.
        let mut inds: Vec<(NetId, NetId, f64)> = Vec::new();
        let mut ind_refs: Vec<String> = Vec::new();
        let mut ind_l: Vec<f64> = Vec::new();
        for p in circuit.parts() {
            if matches!(p.kind, PartKind::Inductor) {
                let (a, b) = terminals2(p)?;
                let l = magnitude(p)?;
                if l <= 0.0 {
                    return Err(SimError::BadValue(p.reference.clone()));
                }
                inds.push((a, b, l));
                ind_refs.push(p.reference.clone());
                ind_l.push(l);
            }
        }
        let coupling = couplings_of(circuit, &ind_refs, &ind_l);

        // Transmission lines (`CP-SIM-M7.2`): lossless lines get two branch rows
        // each (right after the coupled-inductor branches); lossy lines get none.
        let line_base = layout.n_nodes()
            + layout.vsources.len()
            + layout.n_controlled_branches()
            + coupling.count;
        let mut lines: Vec<TLine> = Vec::with_capacity(circuit.lines().len());
        let mut line_branches = 0usize;
        for ln in circuit.lines() {
            let node = |t| layout.node_of(circuit, t);
            let (loss, branch) = match ln.kind {
                LineKind::Lossless => {
                    let b = Some(line_base + line_branches);
                    line_branches += 2;
                    (None, b)
                }
                LineKind::Lossy { r, g } => {
                    (Some((r, g, ln.z0 * ln.delay, ln.delay / ln.z0)), None)
                }
            };
            lines.push(TLine {
                a1: node(ln.a1),
                a2: node(ln.a2),
                b1: node(ln.b1),
                b2: node(ln.b2),
                z0: ln.z0,
                delay: ln.delay,
                loss,
                branch,
            });
        }

        let switches: Vec<TSwitch> = circuit
            .switches()
            .iter()
            .map(|sw| TSwitch::resolve(circuit, layout, sw))
            .collect();

        let devices = collect_nonlinear(circuit, registry)?;
        Ok(Self {
            circuit,
            layout,
            caps,
            inds,
            coupling,
            lines,
            line_branches,
            switches,
            devices,
            n: layout.n_nodes(),
            m: layout.vsources.len(),
            cb: layout.n_controlled_branches(),
            method,
        })
    }

    /// Count of lossy transmission lines (each carrying running companion state).
    fn n_lossy(&self) -> usize {
        self.lines.iter().filter(|l| l.loss.is_some()).count()
    }

    /// Solve the `t = 0` start: capacitors held at 0 V (their initial condition,
    /// each a 0 V source), inductors open, and every transmission line in its
    /// quiescent matched state (each port presenting `Z₀`, no history). Returns the
    /// node solution, the initial capacitor voltages (inductor currents start at
    /// 0 A), and the initial [`LineState`] (the committed `t = 0` sample).
    fn initial(&self) -> Result<(Vec<f64>, Vec<f64>, LineState), SimError> {
        let (circuit, layout, n, m, cb) = (self.circuit, self.layout, self.n, self.m, self.cb);
        // At t = 0 every line is relaxed: with zero history a Branin port reduces to
        // a plain `Z₀` resistor (`v = Z₀·i`), the correct matched initial condition,
        // so all lines (lossless and lossy) are stamped that way here. Their branch
        // rows sit past the capacitor 0 V sources.
        let init_line_base = n + m + cb + self.caps.len();
        let stamp_at = |sys: &mut System, alpha: f64| -> Result<(), SimError> {
            stamp_resistors(sys, circuit, layout)?;
            // t = 0 uses the waveform bias (sine → offset, pulse → v1).
            stamp_vsources(sys, circuit, layout, None, alpha)?;
            stamp_current_sources(sys, circuit, layout, None, alpha)?;
            stamp_controlled_sources(sys, circuit, layout)?;
            // Capacitor 0 V sources sit on branches past the controlled-source rows.
            for (ci, (a, b, _)) in self.caps.iter().enumerate() {
                let k = n + m + cb + ci;
                sys.stamp_vsource(
                    layout.node_of(circuit, *a),
                    layout.node_of(circuit, *b),
                    k,
                    0.0,
                );
            }
            // Every line as a quiescent `Z₀` port pair (zero history).
            for (i, ln) in self.lines.iter().enumerate() {
                let ka = init_line_base + 2 * i;
                stamp_lossless_line(sys, ln, ka, ka + 1, 0.0, 0.0);
            }
            // Switches start **open** at the t = 0 initial condition (no previous
            // step to read the control from). `roff` is finite, so the branch is a
            // large resistor — never a true open that would float a node. The t = 0
            // sample is only the initial condition; the steady state the converter
            // settles to over many PWM cycles is independent of this choice.
            for sw in &self.switches {
                sys.stamp_conductance(sw.a, sw.b, 1.0 / sw.roff);
            }
            Ok(())
        };
        // The t = 0 solve gets the same convergence-aid ladder as the DC operating
        // point: a stiff nonlinear bias the op-point can crack must not spuriously
        // fail a transient just because it is the first sample. Its structure (caps
        // held as 0 V sources) differs from the stepping structure, so it uses its
        // own solver rather than the one the step loop reuses.
        let init_size = init_line_base + 2 * self.lines.len();
        let mut solver = ReusableSolver::new(init_size);
        let x0 = solve_aided(
            circuit,
            layout,
            init_size,
            &self.devices,
            &stamp_at,
            &mut solver,
        )?;
        let cap_v0 = self
            .caps
            .iter()
            .map(|(a, b, _)| {
                across(
                    &x0,
                    layout.node_of(circuit, *a),
                    layout.node_of(circuit, *b),
                )
            })
            .collect();

        // The committed t = 0 line sample: every lossless line's port state read off
        // the init branches; lossy companion state starts at zero.
        let mut lossless0 = Vec::new();
        for (i, ln) in self.lines.iter().enumerate() {
            if ln.branch.is_none() {
                continue; // lossy lines keep no history
            }
            let (ka, kb) = (init_line_base + 2 * i, init_line_base + 2 * i + 1);
            lossless0.push([
                across(&x0, ln.a1, ln.a2),
                x0[ka],
                across(&x0, ln.b1, ln.b2),
                x0[kb],
            ]);
        }
        let line_state = LineState {
            times: vec![0.0],
            lossless: vec![lossless0],
            lossy: vec![[0.0; 3]; self.n_lossy()],
        };
        Ok((x0, cap_v0, line_state))
    }

    /// One Backward-Euler step of size `h` landing at time `t_end`, from the
    /// capacitor voltages `cap_prev` and inductor currents `ind_prev`. Returns the
    /// node solution at `t_end` and the refreshed `(cap_prev, ind_prev)`.
    #[allow(clippy::too_many_arguments)]
    fn step(
        &self,
        t_end: f64,
        h: f64,
        cap_prev: &[f64],
        ind_prev: &[f64],
        cap_i_prev: &[f64],
        ind_v_prev: &[f64],
        line_prev: &LineState,
        node_prev: &[f64],
        solver: &mut ReusableSolver,
    ) -> Result<StepOut, SimError> {
        let (circuit, layout) = (self.circuit, self.layout);
        let trap = matches!(self.method, IntegrationMethod::Trapezoidal);
        // `alpha` ramps the independent sources only (source stepping); the
        // reactive companions carry state from the previous step and stay at full.
        let stamp_at = |sys: &mut System, alpha: f64| -> Result<(), SimError> {
            stamp_resistors(sys, circuit, layout)?;
            // Sources evaluated at the step's end time.
            stamp_vsources(sys, circuit, layout, Some(t_end), alpha)?;
            stamp_current_sources(sys, circuit, layout, Some(t_end), alpha)?;
            stamp_controlled_sources(sys, circuit, layout)?;
            for (ci, (a, b, c)) in self.caps.iter().enumerate() {
                let (ia, ib) = (layout.node_of(circuit, *a), layout.node_of(circuit, *b));
                if trap {
                    // Trapezoidal (`CP-SIM-M10.2`): `i_C,n = (2C/h)(v_n − v_{n−1}) −
                    // i_{n−1}`, a Norton of conductance `g_eq = 2C/h` and history
                    // current `g_eq·v_prev + i_prev`.
                    let g_eq = 2.0 * c / h;
                    sys.stamp_conductance(ia, ib, g_eq);
                    sys.stamp_current(ia, ib, g_eq * cap_prev[ci] + cap_i_prev[ci]);
                } else {
                    // Backward Euler — the original companion, bit-for-bit.
                    let g_eq = c / h;
                    sys.stamp_conductance(ia, ib, g_eq);
                    sys.stamp_current(ia, ib, g_eq * cap_prev[ci]);
                }
            }
            // Isolated inductors.
            for (li, (a, b, l)) in self.inds.iter().enumerate() {
                if self.coupling.branch_of[li].is_some() {
                    continue; // coupled — stamped by the branch-row loop below (always BE)
                }
                let (ia, ib) = (layout.node_of(circuit, *a), layout.node_of(circuit, *b));
                if trap {
                    // Trapezoidal: `i_n = (h/2L)(v_n + v_{n−1}) + i_{n−1}`, a Norton of
                    // conductance `g_eq = h/2L` and history current `g_eq·v_prev +
                    // i_prev` oriented a → b.
                    let g_eq = h / (2.0 * l);
                    sys.stamp_conductance(ia, ib, g_eq);
                    sys.stamp_current(ib, ia, ind_prev[li] + g_eq * ind_v_prev[li]);
                } else {
                    // Backward Euler — the original conductance companion, bit-for-bit.
                    let g_eq = h / l;
                    sys.stamp_conductance(ia, ib, g_eq);
                    // History source `i_prev` oriented a → b: current flows a into b.
                    sys.stamp_current(ib, ia, ind_prev[li]);
                }
            }
            // Coupled inductors: a branch-current row each. Branch `k`'s equation is
            // `(v_a − v_b) − Σⱼ (Lₖⱼ/h)·iⱼ = −Σⱼ (Lₖⱼ/h)·iⱼ_prev` (Backward Euler on
            // `v = L·di/dt` with the mutual terms), which stamps exactly at `k = 1`
            // where an admittance companion would need the singular inverse of L.
            let cbase = self.n + self.m + self.cb;
            for group in &self.coupling.groups {
                for (li_local, &li) in group.members.iter().enumerate() {
                    let (a, b, _) = self.inds[li];
                    let k_col = cbase + self.coupling.branch_of[li].expect("coupled");
                    let (ia, ib) = (layout.node_of(circuit, a), layout.node_of(circuit, b));
                    // The `(v_a − v_b)` term + the branch-current node coupling.
                    sys.stamp_vsource(ia, ib, k_col, 0.0);
                    let mut rhs = 0.0;
                    for (lj_local, &lj) in group.members.iter().enumerate() {
                        let l_kj = group.lmat[li_local][lj_local];
                        if l_kj == 0.0 {
                            continue;
                        }
                        let j_col = cbase + self.coupling.branch_of[lj].expect("coupled");
                        sys.stamp_matrix(Some(k_col), Some(j_col), -l_kj / h);
                        rhs += (l_kj / h) * ind_prev[lj];
                    }
                    sys.stamp_current(Some(k_col), None, -rhs);
                }
            }
            // Transmission lines (`CP-SIM-M7.2`). A lossless line reads its delayed
            // port history (Branin's E) and takes its two branch rows; a lossy line
            // stamps its lumped-π Backward-Euler companions from its previous state.
            let mut ll = 0usize; // lossless-line index
            let mut ly = 0usize; // lossy-line index
            for ln in &self.lines {
                match ln.branch {
                    Some(b0) => {
                        let port = line_prev.port_at(ll, t_end - ln.delay);
                        let [v1, i1, v2, i2] = port;
                        let e1 = v2 + ln.z0 * i2; // from the far port one delay ago
                        let e2 = v1 + ln.z0 * i1;
                        stamp_lossless_line(sys, ln, b0, b0 + 1, e1, e2);
                        ll += 1;
                    }
                    None => {
                        stamp_lossy_line(sys, ln, h, line_prev.lossy[ly]);
                        ly += 1;
                    }
                }
            }
            // Controlled switches (`CP-SIM-M9.1`): a bare conductance whose value is
            // chosen from the previous step's control-node voltage (`Ron` above the
            // threshold, `Roff` below). No switches ⇒ this loop is empty and the step
            // system is byte-identical to the pre-M9 assembly.
            for sw in &self.switches {
                sys.stamp_conductance(sw.a, sw.b, sw.conductance(node_prev));
            }
            Ok(())
        };
        // Each step gets the aided ladder too (plain NR first, so a convergent step
        // is unchanged); this keeps a nonlinear transient as robust as its op-point.
        // The caller's `solver` is shared across steps: every step stamps the same
        // structure (the reactive companions never change shape), so on a large net
        // the fill-reducing ordering is computed once and only refactored per step.
        let cbase = self.n + self.m + self.cb;
        let x = solve_aided(
            circuit,
            layout,
            cbase + self.coupling.count + self.line_branches,
            &self.devices,
            &stamp_at,
            solver,
        )?;
        let new_caps: Vec<f64> = self
            .caps
            .iter()
            .map(|(a, b, _)| across(&x, layout.node_of(circuit, *a), layout.node_of(circuit, *b)))
            .collect();
        let new_inds: Vec<f64> = self
            .inds
            .iter()
            .enumerate()
            .map(|(li, (a, b, l))| match self.coupling.branch_of[li] {
                // A coupled inductor's current is a solved branch unknown (always BE).
                Some(off) => x[cbase + off],
                None => {
                    let v = across(&x, layout.node_of(circuit, *a), layout.node_of(circuit, *b));
                    if trap {
                        // Trapezoidal: `i_n = (h/2L)·v_n + i_prev + (h/2L)·v_prev`.
                        let g_eq = h / (2.0 * l);
                        ind_prev[li] + g_eq * (v + ind_v_prev[li])
                    } else {
                        // Backward Euler: `i += (h/L)·v`.
                        ind_prev[li] + (h / l) * v
                    }
                }
            })
            .collect();
        // Trapezoidal carries one extra history per two-terminal reactance — the
        // capacitor current and the isolated-inductor voltage at this step — so the
        // next step's companion can use the second-order update. Backward Euler needs
        // neither, so both stay empty and the default path threads nothing.
        let (new_cap_i, new_ind_v) = if trap {
            let g_eq_c = |c: f64| 2.0 * c / h;
            let cap_i = self
                .caps
                .iter()
                .enumerate()
                .map(|(ci, (_, _, c))| g_eq_c(*c) * (new_caps[ci] - cap_prev[ci]) - cap_i_prev[ci])
                .collect();
            let ind_v = self
                .inds
                .iter()
                .map(|(a, b, _)| {
                    across(&x, layout.node_of(circuit, *a), layout.node_of(circuit, *b))
                })
                .collect();
            (cap_i, ind_v)
        } else {
            (Vec::new(), Vec::new())
        };

        // Refresh the transmission-line state from the solved step.
        let mut new_lossless = Vec::new();
        let mut new_lossy = Vec::new();
        let mut ly = 0usize;
        for ln in &self.lines {
            match ln.branch {
                Some(b0) => new_lossless.push([
                    across(&x, ln.a1, ln.a2),
                    x[b0],
                    across(&x, ln.b1, ln.b2),
                    x[b0 + 1],
                ]),
                None => {
                    let (r, _, l, _) = ln.loss.expect("lossy carries RLGC");
                    let [is_prev, ..] = line_prev.lossy[ly];
                    let g_eq = 1.0 / (r + l / h);
                    let is_new = g_eq * (across(&x, ln.a1, ln.b1) + (l / h) * is_prev);
                    new_lossy.push([is_new, across(&x, ln.a1, ln.a2), across(&x, ln.b1, ln.b2)]);
                    ly += 1;
                }
            }
        }

        Ok(StepOut {
            x,
            caps: new_caps,
            inds: new_inds,
            lossless: new_lossless,
            lossy: new_lossy,
            cap_i: new_cap_i,
            ind_v: new_ind_v,
        })
    }

    /// Package recorded `times` × per-node `traces` into one [`Signal`] per node.
    fn signals(&self, times: Vec<f64>, traces: Vec<Vec<f64>>) -> Vec<Signal> {
        traces
            .into_iter()
            .enumerate()
            .map(|(d, y)| {
                Signal::time(
                    format!("v(n{})", self.layout.inv[d]),
                    Unit::Volt,
                    times.clone(),
                    y,
                )
            })
            .collect()
    }
}

/// Transient dispatcher. `step = Some(h)` runs the fixed-step integration exactly as
/// M0 did (bit-for-bit under the default Backward Euler — the existing tests do not
/// move); `step = None` asks the engine to choose the timestep and runs the
/// error-controlled adaptive driver ([`adaptive_transient`], `CP-SIM-M1.6`). `method`
/// selects the integrator (`CP-SIM-M10.2`); a non-default method requires a fixed
/// timestep (the adaptive driver's error control is first-order Backward Euler).
fn transient(
    circuit: &Circuit,
    layout: &Layout,
    duration: f64,
    step: Option<f64>,
    registry: &DeviceRegistry,
    method: IntegrationMethod,
) -> Result<Vec<Signal>, SimError> {
    if duration <= 0.0 {
        return Err(SimError::BadValue("transient duration/step".into()));
    }
    // A non-default integrator (trapezoidal) only runs at a fixed timestep: the
    // adaptive driver's step-doubling LTE estimate is first-order Backward Euler by
    // construction, so mixing it with a second-order method would mis-control the step.
    if !matches!(method, IntegrationMethod::BackwardEuler) && !matches!(step, Some(h) if h > 0.0) {
        return Err(SimError::Unsupported(
            "trapezoidal integration needs a fixed transient timestep",
        ));
    }
    let model = TransientModel::build(circuit, layout, registry, method)?;
    // Transmission lines need the uniform history grid the fixed step provides (the
    // delayed port state is interpolated from committed samples), so a circuit with
    // a line requires an explicit timestep; the adaptive driver rejects it cleanly.
    if !model.lines.is_empty() && !matches!(step, Some(h) if h > 0.0) {
        return Err(SimError::Unsupported(
            "a transmission line needs a fixed transient timestep",
        ));
    }
    // A controlled switch reads its control from the previous committed step, so it
    // needs the uniform grid a fixed step gives (the adaptive driver's reject/retry
    // would re-read a half-formed state); require an explicit timestep, as lines do.
    if !model.switches.is_empty() && !matches!(step, Some(h) if h > 0.0) {
        return Err(SimError::Unsupported(
            "a controlled switch needs a fixed transient timestep",
        ));
    }
    // A lossless line reads its delayed port history by interpolating committed
    // samples one delay back. A timestep coarser than the line delay pushes that
    // lookback (`t − delay`) past the last committed sample, where `LineState::port_at`
    // would silently clamp to it — under-resolving the travelling wave. Reject it
    // explicitly rather than return a quietly wrong waveform. (`CP-SIM-M8.1`.) The
    // delay-matched step (`h == delay`, lookback = the previous sample exactly) is fine.
    if let Some(h) = step {
        if h > 0.0 {
            if let Some(min_delay) = model
                .lines
                .iter()
                .filter(|l| l.loss.is_none())
                .map(|l| l.delay)
                .reduce(f64::min)
            {
                if h > min_delay * (1.0 + 1e-9) {
                    return Err(SimError::Unsupported(
                        "transient timestep is coarser than a lossless transmission-line delay",
                    ));
                }
            }
        }
    }
    let (x0, cap_v0, line0) = model.initial()?;
    match step {
        Some(h) if h > 0.0 => fixed_transient(&model, duration, h, x0, cap_v0, line0),
        Some(_) => Err(SimError::BadValue("transient duration/step".into())),
        None => adaptive_transient(&model, duration, x0, cap_v0),
    }
}

/// Fixed-step Backward Euler at uniform `t = k·h`, `k = 1 … round(duration/h)`.
/// Identical to the original M0 loop, so every fixed-step result is unchanged.
fn fixed_transient(
    model: &TransientModel,
    duration: f64,
    h: f64,
    x0: Vec<f64>,
    cap_v0: Vec<f64>,
    mut line_state: LineState,
) -> Result<Vec<Signal>, SimError> {
    let steps = (duration / h).round().max(1.0) as usize;
    let n = model.n;
    let mut times: Vec<f64> = Vec::with_capacity(steps + 1);
    let mut traces: Vec<Vec<f64>> = vec![Vec::with_capacity(steps + 1); n];
    times.push(0.0);
    for (d, trace) in traces.iter_mut().enumerate() {
        trace.push(x0[d]);
    }
    let mut cap_prev = cap_v0;
    let mut ind_prev = vec![0.0; model.inds.len()];
    // Trapezoidal carries one extra history per two-terminal reactance (capacitor
    // current, isolated-inductor voltage), both zero at the relaxed t = 0 start;
    // Backward Euler carries none, so these stay empty and are never read on the
    // default path — keeping it bit-for-bit (`CP-SIM-M10.2`).
    let trap = matches!(model.method, IntegrationMethod::Trapezoidal);
    let mut cap_i_prev = if trap {
        vec![0.0; model.caps.len()]
    } else {
        Vec::new()
    };
    let mut ind_v_prev = if trap {
        vec![0.0; model.inds.len()]
    } else {
        Vec::new()
    };
    // The previous step's full solution — a controlled switch reads its control-node
    // voltage from here (`CP-SIM-M9.1`). It starts at the t = 0 initial solution.
    let mut node_prev = x0;
    // One solver for the whole run: every step stamps the same structure, so a
    // large net analyzes the fill-reducing ordering once and refactors per step.
    let mut solver = ReusableSolver::new(
        model.n + model.m + model.cb + model.coupling.count + model.line_branches,
    );
    for k in 1..=steps {
        let t = k as f64 * h;
        let out = model.step(
            t,
            h,
            &cap_prev,
            &ind_prev,
            &cap_i_prev,
            &ind_v_prev,
            &line_state,
            &node_prev,
            &mut solver,
        )?;
        times.push(t);
        for (d, trace) in traces.iter_mut().enumerate() {
            trace.push(out.x[d]);
        }
        cap_prev = out.caps;
        ind_prev = out.inds;
        cap_i_prev = out.cap_i;
        ind_v_prev = out.ind_v;
        node_prev = out.x;
        // Commit this step's transmission-line state: append the lossless history
        // sample (so later steps can read it one delay back) and advance the lossy
        // companion state.
        if !model.lines.is_empty() {
            line_state.times.push(t);
            line_state.lossless.push(out.lossless);
            line_state.lossy = out.lossy;
        }
    }
    Ok(model.signals(times, traces))
}

/// The normalized step-doubling error: the largest per-node difference between
/// the coarse one-step solution `x_coarse` and the finer two-half-step solution
/// `x_fine`, each scaled by `abstol + reltol·max|·|`. For first-order Backward
/// Euler this difference is the finer solution's local truncation error, so a
/// value ≤ 1 means the step met tolerance.
fn lte_norm(x_fine: &[f64], x_coarse: &[f64]) -> f64 {
    x_fine
        .iter()
        .zip(x_coarse)
        .map(|(&f, &c)| {
            let scale = LTE_ABSTOL + LTE_RELTOL * f.abs().max(c.abs());
            (f - c).abs() / scale
        })
        .fold(0.0, f64::max)
}

/// The next-step multiplier from the normalized error: `safety·err^(−1/2)` (the
/// Backward-Euler order-1 exponent), clamped to `[SHRINK_MIN, GROW_MAX]`. A
/// vanishing error (e.g. a memoryless step) grows at the cap.
fn lte_step_factor(err: f64) -> f64 {
    if err <= 0.0 {
        return LTE_GROW_MAX;
    }
    (LTE_SAFETY * err.powf(-0.5)).clamp(LTE_SHRINK_MIN, LTE_GROW_MAX)
}

/// Error-controlled adaptive Backward Euler (`CP-SIM-M1.6`).
///
/// Each attempt estimates the local truncation error by step doubling — one step
/// of `h` versus two of `h/2` — accepts the finer solution when [`lte_norm`] ≤ 1
/// (or the step has hit its floor, to guarantee progress), and resizes `h` by
/// [`lte_step_factor`] within `[h_min, h_max]`. The result is a non-uniform time
/// grid: fine where the solution bends, coarse where it is placid — materially
/// fewer points than a fixed step dense enough to resolve the fast region.
fn adaptive_transient(
    model: &TransientModel,
    duration: f64,
    x0: Vec<f64>,
    cap_v0: Vec<f64>,
) -> Result<Vec<Signal>, SimError> {
    let n = model.n;
    let h_max = duration / MIN_ADAPTIVE_STEPS;
    let h_min = duration * H_MIN_FRAC;

    let mut times: Vec<f64> = vec![0.0];
    let mut traces: Vec<Vec<f64>> = (0..n).map(|d| vec![x0[d]]).collect();
    let mut cap_prev = cap_v0;
    let mut ind_prev = vec![0.0; model.inds.len()];
    // Previous-step solution for any controlled switch (`CP-SIM-M9.1`). The adaptive
    // driver never runs with a switch (the dispatcher routes a switch-bearing circuit
    // to the fixed step, as for lines), so this is never read here; it is threaded
    // only to share the one [`step`] with the fixed driver.
    let mut node_prev = x0;
    let mut t = 0.0;
    let mut h = (duration / 100.0).clamp(h_min, h_max);
    // Shared across every accept/reject attempt (coarse + two half steps): all stamp
    // the same structure, so a large net reuses one symbolic factorization throughout.
    let mut solver = ReusableSolver::new(model.n + model.m + model.cb + model.coupling.count);
    // The adaptive driver never runs with transmission lines (the dispatcher routes
    // a line-bearing circuit to the fixed step), so a quiescent, never-read line
    // state suffices here.
    let no_lines = LineState {
        times: vec![0.0],
        lossless: vec![Vec::new()],
        lossy: Vec::new(),
    };

    let mut iters = 0usize;
    while t < duration * (1.0 - 1e-12) {
        iters += 1;
        if iters > ADAPTIVE_ITER_CAP {
            return Err(SimError::NonConvergence(ADAPTIVE_ITER_CAP));
        }
        // Clamp the attempt to land exactly on `duration`.
        let h_try = h.min(duration - t);
        let at_floor = h_try <= h_min * (1.0 + 1e-9);

        // Coarse: one step of h_try. Fine: two steps of h_try/2. The adaptive driver
        // is Backward Euler by construction (the dispatcher rejects any other method),
        // so no trapezoidal current/voltage history is threaded — empty slices here.
        let coarse = model.step(
            t + h_try,
            h_try,
            &cap_prev,
            &ind_prev,
            &[],
            &[],
            &no_lines,
            &node_prev,
            &mut solver,
        )?;
        let half = 0.5 * h_try;
        let mid = model.step(
            t + half,
            half,
            &cap_prev,
            &ind_prev,
            &[],
            &[],
            &no_lines,
            &node_prev,
            &mut solver,
        )?;
        let fine = model.step(
            t + h_try,
            half,
            &mid.caps,
            &mid.inds,
            &[],
            &[],
            &no_lines,
            &mid.x,
            &mut solver,
        )?;

        let err = lte_norm(&fine.x, &coarse.x);
        if err <= 1.0 || at_floor {
            // Accept the finer solution and advance.
            t += h_try;
            cap_prev = fine.caps;
            ind_prev = fine.inds;
            times.push(t);
            for (d, trace) in traces.iter_mut().enumerate() {
                trace.push(fine.x[d]);
            }
            node_prev = fine.x;
            h = (h_try * lte_step_factor(err)).clamp(h_min, h_max);
        } else {
            // Reject: shrink and retry from the same state.
            h = (h_try * lte_step_factor(err)).max(h_min);
        }
    }

    Ok(model.signals(times, traces))
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

/// The frequency-independent assembly of a circuit's small-signal AC system,
/// gathered once (`CP-SIM-M1.4`, shared with the S-parameter path `CP-SIM-M9.2`).
///
/// [`build`](AcNetwork::build) linearizes the circuit about its DC operating point
/// and collects every stamp that does not depend on frequency — resistor
/// conductances, capacitor/inductor reactances, the coupled-inductor and
/// transmission-line companions, each nonlinear device's small-signal Jacobian, and
/// the voltage-source branch rows. [`assemble`](AcNetwork::assemble) stamps them
/// into a fresh [`ComplexSystem`] at a given angular frequency with **every
/// independent source quiet** (voltage sources as 0 V shorts, current sources open).
/// The caller then adds the excitation: [`ac`] a 1∠0 drive on each AC-marked source,
/// [`Engine::s_parameters`] a `Z₀` port source/termination pair. Every stamp is
/// additive, so both callers assemble a system byte-identical to the pre-refactor
/// inline loop — the golden AC suite is unchanged.
struct AcNetwork<'a> {
    circuit: &'a Circuit,
    layout: &'a Layout,
    /// Node count (the signal rows).
    n: usize,
    /// Full MNA system size (nodes + branch rows).
    size: usize,
    /// First coupled-inductor branch row.
    ac_base: usize,
    conductances: Vec<(Option<usize>, Option<usize>, f64)>,
    caps: Vec<(Option<usize>, Option<usize>, f64)>,
    /// Isolated inductors (admittance companion).
    inds: Vec<(Option<usize>, Option<usize>, f64)>,
    /// All inductors in part order (for coupled-group stamping).
    all_inds: Vec<(Option<usize>, Option<usize>, f64)>,
    coupling: Couplings,
    lines: Vec<AcLine>,
    line_branch_of: Vec<usize>,
    device_g: Vec<(Option<usize>, Option<usize>, f64)>,
    /// Every voltage source as `(plus, minus, branch row k)`, stamped as a 0 V short
    /// by [`assemble`]; the caller drives the AC ones through the branch RHS.
    vsources: Vec<(Option<usize>, Option<usize>, usize)>,
    /// Branch rows `k` of the AC-drive voltage sources (a 1∠0 phasor each).
    ac_drive_branches: Vec<usize>,
    /// AC-drive current sources as `(a, b)` — a 1∠0 injection a → b.
    ac_drive_csources: Vec<(Option<usize>, Option<usize>)>,
}

impl<'a> AcNetwork<'a> {
    fn build(
        circuit: &'a Circuit,
        layout: &'a Layout,
        registry: &DeviceRegistry,
    ) -> Result<Self, SimError> {
        // Bias point for linearizing nonlinear devices. A linear circuit needs none,
        // so this skips the DC gate that used to reject a DC-floating node (see
        // [`small_signal_bias`]); the complex solve catches a true singularity.
        let x_dc = small_signal_bias(circuit, layout, registry)?;

        let n = layout.n_nodes();
        let m = layout.vsources.len();
        // Isolated inductors are admittances in AC — no branch rows; coupled
        // inductors get a branch row each (after the controlled-source branches),
        // because mutual coupling is singular to invert at the ideal k = 1.
        let ac_base = n + m + layout.n_controlled_branches();

        let mut conductances: Vec<(Option<usize>, Option<usize>, f64)> = Vec::new();
        let mut caps: Vec<(Option<usize>, Option<usize>, f64)> = Vec::new();
        let mut all_inds: Vec<(Option<usize>, Option<usize>, f64)> = Vec::new();
        let mut ind_refs: Vec<String> = Vec::new();
        let mut ind_l: Vec<f64> = Vec::new();
        for part in circuit.parts() {
            match part.kind {
                PartKind::Resistor => {
                    let r = resistor_value(circuit, part)?;
                    let (a, b) = terminals2(part)?;
                    conductances.push((
                        layout.node_of(circuit, a),
                        layout.node_of(circuit, b),
                        1.0 / r,
                    ));
                }
                PartKind::Capacitor => {
                    let c = magnitude(part)?;
                    if c <= 0.0 {
                        return Err(SimError::BadValue(part.reference.clone()));
                    }
                    let (a, b) = terminals2(part)?;
                    caps.push((layout.node_of(circuit, a), layout.node_of(circuit, b), c));
                }
                PartKind::Inductor => {
                    let l = magnitude(part)?;
                    if l <= 0.0 {
                        return Err(SimError::BadValue(part.reference.clone()));
                    }
                    let (a, b) = terminals2(part)?;
                    all_inds.push((layout.node_of(circuit, a), layout.node_of(circuit, b), l));
                    ind_refs.push(part.reference.clone());
                    ind_l.push(l);
                }
                _ => {}
            }
        }
        // Magnetic coupling (`CP-SIM-M6.1`). Isolated inductors (`branch_of == None`)
        // stay admittances in `inds`; coupled ones get a branch row. With no
        // couplings this is the pre-M6 path, bit-for-bit.
        let coupling = couplings_of(circuit, &ind_refs, &ind_l);
        // Transmission lines (`CP-SIM-M7.2`): a lossless line adds two branch rows
        // (its `e^{−jωT}` travelling-wave companion), after the coupled-inductor
        // branches; a lossy line adds none. No lines ⇒ the pre-M7 layout stands.
        let line_base = ac_base + coupling.count;
        let lines: Vec<AcLine> = circuit
            .lines()
            .iter()
            .map(|ln| AcLine::resolve(circuit, layout, ln))
            .collect();
        let mut n_line_branches = 0usize;
        let mut line_branch_of: Vec<usize> = Vec::with_capacity(lines.len());
        for ln in &lines {
            line_branch_of.push(line_base + n_line_branches);
            if ln.loss.is_none() {
                n_line_branches += 2;
            }
        }
        let size = line_base + n_line_branches;
        let inds: Vec<(Option<usize>, Option<usize>, f64)> = all_inds
            .iter()
            .enumerate()
            .filter(|(i, _)| coupling.branch_of[*i].is_none())
            .map(|(_, &t)| t)
            .collect();

        // Nonlinear devices → their small-signal Jacobian at the DC bias (a real
        // admittance block), flattened to `(row, col, g)` matrix entries.
        let devices = collect_nonlinear(circuit, registry)?;
        let mut device_g: Vec<(Option<usize>, Option<usize>, f64)> = Vec::new();
        for dev in &devices {
            let nodes: Vec<Option<usize>> = dev
                .terminals()
                .iter()
                .map(|&t| layout.node_of(circuit, t))
                .collect();
            let v: Vec<f64> = nodes
                .iter()
                .map(|&nd| nd.map_or(0.0, |i| x_dc[i]))
                .collect();
            let comp = dev.linearize(&v, &mut vec![0.0; dev.state_len()], false);
            for (a, &na) in nodes.iter().enumerate() {
                for (b, &nb) in nodes.iter().enumerate() {
                    device_g.push((na, nb, comp.g[a][b]));
                }
            }
        }

        // Voltage sources → branch row k (stamped as a 0 V short in `assemble`);
        // record which are AC drives so the caller adds their 1∠0 branch RHS.
        let mut vsources: Vec<(Option<usize>, Option<usize>, usize)> = Vec::new();
        let mut ac_drive_branches: Vec<usize> = Vec::new();
        for (branch, &pi) in layout.vsources.iter().enumerate() {
            let part = &circuit.parts()[pi];
            let (plus, minus) = terminals2(part)?;
            let k = n + branch;
            vsources.push((
                layout.node_of(circuit, plus),
                layout.node_of(circuit, minus),
                k,
            ));
            if is_ac_drive(part) {
                ac_drive_branches.push(k);
            }
        }
        let mut ac_drive_csources: Vec<(Option<usize>, Option<usize>)> = Vec::new();
        for part in circuit.parts() {
            if matches!(part.kind, PartKind::CurrentSource) && is_ac_drive(part) {
                let (a, b) = terminals2(part)?;
                ac_drive_csources.push((layout.node_of(circuit, a), layout.node_of(circuit, b)));
            }
        }

        Ok(Self {
            circuit,
            layout,
            n,
            size,
            ac_base,
            conductances,
            caps,
            inds,
            all_inds,
            coupling,
            lines,
            line_branch_of,
            device_g,
            vsources,
            ac_drive_branches,
            ac_drive_csources,
        })
    }

    /// Stamp the whole frequency-independent network into a fresh complex system at
    /// angular frequency `w`, with every independent source quiet (voltage sources
    /// as 0 V shorts, current sources open). The caller adds the excitation.
    fn assemble(&self, w: f64) -> Result<ComplexSystem, SimError> {
        let mut sys = ComplexSystem::new(self.size);
        self.stamp_into(&mut sys, w)?;
        Ok(sys)
    }

    /// Assemble forced to the dense backing — the exact per-frequency dense solve a
    /// swept AC uses below the sparse/ordered thresholds, exposed so a bench or test
    /// can race the ordered reusable path against it at any size (`CP-SIM-M10.1`).
    fn assemble_dense(&self, w: f64) -> Result<ComplexSystem, SimError> {
        let mut sys = ComplexSystem::dense(self.size);
        self.stamp_into(&mut sys, w)?;
        Ok(sys)
    }

    /// Stamp the whole frequency-independent network into `sys` at angular frequency
    /// `w`, every independent source quiet. The backing (dense or sparse) is the
    /// caller's; the stamps are identical either way, so the assembled matrix is
    /// byte-for-byte the same regardless.
    fn stamp_into(&self, sys: &mut ComplexSystem, w: f64) -> Result<(), SimError> {
        for &(a, b, g) in &self.conductances {
            sys.stamp_admittance(a, b, Complex::new(g, 0.0));
        }
        for &(a, b, c) in &self.caps {
            sys.stamp_admittance(a, b, Complex::new(0.0, w * c)); // jωC
        }
        for &(a, b, l) in &self.inds {
            sys.stamp_admittance(a, b, Complex::new(0.0, -1.0 / (w * l))); // 1/(jωL)
        }
        // Coupled-inductor branch rows: `v = jω·L·i` with the mutual terms — valid
        // at the ideal k = 1 where the admittance form is singular.
        for group in &self.coupling.groups {
            for (li_local, &li) in group.members.iter().enumerate() {
                let (a, b, _) = self.all_inds[li];
                let k_col = self.ac_base + self.coupling.branch_of[li].expect("coupled");
                sys.stamp_vsource(a, b, k_col, Complex::ZERO);
                for (lj_local, &lj) in group.members.iter().enumerate() {
                    let l_kj = group.lmat[li_local][lj_local];
                    if l_kj == 0.0 {
                        continue;
                    }
                    let j_col = self.ac_base + self.coupling.branch_of[lj].expect("coupled");
                    sys.stamp_matrix(Some(k_col), Some(j_col), Complex::new(0.0, -w * l_kj));
                }
            }
        }
        for &(a, b, g) in &self.device_g {
            sys.stamp_matrix(a, b, Complex::new(g, 0.0));
        }
        for (li, ln) in self.lines.iter().enumerate() {
            ln.stamp_ac(sys, w, self.line_branch_of[li]);
        }
        for &(plus, minus, k) in &self.vsources {
            sys.stamp_vsource(plus, minus, k, Complex::ZERO);
        }
        stamp_controlled_sources_complex(sys, self.circuit, self.layout)?;
        Ok(())
    }
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
    registry: &DeviceRegistry,
) -> Result<Vec<Signal>, SimError> {
    if !f_start.is_finite()
        || !f_stop.is_finite()
        || f_start <= 0.0
        || f_stop <= f_start
        || points_per_decade == 0
    {
        return Err(SimError::BadValue("AC sweep range/points".into()));
    }

    // Gather the frequency-independent assembly once, then re-stamp per frequency.
    let net = AcNetwork::build(circuit, layout, registry)?;
    let freqs = log_frequencies(f_start, f_stop, points_per_decade);
    let mut per_node: Vec<Vec<Complex>> = vec![Vec::with_capacity(freqs.len()); net.n];

    // One solver for the whole sweep: every frequency assembles the *same structure*
    // (only the `jωC` / `1/jωL` numbers move), so a large network analyzes the
    // fill-reducing ordering once and only refactors per frequency. Below the
    // threshold it is a bit-for-bit pass-through to the dense/natural-order solve, so
    // small AC fixtures are byte-identical to the pre-reuse per-frequency solve.
    let mut solver = ComplexReusableSolver::new(net.size);
    for &f in &freqs {
        let w = TAU * f;
        let mut sys = net.assemble(w)?;
        // AC excitation: a 1∠0 phasor on every AC-marked source. For a voltage
        // source this is `volts = 1` added to its branch-row RHS (the 0 V short's
        // ±1 couplings were already stamped by `assemble`); for a current source a
        // 1∠0 injection a → b. The result is byte-identical to the pre-refactor loop.
        for &k in &net.ac_drive_branches {
            sys.stamp_current(Some(k), None, Complex::ONE);
        }
        for &(a, b) in &net.ac_drive_csources {
            sys.stamp_current(b, a, Complex::ONE);
        }

        let x = solver.solve(sys).ok_or(SimError::Singular)?;
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

// ---------------------------------------------------------------------------
// Transmission lines — AC stamp (CP-SIM-M7.2)
// ---------------------------------------------------------------------------

/// A transmission line resolved to MNA node indices for **AC** stamping
/// (`CP-SIM-M7.2`). The lossless line is Branin's travelling-wave companion in
/// phasor form — each port a resistor `Z₀` coupled to the other port one delay ago
/// through `e^{−jωT}`; the lossy line is a lumped single-π of `R + jωL` series and
/// `(G + jωC)/2` shunt admittances, with `L = Z₀·T`, `C = T/Z₀`.
struct AcLine {
    a1: Option<usize>,
    a2: Option<usize>,
    b1: Option<usize>,
    b2: Option<usize>,
    z0: f64,
    delay: f64,
    /// `None` = lossless; `Some((r, g, l, c))` = lumped-LC lossy.
    loss: Option<(f64, f64, f64, f64)>,
}

impl AcLine {
    fn resolve(circuit: &Circuit, layout: &Layout, ln: &Line) -> Self {
        let node = |t| layout.node_of(circuit, t);
        let loss = match ln.kind {
            LineKind::Lossless => None,
            LineKind::Lossy { r, g } => Some((r, g, ln.z0 * ln.delay, ln.delay / ln.z0)),
        };
        Self {
            a1: node(ln.a1),
            a2: node(ln.a2),
            b1: node(ln.b1),
            b2: node(ln.b2),
            z0: ln.z0,
            delay: ln.delay,
            loss,
        }
    }

    /// Stamp this line into the complex AC system at angular frequency `w`, using
    /// branch rows `kbase` / `kbase + 1` (lossless only; a lossy line ignores them).
    fn stamp_ac(&self, sys: &mut ComplexSystem, w: f64, kbase: usize) {
        match self.loss {
            None => {
                // Branch currents `i1`, `i2` are the currents *into the line*; the
                // two port equations are `v1 − Z₀·i1 = E(v2 + Z₀·i2)` and its B↔A
                // twin, with `E = e^{−jωT}`.
                let wt = w * self.delay;
                let e = Complex::new(wt.cos(), -wt.sin());
                let z0 = Complex::new(self.z0, 0.0);
                let one = Complex::ONE;
                let (ka, kb) = (kbase, kbase + 1);

                // Port A branch row.
                sys.stamp_matrix(Some(ka), self.a1, one);
                sys.stamp_matrix(Some(ka), self.a2, -one);
                sys.stamp_matrix(Some(ka), Some(ka), -z0);
                sys.stamp_matrix(Some(ka), self.b1, -e);
                sys.stamp_matrix(Some(ka), self.b2, e);
                sys.stamp_matrix(Some(ka), Some(kb), -e * z0);
                // Port A node coupling: `i1` leaves node a1, returns at a2 (the +1
                // sense of a voltage source's branch current).
                sys.stamp_matrix(self.a1, Some(ka), one);
                sys.stamp_matrix(self.a2, Some(ka), -one);

                // Port B branch row.
                sys.stamp_matrix(Some(kb), self.b1, one);
                sys.stamp_matrix(Some(kb), self.b2, -one);
                sys.stamp_matrix(Some(kb), Some(kb), -z0);
                sys.stamp_matrix(Some(kb), self.a1, -e);
                sys.stamp_matrix(Some(kb), self.a2, e);
                sys.stamp_matrix(Some(kb), Some(ka), -e * z0);
                sys.stamp_matrix(self.b1, Some(kb), one);
                sys.stamp_matrix(self.b2, Some(kb), -one);
            }
            Some((r, g, l, c)) => {
                // Lumped single-π: series admittance `1/(R + jωL)` between a1,b1 and
                // shunt `(G + jωC)/2` at each port.
                let series = Complex::new(r, w * l).inv();
                sys.stamp_admittance(self.a1, self.b1, series);
                let half = Complex::new(0.5 * g, 0.5 * w * c);
                sys.stamp_admittance(self.a1, self.a2, half);
                sys.stamp_admittance(self.b1, self.b2, half);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Small-signal transfer function + pole/zero analysis (CP-SIM-M7.1)
// ---------------------------------------------------------------------------

/// Upper bound on the linearized MNA system size the pole/zero + transfer-function
/// analysis ([`Engine::transfer_function`]) will attempt.
///
/// The pole extraction runs a dense `O(N³)` hand-rolled eigensolver
/// (a compact in-crate dense eigensolver), so the analysis is deliberately capped at a **modest size**:
/// a larger circuit is reported [`SimError::Unsupported`] rather than solved
/// slowly. This covers every lumped circuit a `.TF` is realistically asked for
/// (the gate RC / divider are size ≤ 3); the augmented zero pencil adds one row,
/// so the bound is enforced against `size + 1`.
pub const PZ_MAX_SYSTEM: usize = 128;

/// The result of a small-signal transfer-function / pole-zero analysis
/// ([`Engine::transfer_function`], `CP-SIM-M7.1`): a circuit linearized about its
/// DC operating point and probed between one input source and one output node.
///
/// The gain and port impedances are DC (`s = 0`) small-signal quantities, exactly
/// as a SPICE `.TF` reports; the poles are intrinsic to the circuit (independent
/// of the chosen port), while the zeros are the transmission zeros of the chosen
/// input→output path.
#[derive(Debug, Clone, PartialEq)]
pub struct TransferFunction {
    /// The DC small-signal gain from the input source to the output node:
    /// `V(output)/V(input)` for a voltage-source input (dimensionless), or
    /// `V(output)/I(input)` (a transresistance, in ohms) for a current-source input.
    pub dc_gain: f64,
    /// The DC resistance looking into the input source's port (ohms): the applied
    /// test level over the current it delivers.
    pub input_impedance: f64,
    /// The DC resistance looking into the output port to ground, with every
    /// independent source quieted (ohms): a 1 A probe injected at the output node
    /// over the voltage it develops.
    pub output_impedance: f64,
    /// The pole locations in the Laplace `s`-plane (rad/s) — the finite `s` solving
    /// `det(G + sC) = 0`. Real poles carry a zero imaginary part; complex poles come
    /// in conjugate pairs. A purely resistive circuit has none.
    pub poles: Vec<Complex>,
    /// The finite transmission zeros (rad/s), *where tractable*: `Some(vec)` — the
    /// zeros of the input→output path, possibly empty (no finite zeros, as for a
    /// first-order low-pass) — or `None` when the augmented zero pencil is singular
    /// (e.g. a zero exactly at the origin, or a feed-through the reduction cannot
    /// isolate) or the output is ground.
    pub zeros: Option<Vec<Complex>>,
}

impl Engine {
    /// Small-signal transfer-function / pole-zero analysis (`CP-SIM-M7.1`): the
    /// `.TF` report between an independent `input_source` and the `output` node.
    ///
    /// The circuit is linearized about its DC operating point (reusing the
    /// Newton–Raphson bias, so nonlinear devices enter by their small-signal
    /// conductance, exactly as [`Analysis::Ac`] does). From the linearized MNA it
    /// reports the DC gain, the input/output port resistances, and the pole (and,
    /// where tractable, zero) locations.
    ///
    /// **Poles** are the finite roots of `det(G + sC) = 0` for the conductance
    /// matrix `G` and the capacitance/inductance matrix `C` — a generalized
    /// eigenproblem. Because `C` is singular in MNA (branch rows and purely
    /// resistive nodes carry no reactance), the problem is reduced to a *standard*
    /// eigenproblem on `G⁻¹C` (`G` is nonsingular at a valid operating point): the
    /// finite poles are `s = −1/μ` over the nonzero eigenvalues `μ`, with the
    /// `μ ≈ 0` eigenvalues (poles at infinity) discarded. The eigenvalues come from
    /// the hand-rolled QR iteration in a compact in-crate dense eigensolver — **no external
    /// linear-algebra crate**.
    ///
    /// Bounded to systems of at most [`PZ_MAX_SYSTEM`] unknowns (the dense
    /// eigensolver is `O(N³)`); a larger circuit returns [`SimError::Unsupported`].
    /// A circuit carrying a transmission line is also [`SimError::Unsupported`]
    /// here (a distributed delay has no finite-dimensional pole set). The
    /// `input_source` must name an independent voltage or current source present in
    /// the circuit, else [`SimError::UnknownPart`] / [`SimError::Unsupported`].
    pub fn transfer_function(
        &self,
        circuit: &Circuit,
        output: NetId,
        input_source: &str,
    ) -> Result<TransferFunction, SimError> {
        let reg = &self.registry;
        validate(circuit, reg)?;
        if !circuit.lines().is_empty() {
            return Err(SimError::Unsupported(
                "transfer function with a transmission line",
            ));
        }
        let layout = Layout::build(circuit);

        // Classify the input source and locate how it enters the system.
        let in_part = circuit
            .part_by_ref(input_source)
            .ok_or_else(|| SimError::UnknownPart(input_source.to_string()))?;
        let input = match in_part.kind {
            PartKind::VoltageSource => {
                let pos = layout
                    .vsources
                    .iter()
                    .position(|&pi| circuit.parts()[pi].reference == input_source)
                    .expect("a voltage source is always in the layout");
                TfInput::Volt(layout.n_nodes() + pos)
            }
            PartKind::CurrentSource => {
                let (a, b) = terminals2(in_part)?;
                TfInput::Curr(layout.node_of(circuit, a), layout.node_of(circuit, b))
            }
            _ => {
                return Err(SimError::Unsupported(
                    "transfer-function input is not an independent source",
                ))
            }
        };
        let out_node = layout.node_of(circuit, output);

        // Linearize about the DC operating point and assemble the (G, C) pencil. A
        // linear circuit needs no bias, so this skips the DC gate that used to reject a
        // DC-floating node (see [`small_signal_bias`]).
        let x_dc = small_signal_bias(circuit, &layout, reg)?;
        let (g, c, size) = assemble_pencil(circuit, &layout, &x_dc, reg)?;
        if size + 1 > PZ_MAX_SYSTEM {
            return Err(SimError::Unsupported(
                "circuit too large for pole/zero analysis",
            ));
        }

        // DC gain + port impedances: two solves of the real G matrix (s = 0). The
        // first excites the input (unit drive); the second injects a unit probe
        // current at the output with every independent source quiet.
        let mut b_in = vec![0.0; size];
        match input {
            TfInput::Volt(k) => b_in[k] = 1.0, // a 1 V source
            TfInput::Curr(a, b) => {
                // 1 A forced into node a and out of node b (the source's direction).
                if let Some(i) = a {
                    b_in[i] += 1.0;
                }
                if let Some(j) = b {
                    b_in[j] -= 1.0;
                }
            }
        }
        let mut b_out = vec![0.0; size];
        if let Some(o) = out_node {
            b_out[o] = 1.0;
        }
        let sols = eig::solve_multi(&g, size, &[b_in.clone(), b_out]).ok_or(SimError::Singular)?;
        let x_in = &sols[0];
        let x_out = &sols[1];

        let out_v = out_node.map_or(0.0, |o| x_in[o]);
        let (dc_gain, input_impedance) = match input {
            TfInput::Volt(k) => {
                // Gain = V(out)/1 V. R_in = 1 V / delivered current (= −branch current).
                let i_branch = x_in[k];
                let r_in = if i_branch != 0.0 {
                    1.0 / (-i_branch)
                } else {
                    f64::INFINITY
                };
                (out_v, r_in)
            }
            TfInput::Curr(a, b) => {
                // Gain = V(out)/1 A (transresistance). R_in = V(port)/1 A.
                let v_port = a.map_or(0.0, |i| x_in[i]) - b.map_or(0.0, |j| x_in[j]);
                (out_v, v_port)
            }
        };
        let output_impedance = out_node.map_or(0.0, |o| x_out[o]);

        // Poles: s = −1/μ over the finite (nonzero) eigenvalues μ of G⁻¹C.
        let poles = finite_poles(&g, &c, size)?;
        // Zeros (where tractable): the augmented Rosenbrock pencil.
        let zeros = transmission_zeros(&g, &c, size, &b_in, out_node);

        Ok(TransferFunction {
            dc_gain,
            input_impedance,
            output_impedance,
            poles,
            zeros,
        })
    }
}

// ---------------------------------------------------------------------------
// 2-port S-parameters (CP-SIM-M9.2)
// ---------------------------------------------------------------------------

/// The scattering (S-) parameters of a 2-port, referenced to a real system
/// impedance `z0`, at one frequency ([`Engine::s_parameters`], `CP-SIM-M9.2`).
///
/// Each `S_ij` is the ratio of the wave emerging from port `i` to the wave
/// incident on port `j` with every other port matched-terminated. `s11`/`s22` are
/// the input/output reflection coefficients; `s21`/`s12` the forward/reverse
/// transmission. The [`z`](SParams::z) field carries the equivalent open-circuit
/// impedance (Z-) matrix converted from the S-parameters; it can be non-finite for
/// a network with no Z representation (a bare series element), which is a property
/// of the Z form, not an error.
#[derive(Debug, Clone, PartialEq)]
pub struct SParams {
    /// Frequency the parameters were computed at (Hz).
    pub freq: f64,
    /// Reference (system) impedance `Z₀` (Ω), real.
    pub z0: f64,
    /// Input reflection coefficient `S₁₁`.
    pub s11: Complex,
    /// Reverse transmission `S₁₂`.
    pub s12: Complex,
    /// Forward transmission `S₂₁`.
    pub s21: Complex,
    /// Output reflection coefficient `S₂₂`.
    pub s22: Complex,
    /// The 2-port impedance (Z-) matrix `[[Z₁₁, Z₁₂], [Z₂₁, Z₂₁]]` (Ω), converted
    /// from the S-parameters referenced to `z0`.
    pub z: [[Complex; 2]; 2],
}

impl Engine {
    /// Two-port scattering parameters referenced to a real impedance `z0`, computed
    /// from the linearized AC solve at `freq` (`CP-SIM-M9.2`).
    ///
    /// Port 1 is the net `port1` (referenced to ground), port 2 the net `port2`.
    /// The measurement is the textbook terminated setup: each S-column comes from
    /// one complex solve in which the driven port is fed by a 1 V source behind
    /// `z0` and the other port is terminated in `z0`, with the circuit's own
    /// independent sources quiet (bias sources are AC shorts). Reading the port
    /// node voltages `V₁`, `V₂` under a drive at port 1 gives `S₁₁ = 2·V₁ − 1` and
    /// `S₂₁ = 2·V₂`; the symmetric drive at port 2 gives `S₂₂` and `S₁₂`. The
    /// equivalent Z-matrix is then converted from the S-parameters.
    ///
    /// Reuses the shared `AcNetwork` assembly (so coupled inductors, transmission
    /// lines, nonlinear small-signal models, and controlled sources are all honored
    /// exactly as [`Analysis::Ac`] handles them), referenced about the DC operating
    /// point. The API is purely additive — no existing analysis path is touched.
    ///
    /// Errors: [`SimError::BadValue`] for a non-finite/non-positive `z0` or `freq`,
    /// or a port that resolves to ground (a port needs a non-ground node to drive);
    /// [`SimError::Singular`] if the terminated system has no solution.
    pub fn s_parameters(
        &self,
        circuit: &Circuit,
        port1: NetId,
        port2: NetId,
        z0: f64,
        freq: f64,
    ) -> Result<SParams, SimError> {
        let reg = &self.registry;
        validate(circuit, reg)?;
        if !(z0.is_finite() && z0 > 0.0 && freq.is_finite() && freq > 0.0) {
            return Err(SimError::BadValue("S-parameter z0/frequency".into()));
        }
        let layout = Layout::build(circuit);
        let p1 = layout
            .node_of(circuit, port1)
            .ok_or_else(|| SimError::BadValue("S-parameter port 1 is ground".into()))?;
        let p2 = layout
            .node_of(circuit, port2)
            .ok_or_else(|| SimError::BadValue("S-parameter port 2 is ground".into()))?;

        let net = AcNetwork::build(circuit, &layout, reg)?;
        let w = TAU * freq;
        let g0 = 1.0 / z0; // port source/termination conductance
        let y0 = Complex::new(g0, 0.0);
        let drive = Complex::new(g0, 0.0); // Norton current of a 1 V EMF behind z0

        // Solve 1 — drive port 1 (1 V behind z0), terminate port 2 in z0.
        let v_drive1 = {
            let mut sys = net.assemble(w)?;
            sys.stamp_admittance(Some(p1), None, y0);
            sys.stamp_admittance(Some(p2), None, y0);
            sys.stamp_current(Some(p1), None, drive);
            sys.solve().ok_or(SimError::Singular)?
        };
        let (v1a, v2a) = (v_drive1[p1], v_drive1[p2]);

        // Solve 2 — drive port 2, terminate port 1 in z0.
        let v_drive2 = {
            let mut sys = net.assemble(w)?;
            sys.stamp_admittance(Some(p1), None, y0);
            sys.stamp_admittance(Some(p2), None, y0);
            sys.stamp_current(Some(p2), None, drive);
            sys.solve().ok_or(SimError::Singular)?
        };
        let (v1b, v2b) = (v_drive2[p1], v_drive2[p2]);

        let two = Complex::new(2.0, 0.0);
        let s11 = two * v1a - Complex::ONE;
        let s21 = two * v2a;
        let s22 = two * v2b - Complex::ONE;
        let s12 = two * v1b;
        let z = s_to_z(s11, s12, s21, s22, z0);

        Ok(SParams {
            freq,
            z0,
            s11,
            s12,
            s21,
            s22,
            z,
        })
    }
}

/// Convert 2-port S-parameters (referenced to real `z0`) to the open-circuit
/// impedance (Z-) matrix. The determinant `(1−S₁₁)(1−S₂₂) − S₁₂·S₂₁` vanishes for a
/// network with no Z representation (e.g. a bare series impedance), where the
/// entries go non-finite — a property of the Z form, surfaced rather than hidden.
fn s_to_z(s11: Complex, s12: Complex, s21: Complex, s22: Complex, z0: f64) -> [[Complex; 2]; 2] {
    let one = Complex::ONE;
    let z0c = Complex::new(z0, 0.0);
    let two = Complex::new(2.0, 0.0);
    let denom = (one - s11) * (one - s22) - s12 * s21;
    let z11 = z0c * ((one + s11) * (one - s22) + s12 * s21) / denom;
    let z12 = z0c * (two * s12) / denom;
    let z21 = z0c * (two * s21) / denom;
    let z22 = z0c * ((one - s11) * (one + s22) + s12 * s21) / denom;
    [[z11, z12], [z21, z22]]
}

/// How a chosen `.TF` input source enters the linearized system.
enum TfInput {
    /// An independent voltage source, by its absolute MNA branch-row index.
    Volt(usize),
    /// An independent current source, by its `(plus, minus)` node indices
    /// (`None` = ground).
    Curr(Option<usize>, Option<usize>),
}

/// Assemble the linearized small-signal `(G, C)` matrix pencil about the DC
/// operating point `x_dc`, as dense row-major `size × size` arrays, plus `size`.
///
/// `G` is the DC conductance / MNA matrix — resistors, controlled sources, the ±1
/// couplings of voltage sources and (shorted) inductors, and every nonlinear
/// device's small-signal Jacobian at the bias — assembled through the ordinary
/// stamp API so each device's contribution is byte-identical to the AC/DC path.
/// `C` is the reactive matrix: a capacitor adds `C` to its node block, and an
/// inductor branch `k` adds `−L` on `C[k][k]` (with `−M = −k√(LᵢLⱼ)` off-diagonal
/// for a coupled pair), so that `G + sC` is the Laplace-domain system. Every
/// inductor takes a branch row here (the short's ±1 in `G`, its reactance in `C`),
/// matching the inductor-branch placement [`solve_dc`] uses.
fn assemble_pencil(
    circuit: &Circuit,
    layout: &Layout,
    x_dc: &[f64],
    registry: &DeviceRegistry,
) -> Result<(Vec<f64>, Vec<f64>, usize), SimError> {
    let n = layout.n_nodes();
    let m = layout.vsources.len();
    let cb = layout.n_controlled_branches();
    let ind_base = layout.controlled_base() + cb; // first inductor branch row
    let n_ind = layout.inductors.len();
    let size = n + m + cb + n_ind;

    // G through the ordinary stamp API (forced dense), reusing the device stamps.
    let mut gsys = System::dense(size);
    stamp_resistors(&mut gsys, circuit, layout)?;
    stamp_controlled_sources(&mut gsys, circuit, layout)?;
    // Voltage-source ±1 couplings (the RHS is irrelevant to G, so stamp 0 V — no
    // value lookup, so a source carrying only a waveform is fine).
    for (branch, &pi) in layout.vsources.iter().enumerate() {
        let (plus, minus) = terminals2(&circuit.parts()[pi])?;
        gsys.stamp_vsource(
            layout.node_of(circuit, plus),
            layout.node_of(circuit, minus),
            n + branch,
            0.0,
        );
    }
    // Inductor shorts: the ±1 branch couplings (the sL term lives in C).
    for (bi, &pi) in layout.inductors.iter().enumerate() {
        let (a, b) = terminals2(&circuit.parts()[pi])?;
        gsys.stamp_vsource(
            layout.node_of(circuit, a),
            layout.node_of(circuit, b),
            ind_base + bi,
            0.0,
        );
    }
    // Nonlinear devices → their small-signal Jacobian at the bias (as in `ac`).
    let devices = collect_nonlinear(circuit, registry)?;
    for dev in &devices {
        let nodes: Vec<Option<usize>> = dev
            .terminals()
            .iter()
            .map(|&t| layout.node_of(circuit, t))
            .collect();
        let v: Vec<f64> = nodes
            .iter()
            .map(|&nd| nd.map_or(0.0, |i| x_dc[i]))
            .collect();
        let comp = dev.linearize(&v, &mut vec![0.0; dev.state_len()], false);
        for (a, &na) in nodes.iter().enumerate() {
            for (b, &nb) in nodes.iter().enumerate() {
                gsys.stamp_matrix(na, nb, comp.g[a][b]);
            }
        }
    }
    let g = gsys.dense_matrix();

    // C: capacitor node admittances + the inductance matrix on the branch rows.
    let mut c = vec![0.0; size * size];
    let put = |c: &mut [f64], r: usize, col: usize, val: f64| c[r * size + col] += val;
    for part in circuit.parts() {
        if matches!(part.kind, PartKind::Capacitor) {
            let cap = magnitude(part)?;
            if cap <= 0.0 {
                return Err(SimError::BadValue(part.reference.clone()));
            }
            let (a, b) = terminals2(part)?;
            let (ia, ib) = (layout.node_of(circuit, a), layout.node_of(circuit, b));
            if let Some(i) = ia {
                put(&mut c, i, i, cap);
            }
            if let Some(j) = ib {
                put(&mut c, j, j, cap);
            }
            if let (Some(i), Some(j)) = (ia, ib) {
                put(&mut c, i, j, -cap);
                put(&mut c, j, i, -cap);
            }
        }
    }
    // Self-inductances on the branch diagonal; mutuals from the couplings.
    let mut ind_l = vec![0.0; n_ind];
    for (bi, &pi) in layout.inductors.iter().enumerate() {
        let l = magnitude(&circuit.parts()[pi])?;
        if l <= 0.0 {
            return Err(SimError::BadValue(circuit.parts()[pi].reference.clone()));
        }
        ind_l[bi] = l;
        put(&mut c, ind_base + bi, ind_base + bi, -l);
    }
    for cpl in circuit.couplings() {
        let pos = |r: &str| {
            layout
                .inductors
                .iter()
                .position(|&pi| circuit.parts()[pi].reference == r)
        };
        if let (Some(i), Some(j)) = (pos(&cpl.a), pos(&cpl.b)) {
            if i != j {
                let mutual = cpl.k * (ind_l[i] * ind_l[j]).sqrt();
                put(&mut c, ind_base + i, ind_base + j, -mutual);
                put(&mut c, ind_base + j, ind_base + i, -mutual);
            }
        }
    }
    Ok((g, c, size))
}

/// The finite poles `s = −1/μ` over the nonzero eigenvalues `μ` of `G⁻¹C`.
///
/// `G⁻¹C` is formed by one multi-RHS solve (a column of `C` per solve); its
/// eigenvalues come from the hand-rolled QR iteration. The `μ ≈ 0` eigenvalues are
/// the poles at infinity (the `C`-nullspace directions) and are filtered out
/// relative to the largest eigenvalue magnitude.
fn finite_poles(g: &[f64], c: &[f64], size: usize) -> Result<Vec<Complex>, SimError> {
    let rhs: Vec<Vec<f64>> = (0..size)
        .map(|col| (0..size).map(|r| c[r * size + col]).collect())
        .collect();
    let cols = eig::solve_multi(g, size, &rhs).ok_or(SimError::Singular)?;
    let mut m = vec![0.0; size * size];
    for (col, xc) in cols.iter().enumerate() {
        for (r, &v) in xc.iter().enumerate() {
            m[r * size + col] = v;
        }
    }
    let evals = eig::eigenvalues(&m, size)
        .ok_or(SimError::Unsupported("pole eigensolver did not converge"))?;
    Ok(finite_reciprocals(&evals, true))
}

/// The transmission zeros of the input→output path, *where tractable*.
///
/// They are the finite `s` making the Rosenbrock system pencil
/// `[[−G − sC, b], [lᵀ, 0]]` drop rank, with `b` the input excitation column and
/// `l` the output selector. Reduced (exactly as the poles are) to a standard
/// eigenproblem on `M₁⁻¹M₂` where `M₁ = [[−G, b],[lᵀ, 0]]` and `M₂ = diag(C, 0)`:
/// the zeros are `s = 1/ν` over the nonzero eigenvalues `ν`. Returns `None` when
/// the output is ground or `M₁` is singular (a zero at the origin, or a path the
/// reduction cannot isolate) — the documented "where tractable" boundary.
fn transmission_zeros(
    g: &[f64],
    c: &[f64],
    size: usize,
    b: &[f64],
    out_node: Option<usize>,
) -> Option<Vec<Complex>> {
    let out = out_node?;
    let aug = size + 1;
    // M₁ = [[−G, b], [eₒᵤₜᵀ, 0]].
    let mut m1 = vec![0.0; aug * aug];
    for r in 0..size {
        for col in 0..size {
            m1[r * aug + col] = -g[r * size + col];
        }
        m1[r * aug + size] = b[r];
    }
    m1[size * aug + out] = 1.0;
    // M₂ columns: Cₖ padded with a zero last row, and an all-zero last column.
    let mut rhs: Vec<Vec<f64>> = (0..size)
        .map(|col| {
            let mut v = vec![0.0; aug];
            for (r, slot) in v.iter_mut().enumerate().take(size) {
                *slot = c[r * size + col];
            }
            v
        })
        .collect();
    rhs.push(vec![0.0; aug]);
    let cols = eig::solve_multi(&m1, aug, &rhs)?; // None ⇒ M₁ singular ⇒ no tractable zeros
    let mut prod = vec![0.0; aug * aug];
    for (col, xc) in cols.iter().enumerate() {
        for (r, &v) in xc.iter().enumerate() {
            prod[r * aug + col] = v;
        }
    }
    let evals = eig::eigenvalues(&prod, aug)?;
    Some(finite_reciprocals(&evals, false))
}

/// Map eigenvalues to the finite reciprocals a pole/zero reduction needs: drop the
/// `≈ 0` eigenvalues (reciprocal at infinity) relative to the largest magnitude,
/// then return `1/z` (zeros) or `−1/z` (poles, `negate = true`) for the rest.
fn finite_reciprocals(evals: &[Complex], negate: bool) -> Vec<Complex> {
    let max_mag = evals.iter().map(|z| z.magnitude()).fold(0.0, f64::max);
    if max_mag == 0.0 {
        return Vec::new();
    }
    let thresh = 1e-9 * max_mag;
    evals
        .iter()
        .filter(|z| z.magnitude() > thresh)
        .map(|z| {
            let inv = z.inv();
            if negate {
                Complex::new(-inv.re, -inv.im)
            } else {
                inv
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Noise analysis (CP-SIM-M2.1)
// ---------------------------------------------------------------------------

/// Boltzmann constant `k` (J/K), for thermal-noise spectral density.
const BOLTZMANN_K: f64 = 1.380_649e-23;
/// Noise-analysis absolute temperature `T` (K) — 27 °C. The resistor thermal-noise
/// current PSD is `4kT/R`; the output-referred floor of a lone resistor is `4kTR`.
const NOISE_TEMP_K: f64 = 300.15;

/// Small-signal noise analysis (`CP-SIM-M2.1`).
///
/// The output-referred noise PSD is the incoherent sum over independent noise
/// sources of `|Z_k(f)|²·S_k`, where `S_k` is a source's intrinsic spectral
/// density and `Z_k(f) = V(output)/I_k` is the transimpedance from that source to
/// the output. At M2.1 the only generator is resistor thermal noise: resistor `R`
/// carries a noise **current** source of PSD `S = 4kT/R` (A²/Hz) across its own
/// terminals, so its output contribution is `|Z|²·4kT/R` (V²/Hz).
///
/// Each transimpedance reuses the complex-MNA network the AC path builds —
/// resistors as `G`, capacitors as `jωC`, inductors as `1/(jωL)`, nonlinear
/// devices as their small-signal conductance at the DC bias, and controlled
/// sources in their complex form — but with **every independent source quiet** (a
/// voltage source is a short, a current source an open). A unit current injected
/// across a resistor's nodes then reads the transimpedance straight off the output
/// node. (The direct per-source solve is O(resistors) solves per frequency; an
/// adjoint single-solve is future work, as the sparse solver is.)
fn noise(
    circuit: &Circuit,
    layout: &Layout,
    output: NetId,
    f_start: f64,
    f_stop: f64,
    points_per_decade: u32,
    registry: &DeviceRegistry,
) -> Result<Vec<Signal>, SimError> {
    if !f_start.is_finite()
        || !f_stop.is_finite()
        || f_start <= 0.0
        || f_stop <= f_start
        || points_per_decade == 0
    {
        return Err(SimError::BadValue("noise sweep range/points".into()));
    }

    // DC bias, to linearize any nonlinear device. A linear circuit needs none, so this
    // skips the DC gate that used to reject a DC-floating node (see
    // [`small_signal_bias`]); the complex solve below still catches a true singularity.
    let x_dc = small_signal_bias(circuit, layout, registry)?;

    let n = layout.n_nodes();
    let m = layout.vsources.len();
    // Inductors are admittances here (no branch rows), as in AC; voltage sources
    // keep their shorting branch rows, controlled sources their branches.
    let size = n + m + layout.n_controlled_branches();
    let out_node = layout.node_of(circuit, output);

    // Frequency-independent passive data, collected once.
    let mut conductances: Vec<(Option<usize>, Option<usize>, f64)> = Vec::new();
    let mut caps: Vec<(Option<usize>, Option<usize>, f64)> = Vec::new();
    let mut inds: Vec<(Option<usize>, Option<usize>, f64)> = Vec::new();
    // Resistor thermal-noise sources: (nodes, current PSD `4kT/R`).
    let mut noise_sources: Vec<(Option<usize>, Option<usize>, f64)> = Vec::new();
    for part in circuit.parts() {
        match part.kind {
            PartKind::Resistor => {
                let r = resistor_value(circuit, part)?;
                let (a, b) = terminals2(part)?;
                let (na, nb) = (layout.node_of(circuit, a), layout.node_of(circuit, b));
                conductances.push((na, nb, 1.0 / r));
                noise_sources.push((na, nb, 4.0 * BOLTZMANN_K * NOISE_TEMP_K / r));
            }
            PartKind::Capacitor => {
                let c = magnitude(part)?;
                if c <= 0.0 {
                    return Err(SimError::BadValue(part.reference.clone()));
                }
                let (a, b) = terminals2(part)?;
                caps.push((layout.node_of(circuit, a), layout.node_of(circuit, b), c));
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

    // Nonlinear devices → small-signal Jacobian at the DC bias (purely real), as AC.
    let devices = collect_nonlinear(circuit, registry)?;
    let mut device_g: Vec<(Option<usize>, Option<usize>, f64)> = Vec::new();
    for dev in &devices {
        let nodes: Vec<Option<usize>> = dev
            .terminals()
            .iter()
            .map(|&t| layout.node_of(circuit, t))
            .collect();
        let v: Vec<f64> = nodes
            .iter()
            .map(|&nd| nd.map_or(0.0, |i| x_dc[i]))
            .collect();
        let comp = dev.linearize(&v, &mut vec![0.0; dev.state_len()], false);
        for (a, &na) in nodes.iter().enumerate() {
            for (b, &nb) in nodes.iter().enumerate() {
                device_g.push((na, nb, comp.g[a][b]));
            }
        }
    }

    // Voltage sources shorting branch rows (0 V drive — every independent source is
    // quiet in a noise analysis, so no part carries a 1∠0 stimulus).
    let vsource_shorts: Vec<(Option<usize>, Option<usize>, usize)> = layout
        .vsources
        .iter()
        .enumerate()
        .map(|(branch, &pi)| {
            let (plus, minus) = terminals2(&circuit.parts()[pi])?;
            Ok::<_, SimError>((
                layout.node_of(circuit, plus),
                layout.node_of(circuit, minus),
                n + branch,
            ))
        })
        .collect::<Result<_, _>>()?;

    // Stamp the whole quiet network at angular frequency `w` into a fresh system.
    let build_network = |w: f64| -> Result<ComplexSystem, SimError> {
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
        for &(a, b, g) in &device_g {
            sys.stamp_matrix(a, b, Complex::new(g, 0.0));
        }
        for &(plus, minus, k) in &vsource_shorts {
            sys.stamp_vsource(plus, minus, k, Complex::ZERO);
        }
        stamp_controlled_sources_complex(&mut sys, circuit, layout)?;
        Ok(sys)
    };

    let freqs = log_frequencies(f_start, f_stop, points_per_decade);
    let mut psd: Vec<f64> = Vec::with_capacity(freqs.len());
    // A noise sweep is the ideal case for factorization reuse: the same structure at
    // every frequency, and *several* right-hand sides per frequency (a 1 A probe per
    // noise source). Above the ordered threshold we factor the quiet network once per
    // frequency (reusing the symbolic ordering across frequencies) and back-solve each
    // probe; below it we keep the exact pre-reuse path (a fresh solve per probe),
    // byte-identical on the small golden/determinism fixtures.
    let ordered = size >= COMPLEX_KLU_THRESHOLD;
    let mut sym: Option<ComplexSymbolic> = None;
    let mut num: Option<mna_complex::ComplexNumeric> = None;
    // A 1 A probe's right-hand side (`+1` at `na`, `−1` at `nb`) — what `stamp_current`
    // would add to a quiet system's RHS, built directly for the reuse path.
    let probe_rhs = |na: Option<usize>, nb: Option<usize>| {
        let mut rhs = vec![Complex::ZERO; size];
        if let Some(i) = na {
            rhs[i] = rhs[i] + Complex::ONE;
        }
        if let Some(j) = nb {
            rhs[j] = rhs[j] - Complex::ONE;
        }
        rhs
    };
    for &f in &freqs {
        let w = TAU * f;
        let mut total = 0.0;
        if ordered {
            let (mat, _z) = build_network(w)?.into_ordered_parts();
            // Reuse the cached ordering + pivot sequence when it still holds; else
            // analyze + factor afresh (first frequency, or after a pivot collapse).
            let mut reused = false;
            if let (Some(s), Some(nm)) = (sym.as_ref(), num.as_mut()) {
                reused = nm.refactor(s, &mat).is_some();
            }
            if !reused {
                let s = ComplexSymbolic::analyze(&mat);
                let nm = s.factor(&mat).ok_or(SimError::Singular)?;
                sym = Some(s);
                num = Some(nm);
            }
            let nm = num.as_ref().expect("factored above");
            for &(na, nb, s_k) in &noise_sources {
                let x = nm.solve(&probe_rhs(na, nb));
                let z = out_node.map_or(Complex::ZERO, |i| x[i]);
                total += z.magnitude().powi(2) * s_k;
            }
        } else {
            // Each noise source: inject a 1 A probe across its nodes, solve, and read
            // the transimpedance off the output; accumulate |Z|²·S incoherently.
            for &(na, nb, s_k) in &noise_sources {
                let mut sys = build_network(w)?;
                sys.stamp_current(na, nb, Complex::ONE);
                let x = sys.solve().ok_or(SimError::Singular)?;
                let z = out_node.map_or(Complex::ZERO, |i| x[i]);
                total += z.magnitude().powi(2) * s_k;
            }
        }
        psd.push(total);
    }

    Ok(vec![Signal {
        name: format!("onoise(n{})", circuit.resolve(output).0),
        unit: Unit::Volt, // output-referred PSD, in V²/Hz (see `Analysis::Noise`)
        domain: Domain::Freq,
        x: freqs,
        y: SignalData::Real(psd),
    }])
}

// ---------------------------------------------------------------------------
// Monte-Carlo + component tolerances (CP-SIM-M2.2)
// ---------------------------------------------------------------------------

/// A tiny seeded PRNG — SplitMix64. Deterministic and self-contained (no external
/// dependency, no OS entropy), so a given seed yields exactly the same stream on
/// every platform and every run, which is the whole point of the Monte-Carlo gate.
/// SplitMix64 is the standard seeding generator: one `wrapping_add` of the golden-
/// ratio constant per draw, then an avalanche mix.
struct SplitMix64 {
    state: u64,
}

/// SplitMix64's golden-ratio increment. Because each draw adds exactly this to the
/// state, the state after `k` draws from `new(seed)` is `seed + k·γ` — which is
/// what [`SplitMix64::at_offset`] exploits to seek into the stream.
const SPLITMIX_GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

impl SplitMix64 {
    /// A generator positioned so its first draw is draw number `offset` of the
    /// `seed` stream (`at_offset(seed, 0)` is the plain seeding). Since the state
    /// advances by a fixed `γ` per draw, this is an exact `O(1)` seek:
    /// `at_offset(seed, k)` reproduces the stream from draw `k` onward, bit-for-bit.
    /// It lets each Monte-Carlo run own a fixed slice of the single seeded stream,
    /// so a run's perturbations depend only on its index — never on how runs are
    /// scheduled across threads.
    fn at_offset(seed: u64, offset: u64) -> Self {
        Self {
            state: seed.wrapping_add(offset.wrapping_mul(SPLITMIX_GAMMA)),
        }
    }

    /// The next 64 raw bits.
    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(SPLITMIX_GAMMA);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A uniform `f64` in `[0, 1)`, built from the top 53 bits (the mantissa width),
    /// so every representable value is equally likely and the result is exact.
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A uniform `f64` in `[-1, 1)` — the symmetric tolerance draw.
    fn next_signed(&mut self) -> f64 {
        2.0 * self.next_f64() - 1.0
    }
}

impl Engine {
    /// `Analysis::MonteCarlo`: run `inner` `runs` times, each on a copy of the
    /// circuit whose toleranced parts are perturbed within their ±band, and record
    /// the `output` node's value at `sample` from every run.
    ///
    /// **Determinism.** The single [`SplitMix64`] stream seeded from `seed` is
    /// partitioned by run: run `k` owns draws `[k·P, (k+1)·P)` (`P` = the number of
    /// toleranced parts), reached in `O(1)` by [`SplitMix64::at_offset`]. The set of
    /// toleranced parts, their order, and the per-run draw count are all fixed by
    /// the circuit, so the same seed replays the identical perturbations and
    /// therefore the identical samples — bit for bit, and *independently of how the
    /// runs are scheduled*. Each run perturbs from the original nominal values (it
    /// clones the untouched circuit), so draws never compound. A part with
    /// tolerance `t` and nominal value `v` becomes `v·(1 + t·u)`, `u` uniform in
    /// `[-1, 1)`. This per-run independence is exactly what lets the native
    /// `parallel` feature fan the runs across threads with no change in output
    /// (`CP-SIM-M4.3`).
    fn monte_carlo(
        &self,
        circuit: &Circuit,
        seed: u64,
        runs: usize,
        output: NetId,
        sample: usize,
        inner: &Analysis,
    ) -> Result<Vec<Signal>, SimError> {
        // The public entry allows the parallel executor when the feature is on;
        // the forced-sequential twin (tests) shares every bit of the per-run work.
        self.monte_carlo_impl(circuit, seed, runs, output, sample, inner, true)
    }

    /// Always-sequential Monte-Carlo, for the single-vs-multi determinism gate — it
    /// runs the identical per-run computation through the plain sequential loop
    /// regardless of the `parallel` feature, so a test can compare it against the
    /// (possibly threaded) public [`monte_carlo`](Self::monte_carlo).
    #[cfg(test)]
    fn monte_carlo_sequential(
        &self,
        circuit: &Circuit,
        seed: u64,
        runs: usize,
        output: NetId,
        sample: usize,
        inner: &Analysis,
    ) -> Result<Vec<Signal>, SimError> {
        self.monte_carlo_impl(circuit, seed, runs, output, sample, inner, false)
    }

    /// The shared Monte-Carlo core. `parallel_ok` selects the executor: the native
    /// `parallel` feature fans the independent runs across OS threads, else (or on
    /// wasm, or when `parallel_ok` is false) it is the plain sequential loop. Both
    /// assemble the samples in run-index order, so the output is identical.
    #[allow(clippy::too_many_arguments)] // mirrors Analysis::MonteCarlo's fields + an executor flag
    fn monte_carlo_impl(
        &self,
        circuit: &Circuit,
        seed: u64,
        runs: usize,
        output: NetId,
        sample: usize,
        inner: &Analysis,
        parallel_ok: bool,
    ) -> Result<Vec<Signal>, SimError> {
        if runs == 0 {
            return Err(SimError::BadValue("monte-carlo run count".into()));
        }
        // The toleranced parts, captured once in part-index order with their nominal
        // magnitude/unit — the fixed draw schedule that makes the stream reproducible.
        let toleranced: Vec<(usize, f64, f64, Unit)> = circuit
            .parts()
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                let tol = circuit.tolerance(&p.reference)?;
                let value = p.value?;
                Some((i, tol, value.magnitude, value.unit))
            })
            .collect();
        let out_name = format!("v(n{})", circuit.resolve(output).0);
        let per_run = toleranced.len() as u64;

        // One run: seed a generator at this run's fixed slice of the stream, perturb
        // a fresh clone, solve the inner analysis, read back the sample + its unit.
        let run_one = |k: usize| -> Result<(f64, Unit), SimError> {
            let mut rng = SplitMix64::at_offset(seed, k as u64 * per_run);
            let mut work = circuit.clone();
            for &(pi, tol, nominal, part_unit) in &toleranced {
                let factor = 1.0 + tol * rng.next_signed();
                let reference = work.parts()[pi].reference.clone();
                work.set_value(&reference, Value::new(nominal * factor, part_unit));
            }
            let sigs = self.run(&work, inner)?;
            let sig = sigs
                .iter()
                .find(|s| s.name == out_name)
                .ok_or_else(|| SimError::UnknownPart(out_name.clone()))?;
            let value = match &sig.y {
                SignalData::Real(v) => v.get(sample).copied(),
                SignalData::Complex(v) => v.get(sample).map(|z| z.magnitude()),
            }
            .ok_or_else(|| SimError::BadValue("monte-carlo sample index".into()))?;
            Ok((value, sig.unit))
        };

        let results = if parallel_ok {
            run_indexed(runs, &run_one)?
        } else {
            (0..runs).map(&run_one).collect::<Result<Vec<_>, _>>()?
        };
        let unit = results.first().map(|&(_, u)| u).unwrap_or(Unit::Volt);
        let samples: Vec<f64> = results.into_iter().map(|(v, _)| v).collect();

        Ok(vec![Signal::time(
            format!("mc(n{})", circuit.resolve(output).0),
            unit,
            (0..runs).map(|i| i as f64).collect(),
            samples,
        )])
    }
}

// ---------------------------------------------------------------------------
// Parallel execution of independent analyses (CP-SIM-M4.3)
// ---------------------------------------------------------------------------

/// Run `n` independent, index-addressed jobs and collect their results **in index
/// order** — the executor behind parallel Monte-Carlo and sweeps (`CP-SIM-M4.3`).
///
/// With the native `parallel` feature on (and off wasm) the index range is split
/// into contiguous per-thread chunks run on OS threads via [`std::thread::scope`]
/// (no dependencies); otherwise it is a plain sequential loop. Either way the
/// output is assembled strictly by job index and each job is a pure function of
/// its index, so the result is **identical regardless of thread scheduling** — the
/// determinism the gate requires. The first error in index order is returned.
#[cfg(all(feature = "parallel", not(target_arch = "wasm32")))]
fn run_indexed<T, F>(n: usize, f: &F) -> Result<Vec<T>, SimError>
where
    T: Send,
    F: Fn(usize) -> Result<T, SimError> + Sync,
{
    let threads = parallel_threads(n);
    if threads <= 1 {
        return (0..n).map(f).collect();
    }
    let chunk = n.div_ceil(threads);
    // Each thread owns a contiguous index range and returns its results in order.
    let per_thread: Vec<Result<Vec<T>, SimError>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let start = (t * chunk).min(n);
                let end = ((t + 1) * chunk).min(n);
                scope.spawn(move || (start..end).map(f).collect::<Result<Vec<T>, SimError>>())
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("a parallel analysis thread panicked"))
            .collect()
    });
    // Concatenate the chunks in thread order (= ascending index order).
    let mut out = Vec::with_capacity(n);
    for chunk in per_thread {
        out.extend(chunk?);
    }
    Ok(out)
}

/// Sequential executor — the default build, and any wasm / feature-off build. Same
/// signature and semantics as the threaded [`run_indexed`], so callers are
/// identical; the bounds match it too (harmless here) to keep one call site.
#[cfg(not(all(feature = "parallel", not(target_arch = "wasm32"))))]
fn run_indexed<T, F>(n: usize, f: &F) -> Result<Vec<T>, SimError>
where
    T: Send,
    F: Fn(usize) -> Result<T, SimError> + Sync,
{
    (0..n).map(f).collect()
}

/// The number of worker threads to use for `n` independent jobs: the hardware
/// parallelism, capped at `n` (never more threads than jobs) and at least 1.
#[cfg(all(feature = "parallel", not(target_arch = "wasm32")))]
fn parallel_threads(n: usize) -> usize {
    std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(1)
        .min(n)
        .max(1)
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

// ---------------------------------------------------------------------------
// DC sweep + parametric sweep (CP-SIM-M1.5)
// ---------------------------------------------------------------------------

/// The unit a sweep must drive its target source with, and the guard that the
/// target exists and is an independent source we can retarget.
fn sweep_source_unit(circuit: &Circuit, source: &str) -> Result<Unit, SimError> {
    match circuit.part_by_ref(source) {
        Some(p) if matches!(p.kind, PartKind::VoltageSource) => Ok(Unit::Volt),
        Some(p) if matches!(p.kind, PartKind::CurrentSource) => Ok(Unit::Ampere),
        Some(_) => Err(SimError::Unsupported(
            "sweep target is not an independent source",
        )),
        None => Err(SimError::UnknownPart(source.to_string())),
    }
}

/// The sample points a sweep visits: `start` to `stop` inclusive in steps of
/// `step`. `step` must be finite, non-zero, and point from `start` toward `stop`.
/// Endpoints are included — the count is nudged by a hair so float drift cannot
/// drop the final sample — and a zero span yields the single point `[start]`.
fn sweep_values(start: f64, stop: f64, step: f64) -> Result<Vec<f64>, SimError> {
    if !start.is_finite() || !stop.is_finite() || !step.is_finite() || step == 0.0 {
        return Err(SimError::BadValue("DC sweep range/step".into()));
    }
    let span = stop - start;
    if span != 0.0 && span.signum() != step.signum() {
        return Err(SimError::BadValue(
            "DC sweep step points the wrong way".into(),
        ));
    }
    let count = (span / step + 1e-9).floor() as usize; // number of whole steps
    Ok((0..=count).map(|i| start + i as f64 * step).collect())
}

/// `Analysis::DcSweep`: step an independent source's level from `start` to `stop`
/// by `step`, solve the DC operating point (convergence aids and all) at every
/// sample, and return one [`Signal`] per node — the node voltage as a function of
/// the swept level, with the swept level on the `x` axis.
///
/// The swept source is driven as a plain DC source at each level (any waveform it
/// carried is dropped for the sweep, so the stepped value always takes effect).
/// The topology never changes across the sweep, so the MNA layout is built once.
fn dc_sweep(
    circuit: &Circuit,
    source: &str,
    start: f64,
    stop: f64,
    step: f64,
    registry: &DeviceRegistry,
) -> Result<Vec<Signal>, SimError> {
    let unit = sweep_source_unit(circuit, source)?;
    let values = sweep_values(start, stop, step)?;

    // The topology is fixed across the sweep, so build the base circuit (any
    // waveform dropped) and its MNA layout once; each point only retargets the
    // swept source's level. The points are independent operating-point solves, so
    // the native `parallel` feature fans them across threads (`CP-SIM-M4.3`); the
    // results are assembled in sweep order either way.
    let base = {
        let mut w = circuit.clone();
        w.set_waveform(source, Waveform::Dc); // sweep the DC level, ignore any waveform
        w
    };
    let layout = Layout::build(&base);
    let n = layout.n_nodes();
    let solve_point = |k: usize| -> Result<Vec<f64>, SimError> {
        let mut work = base.clone();
        work.set_value(source, Value::new(values[k], unit));
        match solve_dc(&work, &layout, registry) {
            // A node that floats at DC (a capacitive divider's mid-node, an AC-coupled
            // subnode) leaves the operating point singular. Condition it with a residual
            // SPICE-style GMIN and re-solve, so the sweep returns a gmin-pinned trace for
            // that node instead of erroring. Only the point that would otherwise error
            // takes this path, so any sweep that already solved is bit-for-bit unchanged.
            Err(SimError::Singular) => {
                solve_dc_conditioned(&work, &layout, registry, GMIN_RESIDUAL)
            }
            other => other,
        }
    };
    let points = run_indexed(values.len(), &solve_point)?;

    let mut traces: Vec<Vec<f64>> = vec![Vec::with_capacity(values.len()); n];
    for x in &points {
        for (d, trace) in traces.iter_mut().enumerate() {
            trace.push(x[d]);
        }
    }

    Ok(traces
        .into_iter()
        .enumerate()
        .map(|(d, y)| {
            Signal::time(
                format!("v(n{})", layout.inv[d]),
                Unit::Volt,
                values.clone(),
                y,
            )
        })
        .collect())
}

impl Engine {
    /// `Analysis::Parametric`: run the boxed `inner` analysis once per value in
    /// `values`, retargeting `source`'s DC level to each value first. Every signal
    /// the inner run returns is tagged with the swept value (`"v(n2) @ V1=3"`) so
    /// the families stay distinguishable, and the runs are concatenated in order.
    ///
    /// The inner analysis flows back through [`Engine::run`], so it may be any
    /// analysis — an operating point, a transient, an AC sweep, even a nested
    /// sweep. As in [`dc_sweep`], the swept source is driven as a DC source at the
    /// swept level.
    fn parametric(
        &self,
        circuit: &Circuit,
        source: &str,
        values: &[f64],
        inner: &Analysis,
    ) -> Result<Vec<Signal>, SimError> {
        // Validate the target up front so an empty `values` list still rejects a typo.
        let unit = sweep_source_unit(circuit, source)?;

        let base = {
            let mut w = circuit.clone();
            w.set_waveform(source, Waveform::Dc);
            w
        };
        // Each value's inner run is independent, so the native `parallel` feature
        // fans them across threads (`CP-SIM-M4.3`); the tagged signal families are
        // concatenated in value order, matching the sequential result exactly.
        let one = |k: usize| -> Result<Vec<Signal>, SimError> {
            let val = values[k];
            let mut work = base.clone();
            work.set_value(source, Value::new(val, unit));
            let sigs = self.run(&work, inner)?;
            Ok(sigs
                .into_iter()
                .map(|mut sig| {
                    sig.name = format!("{} @ {source}={val}", sig.name);
                    sig
                })
                .collect())
        };
        let groups = run_indexed(values.len(), &one)?;
        Ok(groups.into_iter().flatten().collect())
    }
}

/// Internal hooks for benchmarks and solver-equivalence tests — **not** part of
/// the stable API (hidden from the docs, no semver promise). They assemble the
/// linear DC MNA system for a circuit and solve it with a *forced* dense or
/// sparse path, so a bench can race the two solvers on one identical matrix at a
/// size where [`Engine::run`] would otherwise auto-pick sparse. (`CP-SIM-M2.4`.)
#[doc(hidden)]
pub mod bench_support {
    use super::*;

    /// Assemble the linear DC system (resistors, independent and controlled
    /// sources, inductor shorts) for `circuit` into a system built by `make` (so
    /// the backing — dense or sparse — is forced) — the identical stamp
    /// [`solve_dc`] uses, minus the nonlinear Newton loop (so it is exact for a
    /// purely linear network such as a resistor ladder). Panics on a malformed
    /// circuit; this is bench / test setup, never a shipped path.
    fn linear_dc_system(circuit: &Circuit, make: fn(usize) -> System) -> System {
        let layout = Layout::build(circuit);
        let size = layout.n_nodes()
            + layout.vsources.len()
            + layout.n_controlled_branches()
            + layout.inductors.len();
        let mut sys = make(size);
        stamp_resistors(&mut sys, circuit, &layout).expect("resistor stamp");
        stamp_vsources(&mut sys, circuit, &layout, None, 1.0).expect("vsource stamp");
        stamp_current_sources(&mut sys, circuit, &layout, None, 1.0).expect("current stamp");
        stamp_controlled_sources(&mut sys, circuit, &layout).expect("controlled stamp");
        stamp_inductor_shorts(&mut sys, circuit, &layout).expect("inductor stamp");
        sys
    }

    /// Solve `circuit`'s linear DC operating point with the forced dense backing.
    pub fn dc_solve_dense(circuit: &Circuit) -> Option<Vec<f64>> {
        linear_dc_system(circuit, System::dense).solve()
    }

    /// Solve `circuit`'s linear DC operating point with the forced sparse backing.
    pub fn dc_solve_sparse(circuit: &Circuit) -> Option<Vec<f64>> {
        linear_dc_system(circuit, System::sparse).solve()
    }

    // --- CP-SIM-M10.1: reusable ordered complex AC/noise solver ------------

    pub use crate::mna_complex::{ComplexNumeric, ComplexSparseMatrix, ComplexSymbolic};

    /// A swept-AC solve of `circuit` that **forces the per-frequency dense complex
    /// solve** at every point (the exact pre-`CP-SIM-M10.1` behavior), returning the
    /// node-voltage phasors as `per_node[node][freq_index]`. Used both as the
    /// reference the ordered reusable path is checked against (`swept_ac_ordered_…`
    /// test) and as the dense baseline in the `ac_scaling` bench.
    pub fn ac_sweep_dense(
        circuit: &Circuit,
        f_start: f64,
        f_stop: f64,
        ppd: u32,
    ) -> Option<Vec<Vec<Complex>>> {
        ac_sweep(circuit, f_start, f_stop, ppd, false)
    }

    /// A swept-AC solve of `circuit` that routes through the **reusable ordered
    /// complex LU** (`ComplexReusableSolver`): factor the structure once, then
    /// refactor and back-solve per frequency. Returns the same `per_node[node][freq]`
    /// phasors as [`ac_sweep_dense`], to a few ULP.
    pub fn ac_sweep_ordered(
        circuit: &Circuit,
        f_start: f64,
        f_stop: f64,
        ppd: u32,
    ) -> Option<Vec<Vec<Complex>>> {
        ac_sweep(circuit, f_start, f_stop, ppd, true)
    }

    fn ac_sweep(
        circuit: &Circuit,
        f_start: f64,
        f_stop: f64,
        ppd: u32,
        ordered: bool,
    ) -> Option<Vec<Vec<Complex>>> {
        let layout = Layout::build(circuit);
        let reg = DeviceRegistry::default();
        let net = AcNetwork::build(circuit, &layout, &reg).ok()?;
        let freqs = log_frequencies(f_start, f_stop, ppd);
        let mut per_node: Vec<Vec<Complex>> = vec![Vec::with_capacity(freqs.len()); net.n];
        let mut solver = ComplexReusableSolver::new(if ordered { net.size } else { 0 });
        for &f in &freqs {
            let w = TAU * f;
            let mut sys = if ordered {
                net.assemble(w).ok()?
            } else {
                net.assemble_dense(w).ok()?
            };
            for &k in &net.ac_drive_branches {
                sys.stamp_current(Some(k), None, Complex::ONE);
            }
            for &(a, b) in &net.ac_drive_csources {
                sys.stamp_current(b, a, Complex::ONE);
            }
            let x = if ordered {
                solver.solve(sys)?
            } else {
                sys.solve()?
            };
            for (d, col) in per_node.iter_mut().enumerate() {
                col.push(x[d]);
            }
        }
        Some(per_node)
    }

    /// Solve `circuit`'s linear DC operating point through the fill-reducing ordered
    /// KLU path — the route [`Engine::run`] takes above the engine's `KLU_THRESHOLD`
    /// (`CP-SIM-M5.1`). Assembles the identical system [`dc_solve_sparse`] does, then
    /// analyzes + factors + solves it ordered, so a bench can race the two on one
    /// matrix at any size. (A single solve; the symbolic-reuse win is `refactor_reuse`.)
    pub fn dc_solve_ordered(circuit: &Circuit) -> Option<Vec<f64>> {
        let (mat, rhs) = linear_dc_system(circuit, System::sparse).into_ordered_parts();
        let sym = Symbolic::analyze(&mat);
        Some(sym.factor(&mat)?.solve(&rhs))
    }

    // --- CP-SIM-M4.2: fill-reducing ordered reusable sparse LU -------------

    pub use crate::mna::{Numeric, SparseMatrix, Symbolic};

    /// Build the `CP-SIM-M4.2` "arrow" benchmark matrix: a hub (node 0) joined by a
    /// conductance to each of `leaves` leaf nodes, each leaf also tied to ground,
    /// with a probe current injected at the hub — returned as a [`SparseMatrix`]
    /// plus its right-hand side. The hub is numbered first, so natural-order
    /// elimination fills the whole leaf block (`O(leaves²)`) while the fill-reducing
    /// order eliminates it last (`O(leaves)`): the scaling the bench measures.
    pub fn arrow_matrix(leaves: usize) -> (SparseMatrix, Vec<f64>) {
        let size = leaves + 1;
        let g = 1e-3;
        let mut m = SparseMatrix::new(size);
        for leaf in 1..=leaves {
            m.stamp_conductance(Some(0), Some(leaf), g);
            m.stamp_conductance(Some(leaf), None, g);
        }
        let mut rhs = vec![0.0; size];
        rhs[0] = 1e-3;
        (m, rhs)
    }

    /// Solve the arrow via **today's natural-order sparse path** (`System::sparse`)
    /// — the baseline the ordered solver is raced against.
    pub fn arrow_natural_solve(leaves: usize) -> Option<Vec<f64>> {
        let size = leaves + 1;
        let g = 1e-3;
        let mut s = System::sparse(size);
        for leaf in 1..=leaves {
            s.stamp_conductance(Some(0), Some(leaf), g);
            s.stamp_conductance(Some(leaf), None, g);
        }
        s.stamp_current(Some(0), None, 1e-3);
        s.solve()
    }

    /// Solve the arrow via the fill-reducing ordered LU: analyze + factor + solve.
    pub fn arrow_ordered_solve(leaves: usize) -> Option<Vec<f64>> {
        let (m, rhs) = arrow_matrix(leaves);
        let sym = Symbolic::analyze(&m);
        Some(sym.factor(&m)?.solve(&rhs))
    }

    // --- CP-SIM-M4.3: parallel Monte-Carlo ---------------------------------

    /// The Monte-Carlo speedup fixture: a 5 V source into a ±5 %-toleranced 1 kΩ
    /// resistor feeding a diode to ground. Each run is a nonlinear Newton solve, so
    /// the per-run work is real (not swamped by the circuit clone) and the thread
    /// fan-out shows a clear speedup.
    fn mc_fixture() -> (Circuit, NetId) {
        let mut c = Circuit::new("mc_bench");
        let vin = c.new_net();
        let mid = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            Value::parse("5", Unit::Volt).ok(),
            vec![vin, NetId::GROUND],
        );
        c.add_part(
            "R1",
            PartKind::Resistor,
            Value::parse("1k", Unit::Ohm).ok(),
            vec![vin, mid],
        );
        c.add_part(
            "D1",
            PartKind::Diode(DiodeModel::default()),
            None,
            vec![mid, NetId::GROUND],
        );
        c.set_tolerance("R1", 0.05);
        (c, mid)
    }

    /// Run the Monte-Carlo fixture `runs` times through the **forced-sequential**
    /// path — the `CP-SIM-M4.3` bench baseline.
    pub fn monte_carlo_sequential(runs: usize) -> Vec<Signal> {
        let (c, mid) = mc_fixture();
        Engine::new()
            .monte_carlo_impl(&c, 0xC0FFEE, runs, mid, 0, &Analysis::OperatingPoint, false)
            .expect("mc bench solves")
    }

    /// Run the Monte-Carlo fixture `runs` times through the public path — threaded
    /// when the `parallel` feature is on, so timing it against
    /// [`monte_carlo_sequential`] shows the speedup.
    pub fn monte_carlo_parallel(runs: usize) -> Vec<Signal> {
        let (c, mid) = mc_fixture();
        Engine::new()
            .monte_carlo_impl(&c, 0xC0FFEE, runs, mid, 0, &Analysis::OperatingPoint, true)
            .expect("mc bench solves")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ohmforge_core::value::Value;

    fn v(s: &str, u: Unit) -> Option<Value> {
        Some(Value::parse(s, u).unwrap())
    }

    // Built-ins-only shims: the engine-internal `solve_dc` / `collect_nonlinear`
    // now take a `&DeviceRegistry` (`CP-SIM-M4.1`), but every pre-M4 test exercises
    // only built-in devices. These local wrappers (which shadow the glob-imported
    // originals) forward an empty registry, so the existing call sites stay
    // unchanged and keep their exact behavior. Plugin devices are covered by
    // `plugin_square_law_device_matches_hand_analysis`, which drives a real
    // `Engine` registry instead.
    fn solve_dc(circuit: &Circuit, layout: &Layout) -> Result<Vec<f64>, SimError> {
        super::solve_dc(circuit, layout, &DeviceRegistry::new())
    }
    fn collect_nonlinear(circuit: &Circuit) -> Result<Vec<Box<dyn NonlinearDevice>>, SimError> {
        super::collect_nonlinear(circuit, &DeviceRegistry::new())
    }
    // --- Convergence aids — CP-SIM-M1.3 ------------------------------------

    /// Build the plain-NR DC stamp and solve it with a cold start, no gmin, full
    /// sources — i.e. the fast path alone, bypassing the convergence aids. Used by
    /// the gate test to show a stiff circuit the aids are needed for.
    fn plain_nr_dc(c: &Circuit) -> Result<Vec<f64>, SimError> {
        let layout = Layout::build(c);
        let size = layout.n_nodes() + layout.vsources.len() + layout.inductors.len();
        let devices = collect_nonlinear(c).unwrap();
        let mut solver = ReusableSolver::new(size);
        solve_nr(
            c,
            &layout,
            size,
            &devices,
            &|sys| {
                stamp_resistors(sys, c, &layout)?;
                stamp_vsources(sys, c, &layout, None, 1.0)?;
                stamp_current_sources(sys, c, &layout, None, 1.0)?;
                stamp_inductor_shorts(sys, c, &layout)?;
                Ok(())
            },
            None,
            0.0,
            &mut solver,
        )
    }

    /// The acceptance gate: a stiff netlist plain Newton–Raphson cannot crack, but
    /// the convergence aids can.
    ///
    /// A diode-connected NMOS (gate tied to drain) is fed only by an ideal current
    /// source — the channel is the drain node's *sole* DC path. At the cold-start
    /// guess (every node 0 V) the gate sits at 0 V ≤ Vth, so the channel is cut off
    /// with exactly zero transconductance: the drain row is all zeros and plain NR
    /// solves a singular system and fails. gmin stepping props the node up with a
    /// ramped conductance to ground, walks the bias onto the channel, and lands the
    /// true operating point once the ramp reaches gmin = 0.
    ///
    /// Diode-connected ⇒ always saturated, so with λ = 0 the forced current obeys
    /// I = K·(Vd − Vth)². With defaults K = 1 mA/V², Vth = 1 V and I = 1 mA that is
    /// Vd = 2.0 V exactly — the value the aided solve must return.
    #[test]
    fn stiff_mosfet_needs_convergence_aids() {
        let mut c = Circuit::new("isrc_mos");
        let d = c.new_net();
        c.add_part(
            "I1",
            PartKind::CurrentSource,
            v("1m", Unit::Ampere),
            vec![NetId::GROUND, d],
        );
        c.add_part(
            "M1",
            PartKind::Mosfet(Polarity::N, MosfetModel::default()),
            None,
            vec![d, d, NetId::GROUND],
        );

        // Plain NR alone cannot solve it (singular at the cold-start guess).
        assert!(
            plain_nr_dc(&c).is_err(),
            "plain NR unexpectedly solved the stiff netlist — the gate proves nothing"
        );

        // The aided operating point solves and matches the square-law prediction.
        let layout = Layout::build(&c);
        let x = solve_dc(&c, &layout).unwrap();
        let vd = x[layout.node_of(&c, d).unwrap()];
        assert!(
            (vd - 2.0).abs() <= 1e-6,
            "aided Vd = {vd} V, expected 2.0 V"
        );
        // KCL: the channel carries exactly the injected current at that bias.
        let m = MosfetModel::default();
        let id = m.kp * (vd - m.vth).powi(2);
        assert!(
            (id - 1e-3).abs() <= 1e-9,
            "channel current {id} A != injected 1 mA"
        );
    }

    /// Both aids, called directly, reach the diode–resistor operating point from a
    /// cold start — covering the gmin- and source-stepping continuations and
    /// showing each lands the same junction drop the plain solve does.
    #[test]
    fn both_aids_reach_the_diode_resistor_point() {
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
        c.add_part(
            "D1",
            PartKind::Diode(DiodeModel::default()),
            None,
            vec![mid, NetId::GROUND],
        );

        let layout = Layout::build(&c);
        let size = layout.n_nodes() + layout.vsources.len() + layout.inductors.len();
        let devices = collect_nonlinear(&c).unwrap();
        let stamp_at = |sys: &mut System, alpha: f64| -> Result<(), SimError> {
            stamp_resistors(sys, &c, &layout)?;
            stamp_vsources(sys, &c, &layout, None, alpha)?;
            stamp_current_sources(sys, &c, &layout, None, alpha)?;
            stamp_inductor_shorts(sys, &c, &layout)?;
            Ok(())
        };

        let reference = solve_dc(&c, &layout).unwrap()[layout.node_of(&c, mid).unwrap()];
        let g = gmin_stepping(&c, &layout, size, &devices, &stamp_at)
            .unwrap()
            .expect("gmin stepping converged");
        let s = source_stepping(&c, &layout, size, &devices, &stamp_at)
            .unwrap()
            .expect("source stepping converged");
        let gi = layout.node_of(&c, mid).unwrap();
        assert!(
            (g[gi] - reference).abs() <= 1e-9,
            "gmin: {} vs {reference}",
            g[gi]
        );
        assert!(
            (s[gi] - reference).abs() <= 1e-9,
            "source: {} vs {reference}",
            s[gi]
        );
    }

    /// A plain-convergent circuit must never pay for the aids: the diode–resistor
    /// bias solves on the plain-NR fast path alone, with no stepping.
    #[test]
    fn plain_convergent_circuit_skips_the_aids() {
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
        c.add_part(
            "D1",
            PartKind::Diode(DiodeModel::default()),
            None,
            vec![mid, NetId::GROUND],
        );
        // If plain NR already solves it, the aids are not exercised for this input.
        assert!(
            plain_nr_dc(&c).is_ok(),
            "fast path must crack a plain-convergent bias"
        );
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

    /// A lossless parallel LC tank, kicked by a brief current pulse, then left to
    /// ring. Node `tank` has `C` and `L` both to ground; the current source injects a
    /// short pulse to build energy. With no resistance the oscillation is ideal, so
    /// its amplitude should persist — and that is exactly what a second-order,
    /// energy-conserving integrator preserves and a first-order one does not.
    fn lc_tank() -> (Circuit, NetId) {
        let mut c = Circuit::new("lc_tank");
        let tank = c.new_net();
        c.add_part(
            "L1",
            PartKind::Inductor,
            Some(Value::new(1e-3, Unit::Henry)),
            vec![tank, NetId::GROUND],
        );
        c.add_part(
            "C1",
            PartKind::Capacitor,
            Some(Value::new(1e-6, Unit::Farad)),
            vec![tank, NetId::GROUND],
        );
        c.add_part(
            "I1",
            PartKind::CurrentSource,
            Some(Value::new(0.0, Unit::Ampere)),
            vec![NetId::GROUND, tank],
        );
        // A single 20 µs, 1 A kick (≈ T/10 of the 198.7 µs resonance), then quiet.
        assert!(c.set_waveform(
            "I1",
            Waveform::Pulse {
                v1: 0.0,
                v2: 1.0,
                delay: 0.0,
                width: 20e-6,
                period: 0.0,
            },
        ));
        (c, tank)
    }

    /// The peak absolute tank voltage over a time window `[t0, t1]` of a transient
    /// result — the ringing envelope the gate compares early vs late.
    fn peak_in_window(sigs: &[Signal], name: &str, t0: f64, t1: f64) -> f64 {
        let sig = sigs.iter().find(|s| s.name == name).expect("tank signal");
        let ys = sig.real().unwrap();
        sig.x
            .iter()
            .zip(ys)
            .filter(|(&t, _)| t >= t0 && t <= t1)
            .map(|(_, &y)| y.abs())
            .fold(0.0, f64::max)
    }

    /// `CP-SIM-M10.2` accuracy gate: an LC tank that Backward Euler over-damps rings
    /// on undiminished under the opt-in trapezoidal integrator. Both run the same
    /// fixed step; the trapezoidal late-window amplitude stays within a few percent of
    /// its early-window amplitude (energy conserved), while the Backward-Euler
    /// amplitude has collapsed to a small fraction of it (artificial damping).
    #[test]
    fn lc_tank_rings_under_trapezoidal_where_backward_euler_decays() {
        let (c, tank) = lc_tank();
        let name = format!("v(n{})", c.resolve(tank).0);
        let (h, duration) = (2e-6, 4e-3); // ~100 pts/period, ~20 periods
        let (t_early0, t_early1) = (0.5e-3, 1.5e-3);
        let (t_late0, t_late1) = (3.0e-3, 4.0e-3);

        // Default Backward Euler (via run_transient, == Analysis::Transient).
        let be = Engine::new()
            .run_transient(&c, duration, h, IntegrationMethod::BackwardEuler)
            .unwrap();
        let be_early = peak_in_window(&be, &name, t_early0, t_early1);
        let be_late = peak_in_window(&be, &name, t_late0, t_late1);
        assert!(be_early > 1.0, "BE kick did not excite the tank");
        assert!(
            be_late < 0.5 * be_early,
            "Backward Euler should over-damp: early {be_early} V, late {be_late} V"
        );

        // Opt-in trapezoidal: the oscillation persists.
        let tr = Engine::new()
            .run_transient(&c, duration, h, IntegrationMethod::Trapezoidal)
            .unwrap();
        let tr_early = peak_in_window(&tr, &name, t_early0, t_early1);
        let tr_late = peak_in_window(&tr, &name, t_late0, t_late1);
        assert!(
            tr_late > 0.95 * tr_early,
            "trapezoidal should sustain the ring: early {tr_early} V, late {tr_late} V"
        );
    }

    /// `CP-SIM-M10.2` opt-in-default proof: selecting the default
    /// [`IntegrationMethod::BackwardEuler`] through the new `run_transient` surface
    /// yields a result **byte-for-byte identical** to the pre-M10 fixed-step
    /// `Analysis::Transient` path — so the opt-in never perturbs the default, on a
    /// representative RLC that exercises both a capacitor and an inductor companion.
    #[test]
    fn run_transient_backward_euler_matches_analysis_transient_bit_for_bit() {
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
        let (duration, h) = (3e-4, 2e-7);
        let viaanalysis = Engine::new()
            .run(
                &c,
                &Analysis::Transient {
                    duration,
                    step: Some(h),
                },
            )
            .unwrap();
        let viamethod = Engine::new()
            .run_transient(&c, duration, h, IntegrationMethod::BackwardEuler)
            .unwrap();
        assert_eq!(
            viaanalysis, viamethod,
            "run_transient(BackwardEuler) must equal Analysis::Transient bit-for-bit"
        );
    }

    /// Trapezoidal integration requires a fixed timestep — an adaptive request (no
    /// step) is rejected cleanly rather than silently mis-controlled.
    #[test]
    fn trapezoidal_needs_a_fixed_step() {
        let (c, _tank) = lc_tank();
        let layout = Layout::build(&c);
        let err = transient(
            &c,
            &layout,
            1e-3,
            None,
            &DeviceRegistry::default(),
            IntegrationMethod::Trapezoidal,
        )
        .unwrap_err();
        assert!(matches!(err, SimError::Unsupported(_)));
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
            c.add_part(
                "D1",
                PartKind::Diode(DiodeModel::default()),
                None,
                vec![node, NetId::GROUND],
            );

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

    /// `CP-SIM-M5.2` gate: the emission coefficient `n` and the series resistance
    /// `R_s` both reach the device and shift the I–V curve exactly as the model
    /// predicts. Forcing a known current `I` through the diode, the terminal voltage
    /// must be the junction drop `n·Vt·ln(I/Is + 1)` *plus* the ohmic `I·R_s` — and a
    /// default diode (`n = 1`, `R_s = 0`) must still land on the plain Shockley drop.
    #[test]
    fn diode_model_n_and_rs_shift_the_iv_curve() {
        let is = 1e-14;
        let vt = 0.025_852;
        // (n, Rs): an ideal default, a high-emission junction, a series-resistive
        // one, and one with both effects.
        for &(n, rs) in &[(1.0, 0.0), (2.0, 0.0), (1.0, 50.0), (1.8, 100.0)] {
            for &i in &[1e-6, 1e-3, 1e-2_f64] {
                let mut c = Circuit::new("diode_model");
                let node = c.new_net();
                c.add_part(
                    "I1",
                    PartKind::CurrentSource,
                    v(&format!("{i}"), Unit::Ampere),
                    vec![NetId::GROUND, node],
                );
                c.add_part(
                    "D1",
                    PartKind::Diode(DiodeModel { is, n, rs }),
                    None,
                    vec![node, NetId::GROUND],
                );
                let layout = Layout::build(&c);
                let x = solve_dc(&c, &layout).unwrap();
                let vd = x[layout.node_of(&c, node).unwrap()];

                // Terminal voltage = junction drop (n·Vt·ln(I/Is+1)) + ohmic I·Rs.
                let expected = n * vt * (i / is + 1.0).ln() + i * rs;
                assert!(
                    (vd - expected).abs() <= 1e-6 + 1e-6 * expected.abs(),
                    "n={n}, Rs={rs}, I={i}: vd={vd} V, expected {expected} V"
                );
            }
        }

        // Explicit "default is unchanged": the default model reproduces the plain
        // Shockley junction (no series drop) to the same tolerance diode_iv holds.
        let mut c = Circuit::new("default_diode");
        let node = c.new_net();
        c.add_part(
            "I1",
            PartKind::CurrentSource,
            v("1m", Unit::Ampere),
            vec![NetId::GROUND, node],
        );
        c.add_part(
            "D1",
            PartKind::Diode(DiodeModel::default()),
            None,
            vec![node, NetId::GROUND],
        );
        let layout = Layout::build(&c);
        let vd = solve_dc(&c, &layout).unwrap()[layout.node_of(&c, node).unwrap()];
        assert!(
            (vd - VTE * (1e-3 / IS + 1.0).ln()).abs() <= 1e-7,
            "default diode drifted from the plain Shockley drop: {vd} V"
        );
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
        c.add_part(
            "D1",
            PartKind::Diode(DiodeModel::default()),
            None,
            vec![mid, NetId::GROUND],
        );

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
        c.add_part(
            "D1",
            PartKind::Diode(DiodeModel::default()),
            None,
            vec![node, NetId::GROUND],
        );

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

    /// A long RC ladder whose MNA system clears [`COMPLEX_KLU_THRESHOLD`] (so
    /// `Engine::run(Ac)` takes the reusable ordered complex path): an AC-driven source
    /// at node 0, then `rungs` sections of a series R into a rung node with a shunt C
    /// to ground. `rungs + 1` nodes + one source branch — a representative swept-AC
    /// network large enough to exercise the ordered path. Returns the circuit and its
    /// output net (the last rung).
    fn ac_rc_ladder(rungs: usize) -> (Circuit, NetId) {
        let mut c = Circuit::new("ac_ladder");
        let n0 = c.new_net();
        c.add_part("V1", PartKind::VoltageSource, None, vec![n0, NetId::GROUND]);
        assert!(c.set_waveform(
            "V1",
            Waveform::Sine {
                amplitude: 1.0,
                freq: 1_000.0,
                offset: 0.0,
                phase_deg: 0.0,
            },
        ));
        let mut prev = n0;
        let mut last = n0;
        for i in 0..rungs {
            let node = c.new_net();
            c.add_part(
                format!("R{i}"),
                PartKind::Resistor,
                v("1k", Unit::Ohm),
                vec![prev, node],
            );
            c.add_part(
                format!("C{i}"),
                PartKind::Capacitor,
                Some(Value::new(10e-9, Unit::Farad)),
                vec![node, NetId::GROUND],
            );
            prev = node;
            last = node;
        }
        (c, last)
    }

    /// `CP-SIM-M10.1` AC gate: a swept AC on a ladder past the ordered threshold,
    /// solved through `Engine::run` (the reusable ordered complex factorization),
    /// equals the per-frequency **dense** complex solve at every node and every swept
    /// frequency to ≤1e-12 — the ordered LU is the same factorization merely
    /// reordered. Proves the reuse path is correct where it engages.
    #[test]
    fn swept_ac_ordered_matches_dense() {
        let (c, _out) = ac_rc_ladder(300); // MNA size 302 ≥ COMPLEX_KLU_THRESHOLD (256)
        let (f_start, f_stop, ppd) = (10.0, 1e6, 10);

        // The engine path (auto-selects the ordered reusable solver at this size).
        let sigs = Engine::new()
            .run(
                &c,
                &Analysis::Ac {
                    f_start,
                    f_stop,
                    points_per_decade: ppd,
                },
            )
            .unwrap();

        // The forced per-frequency dense reference.
        let dense = bench_support::ac_sweep_dense(&c, f_start, f_stop, ppd).unwrap();

        let mut worst = 0.0_f64;
        for (node, sig) in sigs.iter().enumerate() {
            let engine = sig.complex().unwrap();
            assert_eq!(engine.len(), dense[node].len(), "freq count mismatch");
            for (a, b) in engine.iter().zip(&dense[node]) {
                worst = worst.max((*a - *b).magnitude());
            }
        }
        assert!(
            worst <= 1e-12,
            "ordered swept-AC diverged from the dense solve: max |Δ| = {worst}"
        );
    }

    /// `CP-SIM-M10.1` bench_support sanity: the ordered swept-AC helper and the dense
    /// helper agree to ≤1e-12 (the same assertion the bench relies on to race a
    /// like-for-like result), and the ordered path is actually engaged at this size.
    #[test]
    fn ac_sweep_helpers_agree_ordered_vs_dense() {
        let (c, _out) = ac_rc_ladder(260);
        let dense = bench_support::ac_sweep_dense(&c, 100.0, 1e5, 8).unwrap();
        let ordered = bench_support::ac_sweep_ordered(&c, 100.0, 1e5, 8).unwrap();
        let mut worst = 0.0_f64;
        for (dn, on) in dense.iter().zip(&ordered) {
            for (a, b) in dn.iter().zip(on) {
                worst = worst.max((*a - *b).magnitude());
            }
        }
        assert!(worst <= 1e-12, "ac_sweep helpers diverged: {worst}");
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

    // --- BJT (Ebers–Moll) — CP-SIM-M1.2 ------------------------------------

    #[test]
    fn bjt_common_emitter_bias() {
        // CE stage (the acceptance gate): Vcc = 10 V, base fed through RB = 1 MΩ,
        // collector load RC = 4.7 kΩ, emitter grounded. NPN Q1 = [C, B, E]. Hand
        // analysis: Vbe lands in the silicon window, Ib = (Vcc − Vbe)/RB,
        // Ic = βF·Ib, and Vc = Vcc − Ic·RC sits mid-rail (forward-active).
        let mut c = Circuit::new("ce");
        let vcc = c.new_net();
        let base = c.new_net();
        let coll = c.new_net();
        c.add_part(
            "Vcc",
            PartKind::VoltageSource,
            v("10", Unit::Volt),
            vec![vcc, NetId::GROUND],
        );
        c.add_part(
            "RB",
            PartKind::Resistor,
            v("1meg", Unit::Ohm),
            vec![vcc, base],
        );
        c.add_part(
            "RC",
            PartKind::Resistor,
            v("4.7k", Unit::Ohm),
            vec![vcc, coll],
        );
        c.add_part(
            "Q1",
            PartKind::Bjt(Polarity::N, BjtModel::default()),
            None,
            vec![coll, base, NetId::GROUND],
        );

        let layout = Layout::build(&c);
        let x = solve_dc(&c, &layout).unwrap();
        let vb = x[layout.node_of(&c, base).unwrap()];
        let vc = x[layout.node_of(&c, coll).unwrap()];

        // Vbe = Vb (emitter at ground) in the classic 0.6–0.7 V window.
        assert!(
            (0.6..0.7).contains(&vb),
            "Vbe = {vb} V out of the silicon window"
        );
        // Collector sits between the rails: forward-active, not saturated or off.
        assert!(vc > vb && vc < 10.0, "Vc = {vc} V not in the active range");

        let m = BjtModel::default();
        let ib = (10.0 - vb) / 1e6; // resistor current into the base
        let ic = (10.0 - vc) / 4.7e3; // resistor current into the collector
                                      // Current gain: Ic ≈ βF·Ib within 2%.
        assert!(
            (ic / ib - m.bf).abs() / m.bf <= 0.02,
            "Ic/Ib = {} vs βF = {}",
            ic / ib,
            m.bf
        );
        // KCL at the collector: the transistor's own Ebers–Moll collector current
        // equals the resistor current it draws. (Vbe = Vb, Vbc = Vb − Vc.)
        let (vbe, vbc) = (vb, vb - vc);
        let it = m.is * ((vbe / m.vt).exp() - (vbc / m.vt).exp());
        let ibc = m.is / m.br * ((vbc / m.vt).exp() - 1.0);
        let ic_model = it - ibc;
        assert!(
            (ic_model - ic).abs() <= 1e-9 + 1e-3 * ic,
            "Ic(model) = {ic_model} vs Ic(R) = {ic}"
        );
    }

    #[test]
    fn bjt_active_region_relations() {
        // Force the base current with an ideal source (Ib = 10 µA into the base)
        // and read the collector back: an NPN in forward active must obey both
        // Ic ≈ βF·Ib and Ic ≈ Is·exp(Vbe/Vt). Vcc = 5 V through RC = 1 kΩ keeps the
        // collector above the base (active).
        let ib = 10e-6;
        let mut c = Circuit::new("bjt_active");
        let vcc = c.new_net();
        let base = c.new_net();
        let coll = c.new_net();
        c.add_part(
            "Vcc",
            PartKind::VoltageSource,
            v("5", Unit::Volt),
            vec![vcc, NetId::GROUND],
        );
        c.add_part(
            "RC",
            PartKind::Resistor,
            v("1k", Unit::Ohm),
            vec![vcc, coll],
        );
        // Ib injected from ground into the base — the sole base-node path.
        c.add_part(
            "IB",
            PartKind::CurrentSource,
            v("10u", Unit::Ampere),
            vec![NetId::GROUND, base],
        );
        c.add_part(
            "Q1",
            PartKind::Bjt(Polarity::N, BjtModel::default()),
            None,
            vec![coll, base, NetId::GROUND],
        );

        let layout = Layout::build(&c);
        let x = solve_dc(&c, &layout).unwrap();
        let vb = x[layout.node_of(&c, base).unwrap()];
        let vc = x[layout.node_of(&c, coll).unwrap()];
        let m = BjtModel::default();

        let ic = (5.0 - vc) / 1e3; // collector current through RC
                                   // βF relation.
        assert!(
            (ic / ib - m.bf).abs() / m.bf <= 0.02,
            "Ic/Ib = {} vs βF = {}",
            ic / ib,
            m.bf
        );
        // Exponential law: Ic ≈ Is·exp(Vbe/Vt), Vbe = Vb (emitter grounded).
        let ic_exp = m.is * (vb / m.vt).exp();
        assert!(
            (ic_exp - ic).abs() / ic <= 0.02,
            "Is·exp(Vbe/Vt) = {ic_exp} vs Ic = {ic} (Vbe = {vb})"
        );
    }

    #[test]
    fn pnp_mirrors_npn_active_region() {
        // PNP validates the polarity fold: emitter at +5 V, collector load RC to
        // ground, base current pulled out by a source (|Ib| = 10 µA). Forward
        // active ⇒ Veb in the silicon window and |Ic| ≈ βF·|Ib|.
        let ib = 10e-6;
        let mut c = Circuit::new("pnp_active");
        let vee = c.new_net(); // emitter rail (+5 V)
        let base = c.new_net();
        let coll = c.new_net();
        c.add_part(
            "Vee",
            PartKind::VoltageSource,
            v("5", Unit::Volt),
            vec![vee, NetId::GROUND],
        );
        c.add_part(
            "RC",
            PartKind::Resistor,
            v("1k", Unit::Ohm),
            vec![coll, NetId::GROUND],
        );
        // Pull Ib out of the base (base → ground), the sole base-node path.
        c.add_part(
            "IB",
            PartKind::CurrentSource,
            v("10u", Unit::Ampere),
            vec![base, NetId::GROUND],
        );
        c.add_part(
            "Q1",
            PartKind::Bjt(Polarity::P, BjtModel::default()),
            None,
            vec![coll, base, vee],
        );

        let layout = Layout::build(&c);
        let x = solve_dc(&c, &layout).unwrap();
        let vb = x[layout.node_of(&c, base).unwrap()];
        let vc = x[layout.node_of(&c, coll).unwrap()];

        let veb = 5.0 - vb; // emitter at +5 V
        assert!((0.6..0.7).contains(&veb), "Veb = {veb} V out of the window");
        assert!(vc < vb, "Vc = {vc} not below Vb = {vb} — PNP not active");
        let ic = vc / 1e3; // transistor sources this into the collector node
        let m = BjtModel::default();
        assert!(
            (ic / ib - m.bf).abs() / m.bf <= 0.02,
            "|Ic|/|Ib| = {} vs βF = {}",
            ic / ib,
            m.bf
        );
    }

    // --- MOSFET (level-1 square law) — CP-SIM-M1.2 -------------------------

    #[test]
    fn mosfet_saturation_square_law() {
        // NMOS common-source: Vdd = 5 V through RD = 200 Ω, gate held at 3 V,
        // source grounded. With Vth = 1 V, K = 1 mA/V², λ = 0.02 and Vgs = 3 V the
        // device is in saturation (Vds > Vov), where Id = K·(Vgs−Vth)²·(1+λ·Vds).
        let model = MosfetModel {
            vth: 1.0,
            kp: 1e-3,
            lambda: 0.02,
        };
        let mut c = Circuit::new("nmos_sat");
        let vdd = c.new_net();
        let gate = c.new_net();
        let drain = c.new_net();
        c.add_part(
            "Vdd",
            PartKind::VoltageSource,
            v("5", Unit::Volt),
            vec![vdd, NetId::GROUND],
        );
        c.add_part(
            "Vg",
            PartKind::VoltageSource,
            v("3", Unit::Volt),
            vec![gate, NetId::GROUND],
        );
        c.add_part(
            "RD",
            PartKind::Resistor,
            v("200", Unit::Ohm),
            vec![vdd, drain],
        );
        c.add_part(
            "M1",
            PartKind::Mosfet(Polarity::N, model),
            None,
            vec![drain, gate, NetId::GROUND],
        );

        let layout = Layout::build(&c);
        let x = solve_dc(&c, &layout).unwrap();
        let vd = x[layout.node_of(&c, drain).unwrap()];

        let (vgs, vds) = (3.0, vd); // source grounded
        let vov = vgs - model.vth;
        assert!(vds > vov, "Vds = {vds} not in saturation (Vov = {vov})");
        let id = (5.0 - vd) / 200.0; // drain current through RD
        let id_model = model.kp * vov * vov * (1.0 + model.lambda * vds);
        assert!(
            (id - id_model).abs() <= 1e-9 + 1e-3 * id_model,
            "Id = {id} vs square-law {id_model}"
        );
    }

    #[test]
    fn nmos_diode_connected_bias() {
        // Diode-connected NMOS (gate tied to drain) with RD = 1 kΩ to Vdd = 5 V,
        // source grounded. Gate = drain ⇒ always saturated, so the bias solves
        // (Vdd − Vd)/RD = K·(Vd − Vth)². With defaults (Vth = 1, K = 1 mA/V²):
        // Vd² − Vd − 4 = 0 ⇒ Vd = (1 + √17)/2 ≈ 2.56 V.
        let mut c = Circuit::new("nmos_diode");
        let vdd = c.new_net();
        let drain = c.new_net();
        c.add_part(
            "Vdd",
            PartKind::VoltageSource,
            v("5", Unit::Volt),
            vec![vdd, NetId::GROUND],
        );
        c.add_part(
            "RD",
            PartKind::Resistor,
            v("1k", Unit::Ohm),
            vec![vdd, drain],
        );
        // Gate and drain share `drain` → diode connection.
        c.add_part(
            "M1",
            PartKind::Mosfet(Polarity::N, MosfetModel::default()),
            None,
            vec![drain, drain, NetId::GROUND],
        );

        let layout = Layout::build(&c);
        let x = solve_dc(&c, &layout).unwrap();
        let vd = x[layout.node_of(&c, drain).unwrap()];

        let expected = (1.0 + 17.0_f64.sqrt()) / 2.0; // ≈ 2.562 V
        assert!(
            (vd - expected).abs() <= 1e-3,
            "Vd = {vd} V, expected {expected} V"
        );
        // KCL check: resistor current equals the square-law channel current.
        let m = MosfetModel::default();
        let id = (5.0 - vd) / 1e3;
        let id_model = m.kp * (vd - m.vth).powi(2);
        assert!(
            (id - id_model).abs() <= 1e-9 + 1e-3 * id_model,
            "Id = {id} vs square-law {id_model}"
        );
    }

    #[test]
    fn nmos_cutoff_is_off() {
        // Gate below threshold (Vgs = 0.5 V < Vth = 1 V): the channel is cut off,
        // so Id ≈ 0 and the drain is pulled to the rail through RD.
        let mut c = Circuit::new("nmos_cutoff");
        let vdd = c.new_net();
        let gate = c.new_net();
        let drain = c.new_net();
        c.add_part(
            "Vdd",
            PartKind::VoltageSource,
            v("5", Unit::Volt),
            vec![vdd, NetId::GROUND],
        );
        c.add_part(
            "Vg",
            PartKind::VoltageSource,
            v("0.5", Unit::Volt),
            vec![gate, NetId::GROUND],
        );
        c.add_part(
            "RD",
            PartKind::Resistor,
            v("1k", Unit::Ohm),
            vec![vdd, drain],
        );
        c.add_part(
            "M1",
            PartKind::Mosfet(Polarity::N, MosfetModel::default()),
            None,
            vec![drain, gate, NetId::GROUND],
        );

        let layout = Layout::build(&c);
        let x = solve_dc(&c, &layout).unwrap();
        let vd = x[layout.node_of(&c, drain).unwrap()];
        let id = (5.0 - vd) / 1e3;
        assert!(id.abs() < 1e-9, "cutoff current {id} A not ~0");
        assert!(
            (vd - 5.0).abs() < 1e-6,
            "drain {vd} V not pulled to the rail"
        );
    }

    #[test]
    fn pmos_saturation_square_law() {
        // PMOS validates the polarity fold: source at +5 V, gate at 2 V, drain
        // through RD = 200 Ω to ground. Vsg = 3 V, Vov = 2 V, saturated. The
        // sourced current obeys Id = K·(Vsg−|Vth|)²·(1+λ·Vsd).
        let model = MosfetModel {
            vth: 1.0,
            kp: 1e-3,
            lambda: 0.02,
        };
        let mut c = Circuit::new("pmos_sat");
        let vdd = c.new_net();
        let gate = c.new_net();
        let drain = c.new_net();
        c.add_part(
            "Vdd",
            PartKind::VoltageSource,
            v("5", Unit::Volt),
            vec![vdd, NetId::GROUND],
        );
        c.add_part(
            "Vg",
            PartKind::VoltageSource,
            v("2", Unit::Volt),
            vec![gate, NetId::GROUND],
        );
        c.add_part(
            "RD",
            PartKind::Resistor,
            v("200", Unit::Ohm),
            vec![drain, NetId::GROUND],
        );
        c.add_part(
            "M1",
            PartKind::Mosfet(Polarity::P, model),
            None,
            vec![drain, gate, vdd],
        );

        let layout = Layout::build(&c);
        let x = solve_dc(&c, &layout).unwrap();
        let vd = x[layout.node_of(&c, drain).unwrap()];

        let (vsg, vsd) = (5.0 - 2.0, 5.0 - vd);
        let vov = vsg - model.vth;
        assert!(vsd > vov, "Vsd = {vsd} not in saturation (Vov = {vov})");
        let id = vd / 200.0; // sourced into the drain node, out through RD
        let id_model = model.kp * vov * vov * (1.0 + model.lambda * vsd);
        assert!(
            (id - id_model).abs() <= 1e-9 + 1e-3 * id_model,
            "Id = {id} vs square-law {id_model}"
        );
    }

    #[test]
    fn bjt_ce_small_signal_gain() {
        // AC validates the multiport small-signal stamp: a CE amp biased by a
        // base source (offset 0.65 V, 1∠0 AC), RC = 4.7 kΩ to Vcc, emitter
        // grounded. With no Early effect the midband gain is Av = −gm·RC, where
        // gm = Is·exp(Vbe/Vt)/Vt at the DC bias — the transconductance entry the
        // Jacobian contributes to the complex system.
        let mut c = Circuit::new("ce_ac");
        let vcc = c.new_net();
        let base = c.new_net();
        let coll = c.new_net();
        c.add_part(
            "Vcc",
            PartKind::VoltageSource,
            v("10", Unit::Volt),
            vec![vcc, NetId::GROUND],
        );
        c.add_part(
            "RC",
            PartKind::Resistor,
            v("4.7k", Unit::Ohm),
            vec![vcc, coll],
        );
        c.add_part(
            "Vin",
            PartKind::VoltageSource,
            None,
            vec![base, NetId::GROUND],
        );
        c.add_part(
            "Q1",
            PartKind::Bjt(Polarity::N, BjtModel::default()),
            None,
            vec![coll, base, NetId::GROUND],
        );
        // Sine → 1∠0 AC drive; its offset is the DC base bias.
        assert!(c.set_waveform(
            "Vin",
            Waveform::Sine {
                amplitude: 1.0,
                freq: 1_000.0,
                offset: 0.65,
                phase_deg: 0.0,
            },
        ));

        let sigs = Engine::new()
            .run(
                &c,
                &Analysis::Ac {
                    f_start: 100.0,
                    f_stop: 10_000.0,
                    points_per_decade: 2,
                },
            )
            .unwrap();

        let m = BjtModel::default();
        let gm = m.is * (0.65 / m.vt).exp() / m.vt;
        let expected = gm * 4.7e3; // |Av|
        let name = format!("v(n{})", c.resolve(coll).0);
        let sig = sigs.iter().find(|s| s.name == name).unwrap();
        let h = &sig.complex().unwrap()[0];
        // Resistive load, no reactances ⇒ flat gain, 180° inverted.
        assert!(
            (h.magnitude() - expected).abs() / expected <= 0.03,
            "|Av| = {} vs −gm·RC = {expected}",
            h.magnitude()
        );
        assert!(
            (h.phase_deg().abs() - 180.0).abs() <= 1.0,
            "phase {}° not inverting",
            h.phase_deg()
        );
    }

    // --- Adaptive timestep (LTE control) — CP-SIM-M1.6 ---------------------

    #[test]
    fn adaptive_transient_beats_fixed_on_an_rc_step() {
        // Gate: an RC step run over 20 τ has a fast rise then a long placid tail —
        // a fixed step must stay small throughout to resolve the knee, while the
        // adaptive step grows once the curve flattens. The adaptive run must use
        // materially fewer points than a fine fixed reference while holding the
        // same order of accuracy against the analytic 5·(1 − e^(−t/RC)), RC = 1 ms.
        let rc = 1e-3;
        let dur = 20e-3;
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
        let name = format!("v(n{})", c.resolve(out).0);
        let max_err = |sig: &Signal| -> f64 {
            sig.x
                .iter()
                .zip(sig.real().unwrap())
                .map(|(&t, &y)| (y - 5.0 * (1.0 - (-t / rc).exp())).abs())
                .fold(0.0, f64::max)
        };

        // Adaptive (step = None): the engine chooses the grid.
        let asig_all = Engine::new()
            .run(
                &c,
                &Analysis::Transient {
                    duration: dur,
                    step: None,
                },
            )
            .unwrap();
        let asig = asig_all.iter().find(|s| s.name == name).unwrap();
        // Fine fixed reference at 2 µs (10 000 steps).
        let fsig_all = Engine::new()
            .run(
                &c,
                &Analysis::Transient {
                    duration: dur,
                    step: Some(2e-6),
                },
            )
            .unwrap();
        let fsig = fsig_all.iter().find(|s| s.name == name).unwrap();

        let (a_pts, f_pts) = (asig.len(), fsig.len());
        let (a_err, f_err) = (max_err(asig), max_err(fsig));

        // Materially fewer points — at least 4× (the real ratio is ~50×).
        assert!(
            a_pts * 4 < f_pts,
            "adaptive used {a_pts} points vs fine {f_pts} — not materially fewer"
        );
        // Holds accuracy: a small absolute error, and the same order as the fine
        // reference (within 15×, not the ~10 000× a coarse fixed grid would cost).
        assert!(a_err <= 0.02, "adaptive max error {a_err} V too large");
        assert!(
            a_err <= 15.0 * f_err,
            "adaptive error {a_err} not comparable to fine {f_err}"
        );

        // The grid actually adapts: the first step (on the steep rise) is far
        // finer than the last (on the flat tail), and it lands exactly on `dur`.
        let first_gap = asig.x[1] - asig.x[0];
        let last_gap = asig.x[a_pts - 1] - asig.x[a_pts - 2];
        assert!(
            last_gap > 4.0 * first_gap,
            "step did not coarsen: first {first_gap} s, last {last_gap} s"
        );
        assert!((asig.x[0]).abs() < 1e-15, "starts at t = 0");
        assert!(
            (asig.x[a_pts - 1] - dur).abs() <= dur * 1e-9,
            "ends at {} s, expected {dur} s",
            asig.x[a_pts - 1]
        );
        // Physically sane: uncharged start, charged near the rail at the end.
        let y = asig.real().unwrap();
        assert!(y[0].abs() < 1e-9 && *y.last().unwrap() > 4.9);
    }

    #[test]
    fn adaptive_and_fixed_agree_on_the_rlc_ring() {
        // A reactive, oscillatory circuit (underdamped series RLC) is a sterner LTE
        // test than a monotone RC: the adaptive grid must track the ring. Its first
        // overshoot peak must land at the same place a fine fixed run puts it.
        let build = || {
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
            (c, out)
        };
        let (c, out) = build();
        let name = format!("v(n{})", c.resolve(out).0);
        let dur = 400e-6;

        let peak_time = |sig: &Signal| -> (f64, f64) {
            let (xs, ys) = (&sig.x, sig.real().unwrap());
            for i in 1..ys.len() - 1 {
                if ys[i] >= ys[i - 1] && ys[i] > ys[i + 1] {
                    return (xs[i], ys[i]);
                }
            }
            (f64::NAN, f64::NAN)
        };

        let a = Engine::new()
            .run(
                &c,
                &Analysis::Transient {
                    duration: dur,
                    step: None,
                },
            )
            .unwrap();
        let asig = a.iter().find(|s| s.name == name).unwrap();
        let f = Engine::new()
            .run(
                &c,
                &Analysis::Transient {
                    duration: dur,
                    step: Some(2e-8),
                },
            )
            .unwrap();
        let fsig = f.iter().find(|s| s.name == name).unwrap();

        let (at, av) = peak_time(asig);
        let (ft, fv) = peak_time(fsig);
        assert!(av > 5.0 && fv > 5.0, "both overshoot 5 V (a={av}, f={fv})");
        // The adaptive first peak lands within 3 % of the fine reference's.
        assert!(
            (at - ft).abs() / ft <= 0.03,
            "adaptive peak at {at} s vs fine {ft} s"
        );
        assert!(
            asig.len() * 3 < fsig.len(),
            "adaptive {} pts not materially fewer than fine {}",
            asig.len(),
            fsig.len()
        );
    }

    // --- DC sweep + parametric sweep — CP-SIM-M1.5 -------------------------

    /// Build the 1k/1k divider driven by `V1` (DC, initial 0 V).
    fn divider() -> (Circuit, NetId, NetId) {
        let mut c = Circuit::new("divider");
        let vin = c.new_net();
        let mid = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            v("0", Unit::Volt),
            vec![vin, NetId::GROUND],
        );
        c.add_part("R1", PartKind::Resistor, v("1k", Unit::Ohm), vec![vin, mid]);
        c.add_part(
            "R2",
            PartKind::Resistor,
            v("1k", Unit::Ohm),
            vec![mid, NetId::GROUND],
        );
        (c, vin, mid)
    }

    #[test]
    fn dc_sweep_divider_is_linear_in_the_source() {
        // Sweep V1 0 → 10 V in 1 V steps: the midpoint tracks Vin/2 and the input
        // node tracks the swept level, so the whole family is linear in the source.
        let (c, vin, mid) = divider();
        let sigs = Engine::new()
            .run(
                &c,
                &Analysis::DcSweep {
                    source: "V1".into(),
                    start: 0.0,
                    stop: 10.0,
                    step: 1.0,
                },
            )
            .unwrap();

        let mid_name = format!("v(n{})", c.resolve(mid).0);
        let mid_sig = sigs.iter().find(|s| s.name == mid_name).unwrap();
        // 11 inclusive samples, the swept level on the x axis (0 … 10 V).
        assert_eq!(mid_sig.x.len(), 11, "expected 11 sweep points");
        assert!((mid_sig.x[0]).abs() < 1e-12);
        assert!((mid_sig.x[10] - 10.0).abs() < 1e-9);
        for (&xin, &vm) in mid_sig.x.iter().zip(mid_sig.real().unwrap()) {
            assert!((vm - xin / 2.0).abs() <= 1e-9, "x={xin}: mid={vm}");
        }

        let vin_name = format!("v(n{})", c.resolve(vin).0);
        let vin_sig = sigs.iter().find(|s| s.name == vin_name).unwrap();
        for (&xin, &vv) in vin_sig.x.iter().zip(vin_sig.real().unwrap()) {
            assert!((vv - xin).abs() <= 1e-9, "x={xin}: vin={vv}");
        }
    }

    #[test]
    fn dc_sweep_diode_matches_shockley() {
        // Sweep the current forced through a diode (1 → 10 mA) and read the
        // junction voltage back: each point is the inverse-Shockley law
        // V = n·Vt·ln(I/Is + 1), solved by the Newton path at every sample.
        let mut c = Circuit::new("diode_sweep");
        let node = c.new_net();
        c.add_part(
            "I1",
            PartKind::CurrentSource,
            v("1m", Unit::Ampere),
            vec![NetId::GROUND, node],
        );
        c.add_part(
            "D1",
            PartKind::Diode(DiodeModel::default()),
            None,
            vec![node, NetId::GROUND],
        );

        let sigs = Engine::new()
            .run(
                &c,
                &Analysis::DcSweep {
                    source: "I1".into(),
                    start: 1e-3,
                    stop: 10e-3,
                    step: 1e-3,
                },
            )
            .unwrap();

        let name = format!("v(n{})", c.resolve(node).0);
        let sig = sigs.iter().find(|s| s.name == name).unwrap();
        assert_eq!(sig.x.len(), 10);
        for (&i, &vd) in sig.x.iter().zip(sig.real().unwrap()) {
            let expected = VTE * (i / IS + 1.0).ln();
            assert!(
                (vd - expected).abs() <= 1e-7,
                "I={i}: vd={vd}, expected {expected}"
            );
        }
    }

    #[test]
    fn parametric_runs_inner_once_per_value() {
        // Parametric over V1 ∈ {2, 5, 8} V with an inner DC operating point of the
        // divider: three runs × two nodes = six tagged signals, midpoint = Vin/2.
        let (c, _vin, mid) = divider();
        let values = vec![2.0, 5.0, 8.0];
        let sigs = Engine::new()
            .run(
                &c,
                &Analysis::Parametric {
                    source: "V1".into(),
                    values: values.clone(),
                    inner: Box::new(Analysis::OperatingPoint),
                },
            )
            .unwrap();

        assert_eq!(sigs.len(), 6, "2 nodes × 3 values");
        let mid_raw = c.resolve(mid).0;
        for &val in &values {
            let tag = format!("v(n{mid_raw}) @ V1={val}");
            let s = sigs
                .iter()
                .find(|s| s.name == tag)
                .unwrap_or_else(|| panic!("missing tagged signal {tag}"));
            assert!(
                (s.real().unwrap()[0] - val / 2.0).abs() <= 1e-9,
                "{tag}: mid={}",
                s.real().unwrap()[0]
            );
        }
    }

    #[test]
    fn parametric_over_transient_sweeps_the_bias() {
        // An inner transient per value proves the inner analysis is a full run, not
        // just an operating point: an RC charging from a swept rail settles toward
        // that rail. Sweep V1 ∈ {1, 4} V; the last sample of v(out) nears the rail.
        let mut c = Circuit::new("rc");
        let vin = c.new_net();
        let out = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            v("1", Unit::Volt),
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
                &Analysis::Parametric {
                    source: "V1".into(),
                    values: vec![1.0, 4.0],
                    inner: Box::new(Analysis::Transient {
                        duration: 5e-3,
                        step: Some(5e-6),
                    }),
                },
            )
            .unwrap();

        let out_raw = c.resolve(out).0;
        for &rail in &[1.0_f64, 4.0] {
            let tag = format!("v(n{out_raw}) @ V1={rail}");
            let s = sigs.iter().find(|s| s.name == tag).unwrap();
            let y = s.real().unwrap();
            assert!(y[0].abs() < 1e-9, "{tag}: starts uncharged");
            assert!(
                *y.last().unwrap() > 0.98 * rail,
                "{tag}: ends at {} V, near {rail}",
                y.last().unwrap()
            );
        }
    }

    #[test]
    fn sweep_rejects_bad_targets_and_steps() {
        let (c, _vin, _mid) = divider();
        let run = |a| Engine::new().run(&c, &a);

        // Unknown source reference.
        assert!(matches!(
            run(Analysis::DcSweep {
                source: "VX".into(),
                start: 0.0,
                stop: 10.0,
                step: 1.0,
            })
            .unwrap_err(),
            SimError::UnknownPart(_)
        ));
        // A resistor is not an independent source.
        assert!(matches!(
            run(Analysis::DcSweep {
                source: "R1".into(),
                start: 0.0,
                stop: 10.0,
                step: 1.0,
            })
            .unwrap_err(),
            SimError::Unsupported(_)
        ));
        // Zero step, and a step pointing the wrong way, are both bad values.
        assert!(matches!(
            run(Analysis::DcSweep {
                source: "V1".into(),
                start: 0.0,
                stop: 10.0,
                step: 0.0,
            })
            .unwrap_err(),
            SimError::BadValue(_)
        ));
        assert!(matches!(
            run(Analysis::DcSweep {
                source: "V1".into(),
                start: 0.0,
                stop: 10.0,
                step: -1.0,
            })
            .unwrap_err(),
            SimError::BadValue(_)
        ));
        // Parametric validates the target even with an empty value list.
        assert!(matches!(
            run(Analysis::Parametric {
                source: "VX".into(),
                values: vec![],
                inner: Box::new(Analysis::OperatingPoint),
            })
            .unwrap_err(),
            SimError::UnknownPart(_)
        ));
    }

    // --- Controlled (dependent) sources — CP-SIM-M2.3 ----------------------

    #[test]
    fn vcvs_gain_is_exact_at_the_op_point() {
        // The acceptance gate: a VCVS with gain 10 reproduces exactly 10× the
        // sensed input at the operating point. Vin = 1 V on the control port;
        // E1 drives `out` from `vin` with E = 10 into a 1 kΩ load.
        let mut c = Circuit::new("vcvs");
        let vin = c.new_net();
        let out = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            v("1", Unit::Volt),
            vec![vin, NetId::GROUND],
        );
        // E1 = [out+, out−, ctrl+, ctrl−]; out = 10·(vin − gnd).
        c.add_part(
            "E1",
            PartKind::Vcvs(10.0),
            None,
            vec![out, NetId::GROUND, vin, NetId::GROUND],
        );
        c.add_part(
            "RL",
            PartKind::Resistor,
            v("1k", Unit::Ohm),
            vec![out, NetId::GROUND],
        );

        let layout = Layout::build(&c);
        let x = solve_dc(&c, &layout).unwrap();
        let vo = x[layout.node_of(&c, out).unwrap()];
        assert!(
            (vo - 10.0).abs() <= 1e-9,
            "VCVS output {vo} V, expected 10 V"
        );
    }

    #[test]
    fn vcvs_gain_is_flat_in_ac() {
        // The same gain-10 stage in AC: a frequency-independent gain, so |H| = 10
        // (+20 dB) at 0° everywhere in the sweep — the controlled source stamps in
        // the complex system too.
        let mut c = Circuit::new("vcvs_ac");
        let vin = c.new_net();
        let out = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            None,
            vec![vin, NetId::GROUND],
        );
        c.add_part(
            "E1",
            PartKind::Vcvs(10.0),
            None,
            vec![out, NetId::GROUND, vin, NetId::GROUND],
        );
        c.add_part(
            "RL",
            PartKind::Resistor,
            v("1k", Unit::Ohm),
            vec![out, NetId::GROUND],
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

        let sigs = Engine::new()
            .run(
                &c,
                &Analysis::Ac {
                    f_start: 10.0,
                    f_stop: 1e6,
                    points_per_decade: 5,
                },
            )
            .unwrap();
        let name = format!("v(n{})", c.resolve(out).0);
        let sig = sigs.iter().find(|s| s.name == name).unwrap();
        for h in sig.complex().unwrap() {
            assert!(
                (h.magnitude() - 10.0).abs() <= 1e-6,
                "|H| = {} not flat at 10",
                h.magnitude()
            );
            assert!(
                h.phase_deg().abs() <= 1e-6,
                "phase {}° not 0",
                h.phase_deg()
            );
        }
    }

    #[test]
    fn vccs_transconductance_into_a_load() {
        // A VCCS of G = 10 mA/V driving a 1 kΩ load: Vout = G·Vin·R. With Vin = 2 V
        // that is 0.010·2·1000 = 20 V. The source pushes its current out+ → out−
        // inside, so `[gnd, load]` sinks the current into the load node.
        let mut c = Circuit::new("vccs");
        let vin = c.new_net();
        let load = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            v("2", Unit::Volt),
            vec![vin, NetId::GROUND],
        );
        // G1 = [out+, out−, ctrl+, ctrl−] = [gnd, load, vin, gnd].
        c.add_part(
            "G1",
            PartKind::Vccs(10e-3),
            None,
            vec![NetId::GROUND, load, vin, NetId::GROUND],
        );
        c.add_part(
            "RL",
            PartKind::Resistor,
            v("1k", Unit::Ohm),
            vec![load, NetId::GROUND],
        );

        let layout = Layout::build(&c);
        let x = solve_dc(&c, &layout).unwrap();
        let vo = x[layout.node_of(&c, load).unwrap()];
        let expected = 10e-3 * 2.0 * 1000.0;
        assert!(
            (vo - expected).abs() <= 1e-9,
            "VCCS load {vo} V, expected {expected} V"
        );
    }

    #[test]
    fn ideal_opamp_noninverting_gain() {
        // The behavioral-modeling payoff: an ideal op-amp is a VCVS with a huge
        // open-loop gain. In a non-inverting config (Rf from out to the inverting
        // node, Rg from there to ground) the closed-loop gain is 1 + Rf/Rg.
        // Rf = 9k, Rg = 1k ⇒ gain 10, so Vin = 1 V → Vout ≈ 10 V.
        let mut c = Circuit::new("opamp");
        let vin = c.new_net(); // non-inverting input (V+)
        let vfb = c.new_net(); // inverting input / feedback node (V−)
        let vout = c.new_net();
        c.add_part(
            "Vin",
            PartKind::VoltageSource,
            v("1", Unit::Volt),
            vec![vin, NetId::GROUND],
        );
        // Op-amp: out = A·(V+ − V−), A huge. [out+, out−, ctrl+, ctrl−].
        c.add_part(
            "EOP",
            PartKind::Vcvs(1e6),
            None,
            vec![vout, NetId::GROUND, vin, vfb],
        );
        c.add_part(
            "Rf",
            PartKind::Resistor,
            v("9k", Unit::Ohm),
            vec![vout, vfb],
        );
        c.add_part(
            "Rg",
            PartKind::Resistor,
            v("1k", Unit::Ohm),
            vec![vfb, NetId::GROUND],
        );

        let layout = Layout::build(&c);
        let x = solve_dc(&c, &layout).unwrap();
        let vo = x[layout.node_of(&c, vout).unwrap()];
        // Closed-loop gain 1 + 9k/1k = 10; the finite open-loop gain leaves a
        // ~1/A·β residual, far inside this tolerance.
        assert!(
            (vo - 10.0).abs() <= 1e-2,
            "non-inverting gain output {vo} V, expected ≈ 10 V"
        );
    }

    #[test]
    fn cccs_mirrors_a_sensed_current() {
        // Current-controlled current source. A 0 V sense source reads the 1 mA
        // flowing through R1 (1 V across 1 kΩ); F1 reproduces F·I = 5 mA into a
        // 1 kΩ load, so Vload = 5 mA·1 kΩ = 5 V.
        let mut c = Circuit::new("cccs");
        let a = c.new_net();
        let b = c.new_net();
        let load = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            v("1", Unit::Volt),
            vec![a, NetId::GROUND],
        );
        c.add_part("R1", PartKind::Resistor, v("1k", Unit::Ohm), vec![a, b]);
        // 0 V sense source: its branch current is the controlling current.
        c.add_part(
            "Vsense",
            PartKind::VoltageSource,
            v("0", Unit::Volt),
            vec![b, NetId::GROUND],
        );
        // F1 = [out+, out−] = [gnd, load]; current F·I(Vsense) flows into the load.
        c.add_part(
            "F1",
            PartKind::Cccs(5.0, "Vsense".into()),
            None,
            vec![NetId::GROUND, load],
        );
        c.add_part(
            "RL",
            PartKind::Resistor,
            v("1k", Unit::Ohm),
            vec![load, NetId::GROUND],
        );

        let layout = Layout::build(&c);
        let x = solve_dc(&c, &layout).unwrap();
        let vo = x[layout.node_of(&c, load).unwrap()];
        assert!((vo - 5.0).abs() <= 1e-9, "CCCS load {vo} V, expected 5 V");
    }

    #[test]
    fn ccvs_transresistance_from_a_sensed_current() {
        // Current-controlled voltage source. The same 1 mA sensed current drives
        // H1 with R = 2 kΩ transresistance, so Vout = R·I = 2 kΩ·1 mA = 2 V,
        // independent of the output load.
        let mut c = Circuit::new("ccvs");
        let a = c.new_net();
        let b = c.new_net();
        let out = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            v("1", Unit::Volt),
            vec![a, NetId::GROUND],
        );
        c.add_part("R1", PartKind::Resistor, v("1k", Unit::Ohm), vec![a, b]);
        c.add_part(
            "Vsense",
            PartKind::VoltageSource,
            v("0", Unit::Volt),
            vec![b, NetId::GROUND],
        );
        // H1 = [out+, out−] = [out, gnd]; V(out) = 2000·I(Vsense).
        c.add_part(
            "H1",
            PartKind::Ccvs(2000.0, "Vsense".into()),
            None,
            vec![out, NetId::GROUND],
        );
        c.add_part(
            "RL",
            PartKind::Resistor,
            v("1k", Unit::Ohm),
            vec![out, NetId::GROUND],
        );

        let layout = Layout::build(&c);
        let x = solve_dc(&c, &layout).unwrap();
        let vo = x[layout.node_of(&c, out).unwrap()];
        assert!((vo - 2.0).abs() <= 1e-9, "CCVS output {vo} V, expected 2 V");
    }

    #[test]
    fn controlled_source_with_unknown_control_is_reported() {
        // A CCCS naming a controlling element that is not a sensing voltage source
        // surfaces as a clear error, not a panic or a silent drop.
        let mut c = Circuit::new("bad_ctrl");
        let out = c.new_net();
        c.add_part(
            "F1",
            PartKind::Cccs(2.0, "Vmissing".into()),
            None,
            vec![out, NetId::GROUND],
        );
        c.add_part(
            "RL",
            PartKind::Resistor,
            v("1k", Unit::Ohm),
            vec![out, NetId::GROUND],
        );
        let err = Engine::new()
            .run(&c, &Analysis::OperatingPoint)
            .unwrap_err();
        assert!(
            matches!(err, SimError::BadControl(ref r) if r == "Vmissing"),
            "expected BadControl, got {err:?}"
        );
    }

    // --- Noise analysis — CP-SIM-M2.1 --------------------------------------

    #[test]
    fn resistor_thermal_noise() {
        // The acceptance gate: a lone resistor to ground has an output-referred
        // thermal-noise PSD equal to 4kTR (V²/Hz), flat over frequency. Its own
        // conductance is the only admittance at the node, so the transimpedance
        // from its noise current source (PSD 4kT/R) to the node is exactly R, and
        // R²·(4kT/R) = 4kTR.
        let r = 1_000.0_f64;
        let mut c = Circuit::new("one_r");
        let out = c.new_net();
        c.add_part(
            "R1",
            PartKind::Resistor,
            v("1k", Unit::Ohm),
            vec![out, NetId::GROUND],
        );

        let sigs = Engine::new()
            .run(
                &c,
                &Analysis::Noise {
                    output: out,
                    f_start: 1.0,
                    f_stop: 1e6,
                    points_per_decade: 5,
                },
            )
            .unwrap();
        let name = format!("onoise(n{})", c.resolve(out).0);
        let sig = sigs
            .iter()
            .find(|s| s.name == name)
            .expect("onoise present");
        assert_eq!(sig.domain, Domain::Freq);

        let expected = 4.0 * BOLTZMANN_K * NOISE_TEMP_K * r; // 4kTR
        let psd = sig.real().unwrap();
        assert!(!psd.is_empty());
        for &s in psd {
            // Flat thermal floor across the whole sweep, to a tight relative tol.
            assert!(
                (s - expected).abs() <= 1e-6 * expected,
                "noise PSD {s} V²/Hz != 4kTR {expected}"
            );
        }
    }

    #[test]
    fn resistor_divider_output_noise_combines_both() {
        // A resistor divider's output noise is the thermal noise of the parallel
        // combination: each resistor's noise current (PSD 4kT/R_i) sees R1∥R2 to the
        // output, so S_out = (R1∥R2)²·4kT·(1/R1 + 1/R2) = 4kT·(R1∥R2). The quiet V1
        // is a short, so the two resistors share the output node's R1∥R2.
        let (r1, r2) = (1_000.0_f64, 3_000.0_f64);
        let mut c = Circuit::new("divider_noise");
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
            v("3k", Unit::Ohm),
            vec![mid, NetId::GROUND],
        );

        let sigs = Engine::new()
            .run(
                &c,
                &Analysis::Noise {
                    output: mid,
                    f_start: 10.0,
                    f_stop: 1e5,
                    points_per_decade: 3,
                },
            )
            .unwrap();
        let name = format!("onoise(n{})", c.resolve(mid).0);
        let sig = sigs.iter().find(|s| s.name == name).unwrap();

        let rpar = r1 * r2 / (r1 + r2);
        let expected = 4.0 * BOLTZMANN_K * NOISE_TEMP_K * rpar; // 4kT·(R1∥R2)
        for &s in sig.real().unwrap() {
            assert!(
                (s - expected).abs() <= 1e-6 * expected,
                "divider output noise {s} != 4kT·(R1∥R2) {expected}"
            );
        }
    }

    // --- Monte-Carlo + tolerances — CP-SIM-M2.2 ----------------------------

    /// A 1k/1k divider off a 10 V rail, both resistors toleranced ±5 %.
    fn mc_divider() -> (Circuit, NetId) {
        let mut c = Circuit::new("mc_divider");
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
        assert!(c.set_tolerance("R1", 0.05));
        assert!(c.set_tolerance("R2", 0.05));
        (c, mid)
    }

    #[test]
    fn monte_carlo_is_deterministic_under_a_fixed_seed() {
        let (c, mid) = mc_divider();
        let mc = |seed| Analysis::MonteCarlo {
            seed,
            runs: 400,
            output: mid,
            sample: 0,
            inner: Box::new(Analysis::OperatingPoint),
        };
        let run = |a| {
            Engine::new().run(&c, &a).unwrap()[0]
                .real()
                .unwrap()
                .to_vec()
        };

        // Same seed → bit-for-bit identical samples, twice.
        let a = run(mc(42));
        let b = run(mc(42));
        assert_eq!(a, b, "a fixed seed must reproduce the samples exactly");
        assert_eq!(a.len(), 400);

        // A different seed → a different draw (deterministic, but not the same).
        let d = run(mc(7));
        assert_ne!(a, d, "a different seed must give a different draw");

        // The spread is consistent with ±5 % resistors. mid = 10·R2/(R1+R2) with
        // R1,R2 ∈ [950, 1050] Ω ⇒ mid ∈ (4.63, 5.39) V, centered on 5 V, and the
        // family actually spreads (not a single repeated value).
        let (mut lo, mut hi, mut sum) = (f64::INFINITY, f64::NEG_INFINITY, 0.0);
        for &s in &a {
            assert!(
                (4.5..5.5).contains(&s),
                "sample {s} V outside the ±5 % divider window"
            );
            lo = lo.min(s);
            hi = hi.max(s);
            sum += s;
        }
        let mean = sum / a.len() as f64;
        assert!(
            (mean - 5.0).abs() < 0.05,
            "mean {mean} V not centered on 5 V"
        );
        assert!(hi - lo > 0.1, "samples barely spread (range {})", hi - lo);
    }

    #[test]
    fn monte_carlo_without_tolerances_is_a_constant() {
        // No toleranced parts ⇒ no PRNG draws ⇒ every run is the nominal circuit,
        // so the samples are the exact nominal output repeated (5 V for the divider).
        let mut c = Circuit::new("nominal");
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
        let sigs = Engine::new()
            .run(
                &c,
                &Analysis::MonteCarlo {
                    seed: 1,
                    runs: 8,
                    output: mid,
                    sample: 0,
                    inner: Box::new(Analysis::OperatingPoint),
                },
            )
            .unwrap();
        for &s in sigs[0].real().unwrap() {
            assert!((s - 5.0).abs() < 1e-9, "nominal divider sample {s} != 5 V");
        }
    }

    #[test]
    fn monte_carlo_rejects_zero_runs() {
        let (c, mid) = mc_divider();
        let err = Engine::new()
            .run(
                &c,
                &Analysis::MonteCarlo {
                    seed: 0,
                    runs: 0,
                    output: mid,
                    sample: 0,
                    inner: Box::new(Analysis::OperatingPoint),
                },
            )
            .unwrap_err();
        assert!(matches!(err, SimError::BadValue(_)), "got {err:?}");
    }

    // --- Parallel analyses — CP-SIM-M4.3 -----------------------------------

    /// The `CP-SIM-M4.3` determinism gate: the public Monte-Carlo (fanned across
    /// threads when the `parallel` feature is on) returns **exactly** the same
    /// `Signal` as the forced-sequential path, bit for bit, independently of thread
    /// scheduling. Run it with `--features parallel` to exercise the threaded path;
    /// without it both sides are sequential and the equality still holds. A healthy
    /// run count (512) spans several thread chunks.
    #[test]
    fn parallel_monte_carlo_matches_sequential() {
        let (c, mid) = mc_divider();
        let engine = Engine::new();
        let (seed, runs) = (0x5EED_1234_u64, 512usize);

        // Public path: threaded under `--features parallel`, else sequential.
        let parallel = engine
            .run(
                &c,
                &Analysis::MonteCarlo {
                    seed,
                    runs,
                    output: mid,
                    sample: 0,
                    inner: Box::new(Analysis::OperatingPoint),
                },
            )
            .unwrap();
        // Forced-sequential reference (the plain loop, always).
        let sequential = engine
            .monte_carlo_sequential(&c, seed, runs, mid, 0, &Analysis::OperatingPoint)
            .unwrap();

        assert_eq!(
            parallel, sequential,
            "parallel Monte-Carlo diverged from the sequential path"
        );
        assert_eq!(parallel[0].real().unwrap().len(), runs);
    }

    /// A parallel DC sweep (many independent operating-point solves) is
    /// bit-for-bit reproducible across runs — the index-ordered executor means the
    /// thread schedule can never reorder or perturb the result. A 60-point diode
    /// sweep spans several chunks.
    #[test]
    fn parallel_dc_sweep_is_reproducible() {
        let mut c = Circuit::new("diode_sweep");
        let node = c.new_net();
        c.add_part(
            "I1",
            PartKind::CurrentSource,
            v("1m", Unit::Ampere),
            vec![NetId::GROUND, node],
        );
        c.add_part(
            "D1",
            PartKind::Diode(DiodeModel::default()),
            None,
            vec![node, NetId::GROUND],
        );
        let sweep = Analysis::DcSweep {
            source: "I1".into(),
            start: 1e-3,
            stop: 60e-3,
            step: 1e-3,
        };
        let a = Engine::new().run(&c, &sweep).unwrap();
        let b = Engine::new().run(&c, &sweep).unwrap();
        assert_eq!(a, b, "parallel DC sweep is not reproducible");
        assert_eq!(a[0].x.len(), 60);
    }

    /// A parallel parametric sweep (an inner transient per value) is reproducible
    /// and preserves value order in the tagged signal families.
    #[test]
    fn parallel_parametric_is_reproducible() {
        let mut c = Circuit::new("rc");
        let vin = c.new_net();
        let out = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            v("1", Unit::Volt),
            vec![vin, NetId::GROUND],
        );
        c.add_part("R1", PartKind::Resistor, v("1k", Unit::Ohm), vec![vin, out]);
        c.add_part(
            "C1",
            PartKind::Capacitor,
            v("1u", Unit::Farad),
            vec![out, NetId::GROUND],
        );
        let values: Vec<f64> = (1..=16).map(|i| i as f64).collect();
        let param = Analysis::Parametric {
            source: "V1".into(),
            values: values.clone(),
            inner: Box::new(Analysis::Transient {
                duration: 2e-3,
                step: Some(5e-4),
            }),
        };
        let a = Engine::new().run(&c, &param).unwrap();
        let b = Engine::new().run(&c, &param).unwrap();
        assert_eq!(a, b, "parallel parametric sweep is not reproducible");
        // The value families stay in order: the last signal carries the last value.
        assert!(a
            .last()
            .unwrap()
            .name
            .ends_with(&format!("@ V1={}", values.last().unwrap())));
    }

    #[test]
    fn noise_rejects_a_bad_sweep_range() {
        let mut c = Circuit::new("one_r");
        let out = c.new_net();
        c.add_part(
            "R1",
            PartKind::Resistor,
            v("1k", Unit::Ohm),
            vec![out, NetId::GROUND],
        );
        let err = Engine::new()
            .run(
                &c,
                &Analysis::Noise {
                    output: out,
                    f_start: 0.0, // non-positive start is rejected
                    f_stop: 1e3,
                    points_per_decade: 10,
                },
            )
            .unwrap_err();
        assert!(matches!(err, SimError::BadValue(_)), "got {err:?}");
    }

    // --- Audit regressions -------------------------------------------------

    /// A transient's `t = 0` solve must enjoy the same convergence aids as the DC
    /// operating point. The stiff diode-connected-MOSFET-off-a-current-source bias
    /// (the [`stiff_mosfet_needs_convergence_aids`] netlist) needs gmin stepping;
    /// before the fix the transient's `initial()` used plain Newton–Raphson and so
    /// failed with [`SimError::Singular`], even though the op point solved. The
    /// operating point is a current-source-held DC bias, so every transient sample
    /// must equal the op-point Vd = 2.0 V.
    #[test]
    fn transient_initial_point_uses_convergence_aids() {
        let mut c = Circuit::new("isrc_mos_tran");
        let d = c.new_net();
        c.add_part(
            "I1",
            PartKind::CurrentSource,
            v("1m", Unit::Ampere),
            vec![NetId::GROUND, d],
        );
        c.add_part(
            "M1",
            PartKind::Mosfet(Polarity::N, MosfetModel::default()),
            None,
            vec![d, d, NetId::GROUND],
        );

        let sigs = Engine::new()
            .run(
                &c,
                &Analysis::Transient {
                    duration: 1e-3,
                    step: Some(2e-4),
                },
            )
            .expect("transient must solve the stiff bias via the convergence aids");
        let name = format!("v(n{})", c.resolve(d).0);
        let sig = sigs.iter().find(|s| s.name == name).expect("v(d) present");
        for &vd in sig.real().unwrap() {
            assert!(
                (vd - 2.0).abs() <= 1e-6,
                "transient v(d) = {vd} V, expected the op-point 2.0 V"
            );
        }
    }

    /// A non-positive capacitance has no physical meaning and gives a zero/negative
    /// companion conductance `C/h` that silently poisons the transient solve
    /// (before the fix it returned a finite but nonsensical voltage). It must be
    /// rejected as a [`SimError::BadValue`] in every analysis that uses the value —
    /// transient, AC, and noise — exactly as a non-positive resistance/inductance is.
    #[test]
    fn nonpositive_capacitor_is_rejected() {
        let build = |cap: f64| {
            let mut c = Circuit::new("badcap");
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
                Some(Value::new(cap, Unit::Farad)),
                vec![out, NetId::GROUND],
            );
            // Mark V1 as an AC drive so the AC/noise paths have a stimulus to build.
            c.set_waveform(
                "V1",
                Waveform::Sine {
                    amplitude: 1.0,
                    freq: 1_000.0,
                    offset: 0.0,
                    phase_deg: 0.0,
                },
            );
            (c, out)
        };

        for &bad in &[-1e-6, 0.0] {
            let (c, out) = build(bad);
            let tran = Engine::new().run(
                &c,
                &Analysis::Transient {
                    duration: 1e-3,
                    step: Some(1e-5),
                },
            );
            assert!(
                matches!(tran, Err(SimError::BadValue(ref r)) if r == "C1"),
                "C = {bad} F transient should be BadValue(C1), got {tran:?}"
            );
            let ac = Engine::new().run(
                &c,
                &Analysis::Ac {
                    f_start: 10.0,
                    f_stop: 1e4,
                    points_per_decade: 5,
                },
            );
            assert!(
                matches!(ac, Err(SimError::BadValue(ref r)) if r == "C1"),
                "C = {bad} F AC should be BadValue(C1), got {ac:?}"
            );
            let noise = Engine::new().run(
                &c,
                &Analysis::Noise {
                    output: out,
                    f_start: 10.0,
                    f_stop: 1e4,
                    points_per_decade: 5,
                },
            );
            assert!(
                matches!(noise, Err(SimError::BadValue(ref r)) if r == "C1"),
                "C = {bad} F noise should be BadValue(C1), got {noise:?}"
            );
        }

        // A sane positive capacitor still runs (guards against over-rejection).
        let (c, _out) = build(1e-6);
        assert!(Engine::new()
            .run(
                &c,
                &Analysis::Transient {
                    duration: 1e-3,
                    step: Some(1e-5),
                },
            )
            .is_ok());
    }

    // --- Behavioral (expression) sources — CP-SIM-M3.1 ---------------------

    /// Build an unloaded source node (1 MΩ bleed to ground keeps it non-floating)
    /// driven by a behavioral `Expr` waveform on `V1`.
    fn expr_source(src: &str) -> (Circuit, NetId) {
        let mut c = Circuit::new("expr");
        let out = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            None, // an expression carries its own levels — no DC Value needed
            vec![out, NetId::GROUND],
        );
        c.add_part(
            "R1",
            PartKind::Resistor,
            v("1meg", Unit::Ohm),
            vec![out, NetId::GROUND],
        );
        assert!(c.set_waveform("V1", Waveform::Expr(src.into())));
        (c, out)
    }

    #[test]
    fn expr_source_reproduces_a_sine_in_transient() {
        // A behavioral source driving sin(2π·1 kHz·t) must trace the analytic sine
        // at every sampled time, and start at its t = 0 value (sin 0 = 0).
        let (c, out) = expr_source("sin(2*pi*1000*t)");
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
        let y = sig.real().unwrap();
        assert!(y[0].abs() < 1e-9, "t=0 value {} != sin(0)", y[0]);
        for (&t, &val) in sig.x.iter().zip(y).skip(1) {
            let expected = (TAU * 1000.0 * t).sin();
            assert!(
                (val - expected).abs() <= 1e-6,
                "t={t}: got {val}, expected {expected}"
            );
        }
    }

    #[test]
    fn expr_source_evaluates_a_compound_waveform() {
        // A damped sinusoid exp(−t/τ)·sin(2π·1 kHz·t), τ = 1 ms — exercises the
        // functions, the division, and operator precedence together.
        let (c, out) = expr_source("exp(-t/0.001)*sin(2*pi*1000*t)");
        let h = 2e-6;
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
        for (&t, &val) in sig.x.iter().zip(sig.real().unwrap()).skip(1) {
            let expected = (-t / 1e-3).exp() * (TAU * 1000.0 * t).sin();
            assert!(
                (val - expected).abs() <= 1e-6,
                "t={t}: got {val}, expected {expected}"
            );
        }
    }

    #[test]
    fn malformed_expression_is_a_clean_error() {
        // An unbalanced expression must surface as BadExpr(V1) — a clean error at
        // the very first (t = 0) evaluation, never a panic.
        let (c, _out) = expr_source("sin(2*");
        let err = Engine::new()
            .run(
                &c,
                &Analysis::Transient {
                    duration: 1e-3,
                    step: Some(1e-5),
                },
            )
            .unwrap_err();
        assert!(
            matches!(err, SimError::BadExpr(ref r) if r == "V1"),
            "expected BadExpr(V1), got {err:?}"
        );
    }

    // --- Resistor temperature coefficient — CP-SIM-M3.1 --------------------

    #[test]
    fn resistor_tempco_shifts_a_divider() {
        // A 1k/1k divider off 10 V sits at 5 V. Give the top resistor a +1 %/°C
        // tempco: at the default 27 °C it is inert (exactly 5 V, no regression),
        // and heating to 127 °C grows R1 to 2 kΩ so the midpoint drops to
        // 10·R2/(R1+R2) = 10/3 V — the physically correct direction.
        let mut c = Circuit::new("tempco_div");
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
        assert!(c.set_tempco("R1", 0.01)); // +1 %/°C

        // The topology is fixed across the temperature change, so build once.
        let layout = Layout::build(&c);
        let mid_idx = layout.node_of(&c, mid).unwrap();

        // Default temperature: the tempco term is zero → exactly 5 V, unchanged.
        let v_nominal = solve_dc(&c, &layout).unwrap()[mid_idx];
        assert!(
            (v_nominal - 5.0).abs() < 1e-12,
            "default-temperature divider = {v_nominal} V, expected exactly 5 V"
        );

        // Heat to 127 °C (ΔT = 100 °C): R1 → 1000·(1 + 0.01·100) = 2000 Ω.
        assert!(c.set_temperature(127.0));
        let v_hot = solve_dc(&c, &layout).unwrap()[mid_idx];
        let expected = 10.0 * 1000.0 / (2000.0 + 1000.0);
        assert!(
            (v_hot - expected).abs() <= 1e-9,
            "hot divider = {v_hot} V, expected {expected} V"
        );
        assert!(
            v_hot < v_nominal,
            "midpoint {v_hot} V did not drop as the top leg heated (from {v_nominal} V)"
        );
    }

    // --- Sparse solver routing — CP-SIM-M2.4 -------------------------------

    /// An `n`-rung resistor ladder (10 V source, series 1 kΩ into each rung node,
    /// 1 kΩ shunt to ground), returning the circuit and the ordered rung nets.
    fn ladder(n: usize) -> (Circuit, NetId, Vec<NetId>) {
        let mut c = Circuit::new("ladder");
        let vin = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            v("10", Unit::Volt),
            vec![vin, NetId::GROUND],
        );
        let mut nodes = Vec::with_capacity(n);
        let mut prev = vin;
        for i in 0..n {
            let node = c.new_net();
            c.add_part(
                format!("Rs{i}"),
                PartKind::Resistor,
                v("1k", Unit::Ohm),
                vec![prev, node],
            );
            c.add_part(
                format!("Rp{i}"),
                PartKind::Resistor,
                v("1k", Unit::Ohm),
                vec![node, NetId::GROUND],
            );
            prev = node;
        }
        for i in 1..=n {
            nodes.push(NetId(1 + i as u32)); // rung nets follow vin (= net 1)
        }
        (c, vin, nodes)
    }

    /// A large ladder clears [`mna::SPARSE_THRESHOLD`], so `Engine::run` takes the
    /// sparse path. Its operating point must (a) match the forced *dense* solve of
    /// the very same matrix bit-for-bit — proving the sparse path is wired in
    /// correctly — and (b) be physically right: v(vin) = 10 V exactly and every
    /// rung node a positive, strictly attenuated voltage.
    #[test]
    fn large_ladder_uses_sparse_and_matches_dense() {
        let n = 200; // system size 202 ≥ SPARSE_THRESHOLD (64) → sparse path
        assert!(
            n + 2 >= crate::mna::SPARSE_THRESHOLD,
            "test must exercise the sparse path"
        );
        let (c, vin, nodes) = ladder(n);

        // Engine (auto-dispatch → sparse) vs the forced dense solve of the matrix.
        let sigs = Engine::new().run(&c, &Analysis::OperatingPoint).unwrap();
        let dense = bench_support::dc_solve_dense(&c).expect("dense ladder solves");

        // v(vin) is pinned to the source: exactly 10 V.
        let vin_name = format!("v(n{})", c.resolve(vin).0);
        let vin_sig = sigs.iter().find(|s| s.name == vin_name).unwrap();
        assert_eq!(vin_sig.real().unwrap()[0], 10.0, "v(vin) must be 10 V");

        // Each node's sparse-engine voltage equals the dense solve to the bit, and
        // the rung voltages attenuate monotonically toward ground.
        let layout = Layout::build(&c);
        let mut last = 10.0;
        for &net in &nodes {
            let idx = layout.node_of(&c, net).unwrap();
            let name = format!("v(n{})", c.resolve(net).0);
            let got = sigs
                .iter()
                .find(|s| s.name == name)
                .unwrap()
                .real()
                .unwrap()[0];
            assert_eq!(got, dense[idx], "node {net:?}: sparse engine != dense");
            assert!(got > 0.0 && got < last, "ladder not monotone at {net:?}");
            last = got;
        }
    }

    // --- Ordered KLU hot-loop routing — CP-SIM-M5.1 ------------------------

    /// A star / "hub" resistor net: one hub node tied by 1 kΩ to each of `leaves`
    /// leaf nodes, every leaf also tied to ground by 1 kΩ, and a 10 V source driving
    /// the hub. Numbering the hub first (it is the source node, net 1) is the fill
    /// pathology min-degree ordering is built to defeat — natural order fills the
    /// whole leaf block. Returns the circuit, the hub net, and the leaf nets.
    fn hub(leaves: usize) -> (Circuit, NetId, Vec<NetId>) {
        let mut c = Circuit::new("hub");
        let h = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            v("10", Unit::Volt),
            vec![h, NetId::GROUND],
        );
        let mut leaf_nets = Vec::with_capacity(leaves);
        for i in 0..leaves {
            let leaf = c.new_net();
            c.add_part(
                format!("Rh{i}"),
                PartKind::Resistor,
                v("1k", Unit::Ohm),
                vec![h, leaf],
            );
            c.add_part(
                format!("Rg{i}"),
                PartKind::Resistor,
                v("1k", Unit::Ohm),
                vec![leaf, NetId::GROUND],
            );
            leaf_nets.push(leaf);
        }
        (c, h, leaf_nets)
    }

    /// Above [`mna::KLU_THRESHOLD`], `Engine::run` routes the solve through the
    /// fill-reducing ordered LU. On a large hub net its operating point must match
    /// the forced *dense* reference to ≤1e-12 (the ordered path is the same LU,
    /// merely reordered, so it agrees to a few ULP rather than bit-for-bit), and be
    /// physically right — the source pins the hub to 10 V and, by the symmetric
    /// 1 kΩ/1 kΩ divider at every leaf, each leaf sits at exactly half the hub.
    #[test]
    fn large_hub_uses_ordered_klu_and_matches_dense() {
        let leaves = 400; // system size 402 ≥ KLU_THRESHOLD (256) → ordered path
        assert!(
            leaves + 2 >= crate::mna::KLU_THRESHOLD,
            "test must clear the KLU threshold"
        );
        let (c, hub_net, leaf_nets) = hub(leaves);

        let sigs = Engine::new().run(&c, &Analysis::OperatingPoint).unwrap();
        let dense = bench_support::dc_solve_dense(&c).expect("dense hub solves");
        let layout = Layout::build(&c);

        // Hub pinned to the source.
        let hub_name = format!("v(n{})", c.resolve(hub_net).0);
        let hub_v = sigs
            .iter()
            .find(|s| s.name == hub_name)
            .unwrap()
            .real()
            .unwrap()[0];
        assert!(
            (hub_v - 10.0).abs() <= 1e-12,
            "hub = {hub_v} V, expected 10 V"
        );

        // Every node agrees with the dense reference to ≤1e-12, and each leaf is the
        // hub's symmetric half-divider (5 V).
        let mut max_diff = 0.0_f64;
        for s in &sigs {
            // Parse the node index out of the "v(nX)" signal back to a dense index.
            let raw: u32 = s.name["v(n".len()..s.name.len() - 1].parse().unwrap();
            let idx = *layout.index.get(&raw).unwrap();
            let got = s.real().unwrap()[0];
            max_diff = max_diff.max((got - dense[idx]).abs());
        }
        assert!(
            max_diff <= 1e-12,
            "ordered engine vs dense max |Δ| = {max_diff} (> 1e-12)"
        );
        for &leaf in &leaf_nets {
            let name = format!("v(n{})", c.resolve(leaf).0);
            let got = sigs
                .iter()
                .find(|s| s.name == name)
                .unwrap()
                .real()
                .unwrap()[0];
            assert!(
                (got - 5.0).abs() <= 1e-9,
                "leaf {leaf:?} = {got} V, expected 5 V"
            );
        }
    }

    /// A large-net *transient* also takes the ordered path, and reuses one symbolic
    /// factorization across all its steps (the structure never changes). The result
    /// must still match the forced-dense per-step reference to ≤1e-12. A purely
    /// resistive hub has no reactive state, so every step is the same DC solve — its
    /// hub stays at 10 V and leaves at 5 V for the whole run.
    #[test]
    fn large_transient_reuses_ordered_factorization() {
        let leaves = 400; // ≥ KLU_THRESHOLD
        let (c, hub_net, _leaves) = hub(leaves);
        let sigs = Engine::new()
            .run(
                &c,
                &Analysis::Transient {
                    duration: 1e-3,
                    step: Some(1e-4), // 10 steps, each reusing the factorization
                },
            )
            .unwrap();
        let hub_name = format!("v(n{})", c.resolve(hub_net).0);
        let hub = sigs.iter().find(|s| s.name == hub_name).unwrap();
        for &y in hub.real().unwrap() {
            assert!(
                (y - 10.0).abs() <= 1e-12,
                "hub drifted to {y} V across steps"
            );
        }
    }

    // --- Plugin device-model API — CP-SIM-M4.1 -----------------------------

    /// A custom two-terminal nonlinear device living *outside* the engine's
    /// built-ins — the plugin gate. Its current law is `I(V) = k·V²` for the
    /// branch voltage `V = v(a) − v(b)`, flowing a → b. A real external crate would
    /// define exactly this: a type implementing the public [`NonlinearDevice`]
    /// contract, registered with [`Engine::with_device`].
    struct SquareLaw {
        nets: [NetId; 2],
        k: f64,
    }

    impl NonlinearDevice for SquareLaw {
        fn terminals(&self) -> &[NetId] {
            &self.nets
        }

        fn state_len(&self) -> usize {
            0 // smooth polynomial law — no junction limiting needed
        }

        fn linearize(&self, v: &[f64], _state: &mut [f64], _limit: bool) -> Companion {
            let vd = v[0] - v[1];
            let i = self.k * vd * vd; // I = k·V²
            let g = 2.0 * self.k * vd; // dI/dV
                                       // Multiport Norton source, exactly as a built-in device forms it.
            let i_eq = g * vd - i;
            Companion {
                i: vec![i, -i],
                i_eq: vec![i_eq, -i_eq],
                g: vec![vec![g, -g], vec![-g, g]],
            }
        }
    }

    /// The `CP-SIM-M4.1` acceptance gate: a device defined entirely in this test
    /// module (not an engine built-in) participates in a real operating-point
    /// solve through the public registry, and the result matches hand analysis.
    ///
    /// Circuit: `V1 = 2 V — R1(1 kΩ) — n — XSQ(k = 1e-3) — gnd`, XSQ a
    /// [`SquareLaw`]. KCL at `n`: `(2 − Vn)/1000 = 1e-3·Vn²`, i.e.
    /// `Vn² + Vn − 2 = 0`, whose positive root is exactly `Vn = 1 V`. The node
    /// always has a resistive path, so the plain Newton path cracks it.
    #[test]
    fn plugin_square_law_device_matches_hand_analysis() {
        let mut c = Circuit::new("plugin_sqlaw");
        let vin = c.new_net();
        let n = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            v("2", Unit::Volt),
            vec![vin, NetId::GROUND],
        );
        c.add_part("R1", PartKind::Resistor, v("1k", Unit::Ohm), vec![vin, n]);
        // The plugin device: PartKind::Other("sqlaw"), its value carrying k.
        c.add_part(
            "XSQ",
            PartKind::Other("sqlaw".into()),
            Some(Value::new(1e-3, Unit::Ampere)),
            vec![n, NetId::GROUND],
        );

        // An unregistered "sqlaw" is rejected — the registry is the only way in.
        assert!(matches!(
            Engine::new()
                .run(&c, &Analysis::OperatingPoint)
                .unwrap_err(),
            SimError::Unsupported(_)
        ));

        // Register the plugin factory and solve the real operating point.
        let engine = Engine::new().with_device("sqlaw", |part: &Part| {
            let (a, b) = terminals2(part)?;
            let k = part.value.map(|v| v.magnitude).unwrap_or(1e-3);
            Ok(Box::new(SquareLaw { nets: [a, b], k }) as Box<dyn NonlinearDevice>)
        });
        let sigs = engine.run(&c, &Analysis::OperatingPoint).unwrap();

        let name = format!("v(n{})", c.resolve(n).0);
        let vn = sigs
            .iter()
            .find(|s| s.name == name)
            .unwrap()
            .real()
            .unwrap()[0];
        assert!(
            (vn - 1.0).abs() <= 1e-9,
            "plugin device op-point Vn = {vn} V, expected exactly 1 V"
        );
        // KCL closes with the device's own law: the resistor current equals k·Vn².
        let i_r = (2.0 - vn) / 1000.0;
        let i_dev = 1e-3 * vn * vn;
        assert!(
            (i_r - i_dev).abs() <= 1e-12,
            "KCL mismatch: I_R = {i_r} A vs I_dev = {i_dev} A"
        );
    }

    /// The plugin device also flows through the other analyses the registry threads
    /// — here a transient to the same steady state (every sample at the op-point
    /// `Vn = 1 V`, since the sources are DC) — proving `Engine::run` stamps it in
    /// NR / transient, not only the bare operating point.
    #[test]
    fn plugin_device_flows_through_transient() {
        let mut c = Circuit::new("plugin_sqlaw_tran");
        let vin = c.new_net();
        let n = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            v("2", Unit::Volt),
            vec![vin, NetId::GROUND],
        );
        c.add_part("R1", PartKind::Resistor, v("1k", Unit::Ohm), vec![vin, n]);
        c.add_part(
            "XSQ",
            PartKind::Other("sqlaw".into()),
            Some(Value::new(1e-3, Unit::Ampere)),
            vec![n, NetId::GROUND],
        );
        let engine = Engine::new().with_device("sqlaw", |part: &Part| {
            let (a, b) = terminals2(part)?;
            let k = part.value.map(|v| v.magnitude).unwrap_or(1e-3);
            Ok(Box::new(SquareLaw { nets: [a, b], k }) as Box<dyn NonlinearDevice>)
        });
        let sigs = engine
            .run(
                &c,
                &Analysis::Transient {
                    duration: 1e-3,
                    step: Some(1e-4),
                },
            )
            .expect("plugin device must stamp in the transient path");
        let name = format!("v(n{})", c.resolve(n).0);
        let sig = sigs.iter().find(|s| s.name == name).unwrap();
        for &vn in sig.real().unwrap() {
            assert!(
                (vn - 1.0).abs() <= 1e-9,
                "plugin transient Vn = {vn} V, expected the op-point 1 V"
            );
        }
    }

    /// Below the threshold the engine keeps the dense path and its byte-exact
    /// prior results; a small ladder's op point is unchanged and still matches the
    /// forced dense solve. (Regression guard that dispatch did not perturb small
    /// circuits.)
    #[test]
    fn small_ladder_stays_dense_and_unchanged() {
        let n = 4; // size 6 < SPARSE_THRESHOLD → dense path
        let (c, _vin, nodes) = ladder(n);
        let sigs = Engine::new().run(&c, &Analysis::OperatingPoint).unwrap();
        let dense = bench_support::dc_solve_dense(&c).unwrap();
        let layout = Layout::build(&c);
        for &net in &nodes {
            let idx = layout.node_of(&c, net).unwrap();
            let name = format!("v(n{})", c.resolve(net).0);
            let got = sigs
                .iter()
                .find(|s| s.name == name)
                .unwrap()
                .real()
                .unwrap()[0];
            assert_eq!(got, dense[idx]);
        }
    }

    // --- Mutual inductance / transformers — CP-SIM-M6.1 --------------------

    /// Peak-to-peak span of a trace — the transient amplitude the transformer gate
    /// reads off the primary and secondary node waveforms.
    fn vpp(y: &[f64]) -> f64 {
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for &v in y {
            lo = lo.min(v);
            hi = hi.max(v);
        }
        hi - lo
    }

    /// A 2:1 step-down transformer: a 10 Vpp / 1 kHz sine through a tiny series
    /// resistor into the primary winding `L1`, a secondary winding `L2` loaded by
    /// 1 kΩ, with `L1 = 4·L2` so the turns ratio N1:N2 = 2:1 (`√(L2/L1) = 0.5`). The
    /// two windings are magnetically coupled with coefficient `k`. Returns the
    /// circuit plus the primary-winding node and secondary-winding node.
    fn transformer(k: f64) -> (Circuit, NetId, NetId) {
        let mut c = Circuit::new("xfmr");
        let vin = c.new_net();
        let p = c.new_net(); // across L1 (primary winding)
        let sec = c.new_net(); // across L2 (secondary winding)
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            None,
            vec![vin, NetId::GROUND],
        );
        c.set_waveform(
            "V1",
            Waveform::Sine {
                amplitude: 5.0, // 10 Vpp
                freq: 1_000.0,
                offset: 0.0,
                phase_deg: 0.0,
            },
        );
        c.add_part("Rp", PartKind::Resistor, v("1", Unit::Ohm), vec![vin, p]);
        c.add_part(
            "L1",
            PartKind::Inductor,
            Some(Value::new(400e-3, Unit::Henry)),
            vec![p, NetId::GROUND],
        );
        c.add_part(
            "L2",
            PartKind::Inductor,
            Some(Value::new(100e-3, Unit::Henry)),
            vec![sec, NetId::GROUND],
        );
        c.add_part(
            "Rload",
            PartKind::Resistor,
            v("1k", Unit::Ohm),
            vec![sec, NetId::GROUND],
        );
        assert!(c.couple("L1", "L2", k), "coupling L1–L2 must succeed");
        (c, p, sec)
    }

    fn trace<'a>(sigs: &'a [Signal], c: &Circuit, net: NetId) -> &'a [f64] {
        let name = format!("v(n{})", c.resolve(net).0);
        sigs.iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("signal {name} present"))
            .real()
            .unwrap()
    }

    /// The acceptance gate: an ideal (`k = 1`) 2:1 transformer steps a 10 Vpp
    /// primary sine down to ~5 Vpp on the secondary in transient. With `k = 1` the
    /// flux linkages are perfectly proportional, so the secondary equals *half the
    /// primary-winding voltage at every instant* — the exact ideal-transformer
    /// identity, which the Backward-Euler branch stamp preserves to solver roundoff.
    #[test]
    fn ideal_transformer_steps_2to1_in_transient() {
        let (c, p, sec) = transformer(1.0);
        let sigs = Engine::new()
            .run(
                &c,
                &Analysis::Transient {
                    duration: 3e-3, // three 1 kHz periods
                    step: Some(2e-6),
                },
            )
            .unwrap();
        let pv = trace(&sigs, &c, p).to_vec();
        let sv = trace(&sigs, &c, sec).to_vec();

        // Ideal transformer: V(secondary) = V(primary) / 2 at every sample.
        for (&vp, &vs) in pv.iter().zip(&sv) {
            assert!(
                (vs - vp / 2.0).abs() <= 1e-9 * (1.0 + vp.abs()),
                "secondary {vs} != primary/2 {}",
                vp / 2.0
            );
        }

        // ~10 Vpp in, ~5 Vpp out — and the amplitude ratio is exactly the turns ratio.
        let (pp, sp) = (vpp(&pv), vpp(&sv));
        assert!((pp - 10.0).abs() <= 0.3, "primary Vpp {pp}, expected ~10");
        assert!((sp - 5.0).abs() <= 0.15, "secondary Vpp {sp}, expected ~5");
        assert!(
            (sp / pp - 0.5).abs() <= 1e-6,
            "secondary/primary {} != turns ratio 0.5",
            sp / pp
        );
    }

    /// `k < 1` shows measurable leakage: the same transformer with reduced coupling
    /// delivers a strictly smaller secondary amplitude (flux that fails to link the
    /// secondary is lost to leakage inductance).
    #[test]
    fn coupling_below_unity_shows_leakage() {
        let secondary_vpp = |k| {
            let (c, _p, sec) = transformer(k);
            let sigs = Engine::new()
                .run(
                    &c,
                    &Analysis::Transient {
                        duration: 3e-3,
                        step: Some(2e-6),
                    },
                )
                .unwrap();
            vpp(trace(&sigs, &c, sec))
        };
        let ideal = secondary_vpp(1.0);
        let leaky = secondary_vpp(0.8);
        assert!(leaky > 0.0, "secondary still swings with k=0.8 ({leaky})");
        assert!(
            leaky < ideal - 0.05,
            "k=0.8 secondary ({leaky}) should be measurably below the ideal ({ideal})"
        );
    }

    /// An AC sweep transfer matches the turns ratio: the ideal (`k = 1`) transformer
    /// has `V(secondary) = V(primary)/2` at every frequency, i.e. the turns ratio
    /// `√(L2/L1) = 0.5`, flat across the band (no reactive pole appears in the
    /// ratio because the coupling is lossless).
    #[test]
    fn ac_transfer_matches_turns_ratio() {
        let (c, p, sec) = transformer(1.0);
        let sigs = Engine::new()
            .run(
                &c,
                &Analysis::Ac {
                    f_start: 100.0,
                    f_stop: 100_000.0,
                    points_per_decade: 5,
                },
            )
            .unwrap();
        let name = |net: NetId| format!("v(n{})", c.resolve(net).0);
        let pz = sigs
            .iter()
            .find(|s| s.name == name(p))
            .unwrap()
            .complex()
            .unwrap();
        let sz = sigs
            .iter()
            .find(|s| s.name == name(sec))
            .unwrap()
            .complex()
            .unwrap();
        let turns = (100e-3_f64 / 400e-3).sqrt(); // √(L2/L1) = 0.5
        for (zp, zs) in pz.iter().zip(sz) {
            let half = *zp * Complex::new(0.5, 0.0);
            assert!(
                (*zs - half).magnitude() <= 1e-9,
                "AC secondary {zs:?} != primary/2 {half:?}"
            );
            assert!(
                (zs.magnitude() / zp.magnitude() - turns).abs() <= 1e-9,
                "AC transfer {} != turns ratio {turns}",
                zs.magnitude() / zp.magnitude()
            );
        }
    }

    // --- Nonlinear convergence aids (DC op-point) — CP-SIM-M6.2 ------------

    /// The acceptance gate: a tight nonlinear loop plain Newton–Raphson cannot start
    /// on, but gmin stepping cracks.
    ///
    /// Two diode-connected NMOS are stacked between a current source and ground
    /// (`I1` → `d1`; `M1` drain=gate=`d1`, source=`d2`; `M2` drain=gate=`d2`,
    /// source=gnd). At the cold-start guess (all nodes 0 V) both channels are cut
    /// off with zero transconductance, so the `d1`/`d2` rows are all-zero and plain
    /// NR hits a singular Jacobian. gmin stepping props the nodes up and walks the
    /// bias onto the channels. Diode-connected ⇒ always saturated: with defaults
    /// `K = 1 mA/V²`, `Vth = 1 V`, each FET at `I = 1 mA` has `Vov = 1 V`, so
    /// `v(d2) = Vth + Vov = 2 V` and `v(d1) = 2·(Vth + Vov) = 4 V`.
    #[test]
    fn gmin_stepping_cracks_a_tight_nonlinear_loop() {
        let mut c = Circuit::new("fet_stack");
        let d1 = c.new_net();
        let d2 = c.new_net();
        c.add_part(
            "I1",
            PartKind::CurrentSource,
            v("1m", Unit::Ampere),
            vec![NetId::GROUND, d1],
        );
        c.add_part(
            "M1",
            PartKind::Mosfet(Polarity::N, MosfetModel::default()),
            None,
            vec![d1, d1, d2],
        );
        c.add_part(
            "M2",
            PartKind::Mosfet(Polarity::N, MosfetModel::default()),
            None,
            vec![d2, d2, NetId::GROUND],
        );

        // Plain NR alone cannot start (singular at the cold guess).
        assert!(
            plain_nr_dc(&c).is_err(),
            "plain NR unexpectedly solved the stacked loop — the gate proves nothing"
        );

        let layout = Layout::build(&c);
        let size = layout.n_nodes() + layout.vsources.len() + layout.inductors.len();
        let devices = collect_nonlinear(&c).unwrap();
        let stamp_at = |sys: &mut System, alpha: f64| -> Result<(), SimError> {
            stamp_resistors(sys, &c, &layout)?;
            stamp_vsources(sys, &c, &layout, None, alpha)?;
            stamp_current_sources(sys, &c, &layout, None, alpha)?;
            stamp_inductor_shorts(sys, &c, &layout)?;
            Ok(())
        };
        // gmin stepping alone reaches the point.
        let g = gmin_stepping(&c, &layout, size, &devices, &stamp_at)
            .unwrap()
            .expect("gmin stepping converged");
        // …and so does the full aided operating point.
        let x = solve_dc(&c, &layout).unwrap();

        let m = MosfetModel::default();
        let vov = (1e-3_f64 / m.kp).sqrt(); // I = K·Vov² ⇒ Vov = 1 V
        let (want_d1, want_d2) = (2.0 * (m.vth + vov), m.vth + vov); // 4 V, 2 V
        let (gi1, gi2) = (
            layout.node_of(&c, d1).unwrap(),
            layout.node_of(&c, d2).unwrap(),
        );
        for (label, sol) in [("gmin", &g), ("aided", &x)] {
            assert!(
                (sol[gi1] - want_d1).abs() <= 1e-6,
                "{label}: v(d1) = {} V, expected {want_d1} V",
                sol[gi1]
            );
            assert!(
                (sol[gi2] - want_d2).abs() <= 1e-6,
                "{label}: v(d2) = {} V, expected {want_d2} V",
                sol[gi2]
            );
        }
    }

    /// An easy (plain-convergent) nonlinear circuit must still take the direct
    /// Newton path with no extra iterations — the convergence aids never engage.
    ///
    /// `solve_dc` tries plain NR first and returns its result verbatim when it
    /// succeeds; only on failure does it warm-start the gmin / source continuations,
    /// which would land a *stepped* result. So for a circuit with no controlled
    /// sources or inductors — where `plain_nr_dc` and `solve_dc`'s first attempt
    /// stamp the identical system — a **bit-for-bit** match of the two proves the
    /// direct path was taken and the aids added nothing.
    #[test]
    fn easy_circuit_takes_the_direct_path_bit_for_bit() {
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
        c.add_part(
            "D1",
            PartKind::Diode(DiodeModel::default()),
            None,
            vec![mid, NetId::GROUND],
        );

        let layout = Layout::build(&c);
        let direct = plain_nr_dc(&c).expect("plain NR cracks an easy bias");
        let aided = solve_dc(&c, &layout).expect("aided solve");
        assert_eq!(
            direct, aided,
            "an easy circuit must take the direct NR path, bit-for-bit (no aid engaged)"
        );
    }

    // --- CP-SIM-M7.1: transfer function + pole/zero -------------------------

    /// The RC low-pass `.TF` gate: a single real pole at `s = −1/(RC)` (the −3 dB
    /// corner), a unity DC gain, and no finite zeros. `R = 1 kΩ`, `C = 1 µF` →
    /// pole at exactly `−1000 rad/s`.
    #[test]
    fn tf_rc_lowpass_reports_its_single_pole() {
        let (r, cap) = (1_000.0, 1e-6);
        let mut c = Circuit::new("rc");
        let vin = c.new_net();
        let out = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            v("1", Unit::Volt),
            vec![vin, NetId::GROUND],
        );
        c.add_part(
            "R1",
            PartKind::Resistor,
            Some(Value::new(r, Unit::Ohm)),
            vec![vin, out],
        );
        c.add_part(
            "C1",
            PartKind::Capacitor,
            Some(Value::new(cap, Unit::Farad)),
            vec![out, NetId::GROUND],
        );

        let tf = Engine::new()
            .transfer_function(&c, out, "V1")
            .expect("RC transfer function");

        // Exactly one finite pole, real, at −1/(RC) = −1000 rad/s.
        assert_eq!(tf.poles.len(), 1, "RC low-pass has one pole");
        let p = tf.poles[0];
        assert!(p.im.abs() < 1e-6, "the RC pole is real, got {p:?}");
        let expected = -1.0 / (r * cap);
        assert!(
            (p.re - expected).abs() < 1e-6 * expected.abs(),
            "RC pole {} not at −1/(RC) = {expected}",
            p.re
        );
        // DC gain is unity (the capacitor is open at DC), and no finite zeros.
        assert!((tf.dc_gain - 1.0).abs() < 1e-12, "RC DC gain is 1");
        assert_eq!(
            tf.zeros,
            Some(Vec::new()),
            "a first-order low-pass has no finite zeros"
        );
        // The −3 dB corner frequency follows from the pole: fc = |p|/(2π) ≈ 159 Hz.
        let fc = expected.abs() / TAU;
        assert!((fc - 159.154_943).abs() < 1e-3, "corner fc ≈ 159.15 Hz");
    }

    /// The resistive-divider `.TF` gate: gain, input resistance, and output
    /// resistance all match hand analysis **exactly** (resistive, so IEEE-exact).
    /// `R1 = 1 kΩ`, `R2 = 3 kΩ`: gain `R2/(R1+R2) = 0.75`, `R_in = R1+R2 = 4 kΩ`,
    /// `R_out = R1‖R2 = 750 Ω`, and no poles (purely resistive).
    #[test]
    fn tf_resistive_divider_matches_hand_analysis() {
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
            v("3k", Unit::Ohm),
            vec![mid, NetId::GROUND],
        );

        let tf = Engine::new()
            .transfer_function(&c, mid, "V1")
            .expect("divider transfer function");

        assert_eq!(tf.dc_gain, 0.75, "divider gain R2/(R1+R2)");
        assert_eq!(tf.input_impedance, 4000.0, "R_in = R1 + R2");
        assert_eq!(tf.output_impedance, 750.0, "R_out = R1 ‖ R2");
        assert!(tf.poles.is_empty(), "a resistive divider has no poles");
    }

    /// A series-RLC `.TF` gives a complex-conjugate pole pair at the undamped
    /// natural frequency for a lightly-damped choice, sanity-checking the complex
    /// branch of the eigensolver on a real circuit. `L = 1 mH`, `C = 1 µF` →
    /// `ω₀ = 1/√(LC) ≈ 31623 rad/s`; a small series `R` keeps the poles complex.
    #[test]
    fn tf_series_rlc_has_a_complex_pole_pair() {
        let (rr, ll, cc) = (10.0, 1e-3, 1e-6);
        let mut c = Circuit::new("rlc");
        let vin = c.new_net();
        let n1 = c.new_net();
        let out = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            v("1", Unit::Volt),
            vec![vin, NetId::GROUND],
        );
        c.add_part(
            "R1",
            PartKind::Resistor,
            Some(Value::new(rr, Unit::Ohm)),
            vec![vin, n1],
        );
        c.add_part(
            "L1",
            PartKind::Inductor,
            Some(Value::new(ll, Unit::Henry)),
            vec![n1, out],
        );
        c.add_part(
            "C1",
            PartKind::Capacitor,
            Some(Value::new(cc, Unit::Farad)),
            vec![out, NetId::GROUND],
        );

        let tf = Engine::new()
            .transfer_function(&c, out, "V1")
            .expect("RLC transfer function");

        assert_eq!(tf.poles.len(), 2, "RLC has two poles");
        // A lightly-damped pair: complex conjugates, |s| ≈ ω₀ = 1/√(LC).
        let w0 = 1.0 / (ll * cc).sqrt();
        assert!(
            tf.poles.iter().all(|p| p.im.abs() > 1.0),
            "lightly-damped RLC poles are complex, got {:?}",
            tf.poles
        );
        for p in &tf.poles {
            assert!(p.re < 0.0, "a passive circuit's poles are in the LHP");
            assert!(
                (p.magnitude() - w0).abs() < 1e-3 * w0,
                "|pole| {} not ≈ ω₀ = {w0}",
                p.magnitude()
            );
        }
        // DC gain unity (inductor shorts, capacitor opens at DC → V(out) = V(in)).
        assert!((tf.dc_gain - 1.0).abs() < 1e-9, "RLC DC gain is 1");
    }

    /// A `.TF` request naming a missing or non-source input is rejected cleanly.
    #[test]
    fn tf_rejects_a_bad_input_source() {
        let mut c = Circuit::new("divider");
        let vin = c.new_net();
        let mid = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            v("1", Unit::Volt),
            vec![vin, NetId::GROUND],
        );
        c.add_part("R1", PartKind::Resistor, v("1k", Unit::Ohm), vec![vin, mid]);
        c.add_part(
            "R2",
            PartKind::Resistor,
            v("1k", Unit::Ohm),
            vec![mid, NetId::GROUND],
        );
        let eng = Engine::new();
        assert!(matches!(
            eng.transfer_function(&c, mid, "V9"),
            Err(SimError::UnknownPart(_))
        ));
        assert!(matches!(
            eng.transfer_function(&c, mid, "R1"),
            Err(SimError::Unsupported(_))
        ));
    }

    // --- CP-SIM-M7.2: transmission lines ------------------------------------

    /// The far-end signal of a transmission line (`"v(n{resolved})"`).
    fn far_sig<'a>(sigs: &'a [Signal], c: &Circuit, net: NetId) -> &'a Signal {
        let name = format!("v(n{})", c.resolve(net).0);
        sigs.iter()
            .find(|s| s.name == name)
            .expect("far-end signal")
    }

    /// A step source → Rs → lossless line (Z0, Td) → far end, with ports referenced
    /// to ground. `rl = Some(r)` loads the far end; `None` leaves it open.
    fn line_circuit(z0: f64, td: f64, rs: f64, rl: Option<f64>) -> (Circuit, NetId, NetId) {
        let mut c = Circuit::new("tline");
        let src = c.new_net();
        let a1 = c.new_net();
        let b1 = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            v("0", Unit::Volt),
            vec![src, NetId::GROUND],
        );
        // A clean 0 → 1 V step at t = 0.
        c.set_waveform(
            "V1",
            Waveform::Pulse {
                v1: 0.0,
                v2: 1.0,
                delay: 0.0,
                width: 1.0,
                period: 0.0,
            },
        );
        c.add_part(
            "Rs",
            PartKind::Resistor,
            Some(Value::new(rs, Unit::Ohm)),
            vec![src, a1],
        );
        if let Some(r) = rl {
            c.add_part(
                "RL",
                PartKind::Resistor,
                Some(Value::new(r, Unit::Ohm)),
                vec![b1, NetId::GROUND],
            );
        }
        assert!(c.add_line((a1, NetId::GROUND), (b1, NetId::GROUND), z0, td));
        (c, a1, b1)
    }

    /// Sample a time signal at the step nearest time `t`.
    fn sample_at(sig: &Signal, t: f64) -> f64 {
        let i = sig
            .x
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| (**a - t).abs().partial_cmp(&(**b - t).abs()).unwrap())
            .map(|(i, _)| i)
            .unwrap();
        sig.real().unwrap()[i]
    }

    /// Gate: a source-matched lossless line delays a step by its propagation time —
    /// the far end is quiet until `t = Td`, then settles to the matched half-step.
    #[test]
    fn lossless_line_delays_a_matched_step() {
        let (z0, td) = (50.0, 1e-9);
        // Matched both ends: Rs = Z0, RL = Z0 → far end rises to Vstep/2 at t = Td.
        let (c, _a1, b1) = line_circuit(z0, td, z0, Some(z0));
        let sigs = Engine::new()
            .run(
                &c,
                &Analysis::Transient {
                    duration: 4.0 * td,
                    step: Some(td / 100.0),
                },
            )
            .expect("line transient");
        let far = far_sig(&sigs, &c, b1);

        // Quiet well before the delay, settled to ~0.5 V well after it.
        assert!(
            sample_at(far, 0.5 * td).abs() < 0.02,
            "far end must be quiet before Td, got {}",
            sample_at(far, 0.5 * td)
        );
        assert!(
            (sample_at(far, 2.5 * td) - 0.5).abs() < 0.02,
            "matched far end must settle to Vstep/2 after Td, got {}",
            sample_at(far, 2.5 * td)
        );
        // The rise happens around Td: still low at 0.9·Td, risen by 1.1·Td.
        assert!(
            sample_at(far, 0.9 * td) < 0.25,
            "not yet risen just before Td"
        );
        assert!(sample_at(far, 1.1 * td) > 0.25, "risen just after Td");
    }

    /// Gate: an unterminated (open) lossless line reflects — the open far end sees
    /// the incident half-step *double* to the full step, where the matched line
    /// held half. A source-matched near end absorbs the return, so it stays there.
    #[test]
    fn lossless_line_open_end_reflects() {
        let (z0, td) = (50.0, 1e-9);
        let (c, _a1, b1) = line_circuit(z0, td, z0, None); // open far end
        let sigs = Engine::new()
            .run(
                &c,
                &Analysis::Transient {
                    duration: 4.0 * td,
                    step: Some(td / 100.0),
                },
            )
            .expect("open line transient");
        let far = far_sig(&sigs, &c, b1);

        assert!(
            sample_at(far, 0.5 * td).abs() < 0.02,
            "open far end quiet before Td"
        );
        // Full reflection: the incident 0.5 V doubles to ~1.0 V (vs. 0.5 matched).
        assert!(
            (sample_at(far, 2.5 * td) - 1.0).abs() < 0.03,
            "open far end must reflect to ~Vstep (doubling), got {}",
            sample_at(far, 2.5 * td)
        );
    }

    /// Gate: an AC sweep shows the line's **periodic** magnitude response. Every
    /// frequency dependence of a resistor-plus-lossless-line network is carried by
    /// `e^{−jωT}`, so the response repeats exactly with period `1/T` in frequency
    /// (and genuinely varies within a period). Each AC run pins its last sample to
    /// `f_stop`, so three runs read the response at three exact frequencies.
    #[test]
    fn lossless_line_ac_is_periodic_in_frequency() {
        let (z0, td) = (50.0, 1e-9);
        // Mismatched both ends (Rs ≪ Z0, RL ≫ Z0) → a strongly frequency-dependent
        // far end whose magnitude repeats with period 1/T.
        let (mut c, _a1, b1) = line_circuit(z0, td, 10.0, Some(400.0));
        c.set_waveform(
            "V1",
            Waveform::Sine {
                amplitude: 1.0,
                freq: 1.0,
                offset: 0.0,
                phase_deg: 0.0,
            },
        );

        let mag_at = |f_stop: f64| -> f64 {
            let sigs = Engine::new()
                .run(
                    &c,
                    &Analysis::Ac {
                        f_start: 1e8,
                        f_stop,
                        points_per_decade: 30,
                    },
                )
                .expect("line AC");
            let far = far_sig(&sigs, &c, b1);
            far.complex().unwrap().last().unwrap().magnitude()
        };

        let period = 1.0 / td; // 1 GHz
        let f0 = 3e8;
        let base = mag_at(f0);
        let one_period = mag_at(f0 + period);
        let quarter = mag_at(f0 + period / 4.0);

        // One full period later, the magnitude is (numerically) the same…
        assert!(
            (base - one_period).abs() <= 1e-6 * base.max(1.0),
            "response not periodic with 1/T: {base} vs {one_period}"
        );
        // …while a quarter period away it genuinely differs (not a flat response).
        assert!(
            (base - quarter).abs() > 0.05 * base.max(1e-3),
            "response should vary within a period: {base} vs {quarter}"
        );
    }

    /// A lumped-LC **lossy** line stamps and solves in both transient and AC: the
    /// transient reaches a sensible delayed DC level and the AC sweep returns finite
    /// magnitudes at every point. (The lossless gates pin the numerics; this fixes
    /// that the lossy form is wired through both engines.)
    #[test]
    fn lossy_line_runs_in_transient_and_ac() {
        let (z0, td) = (50.0, 1e-9);
        let mut c = Circuit::new("lossy");
        let src = c.new_net();
        let a1 = c.new_net();
        let b1 = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            v("1", Unit::Volt),
            vec![src, NetId::GROUND],
        );
        c.add_part(
            "Rs",
            PartKind::Resistor,
            Some(Value::new(z0, Unit::Ohm)),
            vec![src, a1],
        );
        c.add_part(
            "RL",
            PartKind::Resistor,
            Some(Value::new(1e6, Unit::Ohm)),
            vec![b1, NetId::GROUND],
        );
        // A lossy line with small series R and shunt G.
        assert!(c.add_lossy_line((a1, NetId::GROUND), (b1, NetId::GROUND), z0, td, 5.0, 1e-6));

        // DC source → transient settles; the far end ends between 0 and the rail.
        let tr = Engine::new()
            .run(
                &c,
                &Analysis::Transient {
                    duration: 50.0 * td,
                    step: Some(td / 20.0),
                },
            )
            .expect("lossy line transient");
        let far = far_sig(&tr, &c, b1);
        let last = *far.real().unwrap().last().unwrap();
        assert!(
            (0.0..=1.0).contains(&last) && last > 0.3,
            "lossy far end should settle to a sensible DC level, got {last}"
        );

        // AC: finite magnitudes everywhere (needs a sine drive to excite it).
        c.set_waveform(
            "V1",
            Waveform::Sine {
                amplitude: 1.0,
                freq: 1.0,
                offset: 0.0,
                phase_deg: 0.0,
            },
        );
        let ac = Engine::new()
            .run(
                &c,
                &Analysis::Ac {
                    f_start: 1e6,
                    f_stop: 1e9,
                    points_per_decade: 10,
                },
            )
            .expect("lossy line AC");
        let far = far_sig(&ac, &c, b1);
        assert!(
            far.complex()
                .unwrap()
                .iter()
                .all(|z| z.magnitude().is_finite()),
            "lossy AC magnitudes must be finite"
        );
    }

    /// A transmission line with the adaptive (stepless) transient is rejected
    /// cleanly — it needs the uniform history grid of a fixed timestep.
    #[test]
    fn line_rejects_adaptive_transient() {
        let (c, _a1, _b1) = line_circuit(50.0, 1e-9, 50.0, Some(50.0));
        let err = Engine::new().run(
            &c,
            &Analysis::Transient {
                duration: 4e-9,
                step: None,
            },
        );
        assert!(matches!(err, Err(SimError::Unsupported(_))));
    }

    /// Gate (`CP-SIM-M8.1`, Finding 2): a fixed timestep coarser than a lossless
    /// line's delay would push the delayed port lookback past the committed history,
    /// silently under-resolving the travelling wave. It must be rejected explicitly,
    /// while a step at or below the delay still runs.
    #[test]
    fn line_rejects_timestep_coarser_than_delay() {
        let td = 1e-9;
        let (c, _a1, _b1) = line_circuit(50.0, td, 50.0, Some(50.0));
        // Coarser than the delay → clean rejection, not a quietly wrong waveform.
        let coarse = Engine::new().run(
            &c,
            &Analysis::Transient {
                duration: 10.0 * td,
                step: Some(2.0 * td),
            },
        );
        assert!(
            matches!(coarse, Err(SimError::Unsupported(_))),
            "a step coarser than the line delay must be rejected, got {coarse:?}"
        );
        // A step at the delay (and finer) still solves.
        assert!(
            Engine::new()
                .run(
                    &c,
                    &Analysis::Transient {
                        duration: 10.0 * td,
                        step: Some(td),
                    },
                )
                .is_ok(),
            "a delay-matched timestep must still run"
        );
    }

    // --- CP-SIM-M8.1: DC-floating-node small-signal analyses ----------------

    /// Gate (`CP-SIM-M8.1`, Finding 1): a `V1(sine)—C1—mid—C2—gnd` capacitive divider
    /// (C1 = C2 = 1 nF) solves in AC with `v(mid)` at exactly half the 1∠0 input at
    /// every frequency. The mid-node floats at DC (both caps open), which used to make
    /// the DC-bias gate error `Singular`; the small-signal matrix (`jωC₁`, `jωC₂`) is
    /// well-conditioned, so skipping the bias for this device-free circuit lets it solve.
    #[test]
    fn ac_solves_a_capacitive_divider() {
        let mut c = Circuit::new("cap_divider");
        let inp = c.new_net();
        let mid = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            None,
            vec![inp, NetId::GROUND],
        );
        c.set_waveform(
            "V1",
            Waveform::Sine {
                amplitude: 1.0,
                freq: 1_000.0,
                offset: 0.0,
                phase_deg: 0.0,
            },
        );
        c.add_part(
            "C1",
            PartKind::Capacitor,
            Some(Value::new(1e-9, Unit::Farad)),
            vec![inp, mid],
        );
        c.add_part(
            "C2",
            PartKind::Capacitor,
            Some(Value::new(1e-9, Unit::Farad)),
            vec![mid, NetId::GROUND],
        );
        let sigs = Engine::new()
            .run(
                &c,
                &Analysis::Ac {
                    f_start: 10.0,
                    f_stop: 100_000.0,
                    points_per_decade: 10,
                },
            )
            .expect("capacitive divider must solve in AC despite the DC-floating mid-node");
        let name = format!("v(n{})", c.resolve(mid).0);
        let z = sigs
            .iter()
            .find(|s| s.name == name)
            .expect("mid-node signal")
            .complex()
            .unwrap();
        // C1/(C1+C2) = 0.5 at every frequency (frequency-independent divider).
        for h in z {
            assert!(
                (h.magnitude() - 0.5).abs() < 1e-9,
                "capacitive divider mid must be 0.5 of the input, got {}",
                h.magnitude()
            );
        }
    }

    /// Gate (`CP-SIM-M8.1`, Finding 1): a source-matched, open-ended lossless line
    /// solves in AC. Its far end is connected only through the line, so it floats at
    /// DC (the DC solve stamps no line) and used to error `Singular`; the AC assembly
    /// stamps the line's `e^{−jωT}` companion, so the node is well-defined there.
    #[test]
    fn ac_solves_a_source_matched_open_line() {
        let (z0, td) = (50.0, 1e-9);
        let (mut c, _a1, b1) = line_circuit(z0, td, z0, None); // matched source, open far end
        c.set_waveform(
            "V1",
            Waveform::Sine {
                amplitude: 1.0,
                freq: 1.0,
                offset: 0.0,
                phase_deg: 0.0,
            },
        );
        let sigs = Engine::new()
            .run(
                &c,
                &Analysis::Ac {
                    f_start: 1e6,
                    f_stop: 1e9,
                    points_per_decade: 10,
                },
            )
            .expect("source-matched open line must solve in AC despite its DC-floating far end");
        let far = far_sig(&sigs, &c, b1);
        let z = far.complex().unwrap();
        assert!(!z.is_empty(), "AC produced no far-end samples");
        assert!(
            z.iter()
                .all(|h| h.magnitude().is_finite() && h.magnitude() > 0.0),
            "open-line far-end magnitudes must be finite and nonzero"
        );
    }

    /// Gate (`CP-SIM-M8.1`, Finding 1): sweeping the source of a capacitive divider in
    /// DC no longer errors `Singular`. The caps are open at DC so the mid-node floats;
    /// the residual-GMIN fallback pins it to 0 V and the sweep returns a trace.
    #[test]
    fn dc_sweep_solves_a_capacitive_divider() {
        let mut c = Circuit::new("cap_divider_dc");
        let inp = c.new_net();
        let mid = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            v("0", Unit::Volt),
            vec![inp, NetId::GROUND],
        );
        c.add_part(
            "C1",
            PartKind::Capacitor,
            Some(Value::new(1e-9, Unit::Farad)),
            vec![inp, mid],
        );
        c.add_part(
            "C2",
            PartKind::Capacitor,
            Some(Value::new(1e-9, Unit::Farad)),
            vec![mid, NetId::GROUND],
        );
        let sigs = Engine::new()
            .run(
                &c,
                &Analysis::DcSweep {
                    source: "V1".into(),
                    start: 0.0,
                    stop: 5.0,
                    step: 1.0,
                },
            )
            .expect("capacitive divider DC sweep must solve via the residual-GMIN fallback");
        let name = format!("v(n{})", c.resolve(mid).0);
        let y = sigs
            .iter()
            .find(|s| s.name == name)
            .expect("mid-node sweep trace")
            .real()
            .unwrap();
        assert_eq!(y.len(), 6, "0..=5 V by 1 V is six points");
        // The floating mid-node is gmin-pinned to ~0 V at every swept level.
        for &vmid in y {
            assert!(
                vmid.abs() < 1e-6,
                "floating mid should pin to ~0 V, got {vmid}"
            );
        }
    }

    // --- Power electronics: controlled switch + buck/boost — CP-SIM-M9.1 ----

    /// The real transient trace of node `net` from a finished run.
    fn node_trace(sigs: &[Signal], c: &Circuit, net: NetId) -> Vec<f64> {
        let name = format!("v(n{})", c.resolve(net).0);
        sigs.iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("node {name} present"))
            .real()
            .unwrap()
            .to_vec()
    }

    /// Mean of a trace's steady-state tail (its last `frac` fraction of samples).
    fn tail_mean(y: &[f64], frac: f64) -> f64 {
        let start = ((1.0 - frac) * y.len() as f64) as usize;
        let tail = &y[start..];
        tail.iter().sum::<f64>() / tail.len() as f64
    }

    #[test]
    fn switch_conducts_when_control_is_high_and_blocks_when_low() {
        // A 10 V source feeds a 1 kΩ load through a controlled switch. A DC control
        // net above the threshold closes it (Ron ⇒ load sees ~10 V); below it, the
        // switch is Roff ⇒ the load node collapses to ~0 V. The control net is an
        // independent source, so it is pinned at the t = 0 solve and read back on
        // the first step.
        let build = |vctrl: &str| {
            let mut c = Circuit::new("sw");
            let (vin, out, ctrl) = (c.new_net(), c.new_net(), c.new_net());
            c.add_part(
                "V1",
                PartKind::VoltageSource,
                v("10", Unit::Volt),
                vec![vin, NetId::GROUND],
            );
            c.add_part(
                "VC",
                PartKind::VoltageSource,
                v(vctrl, Unit::Volt),
                vec![ctrl, NetId::GROUND],
            );
            c.add_part(
                "RL",
                PartKind::Resistor,
                v("1k", Unit::Ohm),
                vec![out, NetId::GROUND],
            );
            assert!(c.add_switch(ctrl, vin, out, 0.01, 1e6, 2.5));
            let sigs = Engine::new()
                .run(
                    &c,
                    &Analysis::Transient {
                        duration: 1e-3,
                        step: Some(5e-5),
                    },
                )
                .expect("switch transient solves");
            *node_trace(&sigs, &c, out).last().unwrap()
        };

        let closed = build("5"); // control high ⇒ switch closed
        let open = build("0"); // control low ⇒ switch open
        assert!(closed > 9.9, "closed switch passes the source: {closed}");
        assert!(open < 0.05, "open switch blocks the source: {open}");
    }

    #[test]
    fn buck_converter_steps_12v_down_to_its_duty_output() {
        // Synchronous-style buck: 12 V in, a PWM-chopped switch into an LC filter
        // with a freewheeling diode, at ~42 % duty. In continuous conduction the
        // output averages to ≈ duty·Vin ≈ 5 V (a little under, from the diode drop).
        let mut c = Circuit::new("buck");
        let (vin, sw, out, ctrl) = (c.new_net(), c.new_net(), c.new_net(), c.new_net());
        c.add_part(
            "VIN",
            PartKind::VoltageSource,
            v("12", Unit::Volt),
            vec![vin, NetId::GROUND],
        );
        // PWM clock on the control net: 200 kHz, 42 % duty, 0 → 5 V.
        let (period, duty) = (5e-6, 0.42);
        c.add_part(
            "VPWM",
            PartKind::VoltageSource,
            v("0", Unit::Volt),
            vec![ctrl, NetId::GROUND],
        );
        assert!(c.set_waveform(
            "VPWM",
            Waveform::Pulse {
                v1: 0.0,
                v2: 5.0,
                delay: 0.0,
                width: duty * period,
                period,
            },
        ));
        // High-side switch vin → sw, freewheel diode gnd → sw, LC filter, load.
        assert!(c.add_switch(ctrl, vin, sw, 0.01, 1e6, 2.5));
        c.add_part(
            "D1",
            PartKind::Diode(DiodeModel::default()),
            None,
            vec![NetId::GROUND, sw], // anode gnd, cathode sw: conducts when sw < 0
        );
        c.add_part(
            "L1",
            PartKind::Inductor,
            v("100u", Unit::Henry),
            vec![sw, out],
        );
        c.add_part(
            "C1",
            PartKind::Capacitor,
            v("2u", Unit::Farad),
            vec![out, NetId::GROUND],
        );
        c.add_part(
            "RL",
            PartKind::Resistor,
            v("5", Unit::Ohm),
            vec![out, NetId::GROUND],
        );

        let sigs = Engine::new()
            .run(
                &c,
                &Analysis::Transient {
                    duration: 400e-6,
                    step: Some(period / 40.0),
                },
            )
            .expect("buck transient solves");

        let out_y = node_trace(&sigs, &c, out);
        let sw_y = node_trace(&sigs, &c, sw);
        let vout = tail_mean(&out_y, 0.25);
        // Stepped down to ≈ 5 V (42 % of 12 V, minus the diode drop), well below Vin.
        assert!(
            (4.2..=5.2).contains(&vout),
            "buck output should settle near 5 V, got {vout}"
        );
        // Steady-state ripple is a small fraction of the output (LC filtering works).
        let tail_start = (0.75 * out_y.len() as f64) as usize;
        let tail = &out_y[tail_start..];
        let (mn, mx) = tail
            .iter()
            .fold((f64::MAX, f64::MIN), |(a, b), &x| (a.min(x), b.max(x)));
        assert!(
            mx - mn < 0.4,
            "buck output ripple should be small, got {:.3} V",
            mx - mn
        );
        // The switching node actually chops: high toward Vin, low toward the diode clamp.
        let sw_max = sw_y.iter().cloned().fold(f64::MIN, f64::max);
        let sw_min = sw_y.iter().cloned().fold(f64::MAX, f64::min);
        assert!(
            sw_max > 10.0,
            "switch node should swing up to Vin, got {sw_max}"
        );
        assert!(sw_min < 1.0, "switch node should swing low, got {sw_min}");
    }

    #[test]
    fn boost_converter_steps_the_input_voltage_up() {
        // Boost: 5 V in, inductor charged by a PWM switch to ground, diode to the
        // output cap. In continuous conduction Vout = Vin/(1 − duty) > Vin.
        let mut c = Circuit::new("boost");
        let (vin, sw, out, ctrl) = (c.new_net(), c.new_net(), c.new_net(), c.new_net());
        c.add_part(
            "VIN",
            PartKind::VoltageSource,
            v("5", Unit::Volt),
            vec![vin, NetId::GROUND],
        );
        let (period, duty) = (5e-6, 0.42);
        c.add_part(
            "VPWM",
            PartKind::VoltageSource,
            v("0", Unit::Volt),
            vec![ctrl, NetId::GROUND],
        );
        assert!(c.set_waveform(
            "VPWM",
            Waveform::Pulse {
                v1: 0.0,
                v2: 5.0,
                delay: 0.0,
                width: duty * period,
                period,
            },
        ));
        c.add_part(
            "L1",
            PartKind::Inductor,
            v("50u", Unit::Henry),
            vec![vin, sw],
        );
        // Low-side switch sw → gnd, rectifier diode sw → out.
        assert!(c.add_switch(ctrl, sw, NetId::GROUND, 0.01, 1e6, 2.5));
        c.add_part(
            "D1",
            PartKind::Diode(DiodeModel::default()),
            None,
            vec![sw, out], // anode sw, cathode out
        );
        c.add_part(
            "C1",
            PartKind::Capacitor,
            v("5u", Unit::Farad),
            vec![out, NetId::GROUND],
        );
        c.add_part(
            "RL",
            PartKind::Resistor,
            v("20", Unit::Ohm),
            vec![out, NetId::GROUND],
        );

        let sigs = Engine::new()
            .run(
                &c,
                &Analysis::Transient {
                    duration: 1e-3,
                    step: Some(period / 40.0),
                },
            )
            .expect("boost transient solves");

        let out_y = node_trace(&sigs, &c, out);
        let vout = tail_mean(&out_y, 0.2);
        // Clearly boosted above the 5 V input (ideal Vin/(1−D) ≈ 8.6 V).
        assert!(
            vout > 7.0,
            "boost output should step up above the 5 V input, got {vout}"
        );
        assert!(
            vout < 10.0,
            "boost output should stay below the ideal ceiling, got {vout}"
        );
    }

    // --- 2-port S-parameters — CP-SIM-M9.2 ----------------------------------

    #[test]
    fn matched_pi_attenuator_is_reflectionless_with_predicted_attenuation() {
        // A 6 dB (N = 2) resistive Π-pad matched to Z0 = 50 Ω: shunt 150 Ω at each
        // port, series 37.5 Ω. Matched ⇒ |S11| ≈ 0, and the attenuation sets
        // |S21| = 1/N = 0.5. Symmetric + reciprocal ⇒ S11 = S22, S12 = S21. The
        // Z-matrix exists (shunt paths) and matches the open-circuit hand analysis.
        let mut c = Circuit::new("pi-pad");
        let (p1, p2) = (c.new_net(), c.new_net());
        c.add_part(
            "R1",
            PartKind::Resistor,
            v("150", Unit::Ohm),
            vec![p1, NetId::GROUND],
        );
        c.add_part("R2", PartKind::Resistor, v("37.5", Unit::Ohm), vec![p1, p2]);
        c.add_part(
            "R3",
            PartKind::Resistor,
            v("150", Unit::Ohm),
            vec![p2, NetId::GROUND],
        );

        let sp = Engine::new()
            .s_parameters(&c, p1, p2, 50.0, 1e6)
            .expect("Π-pad S-parameters");

        assert!(
            sp.s11.magnitude() < 1e-9,
            "matched ⇒ |S11|≈0, got {}",
            sp.s11.magnitude()
        );
        assert!(
            sp.s22.magnitude() < 1e-9,
            "matched ⇒ |S22|≈0, got {}",
            sp.s22.magnitude()
        );
        assert!(
            (sp.s21.magnitude() - 0.5).abs() < 1e-9,
            "|S21| should be 1/N = 0.5, got {}",
            sp.s21.magnitude()
        );
        // Reciprocal + symmetric network.
        assert!((sp.s21 - sp.s12).magnitude() < 1e-12);
        // Z-matrix against the open-circuit hand analysis: Z11 = Z22 ≈ 83.33 Ω,
        // Z12 = Z21 ≈ 66.67 Ω.
        assert!((sp.z[0][0].re - 250.0 / 3.0).abs() < 1e-6 && sp.z[0][0].im.abs() < 1e-9);
        assert!((sp.z[1][1].re - 250.0 / 3.0).abs() < 1e-6);
        assert!((sp.z[0][1].re - 200.0 / 3.0).abs() < 1e-6);
        assert!((sp.z[1][0].re - 200.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn series_impedance_two_port_matches_closed_form() {
        // A bare series impedance Z between the two ports (both returned to ground)
        // has the closed-form S-parameters S11 = S22 = Z/(Z + 2·Z0) and
        // S21 = S12 = 2·Z0/(Z + 2·Z0). Checked for a resistor (real) and an
        // inductor (jωL, exercising the complex solve).
        let z0 = 50.0;

        // Series 50 Ω resistor: S11 = 50/150 = 1/3, S21 = 100/150 = 2/3.
        let mut cr = Circuit::new("series-r");
        let (p1, p2) = (cr.new_net(), cr.new_net());
        cr.add_part("R1", PartKind::Resistor, v("50", Unit::Ohm), vec![p1, p2]);
        let spr = Engine::new()
            .s_parameters(&cr, p1, p2, z0, 1e6)
            .expect("series-R S");
        assert!((spr.s11.re - 1.0 / 3.0).abs() < 1e-9 && spr.s11.im.abs() < 1e-9);
        assert!((spr.s21.re - 2.0 / 3.0).abs() < 1e-9 && spr.s21.im.abs() < 1e-9);
        assert!((spr.s22 - spr.s11).magnitude() < 1e-12);
        assert!((spr.s12 - spr.s21).magnitude() < 1e-12);

        // Series inductor: Z = jωL at f = 10 MHz, L = 1 µH.
        let (f, l) = (1e7, 1e-6);
        let mut cl = Circuit::new("series-l");
        let (q1, q2) = (cl.new_net(), cl.new_net());
        cl.add_part("L1", PartKind::Inductor, v("1u", Unit::Henry), vec![q1, q2]);
        let spl = Engine::new()
            .s_parameters(&cl, q1, q2, z0, f)
            .expect("series-L S");
        let z = Complex::new(0.0, TAU * f * l);
        let two_z0 = Complex::new(2.0 * z0, 0.0);
        let want_s11 = z / (z + two_z0);
        let want_s21 = two_z0 / (z + two_z0);
        assert!(
            (spl.s11 - want_s11).magnitude() < 1e-9,
            "S11 {:?} vs {:?}",
            spl.s11,
            want_s11
        );
        assert!(
            (spl.s21 - want_s21).magnitude() < 1e-9,
            "S21 {:?} vs {:?}",
            spl.s21,
            want_s21
        );
    }

    #[test]
    fn s_parameters_reject_bad_arguments() {
        let mut c = Circuit::new("bad");
        let (p1, p2) = (c.new_net(), c.new_net());
        c.add_part("R1", PartKind::Resistor, v("50", Unit::Ohm), vec![p1, p2]);
        c.add_part(
            "R2",
            PartKind::Resistor,
            v("50", Unit::Ohm),
            vec![p2, NetId::GROUND],
        );
        let eng = Engine::new();
        assert!(matches!(
            eng.s_parameters(&c, p1, p2, 0.0, 1e6),
            Err(SimError::BadValue(_))
        ));
        assert!(matches!(
            eng.s_parameters(&c, p1, p2, 50.0, 0.0),
            Err(SimError::BadValue(_))
        ));
        // A ground port has no node to drive.
        assert!(matches!(
            eng.s_parameters(&c, NetId::GROUND, p2, 50.0, 1e6),
            Err(SimError::BadValue(_))
        ));
    }
}
