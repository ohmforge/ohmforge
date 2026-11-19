# ohmforge documentation

The hub for everything written about `ohmforge` — a terminal-first electronics
workbench: analog/mixed-signal simulation, microcontroller co-simulation, virtual
instruments, a scriptable shell, an AI copilot, and a browser build. New? Start
with the tutorial, then dip into the guide for whatever you're doing.

**Status:** v0.8.1 — milestones M0–M9 complete (106/106 checkpoints). Licensed
**Apache-2.0**; contributions under the [DCO](../CONTRIBUTING.md) sign-off.
Source: <https://github.com/ohmforge/ohmforge> · live demo:
<https://ohmforge.github.io/ohmforge/>.

**Two binaries** ship from the `ohmforge` crate: **`of`**, the umbrella CLI
(`of demo|bode|amp|cosim|view|tui`, the offline no-key `of review`/`of drc`
copilot, and the live `of ask` behind `--features http`), and **`eesh`**, the
shell REPL. From source, name the binary explicitly —
`cargo run -p ohmforge --bin of -- <cmd>` or `cargo run -p ohmforge --bin eesh` —
since the crate builds both.

## Learn by doing
- **[Tutorial: getting started in `eesh`](tutorials/getting-started.md)** — build a circuit, run an operating point and a transient, read the braille scope and the op-point overlay, then script it with loops and replay.
- **[Runnable examples](../examples/README.md)** — worked circuits you can run right now: analog ([`examples/analog`](../examples/analog/README.md)), MCU + co-simulation ([`examples/mcu`](../examples/mcu/README.md)), and file formats ([`examples/formats`](../examples/formats)).
- **[In the browser (WebAssembly)](../crates/ohmforge-web/README.md)** — build the engine to WASM and run a sim on a web page (`ohmforge-web`).

## Guides (how things work, for users)
- **[Simulation engine](guide/simulation.md)** — MNA stamping, the solve (dense + the KLU-class sparse path), Newton–Raphson + convergence aids, companion models, and every analysis (op · transient/adaptive + fixed-step · AC · noise · DC/parametric sweep · Monte-Carlo · transfer function/pole-zero · 2-port S-parameters), plus transformers and transmission lines.
- **[Emulation & co-simulation](guide/emulation.md)** — the instruction-accurate cores (AVR ATmega328P/Uno, RP2040/Pico, STM32-class Cortex-M3/M4, ESP32 Xtensa LX6), peripherals, firmware loading, the `PinIo` pin↔net bridge, the mixed-signal boundary, and the lockstep co-sim loop (`of cosim`).
- **[The view & instruments](guide/view.md)** — the interactive `of tui` (with schematic capture and a breadboard view), the scope (trigger/cursors), DMM, function generator, PSU, Bode/spectrum/FFT, the logic analyzer with UART/SPI/I²C decode, and the RF panels (Smith chart, eye diagram).
- **[AI copilot](guide/ai-copilot.md)** — Ask / Design / Debug / Firmware, the offline no-key review/DRC, the verification loop and trust model, the mockable transport, and runtime secrets.
- **[File formats](guide/formats.md)** — the netlist, native project, SPICE (with `.model` cards + a vendor catalog), KiCad import, Fritzing + BOM export, and VCD/CSV waveform export (with the `m`-vs-`meg` gotcha).

## Design & reference
- [`ARCHITECTURE.md`](ARCHITECTURE.md) — crates, the simulation core, co-simulation, the TUI, and the cross-cutting design decisions.
- [`FEATURES.md`](FEATURES.md) — the full feature catalog.
- [`SHELL.md`](SHELL.md) — the `eesh` language (with an "Implemented today" section vs. the design target).
- [`AI.md`](AI.md) — the copilot design and guardrails.
- [`ROADMAP.md`](ROADMAP.md) — phased milestones.

## Project plan (progress)
- [`plan/CHECKPOINTS.md`](plan/CHECKPOINTS.md) — the single source of truth for progress (every checkpoint has a verifiable gate).
- [`plan/README.md`](plan/README.md) — the worktree-per-track development model and inter-track contracts.
- Per-track design docs: [sim-core](plan/track-sim-core.md) · [boards](plan/track-boards.md) · [ui-shell](plan/track-ui-shell.md) · [ai-fmt](plan/track-ai-fmt.md).
- Milestone fan-out plans: [`plan/M1-PLAN.md`](plan/M1-PLAN.md) · [`plan/M2-PLAN.md`](plan/M2-PLAN.md).
