# RC low-pass step response (transient)

```
 in ──[ R1 1k ]── out ──┬── gnd
  │                   [ C1 1u ]
 V1 5 V                   │
  │                      gnd
 gnd
```

Run it:

```sh
cat examples/analog/rc_step.eesh | cargo run -p ee
```

## The circuit

A 5 V source steps on at `t = 0` and charges a 1 µF capacitor through a 1 kΩ
resistor. `run tran 5m` runs a 5 ms time-domain **transient**
([`Analysis::Transient`]); with no explicit step the shell lets the engine pick
an adaptive, error-controlled timestep. The scope plots the probed `out` node.

## The physics

The capacitor voltage obeys a first-order exponential with time constant

```
τ = R·C = 1k · 1µ = 1 ms
v(out, t) = V1 · (1 − e^(−t/τ)) = 5·(1 − e^(−t/1ms))
```

The engine discretizes the capacitor with a **Backward-Euler companion model**: a
conductance `C/h` in parallel with a history current source `(C/h)·v_prev`,
refreshed each step. At `t = 0` the capacitor is held at its initial 0 V, so the
curve starts at the origin and relaxes to the 5 V rail.

## Expected result (verified)

The waveform rises from 0 V and is within ~1 % of the rail by 5 ms (5 τ):

| t      | 1τ (1 ms) | 2τ    | 3τ    | 5τ (5 ms) |
|--------|-----------|-------|-------|-----------|
| v(out) | 3.16 V    | 4.32 V| 4.75 V| 4.96 V    |

The rendered scope tops out at **4.963 V** at `t = 5 ms`, matching
`5·(1 − e^(−5)) = 4.966 V` to within the plot's resolution. The adaptive grid
takes fine steps on the fast initial rise and coarsens across the placid tail.
