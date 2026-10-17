# Microcontroller emulation & co-simulation

This guide explains how ee-assist runs **real microcontroller firmware against a
simulated circuit**: the two emulated cores (AVR and RP2040), their peripherals,
how firmware is loaded, how an MCU pin is bridged to a circuit net, the
mixed-signal boundary, and the lockstep co-simulation loop (`ee cosim`).

It is the user-facing companion to the architecture notes in
[`../ARCHITECTURE.md`](../ARCHITECTURE.md) § "Board & MCU emulation" and the
track design in [`../plan/track-boards.md`](../plan/track-boards.md). Everything
here is demonstrated by the runnable examples under
[`examples/mcu/`](../../examples/mcu/) — each command's output in this guide was
produced by actually running it.

The emulators live in the `ee-mcu` crate. They are **pure computation** — no
wall-clock, no randomness, no I/O — so a run is fully deterministic and the crate
builds for `wasm32-unknown-unknown`. `ee-mcu` depends on `ee-core` and
`ee-digital` but **not** on `ee-sim`: the analog solver and the MCU core are
siblings, and the lockstep co-sim loop that joins them lives in the `ee` binary.

---

## 1. The cores

Both cores are **instruction-accurate**: they execute whole instructions and
track a cycle counter, so simulated time maps onto the instruction stream, but
sub-instruction timing is not modelled. Both implement the shared
[`McuCore`](../../crates/ee-mcu/src/lib.rs) trait (`step`, `step_to`,
`load_firmware`, `pin_count`, `next_event`, `step_to_cycle`).

### AVR — ATmega328P (Arduino Uno / Nano)

An 8-bit Harvard-architecture RISC core (`ee_mcu::avr`):

- **Flash**: 16 K 16-bit words (32 KiB). The program counter is a **word** index.
- **Data space** (byte-addressed, one flat space):

  | range            | contents                                   |
  |------------------|--------------------------------------------|
  | `0x0000..0x0020` | the 32 general-purpose registers `r0..r31` |
  | `0x0020..0x0100` | I/O and extended-I/O registers             |
  | `0x0100..0x0900` | 2 KiB internal SRAM (`RAMEND = 0x08FF`)    |

  `IN`/`OUT` address the I/O window via `addr + 0x20`. `SREG` (`0x5F`), `SPL`
  (`0x5D`) and `SPH` (`0x5E`) are kept as dedicated fields but are reachable at
  those data addresses.
- **Status register** ([`Sreg`](../../crates/ee-mcu/src/avr/sreg.rs)): the eight
  flags `C Z N V S H T I`, packed LSB-first so `IN`/`OUT`/`PUSH`/`POP` of `SREG`
  round-trip exactly. The datasheet three-term carry/half-carry/overflow forms
  are implemented verbatim.
