//! Logic analyzer — multi-channel digital timing view + protocol decode
//! (`CP-UX-M2.1`).
//!
//! Where the [`Oscilloscope`](crate::oscilloscope) renders *analog* braille
//! traces, the logic analyzer renders *digital* channels: a stack of 0/1/X
//! timing waveforms, with decoded protocol frames annotated beneath the line
//! that carries them. Same contract as the other instrument views — it depends
//! only on `ohmforge-core`, is deterministic, and renders to a `String`, so the
//! timing diagram and every decoder are golden-file / unit testable with no
//! live terminal.
//!
//! ## Channels
//!
//! A [`DigitalChannel`] is a *uniformly sampled* logic line: a start time
//! `t0`, a sample period `dt`, and a run of [`Level`]s (`Low`/`High`/`X`). A
//! real [`Signal`](ohmforge_core::signal::Signal) is thresholded into one with
//! [`DigitalChannel::from_signal`]. Uniform sampling is what a real logic
//! analyzer captures (a sample clock), and it is what makes the decoders below
//! simple and exact: a bit time is just a sample count.
//!
//! ## Decoders
//!
//! Three transport decoders turn channels into frames:
//! - [`decode_uart`] — one async line + a baud: start/data/stop → bytes.
//! - [`decode_spi`] — clock + MOSI (+ optional CS): bytes clocked on the edge
//!   selected by `CPOL`/`CPHA`.
//! - [`decode_i2c`] — SCL + SDA: start/stop framing, 7-bit address + R/W, ACK,
//!   and data bytes.
//!
//! Each decoder is written so a malformed or wrong-parameter capture degrades
//! gracefully — it returns the frames it could recover (flagging framing
//! errors) and never panics.

use crate::fmt::{fit, fmt_eng};

/// A single digital sample level.
///
/// A local three-valued level (not `ohmforge_digital::Logic`) keeps the instrument
/// crate dependency-light: a captured line is either driven `0`/`1` or, where
/// the capture is contended/undefined, unknown (`X`). High-impedance has no
/// meaning for an already-captured sample, so there is no `Z`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Logic low (`0`).
    Low,
    /// Logic high (`1`).
    High,
    /// Unknown / undefined (`X`).
    X,
}

impl Level {
    /// Map a level to its bit value, treating unknown as `0`. Decoders use this
    /// to assemble bytes; they separately record framing/ack validity.
    #[inline]
    fn bit(self) -> u8 {
        matches!(self, Level::High) as u8
    }

    /// The glyph used in the timing view: a top bar for high, a bottom bar for
    /// low, a mid shade for unknown.
    #[inline]
    fn glyph(self) -> char {
        match self {
            Level::High => '\u{2594}', // ▔ upper one-eighth block
            Level::Low => '\u{2581}',  // ▁ lower one-eighth block
            Level::X => '\u{2592}',    // ▒ medium shade
        }
    }
}

/// A uniformly sampled digital line: `samples[i]` is the level at `t0 + i·dt`.
#[derive(Debug, Clone, PartialEq)]
pub struct DigitalChannel {
    /// Human label, e.g. `"TX"` or `"SCL"`.
    pub name: String,
    /// Time of sample 0, in seconds.
    pub t0: f64,
    /// Sample period, in seconds.
    pub dt: f64,
    /// The captured levels, one per sample.
    pub samples: Vec<Level>,
}

impl DigitalChannel {
    /// A channel from an explicit run of levels.
    pub fn new(name: impl Into<String>, t0: f64, dt: f64, samples: Vec<Level>) -> Self {
        Self {
            name: name.into(),
            t0,
            dt,
            samples,
        }
    }

    /// A channel from a bit sequence: `0` → [`Level::Low`], non-zero → `High`.
    pub fn from_bits(name: impl Into<String>, t0: f64, dt: f64, bits: &[u8]) -> Self {
        let samples = bits
            .iter()
            .map(|&b| if b != 0 { Level::High } else { Level::Low })
            .collect();
        Self::new(name, t0, dt, samples)
    }

