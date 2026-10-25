//! Drop-in module models: small, pure behavioural models of the parts that
//! populate a board — an [`Led`], a [`SevenSeg`] display, a [`Servo`], and a
//! [`Ws2812`] addressable pixel.
//!
//! Each is deterministic and allocation-light so it runs the same on native and
//! `wasm32`, and carries no simulation state beyond its fixed configuration — the
//! caller feeds in pin levels / voltages / a bit stream and reads back the
//! modelled output (segments lit, current, shaft angle, colour). That keeps them
//! trivially unit-testable and free to compose into a co-simulation loop.

/// A 24-bit RGB colour (8 bits per channel).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    /// Red channel, `0..=255`.
    pub r: u8,
    /// Green channel, `0..=255`.
    pub g: u8,
    /// Blue channel, `0..=255`.
    pub b: u8,
}

impl Rgb {
    /// A colour from its three channels.
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// How a single LED (segment or discrete) is wired relative to its driver pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polarity {
    /// Lit when the driving pin is **high** (pin sources current / common
    /// cathode).
    ActiveHigh,
    /// Lit when the driving pin is **low** (pin sinks current / common anode).
    ActiveLow,
}

impl Polarity {
    /// Whether a pin level lights an element of this polarity. A pin level is a
    /// `bool`: the caller collapses a digital `High`/`Low` to `true`/`false`.
    #[inline]
    pub fn lit(self, pin_high: bool) -> bool {
        match self {
            Polarity::ActiveHigh => pin_high,
            Polarity::ActiveLow => !pin_high,
        }
    }
}

/// A single discrete LED in series with a current-limiting resistor.
///
/// The model is the usual first-order one: the LED conducts above its forward
/// voltage, and the series resistor sets the current `I = (Vpin − Vf) / R`
/// (clamped at zero below `Vf`). [`is_lit`](Self::is_lit) reports whether that
/// current clears a visibility threshold.
#[derive(Debug, Clone, Copy)]
pub struct Led {
    /// Forward voltage drop, in volts.
    pub forward_v: f64,
    /// Series resistance, in ohms.
    pub series_ohms: f64,
    /// Polarity relative to the driving pin.
    pub polarity: Polarity,
}

impl Default for Led {
    /// A red LED (≈1.8 V) with a 220 Ω series resistor, lit when the pin is high.
    fn default() -> Self {
        Self {
            forward_v: 1.8,
            series_ohms: 220.0,
            polarity: Polarity::ActiveHigh,
        }
    }
}

impl Led {
    /// The current (amps) through the LED for a pin voltage, relative to the
    /// pin's inactive rail. `pin_high` picks which rail the pin is at; `vcc` is
    /// the supply. Current only flows when the pin's level makes the LED
    /// conduct — otherwise zero.
    pub fn current(&self, pin_high: bool, vcc: f64) -> f64 {
        if !self.polarity.lit(pin_high) {
            return 0.0;
        }
        // Forward voltage across the LED+resistor branch is the full supply when
        // the pin drives the active rail.
        let drive = (vcc - self.forward_v) / self.series_ohms;
        drive.max(0.0)
    }

    /// Whether the LED is visibly lit from a logic pin level: it conducts and the
    /// resulting current clears a 1 mA visibility floor at the given supply.
    pub fn is_lit(&self, pin_high: bool, vcc: f64) -> bool {
        self.current(pin_high, vcc) >= 1e-3
    }
}

/// The seven segments of a digit, in the conventional `a..g` order.
///
/// ```text
///   aaa
///  f   b
///  f   b
///   ggg
///  e   c
///  e   c
///   ddd
/// ```
pub const SEG_A: usize = 0;
/// Segment index for `b` (top-right).
pub const SEG_B: usize = 1;
/// Segment index for `c` (bottom-right).
pub const SEG_C: usize = 2;
/// Segment index for `d` (bottom).
pub const SEG_D: usize = 3;
/// Segment index for `e` (bottom-left).
pub const SEG_E: usize = 4;
/// Segment index for `f` (top-left).
pub const SEG_F: usize = 5;
/// Segment index for `g` (middle).
pub const SEG_G: usize = 6;

/// A 7-segment digit, decoding which segments light from its seven driver pins.
///
/// The display's seven segment pins are provided as levels in `a..g` order; the
/// [`Polarity`] says whether a high or low pin lights a segment (common-cathode
/// vs common-anode). [`decode`](Self::decode) returns the lit mask; the free
/// function [`digit_segments`] gives the expected mask for a hex digit so a
/// driver can be checked against the glyph it should show.
#[derive(Debug, Clone, Copy)]
pub struct SevenSeg {
    /// Segment polarity relative to the segment pins.
    pub polarity: Polarity,
}

impl Default for SevenSeg {
    /// Common-cathode: a segment lights when its pin is driven high.
    fn default() -> Self {
        Self {
            polarity: Polarity::ActiveHigh,
        }
    }
}

