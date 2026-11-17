# ohmforge-core — circuit graph and core types

The shared data model for [ohmforge](https://github.com/ohmforge/ohmforge): the
circuit graph, physical units and quantities, simulation time, and the
unit-carrying `Signal` result type. It is **pure data and logic** — no I/O, no
UI — and it is **dependency-free, `wasm32`-clean, and deterministic**. Together
with `ohmforge-sim` it forms the compute core every other crate (TUI, shell,
web, AI) builds on, so the exact same model runs in your terminal, headless, and
in the browser.

Everything here is building blocks: a `Circuit` of `Part`s wired across `NetId`
nets (net 0 is always ground), `Value`s with engineering-notation parsing, a
`Unit` enum, and `Signal`s produced by the simulator.

## Usage

```rust
use ohmforge_core::graph::{Circuit, NetId, PartKind};
use ohmforge_core::value::Value;
use ohmforge_core::units::Unit;

// A 5 V source feeding a 10k / 10k divider.
let mut c = Circuit::new("divider");
let vin = c.new_net();
let mid = c.new_net();

c.add_part("V1", PartKind::VoltageSource, Value::parse("5", Unit::Volt).ok(), vec![vin, NetId::GROUND]);
c.add_part("R1", PartKind::Resistor, Value::parse("10k", Unit::Ohm).ok(), vec![vin, mid]);
c.add_part("R2", PartKind::Resistor, Value::parse("10k", Unit::Ohm).ok(), vec![mid, NetId::GROUND]);

// Engineering notation parses as you'd write it on a schematic.
assert_eq!(Value::parse("4k7", Unit::Ohm).unwrap().magnitude, 4700.0);

assert_eq!(c.parts().len(), 3);
```

Feed the `Circuit` to [`ohmforge-sim`](https://crates.io/crates/ohmforge-sim) to
solve it.

---

Part of [**ohmforge**](https://github.com/ohmforge/ohmforge) — the AI-native
circuit & firmware workbench.

License: **MIT OR Apache-2.0**.