    /// Threshold a real [`Signal`](ohmforge_core::signal::Signal) into a digital
    /// channel: samples `≥ threshold` become `High`, below become `Low`, and a
    /// non-finite sample becomes `X`. The sample period is taken from the first
    /// two `x` points (the signal is assumed uniformly sampled, as a captured
    /// logic line is); a complex or empty signal yields an empty channel.
    pub fn from_signal(sig: &ohmforge_core::signal::Signal, threshold: f64) -> Self {
        let y = sig.real().unwrap_or(&[]);
        let t0 = sig.x.first().copied().unwrap_or(0.0);
        let dt = match sig.x.as_slice() {
            [a, b, ..] => b - a,
            _ => 1.0,
        };
        let samples = y
            .iter()
            .map(|&v| {
                if !v.is_finite() {
                    Level::X
                } else if v >= threshold {
                    Level::High
                } else {
                    Level::Low
                }
            })
            .collect();
        Self::new(sig.name.clone(), t0, dt, samples)
    }

    /// Number of samples.
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// Whether the channel carries no samples.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// The time of the last sample (or `t0` when empty).
    fn t_end(&self) -> f64 {
        self.t0 + self.dt * self.samples.len().saturating_sub(1) as f64
    }

    /// The level at absolute time `t`, clamped to the captured range (the
    /// nearest sample at or before `t`). Empty channels read `X`.
    pub fn level_at(&self, t: f64) -> Level {
        if self.samples.is_empty() {
            return Level::X;
        }
        if self.dt <= 0.0 {
            return self.samples[0];
        }
        let i = ((t - self.t0) / self.dt).floor();
        let idx = i.clamp(0.0, (self.samples.len() - 1) as f64) as usize;
        self.samples[idx]
    }

    /// The level of the sample nearest `index`, clamped to range.
    fn at(&self, index: f64) -> Level {
        if self.samples.is_empty() {
            return Level::X;
        }
        let idx = index.round().clamp(0.0, (self.samples.len() - 1) as f64) as usize;
        self.samples[idx]
    }
}

// ── UART ────────────────────────────────────────────────────────────────────

/// Parity scheme for a UART line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parity {
    /// No parity bit.
    None,
    /// Even parity (the parity bit makes the ones-count even).
    Even,
    /// Odd parity (the parity bit makes the ones-count odd).
    Odd,
}

/// UART framing parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UartConfig {
    /// Symbol rate, in baud (bits per second).
    pub baud: f64,
    /// Data bits per frame (commonly 8).
    pub data_bits: u8,
    /// Stop bits per frame (1 or 2).
    pub stop_bits: u8,
    /// Parity scheme.
    pub parity: Parity,
    /// Least-significant bit first (true for standard UART).
    pub lsb_first: bool,
    /// Idle/mark level is high (true for standard UART; start bit is a space).
    pub idle_high: bool,
}

impl UartConfig {
    /// The ubiquitous 8-N-1, LSB-first, idle-high line at `baud`.
    pub fn n81(baud: f64) -> Self {
        Self {
            baud,
            data_bits: 8,
            stop_bits: 1,
            parity: Parity::None,
            lsb_first: true,
            idle_high: true,
        }
    }
}

/// One decoded UART frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UartFrame {
    /// The assembled data byte (LSB/MSB order per the config).
    pub byte: u8,
    /// Start time of the start bit, in seconds.
    pub t_start: f64,
    /// The stop bit(s) were at the idle level (a well-framed byte).
    pub framing_ok: bool,
    /// The parity bit matched (always true when parity is [`Parity::None`]).
    pub parity_ok: bool,
}

