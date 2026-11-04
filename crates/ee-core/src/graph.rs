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

/// The kind of a part. This is deliberately small at M0; `ee-devices` will grow
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
}
