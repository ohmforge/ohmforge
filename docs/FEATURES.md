# Feature catalog

What `ohmforge` does today, at **v0.8.1** — milestones **M0–M9 complete
(106/106 checkpoints)**. Every capability below is backed by code and tests
unless it is explicitly flagged *not yet wired*. For the authoritative execution
ledger (each item with its verifiable gate) see
[`plan/CHECKPOINTS.md`](plan/CHECKPOINTS.md); for the phased history see
[`ROADMAP.md`](ROADMAP.md).

A handful of items are honestly marked **⚠ not yet wired** — they are designed
for but not implemented, so you can tell the shipped surface from the plan.

---

## 1. Circuit simulation

- Passives & sources: resistor, capacitor, inductor, DC/sine/pulse sources,
  independent current sources, ground.
- DC operating point and time-domain **transient** — an error-controlled
  **adaptive** timestep by default, or a **fixed step** (which samples sine/PWM
  stimuli the adaptive driver would stride past).
- Nonlinear devices: **diode** (Shockley, with `IS`/`N`/`RS` model parameters),
  **BJT** (NPN/PNP, Ebers–Moll), **MOSFET** (N/P, level-1), solved by
  **Newton–Raphson** with `gmin`/source-stepping and SPICE junction limiting —
  non-convergence is surfaced in plain language (which node/device stalled).
- Analyses: **DC sweep**, **AC/Bode**, **parametric sweep**, **noise**
  (resistor thermal), **Monte-Carlo** with per-component tolerances and a
  reproducible seed, **transfer function / pole-zero** (`.TF`-style), and
  **2-port S-parameters** (with a Smith-chart readout).
- **Controlled sources**: VCVS (`E`), VCCS (`G`), CCVS (`H`), CCCS (`F`) — the
  ideal-op-amp and sensing building blocks.
- **Transformers / mutual inductance** via `Circuit::couple(l1, l2, k)` (robust
  at the ideal `k = 1`), in transient **and** AC.
- **Transmission lines** (lossless Branin + lossy) in the fixed-step transient
  and AC paths.
- **Thermal-aware** models: a circuit operating temperature and per-part
  temperature coefficients (`tempco`) feed the device models.
- **Switch-mode power**: buck/boost topologies driven by PWM sources.
- Composite **device subcircuits** expanded over the primitives (no bespoke
  solver nodes): op-amp macro-model, analog switch, relay.
- **Solver**: dense Gaussian elimination with partial pivoting for small
  systems, automatically switching to a **KLU-class sparse** path (fill-reducing
  ordering + reusable symbolic factorization) above a size threshold. An opt-in
  native `parallel` feature fans independent analyses (Monte-Carlo samples,
  sweep points) across OS threads with bit-identical results.

## 2. Digital & mixed-signal

- Logic primitives: gates, D/JK flip-flops, latches, counters, shift registers,
  mux/decoder — event-driven, with multi-valued logic (`0/1/X/Z`) and
  configurable propagation delays.
- **HDL-lite**: define a custom logic block from a **truth table**, a Boolean
  **expression** over named inputs, a read-only **ROM**, or a writable **RAM**.
- **Tri-state buses**: multiple tri-state drivers wired together resolve like
  real hardware (one driver → its value, contention → `X`).
- Mixed-signal **boundary elements**: A/D and D/A with configurable thresholds
  (Schmitt hysteresis), Thévenin drive strength, and slew limiting.
- Protocol **decoders** on logic-analyzer channels: UART, SPI, I²C.

## 3. Microcontrollers & boards

- **Arduino Uno / Nano / Mega** (ATmega328P/2560, AVR), instruction-accurate,
  with GPIO / Timer0+Timer1 / Fast-PWM / 10-bit ADC / USART.
- **Raspberry Pi Pico** (RP2040, Cortex-M0+ / ARMv6-M) with SIO GPIO.
- **STM32-class** Cortex-M3/M4 (Thumb-2) core and board.
- **ESP32** (Xtensa LX6, CALL0 subset) core and board.
- **Co-simulation**: firmware drives the simulated circuit and reads simulated
  node voltages through the ADC/input pins — run your real firmware before you
  solder. Closed-loop (ADC → firmware → GPIO) and external/pin-change interrupts
  are modelled.
- Firmware loading: prebuilt **Intel HEX** and raw binary; compile a sketch via
  a **detected external toolchain** (`avr-gcc`/`avr-objcopy` or `arduino-cli`).
- ⚠ Not yet wired: **ELF** firmware loading (recognised, returns
  `UnsupportedFormat`); a firmware **debug view** (registers/memory/disassembly,
  breakpoints) in the TUI; drop-in display/sensor module blocks beyond the
  sensor/actuator models in §3-bis.

### 3-bis. Sensor & actuator models

Built over the primitives (no bespoke solver nodes), at least one wired through
the MCU co-sim bridge:

- NTC **thermistor** (β / Steinhart), **photoresistor** (LDR),
  **potentiometer** (linear wiper), **DC motor** (back-EMF + mechanical time
  constant), **servo** (pulse-width → angle), **stepper** (step → position).

## 4. Virtual instruments