/// Decode a UART line into its byte frames.
///
/// Scans for each start edge (idle → start level), samples the mid-point of the
/// start, data, parity and stop bits at the configured baud, and assembles a
/// byte. A capture shorter than one bit, or a `dt`/baud giving under one sample
/// per bit, decodes to no frames rather than panicking — which is also how a
/// badly wrong baud degrades.
pub fn decode_uart(ch: &DigitalChannel, cfg: &UartConfig) -> Vec<UartFrame> {
    let mut frames = Vec::new();
    if ch.samples.is_empty() || cfg.baud <= 0.0 || ch.dt <= 0.0 || cfg.data_bits == 0 {
        return frames;
    }
    let spb = (1.0 / cfg.baud) / ch.dt; // samples per bit
    if spb < 1.0 {
        return frames;
    }
    let (idle, start) = if cfg.idle_high {
        (Level::High, Level::Low)
    } else {
        (Level::Low, Level::High)
    };
    let data_bits = cfg.data_bits as usize;
    let parity_bits = usize::from(cfg.parity != Parity::None);

    let n = ch.samples.len();
    let mut i = 0usize;
    while i + 1 < n {
        // Hunt for the falling edge into the start bit (idle → start).
        if !(ch.samples[i] == idle && ch.samples[i + 1] == start) {
            i += 1;
            continue;
        }
        let edge = (i + 1) as f64; // first sample of the start bit
                                   // Centre of bit `k` (0 = start bit): edge + (k + 0.5)·spb.
        let centre = |k: usize| edge + (k as f64 + 0.5) * spb;

        // Confirm the start bit really is the start level at its centre.
        let framing_start = ch.at(centre(0)) == start;

        // Assemble the data byte.
        let mut byte = 0u8;
        let mut ones = 0u32;
        for d in 0..data_bits {
            let bit = ch.at(centre(1 + d)).bit();
            ones += u32::from(bit);
            if cfg.lsb_first {
                byte |= bit << d;
            } else {
                byte |= bit << (data_bits - 1 - d);
            }
        }

        // Parity bit (if any).
        let parity_ok = match cfg.parity {
            Parity::None => true,
            Parity::Even | Parity::Odd => {
                let p = ch.at(centre(1 + data_bits)).bit();
                let want_odd = cfg.parity == Parity::Odd;
                let total_ones = ones + u32::from(p);
                (total_ones % 2 == 1) == want_odd
            }
        };

        // Stop bit(s) must sit at the idle level.
        let mut framing_stop = true;
        for s in 0..cfg.stop_bits.max(1) as usize {
            if ch.at(centre(1 + data_bits + parity_bits + s)) != idle {
                framing_stop = false;
            }
        }

        frames.push(UartFrame {
            byte,
            t_start: ch.t0 + edge * ch.dt,
            framing_ok: framing_start && framing_stop,
            parity_ok,
        });

        // Resume scanning inside this frame's stop (idle) region, so the next
        // idle→start transition found is the true next frame — never a High→Low
        // edge in the middle of this frame's data.
        let resume = edge + (1 + data_bits + parity_bits) as f64 * spb;
        i = (resume.floor() as usize).max(i + 1);
    }
    frames
}

// ── SPI ───────────────────────────────────────────────────────────────────

/// SPI clock/phase and bit-order parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpiConfig {
    /// Clock polarity: idle-high clock when true.
    pub cpol: bool,
    /// Clock phase: sample on the trailing edge when true, leading when false.
    pub cpha: bool,
    /// Most-significant bit first (true for the common case).
    pub msb_first: bool,
    /// Bits per word (commonly 8).
    pub bits: u8,
    /// Chip-select is active-low (true for the common case). Only consulted
    /// when a CS channel is supplied.
    pub cs_active_low: bool,
}

impl SpiConfig {
    /// SPI **mode 0** (CPOL=0, CPHA=0), MSB-first, 8-bit, active-low CS — the
    /// overwhelmingly common default.
    pub fn mode0() -> Self {
        Self {
            cpol: false,
            cpha: false,
            msb_first: true,
            bits: 8,
            cs_active_low: true,
        }
    }

    /// Build a config for SPI `mode` (0..=3), MSB-first, 8-bit, active-low CS.
    pub fn mode(mode: u8) -> Self {
        Self {
            cpol: mode & 0b10 != 0,
            cpha: mode & 0b01 != 0,
            msb_first: true,
            bits: 8,
            cs_active_low: true,
        }
    }
}

