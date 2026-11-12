//! CP-BRD-M6.1 host gate: the closed MCU↔analog loop wired to a **real**
//! `ohmforge-sim` node.
//!
//! The `of` binary is the one crate that legitimately links both the MCU and the
//! analog engines, so the end-to-end closed loop — a solved `ohmforge-sim` divider
//! voltage → the AVR ADC → a firmware threshold decision → a GPIO drive — is
//! tested here, not in `ohmforge-mcu` (which stays `ohmforge-sim`-free). The analog value
//! crosses the boundary exactly as the lockstep loop would pass it: a plain
//! number pushed onto the pin through [`PinIo`].

use ohmforge_core::graph::PartKind;
use ohmforge_core::units::Unit;
use ohmforge_core::value::Value;
use ohmforge_core::{Circuit, NetId};
use ohmforge_mcu::avr::Uno;
use ohmforge_mcu::PinDrive;
use ohmforge_sim::{Analysis, Engine};
use ohmforge_tui::NodeVoltages;

/// The CP-BRD-M6.1 firmware: convert A0 and mirror the mid-scale threshold onto
/// D8 (HIGH for a 10-bit code ≥ 512, LOW below). Identical to the `ohmforge-mcu` gate
/// program; duplicated here since an integration test cannot import another
/// crate's test fixtures.
const ADC_THRESHOLD_FW: &[u16] = &[
    0x9A20, // SBI DDRB,0  (PB0/D8 output)
    0xEC00, // LDI r16, ADEN|ADSC
    0x9300, 0x007A, // STS ADCSRA, r16
    0x9100, 0x007A, // LDS r16, ADCSRA
    0xFD06, // SBRC r16,6
    0xCFFC, // RJMP wait
    0x9110, 0x0079, // LDS r17, ADCH
    0x3012, // CPI r17,2
    0xF010, // BRLO low
    0x9A28, // SBI PORTB,0 (D8 HIGH)
    0xCFF3, // RJMP loop
    0x9828, // CBI PORTB,0 (D8 LOW)
    0xCFF1, // RJMP loop
];

/// Build a `V1 → R1 → mid → R2 → gnd` divider and return `v(mid)` from a real
/// `ohmforge-sim` operating-point solve.
fn divider_midpoint(r_top: &str, r_bot: &str) -> f64 {
    let mut c = Circuit::new("divider");
    let vin = c.new_net();
    let mid = c.new_net();
    let gnd = NetId::GROUND;
    c.add_part(
        "V1",
        PartKind::VoltageSource,
        Some(Value::new(5.0, Unit::Volt)),
        vec![vin, gnd],
    );
    c.add_part(
        "R1",
        PartKind::Resistor,
        Some(Value::parse(r_top, Unit::Ohm).unwrap()),
        vec![vin, mid],
    );
    c.add_part(
        "R2",
        PartKind::Resistor,
        Some(Value::parse(r_bot, Unit::Ohm).unwrap()),
        vec![mid, gnd],
    );
    let signals = Engine::new()
        .run(&c, &Analysis::OperatingPoint)
        .expect("op-point solves");
    NodeVoltages::from_signals(&signals)
        .get(c.resolve(mid))
        .expect("v(mid)")
}

/// Run the firmware against a divider whose mid node feeds A0, returning D8's
/// drive after the loop settles.
fn closed_loop(r_top: &str, r_bot: &str) -> (f64, PinDrive) {
    let v_mid = divider_midpoint(r_top, r_bot);

    let mut uno = Uno::new();
    uno.mcu_mut().load_program(ADC_THRESHOLD_FW);
    let mut io = uno.pin_io();
    io.set_input(Uno::analog_pin_id(0).unwrap(), v_mid);
    for _ in 0..400 {
        uno.step_with_io(&mut io);
    }
    (v_mid, io.drive(Uno::digital_pin_id(8).unwrap()))
}

#[test]
fn ee_sim_divider_above_threshold_drives_d8_high() {
    // 1k / 2k divider → v(mid) = 5·2/3 ≈ 3.33 V, above the 2.5 V threshold.
    let (v_mid, d8) = closed_loop("1k", "2k");
    assert!((v_mid - 3.3333).abs() < 1e-3, "solved v(mid) = {v_mid}");
    assert_eq!(
        d8,
        PinDrive::Digital(true),
        "a real ohmforge-sim node above threshold → firmware drives D8 HIGH"
    );
}

#[test]
fn ee_sim_divider_below_threshold_drives_d8_low() {
    // 2k / 1k divider → v(mid) = 5·1/3 ≈ 1.67 V, below the threshold.
    let (v_mid, d8) = closed_loop("2k", "1k");
    assert!((v_mid - 1.6667).abs() < 1e-3, "solved v(mid) = {v_mid}");
    assert_eq!(
        d8,
        PinDrive::Digital(false),
        "a real ohmforge-sim node below threshold → firmware drives D8 LOW"
    );
}
