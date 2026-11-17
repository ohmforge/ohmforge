//! Sensor and actuator models, built by **expanding into the existing
//! [`ohmforge_core`] primitives** (resistors, a capacitor, an inductor, and the
//! controlled sources) rather than adding new [`PartKind`] variants — the same
//! composition pattern as [`crate::subcircuits`].
//!
//! Two shapes live here:
//!
//! - **Resistive transducers** — [`Thermistor`] (temperature → resistance via the
//!   β/Steinhart model), [`Ldr`] (illuminance → resistance), and [`Potentiometer`]
//!   (a three-terminal divider with a `0..1` wiper). Each maps a physical quantity
//!   to a resistance and drives a plain [`PartKind::Resistor`]; the thermistor's
//!   [`apply`](Thermistor::apply) also sets the circuit temperature through
//!   [`Circuit::set_temperature`] so it rides the existing temperature hook rather
//!   than touching `ohmforge-core`.
//! - A **dynamic actuator** — [`DcMotor`], a lumped electro-mechanical model whose
//!   mechanical side is the exact electrical analogue of its equations of motion:
//!   the shaft speed `ω` is the voltage on an internal node, integrated over the
//!   transient by a capacitor (inertia `J`) against a resistor (viscous damping
//!   `B`). Back-EMF and torque cross between the electrical and mechanical halves
//!   through a VCVS and a CCCS, so the solver still only ever sees R/L/C and
//!   controlled sources.
//!
//! The builders are pure and deterministic, so they run identically on native and
//! `wasm32`. (The `ohmforge-sim` dependency the gate tests use is a dev-dependency
//! only; the library itself depends only on `ohmforge-core`.)

use ohmforge_core::graph::PartKind;
use ohmforge_core::{Circuit, NetId, Unit, Value};

/// A resistance value in ohms.
fn ohms(value: f64) -> Option<Value> {
    Some(Value::new(value, Unit::Ohm))
}

/// A capacitance value in farads.
fn farads(value: f64) -> Option<Value> {
    Some(Value::new(value, Unit::Farad))
}

/// An inductance value in henries.
fn henries(value: f64) -> Option<Value> {
    Some(Value::new(value, Unit::Henry))
}

/// 0 °C in kelvin — the Celsius/absolute offset the thermistor model converts
/// through.
const ZERO_CELSIUS_K: f64 = 273.15;

/// An NTC thermistor: a resistor whose value falls with rising temperature,
/// modelled with the single-β (Steinhart–Hart two-point) relation
///
/// ```text
/// R(T) = R0 · exp( β · (1/T − 1/T0) )
/// ```
///
/// with temperatures in kelvin. `β` is the material constant; `r0_ohms` is the
/// nominal resistance at the reference temperature `t0_celsius` (conventionally
/// 25 °C). This is the β-form of the full Steinhart–Hart polynomial, accurate
/// across the modest spans a hobby sensor sees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thermistor {
    /// Nominal resistance at `t0_celsius`, in ohms.
    pub r0_ohms: f64,
    /// Reference temperature for `r0_ohms`, in °C.
    pub t0_celsius: f64,
    /// Material constant β, in kelvin.
    pub beta: f64,
}

impl Default for Thermistor {
    /// A common 10 kΩ NTC: `R0 = 10 kΩ` at 25 °C, `β = 3950 K`.
    fn default() -> Self {
        Self {
            r0_ohms: 10_000.0,
            t0_celsius: 25.0,
            beta: 3950.0,
        }
    }
}

impl Thermistor {
    /// The modelled resistance (ohms) at a temperature in °C.
    pub fn resistance(&self, temp_celsius: f64) -> f64 {
        let t = temp_celsius + ZERO_CELSIUS_K;
        let t0 = self.t0_celsius + ZERO_CELSIUS_K;
        self.r0_ohms * (self.beta * (1.0 / t - 1.0 / t0)).exp()
    }

    /// Stamp the thermistor as a [`PartKind::Resistor`] between `a` and `b`, with
    /// its resistance evaluated at `temp_celsius`.
    pub fn stamp(
        &self,
        circuit: &mut Circuit,
        reference: &str,
        a: NetId,
        b: NetId,
        temp_celsius: f64,
    ) {
        circuit.add_part(
            reference.to_string(),
            PartKind::Resistor,
            ohms(self.resistance(temp_celsius)),
            vec![a, b],
        );
    }