/// Decode SPI MOSI bytes clocked against `clk`.
///
/// The sampling edge is rising when `CPOL == CPHA` and falling otherwise (the
/// four SPI modes). On each sampling edge — gated by `cs` being asserted, when
/// a CS channel is given — the MOSI level is shifted into the current word;
/// every `bits` edges completes a byte. The two channels are read on `clk`'s
/// own timeline, so a slightly different `dt` on MOSI still decodes.
pub fn decode_spi(
    clk: &DigitalChannel,
    mosi: &DigitalChannel,
    cs: Option<&DigitalChannel>,
    cfg: &SpiConfig,
) -> Vec<u8> {
    let mut out = Vec::new();
    if clk.samples.len() < 2 || cfg.bits == 0 {
        return out;
    }
    let sample_on_rising = cfg.cpol == cfg.cpha;
    let bits = cfg.bits as usize;
    let mut acc = 0u32;
    let mut count = 0usize;

    for i in 1..clk.samples.len() {
        let (prev, cur) = (clk.samples[i - 1], clk.samples[i]);
        let rising = prev == Level::Low && cur == Level::High;
        let falling = prev == Level::High && cur == Level::Low;
        let is_sample_edge = if sample_on_rising { rising } else { falling };
        if !is_sample_edge {
            continue;
        }
        let t = clk.t0 + i as f64 * clk.dt;
        if let Some(cs) = cs {
            let asserted = (cs.level_at(t) == Level::Low) == cfg.cs_active_low;
            if !asserted {
                acc = 0;
                count = 0;
                continue;
            }
        }
        let bit = u32::from(mosi.level_at(t).bit());
        if cfg.msb_first {
            acc = (acc << 1) | bit;
        } else {
            acc |= bit << count;
        }
        count += 1;
        if count == bits {
            out.push(acc as u8);
            acc = 0;
            count = 0;
        }
    }
    out
}

// ── I²C ─────────────────────────────────────────────────────────────────────

/// One event on an I²C bus, in capture order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum I2cEvent {
    /// A START (or repeated START) condition: SDA falls while SCL is high.
    Start,
    /// The address byte following a START: 7-bit `address`, `read` flag (the
    /// R/W bit), and whether the target `ack`ed.
    Address {
        /// The 7-bit target address.
        address: u8,
        /// The R/W bit: true = read, false = write.
        read: bool,
        /// The addressed device pulled SDA low for the ACK bit.
        ack: bool,
    },
    /// A data byte and its ACK bit.
    Data {
        /// The byte value.
        value: u8,
        /// The receiver pulled SDA low for the ACK bit.
        ack: bool,
    },
    /// A STOP condition: SDA rises while SCL is high.
    Stop,
}

/// Decode an I²C bus (SCL + SDA) into its event stream.
///
/// START/STOP are detected as SDA transitions while SCL is high; data bits are
/// sampled on each SCL rising edge. The first byte after a START is the address
/// (7 bits + R/W), the ninth clock is its ACK; subsequent bytes are data, each
/// with its ACK. The two lines are read on a shared timeline (same `t0`/`dt`),
/// as a two-probe capture provides.
pub fn decode_i2c(scl: &DigitalChannel, sda: &DigitalChannel) -> Vec<I2cEvent> {
    let mut events = Vec::new();
    let n = scl.samples.len().min(sda.samples.len());
    if n < 2 {
        return events;
    }
    let t_at = |i: usize| scl.t0 + i as f64 * scl.dt;

    let mut collecting = false;
    let mut first_byte = false; // next completed byte is the address
    let mut acc = 0u8;
    let mut count = 0u8; // bits clocked into the current byte-with-ack (0..=8)

    for i in 1..n {
        let scl_prev = scl.level_at(t_at(i - 1));
        let scl_cur = scl.level_at(t_at(i));
        let sda_prev = sda.level_at(t_at(i - 1));
        let sda_cur = sda.level_at(t_at(i));

        // START / STOP: SDA moves while SCL is held high.
        if scl_prev == Level::High && scl_cur == Level::High {
            if sda_prev == Level::High && sda_cur == Level::Low {
                events.push(I2cEvent::Start);
                collecting = true;
                first_byte = true;
                acc = 0;
                count = 0;
                continue;
            }
            if sda_prev == Level::Low && sda_cur == Level::High {
                events.push(I2cEvent::Stop);
                collecting = false;
                acc = 0;
                count = 0;
                continue;
            }
        }

        // Data: sample SDA on the SCL rising edge.
        if collecting && scl_prev == Level::Low && scl_cur == Level::High {
            if count < 8 {
                acc = (acc << 1) | sda_cur.bit();
                count += 1;
            } else {
                // The ninth clock is the ACK bit (SDA low = acknowledged).
                let ack = sda_cur == Level::Low;
                if first_byte {
                    events.push(I2cEvent::Address {
                        address: acc >> 1,
                        read: acc & 1 == 1,
                        ack,
                    });
                    first_byte = false;
                } else {
                    events.push(I2cEvent::Data { value: acc, ack });
                }
                acc = 0;
                count = 0;
            }
        }
    }
    events
}

