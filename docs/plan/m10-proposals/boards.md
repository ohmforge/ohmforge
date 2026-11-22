# M10 proposals — ohmforge MCU/board layer

Scope: `crates/ohmforge-mcu` + `crates/ohmforge-devices`, ohmforge v0.8.1 (M0–M9
complete). This is a ranked proposal for the next round of board-layer work,
grounded in the actual decoders, peripherals, and the co-sim bridge. Read-only
study; nothing here is implemented yet.

Standing constraints honoured by every proposal below:

- `ohmforge-mcu` must **not** depend on `ohmforge-sim`. Analog crosses only
  through the `PinIo` / `PinDrive` exchange (`crates/ohmforge-mcu/src/pin.rs`)
  and the mixed-signal boundary (`crates/ohmforge-mcu/src/boundary.rs`). The
  lockstep loop lives in the `of` binary.
- Devices are **composed over existing `ohmforge-core` primitives** — no new
  `PartKind` (`crates/ohmforge-devices/src/sensors.rs:1`,
  `crates/ohmforge-devices/src/subcircuits.rs:1`).
- Cores stay pure/deterministic and `wasm32`-clean (`crates/ohmforge-mcu/src/lib.rs:13`).

---

## Honest snapshot: what decodes to `Unknown` today

An `Unknown` opcode is not benign. In all three cores it hits a
`debug_assert!(false, …)` — i.e. it **panics in debug/test builds** and silently
degrades to a 1-cycle NOP in release:

- AVR: `crates/ohmforge-mcu/src/avr/core.rs:790`
- Cortex-M: `crates/ohmforge-mcu/src/cortexm/core.rs:1087`
- Xtensa: `crates/ohmforge-mcu/src/xtensa/core.rs:339`

So every gap below is a hard wall for real compiled firmware, not a soft
fallback.

**AVR** (`crates/ohmforge-mcu/src/avr/decode.rs:19`): no `MUL/MULS/FMUL`
(decode.rs:111), no `LPM/SPM` (decode.rs:140), no `SLEEP/WDR/BREAK`, no
`EIJMP/EICALL`. ISA enum has no variants for any of these
(`crates/ohmforge-mcu/src/avr/isa.rs`).

**Cortex-M** (`crates/ohmforge-mcu/src/cortexm/decode.rs:58`): no `CBZ/CBNZ`,
`SDIV/UDIV`, `SSAT/USAT`, `SMLAL*`, `TBB/TBH`, `LDREX/STREX`, any coprocessor/FP,
and no load/store with pre/post-index writeback or scaled register offset — the
imm8 path returns `Unknown` for anything but P=1,U=1,W=0 (decode.rs:785) and the
register path only models LSL #0 (decode.rs:792). Barriers/`CPS`/`BKPT`/`UDF` are
absent or folded to NOP.

**Xtensa** (`crates/ohmforge-mcu/src/xtensa/decode.rs:25`): models only the
**CALL0 (windowless) ABI** and a tiny instruction subset. `op0=0x7`
register-register branches (`BEQ/BNE/BLT/BBC/BBS…`) are not even dispatched
(decode_core, decode.rs:56); shifts/`EXTUI`/`MUL` (RST1–3) are `Unknown`
(decode.rs:73); immediate-compare branches + `LOOP` are `Unknown`
(decode.rs:178); and all narrow Code-Density forms except `MOV.N/RET.N/NOP.N`
are `Unknown` (decode.rs:184). Net effect: the Xtensa core runs hand-written
CALL0 assembly, **not** ESP-IDF / Arduino-ESP32 output (which is windowed and
leans on reg-reg branches).

**Peripheral breadth is very uneven.** AVR has GPIO, Timer0/Timer1 (+PWM), ADC,
USART-TX, SPI master, TWI master
(`crates/ohmforge-mcu/src/avr/periph.rs:1`). **Cortex-M has only the RP2040 SIO
GPIO block and an STM32 GPIO port** — no SysTick, no timer, no ADC, no UART;
`next_event` is hard-coded `None` so the core can never schedule a wake
(`crates/ohmforge-mcu/src/cortexm/periph.rs:1`,
`crates/ohmforge-mcu/src/cortexm/core.rs:1417`). **Xtensa has only GPIO** and no
interrupt matrix at all (`crates/ohmforge-mcu/src/xtensa/periph.rs:1`,
`crates/ohmforge-mcu/src/xtensa/core.rs:25`, next_event `None` at core.rs:400).

