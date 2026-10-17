# Series RLC resonator — underdamped step (transient)

```
 in ──[ R1 10 ]── a ──[ L1 1mH ]── out ──┬── gnd
  │                                    [ C1 1u ]
 V1 5 V                                    │
  │                                       gnd
 gnd
```

Run it:

```sh
cat examples/analog/rlc_resonator.eesh | cargo run -p ee
```

## The circuit

A 5 V step drives a series R-L-C. The output is taken across the capacitor.
`run tran 300u` runs a 300 µs **transient** ([`Analysis::Transient`]); the small
10 Ω series resistance makes the circuit **underdamped**, so the capacitor voltage
rings before settling.

## The physics

A series RLC step response is a damped sinusoid governed by

```
ω0 = 1/√(L·C)          = 1/√(1mH·1µF)   ≈ 31.6 krad/s   (undamped natural freq.)
α  = R/(2L)            = 10/(2·1mH)     = 5.0 krad/s     (damping factor)
ω_d = √(ω0² − α²)                        ≈ 31.2 krad/s   (ringing freq.)
```

Because `α < ω0` the roots are complex: the cap voltage overshoots its 5 V final
value, with the **first peak** at `t_p = π/ω_d ≈ 100.6 µs` and a height

```
v_peak = V1·(1 + e^(−α·t_p)) = 5·(1 + e^(−0.503)) ≈ 8.02 V.
```

Capturing a clean ring is a real test of the transient integrator: the capacitor
and inductor companion models must exchange energy step after step without the
Backward-Euler damping smearing the oscillation.

## Expected result (verified)

The scope shows the cap voltage overshooting to a first peak of **7.982 V** at
roughly 100 µs (theory: 8.02 V at 100.6 µs; Backward Euler adds a little
numerical damping, which trims the peak slightly), then ringing down toward the
5 V DC level. If the ring looks absent, the circuit is over-damped — increase `L`
or decrease `R`.
