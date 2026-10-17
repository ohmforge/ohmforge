//! Golden / determinism harness (`CP-PLAT.5`).
//!
//! A fixed set of representative analyses — DC operating point, transient (fixed
//! and adaptive step), AC, noise, and Monte-Carlo — run through the public
//! [`Engine`] API with fixed parameters and fixed seeds (no wall-clock, no
//! unseeded randomness). The harness asserts two things:
//!
//! 1. **Determinism.** Every case, run twice, produces *bit-for-bit* identical
//!    [`Signal`] output. [`Signal`] derives [`PartialEq`] over its name, unit,
//!    domain and full `x`/`y` sample buffers, so `assert_eq!` is an exact,
//!    whole-waveform comparison. [`full_golden_suite_runs_twice_identically`] is
//!    the aggregate gate: the entire suite, compared as one value, must match.
//!
//! 2. **Golden values.** Where the expected result is exactly representable
//!    (resistive dividers, the source-pinned ladder input, the nominal
//!    Monte-Carlo draw) the harness pins it to the bit; where it rides on
//!    transcendental library functions (`exp`/`sin` in the transient, AC and
//!    noise paths, whose last bits are platform `libm` details) it pins the
//!    physics to a tight tolerance instead. The reproducibility above is the hard
//!    gate; these anchor it to known-good numbers.
//!
//! The Monte-Carlo draw is reproducible to the bit *across platforms*, not merely
//! within a run: its only arithmetic is the in-crate SplitMix64 PRNG (integer
//! ops) and the divider's exact `f64` add/mul/div — no transcendentals — so the
//! committed sample constants below are portable.

use ee_core::graph::{Circuit, NetId, PartKind, Waveform};
use ee_core::{Signal, Unit, Value};
use ee_sim::{Analysis, Engine};

/// Boltzmann constant and the noise temperature the engine uses (mirrored here —
/// they are private to the crate — so the noise golden is self-checking).
const BOLTZMANN_K: f64 = 1.380_649e-23;
const NOISE_TEMP_K: f64 = 300.15;

fn ohms(s: &str) -> Option<Value> {
    Some(Value::parse(s, Unit::Ohm).unwrap())
}
fn volts(s: &str) -> Option<Value> {
    Some(Value::parse(s, Unit::Volt).unwrap())
}

/// A 10 V / 1 kΩ / 1 kΩ divider; returns the circuit and the midpoint net.
fn divider() -> (Circuit, NetId) {
    let mut c = Circuit::new("divider");
    let vin = c.new_net();
    let mid = c.new_net();
    c.add_part(
        "V1",
        PartKind::VoltageSource,
        volts("10"),
        vec![vin, NetId::GROUND],
    );
    c.add_part("R1", PartKind::Resistor, ohms("1k"), vec![vin, mid]);
    c.add_part(
        "R2",
        PartKind::Resistor,
        ohms("1k"),
        vec![mid, NetId::GROUND],
    );
    (c, mid)
}

/// A 5 V step into a 1 kΩ / 1 µF low-pass; returns the circuit and the output net.
fn rc() -> (Circuit, NetId) {
    let mut c = Circuit::new("rc");
    let vin = c.new_net();
    let out = c.new_net();
    c.add_part(
        "V1",
        PartKind::VoltageSource,
        volts("5"),
        vec![vin, NetId::GROUND],
    );
    c.add_part("R1", PartKind::Resistor, ohms("1k"), vec![vin, out]);
    c.add_part(
        "C1",
        PartKind::Capacitor,
        Some(Value::new(1e-6, Unit::Farad)),
        vec![out, NetId::GROUND],
    );
    (c, out)
}