**Interrupt sourcing is narrow even on AVR.** Only `INT0/INT1`, `PCINT0/1/2`,
and `ADC` have vectors and can fire
(`crates/ohmforge-mcu/src/avr/periph.rs:661`, dispatch at periph.rs:759 and
periph.rs:1453). `TIMSK0/TIMSK1` are stored but never consulted to raise an
interrupt (periph.rs:1677, periph.rs:1758), and USART RX only sets the `RXC0`
poll flag with no RX/TX/UDRE vector (periph.rs:1553). So timer- and
serial-interrupt-driven firmware — which is almost all non-trivial Arduino code —
cannot be serviced.

---

## Ranked proposals

### 1. Cortex-M Thumb-2 ISA correctness: `CBZ/CBNZ`, `UDIV/SDIV`, load/store writeback  (rank 1)

**What + why.** These are not exotic — they are what `arm-none-eabi-gcc`/LLVM
emit for *ordinary* C on Cortex-M3/M4, so the first realistic firmware that is
not hand-assembled hits them and panics (core.rs:1087). Concretely:

- `CBZ`/`CBNZ` (compare-and-branch-on-zero) — emitted constantly for loop and
  null guards on ARMv7-M. The 16-bit misc group never decodes them; they fall
  into `decode_misc`'s `_ => Instr::Unknown`
  (`crates/ohmforge-mcu/src/cortexm/decode.rs:422`). No ISA variant exists
  (`crates/ohmforge-mcu/src/cortexm/isa.rs`).
- `UDIV`/`SDIV` — every integer `/` or `%` on M3/M4 compiles to these; they live
  in the T32 data-processing-register space that currently only models
  shifts/`REV`/`CLZ`/multiplies (decode.rs:810) and returns `Unknown` otherwise.
- Load/store **with writeback / pre-/post-index** and scaled register offsets —
  the single-access path bails to `Unknown` for the common `ldr r0,[r1],#4` /
  `str r0,[r1,#4]!` idioms (decode.rs:785, decode.rs:792) that compilers use for
  array walks and stack frames.

**Effort:** M (CBZ/CBNZ: S; UDIV/SDIV: S; writeback addressing: M — needs an
`AddrMode` that carries index/writeback and an execute-side base-register
update). **Risk:** Low–Medium — pure decode+execute, well-specified by the ARM
ARM; the writeback change touches the `Mem`/`AddrMode` shape
(`crates/ohmforge-mcu/src/cortexm/isa.rs`) so needs care that existing load/store
tests still pass. **Cross-crate:** none (no new board/manifest).

**Acceptance gate.** A Rust/C test module compiles (or hand-assembles) a routine
that (a) divides two runtime values, (b) walks an array with post-increment
`LDR`, and (c) loops with `CBNZ`, writing the result to a GPIO byte; run it on
`cortexm::board::Pico` and assert the GPIO output equals the reference
computation, with **zero `Unknown` panics** across the run. A negative test
asserts the three encodings no longer decode to `Instr::Unknown`.

---

### 2. AVR peripheral-interrupt vectors: Timer0/1 (OVF + COMPA/COMPB) and USART (RX/TX/UDRE)  (rank 2)

**What + why.** The interrupt *plumbing* is already built — a latched
controller, priority ordering, `RETI`, and `take_pending_interrupt` polled
before each fetch (`crates/ohmforge-mcu/src/avr/core.rs:804`,
`crates/ohmforge-mcu/src/avr/periph.rs:1453`). What is missing is **sourcing**:
the timers already raise their `TOV`/`OCF` flags (periph.rs:313) and `TIMSK0/1`
are already stored (periph.rs:1677, periph.rs:1758), but nothing turns an
enabled flag into a pending vector. Likewise USART sets `RXC0`/`TXC0`/`UDRE0`
(periph.rs:1553, periph.rs:176) but raises no interrupt. Because Arduino's
`millis()`/`micros()`/`delay()` are driven by the **TIMER0 overflow ISR**, and
`Serial` RX is interrupt-driven, this single gap blocks essentially all
non-trivial Arduino sketches even though the CPU core itself is complete.

Add the vector constants (`TIMER0_OVF`, `TIMER0_COMPA/COMPB`, `TIMER1_OVF`,
`TIMER1_COMPA/COMPB`, `USART_RX`, `USART_UDRE`, `USART_TX`) and fold an enabled
flag into the same priority sweep that `ExtInt::take` + `take_pending_interrupt`
already run (periph.rs:759), clearing the flag on dispatch exactly as the ADC
vector already does (periph.rs:1456).

