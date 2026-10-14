# Roadmap

Phased so that **something real works at the end of every milestone**. Each phase ships a usable slice; nothing waits on the whole thing being done. Ordering favors proving the risky core early (the solver + co-sim) over breadth.

---

## M0 — Skeleton & spark (prove the loop)

**Goal:** type a circuit in `eesh`, simulate it, see a waveform. The end-to-end pipeline works, even if narrow.

- [x] Cargo workspace + crate stubs.
- [x] `ee-core`: circuit graph (union-find nets), typed units/values, engineering-notation parser (`4k7`, `1u`), `Signal` result type.
- [x] `ee-sim`: MNA for **linear** circuits (R, C, L, DC voltage + current sources) + **DC op-point** + **transient** (Backward Euler), validated against analytic RC/RL/RLC. _(time-varying pulse/sine sources: follow-up.)_
- [x] `ee-shell`: minimal REPL — `add`, `wire`, `probe`, `run`, `show`, `help`; interactive (TTY) or scripted (pipe). _(line history + completion: follow-up.)_
- [ ] `ee-tui`: **braille waveform** plot done ✅; schematic list view + dockable shell pane still to do.
- [x] `ee-fmt`: native project save/load (human-readable, lossless round-trip) + friendly netlist import.
- [ ] `criterion` benches on stamp/solve (CI build/test/clippy/fmt/wasm/MSRV/docs on Linux/macOS/Windows is done ✅).

**Demo:** an RC low-pass built by command, transient run, waveform on screen. ✅ pipeline proven.

---

## M1 — A real simulator + instruments + first AI

**Goal:** it's a genuinely useful analog simulator with instruments, and the AI can *explain* what you built.

- [ ] Nonlinear devices: diode/LED, BJT, MOSFET, op-amp; **Newton–Raphson** + convergence aids.
- [ ] Analyses: **DC sweep**, **AC/Bode**, parametric sweep; adaptive timestep.
- [ ] `ee-digital`: gates/flip-flops/counters, event-driven, X/Z logic.
- [ ] `ee-instruments`: oscilloscope (trigger/cursors), multimeter, function generator.
- [ ] `ee-tui`: live overlay (voltage color / current flow), multi-channel scope, dockable panels, mouse placement.
- [ ] `ee-mcu` (`avr`): **Arduino Uno** core + GPIO/timers/PWM/ADC/UART; load `.hex`/`.elf`.
- [ ] `ee-ai`: **Ask** mode (tool-using, grounded in sim results); Claude client + streaming.
- [ ] `ee-fmt`: **SPICE netlist** import/export.
- [ ] `ee-shell`: **scripting** engine (`.eesh`, loops/params) + deterministic **session replay**.
- [ ] Static + `aarch64` builds; example project library.

**Demo:** design an amplifier, sweep its Bode plot, load a blink `.hex` on a Uno, ask "what's the gain at 1 kHz?" and get a grounded answer.

---

## M2 — Co-simulation, mixed-signal, AI that designs

**Goal:** run **real firmware against a simulated circuit**, and let the AI design/debug with verification.

- [ ] **Co-simulation bridge**: MCU pins ↔ sim nodes, synchronized clocks; ADC reads real node voltages.
- [ ] Compile sketches via detected external toolchains (`arduino-cli`, `avr-gcc`, `arm-none-eabi-gcc`).
- [ ] MCU **debug view**: regs/mem/disasm, step, breakpoints, pin/peripheral state.
- [ ] `ee-mcu` (`cortexm`): **Raspberry Pi Pico (RP2040, Cortex-M0+)**; ATmega2560/ATtiny.
- [ ] Mixed-signal boundary elements; logic analyzer + **UART/SPI/I²C decode**.
- [ ] Analyses: **noise**, **Monte-Carlo/tolerances**, behavioral/controlled sources, **thermal-aware models** (temperature/self-heating).
- [ ] Instruments: bench PSU, Bode/spectrum, scope **math/FFT**.
- [ ] `ee-ai`: **Design / Debug / Firmware** modes with the **staged-patch + re-simulate verification loop**; `apply`/`reject`.
- [ ] Visualization: **breadboard view**; export CSV/VCD/SVG/PNG.
- [ ] `ee-shell`: session **snapshots/undo**, named checkpoints.
- [ ] **WASM** build of core + a browser front end.

