//! Memory-mapped peripherals for the ATmega328P core: GPIO, Timer0/Timer1
//! (with PWM on the OC pins), the ADC, the USART transmitter, the SPI master and
//! the two-wire (I²C / TWI) master.
//!
//! # How peripherals hook the I/O space
//!
//! The AVR exposes its peripherals as byte registers in the data-space I/O
//! window (`0x20..0x100`). The core routes every data-space access through
//! [`Avr::read_data`](super::core::Avr::read_data) /
//! [`Avr::write_data`](super::core::Avr::write_data); those two functions ask
//! [`Peripherals::read`] / [`Peripherals::write`] first, and fall back to plain
//! I/O-array storage for addresses no peripheral claims. So `OUT`, `STS`, `SBI`,
//! `CBI` and friends drive real peripheral behaviour without any special casing
//! in the decoder.
//!
//! Peripherals advance from the CPU cycle counter: after every instruction the
//! core calls [`Peripherals::tick`] with the cycles that instruction consumed,
//! so timers count, PWM edges move, the ADC converts and the USART shifts bytes
//! out in step with execution. That keeps a run deterministic and wall-clock
//! free.
//!
//! # Modelled subset
//!
//! - **GPIO** — PORTB/PORTC/PORTD, DDRx and PINx, including the "write-1-to-PINx
//!   toggles PORTx" hardware feature.
//! - **Timer0** (8-bit) — Normal, CTC and Fast-PWM waveform generation at the
//!   configured prescaler, with the overflow (`TOV0`) and compare-match
//!   (`OCF0A`/`OCF0B`) flags, and Fast-PWM output on OC0A (PD6) / OC0B (PD5).
//! - **Timer1** (16-bit) — the same waveform modes with 16-bit `TEMP`-register
//!   write semantics, flags `TOV1`/`OCF1A`/`OCF1B`, and output on OC1A (PB1) /
//!   OC1B (PB2).
//! - **ADC** — a successive-approximation conversion of a settable per-channel
//!   input voltage to a 10-bit code, respecting the prescaler conversion time
//!   and `ADLAR`.
//! - **USART0** — the transmit path: bytes written to `UDR0` are shifted out at
//!   the configured baud (frame time derived from `UBRR0`, `U2X0`, data/stop/
//!   parity bits) and captured for inspection. RX is a minimal stub.
//! - **SPI master** — `SPCR`/`SPSR`/`SPDR`: a write to `SPDR` shifts a byte out
//!   on MOSI (PB3) at the configured SCK (PB5) rate, latching MISO (PB4) in,
//!   with every SCK edge and every completed byte captured and `SPIF` raised.
//! - **TWI master** — `TWBR`/`TWCR`/`TWDR`/`TWSR`: a START/addr/data/STOP state
//!   machine that renders its SDA (PC4) / SCL (PC5) activity as a line trace and
//!   a decoded event stream, posting the matching master status codes.

use crate::boundary::{InputThresholds, OutputDrive, Thevenin};
use crate::pin::{PinDrive, PinId, PinIo};
use ee_digital::Logic;

/// A general-purpose I/O port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Port {
    /// Port B (`PINB`/`DDRB`/`PORTB`): Arduino D8..D13 + crystal pins.
    B,
    /// Port C (`PINC`/`DDRC`/`PORTC`): Arduino A0..A5.
    C,
    /// Port D (`PIND`/`DDRD`/`PORTD`): Arduino D0..D7.
    D,
}

/// The three GPIO ports, in [`PinId`] index order (`B`, `C`, `D`).
const PORTS: [Port; 3] = [Port::B, Port::C, Port::D];

/// Number of pins in the ATmega328P [`PinIo`] exchange: three 8-bit ports.
pub const PIN_COUNT: usize = PORTS.len() * 8;

/// The flat [`PinId`] index for `port` pin `bit`: `port * 8 + bit`.
pub fn pin_id(port: Port, bit: u8) -> PinId {
    PinId::new((port as u16) * 8 + bit as u16)
}

/// The `(port, bit)` a [`PinId`] addresses, or `None` if it is outside the
/// ATmega328P's modelled pins.
pub fn pin_location(id: PinId) -> Option<(Port, u8)> {
    let idx = id.index() as usize;
    let port = *PORTS.get(idx / 8)?;
    Some((port, (idx % 8) as u8))
}

/// Which timer channel drives an output-compare (PWM) pin.
#[derive(Debug, Clone, Copy)]
enum Chan {
    A,
    B,
}

// --- Data-space register addresses (I/O address + 0x20) ---

const PINB: u16 = 0x23;
const DDRB: u16 = 0x24;
const PORTB: u16 = 0x25;
const PINC: u16 = 0x26;
const DDRC: u16 = 0x27;
const PORTC: u16 = 0x28;
const PIND: u16 = 0x29;
const DDRD: u16 = 0x2A;
const PORTD: u16 = 0x2B;

const TIFR0: u16 = 0x35;
const TIFR1: u16 = 0x36;
const TCCR0A: u16 = 0x44;
const TCCR0B: u16 = 0x45;
const TCNT0: u16 = 0x46;
const OCR0A: u16 = 0x47;
const OCR0B: u16 = 0x48;
const TIMSK0: u16 = 0x6E;
const TIMSK1: u16 = 0x6F;

const ADCL: u16 = 0x78;
const ADCH: u16 = 0x79;
const ADCSRA: u16 = 0x7A;
const ADCSRB: u16 = 0x7B;
const ADMUX: u16 = 0x7C;
const DIDR0: u16 = 0x7E;

const TCCR1A: u16 = 0x80;
const TCCR1B: u16 = 0x81;
const TCCR1C: u16 = 0x82;
const TCNT1L: u16 = 0x84;
const TCNT1H: u16 = 0x85;
const ICR1L: u16 = 0x86;
const ICR1H: u16 = 0x87;
const OCR1AL: u16 = 0x88;
const OCR1AH: u16 = 0x89;
const OCR1BL: u16 = 0x8A;
const OCR1BH: u16 = 0x8B;

const UCSR0A: u16 = 0xC0;
const UCSR0B: u16 = 0xC1;
const UCSR0C: u16 = 0xC2;
const UBRR0L: u16 = 0xC4;
const UBRR0H: u16 = 0xC5;
const UDR0: u16 = 0xC6;

// SPI master registers (I/O 0x0C..0x0E, so also reachable via IN/OUT/SBI/SBIS).
const SPCR: u16 = 0x2C;
const SPSR: u16 = 0x2D;
const SPDR: u16 = 0x2E;

// Two-wire (I²C / TWI) master registers (above the IN/OUT window, so firmware
// reaches them with STS/LDS).
const TWBR: u16 = 0xB8;
const TWSR: u16 = 0xB9;
const TWAR: u16 = 0xBA;
const TWDR: u16 = 0xBB;
const TWCR: u16 = 0xBC;
const TWAMR: u16 = 0xBD;

// --- Flag bit masks ---

/// Timer overflow flag (`TOV0`/`TOV1`), bit 0 of `TIFRn`.
const TOV: u8 = 1 << 0;
/// Output-compare A match flag (`OCF0A`/`OCF1A`), bit 1 of `TIFRn`.
const OCF_A: u8 = 1 << 1;
/// Output-compare B match flag (`OCF0B`/`OCF1B`), bit 2 of `TIFRn`.
const OCF_B: u8 = 1 << 2;

// ADCSRA bits.
const ADEN: u8 = 1 << 7;
const ADSC: u8 = 1 << 6;
const ADIF: u8 = 1 << 4;

// UCSR0A bits.
const TXC0: u8 = 1 << 6;
const UDRE0: u8 = 1 << 5;
const U2X0: u8 = 1 << 1;
// UCSR0B bits.
const TXEN0: u8 = 1 << 3;

// SPCR bits.
const SPE: u8 = 1 << 6;
const DORD: u8 = 1 << 5;
const MSTR: u8 = 1 << 4;
const CPOL: u8 = 1 << 3;
const CPHA: u8 = 1 << 2;
// SPSR bits.
const SPIF: u8 = 1 << 7;
const WCOL: u8 = 1 << 6;
const SPI2X: u8 = 1 << 0;

// SPI pin bits on Port B (ATmega328P): SS=PB2, MOSI=PB3, MISO=PB4, SCK=PB5.
const SPI_MOSI_BIT: u8 = 3;
const SPI_MISO_BIT: u8 = 4;
const SPI_SCK_BIT: u8 = 5;

// TWCR bits.
const TWINT: u8 = 1 << 7;
const TWSTA: u8 = 1 << 5;
const TWSTO: u8 = 1 << 4;
const TWEN: u8 = 1 << 2;

// TWI master status codes (the `TWS7:3` field of `TWSR`, prescaler bits masked).
const TW_START: u8 = 0x08;
const TW_REP_START: u8 = 0x10;
const TW_MT_SLA_ACK: u8 = 0x18;
const TW_MT_SLA_NACK: u8 = 0x20;
const TW_MT_DATA_ACK: u8 = 0x28;
const TW_MT_DATA_NACK: u8 = 0x30;

// TWI pin bits on Port C (ATmega328P): SDA=PC4, SCL=PC5.
const TWI_SDA_BIT: u8 = 4;
const TWI_SCL_BIT: u8 = 5;

/// Prescaler divisors indexed by the clock-select bits (`CSn2:0`). Index 0 means
/// the timer is stopped; 6 and 7 select external clock sources, modelled as
/// stopped.
const CS_DIV: [u32; 8] = [0, 1, 8, 64, 256, 1024, 0, 0];

