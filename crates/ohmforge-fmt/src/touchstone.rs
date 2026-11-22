//! Touchstone `.sNp` export (`CP-AF-M10.1`) — turn a swept 2-port
//! S-parameter result into the file every RF tool reads: ADS, AWR, scikit-rf,
//! and the VNAs themselves all speak **Touchstone**.
//!
//! A sibling of [`crate::waveform`]: same house rules — a friendly error type
//! with `Display` + [`std::error::Error`], hand-rolled with zero dependencies
//! beyond `ohmforge-core`, and deterministic output (numbers print via `f64`'s
//! default `Display`, the shortest string that parses back to the identical
//! bits, so a golden file stays byte-exact).
//!
//! ## The dependency-free seam
//!
//! The M9 S-parameter engine (`ohmforge_sim::Engine::s_parameters`) computes a
//! 2-port at **one** frequency. A sweep is just a loop that calls it per
//! frequency and collects the results — and that loop lives in the **shell**,
//! not here. This exporter takes a solver-free view of the data:
//! `&[(f64 /*freq Hz*/, [[Complex; 2]; 2] /*S*/)]`, using
//! [`ohmforge_core::Complex`] rather than the sim crate's `SParams` type, so
//! `ohmforge-fmt` keeps `ohmforge-sim` strictly a dev-dependency — exactly as
//! [`waveform`](crate::waveform) takes [`Signal`](ohmforge_core::Signal)s, not an
//! engine.
//!
//! ## File shape ([`export`])
//!
//! A 2-port RI (real/imaginary) Touchstone:
//!
//! ```text
//! # HZ S RI R 50
//! ! freq  S11:re S11:im  S21:re S21:im  S12:re S12:im  S22:re S22:im
//! 1000000 -0.2 -0.4  0.8 -0.4  0.8 -0.4  -0.2 -0.4
//! …
//! ```
//!
//! The **option line** `# HZ S RI R <z0>` declares hertz frequencies, scattering
//! parameters, rectangular (real/imaginary) number pairs, and the reference
//! impedance. Each data row is a frequency then the four S-parameters as
//! `re im` pairs — in Touchstone's 2-port **column order `S11 S21 S12 S22`**
//! (the historical transpose: `S21`/`S12` are swapped relative to the natural
//! matrix row-major order), which [`export`] writes and [`parse`] reads back.
//!
//! ## Round trip ([`parse`])
//!
//! [`parse`] reads an RI `.s2p` back into a [`Touchstone`] — the reference
//! impedance and one `[[Complex; 2]; 2]` per frequency — so a round trip through
//! [`export`] is checkable without a solver.

use std::error::Error;
use std::fmt::{self, Write as _};

use ohmforge_core::Complex;

/// A Touchstone export/parse failure. Like [`WaveformError`](crate::WaveformError),
/// it is about the *shape* of the data rather than a source line, except for the
/// parse variants that carry the offending 1-based line.
#[derive(Debug, Clone, PartialEq)]
pub enum TouchstoneError {
    /// No frequency points were given — there is nothing to export.
    NoPoints,
    /// A frequency was negative or non-finite (Touchstone frequencies are
    /// non-negative hertz).
    BadFrequency {
        /// The offending frequency value.
        freq: f64,
    },
    /// The option line (`# HZ S RI R <z0>`) was missing on parse.
    MissingOptionLine,
    /// The option line named a format this reader does not support. Only 2-port
    /// `HZ S RI R <z0>` is read back today.
    UnsupportedFormat {
        /// The option line as read.
        line: String,
    },
    /// A data row did not have the `freq` + 8 values an RI 2-port needs.
    BadDataRow {
        /// The 1-based source line.
        line: usize,
        /// How many whitespace fields the row actually had.
        got: usize,
    },
    /// A numeric field could not be parsed as an `f64`.
    BadNumber {
        /// The 1-based source line.
        line: usize,
        /// The token that failed to parse.
        token: String,
    },
}

