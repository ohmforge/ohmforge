# Checkpoint ledger

The single source of truth for **progress**. Each checkpoint has a verifiable **acceptance gate**. States: `[ ]` todo · `[~]` in progress · `[x]` done (gate passing) · `[!]` blocked.

Conventions and how tracks relate: [`README.md`](README.md). Design detail per checkpoint: the matching `track-*.md`.

> Rule: a checkpoint flips to `[x]` **only** when its gate command/test passes on a green `cargo test --workspace`.

---

## Track: sim-core (`ee-core`, `ee-sim`) → design: [`track-sim-core.md`](track-sim-core.md)

### M0 — linear engine + transient
- [ ] **CP-SIM-M0.1** Circuit graph: add/remove parts, `wire` merges nets, name lookup, stamping iteration. — *gate:* `cargo test -p ee-core graph::` covers net-merge + lookup.
- [x] **CP-SIM-M0.2** Units + engineering-notation `Value` parser. — *gate:* `cargo test -p ee-core value::` (done in scaffold).
- [ ] **CP-SIM-M0.3** Linear MNA + DC operating point (R + independent sources). — *gate:* golden resistive-divider solves to 1e-9 (`rc_divider_op`).
- [ ] **CP-SIM-M0.4** Companion models for C/L + Backward-Euler transient. — *gate:* RC step response matches `1-e^(-t/RC)` within 1% (`rc_step_transient`).
- [ ] **CP-SIM-M0.5** `Signal` type + streaming results from an analysis. — *gate:* a headless probe reads a transient `Signal` with correct units/length.

### M1 — nonlinear + frequency domain
- [ ] **CP-SIM-M1.1** Nonlinear device framework + Newton–Raphson; diode/LED. — *gate:* diode I–V matches Shockley within tol (`diode_iv`).
- [ ] **CP-SIM-M1.2** BJT + MOSFET models. — *gate:* common-emitter DC sweep hits known operating points.
- [ ] **CP-SIM-M1.3** Convergence aids (gmin/source stepping, junction limiting). — *gate:* a known-stiff netlist that failed now converges.
- [ ] **CP-SIM-M1.4** AC small-signal (complex MNA). — *gate:* RC low-pass shows −3 dB at `fc` and −45° phase within tol (`rc_ac_bode`).
- [ ] **CP-SIM-M1.5** DC sweep + parametric sweep. — *gate:* sweep produces the expected response family (golden).
- [ ] **CP-SIM-M1.6** Adaptive timestep (LTE control). — *gate:* step count bounded while accuracy held vs fixed-step reference.

### M2 — statistical + scale
- [ ] **CP-SIM-M2.1** Noise analysis (output spectral density). — *gate:* resistor thermal-noise floor matches `4kTR` within tol.
- [ ] **CP-SIM-M2.2** Monte-Carlo + component tolerances (seeded, deterministic). — *gate:* fixed seed reproduces the same spread bit-for-bit.
- [ ] **CP-SIM-M2.3** Controlled + behavioral sources (VCVS/VCCS/…). — *gate:* an ideal-gain VCVS stage matches expected transfer.
- [ ] **CP-SIM-M2.4** Sparse solver path for large nets. — *gate:* criterion bench shows target scaling on a 1k-node ladder.

---

## Track: boards (`ee-digital`, `ee-mcu`, `ee-devices`) → design: [`track-boards.md`](track-boards.md)

### M1 — digital kernel + AVR
- [ ] **CP-BRD-M1.1** Event-driven digital kernel (event heap, delays, 0/1/X/Z). — *gate:* a ring oscillator produces the expected period from gate delays.
- [ ] **CP-BRD-M1.2** Logic primitives (gates, D/JK flip-flops, counters, mux). — *gate:* truth tables + a counter sequence test.
- [ ] **CP-BRD-M1.3** AVR core (ATmega328P): decode + execute + interrupts. — *gate:* AVR instruction test-vectors pass.
- [ ] **CP-BRD-M1.4** AVR peripherals (GPIO, timers/PWM, ADC, UART). — *gate:* measured PWM duty on a pin; UART TX bytes decode correctly.
- [ ] **CP-BRD-M1.5** Uno board def + firmware load (`.hex`/`.elf`). — *gate:* load `blink.hex`; D13 toggles at ~1 Hz in the sim.

### M2 — co-simulation + more cores
- [ ] **CP-BRD-M2.1** Co-sim bridge: pin↔net, clock sync. — *gate:* `analogRead` returns a divider's voltage; `digitalWrite` lights a sim LED.
- [ ] **CP-BRD-M2.2** Mixed-signal boundary elements (threshold, drive strength, rise/fall). — *gate:* boundary behavior test at configured thresholds.
- [ ] **CP-BRD-M2.3** Cortex-M0+ core + RP2040 (Raspberry Pi Pico). — *gate:* ARMv6-M (Thumb) test-vectors; Pico blink toggles a pin.
- [ ] **CP-BRD-M2.4** ATmega2560 / ATtiny variants. — *gate:* variant peripheral map test.
- [ ] **CP-BRD-M2.5** External toolchain compile orchestration (`arduino-cli`/`avr-gcc`/`arm-none-eabi`). — *gate:* compile a `.ino` → hex → run end-to-end (skipped if toolchain absent).

---

## Track: ui-shell (`ee-tui`, `ee-shell`, `ee-instruments`) → design: [`track-ui-shell.md`](track-ui-shell.md)

