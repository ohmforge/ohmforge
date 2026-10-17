# Diode + resistor — the nonlinear junction drop (DC operating point)

```
 in ──[ R1 1k ]── a ──▶|── gnd
  │                   D1
 V1 5 V          (anode = a, cathode = gnd)
  │
 gnd
```

Run it:

```sh
cat examples/analog/diode.eesh | cargo run -p ee
```

> The shell's `add D1 a gnd 1e-14` places a diode whose value is its **saturation
> current** `I_s` (amperes). Terminals are `[anode, cathode]`, so current flows
> `a → gnd` when forward biased.

## The circuit

A 5 V source pushes current through a 1 kΩ series resistor into a diode to
ground. This is the smallest circuit that forces the engine onto its **nonlinear
Newton–Raphson** path ([`Analysis::OperatingPoint`] with a nonlinear device).

## The physics

The diode follows the Shockley law `I = I_s·(e^(V/V_t) − 1)` with
`V_t ≈ 25.85 mV` at 300 K. The resistor current and the diode current must match
(KCL at node `a`):

```
(V1 − v(a)) / R = I_s·(e^(v(a)/V_t) − 1)
```

This is transcendental — no closed form — so the engine linearizes the diode each
iteration (a conductance `g = dI/dV` in parallel with a Norton equivalent
current) and iterates to a consistent solution, with SPICE-style junction
limiting keeping the exponential from overflowing between steps. The result lands
in the familiar silicon window of ~0.6–0.7 V.

## Expected result (verified)

```
operating point:
  v(a) = 0.692544 V
  v(in) = 5.000000 V
```

Check it: the resistor current is `(5 − 0.6925)/1k = 4.31 mA`, and the inverse
Shockley law predicts `v(a) = V_t·ln(I/I_s + 1) = 25.85m·ln(4.31m/1e-14) ≈
0.693 V`. The two agree, which is exactly the KCL balance the Newton solve drove
to.

## Note — a true half-wave rectifier

Rectifying an AC waveform needs a **time-varying (sine) source**, which the
engine supports ([`ee_core::Waveform::Sine`]) but the current stdin shell grammar
cannot yet express (`add` only places DC sources). See
[`half_wave_rectifier.md`](half_wave_rectifier.md) for that circuit, driven
through the engine API directly.
