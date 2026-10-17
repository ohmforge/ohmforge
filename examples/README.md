# Examples

Worked, **run-verified** examples. Each `.eesh` is a script the shell accepts on stdin; each has a companion `.md` (or a section) explaining the circuit, the physics, and the expected result.

## Running them

```sh
# analog circuits + tutorials: pipe the script into the shell
cat examples/analog/rc_step.eesh | cargo run -p ee
cat examples/tutorial/sweep.eesh | cargo run -p ee      # loops/$vars via the script engine

# MCU / co-simulation: run as ee-mcu examples
cargo run -p ee-mcu --example uno_blink
cargo run -p ee-mcu --example cosim_led

# or use the built-in showcases
cargo run -p ee -- demo | bode | amp | cosim | tui
```

## Catalog

- **[`analog/`](analog/README.md)** — divider, RC/RL step, RLC resonator, diode, half-wave rectifier, non-inverting op-amp, common-emitter BJT amplifier. How each works + its verified operating point / waveform.
- **[`mcu/`](mcu/README.md)** — Uno `blink` (D13 at ~1 Hz), co-sim LED (firmware → simulated circuit), PWM duty, UART TX, Pico blink. Runnable `cargo run -p ee-mcu --example …`.
- **[`tutorial/`](tutorial/)** — the scripts that accompany the [getting-started tutorial](../docs/tutorials/getting-started.md): `divider`, `rc_lowpass`, `checkpoints`, `sweep`.
- **[`formats/`](formats/)** — a SPICE deck (`divider.cir`, imports + simulates) and a native project (`rc-lowpass.eeproj`, round-trips); both pinned by a format test so they can't drift.