**Effort:** M. **Risk:** Low — additive to an existing, tested mechanism; main
care is datasheet-correct vector numbers per variant (the 2560/ATtiny strides
differ — see `crates/ohmforge-mcu/src/avr/variant.rs`) and flag-clear-on-service
semantics. **Cross-crate:** none.

**Acceptance gate.** Hand-assembled (HEX, in the `tests/blink.rs` idiom) 328P
firmware: configure Timer1 CTC with `OCIE1A` set and `SEI`, put an `ISR` at the
`TIMER1_COMPA` vector that toggles PB5, and `RJMP self` in `main`. Assert D13
toggles at the expected compare period under co-sim. A second gate pushes a byte
via `usart_rx_push`, enables `RXCIE0`, and asserts the RX ISR runs and reads
`UDR0`.

---

### 3. Cortex-M peripheral layer, starting with SysTick + a general-purpose timer  (rank 3)

**What + why.** The RP2040 and STM32 cores are GPIO-only
(`crates/ohmforge-mcu/src/cortexm/periph.rs:1`) and `next_event` returns `None`
(core.rs:1417), so the ARM cores cannot model time at all: no `delay()`, no
`HAL_Delay`, no periodic ISR, no PWM. **SysTick** is the highest-leverage single
addition because it is *architectural* — identical on M0+, M3 and M4 — so one
implementation serves Pico and STM32 both. Model the four SCS registers
(`SYST_CSR/RVR/CVR/CALIB` at `0xE000_E010`), count down from the CPU cycle
counter in the existing `step_to_cycle` loop (core.rs:1422), set `COUNTFLAG`,
and raise the SysTick exception through the NVIC-lite that already exists
(`crates/ohmforge-mcu/src/cortexm/core.rs:20`). Then wire `next_event` to return
the next SysTick reload so the lockstep loop stops on the tick
(`crates/ohmforge-mcu/src/lib.rs:72`). A follow-on adds one RP2040 TIMER / STM32
TIMx channel for PWM out through `PinDrive::Pwm` (`pin.rs:69`), mirroring the
AVR timer model.

**Effort:** M for SysTick alone; L including a vendor timer + PWM. **Risk:**
Medium — this is the first Cortex-M code to drive the interrupt path under
co-sim and to make `next_event` non-trivial; get the cycle→reload arithmetic and
the loop's early-wake contract right (lib.rs:76). **Cross-crate:** none for
SysTick; the PWM follow-on may want a Pico/STM32 PWM part in
`ohmforge-devices::modules`.

**Acceptance gate.** Firmware that programs `SYST_RVR`, enables SysTick with
`TICKINT`, and has the SysTick handler XOR the LED GPIO bit; run on
`cortexm::board::Pico` and assert GP25 toggles at the programmed tick rate and
that `next_event` reports the pending tick (so the co-sim loop would stop there).

---

### 4. AVR `LPM`/`SPM` (program-memory access) and the `MUL` family  (rank 4)

**What + why.** Both decode to `Unknown` today (decode.rs:140 for `LPM`,
decode.rs:111 for `MUL`). `LPM` underpins every `PROGMEM` read —
`pgm_read_byte`, flash lookup tables, and `Serial.print(F("…"))` — which appear
in a large fraction of real sketches. `MUL/MULS/MULSU/FMUL*` are emitted by
avr-gcc for any `int`/`long` multiply on the 328P (it has a hardware
multiplier), so arithmetic-heavy sketches panic. Add the `Instr` variants
(`crates/ohmforge-mcu/src/avr/isa.rs`), decode them in `decode_two_reg` /
`decode_9` (decode.rs:60, decode.rs:106), and execute: `LPM` reads the flash
byte addressed by `Z` (with the post-increment and `Rd`-vs-`r0` forms) and the
`MUL` family writes the 16-bit product to `r1:r0` setting C/Z per the datasheet.
`SPM` (self-programming) and `SLEEP`/`WDR` can be modest: `SLEEP`/`WDR` as
timed NOPs that still tick peripherals, `SPM` behind a guard.

**Effort:** S (LPM + MUL); S–M including SPM. **Risk:** Low — localized to AVR
decode/execute; the `r1:r0` destination and flag effects are the only subtlety.
**Cross-crate:** none.

**Acceptance gate.** Firmware with a `PROGMEM` byte table read via `LPM Rd, Z+`
into successive GPIO writes, plus a `MUL` of two registers whose product drives a
port; assert the port sequence matches the table and the product, with no
`Unknown` panic.

---