- **Interrupts**: a latched controller. `request_interrupt(n)` marks vector `n`
  pending; before an instruction, if the global enable `I` is set, the
  lowest-numbered pending vector is taken — pushing the return address, clearing
  `I`, and jumping to `n × 2` (the ATmega328P's two-word vector spacing). `RETI`
  pops the address and re-enables interrupts.
- **Instruction coverage**: the common subset — the register ALU ops, the
  immediate ops, `ADIW`/`SBIW`, the unary ops, all the load/store forms
  (`LD/ST` with X/Y/Z pre-dec/post-inc, `LDD/STD`, `LDS/STS`, `IN/OUT`,
  `PUSH/POP`), the bit ops (`SBI/CBI/SBIC/SBIS/SBRC/SBRS/BSET/BCLR/BLD/BST`) and
  control flow (`RJMP/RCALL/IJMP/ICALL/JMP/CALL/RET/RETI`, the full
  `BRBS/BRBC` branch family, `CPSE`). The multiply family, `LPM`/`SPM`,
  `SLEEP`/`WDR`/`BREAK` and `EIJMP`/`EICALL` are not yet modelled and decode to
  `Instr::Unknown` (a no-op, with a debug assertion). See
  [`avr/decode.rs`](../../crates/ee-mcu/src/avr/decode.rs) for the exact list.

The [`Uno`](../../crates/ee-mcu/src/avr/board.rs) board wraps the core at 16 MHz
with the Arduino pin map.

### Cortex-M0+ — RP2040 (Raspberry Pi Pico)

An ARMv6-M (Thumb) core (`ee_mcu::cortexm`):

- **Registers**: sixteen 32-bit registers, with `r13 = SP`, `r14 = LR`,
  `r15 = PC`. A read of `PC` yields the Thumb **PC value** (the current
  instruction's address + 4), so PC-relative loads and branches compute the
  right target; a write to `PC` branches (clearing the Thumb bit).
- **Condition flags** ([`Apsr`](../../crates/ee-mcu/src/cortexm/core.rs)):
  `N`/`Z`/`C`/`V`, packed into the top nibble of `xPSR` with the `T` bit set.
- **Memory**: a flat, little-endian 256 KiB RAM mapped from address 0, plus the
  RP2040 **SIO** GPIO block decoded at `0xD000_0000`. Firmware and the reset
  vector table load at address 0.
- **Exceptions (NVIC-lite)**: `request_interrupt` + `enable_irq` for external
  IRQs, and `SVC` raises SVCall. Taking an exception **stacks the eight-word
  frame** (`r0`–`r3`, `r12`, `LR`, return address, `xPSR`), loads `PC` from the
  vector table, and sets `LR` to an `EXC_RETURN` magic value; a `BX`/`POP` to
  that value unstacks and resumes.
- **Instruction coverage**: the full ARMv6-M data-processing register group,
  the immediate/register/high-register `MOV`/`ADD`/`SUB`/`CMP`, shift-by-
  immediate, `LDR`/`STR` (+ byte/halfword, signed loads) in literal, immediate,
  register and SP-relative forms, `ADR`, `ADD`/`SUB SP`, `PUSH`/`POP`,
  `STMIA`/`LDMIA`, `SXT*`/`UXT*`, `REV*`, `B`/`B<cond>`/`BL`/`BX`/`BLX`, `SVC`,
  and the hint space as `NOP`. `MRS`/`MSR`, the barriers, `CPS`, `BKPT`, `UDF`
  and `WFI`/`WFE` are not modelled (decode to `Instr::Unknown`). See
  [`cortexm/decode.rs`](../../crates/ee-mcu/src/cortexm/decode.rs).

The [`Pico`](../../crates/ee-mcu/src/cortexm/board.rs) board wraps the core at
the RP2040's 125 MHz default with the Pico pin map.

---

## 2. Loading firmware

`McuCore::load_firmware(&[u8])` loads an in-memory image and resets the core:

- **Raw binary** — copied verbatim. On the **AVR** it is little-endian 16-bit
  words straight into flash from word 0. On the **RP2040** it is bytes into RAM
  from address 0, and the first two words are the reset vector table (initial
  `SP`, then the reset `PC` with the Thumb bit stripped), which is applied
  automatically.
- **Intel HEX** (AVR) — an image that begins with `:` is parsed as Intel HEX.
  Record types `00` (data), `01` (EOF) and `04` (extended linear address) are
  supported; the per-record checksum is verified. Malformed records are rejected
  with `FirmwareError::BadImage` rather than panicking.
- **ELF** — recognised by its `0x7F E L F` magic but **not yet supported**
  (`FirmwareError::UnsupportedFormat`).

For tests and hand-assembled snippets the cores also expose `load_program`
(`&[u16]` of raw instruction words / halfwords), which skips the firmware
framing.

The `blink.hex` fixture ([`crates/ee-mcu/tests/blink.hex`](../../crates/ee-mcu/tests/blink.hex))
is a hand-assembled Intel-HEX blink program for the Uno. Its assembly, by flash
word address:

```text
 0: SBI  DDRB, 5        ; 0x9A25 — PB5 (Arduino D13) as an output
 1: LDI  r16, 0x20      ; 0xE200 — bit mask for PB5
 2: (loop) OUT PINB,r16 ; 0xB903 — write-1-to-PINx toggles PORTB bit 5
 3: LDI  r18, 25        ; 0xE129 — outer delay count
 4: (outer) LDI r24,0x10; 0xE180 — inner count = 0x2710 (10000), low byte
 5: LDI  r25, 0x27      ; 0xE297 — inner count, high byte
 6: (inner) SBIW r24, 1 ; 0x9701
 7: BRNE inner          ; 0xF7F1 (offset -2)
 8: DEC  r18            ; 0x952A
 9: BRNE outer          ; 0xF7D1 (offset -6)
10: RJMP loop           ; 0xCFF7 (offset -9)
```

Each 16-bit word is emitted little-endian into a single Intel-HEX data record
(`:` + length + address + type `00` + data + checksum), followed by the EOF
record `:00000001FF`. The delay is `25 × 10000` `SBIW` iterations at 4 cycles
each ≈ 1,000,000 CPU cycles between toggles, so clocking the Uno at 1 MHz makes
D13 toggle at ~1 Hz. See the `uno_blink` example below.

---

## 3. Peripherals

Peripherals advance from the CPU cycle counter: after each instruction the core
calls `tick(cycles)`, so timers count, PWM edges move, the ADC converts and the
USART shifts bytes out in step with execution — deterministic and wall-clock
free.

### AVR ([`avr/periph.rs`](../../crates/ee-mcu/src/avr/periph.rs))

- **GPIO** — `PORTB`/`PORTC`/`PORTD` with `DDRx` (direction), `PORTx` (output
  latch / input pull-up enable) and `PINx` (resolved pin levels), including the
  "write a 1 to `PINx` toggles `PORTx`" hardware feature. A pin reads its output
  latch when it is an output, else its externally driven input level; a timer's
  PWM waveform appears on an output-compare pin only when that pin's `DDR` bit
  selects output.
- **Timer0** (8-bit) and **Timer1** (16-bit) — Normal, CTC and **Fast-PWM**
  waveform modes at the selected prescaler, with the overflow and compare-match
  flags. Fast-PWM output is driven on the OC pins; non-inverting (`COM = 2`) is
  high while `count < OCRx` (duty `OCRx / (TOP + 1)`), inverting (`COM = 3`) is
  the complement. (Phase-correct PWM is not yet modelled.) Timer1 uses the
  16-bit `TEMP`-register write semantics (high byte first).
- **ADC** — a 10-bit successive-approximation conversion of a per-channel input
  voltage against the reference, respecting the prescaler conversion time
  (13 ADC clocks) and the `ADLAR` left-adjust bit. Each Port C pin feeds its ADC
  channel.
- **USART0** — the transmit path: bytes written to `UDR0` shift out at the baud
  implied by `UBRR0`/`U2X0` and the frame shape (data/stop/parity bits), with
  the `UDRE0` (buffer empty) and `TXC0` (transmit complete) flags and a one-deep
  holding buffer. Every shifted byte is captured (`peripherals().usart_tx()`).
  RX is a minimal stub.

### RP2040 SIO ([`cortexm/periph.rs`](../../crates/ee-mcu/src/cortexm/periph.rs))

The single-cycle I/O GPIO block at `0xD000_0000`: `GPIO_OUT` (output latch),
`GPIO_OE` (output enable) and `GPIO_IN` (sampled input), plus the atomic
`*_SET` / `*_CLR` / `*_XOR` aliases real firmware uses (a blink typically
`GPIO_OUT_XOR`s the LED bit). A pin drives whenever its `OE` bit is set.

---

## 4. Pin mapping

The co-sim exchange addresses pins by an opaque `PinId` (a flat index). Obtain
ids from the board, which maps friendly pin names to the core's port/bit.

### Arduino Uno (D-pins / A-pins)

| Arduino pin | AVR port·bit | notes                           |
|-------------|--------------|---------------------------------|
| D0..D7      | PORTD 0..7   | D0/D1 are the USART RX/TX       |
| D8..D13     | PORTB 0..5   | **D13 → PB5** (on-board LED)    |
| A0..A5      | PORTC 0..5   | ADC channels 0..5               |

```rust
use ee_mcu::avr::Uno;
let led = Uno::digital_pin_id(13).unwrap(); // D13 → PB5
let a0  = Uno::analog_pin_id(0).unwrap();   // A0  → PC0 / ADC0
```

### Raspberry Pi Pico (GP-pins)

The GP number **is** the GPIO (and `PinId`) number; the on-board LED is on GP25.

```rust
use ee_mcu::cortexm::Pico;
let led = Pico::gpio_pin_id(25).unwrap(); // GP25 (on-board LED)
```

---

## 5. The `PinIo` pin ↔ net bridge

[`PinIo`](../../crates/ee-mcu/src/pin.rs) is the only state a core and the
outside world swap each step — a fixed bank of pins, each carrying two halves:

- **out** — a [`PinDrive`] the core asserts: `HighZ` (input / not driving),
  `Digital(bool)`, or `Pwm { duty, level }` (an output-compare pin, with its
  effective duty and instantaneous level).
- **in** — the sampled analog input voltage (volts) the loop obtained from the
  solved circuit, which the ADC and the digital-input thresholds read back.

A core only ever reads input voltages and writes pin drives; it knows nothing
about nets or the solver. The co-sim loop reconciles the two with the circuit
graph. Size the exchange to the core's `pin_count()` (`PinIo::new(n)`), or use
the board's `pin_io()`.

```rust
let mut io = uno.pin_io();
io.set_input(a0, 2.5);                 // the loop feeds a solved node voltage in
uno.step_with_io(&mut io);             // sample in, run, publish out
let drive = io.drive(led);             // what the core now drives
```

---

## 6. The mixed-signal boundary

The bridge above exchanges *ideal* levels. The
[`boundary`](../../crates/ee-mcu/src/boundary.rs) module refines both halves so a
pin behaves like real silicon once it meets a non-trivial load:

- **Input side — [`InputThresholds`]**: a node voltage resolves to a four-valued
  logic level through configurable logic-low (`V_IL`) and logic-high (`V_IH`)
  thresholds with a **hysteresis band** between them (a Schmitt-trigger input).
  A voltage in the band holds the previous level rather than chattering. The
  default is a single 2.5 V threshold; `InputThresholds::cmos(vcc)` gives the
  generic `0.3·Vcc`/`0.7·Vcc` band.
- **Output side — [`OutputDrive`]**: a driven pin is a **Thévenin source** — an
  open-circuit rail voltage behind a series on-resistance (`r_source` driving
  high, `r_sink` driving low) — so the delivered voltage sags off the rail by the
  divider the drive forms with the load. An optional slew-rate limit caps how
  fast the open-circuit voltage may move. `pin_thevenin(port, bit)` returns the
  `Thevenin { v_oc, r }` the loop stamps onto the node; `Thevenin::delivered_into`
  / `current_into` solve a resistive load.

```rust
use ee_mcu::boundary::{InputThresholds, OutputDrive};
let p = uno.mcu_mut().peripherals_mut();
p.set_input_thresholds(InputThresholds::cmos(5.0));      // Schmitt inputs
p.set_output_drive(OutputDrive::push_pull(5.0, 40.0));   // 5 V, 40 Ω source R
```

---

## 7. The lockstep co-simulation loop (`ee cosim`)

The real loop lives in the `ee` binary (`ee cosim`) — the one place that
legitimately links both the MCU and the analog engine. One lockstep interval is:

1. **sample** the circuit's solved node voltages into the core (`sample_inputs`,
   done at the top of `step_to_cycle` / `step_with_io`),
2. **run** the core forward (`step_to_cycle(until, &mut io)` runs whole
   instructions to a cycle bound; `step_with_io` runs exactly one),
3. **publish** each pin's `PinDrive` back into the exchange (`publish_drives`),
4. turn each drive into a `Thevenin` source, **stamp** it onto its node and
   **solve** the circuit,
5. feed the solved node voltages back in on the next iteration.

`next_event()` returns the absolute cycle at which the core next has something
scheduled (a timer compare/overflow, a USART bit boundary, an ADC completing).
The loop advances both engines to
`t_next = min(core.next_event(), sim.next_event(), …)`, so an early — but never
late — wake only costs a little efficiency, never correctness.

`ee cosim` wires a tiny AVR program (make D13 an output, drive it high) to a
220 Ω resistor + LED in the analog graph and reports the LED current. The
`cosim_led` example below reproduces that structure entirely inside `ee-mcu`
(solving a simple load by hand through the boundary, since `ee-mcu` cannot depend
on `ee-sim`).

---

## 8. Worked examples

All examples are runnable and live under
[`crates/ee-mcu/examples/`](../../crates/ee-mcu/examples/) (indexed from
[`examples/mcu/`](../../examples/mcu/)). Run any of them with
`cargo run -p ee-mcu --example <name>`. The outputs below are verified.

### `uno_blink` — the ~1 Hz blink

```text
$ cargo run -p ee-mcu --example uno_blink
Arduino Uno blink — D13 (PB5) toggling at ~1 Hz (clock = 1 MHz)

  edge: D13 -> HIGH  at cycle         4  (t = 0.000 s)
  edge: D13 -> low   at cycle   1000107  (t = 1.000 s)
  edge: D13 -> HIGH  at cycle   2000210  (t = 2.000 s)

Measured toggle interval: 1000103 cycles = 1.000 s (≈ 1 Hz, half-period).
```

### `cosim_led` — firmware drives a pin into a simulated LED

```text
$ cargo run -p ee-mcu --example cosim_led
Co-simulation: AVR firmware drives D13 → a simulated LED.
  driver: 5 V push-pull, 40 Ω source R;  load: 220 Ω + LED (2.0 V)

  step 1: D13 low  → LED off   v(d13) = 0.00 V, I(LED) = 0.0 mA
  step 2: D13 HIGH → LED lit   v(d13) = 4.54 V, I(LED) = 11.5 mA

  digitalWrite(13, HIGH) in firmware → 11.5 mA through the simulated LED.
```

### `pwm_duty` — a measured 25 % Fast-PWM duty

```text
$ cargo run -p ee-mcu --example pwm_duty
Timer0 Fast-PWM on OC0A (PD6): OCR0A = 64, TOP = 255 → expect 25 % duty

  sampled 512 points over 4 PWM periods
  pin high fraction (measured duty): 25.0 %
  reported duty via pin_drive:       25.0 %
```

### `uart_tx` — a USART byte stream

```text
$ cargo run -p ee-mcu --example uart_tx
USART0 TX @ 9600 baud (16 MHz): sending "Hi!"

  wrote UDR0 = 0x48 ('H')
  wrote UDR0 = 0x69 ('i')
  wrote UDR0 = 0x21 ('!')

  transmitter captured 3 bytes: [72, 105, 33] = "Hi!"
```

### `pico_blink` — RP2040 firmware toggling GP25

```text
$ cargo run -p ee-mcu --example pico_blink
Raspberry Pi Pico blink — GP25 (on-board LED) via the RP2040 SIO

  after setup: GP25 = low
  toggle 1: GP25 = HIGH
  toggle 2: GP25 = low
  toggle 3: GP25 = HIGH
  toggle 4: GP25 = low
```

The bridge surface itself is also covered end-to-end by the integration tests
[`crates/ee-mcu/tests/blink.rs`](../../crates/ee-mcu/tests/blink.rs) and
[`crates/ee-mcu/tests/cosim.rs`](../../crates/ee-mcu/tests/cosim.rs).

[`PinDrive`]: ../../crates/ee-mcu/src/pin.rs
[`InputThresholds`]: ../../crates/ee-mcu/src/boundary.rs
[`OutputDrive`]: ../../crates/ee-mcu/src/boundary.rs