/// One of the three GPIO ports.
#[derive(Debug, Clone, Copy, Default)]
struct PortReg {
    /// Data-direction register: bit set ⇒ output.
    ddr: u8,
    /// Output latch (and, for inputs, the pull-up enable).
    port: u8,
    /// Externally driven input level, for pins configured as inputs.
    pin_ext: u8,
    /// Per-bit override from a peripheral (e.g. a timer's PWM output).
    oc: [Option<bool>; 8],
}

impl PortReg {
    /// The logic level on pin `bit`. An **input** pin (`DDR` bit clear) reads its
    /// externally driven level. An **output** pin reads a peripheral override (a
    /// timer's PWM waveform) if one is active, else its output latch.
    ///
    /// The OC/PWM override only reaches the pin when the pin is an output: the
    /// ATmega328P datasheet requires the `DDR` bit for the OCnx pin to be set to
    /// enable the output driver, so a timer waveform on a pin left as an input is
    /// not visible on `PINx`. This keeps `pin_level` consistent with
    /// [`Peripherals::pin_drive`](crate::avr::Peripherals::pin_drive), which
    /// already gates on `DDR`.
    fn pin_level(&self, bit: u8) -> bool {
        if self.ddr & (1 << bit) == 0 {
            return self.pin_ext & (1 << bit) != 0;
        }
        if let Some(v) = self.oc[bit as usize] {
            return v;
        }
        self.port & (1 << bit) != 0
    }

    /// The whole `PINx` byte (every pin's resolved level).
    fn pin_byte(&self) -> u8 {
        let mut b = 0u8;
        for bit in 0..8 {
            if self.pin_level(bit) {
                b |= 1 << bit;
            }
        }
        b
    }
}

/// The 8-bit Timer/Counter0.
#[derive(Debug, Clone, Default)]
struct Timer8 {
    tccr_a: u8,
    tccr_b: u8,
    tcnt: u8,
    ocr_a: u8,
    ocr_b: u8,
    timsk: u8,
    /// Pending flags, laid out like `TIFR0` (`TOV`/`OCF_A`/`OCF_B`).
    tifr: u8,
    /// Accumulated CPU cycles not yet turned into timer ticks.
    prescale_acc: u32,
    /// Current OC0A / OC0B output levels while PWM drives them.
    oc_a_level: Option<bool>,
    oc_b_level: Option<bool>,
}

impl Timer8 {
    /// The waveform-generation mode (`WGM02:00`).
    fn wgm(&self) -> u8 {
        ((self.tccr_b >> 3) & 1) << 2 | ((self.tccr_a >> 1) & 1) << 1 | (self.tccr_a & 1)
    }

    /// The counter's TOP value for the current mode.
    fn top(&self) -> u8 {
        match self.wgm() {
            2 | 7 => self.ocr_a, // CTC / Fast-PWM with OCR0A as TOP
            _ => 0xFF,
        }
    }

    /// Advance by `cycles` CPU cycles.
    fn tick(&mut self, cycles: u32) {
        let div = CS_DIV[(self.tccr_b & 0x07) as usize];
        if div == 0 {
            self.refresh_outputs();
            return;
        }
        self.prescale_acc += cycles;
        while self.prescale_acc >= div {
            self.prescale_acc -= div;
            self.count();
        }
    }

    /// One counter tick: advance, wrap at TOP, and raise the matching flags.
    fn count(&mut self) {
        let wgm = self.wgm();
        let top = self.top();
        if self.tcnt >= top {
            self.tcnt = 0;
            match wgm {
                2 => self.tifr |= OCF_A, // CTC: the match at TOP
                _ => self.tifr |= TOV,   // Normal / Fast-PWM overflow
            }
        } else {
            self.tcnt = self.tcnt.wrapping_add(1);
        }
        if wgm != 2 && self.tcnt == self.ocr_a {
            self.tifr |= OCF_A;
        }
        if self.tcnt == self.ocr_b {
            self.tifr |= OCF_B;
        }
        self.refresh_outputs();
    }

    /// Recompute the Fast-PWM output levels for OC0A / OC0B.
    fn refresh_outputs(&mut self) {
        let fast_pwm = matches!(self.wgm(), 3 | 7);
        self.oc_a_level = pwm_level(fast_pwm, (self.tccr_a >> 6) & 3, self.tcnt, self.ocr_a);
        self.oc_b_level = pwm_level(fast_pwm, (self.tccr_a >> 4) & 3, self.tcnt, self.ocr_b);
    }

    /// The Fast-PWM duty on `chan` (fraction of the period the pin is high).
    fn duty(&self, chan: Chan) -> f64 {
        let (com, compare) = match chan {
            Chan::A => ((self.tccr_a >> 6) & 3, self.ocr_a),
            Chan::B => ((self.tccr_a >> 4) & 3, self.ocr_b),
        };
        duty_fraction(com, compare as u16, self.top() as u16)
    }

    /// Cycles until this timer's next modelled event (compare match or
    /// overflow/wrap), or `None` if it is stopped.
    fn next_event_in(&self) -> Option<u64> {
        let div = CS_DIV[(self.tccr_b & 0x07) as usize];
        if div == 0 {
            return None;
        }
        let top = self.top();
        let counts = counts_to_wrap(self.tcnt, top)
            .min(counts_to_match(self.tcnt, self.ocr_a, top))
            .min(counts_to_match(self.tcnt, self.ocr_b, top));
        Some(counts as u64 * div as u64 - self.prescale_acc as u64)
    }
}

/// The 16-bit Timer/Counter1.
#[derive(Debug, Clone, Default)]
struct Timer16 {
    tccr_a: u8,
    tccr_b: u8,
    tccr_c: u8,
    tcnt: u16,
    ocr_a: u16,
    ocr_b: u16,
    icr: u16,
    timsk: u8,
    tifr: u8,
    /// The shared high-byte temporary register used for 16-bit writes.
    temp: u8,
    prescale_acc: u32,
    oc_a_level: Option<bool>,
    oc_b_level: Option<bool>,
}

impl Timer16 {
    /// The waveform-generation mode (`WGM13:10`).
    fn wgm(&self) -> u8 {
        ((self.tccr_b >> 4) & 1) << 3
            | ((self.tccr_b >> 3) & 1) << 2
            | ((self.tccr_a >> 1) & 1) << 1
            | (self.tccr_a & 1)
    }

    fn top(&self) -> u16 {
        match self.wgm() {
            4 | 15 => self.ocr_a,
            12 | 14 => self.icr,
            5 => 0x00FF,
            6 => 0x01FF,
            7 => 0x03FF,
            _ => 0xFFFF,
        }
    }

    fn tick(&mut self, cycles: u32) {
        let div = CS_DIV[(self.tccr_b & 0x07) as usize];
        if div == 0 {
            self.refresh_outputs();
            return;
        }
        self.prescale_acc += cycles;
        while self.prescale_acc >= div {
            self.prescale_acc -= div;
            self.count();
        }
    }

    fn count(&mut self) {
        let wgm = self.wgm();
        let top = self.top();
        if self.tcnt >= top {
            self.tcnt = 0;
            match wgm {
                4 | 12 => self.tifr |= OCF_A, // CTC: the match at TOP
                _ => self.tifr |= TOV,
            }
        } else {
            self.tcnt = self.tcnt.wrapping_add(1);
        }
        if !matches!(wgm, 4 | 12) && self.tcnt == self.ocr_a {
            self.tifr |= OCF_A;
        }
        if self.tcnt == self.ocr_b {
            self.tifr |= OCF_B;
        }
        self.refresh_outputs();
    }

    fn refresh_outputs(&mut self) {
        let fast_pwm = matches!(self.wgm(), 5 | 6 | 7 | 14 | 15);
        let top = self.top();
        self.oc_a_level = pwm_level16(fast_pwm, (self.tccr_a >> 6) & 3, self.tcnt, self.ocr_a, top);
        self.oc_b_level = pwm_level16(fast_pwm, (self.tccr_a >> 4) & 3, self.tcnt, self.ocr_b, top);
    }

    /// The Fast-PWM duty on `chan` (fraction of the period the pin is high).
    fn duty(&self, chan: Chan) -> f64 {
        let (com, compare) = match chan {
            Chan::A => ((self.tccr_a >> 6) & 3, self.ocr_a),
            Chan::B => ((self.tccr_a >> 4) & 3, self.ocr_b),
        };
        duty_fraction(com, compare, self.top())
    }

    /// Cycles until this timer's next modelled event, or `None` if stopped.
    fn next_event_in(&self) -> Option<u64> {
        let div = CS_DIV[(self.tccr_b & 0x07) as usize];
        if div == 0 {
            return None;
        }
        let top = self.top();
        let counts = counts_to_wrap16(self.tcnt, top)
            .min(counts_to_match16(self.tcnt, self.ocr_a, top))
            .min(counts_to_match16(self.tcnt, self.ocr_b, top));
        Some(counts * div as u64 - self.prescale_acc as u64)
    }
}

/// Fast-PWM output level for an 8-bit timer, given the two `COM` bits.
///
/// Non-inverting (`COM == 2`): high while `count < compare`. Inverting
/// (`COM == 3`): the complement. `COM == 0` disconnects the pin (no override);
/// `COM == 1` (toggle) is not modelled as a PWM output.
fn pwm_level(fast_pwm: bool, com: u8, count: u8, compare: u8) -> Option<bool> {
    if !fast_pwm {
        return None;
    }
    let high = compare != 0 && count < compare;
    match com {
        2 => Some(high),
        3 => Some(!high),
        _ => None,
    }
}

