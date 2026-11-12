//! The circuit graph — the single source of truth the shell, TUI, and AI all
//! mutate through the same API.
//!
//! A [`Circuit`] is a set of **parts** connected at **nets** (electrically
//! common nodes). Ground is always net 0.

use std::collections::BTreeMap;

use crate::value::Value;

/// A net (electrical node). Net 0 is ground by convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NetId(pub u32);

impl NetId {
    pub const GROUND: NetId = NetId(0);
}

/// The nominal reference temperature (°C) component temperature coefficients are
/// specified about, and the default [`Circuit`] temperature. Every tempco term
/// vanishes at this temperature, so a circuit left at the default reproduces the
/// temperature-independent solve exactly. (`CP-SIM-M3.1`.)
pub const NOMINAL_TEMP_C: f64 = 27.0;

/// A placed part.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PartId(pub u32);

/// The kind of a part. This is deliberately small at M0; `ohmforge-devices` will grow
/// the real model library (see docs/ROADMAP.md).
#[derive(Debug, Clone)]
pub enum PartKind {
    Resistor,
    Capacitor,
    Inductor,
    VoltageSource,
    /// Independent current source (value in amperes).
    CurrentSource,
    /// Semiconductor junction diode (Shockley model). Terminals are
    /// `[anode, cathode]`; forward current flows anode → cathode. Carries its own
    /// [`DiodeModel`] parameters — saturation current `I_s`, emission coefficient
    /// `n`, and series resistance `R_s` — so a SPICE `.model` `N`/`RS` reaches the
    /// device. Defaults are a small-signal silicon diode (`I_s = 1e-14 A`, `n = 1`,
    /// `R_s = 0 Ω`). (`CP-SIM-M1.1`, model params `CP-SIM-M5.2`.)
    Diode(DiodeModel),
    /// Bipolar junction transistor, Ebers–Moll injection model. Terminals are
    /// `[collector, base, emitter]`; the [`Polarity`] selects NPN (`N`) or
    /// PNP (`P`). Carries its own [`BjtModel`] parameters. (`CP-SIM-M1.2`.)
    Bjt(Polarity, BjtModel),
    /// MOSFET, level-1 square-law model with the body tied to the source.
    /// Terminals are `[drain, gate, source]`; the [`Polarity`] selects NMOS
    /// (`N`) or PMOS (`P`). Carries its own [`MosfetModel`] parameters.
    /// (`CP-SIM-M1.2`.)
    Mosfet(Polarity, MosfetModel),
    /// Voltage-controlled voltage source (VCVS), gain `E` (V/V). Terminals are
    /// `[out+, out−, ctrl+, ctrl−]`; it drives `V(out+) − V(out−) = E·(V(ctrl+)
    /// − V(ctrl−))`. The building block for an ideal op-amp. (`CP-SIM-M2.3`.)
    Vcvs(f64),
    /// Voltage-controlled current source (VCCS), transconductance `G` (A/V).
    /// Terminals are `[out+, out−, ctrl+, ctrl−]`; it pushes a current `G·(V(ctrl+)
    /// − V(ctrl−))` through the output, flowing `out+ → out−` inside the source.
    /// (`CP-SIM-M2.3`.)
    Vccs(f64),
    /// Current-controlled voltage source (CCVS), transresistance `R` (V/A).
    /// Terminals are `[out+, out−]`; the controlling current is sensed through the
    /// named voltage source's branch (a 0 V sense source is the usual idiom),
    /// driving `V(out+) − V(out−) = R·I(ctrl)`. (`CP-SIM-M2.3`.)
    Ccvs(f64, String),
    /// Current-controlled current source (CCCS), current gain `F` (A/A). Terminals
    /// are `[out+, out−]`; the controlling current is sensed through the named
    /// voltage source's branch, and `F·I(ctrl)` flows `out+ → out−` inside the
    /// source. (`CP-SIM-M2.3`.)
    Cccs(f64, String),
    /// Anything not yet modeled natively, identified by name.
    Other(String),
}

/// Semiconductor channel/junction polarity. For a [`PartKind::Bjt`] this is NPN
/// (`N`) vs. PNP (`P`); for a [`PartKind::Mosfet`] it is n-channel (`N`) vs.
/// p-channel (`P`). It flips every current and controlling-voltage sign in the
/// device model, so one set of equations serves both halves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polarity {
    /// NPN bipolar / n-channel MOSFET.
    N,
    /// PNP bipolar / p-channel MOSFET.
    P,
}

impl Polarity {
    /// The sign the model multiplies its controlling voltages and terminal
    /// currents by: `+1` for `N`, `−1` for `P`.
    pub const fn sign(self) -> f64 {
        match self {
            Polarity::N => 1.0,
            Polarity::P => -1.0,
        }
    }
}

