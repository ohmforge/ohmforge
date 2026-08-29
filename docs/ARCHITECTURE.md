# Architecture

`ee-assist` is a Cargo **workspace** of focused crates. The design goals, in priority order:

1. **Fast & small** — real-time interactivity on modest hardware; a static binary a few MB in size.
2. **Portable** — runs on Linux/macOS/Windows, ARM SBCs, over SSH, and in the browser via WASM.
3. **Decoupled core** — the simulation engine has no I/O and no UI dependency, so it runs in the TUI, headless (CI/scripts), or WASM unchanged.
4. **Deterministic & reproducible** — same project + same seed → same result. Essential for teaching, tests, and AI verification.

```
                       ┌───────────────────────────────────────────┐
                       │                  ee (bin)                   │
                       │   app orchestration, config, session state  │
                       └───────┬───────────────┬───────────────┬─────┘
                               │               │               │
                    ┌──────────▼───┐   ┌────────▼──────┐  ┌─────▼──────┐
                    │   ee-tui     │   │   ee-shell    │  │   ee-ai    │
                    │ (ratatui)    │   │   (eesh)      │  │  (copilot) │
                    │ schematic,   │   │ REPL, script, │  │ tools,     │
                    │ scope, panes │   │ completion    │  │ RAG, LLM   │
                    └──────┬───────┘   └───────┬───────┘  └─────┬──────┘
                           │                   │                │
                           └─────────┬─────────┴────────┬───────┘
                                     │                   │
                          ┌──────────▼─────────┐  ┌──────▼───────────┐
                          │   ee-instruments   │  │     ee-fmt       │
                          │ scope, DMM, funcgen│  │ project I/O,     │
                          │ logic analyzer, PSU│  │ SPICE/KiCad io   │
                          └──────────┬─────────┘  └──────┬───────────┘
                                     │                   │
        ┌───────────────┬───────────┴──────┬────────────┴───────┐
        │               │                  │                    │
 ┌──────▼──────┐ ┌──────▼──────┐   ┌────────▼───────┐   ┌────────▼───────┐
 │  ee-sim     │ │ ee-digital  │   │    ee-mcu      │   │   ee-devices   │
 │ analog MNA, │ │ event-driven│   │ MCU cores +    │   │ device models, │
 │ transient,  │ │ logic,      │   │ peripherals,   │   │ board defs,    │
 │ AC/DC/noise │ │ HDL-lite    │   │ co-sim bridge  │   │ part library   │
 └──────┬──────┘ └──────┬──────┘   └────────┬───────┘   └────────┬───────┘
        └───────────────┴─────────┬─────────┴────────────────────┘
                                  │
                          ┌───────▼────────┐
                          │    ee-core     │
                          │ netlist graph, │
                          │ units, values, │
                          │ time, events   │
                          └────────────────┘
```

