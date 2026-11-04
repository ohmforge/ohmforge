//! The shipped `examples/formats/` files are real: each is loaded here and
//! proven to do what its guide claims — the SPICE deck imports and simulates to
//! the divider midpoint, the KiCad netlist imports and simulates to the same
//! midpoint, and the native project round-trips byte-for-byte and simulates. If
//! an example drifts from the code, this test fails.

use ee_core::graph::{DiodeModel, NetId, PartKind};
use ee_core::units::Unit;
use ee_core::value::Value;
use ee_fmt::{import_kicad, import_spice, load_project, save_project, to_bom_csv};
use ee_sim::{Analysis, Engine};

/// The example files, embedded at compile time (paths are relative to this
/// source file: `crates/ee-fmt/tests/` → repo root `examples/formats/`).
const DIVIDER_CIR: &str = include_str!("../../../examples/formats/divider.cir");
const DIVIDER_NET: &str = include_str!("../../../examples/formats/divider.net");
const RC_LOWPASS: &str = include_str!("../../../examples/formats/rc-lowpass.eeproj");
const DIODE_CLAMP: &str = include_str!("../../../examples/formats/diode-clamp.cir");

/// Read one solved node voltage by its resolved-net signal name.
fn node_volts(circuit: &ee_core::graph::Circuit, net: ee_core::graph::NetId) -> f64 {
    let sigs = Engine::new()
        .run(circuit, &Analysis::OperatingPoint)
        .expect("operating point solves");
    let name = format!("v(n{})", circuit.resolve(net).0);
    sigs.iter()
        .find(|s| s.name == name)
        .and_then(|s| s.real())
        .map(|v| v[0])
        .unwrap_or(0.0)
}

#[test]
fn example_spice_deck_imports_and_simulates() {
    let circuit = import_spice(DIVIDER_CIR).expect("the example deck imports");
    assert_eq!(
        circuit.name,
        "* ee-assist example: a 10 V resistive divider (DC operating point)"
    );
    assert_eq!(circuit.parts().len(), 3, "V1, R1, R2");

    // Two equal resistors across 10 V put the midpoint at 5 V — a real solve.
    let out = circuit.part_by_ref("R1").unwrap().terminals[1];
    assert!(
        (node_volts(&circuit, out) - 5.0).abs() < 1e-9,
        "divider midpoint is 5 V"
    );
}

#[test]
fn example_kicad_netlist_imports_and_simulates() {
    let circuit = import_kicad(DIVIDER_NET).expect("the example KiCad netlist imports");
    assert_eq!(circuit.parts().len(), 3, "V1, R1, R2");

    // The source parsed its `DC 10` value, and the two equal resistors put the
    // `/out` midpoint at 5 V — a real solve of the imported topology.
    assert_eq!(
        circuit.part_by_ref("V1").unwrap().value.unwrap().magnitude,
        10.0
    );
    let out = circuit.part_by_ref("R1").unwrap().terminals[1];
    assert!(
        (node_volts(&circuit, out) - 5.0).abs() < 1e-9,
        "KiCad divider midpoint is 5 V"
    );

    // And its BOM lists every part: the two 1 kΩ resistors group to qty 2.
    let bom = to_bom_csv(&circuit);
    assert!(bom.starts_with("kind,value,quantity,references\n"), "{bom}");
    assert!(bom.contains("Resistor,1000Ω,2,R1 R2\n"), "{bom}");
    assert!(bom.contains("Voltage source,10V,1,V1\n"), "{bom}");
}

#[test]
fn example_native_project_round_trips_and_simulates() {
    let circuit = load_project(RC_LOWPASS).expect("the example project loads");
    assert_eq!(circuit.name, "rc-lowpass");
    assert_eq!(circuit.parts().len(), 3, "V1, R1, C1");

    // Byte-for-byte lossless: re-saving the loaded project reproduces the file.
    assert_eq!(
        save_project(&circuit),
        RC_LOWPASS,
        "the shipped project file is exactly what save() emits"
    );

    // And it is a solvable circuit: at DC the cap is open, so v(out) = 5 V.
    let out = circuit.part_by_ref("R1").unwrap().terminals[1];
    assert!(
        (node_volts(&circuit, out) - 5.0).abs() < 1e-9,
        "DC output sits at the rail through the series R"
    );
}

#[test]
fn example_spice_model_deck_applies_params_and_simulates() {
    // The `.model`-backed example: its `.model 1N4148 D (IS=…)` card must reach
    // the engine's diode and move the operating point off the default.
    let circuit = import_spice(DIODE_CLAMP).expect("the diode `.model` deck imports");
    assert_eq!(circuit.parts().len(), 3, "V1, R1, D1");

    // The SPICE IS parameter landed on the diode model (amperes).
    let d1 = circuit.part_by_ref("D1").unwrap();
    match &d1.kind {
        PartKind::Diode(m) => assert!(
            (m.is - 2.52e-9).abs() < 1e-18,
            "IS=2.52n mapped onto the diode"
        ),
        other => panic!("expected diode, got {other:?}"),
    }

    // The junction node (D1's anode = R1's far end) settles at a real forward
    // drop in the silicon window.
    let d_node = d1.terminals[0];
    let v_modeled = node_volts(&circuit, d_node);
    assert!(
        (0.25..0.8).contains(&v_modeled),
        "modeled junction drop {v_modeled} V out of range"
    );

    // Proof the params *matter*: the identical topology with a *default* diode
    // (engine IS = 1e-14, N = 1, RS = 0) settles at a different operating point.
    // The 1N4148's larger IS pulls the drop down while its N = 1.752 scales Vt
    // back up, netting a realistic ~0.65 V junction a measurable step off the
    // default's ~0.69 V.
    let mut dflt = ee_core::graph::Circuit::new("default-diode clamp");
    let n_in = dflt.new_net();
    let n_d = dflt.new_net();
    dflt.add_part(
        "V1",
        PartKind::VoltageSource,
        Some(Value::new(5.0, Unit::Volt)),
        vec![n_in, NetId::GROUND],
    );
    dflt.add_part(
        "R1",
        PartKind::Resistor,
        Some(Value::new(1000.0, Unit::Ohm)),
        vec![n_in, n_d],
    );
    dflt.add_part(
        "D1",
        PartKind::Diode(DiodeModel::default()),
        None,
        vec![n_d, NetId::GROUND],
    );
    let v_default = node_volts(&dflt, dflt.part_by_ref("D1").unwrap().terminals[0]);
    assert!(
        v_default > v_modeled && v_default - v_modeled > 0.02,
        "the IS/N/RS model should move the operating point off the default: \
         modeled {v_modeled} V vs default {v_default} V"
    );
}
