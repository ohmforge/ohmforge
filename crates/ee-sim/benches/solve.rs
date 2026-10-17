//! Criterion benches for the simulator's hot paths: MNA stamp + solve, driven
//! through the public [`Engine`] API (the `System` stamp/solve is `pub(crate)`,
//! so we exercise it the way every front end does — by running an analysis).
//!
//! Groups:
//! - `op_point` — DC operating point of a resistor ladder (stamp + one solve).
//! - `transient` — a Backward-Euler RC low-pass and a series RLC (stamp + solve
//!   per timestep, fixed modest step counts so timings stay stable).
//! - `op_point_scaling` — op-point solve of an N-rung ladder for a few N, so the
//!   dense stamp+solve cost curve (≈ O(N³)) is visible.
//! - `sparse_vs_dense` — the `CP-SIM-M2.4` gate: the dense `O(N³)` solve raced
//!   head-to-head against the sparse path on the identical ladder matrix at
//!   N ≈ 250…1000, where the sparse path's near-linear factorization pulls
//!   markedly ahead.
//! - `sparse_scaling` — the sparse path alone over N = 1000…4000, to show it
//!   keeps scaling where the dense cube is already impractical.
//!
//! The head-to-head uses [`ee_sim::bench_support`], a hidden hook that forces
//! each solver on one assembled matrix (the engine itself auto-picks sparse
//! above its threshold, so there is otherwise no way to time dense at N ≈ 1000).
//!
//! Run a quick pass with, e.g.:
//! `cargo bench -p ee-sim -- --warm-up-time 0.2 --measurement-time 1.0 --sample-size 20`

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use ee_core::graph::{Circuit, NetId, PartKind};
use ee_core::{Unit, Value};
use ee_sim::{bench_support, Analysis, Engine};

/// Parse an engineering-notation value, panicking on a bad literal (bench setup).
fn val(s: &str, unit: Unit) -> Option<Value> {
    Some(Value::parse(s, unit).unwrap())
}

/// An `n`-rung resistor ladder driven by a 10 V source: a series 1 kΩ into each
/// rung node with a 1 kΩ shunt to ground. Yields `n + 1` nodes plus one source
/// branch — a representative, always-solvable DC network.
fn resistor_ladder(n: usize) -> Circuit {
    let mut c = Circuit::new("ladder");
    let vin = c.new_net();
    c.add_part(
        "V1",
        PartKind::VoltageSource,
        val("10", Unit::Volt),
        vec![vin, NetId::GROUND],
    );
    let mut prev = vin;
    for i in 0..n {
        let node = c.new_net();
        c.add_part(
            format!("Rs{i}"),
            PartKind::Resistor,
            val("1k", Unit::Ohm),
            vec![prev, node],
        );
        c.add_part(
            format!("Rp{i}"),
            PartKind::Resistor,
            val("1k", Unit::Ohm),
            vec![node, NetId::GROUND],
        );
        prev = node;
    }
    c
}

/// 5 V step into a 1 kΩ / 1 µF low-pass (RC = 1 ms).
fn rc_circuit() -> Circuit {
    let mut c = Circuit::new("rc");
    let vin = c.new_net();
    let out = c.new_net();
    c.add_part(
        "V1",
        PartKind::VoltageSource,
        val("5", Unit::Volt),
        vec![vin, NetId::GROUND],
    );
    c.add_part(
        "R1",
        PartKind::Resistor,
        val("1k", Unit::Ohm),
        vec![vin, out],
    );
    c.add_part(
        "C1",
        PartKind::Capacitor,
        val("1u", Unit::Farad),
        vec![out, NetId::GROUND],
    );
    c
}

