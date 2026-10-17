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

use ee_core::graph::{BjtModel, Circuit, MosfetModel, NetId, Part, PartKind, Polarity, Waveform};
use ee_core::value::Value;
use ee_core::{Complex, Domain, Signal, SignalData, Unit};
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
            Analysis::DcSweep {
                source,
                start,
                stop,
                step,
            } => dc_sweep(circuit, source, *start, *stop, *step),
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
            | PartKind::Diode
            | PartKind::Bjt(..)
            | PartKind::Mosfet(..)
            | PartKind::Vcvs(..)
            | PartKind::Vccs(..)
            | PartKind::Ccvs(..)
            | PartKind::Cccs(..) => {}
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
/// that [`stamp_companion`] writes into the MNA system. A diode is the
/// two-terminal case; a BJT and a MOSFET stamp a 3×3 block. (`CP-SIM-M1.2`.)
trait NonlinearDevice {
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
struct Companion {
    /// Terminal currents `I_i` (the current flowing *from node `i` into the
    /// device*) at the linearization point — the solver's current-convergence
    /// probe.
    i: Vec<f64>,
    /// RHS equivalent-current sources: `i_eq[a]` is injected into node `a`'s row,
    /// equal to `Σ_j g[a][j]·V*_j − I_a` at the (possibly limited) linearization
    /// point. The model precomputes it so junction limiting stays internal and
    /// the stamped companion is self-consistent — the multiport generalization of
    /// the two-terminal Norton source `I_eq = I(v) − g·v`.
    i_eq: Vec<f64>,
    /// Jacobian `g[i][j] = ∂I_i/∂V_j`, a dense `n × n` block in terminal order
    /// (possibly asymmetric — a transconductance is not reciprocal).
    g: Vec<Vec<f64>>,
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

/// A junction diode. Terminals `[anode, cathode]`; the branch voltage is
/// `v(anode) − v(cathode)` and forward current flows anode → cathode.
struct Diode {
    nets: [NetId; 2],
    model: DiodeModel,
}

impl NonlinearDevice for Diode {
    fn terminals(&self) -> &[NetId] {
        &self.nets
    }

    fn state_len(&self) -> usize {
        1 // the single junction voltage
    }

