# Changelog

All notable changes to **ohmforge** are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
During the pre-1.0 phase, minor versions may include breaking changes; these are
called out in the relevant entry. Entries are grouped by release; for exact
timestamps see the Git tag history.

## [Unreleased]

_Nothing yet. Open a [Discussion](https://github.com/ohmforge/ohmforge/discussions) or [Issue](https://github.com/ohmforge/ohmforge/issues) to propose the next change._

## [0.9.0]

**M10 — improvements across every area** (from a six-track planning fan-out).

### Added
- **Simulation:** opt-in **trapezoidal integration** (`IntegrationMethod`, Backward-Euler stays default) so LC tanks ring correctly; a reusable ordered **complex-solver** for AC/noise (**~6–18× faster swept-AC** at scale).
- **MCU:** Cortex-M **Thumb-2 correctness** (`CBZ/CBNZ`, `UDIV/SDIV`, load/store writeback) so compiled C runs, a graceful `Unknown`-opcode halt (no panic), and **AVR Timer0/1 + USART interrupt vectors** (`millis`/`delay`/`Serial`-style ISRs).
- **UI:** live oscilloscope **trigger + Δt/Δv/freq cursors** in the scope pane, a swept **Smith S11 locus**, a `?`/F1 **help/keymap overlay**, and `run fft` / `measure` shell verbs.
- **Formats/AI:** **Touchstone `.s2p`** export; **SINE/PULSE** source parsing in the netlist importer; 3 new DRC rules, 11 new calculators, 10 new catalog parts in the offline (no-key) copilot.
- **Browser:** diodes/BJTs/MOSFETs/current sources now **simulate in the browser** (auto-routing through the SPICE importer); in-browser **AC/Bode**; **PWA/offline** (manifest + service worker).
- **Platform:** a **performance-regression CI** (Criterion benches vs a committed baseline), and **`proptest` property tests + `cargo-fuzz` targets** on the parsers and a solver KCL property (dev-only; shipped crates stay dependency-free).

### Fixed
- Digital `Expr` lexer panicked on multi-byte UTF-8 input (found by the new fuzzing); now iterates `char_indices`.
- README `cargo run -p ohmforge` commands disambiguated to `--bin of`/`--bin eesh` (two binaries).

## [0.8.1]

### Added
- `run tran <dur> <step>` — fixed-step transient that samples sine/PWM sources the adaptive default strode past.
- `run sparams <p1> <p2> [z0] [freq]` — wires `Engine::s_parameters` S11 onto a Smith chart.

### Changed
- Elite redesign of the landing page (demo wiring kept byte-identical).

### Fixed
- CI is green on Linux, macOS, and Windows: added `.gitattributes` (`eol=lf`) so byte-exact golden tests pass on Windows, and enabled the crossterm `windows` feature so the TUI builds there.
- Cleaned up Cargo manifest warnings (redundant workspace homepage, uninherited authors).

## [0.8.0]

M9 — power, sensors, RF, and visual capture.

### Added
- **Power electronics**: a controlled switching element via `Circuit::add_switch(...)` stamped in the fixed-step transient (buck/boost converters simulate; switch-free circuits assemble byte-identically).
- **2-port S-parameters**: `Engine::s_parameters(...)` (S11/S21/S12/S22 + the Z 2-port matrix) from the linearized AC solve.
- **Sensor & actuator models** in `ohmforge-devices`: NTC thermistor, photoresistor, potentiometer, DC motor (back-EMF), servo (pulse→angle), and stepper — built over existing primitives, one wired through MCU co-sim.
- **Visual schematic capture** in the TUI: mouse place/move/wire on a grid canvas driving the real `Circuit` through the shared edit path.
- **RF instrument panels**: a Smith chart (Γ/S11) and an eye diagram.
- **Drag-and-drop browser schematic canvas**: place/wire parts visually, round-trip to a netlist, and re-simulate.

## [0.7.0]

Public debut.

### Changed
- Rebranded `ee-assist` → **ohmforge** and published at <https://github.com/ohmforge/ohmforge>.
- **Relicensed to Apache-2.0** as the single license (the previous MIT dual-license was dropped). The ohmforge name and logo are now trademarks; see [`TRADEMARK.md`](TRADEMARK.md).

## [0.6.0]

M8 — hardening & reach, following hands-on use and a read-only QA audit.

### Added
- `ee review` / `ee drc` run the offline, no-key design-rule review; `ee ask <question>` is the live copilot behind the opt-in `http` feature.
- Shell reach: `run ac/dc/noise/mc/tf` (alongside `tran`/`op`), multi-terminal parts (BJT/MOSFET/controlled sources), and waveform/tolerance/tempco/temperature verbs.

### Fixed
- Linear AC/noise/`.TF`/DC-sweep now solve circuits with a DC-floating node instead of spuriously erroring `Singular`.
- TUI hardening: eliminated the full-`Clear` flicker, separated playback from the live overlay, sized the scope to its pane, and added an input caret.
- Instrument rendering: FFT resamples a non-uniform (adaptive) grid, the Bode `−3 dB` corner references the passband peak, and axis widths count characters not bytes.
- AI correctness: tighter LED/ground DRC, log-symmetric debug fault ranking, a capped tool loop that returns a partial transcript, and request/connect timeouts on the native HTTP client.
- Replaced the O(N²) undo with cheap reverse-ops and capped runaway `repeat`.

## [0.5.0]

M7 — RF, real signals, and going live.

### Added
- **Pole-zero + small-signal transfer function**: `Engine::transfer_function(...)` with a hand-rolled dense eigensolver (no external linear-algebra crate).
- **Transmission lines**: lossless (Branin travelling-wave) and lumped-LC lossy lines in transient and AC via `Circuit::add_line` / `add_lossy_line`.
- **ESP32 Xtensa LX6** core and an `Esp32` board over a documented supported ISA subset.
- **Live HTTP transport** behind the `http` feature (native `reqwest` + rustls) for a real Messages round-trip; the default build stays dependency-free and `wasm32`-clean.
- **Transient play/scrub**: a time-cursor playback over a cached transient.
- **Hosted signaling relay** so browser co-edit converges across machines, behind the opt-in `relay` feature.

## [0.4.0]

M6 — coupling & closed loops.

### Added
- **Mutual inductance / transformers** via `Circuit::couple(l1, l2, k)` in transient and AC.
- DC op-point **convergence aids** (gmin stepping + source ramping) that engage only on non-convergence.
- **Closed-loop MCU↔analog co-sim**: AVR ADC samples a real node and firmware reacts, plus external pin-change interrupts.
- Native-project **device-model parameter round-trip** (diode/BJT/MOSFET).
- **AI auto-tune**: closed-loop component-value optimization against a measured spec, offline.
- **Live interactive tuning + current-flow animation** in the TUI.
- Real-time co-edit **merge layer** (op-based CRDT-style `Replica`).

## [0.3.0]

M5 — close the debt and extend the frontier.

### Added
- Wired the **KLU-class ordered/reusable solver** into the engine's default hot loop.
- `DiodeModel` (`Is`/`n`/`Rs`) so SPICE `N`/`RS` parameters apply.
- **SPICE device export** (D/Q/M + `.model`, E/G/H/F) and a native project **waveform / tolerance / tempco round-trip**.
- AVR **SPI + I²C (TWI) master**.
- **Composite subcircuits**: op-amp macromodel, analog switch, relay (over existing primitives).
- **Interactive schematic editing** in the live TUI and in the browser, plus full-session shareable links.

## [0.2.0]

M4 — plugins, scale, and reach.

### Added
- **Plugin device-model API** + registry, and an instrument/block plugin surface with session export.
- **KLU-class sparse solver**: fill-reducing ordering + symbolic-factorization reuse.
- **Multithreaded analyses** (Monte-Carlo / sweep), native-gated and deterministic.
- **Cortex-M3/M4 (Thumb-2)** STM32-class core and board.
- **SPICE `.model` import** + a small vendor model library.
- Shareable circuit links via a URL fragment.

## [0.1.0]

M0–M3 — the foundation. The first tagged release (Apache-2.0).

### Added
- **Simulation core**: circuit graph, MNA DC operating point, companion-model transient (Backward-Euler + adaptive timestep), nonlinear devices (diode/BJT/MOSFET) with Newton–Raphson and convergence aids, AC small-signal/Bode, DC + parametric sweeps, noise, seeded Monte-Carlo, controlled sources, and a sparse solver for large nets.
- **Digital & MCU**: an event-driven digital kernel + logic primitives; instruction-accurate AVR (ATmega328P) and Cortex-M0+ (RP2040/Pico) cores with GPIO/timers/PWM/ADC/UART; and a lockstep co-sim bridge (`ee cosim`).
- **Instruments & UI**: oscilloscope, DMM, function generator, bench PSU, a live per-net overlay, a dockable TUI, braille waveform plots, a logic analyzer with UART/SPI/I²C decode, Bode/FFT, and a breadboard view.
- **Shell & scripting**: the `eesh` command language, REPL, `.eesh` scripting, and deterministic replay.
- **Formats & AI**: native project format, SPICE import/export, KiCad import, BOM/VCD/CSV/Fritzing export, and a grounded Claude copilot (offline DRC + verify loop + Design/Debug/Firmware + tutor modes).
- **Platform**: a WASM compute core + minimal browser app, a CI matrix (Linux/macOS/Windows, MSRV 1.79), aarch64 + `wasm32` builds, Criterion benches, and a bit-for-bit determinism harness.

[Unreleased]: https://github.com/ohmforge/ohmforge/compare/v0.8.1...HEAD
[0.8.1]: https://github.com/ohmforge/ohmforge/compare/v0.8.0...v0.8.1
[0.8.0]: https://github.com/ohmforge/ohmforge/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/ohmforge/ohmforge/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/ohmforge/ohmforge/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/ohmforge/ohmforge/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/ohmforge/ohmforge/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/ohmforge/ohmforge/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/ohmforge/ohmforge/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/ohmforge/ohmforge/releases/tag/v0.1.0