/// Fast-PWM output level for the 16-bit timer (see [`pwm_level`]).
fn pwm_level16(fast_pwm: bool, com: u8, count: u16, compare: u16, top: u16) -> Option<bool> {
    if !fast_pwm {
        return None;
    }
    let high = compare != 0 && count < compare && count <= top;
    match com {
        2 => Some(high),
        3 => Some(!high),
        _ => None,
    }
}

/// Fast-PWM duty (fraction of the period the pin is high) for the given `COM`
/// bits, compare value and TOP. Non-inverting (`COM == 2`, and any value with an
/// active output) is `compare / (TOP + 1)`; inverting (`COM == 3`) is the
/// complement.
fn duty_fraction(com: u8, compare: u16, top: u16) -> f64 {
    if top == 0 {
        return 0.0;
    }
    let d = (compare as f64 / (top as f64 + 1.0)).clamp(0.0, 1.0);
    match com {
        3 => 1.0 - d,
        _ => d,
    }
}

/// Counts until an 8-bit up-counter at `cnt` (TOP `top`) next wraps through zero.
fn counts_to_wrap(cnt: u8, top: u8) -> u32 {
    if cnt >= top {
        1
    } else {
        (top - cnt) as u32 + 1
    }
}

/// Counts until an 8-bit up-counter at `cnt` (TOP `top`) next becomes `tgt`, or a
/// large sentinel if `tgt` lies above TOP and is never reached.
fn counts_to_match(cnt: u8, tgt: u8, top: u8) -> u32 {
    if tgt > top {
        return u32::MAX;
    }
    if cnt < top {
        if tgt > cnt {
            (tgt - cnt) as u32
        } else {
            (top - cnt) as u32 + 1 + tgt as u32
        }
    } else {
        // At or past TOP the next count resets to zero.
        1 + tgt as u32
    }
}

/// [`counts_to_wrap`] for the 16-bit timer.
fn counts_to_wrap16(cnt: u16, top: u16) -> u64 {
    if cnt >= top {
        1
    } else {
        (top - cnt) as u64 + 1
    }
}

/// [`counts_to_match`] for the 16-bit timer.
fn counts_to_match16(cnt: u16, tgt: u16, top: u16) -> u64 {
    if tgt > top {
        return u64::MAX;
    }
    if cnt < top {
        if tgt > cnt {
            (tgt - cnt) as u64
        } else {
            (top - cnt) as u64 + 1 + tgt as u64
        }
    } else {
        1 + tgt as u64
    }
}

/// The successive-approximation ADC.
#[derive(Debug, Clone)]
struct Adc {
    adcsra: u8,
    adcsrb: u8,
    admux: u8,
    /// The last conversion result (right-adjusted 10-bit code).
    result: u16,
    /// CPU cycles remaining in an in-flight conversion (`0` ⇒ idle).
    countdown: u32,
    /// Per-channel input voltage.
    inputs: [f64; 8],
    /// Reference voltage used to scale the input to a code.
    vref: f64,
}

impl Default for Adc {
    fn default() -> Self {
        Self {
            adcsra: 0,
            adcsrb: 0,
            admux: 0,
            result: 0,
            countdown: 0,
            inputs: [0.0; 8],
            vref: 5.0,
        }
    }
}

impl Adc {
    /// Begin a conversion if `ADEN` and `ADSC` are set and none is in flight.
    fn maybe_start(&mut self) {
        if self.adcsra & ADEN != 0 && self.adcsra & ADSC != 0 && self.countdown == 0 {
            // Prescaler: ADPS2:0 → 2,2,4,8,16,32,64,128. A normal conversion is
            // 13 ADC clocks; we ignore the longer first-conversion case.
            let adps = (self.adcsra & 0x07) as u32;
            let prescale = 1u32 << adps.max(1);
            self.countdown = 13 * prescale;
        }
    }

    fn tick(&mut self, cycles: u32) {
        if self.countdown == 0 {
            return;
        }
        self.countdown = self.countdown.saturating_sub(cycles);
        if self.countdown == 0 {
            let ch = (self.admux & 0x0F).min(7) as usize;
            self.result = sar_convert(self.inputs[ch], self.vref);
            self.adcsra &= !ADSC; // conversion complete
            self.adcsra |= ADIF;
        }
    }

    /// `ADCL`/`ADCH` honouring the `ADLAR` left-adjust bit.
    fn data_low(&self) -> u8 {
        if self.admux & (1 << 5) != 0 {
            (self.result << 6) as u8
        } else {
            self.result as u8
        }
    }
    fn data_high(&self) -> u8 {
        if self.admux & (1 << 5) != 0 {
            (self.result >> 2) as u8
        } else {
            (self.result >> 8) as u8
        }
    }
}

/// A 10-bit successive-approximation conversion of `v` volts against `vref`.
///
/// Mirrors the datasheet's `ADC = Vin · 1024 / Vref`, converging bit-by-bit from
/// the MSB and clamping to the 10-bit range.
fn sar_convert(v: f64, vref: f64) -> u16 {
    if vref <= 0.0 {
        return 0;
    }
    let mut code = 0u16;
    for bit in (0..10).rev() {
        let trial = code | (1 << bit);
        if (trial as f64) * vref / 1024.0 <= v {
            code = trial;
        }
    }
    code.min(1023)
}

/// The USART0 transmitter (plus a minimal receive buffer).
#[derive(Debug, Clone)]
struct Usart {
    ucsr_a: u8,
    ucsr_b: u8,
    ucsr_c: u8,
    ubrr: u16,
    /// The shared high-byte temp for the 16-bit `UBRR0` write.
    ubrr_temp: u8,
    /// The byte currently being shifted out and the cycles left in its frame.
    tx_shift: Option<u8>,
    tx_remaining: u32,
    /// The one-deep `UDR0` transmit holding buffer.
    tx_buffer: Option<u8>,
    /// Every byte the transmitter has shifted out, in order.
    tx_capture: Vec<u8>,
    /// Minimal RX: the last injected byte.
    rx_data: u8,
}

impl Default for Usart {
    fn default() -> Self {
        Self {
            // UDRE0 set at reset: the transmit buffer starts empty.
            ucsr_a: UDRE0,
            ucsr_b: 0,
            // 8-bit character size (UCSZ01:0 = 11) is the reset default.
            ucsr_c: 0x06,
            ubrr: 0,
            ubrr_temp: 0,
            tx_shift: None,
            tx_remaining: 0,
            tx_buffer: None,
            tx_capture: Vec::new(),
            rx_data: 0,
        }
    }
}

impl Usart {
    /// The number of CPU cycles one frame occupies at the current configuration.
    fn frame_cycles(&self) -> u32 {
        let div = if self.ucsr_a & U2X0 != 0 { 8 } else { 16 };
        let bit_cycles = div * (self.ubrr as u32 + 1);
        // Character size: UCSZ02 (UCSR0B bit 2) and UCSZ01:0 (UCSR0C bits 2:1).
        let ucsz = ((self.ucsr_b >> 2) & 1) << 2 | ((self.ucsr_c >> 1) & 3);
        let data_bits = match ucsz {
            0 => 5,
            1 => 6,
            2 => 7,
            _ => 8, // 3 ⇒ 8-bit; 9-bit (7) is modelled as 8 for frame timing
        };
        let stop_bits = if self.ucsr_c & (1 << 3) != 0 { 2 } else { 1 };
        // Parity: UPM01 (UCSR0C bit 5) enables it.
        let parity = if self.ucsr_c & (1 << 5) != 0 { 1 } else { 0 };
        let frame_bits = 1 + data_bits + parity + stop_bits;
        frame_bits * bit_cycles
    }

    /// Accept a byte written to `UDR0`.
    fn write_udr(&mut self, byte: u8) {
        if self.ucsr_b & TXEN0 == 0 {
            return; // transmitter disabled
        }
        if self.tx_shift.is_none() {
            self.tx_shift = Some(byte);
            self.tx_remaining = self.frame_cycles();
            self.ucsr_a &= !TXC0;
        } else {
            self.tx_buffer = Some(byte);
            self.ucsr_a &= !UDRE0; // holding buffer now full
        }
    }

    fn tick(&mut self, cycles: u32) {
        if self.tx_shift.is_none() {
            return;
        }
        self.tx_remaining = self.tx_remaining.saturating_sub(cycles);
        if self.tx_remaining == 0 {
            let done = self.tx_shift.take().unwrap();
            self.tx_capture.push(done);
            self.ucsr_a |= TXC0;
            if let Some(next) = self.tx_buffer.take() {
                self.tx_shift = Some(next);
                self.tx_remaining = self.frame_cycles();
            }
            self.ucsr_a |= UDRE0; // buffer is empty again
        }
    }
}

/// One SCK transition captured during an SPI master transfer.
///
/// Every half-period the SPI shifts, it records the resulting SCK level, the
/// MOSI level the master is presenting, and whether this is the bit's sampling
/// (leading) edge — the point at which a slave latches MOSI and the master
/// latches MISO. The gate asserts the captured leading-edge MOSI bits
/// reconstruct the byte written to `SPDR`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpiEdge {
    /// The SCK level after this transition.
    pub sck: bool,
    /// The MOSI level the master drives across this transition.
    pub mosi: bool,
    /// Whether this is the bit's sampling (leading) edge.
    pub sample: bool,
}