    /// Set the modelled resistor to the temperature `temp_celsius`: update the
    /// resistor named `reference` to `R(temp_celsius)` **and** set the circuit's
    /// operating temperature through the existing [`Circuit::set_temperature`]
    /// hook, so the rest of the circuit sees the same ambient. Returns `true` if a
    /// part with that reference exists.
    pub fn apply(&self, circuit: &mut Circuit, reference: &str, temp_celsius: f64) -> bool {
        circuit.set_temperature(temp_celsius);
        circuit.set_value(
            reference,
            Value::new(self.resistance(temp_celsius), Unit::Ohm),
        )
    }
}

/// A photoresistor (light-dependent resistor): resistance falls as illuminance
/// rises, following the usual log-linear power law
///
/// ```text
/// R(E) = r_ref_ohms · (e_ref_lux / E) ^ gamma
/// ```
///
/// `r_ref_ohms` is the resistance at the reference illuminance `e_ref_lux`, and
/// `gamma` (≈ 0.5–0.9 for a typical CdS cell) is the slope of the log(R)/log(E)
/// line. Illuminance is clamped to a small positive floor so total darkness maps
/// to a large but finite (non-infinite) resistance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ldr {
    /// Resistance at `e_ref_lux`, in ohms.
    pub r_ref_ohms: f64,
    /// Reference illuminance for `r_ref_ohms`, in lux.
    pub e_ref_lux: f64,
    /// Log-log slope γ (dimensionless).
    pub gamma: f64,
}

impl Default for Ldr {
    /// A typical CdS cell: 10 kΩ at 10 lux, `γ = 0.7`.
    fn default() -> Self {
        Self {
            r_ref_ohms: 10_000.0,
            e_ref_lux: 10.0,
            gamma: 0.7,
        }
    }
}

impl Ldr {
    /// The modelled resistance (ohms) at an illuminance in lux.
    pub fn resistance(&self, lux: f64) -> f64 {
        // Floor the illuminance so darkness is a large finite resistance, not ∞.
        let e = lux.max(1e-6);
        self.r_ref_ohms * (self.e_ref_lux / e).powf(self.gamma)
    }

    /// Stamp the LDR as a [`PartKind::Resistor`] between `a` and `b`, with its
    /// resistance evaluated at `lux`.
    pub fn stamp(&self, circuit: &mut Circuit, reference: &str, a: NetId, b: NetId, lux: f64) {
        circuit.add_part(
            reference.to_string(),
            PartKind::Resistor,
            ohms(self.resistance(lux)),
            vec![a, b],
        );
    }

    /// Update the resistor named `reference` to its resistance at `lux`. Returns
    /// `true` if a part with that reference exists.
    pub fn apply(&self, circuit: &mut Circuit, reference: &str, lux: f64) -> bool {
        circuit.set_value(reference, Value::new(self.resistance(lux), Unit::Ohm))
    }
}

/// A three-terminal potentiometer: a total resistance split by a wiper at a
/// fractional position `0..=1` into two [`PartKind::Resistor`] legs.
///
/// With terminals `a`, `w` (wiper) and `b`, position `0` puts the wiper at `b`
/// and position `1` puts it at `a`. Wired as a divider (`a` to a supply, `b` to
/// ground), the wiper voltage is linear in the position — `V(w) = position ·
/// V(a)` — so the position reads directly as the output fraction. Each leg is
/// floored at a tiny resistance so an end-stop never stamps a singular 0 Ω short.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Potentiometer {
    /// End-to-end resistance (`a` to `b`), in ohms.
    pub total_ohms: f64,
}

impl Default for Potentiometer {
    /// A 10 kΩ linear pot.
    fn default() -> Self {
        Self {
            total_ohms: 10_000.0,
        }
    }
}

impl Potentiometer {
    /// The `(a→w, w→b)` leg resistances for a wiper `position` (clamped `0..=1`),
    /// each floored at a tiny value so neither leg is a singular short.
    fn legs(&self, position: f64) -> (f64, f64) {
        let p = position.clamp(0.0, 1.0);
        let floor = (self.total_ohms * 1e-9).max(1e-6);
        // position 1 → wiper at `a` (aw ≈ 0), position 0 → wiper at `b`, so a
        // supply-to-ground divider reads V(w) = position · V(a).
        let aw = (self.total_ohms * (1.0 - p)).max(floor);
        let wb = (self.total_ohms * p).max(floor);
        (aw, wb)
    }

