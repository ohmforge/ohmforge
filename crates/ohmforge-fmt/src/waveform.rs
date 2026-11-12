//! Waveform export (`CP-AF-M2.3`) — turn [`Signal`]s into the two files the
//! wider ecosystem reads: **CSV** (a spreadsheet / plotting staple) and **VCD**
//! (Value Change Dump, what GTKWave and friends open).
//!
//! A sibling of [`crate::netlist`]/[`crate::project`]/[`crate::spice`]: same
//! house rules — a friendly error type with `Display` + [`std::error::Error`],
//! hand-rolled with zero dependencies beyond `ohmforge-core`, and deterministic output
//! (numbers print via `f64`'s default `Display`, the shortest string that parses
//! back to the identical bits).
//!
//! ## CSV ([`to_csv`])
//!
//! One header row then one row per sample: the independent axis first
//! (`time_s` for a time-domain signal, `freq_hz` for a frequency-domain one),
//! then each signal's column(s). A real signal is one column named after it; a
//! **complex** signal (AC) becomes two columns — `<name>_mag` and
//! `<name>_phase_deg` — so magnitude/phase survive a round trip to a plotter.
//! All signals must share one x-axis (same length and sample points), exactly as
//! a single analysis run produces.
//!
//! ## VCD ([`to_vcd`])
//!
//! A well-formed Value Change Dump: a `$timescale`/`$scope`/`$var` header, then
//! time-ordered value-change records. The timescale is chosen from the sample
//! spacing so every sample lands on a distinct, monotonically increasing integer
//! tick. **Analog** signals are emitted as `real` VCD vars (`r<value> <id>`);
//! a signal whose every sample is exactly `0` or `1` is treated as **digital**
//! and emitted as a one-bit scalar (`0<id>`/`1<id>`). VCD is inherently a
//! time dump, so a complex (AC) signal is rejected — use [`to_csv`] for those.

use std::error::Error;
use std::fmt::{self, Write as _};

use ohmforge_core::signal::{Domain, Signal, SignalData};

/// A waveform-export failure. Unlike the parsers' line-tagged errors, an export
/// error is about the *shape* of the signals handed in, so it names the offending
/// signal instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaveformError {
    /// No signals were given — there is nothing to export.
    NoSignals,
    /// A signal's `y` sample count does not match its `x` axis length.
    LengthMismatch {
        /// The signal whose axes disagree.
        signal: String,
        /// The number of `x` sample points.
        x: usize,
        /// The number of `y` samples.
        y: usize,
    },
    /// A signal does not share the common x-axis (different length or sample
    /// points) that CSV/VCD columns are written against.
    AxisMismatch {
        /// The signal that disagrees with the first signal's axis.
        signal: String,
    },
    /// VCD time must not run backwards; this signal's x-axis does at `at`.
    NonMonotonic {
        /// The signal whose x-axis decreases.
        signal: String,
        /// The 0-based sample index where `x[at] < x[at-1]`.
        at: usize,
    },
    /// A complex (AC / frequency-domain) signal was handed to the VCD writer,
    /// which only models a real time dump. Export it as CSV instead.
    ComplexInVcd {
        /// The complex signal that cannot be dumped.
        signal: String,
    },
}

impl fmt::Display for WaveformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WaveformError::NoSignals => write!(f, "no signals to export"),
            WaveformError::LengthMismatch { signal, x, y } => write!(
                f,
                "signal {signal:?} has {x} x-samples but {y} y-samples"
            ),
            WaveformError::AxisMismatch { signal } => write!(
                f,
                "signal {signal:?} does not share the common x-axis (all signals must be sampled alike)"
            ),
            WaveformError::NonMonotonic { signal, at } => write!(
                f,
                "signal {signal:?} has a non-monotonic time axis at sample {at} (VCD time cannot run backwards)"
            ),
            WaveformError::ComplexInVcd { signal } => write!(
                f,
                "signal {signal:?} is complex (AC) — VCD is a real time dump; export it as CSV instead"
            ),
        }
    }
}

impl Error for WaveformError {}

/// The CSV header name for an independent axis.
fn axis_name(domain: Domain) -> &'static str {
    match domain {
        Domain::Time => "time_s",
        Domain::Freq => "freq_hz",
    }
}