impl fmt::Display for TouchstoneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TouchstoneError::NoPoints => write!(f, "no frequency points to export"),
            TouchstoneError::BadFrequency { freq } => {
                write!(f, "frequency {freq} is negative or non-finite")
            }
            TouchstoneError::MissingOptionLine => {
                write!(f, "missing the `# HZ S RI R <z0>` option line")
            }
            TouchstoneError::UnsupportedFormat { line } => write!(
                f,
                "unsupported Touchstone format {line:?} (only `HZ S RI R <z0>` is read)"
            ),
            TouchstoneError::BadDataRow { line, got } => write!(
                f,
                "line {line}: expected a frequency and 8 S values (RI 2-port), found {got} field(s)"
            ),
            TouchstoneError::BadNumber { line, token } => {
                write!(f, "line {line}: {token:?} is not a number")
            }
        }
    }
}

impl Error for TouchstoneError {}

/// A parsed Touchstone 2-port: its reference impedance and one S-matrix per
/// frequency. Returned by [`parse`].
#[derive(Debug, Clone, PartialEq)]
pub struct Touchstone {
    /// The reference impedance `Z₀` (ohms) from the option line.
    pub z0: f64,
    /// The swept points: `(frequency in Hz, S-matrix)` with the S-matrix indexed
    /// `s[row][col]` (so `s[0][0]` is S11, `s[1][0]` is S21, …), the natural
    /// row-major order — the Touchstone column transpose is handled on the way
    /// in and out.
    pub points: Vec<(f64, [[Complex; 2]; 2])>,
}

/// Export a swept 2-port S-parameter result as a Touchstone `.s2p` string in the
/// `# HZ S RI R <z0>` format. See the module docs for the file shape and the
/// 2-port column order.
///
/// `points` pairs each frequency (hertz) with its S-matrix indexed
/// `s[row][col]`. `z0` is the reference impedance (ohms) written on the option
/// line. Numbers print with `f64`'s default `Display`, so the output is the
/// shortest round-trippable form and byte-stable for golden tests.
///
/// Fails with [`TouchstoneError::NoPoints`] for an empty sweep and
/// [`TouchstoneError::BadFrequency`] for a negative or non-finite frequency.
pub fn export(points: &[(f64, [[Complex; 2]; 2])], z0: f64) -> Result<String, TouchstoneError> {
    if points.is_empty() {
        return Err(TouchstoneError::NoPoints);
    }
    for &(freq, _) in points {
        if !freq.is_finite() || freq < 0.0 {
            return Err(TouchstoneError::BadFrequency { freq });
        }
    }

    let mut out = String::new();
    // The option line: hertz, S-parameters, real/imaginary pairs, reference Z0.
    let _ = writeln!(out, "# HZ S RI R {z0}");
    // A column-legend comment (Touchstone comments start with `!`).
    out.push_str("! freq S11:re S11:im S21:re S21:im S12:re S12:im S22:re S22:im\n");

    for &(freq, s) in points {
        // Touchstone 2-port column order is S11 S21 S12 S22 — note the S21/S12
        // transpose relative to the natural matrix row-major order.
        let s11 = s[0][0];
        let s21 = s[1][0];
        let s12 = s[0][1];
        let s22 = s[1][1];
        let _ = writeln!(
            out,
            "{freq} {} {} {} {} {} {} {} {}",
            s11.re, s11.im, s21.re, s21.im, s12.re, s12.im, s22.re, s22.im
        );
    }

    Ok(out)
}