/// Ebers–Moll parameters for a [`PartKind::Bjt`]. Defaults are a generic
/// small-signal silicon transistor at 300 K.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BjtModel {
    /// Transport saturation current `I_s` (A).
    pub is: f64,
    /// Forward current gain `β_F` (dimensionless).
    pub bf: f64,
    /// Reverse current gain `β_R` (dimensionless).
    pub br: f64,
    /// Thermal voltage `V_t = kT/q` (V).
    pub vt: f64,
}

impl Default for BjtModel {
    fn default() -> Self {
        Self {
            is: 1e-14,
            bf: 100.0,
            br: 1.0,
            vt: 0.025_852,
        }
    }
}

/// Level-1 (square-law) parameters for a [`PartKind::Mosfet`]. Defaults are a
/// generic enhancement device: a 1 V threshold, 1 mA/V² transconductance, and no
/// channel-length modulation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MosfetModel {
    /// Threshold voltage magnitude `V_th` (V); the [`Polarity`] sets its sign.
    pub vth: f64,
    /// Transconductance parameter `K = ½·µ·Cox·W/L` (A/V²).
    pub kp: f64,
    /// Channel-length-modulation coefficient `λ` (1/V).
    pub lambda: f64,
}

impl Default for MosfetModel {
    fn default() -> Self {
        Self {
            vth: 1.0,
            kp: 1e-3,
            lambda: 0.0,
        }
    }
}

/// Shockley parameters for a [`PartKind::Diode`] (`CP-SIM-M5.2`), mirroring
/// [`BjtModel`] / [`MosfetModel`]. Defaults are a generic small-signal silicon
/// diode: `I_s = 1e-14 A`, emission `n = 1`, and no series resistance — exactly the
/// values the diode device assumed before the model existed, so a default diode
/// solves bit-for-bit as before. (The thermal voltage `V_t` is a fixed solver
/// constant at the nominal temperature, not a per-part knob, so it is not a field.)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DiodeModel {
    /// Saturation current `I_s` (A).
    pub is: f64,
    /// Emission coefficient `n` (dimensionless); scales the thermal voltage to
    /// `n·V_t` in the junction law.
    pub n: f64,
    /// Ohmic series resistance `R_s` (Ω) in series with the junction; `0` is ideal.
    pub rs: f64,
}

impl Default for DiodeModel {
    fn default() -> Self {
        Self {
            is: 1e-14,
            n: 1.0,
            rs: 0.0,
        }
    }
}

/// How an independent source varies over time.
///
/// The operating point and the `t = 0` transient sample use the waveform's bias:
/// the constant for [`Dc`](Waveform::Dc), the `offset` of a [`Sine`](Waveform::Sine),
/// the `v1` of a [`Pulse`](Waveform::Pulse). During a transient the formula is
/// evaluated at the current time; AC analysis treats a `Sine` source as the
/// 1∠0 small-signal drive. Non-source parts ignore their waveform. (`CP-SIM-M1`.)
///
/// [`Expr`](Waveform::Expr) carries an owned expression string, so `Waveform` is
/// [`Clone`] but not `Copy`.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Waveform {
    /// Constant source — the part's own [`Value`] magnitude is the level. This is
    /// the default, so a source built with [`Circuit::add_part`] behaves exactly
    /// as it did before waveforms existed.
    #[default]
    Dc,
    /// `offset + amplitude·sin(2π·freq·t + phase)`. `freq` is in hertz, `phase_deg`
    /// in degrees. The DC/operating-point value is `offset`.
    Sine {
        amplitude: f64,
        freq: f64,
        offset: f64,
        phase_deg: f64,
    },
    /// A repeating pulse: `v1` until `delay`, then `v2` for `width`, back to `v1`
    /// for the rest of each `period` (set `period <= 0` for a single, non-repeating
    /// pulse). The DC/operating-point value is `v1`.
    Pulse {
        v1: f64,
        v2: f64,
        delay: f64,
        width: f64,
        period: f64,
    },
    /// An arbitrary function of time `t` (seconds), given as a small expression
    /// over `+ - * / ^`, parentheses, the functions `sin cos exp ln sqrt abs`, and
    /// the constants `pi` and `e` — e.g. `"sin(2*pi*1000*t)"`. The simulator
    /// evaluates it at each transient step; the operating point and the `t = 0`
    /// transient sample use its value at `t = 0`. A malformed expression surfaces
    /// as a clean solver error, never a panic. (`CP-SIM-M3.1`.)
    Expr(String),
}

/// A magnetic-coupling relation between two inductors — the circuit-level record
/// behind a transformer (`CP-SIM-M6.1`). It names the two inductors by reference
/// designator and carries their coupling coefficient `k` (`|k| ≤ 1`); the mutual
/// inductance the solver stamps is `M = k·√(L₁·L₂)`.
///
/// This is deliberately a *relation on existing parts*, recorded via
/// [`Circuit::couple`], rather than a new data-carrying [`PartKind`]: the two
/// windings stay ordinary [`Inductor`](PartKind::Inductor) parts, so no match arm
/// anywhere else in the workspace has to learn a new variant.
#[derive(Debug, Clone, PartialEq)]
pub struct Coupling {
    /// Reference designator of the first coupled inductor.
    pub a: String,
    /// Reference designator of the second coupled inductor.
    pub b: String,
    /// Coupling coefficient `k` (`|k| ≤ 1`; `1` is an ideal, lossless transformer,
    /// `0` is no coupling). Negative `k` reverses the relative winding sense.
    pub k: f64,
}

