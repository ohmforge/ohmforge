# ee-assist

**A fast, friendly, AI-assisted electrical-engineering playground that lives in your terminal.**

`ee-assist` is a terminal-native EE workbench for tinkerers, students, and makers. It combines:

- a **circuit simulator** (analog SPICE-style + digital logic + mixed-signal),
- **microcontroller & board emulation** (Arduino/AVR, Raspberry Pi Pico/RP2040, Cortex-M, RISC-V, plus specialized boards),
- **virtual instruments** in the spirit of LabVIEW (oscilloscope, multimeter, logic analyzer, function generator, power supply),
- a **dedicated shell** (`eesh`) — a scriptable REPL for building, wiring, probing, and running circuits,
- and an **AI copilot** that can design circuits, generate firmware, explain what's happening, and debug alongside you.

It's written in **Rust**, runs as a **TUI** (works great over SSH and inside emulators), and is built to be **small, portable, and extremely fast** — including a **WebAssembly** build that runs in a browser.

> Think: a breadboard + oscilloscope + a patient EE tutor, that boots in a terminal anywhere.

---

## Why

Real EE tools are powerful but heavy, GUI-bound, and often expensive. Tinkerers want something they can:

- launch instantly, anywhere (laptop, Raspberry Pi, remote box, container, browser),
- drive from the keyboard and script,
- learn from — *see* current flow, voltages, and waveforms, and ask "why?",
- use to run **real Arduino sketches** against a **simulated circuit** before touching hardware.

## Try it now

What runs today: a real analog/mixed-signal simulator, AVR + RP2040 emulation with co-simulation, instruments, an AI copilot, and a browser build — ~500 tests, all green.

```text
cargo run -p ee -- demo     # build an RC low-pass, run a transient, see the braille scope
cargo run -p ee -- bode     # AC sweep → Bode plot, −3 dB at 1 kHz
cargo run -p ee -- amp      # common-emitter BJT amplifier, op-point with a color overlay
cargo run -p ee -- cosim    # real AVR firmware drives D13 → a simulated LED lights (19.4 mA)
cargo run -p ee -- view     # a static snapshot of the composed TUI (overlay · scope · shell)
cargo run -p ee -- tui      # the live interactive dockable panels
cargo run -p ee             # the eesh REPL;  or:  cat examples/tutorial/rc_lowpass.eesh | cargo run -p ee

# in the browser (WebAssembly):
wasm-pack build crates/ee-web --target web --out-dir www/pkg   # then serve crates/ee-web/www/
```

New here? Start with the **[getting-started tutorial](docs/tutorials/getting-started.md)**.

## What it feels like (design target)

The vision the shell is growing toward — not all of this grammar is wired yet (see [`docs/SHELL.md`](docs/SHELL.md) § "Implemented today"):

```text
eesh> new project blinky
eesh> board add uno                      # Arduino Uno (ATmega328P)
eesh> add led D1 anode=uno.D13 cathode=n1
eesh> add resistor R1 220 n1 gnd
eesh> flash uno examples/blink.ino       # compile + load firmware
eesh> run 3s                             # co-simulate MCU + circuit
eesh> scope uno.D13 D1.current           # live waveforms in the terminal
eesh> ask "why is the LED dimmer than I expected?"
```

```
 ┌─ Schematic ───────────────┐ ┌─ Scope ───────────────────────┐
 │  UNO.D13 ──[R1 220Ω]──┐    │ │ D13 ┌─┐   ┌─┐   ┌─┐   ┌─┐      │
 │                      (D1)  │ │ ────┘ └───┘ └───┘ └───┘ └──    │
 │                       │    │ │ I    ▁▇█▇▁   ▁▇█▇▁   ▁▇█▇▁     │
 │                      GND   │ │                                │
 └───────────────────────────┘ └────────────────────────────────┘
 ┌─ eesh ────────────────────────────────────────────────────────┐
 │ ▸ ask "why is the LED dimmer than I expected?"                 │
 │ AI: Your series resistor limits current to ~14 mA. The Uno's   │
 │     pin sources through an internal ~25Ω; try R1=150Ω for ...  │
 └────────────────────────────────────────────────────────────────┘
```

## Documentation

**[`docs/README.md`](docs/README.md) is the documentation hub.** Highlights:

- **Tutorial:** [getting started in `eesh`](docs/tutorials/getting-started.md) · runnable [`examples/`](examples/README.md) (analog circuits, MCU/co-sim, formats)
- **Guides:** [simulation engine](docs/guide/simulation.md) · [emulation & co-sim](docs/guide/emulation.md) · [the view & instruments](docs/guide/view.md) · [AI copilot](docs/guide/ai-copilot.md) · [file formats](docs/guide/formats.md)
- **Design:** [`ARCHITECTURE.md`](docs/ARCHITECTURE.md) · [`FEATURES.md`](docs/FEATURES.md) · [`SHELL.md`](docs/SHELL.md) · [`AI.md`](docs/AI.md) · [`ROADMAP.md`](docs/ROADMAP.md) · progress ledger [`docs/plan/CHECKPOINTS.md`](docs/plan/CHECKPOINTS.md)

## Status

**M0–M6 complete** (84/84 checkpoints, **v0.4.0**; see [`docs/plan/CHECKPOINTS.md`](docs/plan/CHECKPOINTS.md)). Working today: linear + nonlinear analog (diode with `IS/N/RS`, BJT/MOSFET, Newton–Raphson with **gmin/source-stepping convergence aids**), **mutual inductance / transformers**, AC/Bode, noise, Monte-Carlo, transient with adaptive timestep, a **KLU-class ordered/reusable sparse solver wired into the engine's hot loop** (+ multithreaded analyses), expression/behavioral sources + temperature; an event-driven digital kernel + **HDL-lite** (truth-table/ROM/RAM/tri-state); instruction-accurate **AVR (328P/2560/ATtiny)**, **RP2040 (Cortex-M0+)** and **STM32 (Cortex-M3/M4, Thumb-2)** cores with peripherals (GPIO/timers/PWM/ADC/USART/**SPI/I²C**/**external + pin-change interrupts**) and **lockstep co-simulation** — including a **closed loop** where firmware reads a real analog node through the ADC and drives a pin back — plus manifest boards, drop-in modules (7-seg/servo/WS2812), and composite device subcircuits (op-amp/switch/relay); oscilloscope/DMM/funcgen/PSU, a logic analyzer with UART/SPI/I²C decode, a Bode/FFT panel, a breadboard view, a **dataflow patcher**, and **live schematic editing + component tuning with current-flow animation**; a dockable TUI; SPICE (incl. `.model` devices)/KiCad/VCD/CSV/Fritzing/BOM and a lossless native project format; an AI copilot (Ask/Design/Debug/Firmware + offline mode) whose tools drive a real sim with a verification loop and **spec-driven auto-tune**, and a tutor with graded challenges; and a **WebAssembly browser build** with an editor, shareable-session links, and **real-time co-editing**. Whole workspace stays green — `clippy -D warnings`, `wasm32` core + `ee-web`, `-D warnings` docs.

## License

Dual-licensed under **MIT OR Apache-2.0**.
