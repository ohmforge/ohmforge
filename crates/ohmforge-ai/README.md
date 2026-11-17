# ohmforge-ai — the copilot

The AI copilot for [ohmforge](https://github.com/ohmforge/ohmforge). The model
works through **tools** that map onto the same circuit API the shell uses (add
parts, wire, set params, run analyses, read signals) and **verifies its own
proposals by re-running the simulator** before presenting them. Nothing is
applied to the live circuit without user approval.

It is **offline-first and needs no API key**: deterministic design-rule checks
(DRC), electronics calculators, a local parts index, and an interactive tutor
all run with no network. A live model is reached only through a synchronous
`Transport` trait — a `MockTransport` for tests, and an opt-in `HttpTransport`
(behind the `http` feature) for a live round-trip on native. The crate compiles
for the browser with no TLS stack in the default build.

## Usage

Offline DRC — no key, no network:

```rust
use ohmforge_core::graph::{Circuit, NetId, PartKind};
use ohmforge_core::value::Value;
use ohmforge_core::units::Unit;
use ohmforge_ai::{drc_check, Severity};

// A 5 V source directly shorting an LED (no series resistor).
let mut c = Circuit::new("led");
let n = c.new_net();
c.add_part("V1", PartKind::VoltageSource, Value::parse("5", Unit::Volt).ok(), vec![n, NetId::GROUND]);
c.add_part("D1", PartKind::Diode(Default::default()), None, vec![n, NetId::GROUND]);

for f in drc_check(&c) {
    println!("[{}] {}", f.severity.tag(), f.message);
    assert!(matches!(f.severity, Severity::Error | Severity::Warning));
}
```

To enable the live transport, build with `--features http` and supply the API
key through the environment (`api_key_from_env`); it is never committed.

---

Part of [**ohmforge**](https://github.com/ohmforge/ohmforge) — the AI-native
circuit & firmware workbench.

License: **Apache-2.0**.
