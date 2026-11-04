# Half-wave diode rectifier (transient)

```
 in ──▶|── out ──[ R1 10k ]── gnd
  │     D1
 V1  5 V, 1 kHz sine
  │
 gnd
```

> **Shell limitation.** This circuit needs a *time-varying (sine) source*. The
> engine models it ([`ee_core::Waveform::Sine`], driven in transient by
> [`Analysis::Transient`]), but the current stdin shell grammar can only `add` DC
> sources, so this example is driven through the **engine API** rather than an
> `.eesh` script. See the note at the bottom of [`diode.md`](diode.md).

## The circuit

A 5 V, 1 kHz sine feeds a diode into a 10 kΩ load. The diode conducts only when
its anode is above its cathode, so the load sees the positive half-cycles and
nothing of the negative ones — half-wave rectification.

## The physics

On the positive half-cycle the diode is forward biased and drops ~0.6 V, so

```
v(out) ≈ v(in) − 0.6 V   while v(in) > ~0.6 V,  peaking near 5 − 0.6 = 4.4 V
```

On the negative half-cycle the diode is reverse biased (an open), the load pulls
`out` to 0 V, and `v(out) ≈ 0`. This is a genuinely nonlinear **transient**: at
every timestep the engine re-linearizes the diode about the running node voltages
inside its Newton loop, so the conduction edge is resolved sample by sample.

## Build it (engine API)

```rust
use ee_core::graph::{Circuit, NetId, PartKind, Waveform};
use ee_core::{Unit, Value};
use ee_sim::{Analysis, Engine};

let mut c = Circuit::new("rectifier");
let vin = c.new_net();
let out = c.new_net();
c.add_part("V1", PartKind::VoltageSource, None, vec![vin, NetId::GROUND]);
c.set_waveform("V1", Waveform::Sine { amplitude: 5.0, freq: 1_000.0, offset: 0.0, phase_deg: 0.0 });
c.add_part("D1", PartKind::Diode(DiodeModel::default()), None, vec![vin, out]); // anode=in, cathode=out
c.add_part("R1", PartKind::Resistor, Some(Value::new(10_000.0, Unit::Ohm)), vec![out, NetId::GROUND]);
let sigs = Engine::new()
    .run(&c, &Analysis::Transient { duration: 3e-3, step: Some(2e-6) })
    .unwrap();
```

## Expected result (verified)

Over 3 ms (three cycles) the `out` node is a train of positive humps:

- **peak ≈ 4.37 V** (the 5 V sine crest minus the ~0.63 V junction drop at that
  current), and
- **minimum ≈ 0 V** — the negative half-cycles are blocked.

Adding a reservoir capacitor in parallel with `R1` (e.g. `C1 = 10 µF`) smooths the
humps into a near-DC level that charges up over many cycles — the classic
rectifier-plus-filter, the front end of a linear power supply.
