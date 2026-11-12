//! Engineering-notation values: `4k7` → 4700, `1u` → 1e-6, `2n2` → 2.2e-9.
//!
//! Tinkerers write component values the way datasheets do; the shell and file
//! formats accept that spelling everywhere. This module is the single parser.

use crate::units::Unit;

/// A magnitude paired with its physical [`Unit`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Value {
    pub magnitude: f64,
    pub unit: Unit,
}

impl Value {
    pub const fn new(magnitude: f64, unit: Unit) -> Self {
        Self { magnitude, unit }
    }

    /// Parse an engineering-notation string with an assumed unit.
    ///
    /// Accepts a trailing/embedded SI prefix, including the "value-in-place-of-
    /// decimal-point" form (`4k7`, `2n2`, `1R5`). `meg` is treated as `M`.
    ///
    /// ```
    /// # use ohmforge_core::{value::Value, units::Unit};
    /// assert_eq!(Value::parse("4k7", Unit::Ohm).unwrap().magnitude, 4700.0);
    /// assert_eq!(Value::parse("1u",  Unit::Farad).unwrap().magnitude, 1e-6);
    /// assert_eq!(Value::parse("10k", Unit::Ohm).unwrap().magnitude, 10_000.0);
    /// assert!((Value::parse("2n2", Unit::Farad).unwrap().magnitude - 2.2e-9).abs() < 1e-15);
    /// ```
    pub fn parse(s: &str, unit: Unit) -> Result<Value, ParseError> {
        let s = s.trim();
        if s.is_empty() {
            return Err(ParseError::Empty);
        }
        // Strip one trailing unit token if the user typed it ("10kΩ", "1kHz",
        // "3.3V", "20mA"). Only *recognized* units are stripped, longest first,
        // so we never eat into a prefix word like "meg".
        const UNIT_SUFFIXES: &[&str] = &["Hz", "Ohm", "ohm", "Ω", "V", "A", "F", "H", "s", "W"];
        let mut s = s;
        for suf in UNIT_SUFFIXES {
            if s.len() > suf.len() && s.ends_with(suf) {
                s = &s[..s.len() - suf.len()];
                break;
            }
        }

        // Longest-prefix-name first ("meg").
        if let Some(idx) = s.to_ascii_lowercase().find("meg") {
            return combine(&s[..idx], &s[idx + 3..], 1e6, unit);
        }
        // Find a single-character SI prefix anywhere in the token.
        for (i, c) in s.char_indices() {
            if let Some(mult) = prefix_multiplier(c) {
                return combine(&s[..i], &s[i + c.len_utf8()..], mult, unit);
            }
        }
        // No prefix: a plain number.
        let n: f64 = s.parse().map_err(|_| ParseError::NotANumber)?;
        Ok(Value::new(n, unit))
    }
}

/// Combine the integer part before a prefix with the fractional part after it,
/// so `4k7` == `4.7 * 1000`.
fn combine(head: &str, tail: &str, mult: f64, unit: Unit) -> Result<Value, ParseError> {
    // A lone prefix ("k", "µ") or prefix-plus-stripped-unit ("kΩ") is not a
    // value — reject it rather than silently yielding 0 (which would read as a
    // short circuit).
    if head.is_empty() && tail.is_empty() {
        return Err(ParseError::NotANumber);
    }
    let head_n: f64 = if head.is_empty() {
        0.0
    } else {
        head.parse().map_err(|_| ParseError::NotANumber)?
    };
    let frac: f64 = if tail.is_empty() {
        0.0
    } else {
        // "7" after "4k" means 0.7 (one digit → tenths, etc.).
        let t: f64 = tail.parse().map_err(|_| ParseError::NotANumber)?;
        t / 10f64.powi(tail.len() as i32)
    };
    // The fractional digits after the prefix carry the head's sign, so
    // "-4k7" == -(4.7)·k, not -(4)·k + 0.7·k.
    let magnitude = if head.starts_with('-') {
        head_n - frac
    } else {
        head_n + frac
    };
    Ok(Value::new(magnitude * mult, unit))
}

fn prefix_multiplier(c: char) -> Option<f64> {
    Some(match c {
        'p' => 1e-12,
        'n' => 1e-9,
        'u' | 'µ' => 1e-6,
        'm' => 1e-3,
        'R' | 'r' => 1.0, // "1R5" resistor idiom → 1.5
        'k' | 'K' => 1e3,
        'M' => 1e6, // note: "m" (lowercase) is milli; "M"/"meg" is mega
        'G' => 1e9,
        'T' => 1e12,
        _ => return None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    Empty,
    NotANumber,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() <= b.abs() * 1e-9 + 1e-15, "{a} != {b}");
    }

    #[test]
    fn engineering_notation() {
        approx(Value::parse("4k7", Unit::Ohm).unwrap().magnitude, 4700.0);
        approx(Value::parse("2n2", Unit::Farad).unwrap().magnitude, 2.2e-9);
        approx(Value::parse("1R5", Unit::Ohm).unwrap().magnitude, 1.5);
        approx(Value::parse("1meg", Unit::Ohm).unwrap().magnitude, 1e6);
        approx(Value::parse("100n", Unit::Farad).unwrap().magnitude, 1e-7);
        approx(
            Value::parse("10kHz", Unit::Hertz).unwrap().magnitude,
            10_000.0,
        );
        approx(Value::parse("3.3V", Unit::Volt).unwrap().magnitude, 3.3);
        approx(Value::parse("20mA", Unit::Ampere).unwrap().magnitude, 0.02);
        // Negative values keep the head's sign across the fractional digits.
        approx(Value::parse("-4k7", Unit::Volt).unwrap().magnitude, -4700.0);
        approx(
            Value::parse("-2n2", Unit::Farad).unwrap().magnitude,
            -2.2e-9,
        );
        // A lone prefix or empty input is an error, not a silent 0.
        assert!(Value::parse("", Unit::Ohm).is_err());
        assert!(Value::parse("k", Unit::Ohm).is_err());
        assert!(Value::parse("-", Unit::Ohm).is_err());
    }
}
