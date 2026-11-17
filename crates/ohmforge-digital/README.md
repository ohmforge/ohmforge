# ohmforge-digital — event-driven digital kernel + HDL-lite

The digital logic engine for [ohmforge](https://github.com/ohmforge/ohmforge).
Signals change only at scheduled events, and gates schedule future events after
their propagation delay — far cheaper than analog MNA for pure logic, and the
mechanism that lets fast MCU pins interact with slow analog subcircuits across a
mixed-signal boundary.

The kernel has three pieces:

- **`Logic`** — four-valued logic (`0`/`1`/`X`/`Z`) with `X`-propagating truth
  tables and multi-driver resolution.
- **`EventQueue`** — a deterministic min-heap of timed events keyed in integer
  femtoseconds (`Femtos`), so there are no float-equality ordering hazards.
- **`Sim`** — the netlist (nets, drivers, gates, flip-flops, ROM/RAM, HDL-lite
  truth tables and expressions) and the run loop that drains due events,
  re-resolves nets, and re-evaluates driven gates.

Like the rest of the compute core it is pure compute — no clock, no randomness,
no I/O — and builds for `wasm32-unknown-unknown`.

## Usage

```rust
use ohmforge_digital::{Sim, GateKind, Logic};

// Time is an integer femtosecond tick (`Femtos` is a `u64` alias); 1 ns = 1e6.
let mut sim = Sim::new();
let a = sim.add_net();
let b = sim.add_net();
let y = sim.add_net();

// Drive the two inputs, then AND them with a 1 ns gate delay.
let _da = sim.add_input(a, Logic::High);
let db = sim.add_input(b, Logic::Low);
sim.add_gate(GateKind::And, &[a, b], y, 1_000_000, Logic::X);

// Settle, then flip b high at t = 5 ns and settle again.
sim.run_until(2_000_000);
assert_eq!(sim.value(y), Logic::Low);

sim.set_input(db, Logic::High, 5_000_000);
sim.run_until(10_000_000);
assert_eq!(sim.value(y), Logic::High);
```

---

Part of [**ohmforge**](https://github.com/ohmforge/ohmforge) — the AI-native
circuit & firmware workbench.

License: **Apache-2.0**.