/// The SPI master: SPCR/SPSR/SPDR driving SCK/MOSI out and sampling MISO in.
///
/// A write to `SPDR` (with the SPI enabled as master) starts an 8-bit transfer.
/// Each half of the configured SCK period the transfer advances one edge,
/// toggling SCK and — on sampling edges — latching MISO into the receive
/// assembly; the completed byte is captured and `SPIF` is raised. The whole
/// thing is driven from [`Peripherals::tick`], so it stays deterministic and
/// wall-clock free like the timers and USART.
#[derive(Debug, Clone, Default)]
struct Spi {
    spcr: u8,
    spsr: u8,
    /// The `SPDR` read value: the byte assembled from MISO by the last transfer.
    data: u8,
    /// The byte currently being shifted out (held for the whole transfer).
    tx_byte: u8,
    /// MISO bits latched so far this transfer (MSB first).
    rx_assemble: u8,
    busy: bool,
    /// Bits whose trailing edge has completed (`0..8`).
    bit_slot: u8,
    /// Whether the next edge is the leading edge of the current bit.
    next_leading: bool,
    /// The current SCK level.
    sck: bool,
    /// CPU cycles accumulated toward the next half-period edge.
    half_acc: u32,
    /// The externally presented MISO level (sampled from PB4).
    miso_in: bool,
    /// Every SCK edge of every transfer, in order.
    edges: Vec<SpiEdge>,
    /// Completed shifted-out bytes, in order.
    tx_capture: Vec<u8>,
}

impl Spi {
    /// Whether the SPI is enabled and configured as a master (the only mode that
    /// drives SCK/MOSI).
    fn enabled_master(&self) -> bool {
        self.spcr & SPE != 0 && self.spcr & MSTR != 0
    }

    /// The SCK period in CPU cycles for the current `SPR1:0` and `SPI2X`.
    fn clock_divisor(&self) -> u32 {
        let base = match self.spcr & 0x03 {
            0 => 4,
            1 => 16,
            2 => 64,
            _ => 128,
        };
        if self.spsr & SPI2X != 0 {
            base / 2
        } else {
            base
        }
    }

    /// Cycles in one SCK half-period (at least one).
    fn half_period(&self) -> u32 {
        (self.clock_divisor() / 2).max(1)
    }

    /// The SCK idle level (`CPOL`).
    fn idle_sck(&self) -> bool {
        self.spcr & CPOL != 0
    }

    /// The MOSI bit for the current bit slot, honouring `DORD` (0 ⇒ MSB first).
    fn mosi_bit(&self) -> bool {
        let slot = self.bit_slot.min(7);
        let bit = if self.spcr & DORD != 0 {
            slot
        } else {
            7 - slot
        };
        self.tx_byte & (1 << bit) != 0
    }

    /// Accept a byte written to `SPDR`.
    fn write_data(&mut self, byte: u8) {
        if !self.enabled_master() {
            // Not an enabled master: just latch the byte (slave data / scratch).
            self.data = byte;
            return;
        }
        if self.busy {
            self.spsr |= WCOL; // write collision mid-transfer
            return;
        }
        self.tx_byte = byte;
        self.rx_assemble = 0;
        self.busy = true;
        self.bit_slot = 0;
        self.next_leading = true;
        self.sck = self.idle_sck();
        self.half_acc = 0;
        self.spsr &= !SPIF;
    }

    fn tick(&mut self, cycles: u32) {
        if !self.busy {
            return;
        }
        let half = self.half_period();
        self.half_acc += cycles;
        while self.busy && self.half_acc >= half {
            self.half_acc -= half;
            self.edge();
        }
    }

    /// Advance one SCK edge: toggle the clock, latch MISO on the sampling edge,
    /// record the edge, and complete the byte on the eighth trailing edge.
    fn edge(&mut self) {
        self.sck = !self.sck;
        let cpha = self.spcr & CPHA != 0;
        let leading = self.next_leading;
        // CPHA = 0 samples on the leading edge; CPHA = 1 on the trailing edge.
        let sample = if cpha { !leading } else { leading };
        let mosi = self.mosi_bit();
        if sample {
            self.rx_assemble = (self.rx_assemble << 1) | self.miso_in as u8;
        }
        self.edges.push(SpiEdge {
            sck: self.sck,
            mosi,
            sample,
        });
        if !leading {
            self.bit_slot += 1;
            if self.bit_slot >= 8 {
                self.busy = false;
                self.data = self.rx_assemble;
                self.spsr |= SPIF;
                self.tx_capture.push(self.tx_byte);
                self.sck = self.idle_sck();
            }
        }
        self.next_leading = !self.next_leading;
    }

    /// Cycles until the next modelled SPI edge, or `None` if idle.
    fn next_event_in(&self) -> Option<u64> {
        if !self.busy {
            return None;
        }
        Some((self.half_period() - self.half_acc) as u64)
    }
}

/// A logical event decoded from the TWI master's SDA/SCL line activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TwiEvent {
    /// A START condition (SDA pulled low while SCL is high).
    Start,
    /// A repeated START (a START issued without an intervening STOP).
    RepeatedStart,
    /// A byte the master clocked out, MSB first, with the acknowledge bit it
    /// sampled on the ninth clock (`true` ⇒ ACK).
    Byte {
        /// The transmitted byte (an address+R/W for the first byte after START).
        value: u8,
        /// The acknowledge the master sampled.
        ack: bool,
    },
    /// A STOP condition (SDA released high while SCL is high).
    Stop,
}

/// One `(SDA, SCL)` line state, captured whenever either line changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TwiLine {
    /// The SDA level.
    pub sda: bool,
    /// The SCL level.
    pub scl: bool,
}

/// A TWI master operation awaiting its cycle countdown before it takes effect.
#[derive(Debug, Clone, Copy)]
enum TwiOp {
    Start,
    SendByte(u8),
    Stop,
}

/// The two-wire (I²C) master: a TWBR/TWCR/TWDR/TWSR state machine that generates
/// START / addr / data / STOP activity on SDA/SCL.
///
/// Each operation is requested by writing `TWCR` with `TWINT` set (clearing the
/// flag) plus `TWEN`; it then consumes its SCL-clock time from
/// [`Peripherals::tick`] before taking effect, raising `TWINT` again and posting
/// the matching status code. START/addr/data/STOP are rendered as an SDA/SCL
/// line trace and a decoded [`TwiEvent`] stream for inspection.
#[derive(Debug, Clone)]
struct Twi {
    twbr: u8,
    /// Status (`TWS7:3`) plus the prescaler bits (`TWPS1:0`).
    twsr: u8,
    twar: u8,
    twdr: u8,
    twcr: u8,
    twamr: u8,
    /// Whether a START has been issued (so the next START is a repeated START,
    /// and the next byte is an address rather than data).
    started: bool,
    /// Whether the next transmitted byte is the slave address+R/W.
    addr_phase: bool,
    /// Current SDA/SCL line levels (idle = both released high).
    sda: bool,
    scl: bool,
    /// The pending operation and the CPU cycles left before it completes.
    op: Option<TwiOp>,
    countdown: u32,
    /// The acknowledge a modelled slave returns (ACK every byte by default).
    ack_in: bool,
    /// Decoded START/byte/STOP events, in order.
    events: Vec<TwiEvent>,
    /// Every `(SDA, SCL)` line state, in order (consecutive duplicates merged).
    lines: Vec<TwiLine>,
}

impl Default for Twi {
    fn default() -> Self {
        Self {
            twbr: 0,
            twsr: 0,
            twar: 0,
            twdr: 0,
            twcr: 0,
            twamr: 0,
            started: false,
            addr_phase: false,
            sda: true,
            scl: true,
            op: None,
            countdown: 0,
            ack_in: true,
            events: Vec::new(),
            lines: Vec::new(),
        }
    }
}

impl Twi {
    /// The SCL bit period in CPU cycles: `16 + 2·TWBR·prescaler`.
    fn scl_period(&self) -> u32 {
        let twps = (self.twsr & 0x03) as u32;
        let prescale = 1u32 << (2 * twps); // 1, 4, 16, 64
        (16 + 2 * self.twbr as u32 * prescale).max(2)
    }

    /// Set the status field (`TWS7:3`), preserving the prescaler bits.
    fn set_status(&mut self, code: u8) {
        self.twsr = (self.twsr & 0x03) | (code & 0xF8);
    }

    /// Handle a write to `TWCR`: `TWINT` is cleared by writing a 1 to it, which
    /// launches the operation the other bits request.
    fn write_twcr(&mut self, val: u8) {
        let launch = val & TWINT != 0 && val & TWEN != 0;
        // Store the control byte with TWINT cleared: the hardware re-raises it
        // when the operation finishes.
        self.twcr = val & !TWINT;
        if !launch || self.op.is_some() {
            return;
        }
        if val & TWSTA != 0 {
            self.op = Some(TwiOp::Start);
            self.countdown = self.scl_period();
        } else if val & TWSTO != 0 {
            self.op = Some(TwiOp::Stop);
            self.countdown = self.scl_period();
        } else {
            // Transmit the byte currently in TWDR (8 data clocks + 1 ACK clock).
            self.op = Some(TwiOp::SendByte(self.twdr));
            self.countdown = 9 * self.scl_period();
        }
    }

    fn tick(&mut self, cycles: u32) {
        if self.op.is_none() {
            return;
        }
        self.countdown = self.countdown.saturating_sub(cycles);
        if self.countdown == 0 {
            self.complete_op();
        }
    }

