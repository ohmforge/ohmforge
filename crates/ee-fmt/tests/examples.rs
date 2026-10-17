//! The shipped `examples/formats/` files are real: each is loaded here and
//! proven to do what its guide claims — the SPICE deck imports and simulates to
//! the divider midpoint, and the native project round-trips byte-for-byte and
//! simulates. If an example drifts from the code, this test fails.

use ee_fmt::{import_spice, load_project, save_project};
use ee_sim::{Analysis, Engine};

/// The example files, embedded at compile time (paths are relative to this
/// source file: `crates/ee-fmt/tests/` → repo root `examples/formats/`).
const DIVIDER_CIR: &str = include_str!("../../../examples/formats/divider.cir");
const RC_LOWPASS: &str = include_str!("../../../examples/formats/rc-lowpass.eeproj");

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