/// A transmission line between two ports — the circuit-level record behind an
/// ideal lossless line or a lumped-LC lossy line (`CP-SIM-M7.2`).
///
/// Like [`Coupling`], this is deliberately a *relation on existing nets* recorded
/// via [`Circuit::add_line`] / [`Circuit::add_lossy_line`], **not** a new
/// data-carrying [`PartKind`]: the line adds no part, so no exhaustive match arm
/// anywhere else in the workspace has to learn a new variant. Only the transient
/// and AC engines read it; it is otherwise inert metadata.
///
/// A line joins **port A** `(a1, a2)` to **port B** `(b1, b2)`, each a `(+, −)`
/// node pair, and carries the characteristic impedance `z0` (Ω) and the one-way
/// propagation `delay` (s). A lossless line is the ideal delay line (Branin's
/// travelling-wave companion in transient, `e^{−jωT}` coupling in AC). A lossy
/// line is a lumped-LC approximation of a short line: its total series inductance
/// is `L = z0·delay` and total shunt capacitance `C = delay/z0`, with the extra
/// series resistance `r` (Ω) and shunt conductance `g` (S) of [`LineKind::Lossy`].
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// Port A positive node.
    pub a1: NetId,
    /// Port A negative (return) node.
    pub a2: NetId,
    /// Port B positive node.
    pub b1: NetId,
    /// Port B negative (return) node.
    pub b2: NetId,
    /// Characteristic impedance `Z₀` (Ω).
    pub z0: f64,
    /// One-way propagation delay `T_d` (s).
    pub delay: f64,
    /// Lossless ideal, or a lumped-LC lossy approximation.
    pub kind: LineKind,
}

/// Whether a [`Line`] is the ideal lossless delay line or a lumped-LC lossy line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineKind {
    /// The ideal lossless line: delay + characteristic impedance only.
    Lossless,
    /// A lumped-LC approximation with series resistance `r` (Ω) and shunt
    /// conductance `g` (S) added to the `L = z0·delay` / `C = delay/z0` the ideal
    /// parameters imply. `r = 0`, `g = 0` is the lumped stand-in for a lossless line.
    Lossy {
        /// Total series resistance (Ω).
        r: f64,
        /// Total shunt conductance (S).
        g: f64,
    },
}

/// A part instance: a kind, a human reference (`R1`), a primary value, the nets
/// its terminals connect to, and (for sources) a time [`Waveform`].
#[derive(Debug, Clone)]
pub struct Part {
    pub id: PartId,
    pub reference: String,
    pub kind: PartKind,
    pub value: Option<Value>,
    pub terminals: Vec<NetId>,
    /// The source's time behavior; [`Waveform::Dc`] for everything else. Set via
    /// [`Circuit::set_waveform`]; [`Circuit::add_part`] always starts it at `Dc`.
    pub waveform: Waveform,
}

/// The whole circuit — the single source of truth every front end mutates.
///
/// Nets are merged with a **union-find**: [`wire`](Circuit::wire) unions two net
/// ids in near-constant time regardless of order, and [`resolve`](Circuit::resolve)
/// maps any net id to the representative the solver stamps against. Raw ids stay
/// stable for the UI; resolution happens only at stamp time.
#[derive(Debug, Clone)]
pub struct Circuit {
    pub name: String,
    parts: Vec<Part>,
    /// Union-find parent per net id, indexed by id; `net_parent[i] == i` is a root.
    /// `net_parent[0]` is ground and always its own root.
    net_parent: Vec<u32>,
    next_part: u32,
    /// Optional per-part fractional tolerances, keyed by reference designator —
    /// `0.05` means ±5 %. Absent ⇒ the part is exact. Purely additive metadata the
    /// deterministic solver ignores; only the Monte-Carlo runner reads it to
    /// perturb component values within tolerance. (`CP-SIM-M2.2`.)
    tolerances: BTreeMap<String, f64>,
    /// Optional per-part temperature coefficients (1/°C), keyed by reference
    /// designator. Only the resistor model reads it: its effective resistance is
    /// `R0·(1 + tc·(T − 27 °C))`. Absent ⇒ `tc = 0` (temperature-independent).
    /// (`CP-SIM-M3.1`.)
    tempcos: BTreeMap<String, f64>,
    /// The circuit's operating temperature in °C — the `T` component tempcos shift
    /// about the nominal 27 °C. Defaults to [`NOMINAL_TEMP_C`], so an untouched
    /// circuit matches the temperature-independent solver bit-for-bit. (`CP-SIM-M3.1`.)
    temp_c: f64,
    /// Magnetic-coupling relations between inductor pairs, recorded by
    /// [`couple`](Circuit::couple). Empty ⇒ every inductor is independent (the
    /// pre-M6 behavior, bit-for-bit). Only the transient and AC engines read it; it
    /// is otherwise inert metadata, like [`tolerances`](Circuit::tolerance).
    /// (`CP-SIM-M6.1`.)
    couplings: Vec<Coupling>,
    /// Transmission lines recorded by [`add_line`](Circuit::add_line) /
    /// [`add_lossy_line`](Circuit::add_lossy_line). Empty ⇒ no lines (every pre-M7
    /// circuit, bit-for-bit). Only the transient and AC engines read it.
    /// (`CP-SIM-M7.2`.)
    lines: Vec<Line>,
}