    fn complete_op(&mut self) {
        match self.op.take().expect("complete_op with no pending op") {
            TwiOp::Start => {
                self.emit_start();
                let repeated = self.started;
                self.started = true;
                self.addr_phase = true;
                self.events.push(if repeated {
                    TwiEvent::RepeatedStart
                } else {
                    TwiEvent::Start
                });
                self.set_status(if repeated { TW_REP_START } else { TW_START });
                self.twcr |= TWINT;
            }
            TwiOp::SendByte(b) => {
                let ack = self.ack_in;
                self.emit_byte(b, ack);
                self.events.push(TwiEvent::Byte { value: b, ack });
                let status = if self.addr_phase {
                    if ack {
                        TW_MT_SLA_ACK
                    } else {
                        TW_MT_SLA_NACK
                    }
                } else if ack {
                    TW_MT_DATA_ACK
                } else {
                    TW_MT_DATA_NACK
                };
                self.addr_phase = false;
                self.set_status(status);
                self.twcr |= TWINT;
            }
            TwiOp::Stop => {
                self.emit_stop();
                self.started = false;
                self.addr_phase = false;
                self.events.push(TwiEvent::Stop);
                // A STOP clears TWSTO and does not raise TWINT.
                self.twcr &= !TWSTO;
            }
        }
    }

    /// Record the current line state, merging consecutive duplicates.
    fn push_line(&mut self) {
        let l = TwiLine {
            sda: self.sda,
            scl: self.scl,
        };
        if self.lines.last() != Some(&l) {
            self.lines.push(l);
        }
    }

    /// START: SDA falls while SCL is high, then SCL drops.
    fn emit_start(&mut self) {
        // Release the bus first (needed for a repeated START from mid-byte).
        self.sda = true;
        self.scl = true;
        self.push_line();
        self.sda = false; // SDA 1→0 while SCL high = START
        self.push_line();
        self.scl = false;
        self.push_line();
    }

    /// Clock out eight data bits (MSB first) then the ACK bit.
    fn emit_byte(&mut self, b: u8, ack: bool) {
        for i in (0..8).rev() {
            self.sda = b & (1 << i) != 0;
            self.scl = false;
            self.push_line();
            self.scl = true; // data valid on the rising SCL edge
            self.push_line();
            self.scl = false;
            self.push_line();
        }
        // ACK slot: the master releases SDA, a slave pulls it low to acknowledge.
        self.sda = !ack;
        self.scl = false;
        self.push_line();
        self.scl = true; // ACK sampled
        self.push_line();
        self.scl = false;
        self.push_line();
    }

    /// STOP: SDA rises while SCL is high.
    fn emit_stop(&mut self) {
        self.sda = false;
        self.scl = false;
        self.push_line();
        self.scl = true;
        self.push_line();
        self.sda = true; // SDA 0→1 while SCL high = STOP
        self.push_line();
    }

    /// Cycles until the pending operation completes, or `None` if idle.
    fn next_event_in(&self) -> Option<u64> {
        self.op.map(|_| self.countdown as u64)
    }
}

/// The AVR core's memory-mapped peripherals.
///
/// The live register file (GPIO ports B/C/D and Timer0/Timer1 at their
/// ATmega328P I/O addresses) is the shared subset reused across every variant;
/// `port_count` records how many of those ports the current part actually bonds
/// out, so the co-sim exchange only samples and publishes real pins (see
/// [`variant`](crate::avr::variant) for the declared-vs-modelled split).
#[derive(Debug, Clone, Default)]
pub struct Peripherals {
    ports: [PortReg; 3],
    /// Number of GPIO ports this part bonds out (sizes the [`PinIo`] exchange
    /// and bounds the live-port iteration). `1` for a single-port part, up to
    /// `PORTS.len()` modelled register-decoded ports.
    port_count: usize,
    timer0: Timer8,
    timer1: Timer16,
    adc: Adc,
    usart: Usart,
    spi: Spi,
    twi: Twi,
    /// Digital-input boundary: how a sampled node voltage becomes a logic level.
    thresholds: InputThresholds,
    /// Digital-output boundary: how a driven pin becomes a Thévenin source.
    drive: OutputDrive,
}

impl Peripherals {
    /// A reset peripheral set for the default three-port (ATmega328P) map.
    pub fn new() -> Self {
        Self::with_port_count(PORTS.len())
    }

    /// A reset peripheral set bonding out `port_count` GPIO ports. The co-sim
    /// [`PinIo`] exchange is sized to `port_count * 8`; only the first
    /// `port_count` ports (of the modelled B/C/D set) are sampled and published.
    pub fn with_port_count(port_count: usize) -> Self {
        Self {
            port_count,
            usart: Usart::default(),
            adc: Adc::default(),
            thresholds: InputThresholds::default(),
            drive: OutputDrive::default(),
            ..Default::default()
        }
    }

    /// The number of pins in this part's [`PinIo`] exchange: eight per bonded
    /// port.
    pub fn pin_count(&self) -> usize {
        self.port_count * 8
    }

    /// The live, register-decoded ports to sample/publish: the first
    /// `port_count` of the modelled B/C/D set.
    fn live_port_count(&self) -> usize {
        self.port_count.min(PORTS.len())
    }

    fn port_mut(&mut self, p: Port) -> &mut PortReg {
        &mut self.ports[p as usize]
    }
    fn port(&self, p: Port) -> &PortReg {
        &self.ports[p as usize]
    }

    // --- Inspection / control (used by the board layer and tests) ---

    /// The resolved logic level on `port` pin `bit`.
    pub fn pin(&self, port: Port, bit: u8) -> bool {
        self.port(port).pin_level(bit)
    }

    /// What `port` pin `bit` drives out to the circuit (the co-sim exchange's
    /// *out* half).
    ///
    /// A pin configured as an output drives its PORT latch (or, if a timer owns
    /// it, a PWM waveform). A pin configured as an input drives nothing
    /// ([`PinDrive::HighZ`]) — it only reads its input voltage. (Input pull-ups
    /// are left to the mixed-signal boundary at `CP-BRD-M2.2`.)
    pub fn pin_drive(&self, port: Port, bit: u8) -> PinDrive {
        let p = self.port(port);
        if p.ddr & (1 << bit) == 0 {
            return PinDrive::HighZ;
        }
        if let Some(level) = p.oc[bit as usize] {
            // A timer owns this pin: report its PWM duty alongside the level.
            let duty = self
                .oc_duty(port, bit)
                .unwrap_or(if level { 1.0 } else { 0.0 });
            return PinDrive::Pwm { duty, level };
        }
        PinDrive::Digital(p.port & (1 << bit) != 0)
    }

    /// The PWM duty on an output-compare pin, mapping the Uno's OC pins to their
    /// timer channels: OC0A→PD6, OC0B→PD5, OC1A→PB1, OC1B→PB2.
    fn oc_duty(&self, port: Port, bit: u8) -> Option<f64> {
        match (port, bit) {
            (Port::D, 6) => Some(self.timer0.duty(Chan::A)),
            (Port::D, 5) => Some(self.timer0.duty(Chan::B)),
            (Port::B, 1) => Some(self.timer1.duty(Chan::A)),
            (Port::B, 2) => Some(self.timer1.duty(Chan::B)),
            _ => None,
        }
    }

    /// Sample the co-sim exchange's input voltages into the peripherals: each
    /// pin's voltage resolves to a digital-input level through the configurable
    /// [`InputThresholds`] (with hysteresis — the pin's current level is the
    /// held state for a voltage inside the band), and each Port C pin
    /// additionally feeds its ADC channel. Called at the top of a co-sim step.
    pub fn sample_inputs(&mut self, io: &PinIo) {
        for &port in &PORTS[..self.live_port_count()] {
            for bit in 0..8u8 {
                let v = io.input(pin_id(port, bit));
                // The input's last settled level is the hysteresis memory: a
                // voltage inside the band holds it rather than chattering.
                let previous = if self.port(port).pin_ext & (1 << bit) != 0 {
                    Logic::High
                } else {
                    Logic::Low
                };
                match self.thresholds.resolve(v, previous) {
                    Logic::High => self.set_pin_input(port, bit, true),
                    Logic::Low => self.set_pin_input(port, bit, false),
                    // Unknown/floating in the band with no history: leave the
                    // externally driven level untouched.
                    _ => {}
                }
                if port == Port::C {
                    self.set_adc_input(bit, v);
                }
            }
        }
        // The SPI master latches MISO (PB4) from the resolved pin level.
        self.spi.miso_in = self.port(Port::B).pin_level(SPI_MISO_BIT);
    }

    /// The digital-input thresholds this core samples node voltages through.
    pub fn input_thresholds(&self) -> InputThresholds {
        self.thresholds
    }

    /// Configure the digital-input thresholds (logic-low/high + hysteresis band).
    pub fn set_input_thresholds(&mut self, thresholds: InputThresholds) {
        self.thresholds = thresholds;
    }

    /// The output-drive characteristics driven pins present to the circuit.
    pub fn output_drive(&self) -> OutputDrive {
        self.drive
    }

    /// Configure the output-drive characteristics (rail, source/sink R, slew).
    pub fn set_output_drive(&mut self, drive: OutputDrive) {
        self.drive = drive;
    }

    /// The Thévenin source `port` pin `bit` presents to its node this step
    /// (open-circuit voltage + series resistance), or `None` for a
    /// high-impedance pin. The co-sim loop stamps this onto the node instead of
    /// the ideal rail the M2.1 bridge used.
    pub fn pin_thevenin(&self, port: Port, bit: u8) -> Option<Thevenin> {
        self.drive.thevenin(self.pin_drive(port, bit))
    }

    /// Publish each pin's drive into the co-sim exchange's *out* half. Called at
    /// the end of a co-sim step.
    pub fn publish_drives(&self, io: &mut PinIo) {
        for &port in &PORTS[..self.live_port_count()] {
            for bit in 0..8u8 {
                io.set_drive(pin_id(port, bit), self.pin_drive(port, bit));
            }
        }
    }

