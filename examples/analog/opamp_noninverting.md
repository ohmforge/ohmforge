# Ideal op-amp — non-inverting amplifier (DC operating point)

```
            ┌────────────────┐
  vin ──────┤+               │
            │   EOP  A=1e6   ├──── vout ──┬─────────────── (output)
   ┌────────┤−               │         [ Rf 9k ]
   │        └────────────────┘            │
   │                                     vfb
   │                                      │
   └──────────────────────────────────[ Rg 1k ]
                                          │
                                         gnd
  V+ = vin,  V− = vfb,  vout = A·(V+ − V−)
```

> **Runnable from the shell.** An op-amp is modeled here as a **VCVS** (voltage-
> controlled voltage source), a 4-terminal controlled source
> ([`ohmforge_core::graph::PartKind::Vcvs`]), placed with
> `add EOP <out+> <out-> <ctrl+> <ctrl-> <gain>`. So this example runs directly as a
> script:
>
> ```sh
> cat examples/analog/opamp_noninverting.eesh | cargo run -p ohmforge --bin eesh
> ```
>
> The VCVS stamp is also exercised by the `ideal_opamp_noninverting_gain`
> regression test in `ohmforge-sim`.

## The circuit

The op-amp is an ideal VCVS with a huge open-loop gain `A = 1e6`:
`vout = A·(V+ − V−)`. Feedback resistors `Rf` (output → inverting input) and `Rg`
(inverting input → ground) close the loop in the **non-inverting** configuration.

## The physics

With large `A`, negative feedback forces `V− ≈ V+ = vin`. The feedback network is
a divider from `vout` to ground tapped at `vfb = V−`, so

```
vfb = vout · Rg / (Rf + Rg) = V+ = vin
⟹  vout = vin · (1 + Rf/Rg) = vin · (1 + 9k/1k) = 10 · vin
```

The closed-loop gain `1 + Rf/Rg = 10` is set by the resistor ratio alone; the
finite open-loop gain leaves only a `~1/(A·β)` residual, far below a millivolt
here. This is the engine's behavioral-modeling payoff — a controlled source
turns into an ideal gain block with no transistor-level detail.

## Build it (engine API)

```rust
use ohmforge_core::graph::{Circuit, NetId, PartKind};
use ohmforge_core::{Unit, Value};
use ohmforge_sim::{Analysis, Engine};

let mut c = Circuit::new("opamp");
let vin = c.new_net();   // V+
let vfb = c.new_net();   // V−  (feedback node)
let vout = c.new_net();
c.add_part("Vin", PartKind::VoltageSource, Some(Value::new(1.0, Unit::Volt)), vec![vin, NetId::GROUND]);
// VCVS terminals: [out+, out−, ctrl+, ctrl−]
c.add_part("EOP", PartKind::Vcvs(1e6), None, vec![vout, NetId::GROUND, vin, vfb]);
c.add_part("Rf", PartKind::Resistor, Some(Value::new(9_000.0, Unit::Ohm)), vec![vout, vfb]);
c.add_part("Rg", PartKind::Resistor, Some(Value::new(1_000.0, Unit::Ohm)), vec![vfb, NetId::GROUND]);
let sigs = Engine::new().run(&c, &Analysis::OperatingPoint).unwrap();
```

## Expected result (verified)

With `vin = 1 V`, the output settles to **v(vout) = 9.99990 V**, i.e. the ideal
closed-loop gain of 10 to within the `1/(A·β)` residual (`A·β = 1e6·0.1 = 1e5`, so
the error is ~1e−4). Raise `A` toward infinity and it converges on exactly 10.
