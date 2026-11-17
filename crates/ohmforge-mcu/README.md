# ohmforge-mcu — instruction-accurate MCU cores + lockstep co-sim

Microcontroller cores for [ohmforge](https://github.com/ohmforge/ohmforge),
their peripherals, and the **co-simulation bridge** that maps MCU pins onto
circuit nodes so real firmware runs against a simulated circuit. Cores execute
whole instructions ("instruction-accurate") and track cycle counts, so the
lockstep loop in the `of` binary can advance an MCU and the analog/digital
engines together in simulated time.

Targeted cores: AVR (ATmega328P / Arduino Uno + Nano, ATmega2560, ATtiny),
Cortex-M (RP2040 / Raspberry Pi Pico, STM32-class), and Xtensa (ESP32). The
cores are **pure computation** — no wall-clock, no randomness, no I/O — so a run
is deterministic and the crate builds for `wasm32-unknown-unknown`. The core
knows nothing about the analog engine: it reads input voltages and writes pin
drives through a `PinIo`, and the co-sim loop reconciles those with the circuit
graph.

## Usage

```rust
use ohmforge_mcu::avr::board::Uno;

// A reset Arduino Uno (ATmega328P) clocked at 16 MHz.
let mut uno = Uno::new();

// Load firmware — raw binary or Intel HEX — then run it.
let firmware: &[u8] = include_bytes!("blink.hex");
uno.load_firmware(firmware).expect("valid image");

// Advance the core to 100 ms of simulated time, executing whole instructions.
uno.step_to(0.100);

// Read Arduino digital pin 13 (the on-board LED, PB5).
let led_on = uno.digital_read(13);
println!("D13 = {}", if led_on { "HIGH" } else { "LOW" });
```

For co-simulation, exchange pin state each step with `uno.pin_io()` and
`uno.step_with_io(&mut io)`, mapping pins to circuit nodes via
`Uno::digital_pin_id(..)` / `Uno::analog_pin_id(..)`.

---

Part of [**ohmforge**](https://github.com/ohmforge/ohmforge) — the AI-native
circuit & firmware workbench.

License: **Apache-2.0**.