    /// Cycles from now until the soonest modelled peripheral event (a timer
    /// compare/overflow, the USART finishing its current frame, or an ADC
    /// conversion completing), or `None` if nothing is scheduled.
    pub fn next_event_in(&self) -> Option<u64> {
        let mut best: Option<u64> = None;
        let mut consider = |d: Option<u64>| {
            if let Some(d) = d {
                best = Some(best.map_or(d, |b| b.min(d)));
            }
        };
        consider(self.timer0.next_event_in());
        consider(self.timer1.next_event_in());
        if self.adc.countdown > 0 {
            consider(Some(self.adc.countdown as u64));
        }
        if self.usart.tx_remaining > 0 {
            consider(Some(self.usart.tx_remaining as u64));
        }
        consider(self.spi.next_event_in());
        consider(self.twi.next_event_in());
        best
    }

    /// Drive an external input level onto `port` pin `bit` (for pins the
    /// firmware has configured as inputs).
    pub fn set_pin_input(&mut self, port: Port, bit: u8, level: bool) {
        let mask = 1 << bit;
        if level {
            self.port_mut(port).pin_ext |= mask;
        } else {
            self.port_mut(port).pin_ext &= !mask;
        }
    }

    /// Set the analog input voltage presented on ADC `channel` (`0..8`).
    pub fn set_adc_input(&mut self, channel: u8, volts: f64) {
        if let Some(slot) = self.adc.inputs.get_mut(channel as usize) {
            *slot = volts;
        }
    }

    /// Set the ADC reference voltage (default 5 V).
    pub fn set_adc_vref(&mut self, volts: f64) {
        self.adc.vref = volts;
    }

    /// The bytes the USART transmitter has shifted out so far.
    pub fn usart_tx(&self) -> &[u8] {
        &self.usart.tx_capture
    }

    /// Inject a received byte into the (minimal) USART receiver.
    pub fn usart_rx_push(&mut self, byte: u8) {
        self.usart.rx_data = byte;
        self.usart.ucsr_a |= 1 << 7; // RXC0
    }

    // --- SPI inspection / control ---

    /// Every SCK edge the SPI master has clocked, in order (SCK/MOSI levels and
    /// whether each is a sampling edge).
    pub fn spi_edges(&self) -> &[SpiEdge] {
        &self.spi.edges
    }

    /// The bytes the SPI master has shifted out (one per completed transfer).
    pub fn spi_tx(&self) -> &[u8] {
        &self.spi.tx_capture
    }

    /// The current `SPDR` read value: the byte assembled from MISO by the last
    /// completed transfer.
    pub fn spi_data(&self) -> u8 {
        self.spi.data
    }

    /// Whether an SPI transfer is in progress.
    pub fn spi_busy(&self) -> bool {
        self.spi.busy
    }

    /// Drive the externally presented MISO level (for pure-peripheral tests that
    /// do not run the full co-sim pin exchange).
    pub fn set_spi_miso(&mut self, level: bool) {
        self.spi.miso_in = level;
    }

    // --- TWI inspection / control ---

    /// The TWI master's decoded START / byte / STOP event stream, in order.
    pub fn twi_events(&self) -> &[TwiEvent] {
        &self.twi.events
    }

    /// Every `(SDA, SCL)` line state the TWI master has driven, in order.
    pub fn twi_lines(&self) -> &[TwiLine] {
        &self.twi.lines
    }

    /// Set the acknowledge a modelled slave returns to the TWI master (default
    /// ACK). Clearing it makes the next byte read back as a NACK.
    pub fn set_twi_slave_ack(&mut self, ack: bool) {
        self.twi.ack_in = ack;
    }

    // --- Cycle-driven advance ---

    /// Advance every peripheral by `cycles` CPU cycles, then republish timer PWM
    /// outputs onto their OC pins.
    pub fn tick(&mut self, cycles: u64) {
        let cycles = cycles as u32;
        self.timer0.tick(cycles);
        self.timer1.tick(cycles);
        self.adc.tick(cycles);
        self.usart.tick(cycles);
        self.spi.tick(cycles);
        self.twi.tick(cycles);
        self.apply_oc_outputs();
        self.apply_spi_outputs();
        self.apply_twi_outputs();
    }

    /// Route the timers' PWM outputs to their OC pins: OC0A→PD6, OC0B→PD5,
    /// OC1A→PB1, OC1B→PB2.
    fn apply_oc_outputs(&mut self) {
        self.ports[Port::D as usize].oc[6] = self.timer0.oc_a_level;
        self.ports[Port::D as usize].oc[5] = self.timer0.oc_b_level;
        self.ports[Port::B as usize].oc[1] = self.timer1.oc_a_level;
        self.ports[Port::B as usize].oc[2] = self.timer1.oc_b_level;
    }

    /// Route the SPI master's SCK (PB5) and MOSI (PB3) onto their Port B pins
    /// while the SPI is enabled as a master (the firmware still has to select
    /// those pins as outputs via `DDRB`, matching the datasheet).
    fn apply_spi_outputs(&mut self) {
        let (sck, mosi) = if self.spi.enabled_master() {
            (Some(self.spi.sck), Some(self.spi.mosi_bit()))
        } else {
            (None, None)
        };
        self.ports[Port::B as usize].oc[SPI_SCK_BIT as usize] = sck;
        self.ports[Port::B as usize].oc[SPI_MOSI_BIT as usize] = mosi;
    }

    /// Route the TWI master's SDA (PC4) and SCL (PC5) onto their Port C pins
    /// while the TWI is enabled.
    fn apply_twi_outputs(&mut self) {
        let (sda, scl) = if self.twi.twcr & TWEN != 0 {
            (Some(self.twi.sda), Some(self.twi.scl))
        } else {
            (None, None)
        };
        self.ports[Port::C as usize].oc[TWI_SDA_BIT as usize] = sda;
        self.ports[Port::C as usize].oc[TWI_SCL_BIT as usize] = scl;
    }

    // --- Register access, routed from the core's data-space accessors ---

    /// Read a peripheral register, or `None` if `addr` is not a peripheral.
    pub fn read(&self, addr: u16) -> Option<u8> {
        let v = match addr {
            PINB => self.port(Port::B).pin_byte(),
            DDRB => self.port(Port::B).ddr,
            PORTB => self.port(Port::B).port,
            PINC => self.port(Port::C).pin_byte(),
            DDRC => self.port(Port::C).ddr,
            PORTC => self.port(Port::C).port,
            PIND => self.port(Port::D).pin_byte(),
            DDRD => self.port(Port::D).ddr,
            PORTD => self.port(Port::D).port,

            TCCR0A => self.timer0.tccr_a,
            TCCR0B => self.timer0.tccr_b,
            TCNT0 => self.timer0.tcnt,
            OCR0A => self.timer0.ocr_a,
            OCR0B => self.timer0.ocr_b,
            TIMSK0 => self.timer0.timsk,
            TIFR0 => self.timer0.tifr,

            TCCR1A => self.timer1.tccr_a,
            TCCR1B => self.timer1.tccr_b,
            TCCR1C => self.timer1.tccr_c,
            TCNT1L => self.timer1.tcnt as u8,
            TCNT1H => (self.timer1.tcnt >> 8) as u8,
            ICR1L => self.timer1.icr as u8,
            ICR1H => (self.timer1.icr >> 8) as u8,
            OCR1AL => self.timer1.ocr_a as u8,
            OCR1AH => (self.timer1.ocr_a >> 8) as u8,
            OCR1BL => self.timer1.ocr_b as u8,
            OCR1BH => (self.timer1.ocr_b >> 8) as u8,
            TIMSK1 => self.timer1.timsk,
            TIFR1 => self.timer1.tifr,

            ADCL => self.adc.data_low(),
            ADCH => self.adc.data_high(),
            ADCSRA => self.adc.adcsra,
            ADCSRB => self.adc.adcsrb,
            ADMUX => self.adc.admux,
            DIDR0 => 0,

            UCSR0A => self.usart.ucsr_a,
            UCSR0B => self.usart.ucsr_b,
            UCSR0C => self.usart.ucsr_c,
            UBRR0L => self.usart.ubrr as u8,
            UBRR0H => (self.usart.ubrr >> 8) as u8,
            UDR0 => self.usart.rx_data,

            SPCR => self.spi.spcr,
            SPSR => self.spi.spsr,
            SPDR => self.spi.data,

            TWBR => self.twi.twbr,
            TWSR => self.twi.twsr,
            TWAR => self.twi.twar,
            TWDR => self.twi.twdr,
            TWCR => self.twi.twcr,
            TWAMR => self.twi.twamr,

            _ => return None,
        };
        Some(v)
    }