Everything below `ee-instruments` is **pure compute, I/O-free, and WASM-targetable** — no filesystem, network, clock, or terminal access — which is what makes the WASM build and headless mode trivial. It targets `std` (the float math and collections rely on it, and `wasm32` ships `std`); embedded `no_std` is an explicit non-goal, not a hidden guarantee. See [Cross-cutting design decisions](#cross-cutting-design-decisions).

---

## Crates

| Crate | Responsibility |
|---|---|
| `ee-core` | The shared data model: the circuit graph (nodes/branches/nets), physical **units & quantities** (typed volts/amps/ohms/farads…), simulation time, the event queue, engineering-notation value parsing (`4k7`, `1u`, `2n2`). No solver, no I/O. |
| `ee-sim` | Analog / mixed-signal engine: **Modified Nodal Analysis**, sparse linear solve, Newton–Raphson for nonlinear devices, numerical integration (Backward Euler, Trapezoidal, Gear) for **transient**, plus **DC op-point, DC sweep, AC small-signal, noise, Monte-Carlo, parametric sweep**. |
| `ee-digital` | Event-driven digital logic (gates, flip-flops, counters, memories, buses) with propagation delays and multi-valued logic (`0/1/X/Z`). Optional **HDL-lite** for defining custom logic blocks. |
| `ee-mcu` | Instruction-accurate microcontroller cores and their peripherals; the **co-simulation bridge** that maps MCU pins onto sim nodes. Internal modules: `avr` (ATmega/ATtiny), `cortexm` (RP2040/STM32-class), `riscv` — one crate so the cores share the peripheral/bus and co-sim scaffolding; split into sub-crates only if compile time demands it. |
| `ee-devices` | Concrete device models (R, L, C, sources, diode, BJT, MOSFET, op-amp, common ICs) and **board definitions** (Arduino Uno/Nano/Mega, RPi Pico, ESP32-class, custom boards) + a searchable **parts library**. |
| `ee-instruments` | Virtual instruments layered over the engines: oscilloscope, multimeter, function generator, logic analyzer, bench power supply, spectrum/Bode view. LabVIEW-style **virtual front panels**. |
| `ee-fmt` | File formats & interop: native project format, **SPICE netlist** import/export, **KiCad** netlist read, Fritzing/CSV BOM export, waveform export (CSV/VCD). |
| `ee-tui` | The terminal UI (`ratatui` + `crossterm`): schematic renderer, high-res **braille** waveform plots, dockable panels, mouse placement, themes, help overlay. |
| `ee-shell` | `eesh`: the command language, parser, tab-completion, history, and an embedded scripting engine for automation. |
| `ee-ai` | The AI copilot: LLM client, **tool/function definitions** that let the model manipulate the circuit and read results, prompt templates, RAG over the parts/datasheet index, and offline heuristics. |
| `ee` | The binary: wires crates together, owns session/project state, config, and the event loop. |

### Why a workspace

Each crate compiles, tests, and benchmarks independently. The heavy math (`ee-sim`) can be fuzzed and property-tested in isolation; the TUI can be swapped or embedded; the core can target `wasm32-unknown-unknown` without dragging in a terminal backend.

---

## The simulation core (`ee-sim`)

The heart of the "emulator." A circuit is a graph; simulation solves it over time.

### Formulation — Modified Nodal Analysis (MNA)

We build a system `A·x = z` where `x` holds unknown node voltages (and some branch currents). Each device *stamps* its contribution into `A` and `z`:

- **Linear devices** (R, C, L, independent/controlled sources) stamp constant/companion values.
- **Reactive devices** (C, L) are discretized each timestep into a **companion model** (a conductance + current source) via the chosen integration rule.
- **Nonlinear devices** (diode, BJT, MOSFET) are linearized at the operating point; we iterate with **Newton–Raphson** until convergence, re-stamping each iteration.

### Solver

- Sparse matrix representation; **LU factorization** with partial pivoting. Start with a dependable dense/`faer`-backed path, then move hot circuits to a true sparse solver (KLU-style) as size grows.
- Adaptive timestep (LTE-controlled) for transient; step limiting and `gmin`/source stepping to aid convergence — the classic SPICE robustness tricks.

### Analyses

| Analysis | What it does |
|---|---|
| **DC op-point** | Steady-state voltages/currents (the starting point for everything). |
| **DC sweep** | Sweep a source/parameter, record the response. |
| **Transient** | Time-domain: the scope's bread and butter. |
| **AC** | Small-signal frequency response (Bode magnitude/phase). |
| **Noise** | Output noise spectral density. |
| **Monte-Carlo / worst-case** | Vary component tolerances, characterize spread. |
| **Parametric sweep** | Re-run any analysis across a parameter grid. |

Results stream out as typed **signals** (time/frequency series) that instruments and the TUI consume without knowing solver internals.

### Digital & mixed-signal (`ee-digital`)

Analog MNA is expensive for pure logic. `ee-digital` is an **event-driven** simulator: signals change only at events, gates schedule future events after their propagation delay. Mixed-signal circuits are partitioned; **A/D and D/A boundary elements** translate between a logic level and a voltage/current (with configurable thresholds, drive strength, rise/fall). This is what lets an MCU's fast digital pins interact cheaply with a slow analog subcircuit.

---

## Board & MCU emulation (`ee-mcu`)

The "run real firmware against a simulated circuit" capability — the thing that makes this genuinely useful to Arduino/Pi tinkerers.

### Cores

- **AVR** (`avr` module): instruction-accurate ATmega328P (Uno/Nano), ATmega2560 (Mega), ATtiny — decoder, register file, SRAM/flash/EEPROM, interrupt controller.
- **Cortex-M** (`cortexm` module): the **RP2040 (Raspberry Pi Pico)** is a **Cortex-M0+ (ARMv6-M, Thumb subset)** — start there; STM32-class parts (Cortex-M3/M4, ARMv7-M/Thumb-2) extend the decoder later.
- **RISC-V** (`riscv` module): RV32I(MC) for ESP32-C / modern boards.

### Peripherals

Timers/PWM, GPIO, ADC, USART/UART, SPI, I²C, external interrupts, watchdog — modeled per board. Peripherals are what the firmware actually talks to, and what connects to the circuit.

### Co-simulation bridge

**Crate boundary (important):** `ee-mcu` defines the *bridge surface* — the `McuCore` trait and the pin↔net mapping — but it does **not** depend on `ee-sim`. The lockstep **driver loop** below lives in the `ee` binary, the one place that legitimately depends on both `ee-sim` and `ee-mcu`. This keeps the dependency DAG acyclic (`ee-mcu` and `ee-sim` are siblings, neither importing the other) and lets each be tested in isolation with a stub on the other side.

The crux. Two clocks — the MCU's instruction clock and the analog/digital sim's timestep — advance in lockstep:

```
loop:
  t_next = min(next MCU event, next sim event, next scheduled sample)
  advance MCU to t_next        # executes instructions, updates peripheral state
  push GPIO/PWM/DAC states  ──►  sim nodes (as digital events or voltage sources)
  advance sim to t_next        # solves the circuit
  read node voltages        ──►  MCU ADC / input pins
```

GPIO pins become nodes in the mixed-signal graph; a `digitalWrite(HIGH)` becomes a driven logic event, an `analogRead()` samples a node voltage through the ADC model. This synchronization + the event-driven digital path is what keeps co-simulation real-time.

### Firmware loading

- Accept prebuilt `.hex`/`.elf`.
- Optional **compile step**: shell out to a detected toolchain (`avr-gcc`/`arduino-cli`, `arm-none-eabi-gcc`) to build a sketch → firmware. Toolchains stay external (keeps the binary small); ee-assist orchestrates.

### Raspberry Pi (full SBC) note

A full Linux-class Pi is out of scope for the native emulator (that's QEMU's job). Two supported paths: (1) **emulate the Pico/RP2040** natively and accurately — the common maker target; (2) for a full Pi, model the **GPIO/peripheral boundary** so Pi-side code (or a QEMU-hosted image) drives the simulated circuit through a documented bridge. This gives the "Raspberry Pi things" experience without shipping a kernel emulator.

---

## Virtual instruments (`ee-instruments`) — the LabVIEW feel

Instruments are consumers of signals + controllers of sources, presented as **front panels** in the TUI:

- **Oscilloscope** — multi-channel, triggering (edge/level), timebase, cursors, math channels (A−B, FFT).
- **Multimeter (DMM)** — V/I/R/continuity at a probe point.
- **Function generator** — sine/square/tri/PWM/arbitrary as a source into the circuit.
- **Logic analyzer** — multi-bit digital capture with protocol decode (UART/SPI/I²C).
- **Bench power supply** — programmable voltage/current-limited sources.
- **Bode/spectrum analyzer** — from AC/noise analyses.

Longer-term, a **dataflow patcher** (LabVIEW-style "wire instruments and processing blocks together") composes these into custom virtual benches — see [`FEATURES.md`](FEATURES.md).

---

## The TUI (`ee-tui`)

- **`ratatui` + `crossterm`** for a portable, dependency-light terminal UI that works over SSH.
- **Schematic rendering**: Unicode box-drawing for wires + ASCII component glyphs; a "flow" overlay animates current direction and colors nodes by voltage so you can *see* the circuit working.
- **Waveforms**: **braille** sub-cell plotting for high vertical resolution; multiple stacked channels, cursors, autoscale.
- **Layout**: dockable/resizable panels (schematic, scope, shell, AI chat, parts palette, instrument panels), a command palette, and context help.
- **Input**: keyboard-first, with mouse support for placing/moving parts and dragging wires.

Rendering is decoupled from simulation via a snapshot/observer channel, so a slow redraw never stalls the solver and vice-versa.

---

## Portability & performance

- **Pure Rust**, minimal dependencies; the compute core is I/O-free and `wasm32`-clean (`std`, but no OS/filesystem/network/clock).
- **Static builds** (musl on Linux) → a single small binary; cross-compiled for `aarch64` (Raspberry Pi, Apple Silicon).
- **WASM** (`wasm32-unknown-unknown`): the core + a canvas/xterm front end run the same simulator in a browser — great for the "share a circuit, learn in-page" story.
- **Determinism**: fixed-seed RNG for Monte-Carlo; the same project reproduces bit-for-bit, enabling golden-file tests and letting the AI *verify* its own suggestions by re-running.
- **Benchmarks** (`criterion`) gate the hot paths (matrix stamp/solve, event queue, co-sim step); target interactive frame rates for small/medium circuits and near-real-time MCU co-sim.

---

## Cross-cutting design decisions

These are the project-wide conventions that fell out of the scaffold-hardening review. They bind every crate; a change here is a cross-track event (see [`plan/README.md`](plan/README.md)).

| Concern | Decision | Rationale |
|---|---|---|
| **`Signal` / `Unit` home** | Both live in **`ee-core`**, not `ee-sim`. | Instruments, TUI, AI, and fmt consume signals without depending on the solver — keeps the DAG acyclic and the WASM surface tiny. |
| **Error handling** | Libraries return typed errors via **`thiserror`** (one `enum` per crate, e.g. `SimError`, `McuError`). The `ee` binary and shell use **`anyhow`** at the top edge for context-rich reporting. No `unwrap`/`panic!` on user-reachable paths. | Callers can match on failure kinds; the app layer gets ergonomic `?` + context. |
| **Panic strategy** | Release profile uses **`panic = "unwind"`**, not `abort`. | The TUI installs a panic hook to restore the terminal; the AI verify-loop applies patches on a scratch circuit and must not take the process down on a solver panic. `abort` foreclosed both. (This reverses the scaffold's initial `panic = "abort"`.) |
| **Logging / diagnostics** | **`tracing`** with an env-filtered subscriber, initialized only in the `ee` binary. The compute core emits **no** logs (it takes no I/O); it returns structured diagnostics instead. | Keeps the core WASM-clean and deterministic; the app decides where logs go. |
| **Config** | Layered: built-in defaults → config file (`~/.config/ee-assist/config.toml`) → env (`EE_*`) → CLI flags. Parsed with **`serde`** + a small loader in `ee`. **API keys never live in config committed to disk** — read from env/keyring at runtime (see [`AI.md`](AI.md)). | Predictable precedence; secrets stay out of the repo and project files. |
| **Serialization** | **`serde`** everywhere; the native project format is **versioned** (a `schema_version` field) and human-readable/diff-friendly (`ee-fmt` owns it). | One derive path for project I/O, config, and AI tool payloads; forward-compat via the version tag. |
| **Determinism** | The compute core contains **no wall-clock and no ambient `rand`**. Monte-Carlo and any stochastic step take an explicit **seed**; time comes from the simulation clock, never the OS. | Golden-file tests reproduce bit-for-bit; the AI can re-run and verify. |
| **AI transport (WASM-safe)** | `ee-ai` talks to the model through a **`Transport` trait** (`async fn send(...)`), not a hard-wired HTTP client. Native builds use a `reqwest` impl; `wasm32` uses a `fetch` impl; tests use a **mock** transport with canned responses. | Lets the copilot logic be unit-tested with no network and compile for the browser without dragging a native TLS stack into WASM. |
| **Shell / AI → engine access** | Neither the shell nor the AI calls the solver directly. Both **mutate the `ee-core` graph** and **submit an `Analysis` request** (the `ee-sim` enum); the `ee` dispatcher runs it and returns `Signal`s. The AI emits the **`Command` AST** the shell parses — "ask for it" lowers to the exact same path as "type it". | Single source of truth; the AI can't do anything a user couldn't, and every action is inspectable/replayable. |

## Data flow at runtime

```
user ──► eesh command / TUI action / AI tool-call
        │
        ▼
   ee-core mutates the project graph  (single source of truth)
        │
        ▼
   ee-sim / ee-digital / ee-mcu run the requested analysis
        │
        ▼
   typed Signals + node/branch state  ──►  ee-instruments  ──►  ee-tui render
                                       └──►  ee-ai (reads results to explain/debug)
                                       └──►  ee-fmt (export CSV/VCD/report)
```

The project graph in `ee-core` is the **single source of truth**; the shell, the TUI, and the AI all mutate the *same* model through the same API. That's what lets "type it," "click it," and "ask for it" stay perfectly consistent.