/// Validate that every signal shares the first signal's x-axis (length and
/// sample points) and that each signal's y-length matches its x-length. Returns
/// the shared axis on success.
fn common_axis(signals: &[Signal]) -> Result<&[f64], WaveformError> {
    let first = signals.first().ok_or(WaveformError::NoSignals)?;
    for s in signals {
        let ylen = match &s.y {
            SignalData::Real(v) => v.len(),
            SignalData::Complex(v) => v.len(),
        };
        if ylen != s.x.len() {
            return Err(WaveformError::LengthMismatch {
                signal: s.name.clone(),
                x: s.x.len(),
                y: ylen,
            });
        }
        if s.x.len() != first.x.len() || s.x != first.x {
            return Err(WaveformError::AxisMismatch {
                signal: s.name.clone(),
            });
        }
    }
    Ok(&first.x)
}

/// Export `signals` to CSV text: a header row (the axis column then one or two
/// columns per signal), then one row per sample. See the module docs for the
/// column layout and the complex → magnitude/phase split.
///
/// All signals must be sampled over the same x-axis — the normal case for the
/// signals of a single analysis run. Numbers print with `f64`'s default
/// `Display` so the text is the shortest round-trippable form and byte-stable.
pub fn to_csv(signals: &[Signal]) -> Result<String, WaveformError> {
    let axis = common_axis(signals)?;
    let domain = signals[0].domain;

    let mut out = String::new();
    // Header: the axis, then each signal's column name(s).
    out.push_str(&csv_field(axis_name(domain)));
    for s in signals {
        match &s.y {
            SignalData::Real(_) => {
                out.push(',');
                out.push_str(&csv_field(&s.name));
            }
            SignalData::Complex(_) => {
                out.push(',');
                out.push_str(&csv_field(&format!("{}_mag", s.name)));
                out.push(',');
                out.push_str(&csv_field(&format!("{}_phase_deg", s.name)));
            }
        }
    }
    out.push('\n');

    // One row per sample.
    for (i, &x) in axis.iter().enumerate() {
        let _ = write!(out, "{x}");
        for s in signals {
            match &s.y {
                SignalData::Real(v) => {
                    let _ = write!(out, ",{}", v[i]);
                }
                SignalData::Complex(v) => {
                    let z = v[i];
                    let _ = write!(out, ",{},{}", z.magnitude(), z.phase_deg());
                }
            }
        }
        out.push('\n');
    }

    Ok(out)
}

/// Quote a CSV field per RFC 4180 when it contains a comma, quote, or newline;
/// otherwise return it unchanged. Signal names are usually plain (`v(out)`), but
/// quoting keeps a stray comma from shifting every column.
fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for c in s.chars() {
            if c == '"' {
                out.push('"'); // RFC 4180 doubles an embedded quote
            }
            out.push(c);
        }
        out.push('"');
        out
    } else {
        s.to_string()
    }
}

/// How one VCD variable is dumped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VarKind {
    /// A `real` VCD var, emitted `r<value> <id>` — the analog default.
    Analog,
    /// A one-bit scalar, emitted `<0|1><id>` — a signal that is only ever 0 or 1.
    Digital,
}

/// Classify a real signal: digital iff *every* sample is exactly `0.0` or `1.0`
/// (a clean logic trace), else analog. The exact-0/1 test is deliberately strict
/// so a 0–5 V logic waveform stays analog unless it was normalized to bits.
fn classify(samples: &[f64]) -> VarKind {
    if samples.iter().all(|&v| v == 0.0 || v == 1.0) {
        VarKind::Digital
    } else {
        VarKind::Analog
    }
}

/// A VCD variable: its dump kind, its identifier code, and the real samples
/// backing it (VCD only dumps real time-series).
struct VcdVar<'a> {
    kind: VarKind,
    id: String,
    name: &'a str,
    samples: &'a [f64],
}