/// Parse an RI 2-port Touchstone (`# HZ S RI R <z0>`) back into a [`Touchstone`].
/// The inverse of [`export`]: the 2-port column order `S11 S21 S12 S22` is read
/// back into the natural row-major S-matrix, so a round trip recovers the input.
///
/// Comment lines (`!`) and blank lines are skipped. Only the `HZ S RI R <z0>`
/// format is supported today — other units/parameters/formats are rejected with
/// [`TouchstoneError::UnsupportedFormat`].
pub fn parse(src: &str) -> Result<Touchstone, TouchstoneError> {
    let mut z0: Option<f64> = None;
    let mut points: Vec<(f64, [[Complex; 2]; 2])> = Vec::new();

    for (i, raw) in src.lines().enumerate() {
        let line = i + 1;
        let text = strip_comment(raw).trim();
        if text.is_empty() {
            continue;
        }
        if let Some(rest) = text.strip_prefix('#') {
            z0 = Some(parse_option_line(rest)?);
            continue;
        }
        // A data row: frequency then 8 RI values.
        let z0 = z0.ok_or(TouchstoneError::MissingOptionLine)?;
        let _ = z0; // option line already consumed; data rows just need it present
        let fields: Vec<&str> = text.split_whitespace().collect();
        if fields.len() != 9 {
            return Err(TouchstoneError::BadDataRow {
                line,
                got: fields.len(),
            });
        }
        let num = |idx: usize| -> Result<f64, TouchstoneError> {
            fields[idx]
                .parse::<f64>()
                .map_err(|_| TouchstoneError::BadNumber {
                    line,
                    token: fields[idx].to_string(),
                })
        };
        let freq = num(0)?;
        let s11 = Complex::new(num(1)?, num(2)?);
        let s21 = Complex::new(num(3)?, num(4)?);
        let s12 = Complex::new(num(5)?, num(6)?);
        let s22 = Complex::new(num(7)?, num(8)?);
        // Undo the Touchstone column transpose back into row-major `s[row][col]`.
        points.push((freq, [[s11, s12], [s21, s22]]));
    }

    let z0 = z0.ok_or(TouchstoneError::MissingOptionLine)?;
    if points.is_empty() {
        return Err(TouchstoneError::NoPoints);
    }
    Ok(Touchstone { z0, points })
}

/// Parse the option line body (everything after `#`), returning the reference
/// impedance. Only `HZ S RI R <z0>` is accepted.
fn parse_option_line(rest: &str) -> Result<f64, TouchstoneError> {
    let toks: Vec<String> = rest
        .split_whitespace()
        .map(|t| t.to_ascii_uppercase())
        .collect();
    // Expect: HZ S RI R <z0>
    let ok =
        toks.len() == 5 && toks[0] == "HZ" && toks[1] == "S" && toks[2] == "RI" && toks[3] == "R";
    if !ok {
        return Err(TouchstoneError::UnsupportedFormat {
            line: format!("#{rest}"),
        });
    }
    toks[4]
        .parse::<f64>()
        .map_err(|_| TouchstoneError::UnsupportedFormat {
            line: format!("#{rest}"),
        })
}