// `Default` is hand-written (not derived) so net 0 is pre-seeded as ground: the
// first `new_net()` therefore returns `NetId(1)`, never aliasing `NetId::GROUND`.
impl Default for Circuit {
    fn default() -> Self {
        Self {
            name: String::new(),
            parts: Vec::new(),
            net_parent: vec![0], // net 0 = ground, its own root
            next_part: 0,
            tolerances: BTreeMap::new(),
            tempcos: BTreeMap::new(),
            temp_c: NOMINAL_TEMP_C,
            couplings: Vec::new(),
            lines: Vec::new(),
        }
    }
}

impl Circuit {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    /// Allocate a fresh net id (its own union-find root until wired).
    pub fn new_net(&mut self) -> NetId {
        let id = self.net_parent.len() as u32; // ids are dense from 0
        self.net_parent.push(id);
        NetId(id)
    }

    /// Electrically merge two nets into one common node. Order-independent and
    /// idempotent; ground (net 0) always remains the representative of its group.
    pub fn wire(&mut self, a: NetId, b: NetId) {
        let (ra, rb) = (self.find(a.0), self.find(b.0));
        if ra == rb {
            return;
        }
        // Attach the larger id under the smaller so `NetId::GROUND` (0) stays a root.
        let (root, child) = if ra < rb { (ra, rb) } else { (rb, ra) };
        self.net_parent[child as usize] = root;
    }

    /// Resolve a net id to its group representative — what the solver stamps.
    pub fn resolve(&self, net: NetId) -> NetId {
        NetId(self.find(net.0))
    }

    /// Union-find root walk (no path compression; `&self` for stamp-time use).
    fn find(&self, mut n: u32) -> u32 {
        while self.net_parent[n as usize] != n {
            n = self.net_parent[n as usize];
        }
        n
    }

    /// Find a part by its reference designator (e.g. `"R1"`).
    pub fn part_by_ref(&self, reference: &str) -> Option<&Part> {
        self.parts.iter().find(|p| p.reference == reference)
    }

    /// Add a part; returns its assigned [`PartId`].
    pub fn add_part(
        &mut self,
        reference: impl Into<String>,
        kind: PartKind,
        value: Option<Value>,
        terminals: Vec<NetId>,
    ) -> PartId {
        let id = PartId(self.next_part);
        self.next_part += 1;
        self.parts.push(Part {
            id,
            reference: reference.into(),
            kind,
            value,
            terminals,
            waveform: Waveform::default(),
        });
        id
    }

    /// Attach a time [`Waveform`] to an existing source by reference designator.
    /// Returns `true` if a part with that reference was found and updated, `false`
    /// otherwise. Only independent sources act on it; other parts carry it inertly.
    pub fn set_waveform(&mut self, reference: &str, waveform: Waveform) -> bool {
        match self.parts.iter_mut().find(|p| p.reference == reference) {
            Some(p) => {
                p.waveform = waveform;
                true
            }
            None => false,
        }
    }

    /// Override an existing part's primary [`Value`] by reference designator.
    /// Returns `true` if a part with that reference was found and updated. This is
    /// how a DC/parametric sweep retargets the source it steps: it points the
    /// level at each sweep sample without rebuilding the circuit. (`CP-SIM-M1.5`.)
    pub fn set_value(&mut self, reference: &str, value: Value) -> bool {
        match self.parts.iter_mut().find(|p| p.reference == reference) {
            Some(p) => {
                p.value = Some(value);
                true
            }
            None => false,
        }
    }

    /// Set a part's fractional tolerance by reference designator — `0.05` for
    /// ±5 %. Returns `true` if a part with that reference exists. A non-finite or
    /// negative tolerance is rejected (reported as `false`); a tolerance of `0`
    /// is stored and pins the part to its nominal value. Only the Monte-Carlo
    /// runner reads this; every other analysis is unaffected. (`CP-SIM-M2.2`.)
    pub fn set_tolerance(&mut self, reference: &str, tolerance: f64) -> bool {
        if !tolerance.is_finite() || tolerance < 0.0 || self.part_by_ref(reference).is_none() {
            return false;
        }
        self.tolerances.insert(reference.to_string(), tolerance);
        true
    }

