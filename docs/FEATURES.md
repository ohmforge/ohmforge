# Feature catalog

A menu of what `ee-assist` does (and will do). Items are tagged with the milestone that lands them. **[`ROADMAP.md`](ROADMAP.md) is the authoritative schedule and [`plan/CHECKPOINTS.md`](plan/CHECKPOINTS.md) tracks execution** — where a tag here and the roadmap ever disagree, the roadmap wins. Legend: **[M0]** MVP · **[M1]** · **[M2]** · **[M3]** · **[M4]** stretch/vision.

---

## 1. Circuit simulation

- **[M0]** Passives & sources: resistor, capacitor, inductor, DC/AC/pulse/PWL sources, ground.
- **[M0]** DC operating point + transient analysis.
- **[M1]** Nonlinear devices: diode, LED, Zener, BJT (NPN/PNP), MOSFET (N/P), op-amp (ideal + macro-model).
- **[M1]** DC sweep, AC (Bode), parametric sweep.
- **[M2]** Noise analysis, Monte-Carlo with component tolerances, worst-case.
- **[M2]** Controlled sources (VCVS/VCCS/CCVS/CCCS), behavioral/arbitrary sources.
- **[M2]** Thermal-aware models (temperature parameter, self-heating for select parts).
- **[M1]** Convergence aids: gmin/source stepping, adaptive timestep — "why won't it converge?" surfaced in plain language.

## 2. Digital & mixed-signal

- **[M1]** Logic primitives: gates, flip-flops, latches, counters, shift registers, mux/decoder.
- **[M1]** Multi-valued logic (0/1/X/Z), configurable propagation delays.
- **[M3]** Memories (ROM/RAM), buses, tri-state.
- **[M2]** Mixed-signal boundary elements (A/D, D/A) with thresholds & drive strength.
- **[M3]** **HDL-lite**: define a custom logic block from a truth table or a small expression language.
- **[M2]** Protocol decoders on logic-analyzer channels: UART, SPI, I²C (and encode-side generators).

## 3. Microcontrollers & boards

- **[M1]** **Arduino Uno / Nano** (ATmega328P), instruction-accurate, with GPIO/timers/PWM/ADC/UART.
- **[M2]** **Arduino Mega** (ATmega2560), ATtiny family.
- **[M2]** **Raspberry Pi Pico** (RP2040, Cortex-M0+) with PIO-lite, GPIO, ADC, PWM, UART/SPI/I²C.
- **[M3]** ESP32-class (RISC-V/Xtensa subset), STM32 "blue pill"-class Cortex-M.
- **[M3]** **Specialized / custom boards**: define a board in a manifest (pinout, MCU, onboard parts) and it just works.
- **[M1]** Load prebuilt `.hex`/`.elf`; **[M2]** compile a sketch via a detected external toolchain (`arduino-cli`, `avr-gcc`, `arm-none-eabi-gcc`).
- **[M2]** **Co-simulation**: firmware drives the simulated circuit; ADC/inputs read simulated node voltages — run your real sketch before you solder.
- **[M2]** Firmware **debug view**: registers, memory, disassembly, single-step, breakpoints, pin/peripheral state.
- **[M3]** Common modules as drop-in blocks: 7-seg displays, HD44780 LCD, servos, sensors (temp/light/ultrasonic), motor drivers, WS2812 LEDs.

## 4. Virtual instruments (LabVIEW-flavored)

- **[M0]** Voltage/current **probes** on any node/branch.
- **[M1]** **Oscilloscope**: multi-channel, timebase, edge/level trigger, cursors.
- **[M1]** **Multimeter**: V/I/R/continuity/diode-test.
- **[M1]** **Function generator**: sine/square/tri/PWM/arbitrary source.
- **[M2]** **Logic analyzer** with protocol decode.
- **[M2]** **Bench power supply**: programmable, current-limited.
- **[M2]** **Bode/spectrum** analyzer; scope **math channels** (A−B, ×, FFT).
- **[M3]** **Dataflow patcher** — wire instruments + processing blocks (filters, math, logging) into a custom "virtual instrument," the LabVIEW-style graphical program, saved and reusable.
- **[M3]** **Data logging & automation**: record measurements to CSV, script test sequences, pass/fail limits.

## 5. Visualization & "seeing" circuits

- **[M0]** ASCII/Unicode **schematic view** with named nodes.
- **[M1]** **Live overlay**: color nodes by voltage, animate current direction/magnitude on wires — watch the circuit *work*.
- **[M0]** High-res **braille waveform** plots; **[M1]** stacked multi-channel.
- **[M2]** **Breadboard view**: render the circuit as a breadboard layout (educational, "what it looks like").
- **[M2]** **Bode plots**, phase, and eye-diagram-style views.
- **[M2]** Export schematic/waveform to **SVG/PNG** for sharing and docs.

## 6. The shell (`eesh`)

- **[M0]** REPL: build/wire/probe/run circuits by command; tab-completion, history, inline help.
- **[M0]** Engineering-notation values (`4k7`, `1u`, `2n2`, `1meg`) and typed units everywhere.
- **[M1]** **Scripting**: `source` a `.eesh` script; embedded scripting language for loops/params/automation.
- **[M1]** Deterministic **replay** of a session (great for tutorials and bug reports).
- **[M2]** Session **snapshots/undo**; named checkpoints.
- Full spec in [`SHELL.md`](SHELL.md).

## 7. AI copilot

- **[M1]** **Ask**: natural-language Q&A about the current circuit ("why is Q1 saturated?").
- **[M2]** **Design**: "build a 5V→3.3V regulator at 500 mA" → generates a netlist you can inspect, simulate, and tweak.
- **[M2]** **Debug**: reads sim results and points at the culprit ("R3 drops too much; try …").
- **[M2]** **Firmware gen**: "make the Uno fade the LED with PWM" → sketch you can flash into the emulator.
- **[M3]** **Explain / tutor**: step-by-step teaching mode over the live circuit.
- **[M2]** **Tool-using & verified**: the AI acts through the same API as you (add parts, run sims, read results) and **re-runs the sim to check its own answer** before presenting it.
- **[M3]** **Offline mode**: local heuristics + a bundled knowledge index for common tasks when no API key is present.
- Full design in [`AI.md`](AI.md).

## 8. Learning & content

- **[M1]** Built-in **example projects** (blink, RC filter, half-wave rectifier, 555 timer, H-bridge…).
- **[M3]** **Guided lessons**: interactive, checked steps; "change R and watch the corner frequency move."
- **[M3]** **Challenges/sandbox**: "design a circuit that meets these specs" with automatic grading via the simulator.
- **[M3]** **Glossary & datasheet snippets** surfaced in context; hover a part → what it is and how it behaves.

## 9. Interop & files

- **[M0]** Native project format (human-readable, diff-friendly, git-able).
- **[M1]** **SPICE netlist** import/export (talk to the wider ecosystem).
- **[M3]** **KiCad** netlist import; **BOM** export (CSV).
- **[M2]** Waveform export: **CSV** and **VCD** (view in GTKWave etc.).
- **[M3]** Fritzing-style export for the breadboard view.

## 10. Platform & performance

- **[M0]** Runs on Linux/macOS/Windows terminals and over SSH.
- **[M1]** Static single-binary builds; **aarch64** builds for Raspberry Pi / Apple Silicon.
- **[M2]** **WASM** build: the same simulator in a browser.
- **[M0+]** Benchmarked hot paths; interactive on modest hardware; deterministic & reproducible runs.
