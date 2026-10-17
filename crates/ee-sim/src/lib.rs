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
    /// A part is wired with the wrong number of terminals for its kind.
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
            | PartKind::Diode
            | PartKind::Bjt(..)
            | PartKind::Mosfet(..) => {}
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

/// The three nets a transistor connects — `[collector/drain, base/gate,
/// emitter/source]` — or an error if it is not wired with exactly three.
fn terminals3(part: &Part) -> Result<[NetId; 3], SimError> {
    match part.terminals.as_slice() {
        [a, b, c] => Ok([*a, *b, *c]),
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
}
