# Worked analog examples

Each example is a circuit with a short writeup of how it works (the physics) and
the result the engine produces. The engine is `ee-sim`; the full walk-through of
how it solves these is in [`docs/guide/simulation.md`](../../docs/guide/simulation.md).

## Runnable from the shell

These are `.eesh` scripts the interactive shell accepts on **stdin**. Run one with:

```sh
cat examples/analog/<name>.eesh | cargo run -p ee
```

| Example | Analysis | What it shows |
|---------|----------|---------------|
| [`divider`](divider.md) | `run op` | linear MNA solve — a 10 V → 5 V resistive divider |
| [`rc_step`](rc_step.md) | `run tran` | first-order RC charging, `v = 5(1 − e^(−t/RC))` |
| [`rl_step`](rl_step.md) | `run tran` | first-order RL, inductor voltage decaying as current ramps |
| [`rlc_resonator`](rlc_resonator.md) | `run tran` | underdamped RLC step — a ringing, overshooting response |
| [`diode`](diode.md) | `run op` | the nonlinear Newton–Raphson path — a silicon junction drop |

## Engine-API examples (beyond the current shell grammar)

The stdin shell's `add <ref> <a> <b> <value>` places only **2-terminal, DC** parts
(R, C, L, V, I, D). These three circuits need capabilities the engine has but that
grammar cannot yet express — a time-varying source, a 4-terminal controlled source,
or a 3-terminal transistor — so each writeup drives the circuit through the engine
API (or a built-in `cargo run -p ee <demo>`) and is backed by an `ee-sim`
regression test:

| Example | Needs | Verified by |
|---------|-------|-------------|
| [`half_wave_rectifier`](half_wave_rectifier.md) | sine source ([`Waveform::Sine`]) | engine-API snippet |
| [`opamp_noninverting`](opamp_noninverting.md) | VCVS (4-terminal) | `ideal_opamp_noninverting_gain` test |
| [`common_emitter_bjt`](common_emitter_bjt.md) | BJT (3-terminal) | `cargo run -p ee amp`, `bjt_common_emitter_bias` test |

> Extending the shell grammar to place sines and multi-terminal devices (so these
> three become `.eesh` scripts too) lives in `ee-shell`, outside this engine crate.
> The shell also has no comment syntax today, so `.eesh` files here are kept as pure
> command lines with the prose in the paired `.md`.
