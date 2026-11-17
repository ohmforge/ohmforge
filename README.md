<p align="center">
  <img src="assets/ohmforge-logo.svg" alt="ohmforge" width="380">
</p>

<p align="center">
  <b>The AI-native circuit &amp; firmware workbench.</b><br>
  One small Rust binary that simulates your analog, digital, and MCU firmware together — in your terminal or your browser.
</p>

<p align="center">
  <a href="https://github.com/ohmforge/ohmforge/actions/workflows/ci.yml"><img src="https://github.com/ohmforge/ohmforge/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://crates.io/crates/ohmforge"><img src="https://img.shields.io/crates/v/ohmforge.svg" alt="crates.io"></a>
  <a href="#license"><img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="License: Apache-2.0"></a>
  <img src="https://img.shields.io/badge/built%20with-Rust-dea584.svg" alt="Built with Rust">
  <img src="https://img.shields.io/badge/runs-terminal%20%2B%20WASM-ff9f1c.svg" alt="Terminal + WASM">
  <img src="https://img.shields.io/badge/no%20cloud-required-brightgreen.svg" alt="No cloud required">
  <a href="docs/plan/CHECKPOINTS.md"><img src="https://img.shields.io/badge/milestones-M0–M9%20✓-success.svg" alt="M0–M9 complete"></a>
</p>

---

ohmforge is a **SPICE-class analog + mixed-signal simulator**, an **instruction-accurate MCU emulator** (AVR · RP2040 · STM32 · ESP32), a bench of **virtual instruments**, and an **AI copilot that runs the real solver and checks its own work** — all in a single fast, dependency-light Rust binary that runs in your terminal, over SSH, or in the browser. Nothing goes to any cloud.

> A breadboard + an oscilloscope + a patient EE tutor, that boots in a terminal anywhere.

## ⚡ Try it in 30 seconds

```sh
git clone https://github.com/ohmforge/ohmforge && cd ohmforge

cargo run -p ohmforge --bin of -- demo    # RC low-pass → transient → braille scope
cargo run -p ohmforge --bin of -- bode    # AC sweep → Bode plot, −3 dB at 1 kHz
cargo run -p ohmforge --bin of -- amp     # common-emitter BJT, op-point + color overlay
cargo run -p ohmforge --bin of -- cosim   # real AVR firmware drives D13 → a simulated LED (19.4 mA)
cargo run -p ohmforge --bin of -- tui     # the live, dockable terminal UI
```