    /// Stamp the pot as two resistors — `"{reference}_aw"` (`a`→`w`) and
    /// `"{reference}_wb"` (`w`→`b`) — for a wiper `position` (`0` at `b`, `1` at
    /// `a`, clamped).
    pub fn stamp(
        &self,
        circuit: &mut Circuit,
        reference: &str,
        a: NetId,
        w: NetId,
        b: NetId,
        position: f64,
    ) {
        let (aw, wb) = self.legs(position);
        circuit.add_part(
            format!("{reference}_aw"),
            PartKind::Resistor,
            ohms(aw),
            vec![a, w],
        );
        circuit.add_part(
            format!("{reference}_wb"),
            PartKind::Resistor,
            ohms(wb),
            vec![w, b],
        );
    }

    /// Move the wiper of an already-stamped pot to `position`, updating both legs.
    /// Returns `true` only if both legs (`"{reference}_aw"`/`"{reference}_wb"`)
    /// exist.
    pub fn set_position(&self, circuit: &mut Circuit, reference: &str, position: f64) -> bool {
        let (aw, wb) = self.legs(position);
        let a = circuit.set_value(&format!("{reference}_aw"), Value::new(aw, Unit::Ohm));
        let b = circuit.set_value(&format!("{reference}_wb"), Value::new(wb, Unit::Ohm));
        a && b
    }
}

/// A brushed DC motor: a lumped electro-mechanical model built entirely from
/// primitives.
///
/// The electrical side is the armature resistance `Ra` in series with the
/// inductance `La` and a back-EMF source `E = Ke·ω`. The mechanical side uses the
/// standard force/voltage analogy — shaft speed `ω` is the voltage on an internal
/// node, so the equation of motion `J·dω/dt + B·ω = Kt·I` becomes a capacitor
/// `J` charged by a current `Kt·I` against a resistor `1/B` to ground. The two
/// halves couple through controlled sources:
///
/// - the back-EMF is a [`PartKind::Vcvs`] of gain `Ke` driven by the speed node,
/// - the armature current `I` is read by a 0 V sense source, and
/// - the torque `Kt·I` is injected into the speed node by a [`PartKind::Cccs`].
///
/// Driven from rest by a step voltage, the transient shows the shaft spinning up
/// toward its steady-state speed `ω_ss = V / (Ke + B·Ra/Kt)`, the capacitor
/// integrating the net torque over the run. Resistances/inductances/capacitances
/// carry SI electrical units; the mechanical parameters ride the analogue (the
/// inertia as farads, `1/B` as ohms) and the speed node reads in volts-per-(rad/s).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DcMotor {
    /// Armature resistance `Ra`, in ohms.
    pub armature_ohms: f64,
    /// Armature inductance `La`, in henries.
    pub armature_henries: f64,
    /// Back-EMF constant `Ke`, in volts per (rad/s).
    pub ke: f64,
    /// Torque constant `Kt`, in newton-metres per ampere.
    pub kt: f64,
    /// Rotor moment of inertia `J` (analogue farads on the speed node).
    pub inertia: f64,
    /// Viscous damping `B` (the speed node's leak is `1/B` ohms).
    pub damping: f64,
}

impl Default for DcMotor {
    /// A small hobby motor: `Ra = 1 Ω`, `La = 0.5 mH`, `Ke = Kt = 0.01`,
    /// `J = 1e-5`, `B = 1e-4`.
    fn default() -> Self {
        Self {
            armature_ohms: 1.0,
            armature_henries: 0.5e-3,
            ke: 0.01,
            kt: 0.01,
            inertia: 1e-5,
            damping: 1e-4,
        }
    }
}

impl DcMotor {
    /// The analytic steady-state speed (rad/s) for a constant terminal voltage.
    pub fn steady_state_speed(&self, volts: f64) -> f64 {
        volts / (self.ke + self.damping * self.armature_ohms / self.kt)
    }