/// The VCD identifier code for the var at 0-based `index`. VCD identifiers are
/// one or more characters from the printable-ASCII range `!`(0x21)..=`~`(0x7e);
/// this is a little-endian base-94 encoding of `index`, so **any** number of
/// vars gets a distinct, printable id. The first 94 vars are single characters
/// (`!`, `"`, `#`, …), so small dumps are unchanged; var 94 is the first
/// two-character id. (A plain `b'!'..` counter would emit non-printable bytes
/// past 94 vars and overflow its `u8` past 223 — neither is a valid VCD.)
fn vcd_id(index: usize) -> String {
    const FIRST: u8 = b'!'; // 0x21
    const BASE: usize = (b'~' - b'!' + 1) as usize; // 94 printable codes
    let mut n = index;
    let mut id = String::new();
    loop {
        id.push((FIRST + (n % BASE) as u8) as char);
        n /= BASE;
        if n == 0 {
            break;
        }
    }
    id
}

/// Export `signals` to a well-formed VCD. See the module docs; complex signals
/// are rejected ([`WaveformError::ComplexInVcd`]) because VCD is a real time
/// dump. Analog signals become `real` vars; all-0/1 signals become scalar bits.
///
/// The timescale is derived from the sample spacing so every sample maps to a
/// distinct, strictly increasing integer tick; only *changed* values are written
/// at each tick (plus every var's initial value at the first tick), which is a
/// standard, compact, GTKWave-friendly dump.
pub fn to_vcd(signals: &[Signal]) -> Result<String, WaveformError> {
    let axis = common_axis(signals)?;

    // Collect real-backed vars, rejecting complex (AC) signals. Each var gets a
    // distinct printable-ASCII identifier code via [`vcd_id`].
    let mut vars: Vec<VcdVar> = Vec::with_capacity(signals.len());
    for (index, s) in signals.iter().enumerate() {
        let samples = match &s.y {
            SignalData::Real(v) => v.as_slice(),
            SignalData::Complex(_) => {
                return Err(WaveformError::ComplexInVcd {
                    signal: s.name.clone(),
                })
            }
        };
        vars.push(VcdVar {
            kind: classify(samples),
            id: vcd_id(index),
            name: &s.name,
            samples,
        });
    }

    // VCD time cannot run backwards.
    for s in signals {
        for i in 1..s.x.len() {
            if s.x[i] < s.x[i - 1] {
                return Err(WaveformError::NonMonotonic {
                    signal: s.name.clone(),
                    at: i,
                });
            }
        }
    }

    let (unit_seconds, timescale) = choose_timescale(axis);
    let ticks = ticks_for(axis, unit_seconds);

    let mut out = String::new();
    // Deterministic header: no wall-clock date, so the output is byte-stable.
    out.push_str("$date ohmforge $end\n");
    out.push_str("$version ohmforge-fmt waveform export $end\n");
    let _ = writeln!(out, "$timescale {timescale} $end");
    out.push_str("$scope module ee $end\n");
    for v in &vars {
        let kind = match v.kind {
            VarKind::Analog => "real",
            VarKind::Digital => "wire",
        };
        let _ = writeln!(out, "$var {kind} 1 {} {} $end", v.id, vcd_ident(v.name));
    }
    out.push_str("$upscope $end\n");
    out.push_str("$enddefinitions $end\n");

    // Time-ordered value changes. Emit every var at the first tick, then only
    // the vars whose value changed at each later tick.
    for (i, &t) in ticks.iter().enumerate() {
        let mut changes = String::new();
        for v in &vars {
            let val = v.samples[i];
            let changed = i == 0 || v.samples[i - 1] != val;
            if changed {
                changes.push_str(&dump_value(v.kind, val, &v.id));
                changes.push('\n');
            }
        }
        if !changes.is_empty() {
            let _ = writeln!(out, "#{t}");
            out.push_str(&changes);
        }
    }

    Ok(out)
}

/// Format one value-change record for a var: `r<value> <id>` for analog, or the
/// bit glued to the id (`1<id>`/`0<id>`) for digital.
fn dump_value(kind: VarKind, value: f64, id: &str) -> String {
    match kind {
        VarKind::Analog => format!("r{value} {id}"),
        VarKind::Digital => format!("{}{id}", if value != 0.0 { '1' } else { '0' }),
    }
}

