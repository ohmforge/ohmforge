# ohmforge-sim — SPICE-class analog / mixed-signal engine

The simulation heart of [ohmforge](https://github.com/ohmforge/ohmforge). It
assembles a Modified Nodal Analysis system from an `ohmforge_core::Circuit`,
solves it, and returns typed `Signal`s. Alongside `ohmforge-core` it is the
**dependency-free, `wasm32`-clean, deterministic** compute core — it takes no
clock, no filesystem, and no unseeded randomness, so a run in the browser
matches a run in the terminal bit-for-bit.

Capabilities include DC operating point, time-domain transient, small-signal AC
(Bode), DC and parametric sweeps, noise and seeded Monte-Carlo tolerance
analysis, with a Newton-Raphson solver (gmin/source stepping convergence aids)
for nonlinear devices — diodes, BJTs, MOSFETs, controlled sources — plus
transformers (coupled inductors), transmission lines, and a pole-zero / transfer
function path. An optional `parallel` feature (native only, no extra deps) runs
independent analyses across OS threads.

## Usage

```rust
use ohmforge_core::graph::{Circuit, NetId, PartKind};
use ohmforge_core::value::Value;
use ohmforge_core::units::Unit;
use ohmforge_sim::{Engine, Analysis};

// 5 V across a 10k / 10k divider.
let mut c = Circuit::new("divider");
let vin = c.new_net();
let mid = c.new_net();
c.add_part("V1", PartKind::VoltageSource, Value::parse("5", Unit::Volt).ok(), vec![vin, NetId::GROUND]);
c.add_part("R1", PartKind::Resistor, Value::parse("10k", Unit::Ohm).ok(), vec![vin, mid]);
c.add_part("R2", PartKind::Resistor, Value::parse("10k", Unit::Ohm).ok(), vec![mid, NetId::GROUND]);

// Solve the DC operating point: one Signal per node.
let signals = Engine::new()
    .run(&c, &Analysis::OperatingPoint)
    .expect("circuit solves");

for s in &signals {
    println!("{} = {:?} {:?}", s.name, s.y, s.unit);
}
```

Other analyses are just other `Analysis` variants, e.g.
`Analysis::Transient { duration: 1e-3, step: None }` or
`Analysis::Ac { f_start: 10.0, f_stop: 1e6, points_per_decade: 20 }`.

---

Part of [**ohmforge**](https://github.com/ohmforge/ohmforge) — the AI-native
circuit & firmware workbench.

License: **Apache-2.0**.
