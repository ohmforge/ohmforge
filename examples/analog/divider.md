# Resistive voltage divider (DC operating point)

```
 in ──[ R1 1k ]── out ──[ R2 1k ]── gnd
  │
 V1 10 V
  │
 gnd
```

Run it:

```sh
cat examples/analog/divider.eesh | cargo run -p ohmforge --bin eesh
```

## The circuit

A 10 V source drives two equal 1 kΩ resistors in series. `run op` solves the DC
**operating point** — the steady node voltages with no time dependence
([`Analysis::OperatingPoint`]).

## The physics

Two resistors in series carry the same current `I = V1 / (R1 + R2)`. The output
node sits at the drop across `R2`:

```
v(out) = V1 · R2 / (R1 + R2) = 10 · 1k / 2k = 5 V
```

This is the canonical linear MNA solve: each resistor stamps a conductance
`1/R` between its nodes, the source adds one branch-current row, and a single
Gaussian elimination returns the answer exactly — no Newton iteration, because
there is nothing nonlinear.

## Expected result (verified)

```
operating point:
  v(in) = 10.000000 V
  v(out) = 5.000000 V
```

Change `R2` to `3k` and `v(out)` becomes `10 · 3k/4k = 7.5 V`; swap in a `0`-value
resistor and the engine rejects it as a bad value (a zero-ohm resistor is a short
with undefined current division).