/// Everything before the first `!` Touchstone comment marker.
fn strip_comment(line: &str) -> &str {
    match line.find('!') {
        Some(i) => &line[..i],
        None => line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The known 2-port used in the byte-exact golden and the round trip: a shunt
    /// element across a 50 Ω line at three normalized points `k = ωCZ₀ ∈ {0,1,2}`,
    /// whose exact S-parameters are
    /// `S11 = S22 = −jk/(2+jk)`, `S21 = S12 = 2/(2+jk)`:
    ///
    /// | k | S11      | S21      |
    /// |---|----------|----------|
    /// | 0 | 0 + 0j   | 1 + 0j   |
    /// | 1 | −0.2−0.4j| 0.8−0.4j |
    /// | 2 | −0.5−0.5j| 0.5−0.5j |
    fn shunt_cap_sweep() -> Vec<(f64, [[Complex; 2]; 2])> {
        let point = |freq: f64, s11: Complex, s21: Complex| {
            // Reciprocal, symmetric 2-port: S12 = S21, S22 = S11.
            (freq, [[s11, s21], [s21, s11]])
        };
        vec![
            point(1e6, Complex::new(0.0, 0.0), Complex::new(1.0, 0.0)),
            point(2e6, Complex::new(-0.2, -0.4), Complex::new(0.8, -0.4)),
            point(3e6, Complex::new(-0.5, -0.5), Complex::new(0.5, -0.5)),
        ]
    }

    #[test]
    fn exports_byte_exact_s2p() {
        let s2p = export(&shunt_cap_sweep(), 50.0).unwrap();
        let expected = "\
# HZ S RI R 50
! freq S11:re S11:im S21:re S21:im S12:re S12:im S22:re S22:im
1000000 0 0 1 0 1 0 0 0
2000000 -0.2 -0.4 0.8 -0.4 0.8 -0.4 -0.2 -0.4
3000000 -0.5 -0.5 0.5 -0.5 0.5 -0.5 -0.5 -0.5
";
        assert_eq!(s2p, expected);
    }

    #[test]
    fn round_trips_through_parse_within_tolerance() {
        let sweep = shunt_cap_sweep();
        let s2p = export(&sweep, 50.0).unwrap();
        let back = parse(&s2p).unwrap();
        assert_eq!(back.z0, 50.0);
        assert_eq!(back.points.len(), sweep.len());
        for ((f0, s0), (f1, s1)) in sweep.iter().zip(back.points.iter()) {
            assert_eq!(f0, f1);
            for r in 0..2 {
                for c in 0..2 {
                    // |S| matches within 1e-9 — the proposal's round-trip gate.
                    assert!(
                        (s0[r][c].magnitude() - s1[r][c].magnitude()).abs() < 1e-9,
                        "S[{r}][{c}] magnitude drifted"
                    );
                    assert!((s0[r][c].re - s1[r][c].re).abs() < 1e-12);
                    assert!((s0[r][c].im - s1[r][c].im).abs() < 1e-12);
                }
            }
        }
    }

    #[test]
    fn parse_undoes_the_s21_s12_transpose() {
        // A deliberately asymmetric matrix so the column transpose is observable:
        // S11=1, S21=2, S12=3, S22=4 (s[row][col]).
        let s = [
            [Complex::new(1.0, 0.0), Complex::new(3.0, 0.0)],
            [Complex::new(2.0, 0.0), Complex::new(4.0, 0.0)],
        ];
        let text = export(&[(1e9, s)], 75.0).unwrap();
        // The data row writes the Touchstone order S11 S21 S12 S22 = 1 2 3 4.
        assert!(
            text.contains("1000000000 1 0 2 0 3 0 4 0"),
            "column order S11 S21 S12 S22: {text}"
        );
        let back = parse(&text).unwrap();
        assert_eq!(back.z0, 75.0);
        assert_eq!(back.points[0].1, s, "round trip recovers s[row][col]");
    }

    #[test]
    fn export_rejects_empty_and_bad_frequencies() {
        assert_eq!(export(&[], 50.0).unwrap_err(), TouchstoneError::NoPoints);
        let s = [[Complex::ZERO; 2]; 2];
        assert_eq!(
            export(&[(-1.0, s)], 50.0).unwrap_err(),
            TouchstoneError::BadFrequency { freq: -1.0 }
        );
    }

    #[test]
    fn parse_rejects_unsupported_format_and_malformed_rows() {
        // Magnitude/angle format is not supported yet.
        assert!(matches!(
            parse("# HZ S MA R 50\n1e9 1 0 1 0 1 0 1 0\n").unwrap_err(),
            TouchstoneError::UnsupportedFormat { .. }
        ));
        // A data row with too few columns.
        assert!(matches!(
            parse("# HZ S RI R 50\n1e9 1 0 1 0\n").unwrap_err(),
            TouchstoneError::BadDataRow { line: 2, got: 5 }
        ));
        // No option line at all.
        assert_eq!(
            parse("1e9 1 0 1 0 1 0 1 0\n").unwrap_err(),
            TouchstoneError::MissingOptionLine
        );
    }
}
