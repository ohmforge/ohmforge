# MCU co-simulation examples

Worked, runnable examples of ohmforge's microcontroller emulation: real AVR and
RP2040 firmware driving simulated pins and peripherals. The full narrative — the
cores, firmware loading, peripherals, pin mapping, the `PinIo` bridge, the
mixed-signal boundary and the lockstep co-sim loop — is in
[`docs/guide/emulation.md`](../../docs/guide/emulation.md).

Each example is a standalone Rust program under
[`crates/ohmforge-mcu/examples/`](../../crates/ohmforge-mcu/examples/) (that is where Cargo
runs examples from). Run one with:

```sh
cargo run -p ohmforge-mcu --example <name>
```

The output shown below is verified — it was produced by running the command.

| Example | What it shows |
|---------|---------------|
| [`uno_blink`](../../crates/ohmforge-mcu/examples/uno_blink.rs) | Load the committed `blink.hex` onto an Arduino Uno; D13 (PB5) toggles at ~1 Hz. |
| [`cosim_led`](../../crates/ohmforge-mcu/examples/cosim_led.rs) | The co-sim loop: firmware drives D13 → the mixed-signal boundary → a measured LED current. Mirrors `of cosim`. |
| [`pwm_duty`](../../crates/ohmforge-mcu/examples/pwm_duty.rs) | Timer0 Fast-PWM on OC0A (PD6) at a measured 25 % duty. |
| [`uart_tx`](../../crates/ohmforge-mcu/examples/uart_tx.rs) | USART0 transmitting a byte stream at 9600 baud, captured. |
| [`pico_blink`](../../crates/ohmforge-mcu/examples/pico_blink.rs) | RP2040 (Raspberry Pi Pico) ARMv6-M firmware toggling GP25 through the SIO. |

## `uno_blink`

Loads the hand-assembled Intel-HEX fixture
[`crates/ohmforge-mcu/tests/blink.hex`](../../crates/ohmforge-mcu/tests/blink.hex) (the
assembly → HEX walkthrough is in the guide) and, with the Uno clocked at 1 MHz,
measures the D13 toggle interval.

```sh
$ cargo run -p ohmforge-mcu --example uno_blink
Arduino Uno blink — D13 (PB5) toggling at ~1 Hz (clock = 1 MHz)

  edge: D13 -> HIGH  at cycle         4  (t = 0.000 s)
  edge: D13 -> low   at cycle   1000107  (t = 1.000 s)
  edge: D13 -> HIGH  at cycle   2000210  (t = 2.000 s)

Measured toggle interval: 1000103 cycles = 1.000 s (≈ 1 Hz, half-period).
```

## `cosim_led`

Plays the lockstep loop by hand: tiny firmware makes D13 an output and drives it
high; the pin's drive becomes a Thévenin source (5 V behind 40 Ω) that feeds a
220 Ω + LED load, and the example reports the delivered voltage and current.
`of cosim` in the `of` binary does the same with the full nonlinear analog solver.

```sh
$ cargo run -p ohmforge-mcu --example cosim_led
Co-simulation: AVR firmware drives D13 → a simulated LED.
  driver: 5 V push-pull, 40 Ω source R;  load: 220 Ω + LED (2.0 V)

  step 1: D13 low  → LED off   v(d13) = 0.00 V, I(LED) = 0.0 mA
  step 2: D13 HIGH → LED lit   v(d13) = 4.54 V, I(LED) = 11.5 mA

  digitalWrite(13, HIGH) in firmware → 11.5 mA through the simulated LED.
```

## `pwm_duty`

Configures Timer0 Fast-PWM (non-inverting, `OCR0A = 64`, `TOP = 255`) via the
core's data space — exactly what an `OUT`/`STS` does — then samples OC0A (PD6)
over four PWM periods.

```sh
$ cargo run -p ohmforge-mcu --example pwm_duty
Timer0 Fast-PWM on OC0A (PD6): OCR0A = 64, TOP = 255 → expect 25 % duty

  sampled 512 points over 4 PWM periods
  pin high fraction (measured duty): 25.0 %
  reported duty via pin_drive:       25.0 %
```

## `uart_tx`

Enables USART0, sets 9600 baud (`UBRR0 = 103` at 16 MHz), writes `"Hi!"` to
`UDR0` one byte at a time (waiting for `UDRE0`), and reads back the captured TX
stream.

```sh
$ cargo run -p ohmforge-mcu --example uart_tx
USART0 TX @ 9600 baud (16 MHz): sending "Hi!"

  wrote UDR0 = 0x48 ('H')
  wrote UDR0 = 0x69 ('i')
  wrote UDR0 = 0x21 ('!')

  transmitter captured 3 bytes: [72, 105, 33] = "Hi!"
```

## `pico_blink`

A hand-assembled raw RP2040 image (vector table + code) that loads `SIO_BASE`,
enables GP25 as an output (`GPIO_OE_SET`) and toggles it (`GPIO_OUT_XOR`). The
on-board LED pin alternates on every toggle.

```sh
$ cargo run -p ohmforge-mcu --example pico_blink
Raspberry Pi Pico blink — GP25 (on-board LED) via the RP2040 SIO

  after setup: GP25 = low
  toggle 1: GP25 = HIGH
  toggle 2: GP25 = low
  toggle 3: GP25 = HIGH
  toggle 4: GP25 = low
```