### M0 — shell + first pixels
- [ ] **CP-UX-M0.1** eesh parser + core commands (`add`/`wire`/`probe`/`run`/`set`/`show`/`help`). — *gate:* parse tests; a scripted session builds a circuit graph.
- [ ] **CP-UX-M0.2** TUI shell pane + schematic list/net view. — *gate:* renders a built circuit; layout snapshot test.
- [ ] **CP-UX-M0.3** Braille waveform plot widget. — *gate:* plotting a known sine matches a golden ASCII snapshot.
- [ ] **CP-UX-M0.4** probe → plot pipeline (consume `Signal`). — *gate:* `run` then `scope` shows the RC curve from `CP-SIM-M0.4`.

### M1 — instruments + interaction
- [ ] **CP-UX-M1.1** Oscilloscope (multi-channel, edge/level trigger, cursors). — *gate:* trigger locks a periodic signal; snapshot.
- [ ] **CP-UX-M1.2** Multimeter + function generator + bench PSU panels. — *gate:* DMM reads a node voltage; funcgen injects a source that shows on the scope.
- [ ] **CP-UX-M1.3** Live overlay (voltage color, current-flow animation). — *gate:* overlay values match the DC op-point.
- [ ] **CP-UX-M1.4** Dockable panels + mouse placement + command palette. — *gate:* interaction/event tests.
- [ ] **CP-UX-M1.5** Scripting engine (`.eesh`, loops/params) + deterministic replay. — *gate:* a param-sweep script runs and replays identically.

### M2 — analysis views
- [ ] **CP-UX-M2.1** Logic analyzer + protocol-decode view (UART/SPI/I²C). — *gate:* decode a known UART capture.
- [ ] **CP-UX-M2.2** Breadboard view. — *gate:* renders a small circuit as a breadboard layout (snapshot).
- [ ] **CP-UX-M2.3** Bode/spectrum panel + scope math/FFT channels. — *gate:* Bode plot of `CP-SIM-M1.4` renders correctly.
- [ ] **CP-UX-M2.4** Session snapshots / undo / named checkpoints. — *gate:* undo restores prior graph state.

---

## Track: ai-fmt (`ee-ai`, `ee-fmt`) → design: [`track-ai-fmt.md`](track-ai-fmt.md)

### M0 — persistence
- [ ] **CP-AF-M0.1** Native project format save/load (round-trip). — *gate:* save→load yields an equal circuit; golden file stable.

### M1 — interop + first AI
- [ ] **CP-AF-M1.1** SPICE netlist import/export. — *gate:* import a known netlist (sim matches) and export round-trips.
- [ ] **CP-AF-M1.2** Claude client (Messages API, streaming, tool schema, prompt caching) over a mockable transport. — *gate:* tool-call loop unit-tested with a fake transport (no network).
- [ ] **CP-AF-M1.3** AI tool layer over the circuit API (add/wire/set/run/read). — *gate:* "ask" answers a question grounded in a real sim read (stub model, real sim).

### M2 — verified design + waveform export
- [ ] **CP-AF-M2.1** Verification loop: staged patch → scratch apply → re-sim → measure → present diff; `apply`/`reject` gate. — *gate:* "5 V→3.3 V @ 500 mA" yields a patch whose sim meets spec; nothing applies without approval.
- [ ] **CP-AF-M2.2** Design / Debug / Firmware modes. — *gate:* Debug localizes a seeded fault; Firmware sketch flashes into the emulator (needs `CP-BRD-M2.1`).
- [ ] **CP-AF-M2.3** VCD + CSV waveform export. — *gate:* VCD opens in GTKWave; CSV matches probe data.

### M3 — breadth interop + offline
- [ ] **CP-AF-M3.1** KiCad netlist import; BOM export (CSV). — *gate:* import a known KiCad netlist (sim matches); BOM lists every part with value/qty.
- [ ] **CP-AF-M3.2** Offline mode: RAG parts index + heuristics/tutor. — *gate:* offline value calculators + rule checks (floating node, no ground, LED w/o resistor) fire correctly with no API key.

---

## Track: platform (cross-cutting, on `main`) → design: [`README.md`](README.md) §Cross-cutting
- [ ] **CP-PLAT.1** CI green: fmt + `clippy -D warnings` + test on linux/macOS/windows. — *gate:* CI workflow passes on a PR.
- [ ] **CP-PLAT.2** `aarch64` build (Raspberry Pi / Apple Silicon). — *gate:* cross build succeeds in CI.
- [ ] **CP-PLAT.3** `wasm32-unknown-unknown` build of the compute core. — *gate:* `ee-core`+`ee-sim` compile to wasm (no I/O leak).
- [ ] **CP-PLAT.4** Criterion benches wired for `stamp/solve` + event queue. — *gate:* `cargo bench` runs; thresholds recorded.
- [ ] **CP-PLAT.5** Determinism harness: golden sims reproduce bit-for-bit. — *gate:* golden-file suite passes twice identically.

---

### Progress summary

| Track | Done | Total |
|---|---|---|
| sim-core | 1 | 15 |
| boards | 0 | 10 |
| ui-shell | 0 | 13 |
| ai-fmt | 0 | 9 |
| platform | 0 | 5 |
| **Total** | **1** | **52** |

*Update this table when flipping a checkpoint. `CP-SIM-M0.2` is done (scaffold parser).*