/// Sanitize a signal name into a VCD identifier reference: VCD delimits on
/// whitespace, so collapse any into `_`. Other characters (e.g. the parens in
/// `v(out)`) are left as-is — GTKWave accepts them.
fn vcd_ident(name: &str) -> String {
    name.split_whitespace().collect::<Vec<_>>().join("_")
}

/// Map each x value (seconds) to an integer VCD tick of size `unit_seconds`,
/// forcing the sequence strictly increasing so timestamps never collide or run
/// backwards after rounding.
fn ticks_for(x: &[f64], unit_seconds: f64) -> Vec<u64> {
    let mut ticks = Vec::with_capacity(x.len());
    let mut prev: Option<u64> = None;
    for &xi in x {
        let raw = (xi / unit_seconds).round();
        let mut t = if raw.is_finite() && raw >= 0.0 {
            raw as u64
        } else {
            0
        };
        if let Some(p) = prev {
            if t <= p {
                t = p + 1;
            }
        }
        ticks.push(t);
        prev = Some(t);
    }
    ticks
}

/// Choose a VCD timescale from the sample spacing: the tick size `10^k` seconds
/// where `10^k` is just at or below the smallest step between samples, so each
/// step is at least one tick. Returns the tick size and its `"<1|10|100>
/// <s|ms|us|ns|ps|fs>"` VCD string.
fn choose_timescale(x: &[f64]) -> (f64, String) {
    // The smallest positive gap between consecutive samples sets the resolution.
    let dt = x
        .windows(2)
        .map(|w| w[1] - w[0])
        .filter(|&d| d > 0.0)
        .fold(f64::INFINITY, f64::min);
    // No usable spacing (one sample, or all identical): default to 1 ns.
    let dt = if dt.is_finite() && dt > 0.0 { dt } else { 1e-9 };

    // k = floor(log10(dt)), clamped to the VCD-expressible range [fs .. s].
    let k = dt.log10().floor() as i32;
    let k = k.clamp(-15, 0);
    // The unit exponent is the largest multiple of 3 ≤ k (s=0, ms=-3, …, fs=-15),
    // and the multiplier 10^(k-u) ∈ {1,10,100} makes up the difference.
    let u = (k.div_euclid(3) * 3).clamp(-15, 0);
    let mult = 10_i32.pow((k - u) as u32);
    let unit = match u {
        0 => "s",
        -3 => "ms",
        -6 => "us",
        -9 => "ns",
        -12 => "ps",
        _ => "fs",
    };
    (10f64.powi(k), format!("{mult} {unit}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ohmforge_core::signal::Complex;
    use ohmforge_core::units::Unit;

    // ── CSV ─────────────────────────────────────────────────────────────────

    #[test]
    fn csv_real_signals_match_the_probe_data_exactly() {
        let vout = Signal::time(
            "v(out)",
            Unit::Volt,
            vec![0.0, 1e-3, 2e-3],
            vec![0.0, 0.5, 0.75],
        );
        let iout = Signal::time(
            "i(R1)",
            Unit::Ampere,
            vec![0.0, 1e-3, 2e-3],
            vec![0.0, 5e-4, 7.5e-4],
        );

        let csv = to_csv(&[vout, iout]).unwrap();
        let expected = "\
time_s,v(out),i(R1)
0,0,0
0.001,0.5,0.0005
0.002,0.75,0.00075
";
        assert_eq!(csv, expected);
    }

    #[test]
    fn csv_complex_signal_splits_into_magnitude_and_phase() {
        // A unit vector at +90°: magnitude 1, phase 90°.
        let s = Signal::freq(
            "v(out)",
            Unit::Volt,
            vec![1_000.0],
            vec![Complex::new(0.0, 1.0)],
        );
        let csv = to_csv(&[s]).unwrap();
        let mut lines = csv.lines();
        assert_eq!(lines.next().unwrap(), "freq_hz,v(out)_mag,v(out)_phase_deg");
        assert_eq!(lines.next().unwrap(), "1000,1,90");
        assert!(lines.next().is_none());
    }

    #[test]
    fn csv_quotes_a_name_with_a_comma() {
        let s = Signal::time("v(a,b)", Unit::Volt, vec![0.0], vec![1.0]);
        let csv = to_csv(&[s]).unwrap();
        assert!(csv.starts_with("time_s,\"v(a,b)\"\n"), "{csv}");
    }

    #[test]
    fn csv_rejects_empty_and_mismatched_inputs() {
        assert_eq!(to_csv(&[]).unwrap_err(), WaveformError::NoSignals);

        let a = Signal::time("a", Unit::Volt, vec![0.0, 1.0], vec![0.0, 1.0]);
        let b = Signal::time("b", Unit::Volt, vec![0.0, 2.0], vec![0.0, 1.0]);
        assert_eq!(
            to_csv(&[a, b]).unwrap_err(),
            WaveformError::AxisMismatch { signal: "b".into() }
        );
    }

    // ── VCD ─────────────────────────────────────────────────────────────────

    /// A structural check that the text is a well-formed VCD: it has the required
    /// header directives, every value-change id was declared by a `$var`, and the
    /// `#` timestamps are strictly increasing. Returns the timestamps seen.
    fn assert_well_formed_vcd(vcd: &str) -> Vec<u64> {
        assert!(vcd.contains("$timescale "), "has a timescale: {vcd}");
        assert!(vcd.contains("$enddefinitions $end"), "closes definitions");
        assert!(vcd.contains("$scope module"), "opens a scope");
        assert!(vcd.contains("$upscope $end"), "closes the scope");

        // Collect declared var ids (the token after `$var <kind> 1 `).
        let mut ids = std::collections::HashSet::new();
        for line in vcd.lines() {
            if let Some(rest) = line.strip_prefix("$var ") {
                let tok: Vec<&str> = rest.split_whitespace().collect();
                // kind width id name $end
                ids.insert(tok[2].to_string());
            }
        }
        assert!(!ids.is_empty(), "declares at least one var");

        let mut times = Vec::new();
        let mut in_body = false;
        for line in vcd.lines() {
            if line == "$enddefinitions $end" {
                in_body = true;
                continue;
            }
            if !in_body {
                continue;
            }
            if let Some(t) = line.strip_prefix('#') {
                times.push(t.parse::<u64>().expect("integer timestamp"));
            } else if let Some(rest) = line.strip_prefix('r') {
                // real change: `r<value> <id>`
                let id = rest.split_whitespace().nth(1).expect("real id");
                assert!(ids.contains(id), "value change for declared id {id}");
            } else if line == "0" || line == "1" {
                // not a value line
            } else if !line.is_empty() {
                // scalar change: `<bit><id>`
                let id = &line[1..];
                assert!(ids.contains(id), "scalar change for declared id {id}");
            }
        }
        // Timestamps strictly increase.
        for w in times.windows(2) {
            assert!(w[1] > w[0], "timestamps strictly increase: {times:?}");
        }
        times
    }

    #[test]
    fn vcd_is_well_formed_with_analog_and_digital_vars() {
        // Analog ramp and a digital clock-ish trace, both on a 1 ms grid.
        let x = vec![0.0, 1e-3, 2e-3, 3e-3];
        let analog = Signal::time("v(out)", Unit::Volt, x.clone(), vec![0.0, 1.5, 3.3, 3.3]);
        let digital = Signal::time("clk", Unit::Volt, x.clone(), vec![0.0, 1.0, 0.0, 1.0]);

        let vcd = to_vcd(&[analog, digital]).unwrap();

        // Both var kinds are declared.
        assert!(vcd.contains("$var real 1 ! v(out) $end"), "{vcd}");
        assert!(vcd.contains("$var wire 1 \" clk $end"), "{vcd}");

        let times = assert_well_formed_vcd(&vcd);
        assert_eq!(times.first(), Some(&0), "first sample at t=0");
        assert_eq!(times.len(), 4, "one timestamp per sample (all change)");

        // The analog var's real value changes are present and correct.
        assert!(vcd.contains("r0 !"), "initial analog value: {vcd}");
        assert!(vcd.contains("r1.5 !") && vcd.contains("r3.3 !"), "{vcd}");
        // The digital var toggles as a scalar bit on its id.
        assert!(vcd.contains("0\"") && vcd.contains("1\""), "{vcd}");

        // t=3ms repeats the analog 3.3 → it must NOT re-emit (change-only dump),
        // but the digital bit flips 0→1 so it does.
        let last = vcd.rsplit("#3").next().unwrap();
        assert!(
            !last.contains("r3.3 !"),
            "unchanged analog is not re-dumped: {vcd}"
        );
        assert!(last.contains("1\""), "changed digital bit is dumped: {vcd}");
    }

    #[test]
    fn vcd_timescale_tracks_sample_spacing() {
        // 1 µs spacing → a microsecond-or-finer timescale, ticks 0,1,2…
        let x = vec![0.0, 1e-6, 2e-6];
        let s = Signal::time("n", Unit::Volt, x, vec![0.0, 0.1, 0.2]);
        let vcd = to_vcd(&[s]).unwrap();
        assert!(vcd.contains("$timescale 1 us $end"), "{vcd}");
        let times = assert_well_formed_vcd(&vcd);
        assert_eq!(times, vec![0, 1, 2]);
    }

    #[test]
    fn vcd_rejects_a_complex_signal() {
        let s = Signal::freq(
            "v(out)",
            Unit::Volt,
            vec![1e3],
            vec![Complex::new(1.0, 0.0)],
        );
        assert_eq!(
            to_vcd(&[s]).unwrap_err(),
            WaveformError::ComplexInVcd {
                signal: "v(out)".into()
            }
        );
    }

    /// Regression: many signals must still produce a well-formed VCD. A plain
    /// `b'!'..` id counter emitted non-printable bytes past 94 vars and panicked
    /// (`u8` overflow) past 223; [`vcd_id`] encodes base-94 so every identifier
    /// stays printable ASCII and unique, for any count.
    #[test]
    fn vcd_identifiers_stay_printable_and_unique_for_many_signals() {
        let x = vec![0.0, 1e-3];
        let signals: Vec<Signal> = (0..300)
            .map(|i| Signal::time(format!("s{i}"), Unit::Volt, x.clone(), vec![0.0, 1.0]))
            .collect();

        // No panic, and a structurally well-formed dump.
        let vcd = to_vcd(&signals).expect("300 signals export without panicking");
        let _ = assert_well_formed_vcd(&vcd);

        // Every declared identifier is non-empty, printable ASCII (0x21..=0x7e),
        // and distinct across all 300 vars.
        let mut ids = std::collections::HashSet::new();
        let mut var_lines = 0;
        for line in vcd.lines() {
            if let Some(rest) = line.strip_prefix("$var ") {
                var_lines += 1;
                let id = rest.split_whitespace().nth(2).expect("$var has an id");
                assert!(!id.is_empty(), "identifier is non-empty");
                assert!(
                    id.chars().all(|c| ('!'..='~').contains(&c)),
                    "identifier {id:?} is printable ASCII"
                );
                assert!(ids.insert(id.to_string()), "identifier {id:?} is unique");
            }
        }
        assert_eq!(var_lines, 300, "every signal declared a var");
    }

    /// The base-94 identifier encoding: the first 94 vars are single printable
    /// characters (back-compatible with the old counter) and var 94 rolls over
    /// to the first two-character id — all distinct.
    #[test]
    fn vcd_id_is_backward_compatible_base94() {
        assert_eq!(vcd_id(0), "!");
        assert_eq!(vcd_id(1), "\"");
        assert_eq!(vcd_id(93), "~");
        // 94 distinct single chars, then two-char ids with no collision.
        let all: std::collections::HashSet<String> = (0..1000).map(vcd_id).collect();
        assert_eq!(all.len(), 1000, "every index maps to a distinct id");
        assert_eq!(
            vcd_id(94).chars().count(),
            2,
            "var 94 is the first 2-char id"
        );
    }

    #[test]
    fn vcd_rejects_a_backwards_time_axis() {
        let s = Signal::time("n", Unit::Volt, vec![0.0, 2e-3, 1e-3], vec![0.0, 1.0, 2.0]);
        assert_eq!(
            to_vcd(&[s]).unwrap_err(),
            WaveformError::NonMonotonic {
                signal: "n".into(),
                at: 2
            }
        );
    }
}
