# Worked analog examples

Each example is a circuit with a short writeup of how it works (the physics) and
the result the engine produces. The engine is `ohmforge-sim`; the full walk-through of
how it solves these is in [`docs/guide/simulation.md`](../../docs/guide/simulation.md).

## Runnable from the shell

These are `.eesh` scripts the interactive shell accepts on **stdin**. The `ohmforge`
package ships two binaries (`eesh`, the shell, and `of`, the CLI), so name the
shell explicitly:

```sh
cat examples/analog/<name>.eesh | cargo run -p ohmforge --bin eesh
```

The shell's `add` places two-terminal parts (`R C L V I`, and `D` diodes) **and**
the multi-terminal devices the engine models: `Q` (BJT, `collector base emitter
[npn|pnp]`), `M` (MOSFET), and the controlled sources `E`/`G` (VCVS/VCCS) and
`H`/`F` (CCVS/CCCS). So every operating-point circuit below — including the
nonlinear and multi-terminal ones — runs directly as a script:

| Example | Analysis | What it shows |
|---------|----------|---------------|
| [`divider`](divider.md) | `run op` | linear MNA solve — a 10 V → 5 V resistive divider |
| [`rc_step`](rc_step.md) | `run tran` | first-order RC charging, `v = 5(1 − e^(−t/RC))` |
| [`rl_step`](rl_step.md) | `run tran` | first-order RL, inductor voltage decaying as current ramps |
| [`rlc_resonator`](rlc_resonator.md) | `run tran` | underdamped RLC step — a ringing, overshooting response |
| [`diode`](diode.md) | `run op` | the nonlinear Newton–Raphson path — a silicon junction drop |
| [`diode_clamp`](diode_clamp.md) | `run op` | a diode clamp pinning a node to a rail + one junction drop |
| [`common_emitter_bjt`](common_emitter_bjt.md) | `run op` | NPN voltage-divider bias in the forward-active region (`Q1`) |
| [`opamp_noninverting`](opamp_noninverting.md) | `run op` | ideal op-amp (VCVS `EOP`) closing a ×10 non-inverting loop |

## Engine-API example

| Example | Needs | Verified by |
|---------|-------|-------------|
| [`half_wave_rectifier`](half_wave_rectifier.md) | a sine *transient* | `ohmforge-sim` regression test |

The shell now *attaches* a sine with `set <ref> sine <amp> <freq>` and samples it
with a **fixed-step** transient (`run tran <dur> <step>`), so a time-varying
stimulus renders instead of reading flat. The half-wave rectifier's clean,
numerically-verified waveform (peak ≈ 4.37 V, troughs blocked to ≈ 0 V) is still
presented as an **engine-API** writeup, backed by an `ohmforge-sim` regression
test and the browser demo's `simulate(netlist, duration, step)` — the canonical
reference values come from there.

> `.eesh` files here are kept as pure command lines so they are easy to read as
> transcripts; when piped, the shell *does* accept `#` comments, `set`/`$vars`,
> and `repeat` loops (the prose for each example lives in the paired `.md`).