// ── timing view ───────────────────────────────────────────────────────────

/// A decoded value annotated beneath a channel at a point in time.
#[derive(Debug, Clone)]
struct Annotation {
    channel: usize,
    t: f64,
    label: String,
}

/// A multi-channel digital timing view with decoded frames annotated beneath
/// the channels that carry them.
///
/// Build it, add [`DigitalChannel`]s, optionally [`annotate`](LogicAnalyzer::annotate)
/// decoded frames, and call [`render`](LogicAnalyzer::render) for a framed,
/// multi-line string. Every rendered line is at most `width` character cells.
pub struct LogicAnalyzer {
    title: String,
    channels: Vec<DigitalChannel>,
    annotations: Vec<Annotation>,
    width: usize,
}

impl LogicAnalyzer {
    /// A new, empty analyzer 72 cells wide.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            channels: Vec::new(),
            annotations: Vec::new(),
            width: 72,
        }
    }

    /// Add a channel (drawn top-to-bottom in insertion order).
    pub fn channel(mut self, ch: DigitalChannel) -> Self {
        self.channels.push(ch);
        self
    }

    /// Set the frame width, in character cells.
    pub fn size(mut self, width: usize) -> Self {
        self.width = width.max(16);
        self
    }

    /// Annotate a decoded value under `channel` at absolute time `t`.
    pub fn annotate(mut self, channel: usize, t: f64, label: impl Into<String>) -> Self {
        self.annotations.push(Annotation {
            channel,
            t,
            label: label.into(),
        });
        self
    }

    /// Number of channels.
    pub fn len(&self) -> usize {
        self.channels.len()
    }

    /// Whether the analyzer has no channels.
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    /// The displayed time window `[t_start, t_end]` over all channels.
    fn window(&self) -> (f64, f64) {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for ch in &self.channels {
            if ch.is_empty() {
                continue;
            }
            lo = lo.min(ch.t0);
            hi = hi.max(ch.t_end());
        }
        if !lo.is_finite() || !hi.is_finite() || (hi - lo).abs() < f64::EPSILON {
            (0.0, 1.0)
        } else {
            (lo, hi)
        }
    }

    /// Render the timing diagram to a framed, multi-line string.
    pub fn render(&self) -> String {
        let width = self.width.max(16);
        let plot_w = width.saturating_sub(2).max(1);
        let (t_start, t_end) = self.window();
        let span = t_end - t_start;

        // Map an absolute time to its plot column (0..plot_w-1).
        let col_of = |t: f64| -> usize {
            if span <= 0.0 {
                return 0;
            }
            let frac = ((t - t_start) / span).clamp(0.0, 1.0);
            (frac * (plot_w - 1) as f64).round() as usize
        };

        let mut out = String::new();
        out.push_str("  ");
        out.push_str(&fit(&self.title, width.saturating_sub(2)));
        out.push('\n');

        for (ci, ch) in self.channels.iter().enumerate() {
            // Channel label row.
            out.push_str(&fit(&format!("  {}", ch.name), width));
            out.push('\n');

            // Waveform row: one glyph per column, a vertical bar at a definite
            // level change.
            let mut row = String::with_capacity(plot_w);
            let mut prev: Option<Level> = None;
            for px in 0..plot_w {
                let t = if plot_w > 1 {
                    t_start + span * px as f64 / (plot_w - 1) as f64
                } else {
                    t_start
                };
                let lv = ch.level_at(t);
                let edge = matches!((prev, lv),
                    (Some(p), c) if p != c && p != Level::X && c != Level::X);
                row.push(if edge { '\u{2502}' } else { lv.glyph() });
                prev = Some(lv);
            }
            out.push_str("  ");
            out.push_str(&row);
            out.push('\n');

            // Annotation row for this channel, if any decoded frames land here.
            let mut anns: Vec<&Annotation> = self
                .annotations
                .iter()
                .filter(|a| a.channel == ci)
                .collect();
            anns.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
            if !anns.is_empty() {
                let mut cells = vec![' '; plot_w];
                let mut next_free = 0usize;
                for a in anns {
                    let start = col_of(a.t).max(next_free);
                    if start >= plot_w {
                        break;
                    }
                    for (k, c) in a.label.chars().enumerate() {
                        if start + k >= plot_w {
                            break;
                        }
                        cells[start + k] = c;
                    }
                    // Leave a space before the next label.
                    next_free = start + a.label.chars().count() + 1;
                }
                let line: String = cells.into_iter().collect();
                out.push_str("  ");
                out.push_str(line.trim_end());
                out.push('\n');
            }
        }

        // Shared time axis: a rule, then start/end labels.
        out.push_str("  ");
        out.push_str(&"\u{2500}".repeat(plot_w));
        out.push('\n');
        let xlo = fmt_eng(0.0, "s");
        let xhi = fmt_eng(span, "s");
        let pad = plot_w.saturating_sub(xlo.len() + xhi.len()).max(1);
        out.push_str(&fit(&format!("  {xlo}{}{xhi}", " ".repeat(pad)), width));
        out.push('\n');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthesize a UART line from `bytes` at `cfg` with `spb` samples per bit,
    /// plus idle lead-in/out so the first start edge is unambiguous.
    fn synth_uart(bytes: &[u8], cfg: &UartConfig, spb: usize) -> DigitalChannel {
        let dt = (1.0 / cfg.baud) / spb as f64;
        let (idle, start) = if cfg.idle_high {
            (Level::High, Level::Low)
        } else {
            (Level::Low, Level::High)
        };
        let mut samples = vec![idle; spb * 2]; // idle lead-in
        for &b in bytes {
            samples.extend(std::iter::repeat(start).take(spb)); // start bit
            let mut ones = 0u32;
            for d in 0..cfg.data_bits as usize {
                let bit = (b >> d) & 1; // LSB first
                ones += u32::from(bit);
                let lv = if bit != 0 { Level::High } else { Level::Low };
                samples.extend(std::iter::repeat(lv).take(spb));
            }
            if cfg.parity != Parity::None {
                let want_odd = cfg.parity == Parity::Odd;
                // Choose the parity bit so the total ones-count has the right parity.
                let p = if (ones % 2 == 1) == want_odd { 0 } else { 1 };
                let lv = if p != 0 { Level::High } else { Level::Low };
                samples.extend(std::iter::repeat(lv).take(spb));
            }
            for _ in 0..cfg.stop_bits.max(1) {
                samples.extend(std::iter::repeat(idle).take(spb)); // stop bit(s)
            }
        }
        samples.extend(std::iter::repeat(idle).take(spb * 2)); // idle tail
        DigitalChannel::new("TX", 0.0, dt, samples)
    }

    #[test]
    fn uart_decodes_a_known_capture() {
        let cfg = UartConfig::n81(115_200.0);
        let msg = b"Hi!";
        let line = synth_uart(msg, &cfg, 16);
        let frames = decode_uart(&line, &cfg);
        let bytes: Vec<u8> = frames.iter().map(|f| f.byte).collect();
        assert_eq!(bytes, msg, "decoded bytes match the capture");
        assert!(
            frames.iter().all(|f| f.framing_ok && f.parity_ok),
            "every frame is cleanly framed: {frames:?}"
        );
    }

    #[test]
    fn uart_decodes_odd_parity_and_two_stop_bits() {
        let cfg = UartConfig {
            baud: 9600.0,
            data_bits: 8,
            stop_bits: 2,
            parity: Parity::Odd,
            lsb_first: true,
            idle_high: true,
        };
        let msg = b"\x00\xFF\xA5";
        let line = synth_uart(msg, &cfg, 20);
        let frames = decode_uart(&line, &cfg);
        assert_eq!(frames.iter().map(|f| f.byte).collect::<Vec<_>>(), msg);
        assert!(frames.iter().all(|f| f.framing_ok && f.parity_ok));
    }

    #[test]
    fn uart_wrong_baud_fails_gracefully() {
        let cfg = UartConfig::n81(115_200.0);
        let msg = b"Hello, world";
        let line = synth_uart(msg, &cfg, 16);
        // Decode the same capture at a badly wrong baud: no panic, and the bytes
        // do not come back intact.
        let wrong = UartConfig::n81(115_200.0 * 2.7);
        let frames = decode_uart(&line, &wrong);
        let bytes: Vec<u8> = frames.iter().map(|f| f.byte).collect();
        assert_ne!(bytes, msg, "a wrong baud must not reproduce the message");

        // A sub-sample baud degrades to nothing rather than dividing by zero.
        let absurd = UartConfig::n81(1.0 / line.dt * 2.0);
        assert!(decode_uart(&line, &absurd).is_empty());
    }

    /// Synthesize SPI mode-0 clk+mosi: for each bit, hold MOSI then pulse the
    /// clock low→high (rising edge sampled) → low. `half` samples per phase.
    fn synth_spi_mode0(bytes: &[u8], half: usize) -> (DigitalChannel, DigitalChannel) {
        let mut clk = vec![Level::Low; half]; // idle low lead-in
        let mut mosi = vec![Level::Low; half];
        for &b in bytes {
            for k in 0..8 {
                let bit = (b >> (7 - k)) & 1; // MSB first
                let lv = if bit != 0 { Level::High } else { Level::Low };
                // phase A: clk low, mosi set
                clk.extend(std::iter::repeat(Level::Low).take(half));
                mosi.extend(std::iter::repeat(lv).take(half));
                // phase B: clk high (rising edge sampled), mosi held
                clk.extend(std::iter::repeat(Level::High).take(half));
                mosi.extend(std::iter::repeat(lv).take(half));
            }
        }
        clk.push(Level::Low);
        mosi.push(Level::Low);
        (
            DigitalChannel::new("SCLK", 0.0, 1e-6, clk),
            DigitalChannel::new("MOSI", 0.0, 1e-6, mosi),
        )
    }

    #[test]
    fn spi_mode0_decodes_known_bytes() {
        let want = [0xA5u8, 0x3C, 0xFF, 0x00];
        let (clk, mosi) = synth_spi_mode0(&want, 8);
        let got = decode_spi(&clk, &mosi, None, &SpiConfig::mode0());
        assert_eq!(got, want, "mode-0 MOSI decodes to the clocked bytes");
    }

    /// Build an I²C capture: START, address+R/W, then data bytes (each ACKed),
    /// STOP. `q` samples per phase.
    fn synth_i2c(
        address: u8,
        read: bool,
        data: &[u8],
        q: usize,
    ) -> (DigitalChannel, DigitalChannel) {
        let mut scl = Vec::new();
        let mut sda = Vec::new();
        let push = |s: &mut Vec<Level>, lv: Level, n: usize| {
            s.extend(std::iter::repeat(lv).take(n));
        };
        // Idle bus: both high.
        push(&mut scl, Level::High, q);
        push(&mut sda, Level::High, q);
        // START: SDA high→low while SCL high.
        push(&mut scl, Level::High, q);
        push(&mut sda, Level::Low, q);
        // Drop SCL to begin clocking.
        push(&mut scl, Level::Low, q);
        push(&mut sda, Level::Low, q);

        // Clock out one byte (MSB first) + an ACK bit.
        let clock_byte = |scl: &mut Vec<Level>, sda: &mut Vec<Level>, byte: u8, ack: bool| {
            for k in 0..8 {
                let bit = (byte >> (7 - k)) & 1;
                let lv = if bit != 0 { Level::High } else { Level::Low };
                // SDA set while SCL low, then SCL pulses high and back low.
                push(scl, Level::Low, q);
                push(sda, lv, q);
                push(scl, Level::High, q);
                push(sda, lv, q);
                push(scl, Level::Low, q);
                push(sda, lv, q);
            }
            // ACK bit.
            let alv = if ack { Level::Low } else { Level::High };
            push(scl, Level::Low, q);
            push(sda, alv, q);
            push(scl, Level::High, q);
            push(sda, alv, q);
            push(scl, Level::Low, q);
            push(sda, alv, q);
        };

        let addr_byte = (address << 1) | read as u8;
        clock_byte(&mut scl, &mut sda, addr_byte, true);
        for &d in data {
            clock_byte(&mut scl, &mut sda, d, true);
        }

        // STOP: SDA low→high while SCL high.
        push(&mut scl, Level::Low, q);
        push(&mut sda, Level::Low, q);
        push(&mut scl, Level::High, q);
        push(&mut sda, Level::Low, q);
        push(&mut scl, Level::High, q);
        push(&mut sda, Level::High, q);

        (
            DigitalChannel::new("SCL", 0.0, 1e-6, scl),
            DigitalChannel::new("SDA", 0.0, 1e-6, sda),
        )
    }

    #[test]
    fn i2c_decodes_start_address_data_stop() {
        let (scl, sda) = synth_i2c(0x48, false, &[0xDE, 0xAD], 4);
        let events = decode_i2c(&scl, &sda);
        assert_eq!(events.first(), Some(&I2cEvent::Start), "opens with START");
        assert_eq!(events.last(), Some(&I2cEvent::Stop), "closes with STOP");
        assert_eq!(
            events[1],
            I2cEvent::Address {
                address: 0x48,
                read: false,
                ack: true
            },
            "address + R/W + ACK: {events:?}"
        );
        let data: Vec<u8> = events
            .iter()
            .filter_map(|e| match e {
                I2cEvent::Data { value, .. } => Some(*value),
                _ => None,
            })
            .collect();
        assert_eq!(data, vec![0xDE, 0xAD], "payload bytes: {events:?}");
    }

    #[test]
    fn timing_view_renders_channels_annotations_and_fits_width() {
        let cfg = UartConfig::n81(115_200.0);
        let msg = b"Go";
        let line = synth_uart(msg, &cfg, 16);
        let frames = decode_uart(&line, &cfg);

        let width = 60;
        let mut la = LogicAnalyzer::new("LA: uart TX").size(width).channel(line);
        for f in &frames {
            la = la.annotate(0, f.t_start, format!("0x{:02X}", f.byte));
        }
        let out = la.render();

        assert!(out.contains("LA: uart TX"), "title:\n{out}");
        assert!(out.contains("TX"), "channel label:\n{out}");
        // Decoded byte values annotated beneath the line.
        assert!(out.contains("0x47"), "'G' annotated:\n{out}"); // 'G' = 0x47
        assert!(out.contains("0x6F"), "'o' annotated:\n{out}"); // 'o' = 0x6F
                                                                // A digital waveform glyph was actually drawn.
        assert!(
            out.contains('\u{2594}') || out.contains('\u{2581}'),
            "a high/low bar drew:\n{out}"
        );
        for l in out.lines() {
            assert!(
                l.chars().count() <= width,
                "line exceeds width {width}: {l:?}"
            );
        }
    }

    #[test]
    fn empty_analyzer_and_channel_do_not_panic() {
        let out = LogicAnalyzer::new("empty").size(24).render();
        assert!(out.contains("empty"));

        let ch = DigitalChannel::new("flat", 0.0, 1e-6, Vec::new());
        assert_eq!(ch.level_at(0.0), Level::X);
        assert!(decode_uart(&ch, &UartConfig::n81(9600.0)).is_empty());
    }

    #[test]
    fn from_signal_thresholds_levels() {
        let sig = ohmforge_core::signal::Signal::time(
            "d0",
            ohmforge_core::units::Unit::Volt,
            vec![0.0, 1e-6, 2e-6, 3e-6],
            vec![0.0, 3.3, 3.3, 0.0],
        );
        let ch = DigitalChannel::from_signal(&sig, 1.65);
        assert_eq!(
            ch.samples,
            vec![Level::Low, Level::High, Level::High, Level::Low]
        );
        assert!((ch.dt - 1e-6).abs() < 1e-18);
    }
}