    /// Stamp the motor between its `pos`/`neg` terminals and return the internal
    /// **speed node**, whose voltage is the shaft speed `ω` (rad/s). Internal
    /// parts are named from `reference`: `_Ra`, `_La`, `_sense` (the 0 V armature
    /// current sense), `_bemf` (back-EMF VCVS), `_torque` (torque CCCS), `_J`
    /// (inertia capacitor) and `_B` (damping resistor).
    pub fn stamp(&self, circuit: &mut Circuit, reference: &str, pos: NetId, neg: NetId) -> NetId {
        let m1 = circuit.new_net();
        let m2 = circuit.new_net();
        let m3 = circuit.new_net();
        let speed = circuit.new_net();

        // Electrical loop: pos → Ra → La → (sense) → back-EMF → neg.
        circuit.add_part(
            format!("{reference}_Ra"),
            PartKind::Resistor,
            ohms(self.armature_ohms),
            vec![pos, m1],
        );
        circuit.add_part(
            format!("{reference}_La"),
            PartKind::Inductor,
            henries(self.armature_henries),
            vec![m1, m2],
        );
        // 0 V sense source: its branch current is the armature current I, flowing
        // m2 → m3 (into its + terminal), so I(sense) > 0 drives the motor forward.
        let sense = format!("{reference}_sense");
        circuit.add_part(
            sense.clone(),
            PartKind::VoltageSource,
            Some(Value::new(0.0, Unit::Volt)),
            vec![m2, m3],
        );
        // Back-EMF E = Ke·ω opposing the current: V(m3) − V(neg) = Ke·V(speed).
        circuit.add_part(
            format!("{reference}_bemf"),
            PartKind::Vcvs(self.ke),
            None,
            vec![m3, neg, speed, NetId::GROUND],
        );

        // Mechanical side (force/voltage analogue): torque Kt·I injected into the
        // speed node, inertia J as a capacitor, damping B as a 1/B resistor.
        circuit.add_part(
            format!("{reference}_torque"),
            PartKind::Cccs(self.kt, sense),
            None,
            vec![NetId::GROUND, speed],
        );
        circuit.add_part(
            format!("{reference}_J"),
            PartKind::Capacitor,
            farads(self.inertia),
            vec![speed, NetId::GROUND],
        );
        circuit.add_part(
            format!("{reference}_B"),
            PartKind::Resistor,
            ohms(1.0 / self.damping),
            vec![speed, NetId::GROUND],
        );

        speed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ohmforge_sim::{Analysis, Engine};

    /// Solve the operating point and read back the DC voltage on `net`.
    fn node_voltage(c: &Circuit, net: NetId) -> f64 {
        let sigs = Engine::new()
            .run(c, &Analysis::OperatingPoint)
            .expect("operating point solves");
        signal_last(&sigs, c, net)
    }

    /// The last sample of `v(net)` in a result set (the final value of a transient
    /// series, or the single operating-point value).
    fn signal_last(sigs: &[ohmforge_core::Signal], c: &Circuit, net: NetId) -> f64 {
        let label = format!("v(n{})", c.resolve(net).0);
        sigs.iter()
            .find(|s| s.name == label)
            .and_then(|s| s.real())
            .and_then(|y| y.last().copied())
            .unwrap_or_else(|| panic!("no signal {label} in {sigs:?}"))
    }

    /// Build a 5 V divider: V1 at `top`, a fixed 10 kΩ `top`→`mid`, and a
    /// thermistor `mid`→gnd. Returns the circuit and the `mid` net.
    fn thermistor_divider(th: &Thermistor, temp: f64) -> (Circuit, NetId) {
        let mut c = Circuit::new("ntc");
        let top = c.new_net();
        let mid = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            Some(Value::new(5.0, Unit::Volt)),
            vec![top, NetId::GROUND],
        );
        c.add_part("Rtop", PartKind::Resistor, ohms(10_000.0), vec![top, mid]);
        th.stamp(&mut c, "RT", mid, NetId::GROUND, temp);
        (c, mid)
    }

    #[test]
    fn thermistor_divider_shifts_with_temperature() {
        let th = Thermistor::default(); // 10 kΩ @ 25 °C, β = 3950
                                        // Expected divider midpoint from the β-model resistance at each temp.
        let expect = |temp: f64| {
            let r = th.resistance(temp);
            5.0 * r / (10_000.0 + r)
        };

        let (c25, mid) = thermistor_divider(&th, 25.0);
        let v25 = node_voltage(&c25, mid);
        // At 25 °C the NTC is exactly 10 kΩ → a balanced divider at 2.5 V.
        assert!((v25 - 2.5).abs() < 1e-3, "25 °C → 2.5 V, got {v25}");
        assert!((v25 - expect(25.0)).abs() < 1e-6);

        let (c70, mid70) = thermistor_divider(&th, 70.0);
        let v70 = node_voltage(&c70, mid70);
        // Hotter NTC → lower resistance → lower midpoint, matching the β-model.
        assert!(
            (v70 - expect(70.0)).abs() < 1e-6,
            "70 °C midpoint tracks β-model"
        );
        assert!(
            v70 < v25 - 0.5,
            "70 °C ({v70} V) well below 25 °C ({v25} V)"
        );
    }

    #[test]
    fn thermistor_apply_updates_resistor_and_circuit_temperature() {
        let th = Thermistor::default();
        let (mut c, mid) = thermistor_divider(&th, 25.0);
        assert!(th.apply(&mut c, "RT", 70.0), "RT exists and updates");
        assert!(
            (c.temperature() - 70.0).abs() < 1e-9,
            "circuit temp hook set"
        );
        let v = node_voltage(&c, mid);
        let r = th.resistance(70.0);
        assert!((v - 5.0 * r / (10_000.0 + r)).abs() < 1e-6);
        assert!(!th.apply(&mut c, "nope", 50.0), "unknown ref → false");
    }