impl SevenSeg {
    /// A common-cathode display (segment lit on a high pin).
    pub fn common_cathode() -> Self {
        Self {
            polarity: Polarity::ActiveHigh,
        }
    }

    /// A common-anode display (segment lit on a low pin).
    pub fn common_anode() -> Self {
        Self {
            polarity: Polarity::ActiveLow,
        }
    }

    /// Decode the seven segment pin levels (`a..g`, each `true` = high) into the
    /// lit mask (`a..g`, each `true` = lit), honouring the display polarity.
    pub fn decode(&self, pins: [bool; 7]) -> [bool; 7] {
        let mut lit = [false; 7];
        for (i, &p) in pins.iter().enumerate() {
            lit[i] = self.polarity.lit(p);
        }
        lit
    }

    /// Which hex digit (`0..=15`) the lit mask shows, if it matches a glyph.
    pub fn glyph(&self, pins: [bool; 7]) -> Option<u8> {
        let lit = self.decode(pins);
        (0u8..=15).find(|&d| digit_segments(d) == Some(lit))
    }
}

/// The lit-segment mask (`a..g`) for a hex digit `0..=15`, or `None` out of
/// range. Standard seven-segment font.
pub fn digit_segments(value: u8) -> Option<[bool; 7]> {
    // Bit order a..g (bit 0 = a … bit 6 = g).
    const FONT: [u8; 16] = [
        0b0111111, // 0: a b c d e f
        0b0000110, // 1: b c
        0b1011011, // 2: a b d e g
        0b1001111, // 3: a b c d g
        0b1100110, // 4: b c f g
        0b1101101, // 5: a c d f g
        0b1111101, // 6: a c d e f g
        0b0000111, // 7: a b c
        0b1111111, // 8: all
        0b1101111, // 9: a b c d f g
        0b1110111, // A: a b c e f g
        0b1111100, // b: c d e f g
        0b0111001, // C: a d e f
        0b1011110, // d: b c d e g
        0b1111001, // E: a d e f g
        0b1110001, // F: a e f g
    ];
    let bits = *FONT.get(value as usize)?;
    let mut mask = [false; 7];
    for (i, m) in mask.iter_mut().enumerate() {
        *m = (bits >> i) & 1 == 1;
    }
    Some(mask)
}

/// A hobby servo mapping a control pulse width to a shaft angle.
///
/// The standard interface is a ~50 Hz PWM whose high-time sets the position:
/// `min_us` → `0°`, `max_us` → the full `range_deg`, linear in between and
/// clamped at the ends.
#[derive(Debug, Clone, Copy)]
pub struct Servo {
    /// Pulse width (µs) commanding 0°.
    pub min_us: f64,
    /// Pulse width (µs) commanding the full range.
    pub max_us: f64,
    /// Mechanical travel, in degrees, between `min_us` and `max_us`.
    pub range_deg: f64,
}

impl Default for Servo {
    /// A standard servo: 1000 µs → 0°, 2000 µs → 180°.
    fn default() -> Self {
        Self {
            min_us: 1000.0,
            max_us: 2000.0,
            range_deg: 180.0,
        }
    }
}

impl Servo {
    /// The commanded shaft angle (degrees) for a control pulse width in µs,
    /// clamped to `0..=range_deg` outside the `[min_us, max_us]` window.
    pub fn angle(&self, pulse_us: f64) -> f64 {
        let span = self.max_us - self.min_us;
        if span <= 0.0 {
            return 0.0;
        }
        let frac = ((pulse_us - self.min_us) / span).clamp(0.0, 1.0);
        frac * self.range_deg
    }
}

/// A WS2812 ("NeoPixel") addressable RGB pixel: it latches the first 24 bits of
/// its serial data stream as one colour.
///
/// The wire format is 24 bits per pixel, **most-significant bit first**, in
/// **G, R, B** channel order (the WS2812's native ordering). [`decode`](Self::decode)
/// turns a 24-bit boolean stream into an [`Rgb`]; [`bit`](Self::bit) classifies a
/// single data bit from its high/low pulse durations for driving from raw timing.
#[derive(Debug, Clone, Copy, Default)]
pub struct Ws2812;

impl Ws2812 {
    /// Decode 24 data bits (MSB first, channel order G, R, B) into a colour.
    /// A stream that is not exactly 24 bits long is rejected.
    pub fn decode(&self, bits: &[bool]) -> Option<Rgb> {
        if bits.len() != 24 {
            return None;
        }
        let byte = |chunk: &[bool]| chunk.iter().fold(0u8, |acc, &b| (acc << 1) | b as u8);
        let g = byte(&bits[0..8]);
        let r = byte(&bits[8..16]);
        let b = byte(&bits[16..24]);
        Some(Rgb::new(r, g, b))
    }