    /// The fractional tolerance set for a part, or `None` if it is exact.
    pub fn tolerance(&self, reference: &str) -> Option<f64> {
        self.tolerances.get(reference).copied()
    }

    /// Set the circuit's operating temperature in °C. Component temperature
    /// coefficients ([`set_tempco`](Circuit::set_tempco)) shift their value about
    /// the nominal 27 °C at this temperature. A non-finite temperature is rejected
    /// (reported as `false`) so the solver never sees a `NaN` resistance.
    /// (`CP-SIM-M3.1`.)
    pub fn set_temperature(&mut self, celsius: f64) -> bool {
        if !celsius.is_finite() {
            return false;
        }
        self.temp_c = celsius;
        true
    }

    /// The circuit's operating temperature in °C (default [`NOMINAL_TEMP_C`]).
    pub fn temperature(&self) -> f64 {
        self.temp_c
    }

    /// Set a part's temperature coefficient `tc` (1/°C) by reference designator, so
    /// a resistor's effective resistance becomes `R0·(1 + tc·(T − 27 °C))`. Returns
    /// `true` if a part with that reference exists; a non-finite `tc` is rejected
    /// (reported as `false`). `tc = 0` — the implicit default for any part without
    /// one — leaves the part temperature-independent. (`CP-SIM-M3.1`.)
    pub fn set_tempco(&mut self, reference: &str, tc: f64) -> bool {
        if !tc.is_finite() || self.part_by_ref(reference).is_none() {
            return false;
        }
        self.tempcos.insert(reference.to_string(), tc);
        true
    }

    /// The temperature coefficient (1/°C) set for a part, or `None` if it carries
    /// none (temperature-independent).
    pub fn tempco(&self, reference: &str) -> Option<f64> {
        self.tempcos.get(reference).copied()
    }

    /// Replace the [`DiodeModel`] of an existing [`PartKind::Diode`] by reference
    /// designator — the device-model counterpart of [`set_waveform`](Circuit::set_waveform).
    /// Returns `true` only if a part with that reference exists **and** is a diode;
    /// a non-diode (or unknown) reference is left untouched and reports `false`.
    /// This is how the `ohmforge-fmt` project format re-attaches a diode's `IS`/`N`/`RS`
    /// on load, so a custom model survives a save→load round-trip. (`CP-AF-M6.1`.)
    pub fn set_diode_model(&mut self, reference: &str, model: DiodeModel) -> bool {
        match self.parts.iter_mut().find(|p| p.reference == reference) {
            Some(Part {
                kind: PartKind::Diode(m),
                ..
            }) => {
                *m = model;
                true
            }
            _ => false,
        }
    }

    /// Replace the [`BjtModel`] of an existing [`PartKind::Bjt`] by reference
    /// designator, preserving its [`Polarity`]. Returns `true` only if a part with
    /// that reference exists and is a BJT; otherwise the circuit is untouched and
    /// it reports `false`. The project-format `.model` directive uses this to carry
    /// a BJT's `IS`/`BF`/`BR` across a save→load. (`CP-AF-M6.1`.)
    pub fn set_bjt_model(&mut self, reference: &str, model: BjtModel) -> bool {
        match self.parts.iter_mut().find(|p| p.reference == reference) {
            Some(Part {
                kind: PartKind::Bjt(_, m),
                ..
            }) => {
                *m = model;
                true
            }
            _ => false,
        }
    }

    /// Replace the [`MosfetModel`] of an existing [`PartKind::Mosfet`] by reference
    /// designator, preserving its [`Polarity`]. Returns `true` only if a part with
    /// that reference exists and is a MOSFET; otherwise the circuit is untouched
    /// and it reports `false`. The project-format `.model` directive uses this to
    /// carry a MOSFET's `VTO`/`KP`/`LAMBDA` across a save→load. (`CP-AF-M6.1`.)
    pub fn set_mosfet_model(&mut self, reference: &str, model: MosfetModel) -> bool {
        match self.parts.iter_mut().find(|p| p.reference == reference) {
            Some(Part {
                kind: PartKind::Mosfet(_, m),
                ..
            }) => {
                *m = model;
                true
            }
            _ => false,
        }
    }

