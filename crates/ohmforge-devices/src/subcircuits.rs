//! Composite analog subcircuits: common parts built by **expanding into the
//! existing [`ohmforge_core`] primitives** (resistors, capacitors, a VCVS) rather than
//! adding new [`PartKind`] variants.
//!
//! Each model is a plain configuration struct with a `stamp` method that adds
//! its primitive parts (and any internal nets) to a [`Circuit`]. That keeps the
//! solver's device set small — it only ever sees R/C/VCVS — while the shell and
//! parts library can offer higher-level building blocks:
//!
//! - [`OpAmp`] — an op-amp macromodel: a VCVS for the finite open-loop gain
//!   feeding an output resistor (and optional capacitor) for the output
//!   impedance. In a closed loop it behaves like a real op-amp; in a
//!   non-inverting divider it solves to `1 + Rf/Rg`.
//! - [`AnalogSwitch`] — a voltage-controlled resistance: a resistor stamped at
//!   its low (on) or high (off) value under a control, so it passes or blocks a
//!   DC level.
//! - [`Relay`] — an SPST relay contact: the same switched-resistance idea with a
//!   low contact resistance when energized and a near-open resistance otherwise.
//!
//! The builders are pure and deterministic, so they run identically on native
//! and `wasm32`. (The `ohmforge-sim` dependency the gate tests use is a dev-dependency
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

/// An op-amp macromodel built from a VCVS plus an output R (and optional C).
///
/// The input stage is ideal (infinite input impedance — the VCVS senses the
/// differential input without loading it). A [`PartKind::Vcvs`] with the finite
/// `open_loop_gain` drives an internal node `e = A·(V+ − V−)`; an output
/// resistor of `output_ohms` from `e` to the output pin models the open-loop
/// output impedance, and an optional `output_farads` capacitor to ground forms
/// the output RC. Because the output resistor sits *inside* the feedback loop,
/// the closed-loop behaviour is governed by the external network — a
/// non-inverting divider solves to `1 + Rf/Rg`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OpAmp {
    /// Open-loop DC gain `A` (V/V).
    pub open_loop_gain: f64,
    /// Open-loop output resistance (Ω), in series with the output pin.
    pub output_ohms: f64,
    /// Output capacitance to ground (F) forming the output RC pole; `0` stamps
    /// no capacitor (a pure output resistor). Open at DC, so it never changes
    /// the operating point.
    pub output_farads: f64,
}

impl Default for OpAmp {
    /// A generic high-gain op-amp: `A = 1e6`, 75 Ω output impedance, no output
    /// capacitor.
    fn default() -> Self {
        Self {
            open_loop_gain: 1e6,
            output_ohms: 75.0,
            output_farads: 0.0,
        }
    }
}

impl OpAmp {
    /// Stamp the op-amp between its non-inverting input `in_p`, inverting input
    /// `in_n` and output `out`. Internal parts are named from `reference`: the
    /// VCVS takes `reference` itself, the output resistor `"{reference}_Rout"`,
    /// and the output capacitor (if any) `"{reference}_Cout"`.
    pub fn stamp(
        &self,
        circuit: &mut Circuit,
        reference: &str,
        in_p: NetId,
        in_n: NetId,
        out: NetId,
    ) {
        // Internal gain node: e = A·(V+ − V−) against ground.
        let e = circuit.new_net();
        circuit.add_part(
            reference.to_string(),
            PartKind::Vcvs(self.open_loop_gain),
            None,
            vec![e, NetId::GROUND, in_p, in_n],
        );
        // Output impedance: e → out through R_out.
        circuit.add_part(
            format!("{reference}_Rout"),
            PartKind::Resistor,
            ohms(self.output_ohms),
            vec![e, out],
        );
        if self.output_farads > 0.0 {
            circuit.add_part(
                format!("{reference}_Cout"),
                PartKind::Capacitor,
                farads(self.output_farads),
                vec![out, NetId::GROUND],
            );
        }
    }
}

/// A voltage-controlled analog switch, modelled as a single resistance that
/// takes its low (`on_ohms`) value when the control closes it and its high
/// (`off_ohms`) value when the control opens it.
///
/// The control is the switch's logical state at stamp time (the level driving
/// its enable), which is how a drop-in part maps a digital control line onto the
/// static circuit the solver sees. A closed switch passes a DC level; an open
/// one blocks it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnalogSwitch {
    /// On-state (closed) resistance, in ohms.
    pub on_ohms: f64,
    /// Off-state (open) resistance, in ohms.
    pub off_ohms: f64,
}

impl Default for AnalogSwitch {
    /// A typical CMOS switch: ~100 Ω on, ~1 GΩ off.
    fn default() -> Self {
        Self {
            on_ohms: 100.0,
            off_ohms: 1e9,
        }
    }
}

impl AnalogSwitch {
    /// Stamp the switch between nets `a` and `b`, closed (`on_ohms`) when
    /// `closed`, otherwise open (`off_ohms`).
    pub fn stamp(&self, circuit: &mut Circuit, reference: &str, a: NetId, b: NetId, closed: bool) {
        let r = if closed { self.on_ohms } else { self.off_ohms };
        circuit.add_part(
            reference.to_string(),
            PartKind::Resistor,
            ohms(r),
            vec![a, b],
        );
    }
}