    /// Classify one WS2812 data bit from its measured high/low pulse widths (ns).
    ///
    /// A bit is a fixed-period pulse whose duty distinguishes 0 from 1: a long
    /// high (longer than its low) is a `1`, a short high a `0`. Returns `None` if
    /// the pulse is degenerate (no measurable period).
    pub fn bit(&self, high_ns: f64, low_ns: f64) -> Option<bool> {
        if high_ns + low_ns <= 0.0 {
            return None;
        }
        Some(high_ns > low_ns)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        digit_segments, Led, Polarity, Rgb, Servo, SevenSeg, Ws2812, SEG_A, SEG_B, SEG_C, SEG_G,
    };

    #[test]
    fn seven_seg_shows_the_right_segments_for_a_digit() {
        let d = SevenSeg::common_cathode();
        // Drive the segment pins for a "7": a, b, c lit (high), rest low.
        let mut pins = [false; 7];
        pins[SEG_A] = true;
        pins[SEG_B] = true;
        pins[SEG_C] = true;
        assert_eq!(d.decode(pins), digit_segments(7).unwrap());
        assert_eq!(d.glyph(pins), Some(7), "decodes back to the digit 7");

        // A full "8" lights every segment.
        assert_eq!(d.decode([true; 7]), digit_segments(8).unwrap());
    }

    #[test]
    fn seven_seg_common_anode_inverts_the_pins() {
        let d = SevenSeg::common_anode();
        // For common-anode, a segment lights on a LOW pin. "1" = b, c lit.
        let mut pins = [true; 7]; // all pins high → all off
        pins[SEG_B] = false; // pull b low → lit
        pins[SEG_C] = false; // pull c low → lit
        assert_eq!(d.decode(pins), digit_segments(1).unwrap());
    }

    #[test]
    fn seven_seg_middle_segment_is_g() {
        // Sanity-check the segment ordering: "1" has no middle bar, "8" does.
        assert!(!digit_segments(1).unwrap()[SEG_G]);
        assert!(digit_segments(8).unwrap()[SEG_G]);
        assert!(digit_segments(0).unwrap()[SEG_A]);
    }

    #[test]
    fn led_lights_only_when_its_pin_drives_the_active_rail() {
        let led = Led::default(); // active-high, 1.8 V, 220 Ω
        assert!(led.is_lit(true, 5.0), "high pin at 5 V lights it");
        assert!(!led.is_lit(false, 5.0), "low pin: no current");
        // (5 - 1.8) / 220 ≈ 14.5 mA.
        let i = led.current(true, 5.0);
        assert!((i - 0.01454).abs() < 1e-4, "current ≈ 14.5 mA, got {i}");

        let anode = Led {
            polarity: Polarity::ActiveLow,
            ..Led::default()
        };
        assert!(
            anode.is_lit(false, 5.0),
            "active-low LED lights on a low pin"
        );
        assert!(!anode.is_lit(true, 5.0));
    }

    #[test]
    fn servo_maps_pulse_width_to_angle() {
        let s = Servo::default(); // 1000 µs → 0°, 2000 µs → 180°
        assert!((s.angle(1000.0) - 0.0).abs() < 1e-9);
        assert!((s.angle(1500.0) - 90.0).abs() < 1e-9, "midpoint → 90°");
        assert!((s.angle(2000.0) - 180.0).abs() < 1e-9);
        // Clamped outside the window.
        assert!((s.angle(500.0) - 0.0).abs() < 1e-9);
        assert!((s.angle(2500.0) - 180.0).abs() < 1e-9);
    }

    #[test]
    fn ws2812_decodes_a_bit_pattern_to_a_color() {
        let px = Ws2812;
        // 24 bits MSB-first in G,R,B order. Choose G=0x10, R=0x20, B=0x40.
        let byte_bits = |v: u8| (0..8).rev().map(move |i| (v >> i) & 1 == 1);
        let bits: Vec<bool> = byte_bits(0x10)
            .chain(byte_bits(0x20))
            .chain(byte_bits(0x40))
            .collect();
        assert_eq!(px.decode(&bits), Some(Rgb::new(0x20, 0x10, 0x40)));

        // A pure-green pixel: G all ones, R and B zero.
        let green: Vec<bool> = byte_bits(0xFF)
            .chain(byte_bits(0x00))
            .chain(byte_bits(0x00))
            .collect();
        assert_eq!(px.decode(&green), Some(Rgb::new(0, 255, 0)));

        // Wrong length is rejected.
        assert_eq!(px.decode(&[true; 23]), None);
    }

    #[test]
    fn ws2812_classifies_a_bit_from_pulse_timing() {
        let px = Ws2812;
        assert_eq!(px.bit(700.0, 600.0), Some(true), "long high → 1");
        assert_eq!(px.bit(350.0, 800.0), Some(false), "short high → 0");
        assert_eq!(px.bit(0.0, 0.0), None);
    }
}
