# ohmforge-devices — composite device subcircuits & board definitions

Higher-level device models for [ohmforge](https://github.com/ohmforge/ohmforge)
built from the primitives in `ohmforge-core`: op-amps, relays, analog switches,
and other composite parts that **stamp** themselves into a `Circuit` as the
internal network the solver actually sees. It also carries **board definitions**
(Arduino Uno/Nano/Mega, Raspberry Pi Pico, ESP32-class, and user-defined custom
boards) and a searchable parts library, plus drop-in modules (LED, 7-segment,
servo, WS2812).

A board is a manifest: MCU, pinout, and onboard parts. Custom boards drop in by
writing a small text manifest parsed by `BoardDef::from_manifest`. This crate
sits above the compute core and feeds the engines ordinary `Circuit`s — no
solver state of its own.

## Usage

```rust
use ohmforge_core::graph::{Circuit, NetId};
use ohmforge_devices::subcircuits::OpAmp;
use ohmforge_devices::BoardDef;

// Stamp a generic high-gain op-amp into a circuit.
let mut c = Circuit::new("amp");
let in_p = c.new_net();
let in_n = c.new_net();
let out = c.new_net();

OpAmp::default().stamp(&mut c, "U1", in_p, in_n, out);
// The op-amp expands into a VCVS plus its output impedance network.
assert!(c.part_by_ref("U1").is_some());

// Catalog boards are available as manifests.
let uno = BoardDef::uno();
assert_eq!(uno.mcu, "atmega328p");
```

---

Part of [**ohmforge**](https://github.com/ohmforge/ohmforge) — the AI-native
circuit & firmware workbench.

License: **MIT OR Apache-2.0**.
