# Roadmap

Phased so that **something real works at the end of every milestone**. Each phase ships a usable slice; nothing waits on the whole thing being done. Ordering favors proving the risky core early (the solver + co-sim) over breadth.

**Status (v0.8.1): milestones M0–M9 are complete — 106/106 checkpoints.** The
phases below are the original plan; the boxes record what shipped, with the few
genuine gaps flagged. The vision phase (originally "M4") was delivered and
extended across **M4–M9** — the device-model library and `.model` support, the
KLU-class sparse solver and multi-threaded analyses, transformers/transmission
lines, pole-zero/`.TF` and 2-port S-parameters, switch-mode power, STM32 and
ESP32 cores, sensor/actuator models, RF instruments (Smith chart, eye diagram),
visual schematic capture, and the browser build with real-time co-edit. The
authoritative per-checkpoint ledger is [`plan/CHECKPOINTS.md`](plan/CHECKPOINTS.md).

---

## M0 — Skeleton & spark (prove the loop)

**Goal:** type a circuit in `eesh`, simulate it, see a waveform. The end-to-end pipeline works, even if narrow.

- [x] Cargo workspace + crate stubs.
- [x] `ohmforge-core`: circuit graph (union-find nets), typed units/values, engineering-notation parser (`4k7`, `1u`), `Signal` result type.
- [x] `ohmforge-sim`: MNA for **linear** circuits (R, C, L, DC voltage + current sources) + **DC op-point** + **transient** (Backward Euler), validated against analytic RC/RL/RLC. _(time-varying pulse/sine sources: follow-up.)_
- [x] `ohmforge-shell`: minimal REPL — `add`, `wire`, `probe`, `run`, `show`, `help`; interactive (TTY) or scripted (pipe). _(line history + completion: follow-up.)_
- [x] `ohmforge-tui`: **braille waveform** plot, schematic list view, and dockable shell pane.
- [x] `ohmforge-fmt`: native project save/load (human-readable, lossless round-trip) + friendly netlist import.
- [x] `criterion` benches on stamp/solve; CI build/test/clippy/fmt/wasm/MSRV/docs green on Linux/macOS/Windows.

**Demo:** an RC low-pass built by command, transient run, waveform on screen. ✅ pipeline proven.

---

## M1 — A real simulator + instruments + first AI

**Goal:** it's a genuinely useful analog simulator with instruments, and the AI can *explain* what you built.

- [x] Nonlinear devices: diode/LED, BJT, MOSFET, op-amp; **Newton–Raphson** + convergence aids.
- [x] Analyses: **DC sweep**, **AC/Bode**, parametric sweep; adaptive timestep.
- [x] `ohmforge-digital`: gates/flip-flops/counters, event-driven, X/Z logic.
- [x] `ohmforge-instruments`: oscilloscope (trigger/cursors), multimeter, function generator.
- [x] `ohmforge-tui`: live op-point overlay (voltage colour), multi-channel scope, dockable panels, mouse placement.
- [x] `ohmforge-mcu` (`avr`): **Arduino Uno** core + GPIO/timers/PWM/ADC/UART; load `.hex` (Intel HEX). *ELF is recognised but returns `UnsupportedFormat`.*
- [x] `ohmforge-ai`: **Ask** mode (tool-using, grounded in sim results); Claude client (live transport opt-in behind `--features http`).
- [x] `ohmforge-fmt`: **SPICE netlist** import/export.
- [x] `ohmforge-shell`: **scripting** engine (`.eesh`, loops/params) + deterministic **session replay**.
- [x] Static + `aarch64` builds; example project library.

**Demo:** design an amplifier, sweep its Bode plot, load a blink `.hex` on a Uno, ask "what's the gain at 1 kHz?" and get a grounded answer.

---

## M2 — Co-simulation, mixed-signal, AI that designs

**Goal:** run **real firmware against a simulated circuit**, and let the AI design/debug with verification.

- [x] **Co-simulation bridge**: MCU pins ↔ sim nodes, synchronized clocks; ADC reads real node voltages.
- [x] Compile sketches via a detected external toolchain (`avr-gcc`/`avr-objcopy` or `arduino-cli`).
- [x] `ohmforge-mcu` (`cortexm`): **Raspberry Pi Pico (RP2040, Cortex-M0+)**; ATmega2560 (Mega).
- [x] Mixed-signal boundary elements; logic analyzer + **UART/SPI/I²C decode**.
- [x] Analyses: **noise**, **Monte-Carlo/tolerances**, controlled sources, **thermal-aware models** (temperature + tempco).
- [x] Instruments: bench PSU, Bode/spectrum, scope **math/FFT**.
- [x] `ohmforge-ai`: **Design / Debug / Firmware** modes with the **staged-patch + re-simulate verification loop**; `apply`/`reject`.
- [x] Visualization: **breadboard view**; export CSV/VCD. *(SVG/PNG export is not yet wired.)*
- [x] `ohmforge-shell`: session **snapshots/undo**, named checkpoints.
- [x] **WASM** build of core + a browser front end.
- [ ] MCU **debug view** (regs/mem/disasm, step, breakpoints) in the TUI — **not yet wired**.

**Demo:** wire a Pico + sensor + LED, have the AI write the firmware, co-simulate, watch the logic analyzer decode the I²C traffic, and export a VCD.

---

## M3 — Breadth, boards, and the LabVIEW-style patcher

**Goal:** the "feature-rich, make-anything" phase.

- [x] **Dataflow patcher**: wire processing blocks (filter/math/log/limits) into reusable virtual instruments; automated test sequences with pass/fail.
- [x] **ESP32-class (Xtensa LX6) / STM32-class (Cortex-M3/M4) cores.**
- [x] **HDL-lite** custom logic blocks (truth table / expression / ROM / RAM); buses/tri-state.
- [x] **Sensor/actuator models**: thermistor, LDR, potentiometer, DC motor, servo, stepper (≥1 wired through the co-sim bridge).
- [x] `ohmforge-ai`: **offline mode** (heuristics + design-rule review); tutor mode.
- [x] KiCad netlist import; BOM export; Fritzing-style export.
- [ ] Manifest-defined **custom boards**, drop-in display/7-seg/LCD/WS2812 modules, and **graded challenges** — **not yet wired** (the sensor/actuator models above are the shipped "modules" surface).

**Demo:** build a custom board manifest for your own PCB, drop in an LCD + rotary encoder, run a graded "make a thermostat" challenge, all in the terminal.

---

## M4–M9 — Vision delivered (breadth, RF, power, boards, browser)

- [x] Richer device model library: SPICE `.model` cards + a vendor-model catalog.
- [x] A **KLU-class sparse** solver for larger circuits; opt-in **multi-threaded** analyses (`parallel` feature).
- [x] **Transformers** (mutual inductance) and **transmission lines**; **pole-zero/`.TF`** and **2-port S-parameters**; **switch-mode power** (buck/boost + PWM).
- [x] **RF instruments**: Smith chart and eye diagram.
- [x] **Visual schematic capture** (TUI mouse canvas + browser drag-drop).
- [x] Collaborative/shared sessions: a shareable browser page with real-time **co-edit** (op-based CRDT merge + a WebSocket relay).
- [x] Instrument/processing-**block plugin surface** (custom blocks compose with the built-ins).
- [ ] A broader plugin API for community **device models / boards / lessons** — **not yet wired**.
- [ ] Optional full-SBC Raspberry Pi bridge (QEMU-hosted image ↔ simulated GPIO) — **out of scope** for the native emulator by design.

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