    /// Magnetically couple two inductors with coupling coefficient `k`, recording
    /// the transformer relation the transient and AC engines stamp as a mutual
    /// inductance `M = k·√(L_a·L_b)` (`CP-SIM-M6.1`).
    ///
    /// Both references must name existing [`Inductor`](PartKind::Inductor) parts,
    /// they must be distinct, and `k` must be finite with `|k| ≤ 1` (`1` is an
    /// ideal transformer, `0` no coupling); otherwise the call is a no-op and
    /// returns `false`. Re-coupling the same unordered pair replaces the earlier
    /// coefficient rather than recording a duplicate, so the relation stays
    /// single-valued regardless of argument order. The coupling is pure metadata on
    /// the two existing inductors — it adds no part and no [`PartKind`] variant.
    pub fn couple(&mut self, a: &str, b: &str, k: f64) -> bool {
        if a == b || !k.is_finite() || k.abs() > 1.0 {
            return false;
        }
        let is_inductor = |r: &str| {
            self.part_by_ref(r)
                .is_some_and(|p| matches!(p.kind, PartKind::Inductor))
        };
        if !is_inductor(a) || !is_inductor(b) {
            return false;
        }
        // Keep the pair single-valued, independent of the order it was named in.
        let same_pair = |c: &Coupling| (c.a == a && c.b == b) || (c.a == b && c.b == a);
        if let Some(existing) = self.couplings.iter_mut().find(|c| same_pair(c)) {
            existing.k = k;
        } else {
            self.couplings.push(Coupling {
                a: a.to_string(),
                b: b.to_string(),
                k,
            });
        }
        true
    }

    /// The magnetic-coupling relations recorded by [`couple`](Circuit::couple), in
    /// insertion order. Empty for a circuit with no transformers.
    pub fn couplings(&self) -> &[Coupling] {
        &self.couplings
    }

    /// Record an **ideal lossless** transmission line joining port A to port B,
    /// each a `(+, −)` net pair, with characteristic impedance `z0` (Ω) and one-way
    /// propagation `delay` (s) (`CP-SIM-M7.2`).
    ///
    /// `z0` and `delay` must both be finite and strictly positive; otherwise the
    /// call is a no-op returning `false`. Like [`couple`](Circuit::couple), this is
    /// pure metadata on existing nets — it adds no part and no [`PartKind`] variant.
    pub fn add_line(&mut self, a: (NetId, NetId), b: (NetId, NetId), z0: f64, delay: f64) -> bool {
        if !(z0.is_finite() && z0 > 0.0 && delay.is_finite() && delay > 0.0) {
            return false;
        }
        self.lines.push(Line {
            a1: a.0,
            a2: a.1,
            b1: b.0,
            b2: b.1,
            z0,
            delay,
            kind: LineKind::Lossless,
        });
        true
    }

    /// Record a **lumped-LC lossy** transmission line joining port A to port B,
    /// each a `(+, −)` net pair (`CP-SIM-M7.2`). The ideal parameters `z0`/`delay`
    /// set the line's inductance `L = z0·delay` and capacitance `C = delay/z0`;
    /// `r` (Ω) is the total series resistance and `g` (S) the total shunt
    /// conductance of the loss.
    ///
    /// `z0` and `delay` must be finite and positive and `r`, `g` finite and
    /// non-negative; otherwise the call is a no-op returning `false`. Like
    /// [`add_line`](Circuit::add_line) it adds no part and no [`PartKind`].
    pub fn add_lossy_line(
        &mut self,
        a: (NetId, NetId),
        b: (NetId, NetId),
        z0: f64,
        delay: f64,
        r: f64,
        g: f64,
    ) -> bool {
        if !(z0.is_finite() && z0 > 0.0 && delay.is_finite() && delay > 0.0) {
            return false;
        }
        if !(r.is_finite() && r >= 0.0 && g.is_finite() && g >= 0.0) {
            return false;
        }
        self.lines.push(Line {
            a1: a.0,
            a2: a.1,
            b1: b.0,
            b2: b.1,
            z0,
            delay,
            kind: LineKind::Lossy { r, g },
        });
        true
    }

