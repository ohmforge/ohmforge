//! Shared value formatting for the instrument panels.
//!
//! The oscilloscope grew its own private copies of these helpers first; the
//! multimeter, function generator and bench PSU share this module so their
//! front panels print values the same way — compact fixed-point for axis-style
//! labels and SI-prefixed engineering notation for readouts.

/// Compact number formatting: three decimals, trailing zeros trimmed, no `-0`.
pub(crate) fn fmt_num(v: f64) -> String {
    if v == 0.0 {
        return "0".to_string();
    }
    if !v.is_finite() {
        return if v > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    let mut s = format!("{v:.3}");
    if s.contains('.') {
        s.truncate(s.trim_end_matches('0').trim_end_matches('.').len());
    }
    s
}

/// Format a value in engineering notation with an SI prefix and unit, e.g.
/// `0.001 s` → `1ms`, `1000 Hz` → `1kHz`.
pub(crate) fn fmt_eng(v: f64, unit: &str) -> String {
    if v == 0.0 {
        return format!("0{unit}");
    }
    if !v.is_finite() {
        return format!("{}{unit}", if v > 0.0 { "inf" } else { "-inf" });
    }
    const PREFIXES: &[(f64, &str)] = &[
        (1e12, "T"),
        (1e9, "G"),
        (1e6, "M"),
        (1e3, "k"),
        (1.0, ""),
        (1e-3, "m"),
        (1e-6, "µ"),
        (1e-9, "n"),
        (1e-12, "p"),
    ];
    let a = v.abs();
    for &(factor, prefix) in PREFIXES {
        if a >= factor {
            return format!("{}{prefix}{unit}", fmt_num(v / factor));
        }
    }
    format!("{}p{unit}", fmt_num(v / 1e-12))
}

/// Clip or return a string at most `width` character cells wide (Unicode-aware).
pub(crate) fn fit(s: &str, width: usize) -> String {
    if s.chars().count() > width {
        s.chars().take(width).collect()
    } else {
        s.to_string()
    }
}

/// A single framed line: `prefix` left, `value` right-justified to `width` cells.
pub(crate) fn kv_line(prefix: &str, value: &str, width: usize) -> String {
    let used = prefix.chars().count() + value.chars().count();
    let pad = width.saturating_sub(used).max(1);
    fit(&format!("{prefix}{}{value}", " ".repeat(pad)), width)
}
