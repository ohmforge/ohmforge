# Voltage divider + diode clamp (DC operating point)

```
 in ──[ R1 1k ]── out ──▶|── clamp
  │                      D1      │
 V1 5 V              (anode=out) V2 3 V
  │                              │
 gnd                           gnd
```

Run it:

```sh
cat examples/analog/diode_clamp.eesh | cargo run -p ohmforge --bin eesh
```

## The circuit

A 5 V source feeds `out` through a 1 kΩ series resistor. A diode `D1` ties `out`
to a 3 V clamp rail (`V2`), anode on `out`. This is the nonlinear **Newton–Raphson**
path again, but used as a *protection clamp* — the textbook way to pin a node below
a rail.

## The physics

Left alone, `out` would float up toward 5 V. But as `out` climbs past roughly
`V2 + 0.6 V`, the diode turns on and shunts the excess current into the clamp rail,
holding `out` just above `V2`:

```
v(out) ≈ V2 + V_diode ≈ 3 + 0.66 ≈ 3.66 V
```

The exact drop is set by the Shockley law at the clamp current
`(5 − v(out)) / R1 ≈ 1.34 mA`, which the engine solves self-consistently (KCL at
`out`) rather than assuming a fixed 0.7 V. Swap `V2` for ground and you have a
simple positive clamp to 0.6 V; raise it and the ceiling rises with it.

## Expected result (verified — `cat … | cargo run -p ohmforge --bin eesh`)

```
operating point:
  v(clamp) = 3.000000 V
  v(in) = 5.000000 V
  v(out) = 3.662312 V
```

`out` is clamped to 3.66 V — the 3 V rail plus one silicon junction drop — instead
of the 5 V it would otherwise reach. That 0.66 V over the rail is the signature of
a conducting diode carrying ~1.3 mA.