    #[test]
    fn ldr_resistance_tracks_illuminance() {
        let ldr = Ldr::default(); // 10 kΩ @ 10 lux, γ = 0.7
                                  // At the reference illuminance the resistance is exactly r_ref.
        assert!((ldr.resistance(10.0) - 10_000.0).abs() < 1e-6);
        // Brighter → lower resistance; darker → higher. Monotone decreasing.
        let bright = ldr.resistance(1000.0);
        let dim = ldr.resistance(1.0);
        assert!(bright < 10_000.0 && dim > 10_000.0);
        assert!(bright < ldr.resistance(100.0));
        // Matches the closed-form power law at a checked point.
        let expect = 10_000.0 * (10.0f64 / 1000.0).powf(0.7);
        assert!(
            (bright - expect).abs() < 1e-6,
            "power-law value, got {bright}"
        );

        // Stamped into a divider, the node voltage rises as the LDR brightens
        // (its resistance drops), relative to a fixed top resistor.
        let divider = |lux: f64| {
            let mut c = Circuit::new("ldr");
            let top = c.new_net();
            let mid = c.new_net();
            c.add_part(
                "V1",
                PartKind::VoltageSource,
                Some(Value::new(5.0, Unit::Volt)),
                vec![top, NetId::GROUND],
            );
            c.add_part("Rtop", PartKind::Resistor, ohms(10_000.0), vec![top, mid]);
            ldr.stamp(&mut c, "LDR", mid, NetId::GROUND, lux);
            node_voltage(&c, mid)
        };
        assert!(divider(1000.0) < divider(1.0), "brighter → lower midpoint");
    }

    #[test]
    fn potentiometer_wiper_divides_linearly() {
        // a = 6 V supply, b = ground; the wiper voltage is linear in the position.
        let pot = Potentiometer::default();
        let wiper_v = |pos: f64| {
            let mut c = Circuit::new("pot");
            let a = c.new_net();
            let w = c.new_net();
            c.add_part(
                "V1",
                PartKind::VoltageSource,
                Some(Value::new(6.0, Unit::Volt)),
                vec![a, NetId::GROUND],
            );
            pot.stamp(&mut c, "RV", a, w, NetId::GROUND, pos);
            node_voltage(&c, w)
        };
        // position 0 = wiper at `a` (full supply), 1 = at `b` (ground).
        assert!((wiper_v(0.5) - 3.0).abs() < 1e-6, "mid wiper → 3 V");
        assert!((wiper_v(0.25) - 1.5).abs() < 1e-3, "quarter wiper → 1.5 V");
        assert!(
            (wiper_v(0.75) - 4.5).abs() < 1e-3,
            "three-quarter wiper → 4.5 V"
        );
    }

    #[test]
    fn dc_motor_spins_up_to_steady_state() {
        // Step the armature with 12 V from rest and watch ω integrate up.
        let motor = DcMotor::default();
        let mut c = Circuit::new("motor");
        let term = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            Some(Value::new(12.0, Unit::Volt)),
            vec![term, NetId::GROUND],
        );
        let speed = motor.stamp(&mut c, "M1", term, NetId::GROUND);

        let full = Engine::new()
            .run(
                &c,
                &Analysis::Transient {
                    duration: 0.5,
                    step: Some(1e-4),
                },
            )
            .expect("transient solves");
        let label = format!("v(n{})", c.resolve(speed).0);
        let omega = full
            .iter()
            .find(|s| s.name == label)
            .and_then(|s| s.real())
            .expect("speed signal present");

        let expected = motor.steady_state_speed(12.0); // 600 rad/s with defaults
        let start = omega[0];
        let end = *omega.last().unwrap();
        // Starts from rest, and the mid-run speed is well below the end: a spin-up.
        assert!(start.abs() < 1.0, "starts from rest, got {start}");
        assert!(
            omega[omega.len() / 10] < 0.9 * end,
            "early speed below the plateau (spin-up transient)"
        );
        // Monotone rise toward the analytic steady-state speed.
        assert!(
            omega.windows(2).all(|w| w[1] >= w[0] - 1e-9),
            "speed rises monotonically"
        );
        assert!(
            (end - expected).abs() < 0.03 * expected,
            "settles near ω_ss = {expected}, got {end}"
        );
    }
}
