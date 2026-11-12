# RL step response (transient)

```
 in ──[ R1 100 ]── mid ──[ L1 0.1H ]── gnd
  │
 V1 5 V
  │
 gnd
```

Run it:

```sh
cat examples/analog/rl_step.eesh | cargo run -p ohmforge
```

## The circuit

A 5 V source drives 100 Ω in series with a 0.1 H inductor to ground.
`run tran 5m` runs a 5 ms **transient** ([`Analysis::Transient`]) and plots the
`mid` node — the junction between the resistor and the inductor.

## The physics

The series current rises exponentially toward its resistive limit with time
constant

```
τ = L / R = 0.1 / 100 = 1 ms
i(t) = (V1/R)·(1 − e^(−t/τ)),   i(∞) = 5/100 = 50 mA
```

The probed node voltage is the inductor voltage `v(mid) = V1 − i·R = V1·e^(−t/τ)`,
so it **decays** from the full 5 V (all the source voltage is across the inductor
at `t = 0`, when `i = 0` and `di/dt` is largest) toward 0 V (the inductor becomes
a plain wire once the current stops changing).

The engine discretizes the inductor by Backward Euler as a conductance
`g = h/L` in parallel with a history current source carrying the previous
step's inductor current; at `t = 0` the inductor starts de-energized (`i = 0`,
an open circuit).

## Expected result (verified)

`v(mid)` falls from **5 V** at `t = 0` to **0.034 V** at `t = 5 ms` (≈ 5 τ, where
`5·e^(−5) = 0.034 V`). Equivalently the series current climbs from 0 to
`(5 − 0.034)/100 = 49.7 mA`, within 1 % of the 50 mA limit.