**Demo:** wire a Pico + sensor + LED, have the AI write the firmware, co-simulate, watch the logic analyzer decode the I²C traffic, and export a VCD.

---

## M3 — Breadth, boards, and the LabVIEW-style patcher

**Goal:** the "feature-rich, make-anything" phase.

- [ ] **Dataflow patcher**: wire instruments + processing blocks (filter/math/log/limits) into reusable virtual instruments; data logging & automated test sequences with pass/fail.
- [ ] **Specialized/custom boards** via manifest (pinout + MCU + onboard parts); ESP32-class / STM32-class cores.
- [ ] **HDL-lite** custom logic blocks; memories/buses/tri-state.
- [ ] Common modules: 7-seg/LCD/servo/sensors/motor-driver/WS2812 as drop-in blocks.
- [ ] Learning: **guided lessons** + **graded challenges** run against the simulator; contextual glossary/datasheet snippets.
- [ ] `ee-ai`: **offline mode** (heuristics + local index); tutor mode.
- [ ] KiCad netlist import; BOM export; Fritzing-style export.

**Demo:** build a custom board manifest for your own PCB, drop in an LCD + rotary encoder, run a graded "make a thermostat" challenge, all in the terminal.

---

## M4 — Vision / stretch

- [ ] Richer device model library (SPICE `.model` compatibility, vendor models).
- [ ] True sparse KLU-class solver for large circuits; multi-threaded analyses.
- [ ] Collaborative/shared sessions; publish a circuit as a shareable WASM page ("open in browser, poke it").
- [ ] Plugin API for community device models, boards, instruments, and lessons.
- [ ] Optional full-SBC Raspberry Pi bridge (QEMU-hosted image ↔ simulated GPIO/peripherals).

---

## Cross-cutting (every milestone)

- **Performance:** criterion benches gate the hot paths; keep small/medium circuits interactive and MCU co-sim near-real-time.
- **Determinism & tests:** golden-file simulation tests, property tests on the solver, fuzzing on parsers; fixed-seed Monte-Carlo.
- **Portability:** Linux/macOS/Windows + `aarch64` + WASM stay green in CI.
- **Docs & examples:** every feature lands with an example project and a `help` entry.
- **Friendliness:** error messages are specific and actionable; the AI stays grounded and always asks before changing a circuit.

---

## Risk register (what could go wrong, and the plan)

| Risk | Mitigation |
|---|---|
| SPICE-class **convergence** is genuinely hard. | Lean on proven techniques (gmin/source stepping, adaptive step, limiting); test against known-good netlists; surface convergence issues in plain language early (M1). |
| **Co-simulation timing** (MCU clock vs. sim step) is fiddly. | Lockstep to the nearest event; start with the well-documented AVR/Uno case; validate against real Arduino behavior on reference sketches (M2). |
| **MCU accuracy** is a deep well. | Instruction-accurate first, cycle-accuracy only where it matters (timers/PWM); reuse test-vector suites from existing AVR emulators for validation. |
| **Scope creep** (this doc is huge). | Milestone discipline: every phase ships a working slice; breadth (M3/M4) only after the core is proven (M0–M2). |
| **AI hallucination** eroding trust. | The verification loop in [`AI.md`](AI.md): the model must re-run the sim and cite measured values; nothing auto-applies. |
| **Terminal-render performance** on big waveforms/schematics. | Decouple render from sim via snapshots; braille plotting + dirty-region redraw; bench the TUI too. |