### 5. New core: RISC-V `RV32IMC` (ESP32-C-class and generic)  (rank 5)

**What + why.** Already named as planned (`crates/ohmforge-mcu/src/lib.rs:12`),
and `ohmforge-devices` has an ESP32 manifest stub but no runnable Xtensa-free
path (`crates/ohmforge-devices/src/lib.rs:115`). RISC-V is strategically the
best next core: RV32I is small and fully specified, `M` (mul/div) and `C`
(compressed) cover what real toolchains emit, and modern boards
(ESP32-C3/C6, and generic RV32 targets) are RISC-V. It slots cleanly behind the
existing `McuCore` trait (`lib.rs:50`) alongside the AVR/Cortex-M/Xtensa modules,
reusing the same `PinIo` bridge and a GPIO peripheral in the style of the
existing ones. It is also a cleaner target than finishing the Xtensa windowed
ABI (see rank 6).

**Effort:** L — a new `riscv` module (core/decode/isa/periph/board), ~RV32I +
M + C, a trap/`mret` interrupt path, and a minimal GPIO + timer peripheral.
**Risk:** Medium — large surface, but the ISA is well-documented and the trait
contract is established; biggest risk is scope creep (keep it to IMC + GPIO +
one timer for M10). **Cross-crate:** a `BoardDef` + manifest entry in
`ohmforge-devices` (e.g. an ESP32-C3 or generic `riscv32` board, mirroring
`pico()`/`stm32()` at `crates/ohmforge-devices/src/lib.rs:96`).

**Acceptance gate.** An `rv32imc_blink` test (in the style of
`tests/xtensa_blink.rs`) loads a hand-assembled blink that toggles a GPIO via
the new peripheral and asserts the pin toggles under co-sim; a decode test
asserts representative `I`, `M`, and compressed (`C.*`) encodings round-trip and
none land on `Unknown`.

---

### 6. Device/sensor models: HC-SR04 ultrasonic, rotary encoder, and a generic I²C sensor  (rank 6)

**What + why.** The library has resistive transducers and a DC-motor model
(`crates/ohmforge-devices/src/sensors.rs`) and drop-in modules (LED, 7-seg,
servo, stepper, WS2812 — `crates/ohmforge-devices/src/modules.rs`), but nothing
that exercises the **timing-** and **bus-** heavy side of real projects:

- **HC-SR04 ultrasonic** — a trigger-pulse-in / echo-pulse-out timing model
  (echo width ∝ distance). It is purely digital-timing, composed like the
  existing `Servo`/`Ws2812` pulse models (modules.rs:215, modules.rs:248), and
  directly exercises the input-edge → interrupt path (and so pairs naturally
  with ranks 2–3).
- **Rotary encoder** — two quadrature outputs; a tiny state model that feeds the
  pin-change / `INT0` machinery already present on AVR (periph.rs:719).
- **Generic I²C sensor** (e.g. a temperature/ADC device) — a slave responder for
  the existing AVR TWI master (periph.rs:40), proving the bus end-to-end; built
  over primitives with no new `PartKind` (sensors.rs:1).

**Effort:** M (each model S–M; the I²C slave is the largest). **Risk:** Low —
behavioural models over existing primitives and the existing pin exchange; no
core changes. **Cross-crate:** optional `manifest`/`modules` catalog entries so
boards can `part add ultrasonic`/`encoder`.

**Acceptance gate.** Co-sim test: AVR firmware pulses a trigger pin, the HC-SR04
model drives an echo pulse whose width encodes a set distance, an `INT0`/PCINT
ISR times the echo, and the measured width matches the distance within
tolerance. For the encoder, rotate it N detents and assert the firmware's step
count (via pin-change ISR) equals N.

---

## Deferred / explicitly out of scope for M10

- **Xtensa windowed ABI + reg-reg branches.** Making the Xtensa core run real
  ESP-IDF/Arduino-ESP32 firmware needs the 64-entry windowed register file
  (`CALL4/8/12`, `ENTRY`, `RETW`, `WindowBase`/`WindowStart` rotation) *and* the
  `op0=0x7` branch family *and* the narrow Code-Density forms
  (`crates/ohmforge-mcu/src/xtensa/decode.rs:25`). That is L–XL and, given
  RISC-V is the path for modern Espressif parts (rank 5), lower value per unit
  effort. Worth a dedicated milestone, not a slice of M10.
- **ELF firmware loading** (`crates/ohmforge-mcu/src/lib.rs:63`) — enables
  dropping in real toolchain output directly; a good companion to ranks 1/5 but
  tracked separately.