    /// Write a peripheral register. Returns `true` if `addr` was a peripheral.
    pub fn write(&mut self, addr: u16, val: u8) -> bool {
        match addr {
            // GPIO. Writing a 1 to a PINx bit toggles the matching PORTx bit.
            PINB => self.port_mut(Port::B).port ^= val,
            DDRB => self.port_mut(Port::B).ddr = val,
            PORTB => self.port_mut(Port::B).port = val,
            PINC => self.port_mut(Port::C).port ^= val,
            DDRC => self.port_mut(Port::C).ddr = val,
            PORTC => self.port_mut(Port::C).port = val,
            PIND => self.port_mut(Port::D).port ^= val,
            DDRD => self.port_mut(Port::D).ddr = val,
            PORTD => self.port_mut(Port::D).port = val,

            TCCR0A => {
                self.timer0.tccr_a = val;
                self.timer0.refresh_outputs();
            }
            TCCR0B => {
                self.timer0.tccr_b = val;
                self.timer0.refresh_outputs();
            }
            TCNT0 => self.timer0.tcnt = val,
            OCR0A => self.timer0.ocr_a = val,
            OCR0B => self.timer0.ocr_b = val,
            TIMSK0 => self.timer0.timsk = val,
            // Flag registers are cleared by writing a 1 to the flag bit.
            TIFR0 => self.timer0.tifr &= !val,

            TCCR1A => {
                self.timer1.tccr_a = val;
                self.timer1.refresh_outputs();
            }
            TCCR1B => {
                self.timer1.tccr_b = val;
                self.timer1.refresh_outputs();
            }
            TCCR1C => self.timer1.tccr_c = val,
            // 16-bit writes go high-byte-first through the shared TEMP register.
            TCNT1H => self.timer1.temp = val,
            TCNT1L => self.timer1.tcnt = (self.timer1.temp as u16) << 8 | val as u16,
            ICR1H => self.timer1.temp = val,
            ICR1L => self.timer1.icr = (self.timer1.temp as u16) << 8 | val as u16,
            OCR1AH => self.timer1.temp = val,
            OCR1AL => self.timer1.ocr_a = (self.timer1.temp as u16) << 8 | val as u16,
            OCR1BH => self.timer1.temp = val,
            OCR1BL => self.timer1.ocr_b = (self.timer1.temp as u16) << 8 | val as u16,
            TIMSK1 => self.timer1.timsk = val,
            TIFR1 => self.timer1.tifr &= !val,

            ADCL | ADCH => {} // result registers are read-only
            ADCSRA => {
                // ADIF is cleared by writing a 1 to it; keep the result latched.
                let clear_if = val & ADIF;
                self.adc.adcsra = (val & !ADIF) | (self.adc.adcsra & ADIF & !clear_if);
                self.adc.maybe_start();
            }
            ADCSRB => self.adc.adcsrb = val,
            ADMUX => self.adc.admux = val,
            DIDR0 => {}

            UCSR0A => {
                // TXC0 is cleared by writing a 1; the other writable bits follow.
                let clear_txc = val & TXC0;
                self.usart.ucsr_a = (val & !TXC0) | (self.usart.ucsr_a & TXC0 & !clear_txc);
            }
            UCSR0B => self.usart.ucsr_b = val,
            UCSR0C => self.usart.ucsr_c = val,
            UBRR0H => self.usart.ubrr_temp = val,
            UBRR0L => self.usart.ubrr = (self.usart.ubrr_temp as u16) << 8 | val as u16,
            UDR0 => self.usart.write_udr(val),

            SPCR => self.spi.spcr = val,
            // SPIF/WCOL are read-only status; firmware does not set them here.
            SPSR => self.spi.spsr = (self.spi.spsr & !SPI2X) | (val & SPI2X),
            SPDR => self.spi.write_data(val),

            TWBR => self.twi.twbr = val,
            // Only the prescaler bits (TWPS1:0) of TWSR are writable.
            TWSR => self.twi.twsr = (self.twi.twsr & 0xF8) | (val & 0x03),
            TWAR => self.twi.twar = val,
            TWDR => self.twi.twdr = val,
            TWCR => self.twi.write_twcr(val),
            TWAMR => self.twi.twamr = val,

            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpio_direction_and_levels() {
        let mut p = Peripherals::new();
        // DDRB bit 5 output, PORTB bit 5 high.
        p.write(DDRB, 1 << 5);
        p.write(PORTB, 1 << 5);
        assert_eq!(p.read(PINB).unwrap() & (1 << 5), 1 << 5);
        assert!(p.pin(Port::B, 5));
        // An input pin reflects the externally driven level, not PORT.
        p.set_pin_input(Port::C, 0, true);
        assert!(p.pin(Port::C, 0));
        assert_eq!(p.read(PINC).unwrap() & 1, 1);
    }

    #[test]
    fn writing_pinx_toggles_portx() {
        let mut p = Peripherals::new();
        p.write(DDRB, 1 << 5);
        assert!(!p.pin(Port::B, 5));
        p.write(PINB, 1 << 5); // toggle
        assert!(p.pin(Port::B, 5));
        p.write(PINB, 1 << 5); // toggle back
        assert!(!p.pin(Port::B, 5));
    }

    #[test]
    fn timer0_overflow_flag_after_256_ticks() {
        let mut p = Peripherals::new();
        p.write(TCCR0B, 0x01); // Normal mode, prescaler /1.
        for _ in 0..255 {
            p.tick(1);
        }
        assert_eq!(p.read(TIFR0).unwrap() & TOV, 0, "no overflow before wrap");
        p.tick(1); // 256th tick wraps 0xFF → 0x00
        assert_eq!(p.read(TIFR0).unwrap() & TOV, TOV, "overflow at 256");
    }

    #[test]
    fn timer0_counts_at_prescaler() {
        let mut p = Peripherals::new();
        p.write(TCCR0B, 0x03); // prescaler /64
        p.tick(64 * 10);
        assert_eq!(p.read(TCNT0).unwrap(), 10, "10 ticks after 640 cycles");
    }

    #[test]
    fn timer0_fast_pwm_duty_on_oc0a() {
        let mut p = Peripherals::new();
        // Fast PWM (WGM=3), non-inverting on OC0A (COM0A1=1), prescaler /1.
        p.write(DDRD, 1 << 6); // OC0A = PD6 output
        p.write(OCR0A, 64); // 25% duty (64/256)
        p.write(TCCR0A, (2 << 6) | 0x03); // COM0A=2, WGM01:00=11
        p.write(TCCR0B, 0x01); // WGM02=0, CS=/1
        let mut high = 0u32;
        let n = 256 * 4; // four full PWM periods
        for _ in 0..n {
            p.tick(1);
            if p.pin(Port::D, 6) {
                high += 1;
            }
        }
        let duty = high as f64 / n as f64;
        assert!((duty - 0.25).abs() < 0.02, "measured duty {duty}");
    }

    #[test]
    fn timer1_overflow_and_prescaler() {
        let mut p = Peripherals::new();
        p.write(TCCR1B, 0x01); // Normal, /1
        p.tick(1000);
        assert_eq!(p.read(TCNT1L).unwrap(), (1000u16 & 0xFF) as u8);
        assert_eq!(p.read(TCNT1H).unwrap(), (1000u16 >> 8) as u8);
    }

    #[test]
    fn adc_converts_input_to_10bit_code() {
        let mut p = Peripherals::new();
        p.set_adc_vref(5.0);
        p.set_adc_input(3, 2.5); // half-scale on channel 3
        p.write(ADMUX, 3);
        p.write(ADCSRA, ADEN | ADSC | 0x07); // enable, start, /128
        for _ in 0..(13 * 128 + 8) {
            p.tick(1);
        }
        let code = p.read(ADCL).unwrap() as u16 | (p.read(ADCH).unwrap() as u16) << 8;
        // 2.5 V / 5 V * 1024 = 512.
        assert_eq!(code, 512, "half reference → mid code");
        assert_eq!(p.read(ADCSRA).unwrap() & ADSC, 0, "ADSC cleared");
        assert_eq!(p.read(ADCSRA).unwrap() & ADIF, ADIF, "ADIF set");
    }

    #[test]
    fn pin_drive_reports_input_output_and_pwm() {
        let mut p = Peripherals::new();
        // An un-configured pin is an input: high-impedance.
        assert_eq!(p.pin_drive(Port::B, 0), PinDrive::HighZ);

        // DDRB5 output, PORTB5 high → driving high.
        p.write(DDRB, 1 << 5);
        p.write(PORTB, 1 << 5);
        assert_eq!(p.pin_drive(Port::B, 5), PinDrive::Digital(true));
        p.write(PORTB, 0);
        assert_eq!(p.pin_drive(Port::B, 5), PinDrive::Digital(false));

        // Fast-PWM on OC0A (PD6) at 25 % duty (OCR0A = 64, TOP = 255).
        p.write(DDRD, 1 << 6);
        p.write(OCR0A, 64);
        p.write(TCCR0A, (2 << 6) | 0x03); // COM0A = 2 (non-inverting), WGM = 3
        p.write(TCCR0B, 0x01); // CS = /1
        p.tick(1); // propagate the OC override onto the pin
        match p.pin_drive(Port::D, 6) {
            PinDrive::Pwm { duty, .. } => assert!((duty - 0.25).abs() < 1e-9, "duty {duty}"),
            other => panic!("expected PWM drive, got {other:?}"),
        }
    }

    #[test]
    fn pwm_override_is_invisible_on_pinx_until_ddr_selects_output() {
        // Regression: a timer OC/PWM waveform must only reach the pin when its
        // DDR bit selects output. With OC0A (PD6) left as an input, PINx reads
        // the externally driven level, not the internal PWM waveform — and
        // pin_drive agrees the pin is high-impedance.
        let mut p = Peripherals::new();
        // Configure Fast-PWM on OC0A (PD6), non-inverting, 100% drive region
        // (OCR0A high) so the override level would be a definite 1 if it leaked.
        p.write(OCR0A, 200);
        p.write(TCCR0A, (2 << 6) | 0x03); // COM0A = 2 (non-inverting), WGM = 3
        p.write(TCCR0B, 0x01); // CS = /1
        p.tick(1); // compute + apply the OC override onto PD6's oc slot

        // DDRD6 is still clear → PD6 is an input. Drive it low externally.
        p.set_pin_input(Port::D, 6, false);
        assert!(
            !p.pin(Port::D, 6),
            "PWM override must not drive an input pin's PINx read"
        );
        assert_eq!(
            p.read(PIND).unwrap() & (1 << 6),
            0,
            "PINx bit reflects the (low) input, not the PWM waveform"
        );
        assert_eq!(
            p.pin_drive(Port::D, 6),
            PinDrive::HighZ,
            "an input pin drives nothing regardless of the timer override"
        );

        // Now select output: the PWM override becomes visible on the pin.
        p.write(DDRD, 1 << 6);
        p.tick(1);
        match p.pin_drive(Port::D, 6) {
            PinDrive::Pwm { .. } => {}
            other => panic!("expected PWM drive once DDR selects output, got {other:?}"),
        }
    }

    #[test]
    fn next_event_in_picks_the_nearest_timer_compare() {
        let mut p = Peripherals::new();
        // Nothing running yet.
        assert_eq!(p.next_event_in(), None);

        // Timer0 Normal mode, /1, compare A at 100: next event is the compare
        // match (100 counts) ahead of the 256-count overflow.
        p.write(OCR0A, 100);
        p.write(TCCR0B, 0x01);
        assert_eq!(p.next_event_in(), Some(100));
    }

    #[test]
    fn sample_inputs_and_publish_drives_round_trip() {
        let mut p = Peripherals::new();
        let mut io = PinIo::new(PIN_COUNT);

        // Drive an output pin and publish it.
        p.write(DDRB, 1 << 5);
        p.write(PORTB, 1 << 5);
        p.publish_drives(&mut io);
        assert_eq!(io.drive(pin_id(Port::B, 5)), PinDrive::Digital(true));
        assert_eq!(io.drive(pin_id(Port::D, 0)), PinDrive::HighZ);

        // Feed an above-threshold voltage into an input pin and sample it.
        io.set_input(pin_id(Port::C, 0), 3.3);
        p.sample_inputs(&io);
        assert!(p.pin(Port::C, 0), "3.3 V reads as logic high");
        io.set_input(pin_id(Port::C, 0), 1.0);
        p.sample_inputs(&io);
        assert!(!p.pin(Port::C, 0), "1.0 V reads as logic low");
    }

    #[test]
    fn configurable_thresholds_with_hysteresis_sample_via_pin_io() {
        use crate::boundary::InputThresholds;
        let mut p = Peripherals::new();
        p.set_input_thresholds(InputThresholds::with_hysteresis(2.0, 3.0));
        let mut io = PinIo::new(PIN_COUNT);
        let c0 = pin_id(Port::C, 0);

        // Start low, raise into the band: must hold low until it clears V_IH.
        io.set_input(c0, 2.5);
        p.sample_inputs(&io);
        assert!(!p.pin(Port::C, 0), "in-band from low holds low");
        io.set_input(c0, 3.0);
        p.sample_inputs(&io);
        assert!(p.pin(Port::C, 0), "clears V_IH → high");
        // Fall back into the band: holds high until it drops under V_IL.
        io.set_input(c0, 2.5);
        p.sample_inputs(&io);
        assert!(p.pin(Port::C, 0), "in-band from high holds high");
        io.set_input(c0, 1.9);
        p.sample_inputs(&io);
        assert!(!p.pin(Port::C, 0), "drops under V_IL → low");
    }

    #[test]
    fn pin_thevenin_sags_under_load() {
        use crate::boundary::OutputDrive;
        let mut p = Peripherals::new();
        p.set_output_drive(OutputDrive::push_pull(5.0, 100.0));
        // Drive PB5 high.
        p.write(DDRB, 1 << 5);
        p.write(PORTB, 1 << 5);
        let th = p.pin_thevenin(Port::B, 5).expect("driven pin has a source");
        assert_eq!(th.v_oc, 5.0);
        assert_eq!(th.r, 100.0);
        // Into a 100 Ω load the delivered voltage sags to 2.5 V.
        assert!((th.delivered_into(100.0) - 2.5).abs() < 1e-9);
        // An input pin presents no source.
        assert_eq!(p.pin_thevenin(Port::C, 0), None);
    }

    #[test]
    fn usart_tx_captures_bytes() {
        let mut p = Peripherals::new();
        p.write(UCSR0B, TXEN0);
        p.write(UBRR0H, 0);
        p.write(UBRR0L, 103); // 9600 baud @ 16 MHz
        let frame = p.usart.frame_cycles();
        assert_eq!(frame, 10 * 16 * 104, "8N1 frame = 10 bits");
        let msg = b"Hi!";
        for &b in msg {
            // Wait for the holding buffer to be free (UDRE0 set).
            while p.read(UCSR0A).unwrap() & UDRE0 == 0 {
                p.tick(1);
            }
            p.write(UDR0, b);
            // Advance one full frame so the byte shifts out.
            for _ in 0..frame {
                p.tick(1);
            }
        }
        // Flush any byte still in flight.
        for _ in 0..frame {
            p.tick(1);
        }
        assert_eq!(p.usart_tx(), msg);
    }

    #[test]
    fn spi_master_shifts_a_byte_out_msb_first() {
        let mut p = Peripherals::new();
        // SCK (PB5) and MOSI (PB3) as outputs, SPI enabled as master (mode 0).
        p.write(DDRB, (1 << SPI_SCK_BIT) | (1 << SPI_MOSI_BIT));
        p.write(SPCR, SPE | MSTR);
        p.write(SPDR, 0xA5);
        assert!(p.spi_busy(), "writing SPDR starts a transfer");
        // fosc/4 ⇒ 2-cycle half-period ⇒ 16 edges ⇒ 32 cycles.
        for _ in 0..40 {
            p.tick(1);
        }
        assert!(!p.spi_busy(), "transfer completes");
        assert_eq!(p.spi_tx(), &[0xA5], "the byte written to SPDR shifted out");
        assert_eq!(p.read(SPSR).unwrap() & SPIF, SPIF, "SPIF set on completion");

        // The MOSI level captured on each sampling edge reconstructs the byte,
        // MSB first, and mode-0 sampling edges are the SCK-high (leading) edges.
        let samples: Vec<_> = p.spi_edges().iter().filter(|e| e.sample).collect();
        assert_eq!(samples.len(), 8, "one sampling edge per bit");
        let mut byte = 0u8;
        for e in &samples {
            assert!(e.sck, "mode-0 sampling edge is SCK high");
            byte = (byte << 1) | e.mosi as u8;
        }
        assert_eq!(byte, 0xA5, "captured MOSI bit sequence equals the byte");
    }

    #[test]
    fn spi_master_latches_miso_into_the_data_register() {
        let mut p = Peripherals::new();
        p.write(DDRB, (1 << SPI_SCK_BIT) | (1 << SPI_MOSI_BIT));
        p.write(SPCR, SPE | MSTR);
        p.set_spi_miso(true); // MISO held high ⇒ every sampled bit is 1
        p.write(SPDR, 0x00);
        for _ in 0..40 {
            p.tick(1);
        }
        assert_eq!(
            p.spi_data(),
            0xFF,
            "eight high MISO samples assemble to 0xFF"
        );
    }

    /// Run the TWI far enough to finish any pending operation (the longest is a
    /// 9-clock byte at the reset bit rate, 144 cycles).
    fn finish_twi(p: &mut Peripherals) {
        for _ in 0..200 {
            p.tick(1);
        }
    }

    /// A START is SDA falling (1→0) across two consecutive SCL-high samples.
    fn trace_has_start(lines: &[TwiLine]) -> bool {
        lines
            .windows(2)
            .any(|w| w[0].scl && w[1].scl && w[0].sda && !w[1].sda)
    }

    /// A STOP is SDA rising (0→1) across two consecutive SCL-high samples.
    fn trace_has_stop(lines: &[TwiLine]) -> bool {
        lines
            .windows(2)
            .any(|w| w[0].scl && w[1].scl && !w[0].sda && w[1].sda)
    }

    #[test]
    fn twi_master_write_produces_start_addr_data_stop() {
        let mut p = Peripherals::new();

        // START.
        p.write(TWCR, TWINT | TWSTA | TWEN);
        finish_twi(&mut p);
        assert_eq!(p.read(TWSR).unwrap() & 0xF8, TW_START, "START status");

        // SLA+W: slave 0x28, write bit clear ⇒ 0x50.
        p.write(TWDR, 0x50);
        p.write(TWCR, TWINT | TWEN);
        finish_twi(&mut p);
        assert_eq!(p.read(TWSR).unwrap() & 0xF8, TW_MT_SLA_ACK, "SLA+W ACK");

        // One data byte.
        p.write(TWDR, 0x5A);
        p.write(TWCR, TWINT | TWEN);
        finish_twi(&mut p);
        assert_eq!(p.read(TWSR).unwrap() & 0xF8, TW_MT_DATA_ACK, "data ACK");

        // STOP.
        p.write(TWCR, TWINT | TWSTO | TWEN);
        finish_twi(&mut p);

        assert_eq!(
            p.twi_events(),
            &[
                TwiEvent::Start,
                TwiEvent::Byte {
                    value: 0x50,
                    ack: true
                },
                TwiEvent::Byte {
                    value: 0x5A,
                    ack: true
                },
                TwiEvent::Stop,
            ],
            "decoded START / SLA+W / data / STOP sequence"
        );
        assert!(
            trace_has_start(p.twi_lines()),
            "SDA/SCL trace shows a START"
        );
        assert!(trace_has_stop(p.twi_lines()), "SDA/SCL trace shows a STOP");
    }

    #[test]
    fn twi_master_reports_nack_from_the_slave() {
        let mut p = Peripherals::new();
        p.set_twi_slave_ack(false); // model a slave that does not acknowledge
        p.write(TWCR, TWINT | TWSTA | TWEN);
        finish_twi(&mut p);
        p.write(TWDR, 0x50);
        p.write(TWCR, TWINT | TWEN);
        finish_twi(&mut p);
        assert_eq!(
            p.read(TWSR).unwrap() & 0xF8,
            TW_MT_SLA_NACK,
            "no ACK ⇒ SLA+W NACK status"
        );
    }
}
