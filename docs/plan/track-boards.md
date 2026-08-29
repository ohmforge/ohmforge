# Track design: **boards** (`ee-digital`, `ee-mcu`, `ee-devices`)

> The "run real firmware against a simulated circuit" track — what makes ee-assist genuinely useful to Arduino/Pi tinkerers. Depends on `sim-core` for the graph, the shared clock, and (via the `ee` binary) the analog solver.
>
> Checkpoints: `CP-BRD-M1.1 … CP-BRD-M2.5`. Contracts: [`README.md`](README.md#inter-track-contracts). Cross-cutting rules: [`../ARCHITECTURE.md`](../ARCHITECTURE.md#cross-cutting-design-decisions).

## Scope

| Crate | Owns |
|---|---|
| `ee-digital` | Event-driven digital logic: gates, flip-flops, counters, buses, `0/1/X/Z` values, propagation delays; the reusable **event queue** driver. |
| `ee-mcu` | Instruction-accurate MCU cores + peripherals, and the **bridge surface** (the `McuCore` trait + pin↔net mapping). One crate; internal modules `avr`, `cortexm`, `riscv`. |
| `ee-devices` | Concrete device models (R/L/C, diode, BJT, MOSFET, op-amp, common ICs), **board definitions** (Uno/Nano/Mega, Pico, ESP32-class, custom), and the searchable **parts library**. |

**Dependency discipline (from the architecture review):** `ee-mcu` depends on `ee-core` and `ee-digital`, but **not** on `ee-sim`. The lockstep co-sim *loop* lives in the `ee` binary — the only place that legitimately links both engines. This keeps `ee-sim` and `ee-mcu` siblings and independently testable.

---

## `ee-digital` — event-driven kernel

Analog MNA is far too expensive for pure logic, so digital runs on an **event-driven** model: a signal changes only at scheduled events; a gate, on an input change, schedules its output change one propagation delay in the future.

```
kernel:
  pop earliest event (from the ee-core event heap)
  apply the net's new level
  for each gate sensitive to that net:
      compute new output; if changed, schedule at t + delay
  repeat until the heap is empty or t reaches the horizon
```

- **Values:** `Logic { Zero, One, X, Z }` — X = unknown/contention, Z = high-impedance (needed for buses/tri-state at M3).
- **Primitives (`CP-BRD-M1.2`):** gates (AND/OR/NOT/XOR/NAND/NOR), D and JK flip-flops, latches, counters, shift registers, mux/decoder. Each is a small state machine that schedules events.
- *Gates:* ring oscillator produces the expected period from gate delays (`CP-BRD-M1.1`); truth tables + a counter sequence (`CP-BRD-M1.2`).

The event heap is the `ee-core::time` primitive so the analog transient loop and the digital kernel share one clock — essential for mixed-signal and co-sim.

---

## `ee-mcu` — cores & peripherals

### The `McuCore` trait (bridge surface)

```rust
pub trait McuCore {
    fn step_to(&mut self, t: Seconds, io: &mut PinIo);  // execute until sim time t
    fn next_event(&self) -> Seconds;                    // next timer/interrupt edge
    fn pins(&self) -> &[PinId];
    fn load_firmware(&mut self, image: FirmwareImage) -> Result<(), McuError>;
}
```

`PinIo` is the only thing the core and the outside world exchange: digital drives out, sampled voltages in. The core knows nothing about MNA — it just reads/writes pin state, which the `ee` co-sim loop reconciles with the graph.

### AVR (`avr` module) — `CP-BRD-M1.3`, `M1.4`, `M1.5`

- **ATmega328P** first (Uno/Nano): instruction decoder, 32-register file, SRAM/flash/EEPROM, interrupt controller. Instruction-accurate; cycle-accuracy only where it matters (timers/PWM).
- **Peripherals:** GPIO (DDR/PORT/PIN), Timer0/1/2 with PWM, ADC, USART/UART, SPI, I²C (TWI), external interrupts, watchdog.
- **Firmware:** load `.hex`/`.elf`. *Gate:* `blink.hex` toggles D13 at ~1 Hz.
- Validate against published **AVR instruction test-vectors** (`CP-BRD-M1.3`) and measured PWM duty / decoded UART bytes (`CP-BRD-M1.4`).
- Later: ATmega2560 (Mega) and ATtiny variants share the decoder with a different peripheral map (`CP-BRD-M2.4`).

### Cortex-M (`cortexm` module) — `CP-BRD-M2.3`

The **RP2040 (Raspberry Pi Pico) is a Cortex-M0+ — ARMv6-M, a Thumb subset** (not full Thumb-2). Start there: implement the ARMv6-M instruction set, the RP2040 peripherals (GPIO, ADC, PWM, UART/SPI/I²C, and a PIO-lite model). STM32-class parts (Cortex-M3/M4, ARMv7-M/Thumb-2) extend the decoder afterward. *Gate:* ARMv6-M test-vectors; Pico blink toggles a pin.

### RISC-V (`riscv` module) — later

RV32I(MC) for ESP32-C / modern boards. Same `McuCore` shape.

---

## Co-simulation bridge — the crux (loop lives in `ee`)

Two clocks advance in lockstep to the nearest event:

```
loop:
  t_next = min(core.next_event(), sim.next_event(), next sample)
  core.step_to(t_next, &mut io)      # runs instructions, updates peripherals
  push io.digital_out / PWM / DAC  ─► sim nodes (as logic events or voltage sources)
  sim.advance_to(t_next)             # solve the circuit
  read node voltages               ─► io.analog_in  (feeds ADC / input pins)
```

`digitalWrite(HIGH)` → a driven logic event on that node; `analogRead()` → samples a node voltage through the ADC model. Because the digital path is event-driven (not MNA), this stays near-real-time.
*Gate (`CP-BRD-M2.1`):* `analogRead` returns a divider's voltage; `digitalWrite` lights a sim LED.

### Mixed-signal boundary elements (`CP-BRD-M2.2`)

A/D and D/A boundary elements translate between a logic level and a voltage/current, with configurable **thresholds**, **drive strength**, and **rise/fall** times — the glue that lets a fast digital pin talk to a slow analog subcircuit correctly.

### External toolchains (`CP-BRD-M2.5`)

Compiling a sketch is **delegated** to a detected external toolchain (`arduino-cli`, `avr-gcc`, `arm-none-eabi-gcc`) — kept out of the binary to stay small. ee-assist orchestrates: detect → invoke → collect the hex → load. *Gate:* `.ino` → hex → run end-to-end, **skipped cleanly if the toolchain is absent** (so CI without a toolchain stays green).

---

## `ee-devices` — models, boards, parts

- **Device models** implement the stamping/nonlinear interfaces from `ee-sim` (this is the one place `ee-devices` leans on the sim contracts). R/L/C and sources at M0-adjacent; diode/BJT/MOSFET/op-amp at M1.
- **Board definitions** are **manifests** (pinout + MCU + onboard parts), so a "specialized/custom board" (`M3`) is data, not code: describe the pins and onboard components and it works.
- **Parts library:** a searchable index the shell/AI query ("a 3.3 V LDO at 500 mA").

```toml
# example board manifest (sketch)
[board]
name = "uno"; mcu = "atmega328p"; clock_hz = 16_000_000
[[pin]]  name = "D13"; port = "PB5"; onboard = "led:L"
[[pin]]  name = "A0";  adc_channel = 0
```

---

## Testing strategy

- **Test-vector suites** for each core (reuse published AVR/ARMv6-M vectors) — the fastest path to trustworthy instruction accuracy.
- **Reference-sketch validation:** run known Arduino sketches, compare pin behavior to documented/real results.
- **Deterministic co-sim:** golden traces of pin+node state over time; no wall-clock, seeded where stochastic.
- Toolchain-dependent tests are **feature/skip-gated** so the default `cargo test --workspace` needs no external compiler.

---

## State & next action

- **Depends on:** `CP-SIM-M0.1`/`M0.3`/`M0.5` (graph + op-point + `Signal`) for the analog side of co-sim; the digital kernel can start against `ee-core` alone.
- **Current checkpoint:** `CP-BRD-M1.1` — the event-driven digital kernel (event heap reuse from `ee-core::time`, `0/1/X/Z`, delays).
- **Immediate next step:** implement the kernel + a ring-oscillator test (the M1.1 gate), then `CP-BRD-M1.2` primitives, before starting the AVR decoder at `CP-BRD-M1.3`. Keep `ee-mcu` free of any `ee-sim` dependency.