    /// The transmission lines recorded by [`add_line`](Circuit::add_line) /
    /// [`add_lossy_line`](Circuit::add_lossy_line), in insertion order. Empty for a
    /// circuit with no lines.
    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    pub fn parts(&self) -> &[Part] {
        &self.parts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::Unit;

    #[test]
    fn first_net_is_never_ground() {
        // Both constructors must reserve net 0 for ground.
        assert_ne!(Circuit::new("c").new_net(), NetId::GROUND);
        assert_ne!(Circuit::default().new_net(), NetId::GROUND);
    }

    #[test]
    fn wire_merges_nets_order_independently() {
        let mut c = Circuit::new("c");
        let (a, b, d) = (c.new_net(), c.new_net(), c.new_net());
        c.wire(b, a); // order shouldn't matter
        c.wire(d, b); // chain a—b—d into one node
        assert_eq!(c.resolve(a), c.resolve(b));
        assert_eq!(c.resolve(a), c.resolve(d));
        // Idempotent and self-consistent.
        c.wire(a, d);
        assert_eq!(c.resolve(d), c.resolve(a));
    }

    #[test]
    fn ground_stays_the_representative() {
        let mut c = Circuit::new("c");
        let n = c.new_net();
        c.wire(n, NetId::GROUND);
        assert_eq!(
            c.resolve(n),
            NetId::GROUND,
            "wiring to ground pulls the net to ground"
        );
        assert_eq!(c.resolve(NetId::GROUND), NetId::GROUND);
    }

    #[test]
    fn part_lookup_by_reference() {
        let mut c = Circuit::new("c");
        let (a, b) = (c.new_net(), c.new_net());
        c.add_part("R1", PartKind::Resistor, None, vec![a, b]);
        assert!(c.part_by_ref("R1").is_some());
        assert!(c.part_by_ref("R2").is_none());
    }

    #[test]
    fn parts_default_to_dc_and_set_waveform_targets_by_reference() {
        let mut c = Circuit::new("c");
        let (a, b) = (c.new_net(), c.new_net());
        c.add_part("V1", PartKind::VoltageSource, None, vec![a, b]);
        // A freshly added source is DC by default (no behavior change at M0).
        assert_eq!(c.part_by_ref("V1").unwrap().waveform, Waveform::Dc);

        let wf = Waveform::Sine {
            amplitude: 1.0,
            freq: 1_000.0,
            offset: 0.0,
            phase_deg: 0.0,
        };
        assert!(c.set_waveform("V1", wf.clone()));
        assert_eq!(c.part_by_ref("V1").unwrap().waveform, wf);
        // An unknown reference leaves the circuit untouched and reports false.
        assert!(!c.set_waveform("V9", Waveform::Dc));
    }

    #[test]
    fn set_value_retargets_a_sources_level_by_reference() {
        let mut c = Circuit::new("c");
        let (a, b) = (c.new_net(), c.new_net());
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            Some(Value::new(1.0, Unit::Volt)),
            vec![a, b],
        );
        assert!(c.set_value("V1", Value::new(7.0, Unit::Volt)));
        assert_eq!(c.part_by_ref("V1").unwrap().value.unwrap().magnitude, 7.0);
        // An unknown reference is a no-op reported as false.
        assert!(!c.set_value("V9", Value::new(1.0, Unit::Volt)));
    }

    #[test]
    fn tolerance_is_optional_additive_metadata() {
        let mut c = Circuit::new("c");
        let (a, b) = (c.new_net(), c.new_net());
        c.add_part(
            "R1",
            PartKind::Resistor,
            Some(Value::new(1000.0, Unit::Ohm)),
            vec![a, b],
        );
        // Parts start exact (no tolerance recorded).
        assert_eq!(c.tolerance("R1"), None);
        // A sane tolerance is stored and read back by reference.
        assert!(c.set_tolerance("R1", 0.05));
        assert_eq!(c.tolerance("R1"), Some(0.05));
        // Unknown references, and nonsensical tolerances, are rejected.
        assert!(!c.set_tolerance("R9", 0.05));
        assert!(!c.set_tolerance("R1", -0.1));
        assert!(!c.set_tolerance("R1", f64::NAN));
        // The rejected writes left the earlier value untouched.
        assert_eq!(c.tolerance("R1"), Some(0.05));
    }

    #[test]
    fn set_device_models_target_the_matching_kind_only() {
        let mut c = Circuit::new("models");
        let (a, b, e) = (c.new_net(), c.new_net(), c.new_net());
        c.add_part(
            "D1",
            PartKind::Diode(DiodeModel::default()),
            None,
            vec![a, b],
        );
        c.add_part(
            "Q1",
            PartKind::Bjt(Polarity::P, BjtModel::default()),
            None,
            vec![a, b, e],
        );
        c.add_part(
            "M1",
            PartKind::Mosfet(Polarity::N, MosfetModel::default()),
            None,
            vec![a, b, e],
        );
        c.add_part("R1", PartKind::Resistor, None, vec![a, b]);

        // Each setter updates the model of its matching kind and reports true.
        let dm = DiodeModel {
            is: 2.52e-9,
            n: 1.75,
            rs: 0.5,
        };
        assert!(c.set_diode_model("D1", dm));
        let bm = BjtModel {
            is: 2e-14,
            bf: 250.0,
            br: 3.0,
            vt: 0.026,
        };
        assert!(c.set_bjt_model("Q1", bm));
        let mm = MosfetModel {
            vth: 1.5,
            kp: 2e-3,
            lambda: 0.02,
        };
        assert!(c.set_mosfet_model("M1", mm));

        assert!(matches!(c.part_by_ref("D1").unwrap().kind, PartKind::Diode(m) if m == dm));
        // Polarity is preserved alongside the new model.
        assert!(matches!(
            c.part_by_ref("Q1").unwrap().kind,
            PartKind::Bjt(Polarity::P, m) if m == bm
        ));
        assert!(matches!(
            c.part_by_ref("M1").unwrap().kind,
            PartKind::Mosfet(Polarity::N, m) if m == mm
        ));

        // A wrong kind, or an unknown reference, is rejected and changes nothing.
        assert!(!c.set_diode_model("Q1", dm));
        assert!(!c.set_bjt_model("R1", bm));
        assert!(!c.set_mosfet_model("D1", mm));
        assert!(!c.set_diode_model("nope", dm));
        assert!(matches!(c.part_by_ref("D1").unwrap().kind, PartKind::Diode(m) if m == dm));
    }