In the browser — **[▶ try the live demo](https://ohmforge.github.io/ohmforge/)** (no install), or build it yourself:

```sh
wasm-pack build crates/ohmforge-web --target web --out-dir www/pkg
# then serve crates/ohmforge-web/www/  — e.g.  python3 -m http.server -d crates/ohmforge-web/www
```

ohmforge installs two commands: **`of`** (the workbench CLI — the subcommands above) and **`eesh`** (drops straight into the scriptable shell REPL).

New here? → **[getting-started tutorial](docs/tutorials/getting-started.md)**.

## Who it's for

| | |
|---|---|
| **Students & educators** | A deterministic, bit-for-bit engine you can trust for autograding, plus an offline no-key tutor that runs in a browser tab. |
| **Makers & hobbyists** | Flash your firmware *in simulation first* — the MCU reads a real solved analog node through its ADC and drives a pin back. |
| **Embedded & EE pros** | Scriptable, deterministic analog+firmware co-sim you can gate in CI. No cloud, no license server, runs over SSH. |
| **AI / dev-tool folks** | A copilot that *acts through the same tools you do* and verifies its claims by running the simulator — plus a no-key offline mode. |

## Your first circuit (`eesh`)

```text
add V1 in gnd 5          # a 5 V source from node `in` to ground
add R1 in out 1k
add C1 out gnd 1u
probe out
run tran 5m              # transient; also: run op · ac · dc · noise · mc · tf
show                     # render the braille scope
set R1.value 2k          # retune a component and re-solve
```

Scripts are plain `.eesh` files — see [`examples/`](examples/README.md). Pipe one in: `cat examples/tutorial/rc_lowpass.eesh | cargo run -p ohmforge --bin of`.

## What works today

- **Analog** — linear + nonlinear (diode `IS/N/RS`, BJT, MOSFET) with Newton–Raphson + gmin/source-stepping convergence aids; AC/Bode, noise, Monte-Carlo, adaptive-timestep transient; mutual inductance / transformers; transmission lines; **pole-zero + transfer-function (`.TF`)**; **2-port S-parameters**; **switch-mode power electronics** (PWM-switched buck/boost); a KLU-class sparse solver in the hot loop + multithreaded analyses.
- **Digital & MCU** — an event-driven kernel + HDL-lite; instruction-accurate **AVR (328P/2560/ATtiny)**, **RP2040 (Cortex-M0+)**, **STM32 (Cortex-M3/M4)**, **ESP32 (Xtensa LX6)** cores with GPIO/timers/PWM/ADC/USART/SPI/I²C/interrupts and **lockstep analog↔firmware co-simulation**; **sensor & actuator models** (thermistor, photoresistor, pot, DC motor, servo, stepper).
- **Instruments & UI** — oscilloscope, DMM, function generator, PSU; logic analyzer with UART/SPI/I²C decode; Bode/FFT; **Smith chart + eye diagram**; breadboard view; a dockable TUI with live overlay, component tuning, current-flow animation, transient playback, and **mouse-driven schematic capture**.
- **Interop** — SPICE (incl. `.model` devices), KiCad, VCD/CSV, Fritzing, BOM, and a lossless native project format.
- **AI copilot** — Ask / Design / Debug / Firmware, grounded in the real engine via a verify loop, with spec-driven auto-tune and a tutor — plus a fully **offline, no-key** mode (DRC, value calculators, parts index, grader).
- **Browser** — a WASM build with an in-page editor, a **drag-and-drop schematic canvas**, a runnable example gallery with live component tuning, shareable-session links, and real-time co-editing.

Full status and the 106-checkpoint ledger: [`docs/plan/CHECKPOINTS.md`](docs/plan/CHECKPOINTS.md). Current release: **v0.8.0** (M0–M9 complete). The whole workspace stays green — `clippy -D warnings`, `wasm32` core + `ohmforge-web`, `-D warnings` docs.

## The AI copilot

```sh
# Offline — no key, no network. Design-rule review of a circuit:
printf 'add V1 in gnd 5\nadd R1 in out 1k\nadd D1 out gnd\n' | cargo run -p ohmforge --bin of -- review

# Live — the model drives the real shell + engine and verifies itself:
export ANTHROPIC_API_KEY=sk-...          # optional: EE_BASE_URL / ANTHROPIC_BASE_URL for a proxy
cargo run -p ohmforge --features http --bin of -- ask "design a 5 V to 3.3 V divider and check it"
```

The live transport is behind the opt-in `http` feature, so the default build (and the WASM core) pull **no network or TLS stack**. See [`docs/guide/ai-copilot.md`](docs/guide/ai-copilot.md).

## Install

Today, from source (stable Rust; see [`rust-toolchain.toml`](rust-toolchain.toml)):

```sh
cargo install --path crates/ohmforge          # installs `of` and `eesh`
```

Published crates (`cargo install ohmforge`) land with the first crates.io release — see [`crates/PUBLISHING.md`](crates/PUBLISHING.md).

## Workspace

A Cargo workspace of focused crates — the compute core (`ohmforge-core`, `ohmforge-sim`) is **dependency-free, `wasm32`-clean, and deterministic**:

| Crate | What it is |
|---|---|
| [`ohmforge-core`](crates/ohmforge-core) | circuit graph + core types (dep-free, wasm-clean) |
| [`ohmforge-sim`](crates/ohmforge-sim) | SPICE-class analog / mixed-signal engine (dep-free, wasm-clean) |
| [`ohmforge-digital`](crates/ohmforge-digital) | event-driven digital kernel + HDL-lite |
| [`ohmforge-mcu`](crates/ohmforge-mcu) | instruction-accurate MCU cores + co-sim |
| [`ohmforge-devices`](crates/ohmforge-devices) | composite device subcircuits |
| [`ohmforge-instruments`](crates/ohmforge-instruments) | scope / DMM / funcgen / PSU / logic analyzer / Bode-FFT |
| [`ohmforge-fmt`](crates/ohmforge-fmt) | SPICE / KiCad / VCD / CSV / Fritzing / BOM / project I/O |
| [`ohmforge-tui`](crates/ohmforge-tui) | the terminal UI |
| [`ohmforge-shell`](crates/ohmforge-shell) | the `eesh` command language + REPL |
| [`ohmforge-ai`](crates/ohmforge-ai) | the copilot (offline + opt-in live) |
| [`ohmforge`](crates/ohmforge) | the `of` / `eesh` binary |
| [`ohmforge-web`](crates/ohmforge-web) | WASM bindings + browser app |

## Documentation

**[`docs/README.md`](docs/README.md) is the hub.**
- **Tutorial:** [getting started](docs/tutorials/getting-started.md) · runnable [`examples/`](examples/README.md)
- **Guides:** [simulation](docs/guide/simulation.md) · [emulation & co-sim](docs/guide/emulation.md) · [view & instruments](docs/guide/view.md) · [AI copilot](docs/guide/ai-copilot.md) · [formats](docs/guide/formats.md)
- **Design:** [`ARCHITECTURE.md`](docs/ARCHITECTURE.md) · [`FEATURES.md`](docs/FEATURES.md) · [`SHELL.md`](docs/SHELL.md) · [`ROADMAP.md`](docs/ROADMAP.md) · [`CHECKPOINTS.md`](docs/plan/CHECKPOINTS.md)

## Contributing

Contributions welcome! Please read [`CONTRIBUTING.md`](CONTRIBUTING.md) (we use **DCO** sign-off — `git commit -s`) and our [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md). Security reports: [`SECURITY.md`](SECURITY.md).

## License

Licensed under the **[Apache License 2.0](LICENSE-APACHE)**. Contributions are accepted under the same license.

The **ohmforge** name and logo are trademarks of the project maintainers — see [`TRADEMARK.md`](TRADEMARK.md). The code is free; the brand is protected.