    fn linearize(&self, v: &[f64], state: &mut [f64], limit: bool) -> Companion {
        // Shockley: I = Is·(exp(V/(n·Vt)) − 1), g = dI/dV = (Is/(n·Vt))·exp(V/(n·Vt)).
        let vte = self.model.n * self.model.vt;
        let mut vd = v[0] - v[1];
        if limit {
            vd = pnjlim(vd, state[0], vte, self.model.is);
            state[0] = vd;
        }
        let ex = (vd / vte).exp();
        let i = self.model.is * (ex - 1.0);
        let g = (self.model.is / vte) * ex;
        // Norton equivalent current into the anode, `g·v − I`; the cathode mirrors
        // it. This reproduces the two-terminal stamp bit-for-bit.
        let i_eq = g * vd - i;
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
        match &part.kind {
            PartKind::Diode => {
                let (a, b) = terminals2(part)?;
                devices.push(Box::new(Diode {
                    nets: [a, b],
                    model: diode_model(part),
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
/// `0.0` for the unaided solve.
fn solve_nr(
    circuit: &Circuit,
    layout: &Layout,
    size: usize,
    devices: &[Box<dyn NonlinearDevice>],
    stamp_linear: &dyn Fn(&mut System) -> Result<(), SimError>,
    x_init: Option<&[f64]>,
    gmin: f64,
) -> Result<Vec<f64>, SimError> {
    // Purely linear: one stamp-and-solve is exact (and identical to M0). A linear
    // circuit always solves in one shot, so the convergence aids never reach here.
    if devices.is_empty() {
        let mut sys = System::new(size);
        stamp_linear(&mut sys)?;
        return sys.solve().ok_or(SimError::Singular);
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

        let x_new = sys.solve().ok_or(SimError::Singular)?;
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

/// Solve the DC operating point: resistors + sources, capacitors open, inductors
/// shorted, and any nonlinear device driven to steady state by Newton–Raphson.
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
fn solve_dc(circuit: &Circuit, layout: &Layout) -> Result<Vec<f64>, SimError> {
    let size = layout.n_nodes()
        + layout.vsources.len()
        + layout.n_controlled_branches()
        + layout.inductors.len();
    let devices = collect_nonlinear(circuit)?;
    // The linear stamp at source scale `alpha` (1.0 = the real circuit).
    let stamp_at = |sys: &mut System, alpha: f64| -> Result<(), SimError> {
        stamp_resistors(sys, circuit, layout)?;
        // Operating point: sources at their bias (`None`), scaled for source stepping.
        stamp_vsources(sys, circuit, layout, None, alpha)?;
        stamp_current_sources(sys, circuit, layout, None, alpha)?;
        stamp_controlled_sources(sys, circuit, layout)?;
        stamp_inductor_shorts(sys, circuit, layout)?;
        Ok(())
    };

    // 1. Plain Newton–Raphson, full sources, no gmin, cold start.
    match solve_nr(
        circuit,
        layout,
        size,
        &devices,
        &|sys| stamp_at(sys, 1.0),
        None,
        0.0,
    ) {
        Ok(x) => return Ok(x),
        // Hit the iteration cap or a singular guess — the aids may still get there.
        Err(SimError::NonConvergence(_)) | Err(SimError::Singular) => {}
        Err(e) => return Err(e), // Bad*/Missing* are data faults no aid can cure
    }

    // 2. gmin stepping, then 3. source stepping. Each returns `Ok(None)` when a
    // continuation step fails to converge; a real fault (still singular at the
    // final gmin = 0 — a truly floating node) propagates straight out.
    if let Some(x) = gmin_stepping(circuit, layout, size, &devices, &stamp_at)? {
        return Ok(x);
    }
    if let Some(x) = source_stepping(circuit, layout, size, &devices, &stamp_at)? {
        return Ok(x);
    }
    Err(SimError::NonConvergence(NR_MAX_ITERS))
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
        match solve_nr(
            circuit,
            layout,
            size,
            devices,
            &|sys| stamp_at(sys, 1.0),
            Some(&x),
            gmin,
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
        match solve_nr(
            circuit,
            layout,
            size,
            devices,
            &|sys| stamp_at(sys, alpha),
            Some(&x),
            0.0,
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
/// One Backward-Euler step's outcome: the node solution, the refreshed capacitor
/// voltages, and the refreshed inductor currents — the state the next step reads.
type StepResult = (Vec<f64>, Vec<f64>, Vec<f64>);

struct TransientModel<'a> {
    circuit: &'a Circuit,
    layout: &'a Layout,
    /// Capacitors as `(plus, minus, farads)`.
    caps: Vec<(NetId, NetId, f64)>,
    /// Inductors as `(a, b, henries)`.
    inds: Vec<(NetId, NetId, f64)>,
    /// Nonlinear devices, re-linearized inside each step's Newton loop.
    devices: Vec<Box<dyn NonlinearDevice>>,
    /// Node count and voltage-source branch count. The step system is
    /// `n + m + cb`, with `cb` the controlled-source (VCVS/CCVS) branch rows.
    n: usize,
    m: usize,
    cb: usize,
}

impl<'a> TransientModel<'a> {
    fn build(circuit: &'a Circuit, layout: &'a Layout) -> Result<Self, SimError> {
        let caps: Vec<(NetId, NetId, f64)> = circuit
            .parts()
            .iter()
            .filter(|p| matches!(p.kind, PartKind::Capacitor))
            .map(|p| {
                let (a, b) = terminals2(p)?;
                Ok::<_, SimError>((a, b, magnitude(p)?))
            })
            .collect::<Result<_, _>>()?;

        // A non-positive inductance would blow up `h/L`.
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

        let devices = collect_nonlinear(circuit)?;
        Ok(Self {
            circuit,
            layout,
            caps,
            inds,
            devices,
            n: layout.n_nodes(),
            m: layout.vsources.len(),
            cb: layout.n_controlled_branches(),
        })
    }

    /// Solve the `t = 0` start: capacitors held at 0 V (their initial condition,
    /// each a 0 V source), inductors open. Returns the node solution and the
    /// initial capacitor voltages (the inductor currents start at 0 A).
    fn initial(&self) -> Result<(Vec<f64>, Vec<f64>), SimError> {
        let (circuit, layout, n, m, cb) = (self.circuit, self.layout, self.n, self.m, self.cb);
        let stamp = |sys: &mut System| -> Result<(), SimError> {
            stamp_resistors(sys, circuit, layout)?;
            // t = 0 uses the waveform bias (sine → offset, pulse → v1).
            stamp_vsources(sys, circuit, layout, None, 1.0)?;
            stamp_current_sources(sys, circuit, layout, None, 1.0)?;
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
            Ok(())
        };
        let x0 = solve_nr(
            circuit,
            layout,
            n + m + cb + self.caps.len(),
            &self.devices,
            &stamp,
            None,
            0.0,
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
        Ok((x0, cap_v0))
    }

    /// One Backward-Euler step of size `h` landing at time `t_end`, from the
    /// capacitor voltages `cap_prev` and inductor currents `ind_prev`. Returns the
    /// node solution at `t_end` and the refreshed `(cap_prev, ind_prev)`.
    fn step(
        &self,
        t_end: f64,
        h: f64,
        cap_prev: &[f64],
        ind_prev: &[f64],
    ) -> Result<StepResult, SimError> {
        let (circuit, layout) = (self.circuit, self.layout);
        let stamp = |sys: &mut System| -> Result<(), SimError> {
            stamp_resistors(sys, circuit, layout)?;
            // Sources evaluated at the step's end time.
            stamp_vsources(sys, circuit, layout, Some(t_end), 1.0)?;
            stamp_current_sources(sys, circuit, layout, Some(t_end), 1.0)?;
            stamp_controlled_sources(sys, circuit, layout)?;
            for (ci, (a, b, c)) in self.caps.iter().enumerate() {
                let g_eq = c / h;
                let (ia, ib) = (layout.node_of(circuit, *a), layout.node_of(circuit, *b));
                sys.stamp_conductance(ia, ib, g_eq);
                sys.stamp_current(ia, ib, g_eq * cap_prev[ci]);
            }
            for (li, (a, b, l)) in self.inds.iter().enumerate() {
                let g_eq = h / l;
                let (ia, ib) = (layout.node_of(circuit, *a), layout.node_of(circuit, *b));
                sys.stamp_conductance(ia, ib, g_eq);
                // History source `i_prev` oriented a → b: current flows a into b.
                sys.stamp_current(ib, ia, ind_prev[li]);
            }
            Ok(())
        };
        let x = solve_nr(
            circuit,
            layout,
            self.n + self.m + self.cb,
            &self.devices,
            &stamp,
            None,
            0.0,
        )?;
        let new_caps = self
            .caps
            .iter()
            .map(|(a, b, _)| across(&x, layout.node_of(circuit, *a), layout.node_of(circuit, *b)))
            .collect();
        let new_inds = self
            .inds
            .iter()
            .enumerate()
            .map(|(li, (a, b, l))| {
                ind_prev[li]
                    + (h / l) * across(&x, layout.node_of(circuit, *a), layout.node_of(circuit, *b))
            })
            .collect();
        Ok((x, new_caps, new_inds))
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

/// Transient dispatcher. `step = Some(h)` runs the fixed-step Backward-Euler
/// exactly as M0 did (bit-for-bit — the existing tests do not move); `step = None`
/// asks the engine to choose the timestep and runs the error-controlled adaptive
/// driver ([`adaptive_transient`], `CP-SIM-M1.6`).
fn transient(
    circuit: &Circuit,
    layout: &Layout,
    duration: f64,
    step: Option<f64>,
) -> Result<Vec<Signal>, SimError> {
    if duration <= 0.0 {
        return Err(SimError::BadValue("transient duration/step".into()));
    }
    let model = TransientModel::build(circuit, layout)?;
    let (x0, cap_v0) = model.initial()?;
    match step {
        Some(h) if h > 0.0 => fixed_transient(&model, duration, h, x0, cap_v0),
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
    for k in 1..=steps {
        let t = k as f64 * h;
        let (x, caps, inds) = model.step(t, h, &cap_prev, &ind_prev)?;
        times.push(t);
        for (d, trace) in traces.iter_mut().enumerate() {
            trace.push(x[d]);
        }
        cap_prev = caps;
        ind_prev = inds;
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
    let mut t = 0.0;
    let mut h = (duration / 100.0).clamp(h_min, h_max);

    let mut iters = 0usize;
    while t < duration * (1.0 - 1e-12) {
        iters += 1;
        if iters > ADAPTIVE_ITER_CAP {
            return Err(SimError::NonConvergence(ADAPTIVE_ITER_CAP));
        }
        // Clamp the attempt to land exactly on `duration`.
        let h_try = h.min(duration - t);
        let at_floor = h_try <= h_min * (1.0 + 1e-9);

        // Coarse: one step of h_try. Fine: two steps of h_try/2.
        let (x_coarse, _, _) = model.step(t + h_try, h_try, &cap_prev, &ind_prev)?;
        let half = 0.5 * h_try;
        let (_, caps_mid, inds_mid) = model.step(t + half, half, &cap_prev, &ind_prev)?;
        let (x_fine, caps_fine, inds_fine) = model.step(t + h_try, half, &caps_mid, &inds_mid)?;

        let err = lte_norm(&x_fine, &x_coarse);
        if err <= 1.0 || at_floor {
            // Accept the finer solution and advance.
            t += h_try;
            cap_prev = caps_fine;
            ind_prev = inds_fine;
            times.push(t);
            for (d, trace) in traces.iter_mut().enumerate() {
                trace.push(x_fine[d]);
            }
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
    // Inductors are admittances in AC — no branch rows. Controlled sources keep
    // their branch rows (VCVS/CCVS), right after the voltage-source branches.
    let size = n + m + layout.n_controlled_branches();

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

    // Nonlinear devices → their small-signal Jacobian at the DC bias (a real
    // admittance block). A diode reduces to one conductance between its nodes; a
    // BJT / MOSFET contributes its full multiport transadmittance matrix. Flatten
    // every entry to `(row, col, g)` so each stamps as a raw complex matrix entry.
    let devices = collect_nonlinear(circuit)?;
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
        // Linearize exactly at the bias (no limiting) and keep only the Jacobian.
        let comp = dev.linearize(&v, &mut vec![0.0; dev.state_len()], false);
        for (a, &na) in nodes.iter().enumerate() {
            for (b, &nb) in nodes.iter().enumerate() {
                device_g.push((na, nb, comp.g[a][b]));
            }
        }
    }

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
        for &(a, b, g) in &device_g {
            sys.stamp_matrix(a, b, Complex::new(g, 0.0));
        }
        for &(plus, minus, k, drive) in &vsource_stamps {
            sys.stamp_vsource(plus, minus, k, drive);
        }
        stamp_controlled_sources_complex(&mut sys, circuit, layout)?;
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
) -> Result<Vec<Signal>, SimError> {
    if !f_start.is_finite()
        || !f_stop.is_finite()
        || f_start <= 0.0
        || f_stop <= f_start
        || points_per_decade == 0
    {
        return Err(SimError::BadValue("noise sweep range/points".into()));
    }

    // DC bias, to linearize any nonlinear device (and verify the topology).
    let x_dc = solve_dc(circuit, layout)?;

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
                let r = magnitude(part)?;
                if r <= 0.0 {
                    return Err(SimError::BadValue(part.reference.clone()));
                }
                let (a, b) = terminals2(part)?;
                let (na, nb) = (layout.node_of(circuit, a), layout.node_of(circuit, b));
                conductances.push((na, nb, 1.0 / r));
                noise_sources.push((na, nb, 4.0 * BOLTZMANN_K * NOISE_TEMP_K / r));
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

    // Nonlinear devices → small-signal Jacobian at the DC bias (purely real), as AC.
    let devices = collect_nonlinear(circuit)?;
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
    for &f in &freqs {
        let w = TAU * f;
        let mut total = 0.0;
        // Each noise source: inject a 1 A probe across its nodes, solve, and read
        // the transimpedance off the output; accumulate |Z|²·S incoherently.
        for &(na, nb, s_k) in &noise_sources {
            let mut sys = build_network(w)?;
            sys.stamp_current(na, nb, Complex::ONE);
            let x = sys.solve().ok_or(SimError::Singular)?;
            let z = out_node.map_or(Complex::ZERO, |i| x[i]);
            total += z.magnitude().powi(2) * s_k;
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

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// The next 64 raw bits.
    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
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
    /// **Determinism.** One [`SplitMix64`] is seeded from `seed` and advanced once
    /// per toleranced part, in part-index order, across the whole run sequence. The
    /// set of toleranced parts, their order, and the draw count are all fixed by the
    /// circuit, so the same seed replays the identical stream of perturbations and
    /// therefore the identical samples — bit for bit. Each run perturbs from the
    /// original nominal values (it clones the untouched circuit), so draws never
    /// compound. A part with tolerance `t` and nominal value `v` becomes
    /// `v·(1 + t·u)`, `u` uniform in `[-1, 1)`.
    fn monte_carlo(
        &self,
        circuit: &Circuit,
        seed: u64,
        runs: usize,
        output: NetId,
        sample: usize,
        inner: &Analysis,
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
        let mut rng = SplitMix64::new(seed);
        let mut samples: Vec<f64> = Vec::with_capacity(runs);
        let mut unit = Unit::Volt;
        for _ in 0..runs {
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
            unit = sig.unit;
            let value = match &sig.y {
                SignalData::Real(v) => v.get(sample).copied(),
                SignalData::Complex(v) => v.get(sample).map(|z| z.magnitude()),
            }
            .ok_or_else(|| SimError::BadValue("monte-carlo sample index".into()))?;
            samples.push(value);
        }

        Ok(vec![Signal::time(
            format!("mc(n{})", circuit.resolve(output).0),
            unit,
            (0..runs).map(|i| i as f64).collect(),
            samples,
        )])
    }
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
) -> Result<Vec<Signal>, SimError> {
    let unit = sweep_source_unit(circuit, source)?;
    let values = sweep_values(start, stop, step)?;

    let mut work = circuit.clone();
    work.set_waveform(source, Waveform::Dc); // sweep the DC level, ignore any waveform
    let layout = Layout::build(&work);
    let n = layout.n_nodes();
    let mut traces: Vec<Vec<f64>> = vec![Vec::with_capacity(values.len()); n];
    for &val in &values {
        work.set_value(source, Value::new(val, unit));
        let x = solve_dc(&work, &layout)?;
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

        let mut work = circuit.clone();
        work.set_waveform(source, Waveform::Dc);
        let mut out = Vec::new();
        for &val in values {
            work.set_value(source, Value::new(val, unit));
            for mut sig in self.run(&work, inner)? {
                sig.name = format!("{} @ {source}={val}", sig.name);
                out.push(sig);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ee_core::value::Value;

    fn v(s: &str, u: Unit) -> Option<Value> {
        Some(Value::parse(s, u).unwrap())
    }
    // --- Convergence aids — CP-SIM-M1.3 ------------------------------------

    /// Build the plain-NR DC stamp and solve it with a cold start, no gmin, full
    /// sources — i.e. the fast path alone, bypassing the convergence aids. Used by
    /// the gate test to show a stiff circuit the aids are needed for.
    fn plain_nr_dc(c: &Circuit) -> Result<Vec<f64>, SimError> {
        let layout = Layout::build(c);
        let size = layout.n_nodes() + layout.vsources.len() + layout.inductors.len();
        let devices = collect_nonlinear(c).unwrap();
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
        c.add_part("D1", PartKind::Diode, None, vec![mid, NetId::GROUND]);

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
        c.add_part("D1", PartKind::Diode, None, vec![mid, NetId::GROUND]);
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
        c.add_part("D1", PartKind::Diode, None, vec![node, NetId::GROUND]);

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
}
