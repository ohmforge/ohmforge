# ohmforge documentation

The hub for everything written about `ohmforge`. New? Start with the tutorial, then dip into the guide for whatever you're doing.

## Learn by doing
- **[Tutorial: getting started in `eesh`](tutorials/getting-started.md)** — build a circuit, run an operating point and a transient, read the braille scope and the op-point overlay, then script it with loops and replay.
- **[Runnable examples](../examples/README.md)** — worked circuits you can run right now: analog ([`examples/analog`](../examples/analog/README.md)), MCU + co-simulation ([`examples/mcu`](../examples/mcu/README.md)), and file formats ([`examples/formats`](../examples/formats)).
- **[In the browser (WebAssembly)](../crates/ohmforge-web/README.md)** — build the engine to WASM and run a sim on a web page (`ohmforge-web`).

## Guides (how things work, for users)
- **[Simulation engine](guide/simulation.md)** — MNA stamping, the solve, Newton–Raphson + convergence aids, companion models, and every analysis (op · transient/adaptive · AC · noise · DC/parametric sweep · Monte-Carlo).
- **[Emulation & co-simulation](guide/emulation.md)** — the AVR (ATmega328P/Uno) and RP2040 (Cortex-M0+/Pico) cores, peripherals, firmware loading, the `PinIo` pin↔net bridge, the mixed-signal boundary, and the lockstep co-sim loop (`of cosim`).
- **[The view & instruments](guide/view.md)** — the interactive `of tui`, the scope (trigger/cursors), DMM, function generator, PSU, Bode/spectrum/FFT, and the logic analyzer with UART/SPI/I²C decode — how to use each to debug a circuit.
- **[AI copilot](guide/ai-copilot.md)** — Ask / Design / Debug / Firmware, the verification loop and trust model, the mockable transport, and runtime secrets.
- **[File formats](guide/formats.md)** — the netlist, native project, SPICE, VCD, and CSV formats (with the `m`-vs-`meg` gotcha).

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