/// Underdamped series RLC step: V1 — R(10 Ω) — L(1 mH) — C(1 µF) — gnd.
fn rlc_circuit() -> Circuit {
    let mut c = Circuit::new("rlc");
    let vin = c.new_net();
    let a = c.new_net();
    let out = c.new_net();
    c.add_part(
        "V1",
        PartKind::VoltageSource,
        val("5", Unit::Volt),
        vec![vin, NetId::GROUND],
    );
    c.add_part("R1", PartKind::Resistor, val("10", Unit::Ohm), vec![vin, a]);
    c.add_part(
        "L1",
        PartKind::Inductor,
        val("1m", Unit::Henry),
        vec![a, out],
    );
    c.add_part(
        "C1",
        PartKind::Capacitor,
        val("1u", Unit::Farad),
        vec![out, NetId::GROUND],
    );
    c
}

/// DC operating point of a modest resistor ladder (stamp + a single solve).
fn bench_op_point(c: &mut Criterion) {
    let engine = Engine::new();
    let circuit = resistor_ladder(16);
    let analysis = Analysis::OperatingPoint;
    c.bench_function("op_point/ladder_16", |b| {
        b.iter(|| {
            engine
                .run(black_box(&circuit), black_box(&analysis))
                .unwrap()
        })
    });
}

/// Transient runs: a 1000-step RC low-pass and a 500-step series RLC, each a
/// stamp + solve per timestep.
fn bench_transient(c: &mut Criterion) {
    let engine = Engine::new();

    let rc = rc_circuit();
    let rc_analysis = Analysis::Transient {
        duration: 1e-3,
        step: Some(1e-6), // 1000 steps
    };
    c.bench_function("transient/rc_1000", |b| {
        b.iter(|| engine.run(black_box(&rc), black_box(&rc_analysis)).unwrap())
    });

    let rlc = rlc_circuit();
    let rlc_analysis = Analysis::Transient {
        duration: 500e-6,
        step: Some(1e-6), // 500 steps
    };
    c.bench_function("transient/rlc_500", |b| {
        b.iter(|| {
            engine
                .run(black_box(&rlc), black_box(&rlc_analysis))
                .unwrap()
        })
    });
}

/// Scaling: op-point of an N-rung ladder for a few N, so the stamp+solve cost
/// curve is visible.
fn bench_op_point_scaling(c: &mut Criterion) {
    let engine = Engine::new();
    let analysis = Analysis::OperatingPoint;
    let mut group = c.benchmark_group("op_point_scaling");
    for n in [8usize, 32, 128] {
        let circuit = resistor_ladder(n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &circuit, |b, circuit| {
            b.iter(|| {
                engine
                    .run(black_box(circuit), black_box(&analysis))
                    .unwrap()
            })
        });
    }
    group.finish();
}

/// The `CP-SIM-M2.4` gate: dense vs sparse on the *same* ladder matrix. For each
/// N the dense `O(N³)` elimination and the near-linear sparse factorization solve
/// the identical system, so the ratio of their timings is the scaling win.
fn bench_sparse_vs_dense(c: &mut Criterion) {
    let mut group = c.benchmark_group("sparse_vs_dense");
    // Each solve rebuilds and factors from the circuit, so timings are per solve.
    for n in [250usize, 500, 1000] {
        let circuit = resistor_ladder(n);
        group.bench_with_input(BenchmarkId::new("dense", n), &circuit, |b, circuit| {
            b.iter(|| bench_support::dc_solve_dense(black_box(circuit)).unwrap())
        });
        group.bench_with_input(BenchmarkId::new("sparse", n), &circuit, |b, circuit| {
            b.iter(|| bench_support::dc_solve_sparse(black_box(circuit)).unwrap())
        });
    }
    group.finish();
}

/// The sparse path alone, out to N = 4000 — where a dense solve (≈ 6·10¹⁰ flops)
/// would be impractical — so its own scaling curve is visible end to end.
fn bench_sparse_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("sparse_scaling");
    for n in [1000usize, 2000, 4000] {
        let circuit = resistor_ladder(n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &circuit, |b, circuit| {
            b.iter(|| bench_support::dc_solve_sparse(black_box(circuit)).unwrap())
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_op_point,
    bench_transient,
    bench_op_point_scaling,
    bench_sparse_vs_dense,
    bench_sparse_scaling
);
criterion_main!(benches);
