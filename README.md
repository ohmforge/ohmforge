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

## What it feels like

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

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — how it's built: crates, the simulation core, co-simulation, TUI.
- [`docs/FEATURES.md`](docs/FEATURES.md) — the full feature catalog (simulation, boards, instruments, AI, education).
- [`docs/SHELL.md`](docs/SHELL.md) — the `eesh` command language and scripting.
- [`docs/AI.md`](docs/AI.md) — the AI copilot design (tools, prompts, guardrails, offline mode).
- [`docs/ROADMAP.md`](docs/ROADMAP.md) — phased milestones from MVP to v1 and beyond.

## Status

🚧 **Planning / scaffold.** The workspace layout and design are in place; implementation follows the roadmap. See [`docs/ROADMAP.md`](docs/ROADMAP.md).

## License

TBD (leaning MIT/Apache-2.0 dual, standard for the Rust ecosystem).