    #[test]
    fn temperature_defaults_to_nominal_and_tempco_is_optional_metadata() {
        let mut c = Circuit::new("c");
        let (a, b) = (c.new_net(), c.new_net());
        c.add_part(
            "R1",
            PartKind::Resistor,
            Some(Value::new(1000.0, Unit::Ohm)),
            vec![a, b],
        );
        // An untouched circuit sits at the nominal 27 °C and carries no tempcos.
        assert_eq!(c.temperature(), NOMINAL_TEMP_C);
        assert_eq!(c.tempco("R1"), None);
        // A sane tempco is stored and read back by reference.
        assert!(c.set_tempco("R1", 0.01));
        assert_eq!(c.tempco("R1"), Some(0.01));
        // Unknown references, and non-finite tempcos/temperatures, are rejected.
        assert!(!c.set_tempco("R9", 0.01));
        assert!(!c.set_tempco("R1", f64::NAN));
        assert!(!c.set_temperature(f64::INFINITY));
        assert_eq!(c.tempco("R1"), Some(0.01));
        assert_eq!(c.temperature(), NOMINAL_TEMP_C);
        // A finite temperature is accepted and read back.
        assert!(c.set_temperature(85.0));
        assert_eq!(c.temperature(), 85.0);
    }

    #[test]
    fn couple_records_inductor_pairs_and_validates_its_arguments() {
        let mut c = Circuit::new("xfmr");
        let (a, b, d) = (c.new_net(), c.new_net(), c.new_net());
        c.add_part(
            "L1",
            PartKind::Inductor,
            Some(Value::new(1e-3, Unit::Henry)),
            vec![a, b],
        );
        c.add_part(
            "L2",
            PartKind::Inductor,
            Some(Value::new(1e-3, Unit::Henry)),
            vec![b, d],
        );
        c.add_part(
            "R1",
            PartKind::Resistor,
            Some(Value::new(1000.0, Unit::Ohm)),
            vec![a, d],
        );

        // A fresh circuit has no couplings.
        assert!(c.couplings().is_empty());

        // A valid pair is recorded.
        assert!(c.couple("L1", "L2", 0.9));
        assert_eq!(c.couplings().len(), 1);
        assert_eq!(c.couplings()[0].k, 0.9);

        // Re-coupling the same pair (either order) replaces the coefficient, never
        // duplicates.
        assert!(c.couple("L2", "L1", 1.0));
        assert_eq!(c.couplings().len(), 1);
        assert_eq!(c.couplings()[0].k, 1.0);

        // Rejected: a non-inductor, a self-pair, a missing part, and |k| > 1.
        assert!(!c.couple("L1", "R1", 0.5));
        assert!(!c.couple("L1", "L1", 0.5));
        assert!(!c.couple("L1", "L9", 0.5));
        assert!(!c.couple("L1", "L2", 1.5));
        // All rejections left the single valid coupling untouched.
        assert_eq!(c.couplings().len(), 1);
        assert_eq!(c.couplings()[0].k, 1.0);
    }

    #[test]
    fn add_line_records_lossless_and_lossy_and_validates() {
        let mut c = Circuit::new("lines");
        let (a1, b1) = (c.new_net(), c.new_net());

        // A fresh circuit has no lines.
        assert!(c.lines().is_empty());

        let (pa, pb) = ((a1, NetId::GROUND), (b1, NetId::GROUND));

        // A valid lossless line is recorded with its parameters and kind.
        assert!(c.add_line(pa, pb, 50.0, 1e-9));
        assert_eq!(c.lines().len(), 1);
        assert_eq!(c.lines()[0].z0, 50.0);
        assert_eq!(c.lines()[0].delay, 1e-9);
        assert_eq!(c.lines()[0].kind, LineKind::Lossless);

        // A valid lossy line is recorded with its loss parameters.
        assert!(c.add_lossy_line(pa, pb, 75.0, 2e-9, 5.0, 1e-6));
        assert_eq!(c.lines().len(), 2);
        assert_eq!(c.lines()[1].kind, LineKind::Lossy { r: 5.0, g: 1e-6 });

        // Rejected: non-positive / non-finite z0 or delay, negative loss.
        assert!(!c.add_line(pa, pb, 0.0, 1e-9));
        assert!(!c.add_line(pa, pb, 50.0, -1e-9));
        assert!(!c.add_line(pa, pb, f64::NAN, 1e-9));
        assert!(!c.add_lossy_line(pa, pb, 50.0, 1e-9, -1.0, 0.0));
        assert!(!c.add_lossy_line(pa, pb, 50.0, 1e-9, 0.0, -1.0));
        // Every rejection left the two valid lines untouched.
        assert_eq!(c.lines().len(), 2);
    }
}