/// An SPST relay contact, modelled as a switched resistance: a low
/// `contact_ohms` when the coil is energized (contact closed) and a near-open
/// `open_ohms` when it is not.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Relay {
    /// Closed-contact resistance, in ohms.
    pub contact_ohms: f64,
    /// Open-contact resistance, in ohms.
    pub open_ohms: f64,
}

impl Default for Relay {
    /// A small signal relay: ~0.1 Ω closed, ~1 TΩ open.
    fn default() -> Self {
        Self {
            contact_ohms: 0.1,
            open_ohms: 1e12,
        }
    }
}

impl Relay {
    /// Stamp the SPST contact between nets `a` and `b`, closed (`contact_ohms`)
    /// when `energized`, otherwise open (`open_ohms`).
    pub fn stamp(
        &self,
        circuit: &mut Circuit,
        reference: &str,
        a: NetId,
        b: NetId,
        energized: bool,
    ) {
        let r = if energized {
            self.contact_ohms
        } else {
            self.open_ohms
        };
        circuit.add_part(
            reference.to_string(),
            PartKind::Resistor,
            ohms(r),
            vec![a, b],
        );
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
        let label = format!("v(n{})", c.resolve(net).0);
        sigs.iter()
            .find(|s| s.name == label)
            .and_then(|s| s.real())
            .map(|y| y[0])
            .unwrap_or_else(|| panic!("no signal {label} in {:?}", sigs))
    }

    #[test]
    fn opamp_stamps_a_vcvs_and_output_resistor() {
        let mut c = Circuit::new("op");
        let (a, b, o) = (c.new_net(), c.new_net(), c.new_net());
        OpAmp::default().stamp(&mut c, "U1", a, b, o);
        assert!(matches!(
            c.part_by_ref("U1").unwrap().kind,
            PartKind::Vcvs(_)
        ));
        assert!(matches!(
            c.part_by_ref("U1_Rout").unwrap().kind,
            PartKind::Resistor
        ));
        // Default has no output capacitor.
        assert!(c.part_by_ref("U1_Cout").is_none());
    }

    #[test]
    fn opamp_noninverting_gain_solves_to_one_plus_rf_over_rg() {
        // Non-inverting amp: Vin → V+, feedback Rf (out → V−), Rg (V− → gnd).
        // Closed-loop gain 1 + Rf/Rg = 1 + 9k/1k = 10, so 1 V in → ~10 V out.
        let mut c = Circuit::new("opamp");
        let vin = c.new_net();
        let vfb = c.new_net();
        let vout = c.new_net();
        c.add_part(
            "Vin",
            PartKind::VoltageSource,
            Some(Value::new(1.0, Unit::Volt)),
            vec![vin, NetId::GROUND],
        );
        OpAmp::default().stamp(&mut c, "U1", vin, vfb, vout);
        c.add_part(
            "Rf",
            PartKind::Resistor,
            Some(Value::new(9000.0, Unit::Ohm)),
            vec![vout, vfb],
        );
        c.add_part(
            "Rg",
            PartKind::Resistor,
            Some(Value::new(1000.0, Unit::Ohm)),
            vec![vfb, NetId::GROUND],
        );
        let vo = node_voltage(&c, vout);
        assert!(
            (vo - 10.0).abs() < 1e-2,
            "non-inverting gain ≈ 1 + Rf/Rg = 10, got {vo} V"
        );
    }

    #[test]
    fn analog_switch_passes_when_closed_and_blocks_when_open() {
        // 5 V source through the switch into a 10 kΩ load to ground.
        let divider = |closed| {
            let mut c = Circuit::new("sw");
            let src = c.new_net();
            let out = c.new_net();
            c.add_part(
                "V1",
                PartKind::VoltageSource,
                Some(Value::new(5.0, Unit::Volt)),
                vec![src, NetId::GROUND],
            );
            AnalogSwitch::default().stamp(&mut c, "SW1", src, out, closed);
            c.add_part(
                "Rload",
                PartKind::Resistor,
                Some(Value::new(10_000.0, Unit::Ohm)),
                vec![out, NetId::GROUND],
            );
            node_voltage(&c, out)
        };
        let on = divider(true);
        let off = divider(false);
        assert!(on > 4.9, "closed switch passes the 5 V level, got {on} V");
        assert!(off < 0.1, "open switch blocks the level, got {off} V");
    }

    #[test]
    fn relay_contact_carries_the_load_only_when_energized() {
        // 12 V source through the contact into a 1 kΩ load.
        let divider = |energized| {
            let mut c = Circuit::new("relay");
            let src = c.new_net();
            let out = c.new_net();
            c.add_part(
                "V1",
                PartKind::VoltageSource,
                Some(Value::new(12.0, Unit::Volt)),
                vec![src, NetId::GROUND],
            );
            Relay::default().stamp(&mut c, "K1", src, out, energized);
            c.add_part(
                "Rload",
                PartKind::Resistor,
                Some(Value::new(1000.0, Unit::Ohm)),
                vec![out, NetId::GROUND],
            );
            node_voltage(&c, out)
        };
        assert!(
            divider(true) > 11.9,
            "energized relay carries the load through to the output"
        );
        assert!(divider(false) < 0.1, "de-energized relay opens the output");
    }
}