- Voltage/current **probes** on any node/branch.
- **Oscilloscope**: multi-channel, timebase, edge/level trigger, cursors
  (Δt / 1/Δt / Δv), math channels (A−B, A+B, A·B, **FFT**).
- **Multimeter**: DC volts, true RMS, peak-to-peak, frequency.
- **Function generator**: sine / square / triangle / pulse with amplitude,
  frequency, offset, duty, phase.
- **Bench power supply**: programmable, current-limited, with the CV/CC
  crossover.
- **Logic analyzer** with UART/SPI/I²C protocol decode.
- **Bode / spectrum** analyzer; **FFT** from the scope.
- **RF panels**: a **Smith chart** (Γ/S11) and an **eye diagram** (fold a
  bitstream / periodic signal).
- **Dataflow patcher**: wire processing blocks (Gain/Offset/Abs/Clamp/Limit/
  Add/Sub/Mul/Filter/Rms/Mean/Peak/Log) into reusable virtual instruments, plus
  pass/fail **test sequences**; an instrument/block plugin surface lets custom
  blocks compose with the built-ins.

## 5. Visualization & "seeing" circuits

- ASCII/Unicode **schematic view** with named nodes.
- **Live op-point overlay**: colour nodes by voltage directly on the schematic.
- High-res **braille waveform** plots; stacked multi-channel.
- **Breadboard view**: render the circuit as a breadboard layout.
- Bode plots, phase, eye-diagram and Smith-chart views.
- **Visual schematic capture**: mouse place/move/wire on a grid canvas in the
  TUI, and a drag-and-drop canvas in the browser.
- ⚠ Not yet wired: schematic/waveform export to **SVG/PNG** (waveforms export to
  CSV/VCD today — see §9).

## 6. The shell (`eesh`)

- REPL: build/wire/probe/run circuits by command; tab-completion, history,
  inline `help`.
- Engineering-notation values (`4k7`, `1u`, `2n2`, `1meg`) and typed units.
- **Scripting**: pipe a `.eesh` file on stdin — `#` comments, `set <var>`,
  `$vars`, and `repeat … { }` loops run through the embedded engine, which
  expands to a flat command list for **deterministic replay**.
- Session **snapshots/undo**, named **checkpoints**, and `save`/`load` of the
  whole session.
- Full command reference in [`SHELL.md`](SHELL.md).

## 7. AI copilot

- **Ask**: natural-language Q&A grounded in a real operating-point read.
- **Design**: generate a netlist patch, scratch-apply it to a clone,
  re-simulate, and verify it against a spec before presenting it.
- **Debug**: read sim results and localize the single most-likely faulty part.
- **Firmware**: emit a validated Intel-HEX sketch image for the emulator.
- **Verified & scoped**: the model acts only through the same `eesh`/engine API
  you do; nothing touches your circuit until you `apply` (vs `reject`).
- **Offline, no-key mode**: a deterministic design-rule review / DRC
  (`of review` / `of drc`), value calculators, and a tutor — always reachable
  with no API key and no network.
- **Live transport** (opt-in): a native HTTP path to the Claude Messages API
  behind `--features http` + `ANTHROPIC_API_KEY`, routable via
  `EE_BASE_URL`/`ANTHROPIC_BASE_URL`.
- ⚠ Not yet wired: the `wasm32` `fetch` transport (a documented TODO stub);
  auto-flashing a generated sketch straight into a live emulator (the staged HEX
  is what the co-sim bridge consumes — a thin follow-up).
- Full design in [`AI.md`](AI.md).

## 8. Learning & content

- Built-in **example projects** and runnable scripts ([`../examples/`](../examples/README.md)).
- The offline copilot's **tutor** and value calculators.
- ⚠ Not yet wired: graded challenges with automatic grading, and contextual
  glossary/datasheet hover snippets.

## 9. Interop & files

- Native project format (human-readable, diff-friendly, round-trips byte-for-byte).
- **SPICE netlist** import/export, including real `D`/`Q`/`M` device lines,
  `.model` cards, the four controlled sources, and a small vendor-model catalog
  (1N4148 / 2N3904 / 2N2222 / 1N4007 / 2N7000).
- **KiCad** netlist import.
- **Fritzing**-style export and **BOM** (CSV) export.
- Waveform export: **CSV** and **VCD** (view in GTKWave etc.).

## 10. Platform & performance

- Runs on Linux/macOS/Windows terminals and over SSH; CI is green on all three.
- The compute core is I/O-free and **`wasm32`-clean** (`std`, no OS/filesystem/
  network/clock) — the same engine runs in the TUI, headless, and the browser.
- **WASM** browser build: editor, example gallery + tune slider, drag-and-drop
  canvas, shareable links, and real-time co-edit (op-based CRDT merge + a
  WebSocket relay).
- **Determinism**: fixed-seed Monte-Carlo and golden-file reproducibility —
  the same project + seed reproduces bit-for-bit, which is what lets the AI
  verify its own suggestions.
- `criterion` benches gate the hot paths (matrix stamp/solve, event queue,
  co-sim step).
- ⚠ Not yet wired: a full-SBC Raspberry Pi bridge (QEMU-hosted image ↔ simulated
  GPIO) — out of scope for the native emulator by design.