/// The RC low-pass driven by a 1∠0 AC sine source; returns circuit, input, output.
fn rc_ac() -> (Circuit, NetId, NetId) {
    let (r, cap) = (1_000.0_f64, 159.154_943e-9_f64); // fc ≈ 1 kHz
    let mut c = Circuit::new("rc_ac");
    let inp = c.new_net();
    let out = c.new_net();
    c.add_part(
        "V1",
        PartKind::VoltageSource,
        None,
        vec![inp, NetId::GROUND],
    );
    c.add_part(
        "R1",
        PartKind::Resistor,
        Some(Value::new(r, Unit::Ohm)),
        vec![inp, out],
    );
    c.add_part(
        "C1",
        PartKind::Capacitor,
        Some(Value::new(cap, Unit::Farad)),
        vec![out, NetId::GROUND],
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
    (c, inp, out)
}

/// A lone 1 kΩ resistor to ground; returns the circuit and its node.
fn one_resistor() -> (Circuit, NetId) {
    let mut c = Circuit::new("one_r");
    let out = c.new_net();
    c.add_part(
        "R1",
        PartKind::Resistor,
        ohms("1k"),
        vec![out, NetId::GROUND],
    );
    (c, out)
}

/// The divider with both resistors toleranced ±5 % (the Monte-Carlo fixture).
fn mc_divider() -> (Circuit, NetId) {
    let (mut c, mid) = divider();
    assert!(c.set_tolerance("R1", 0.05));
    assert!(c.set_tolerance("R2", 0.05));
    (c, mid)
}

/// An `n`-rung resistor ladder (10 V source, series + shunt 1 kΩ), returning the
/// circuit and its input net. Large `n` drives the engine down the sparse solve.
fn ladder(n: usize) -> (Circuit, NetId) {
    let mut c = Circuit::new("ladder");
    let vin = c.new_net();
    c.add_part(
        "V1",
        PartKind::VoltageSource,
        volts("10"),
        vec![vin, NetId::GROUND],
    );
    let mut prev = vin;
    for i in 0..n {
        let node = c.new_net();
        c.add_part(
            format!("Rs{i}"),
            PartKind::Resistor,
            ohms("1k"),
            vec![prev, node],
        );
        c.add_part(
            format!("Rp{i}"),
            PartKind::Resistor,
            ohms("1k"),
            vec![node, NetId::GROUND],
        );
        prev = node;
    }
    (c, vin)
}

// --- The fixed analysis suite ------------------------------------------------

/// The representative analyses, each with fixed parameters / seeds. Keeping them
/// in one place means the determinism gate and the per-analysis tests exercise
/// exactly the same set.
fn suite() -> Vec<(Circuit, Analysis)> {
    let ac = Analysis::Ac {
        f_start: 10.0,
        f_stop: 100_000.0,
        points_per_decade: 10,
    };
    vec![
        (divider().0, Analysis::OperatingPoint),
        (ladder(200).0, Analysis::OperatingPoint), // sparse-solve path
        (
            rc().0,
            Analysis::Transient {
                duration: 5e-3,
                step: Some(5e-6),
            },
        ),
        (
            rc().0,
            Analysis::Transient {
                duration: 5e-3,
                step: None,
            },
        ), // adaptive
        (rc_ac().0, ac),
        (
            one_resistor().0,
            Analysis::Noise {
                output: one_resistor().1,
                f_start: 1.0,
                f_stop: 1e6,
                points_per_decade: 5,
            },
        ),
        (
            mc_divider().0,
            Analysis::MonteCarlo {
                seed: 0xC0FFEE,
                runs: 256,
                output: mc_divider().1,
                sample: 0,
                inner: Box::new(Analysis::OperatingPoint),
            },
        ),
    ]
}

/// The aggregate `CP-PLAT.5` gate: the whole fixed suite, run twice, is
/// bit-for-bit identical. One `assert_eq!` over the entire `Vec<Vec<Signal>>`
/// compares every analysis's every sample.
#[test]
fn full_golden_suite_runs_twice_identically() {
    let engine = Engine::new();
    let run = || -> Vec<Vec<Signal>> {
        suite()
            .iter()
            .map(|(c, a)| engine.run(c, a).expect("analysis runs"))
            .collect()
    };
    let first = run();
    let second = run();
    assert_eq!(
        first, second,
        "the golden suite is not bit-for-bit reproducible"
    );
    // Sanity that the suite actually produced output (guards against an empty run
    // vacuously "matching").
    assert_eq!(first.len(), 7, "unexpected suite length");
    assert!(
        first.iter().all(|sigs| !sigs.is_empty()),
        "an analysis produced no signals"
    );
}

/// DC operating point: reproducible, and the divider midpoint is exactly 5 V
/// (resistive, so IEEE-exact) — a committed golden pinned to the bit.
#[test]
fn op_point_deterministic_and_golden() {
    let (c, mid) = divider();
    let engine = Engine::new();
    let a = engine.run(&c, &Analysis::OperatingPoint).unwrap();
    let b = engine.run(&c, &Analysis::OperatingPoint).unwrap();
    assert_eq!(a, b, "op point not reproducible");

    let name = format!("v(n{})", c.resolve(mid).0);
    let mid_v = a.iter().find(|s| s.name == name).unwrap().real().unwrap()[0];
    assert_eq!(mid_v, 5.0, "divider midpoint golden (exact 5 V)");
}

/// A 200-rung ladder's operating point (the sparse solve) is deterministic, and
/// its source-pinned input is exactly 10 V — a committed golden for the sparse
/// path specifically.
#[test]
fn sparse_op_point_deterministic_and_golden() {
    let (c, vin) = ladder(200);
    let engine = Engine::new();
    let a = engine.run(&c, &Analysis::OperatingPoint).unwrap();
    let b = engine.run(&c, &Analysis::OperatingPoint).unwrap();
    assert_eq!(a, b, "sparse op point not reproducible");

    let name = format!("v(n{})", c.resolve(vin).0);
    let vin_v = a.iter().find(|s| s.name == name).unwrap().real().unwrap()[0];
    assert_eq!(vin_v, 10.0, "ladder input golden (exact 10 V)");
}

/// Transient (both fixed and adaptive step) is bit-for-bit reproducible; the
/// fixed-step RC charges from exactly 0 V toward its 5 V rail (physics golden).
#[test]
fn transient_deterministic_and_golden() {
    let (c, out) = rc();
    let engine = Engine::new();
    let name = format!("v(n{})", c.resolve(out).0);

    for step in [Some(5e-6), None] {
        let a = engine
            .run(
                &c,
                &Analysis::Transient {
                    duration: 5e-3,
                    step,
                },
            )
            .unwrap();
        let b = engine
            .run(
                &c,
                &Analysis::Transient {
                    duration: 5e-3,
                    step,
                },
            )
            .unwrap();
        assert_eq!(a, b, "transient (step={step:?}) not reproducible");

        let sig = a.iter().find(|s| s.name == name).unwrap();
        let y = sig.real().unwrap();
        assert_eq!(y[0], 0.0, "RC starts uncharged (exact 0 V)");
        assert!(*y.last().unwrap() > 4.9, "RC should near its 5 V rail");
    }
}

/// AC is bit-for-bit reproducible; the input node is the 1∠0 reference (unity
/// magnitude) at every frequency (physics golden).
#[test]
fn ac_deterministic_and_golden() {
    let (c, inp, _out) = rc_ac();
    let engine = Engine::new();
    let a_in = Analysis::Ac {
        f_start: 10.0,
        f_stop: 100_000.0,
        points_per_decade: 10,
    };
    let a = engine.run(&c, &a_in).unwrap();
    let b = engine.run(&c, &a_in).unwrap();
    assert_eq!(a, b, "AC not reproducible");

    let name = format!("v(n{})", c.resolve(inp).0);
    let z = a
        .iter()
        .find(|s| s.name == name)
        .unwrap()
        .complex()
        .unwrap();
    for h in z {
        assert!(
            (h.magnitude() - 1.0).abs() < 1e-12,
            "input node not unity reference"
        );
    }
}

/// Noise is bit-for-bit reproducible; a lone resistor's output PSD is the flat
/// 4kTR thermal floor at every frequency (physics golden to a tight tolerance).
#[test]
fn noise_deterministic_and_golden() {
    let (c, out) = one_resistor();
    let engine = Engine::new();
    let a_noise = Analysis::Noise {
        output: out,
        f_start: 1.0,
        f_stop: 1e6,
        points_per_decade: 5,
    };
    let a = engine.run(&c, &a_noise).unwrap();
    let b = engine.run(&c, &a_noise).unwrap();
    assert_eq!(a, b, "noise not reproducible");

    let expected = 4.0 * BOLTZMANN_K * NOISE_TEMP_K * 1_000.0; // 4kTR, R = 1 kΩ
    let name = format!("onoise(n{})", c.resolve(out).0);
    for &s in a.iter().find(|s| s.name == name).unwrap().real().unwrap() {
        assert!(
            (s - expected).abs() <= 1e-6 * expected,
            "noise PSD {s} != 4kTR {expected}"
        );
    }
}

/// Monte-Carlo under a fixed seed is bit-for-bit reproducible, and — being free
/// of transcendentals — its first four samples are portable committed goldens.
/// (A different seed must give a different, still-deterministic draw.)
#[test]
fn monte_carlo_deterministic_and_golden() {
    let (c, mid) = mc_divider();
    let engine = Engine::new();
    let mc = |seed| Analysis::MonteCarlo {
        seed,
        runs: 256,
        output: mid,
        sample: 0,
        inner: Box::new(Analysis::OperatingPoint),
    };
    let a = engine.run(&c, &mc(0xC0FFEE)).unwrap();
    let b = engine.run(&c, &mc(0xC0FFEE)).unwrap();
    assert_eq!(a, b, "Monte-Carlo not reproducible under a fixed seed");

    let samples = a[0].real().unwrap();
    assert_eq!(samples.len(), 256);
    // Committed golden: the exact first four draws for seed 0xC0FFEE (SplitMix64 +
    // exact divider arithmetic → portable to the bit).
    const GOLDEN: [f64; 4] = [
        5.032_416_807_567_123,
        4.955_502_538_168_007,
        5.031_411_364_567_176,
        5.210_784_676_963_599,
    ];
    for (i, &g) in GOLDEN.iter().enumerate() {
        assert_eq!(samples[i], g, "Monte-Carlo golden sample {i} drifted");
    }

    let d = engine.run(&c, &mc(7)).unwrap();
    assert_ne!(a, d, "a different seed must give a different draw");
}
