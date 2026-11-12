//! A tiny declarative board-manifest format and its hand-rolled parser.
//!
//! A custom board is described in data — a name, an MCU id, a pin map, and the
//! parts soldered on — so a new board drops in without touching code. The format
//! is deliberately minimal and line-oriented so the parser stays dependency-free
//! and `wasm32`-clean:
//!
//! ```text
//! # comments start with '#'; blank lines are ignored
//! name   myboard
//! mcu    esp32
//! pin    D2=GPIO2
//! pin    LED=GPIO2
//! part   led LED            # a part of kind `led` on signal `LED`
//! part   servo D4
//! ```
//!
//! Each non-blank line is `<directive> <args…>` with whitespace-separated
//! tokens. `name` and `mcu` take one value; `pin` takes a `NAME=PORTBIT` mapping;
//! `part` takes a kind and the pin/signal it attaches to. [`parse`] turns the
//! text into a [`BoardDef`].

use crate::BoardDef;

/// An onboard part recorded by a board manifest: a `kind` (e.g. `"led"`,
/// `"servo"`, `"sevenseg"`, `"ws2812"`) attached to a named `pin`/signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnboardPart {
    /// The part kind, lower-cased (`"led"`, `"servo"`, …).
    pub kind: String,
    /// The pin or signal name it attaches to (as it appears in the pin map).
    pub pin: String,
}

/// Why a board manifest failed to parse. Carries the 1-based line number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    /// A directive this parser does not recognise.
    UnknownDirective { line: usize, directive: String },
    /// A directive with the wrong number / shape of arguments.
    BadArguments { line: usize, detail: String },
    /// A required field (`name` or `mcu`) was never given.
    Missing(&'static str),
}

impl core::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ManifestError::UnknownDirective { line, directive } => {
                write!(f, "line {line}: unknown directive `{directive}`")
            }
            ManifestError::BadArguments { line, detail } => {
                write!(f, "line {line}: {detail}")
            }
            ManifestError::Missing(field) => write!(f, "manifest is missing `{field}`"),
        }
    }
}

impl std::error::Error for ManifestError {}

/// Parse a board manifest into a [`BoardDef`].
///
/// Blank lines and `#` comments are skipped; `name` and `mcu` are required,
/// `pin` and `part` repeat. See the [module docs](self) for the grammar.
pub fn parse(src: &str) -> Result<BoardDef, ManifestError> {
    let mut model: Option<String> = None;
    let mut mcu: Option<String> = None;
    let mut pins: Vec<String> = Vec::new();
    let mut parts: Vec<OnboardPart> = Vec::new();

    for (i, raw) in src.lines().enumerate() {
        let line = i + 1;
        // Strip trailing comments and surrounding whitespace.
        let text = raw.split('#').next().unwrap_or("").trim();
        if text.is_empty() {
            continue;
        }
        let mut toks = text.split_whitespace();
        let directive = toks.next().unwrap(); // non-empty: trim() guaranteed a token
        let rest: Vec<&str> = toks.collect();

        match directive {
            "name" => model = Some(one_arg(line, "name", &rest)?.to_string()),
            "mcu" => mcu = Some(one_arg(line, "mcu", &rest)?.to_string()),
            "pin" => {
                let mapping = one_arg(line, "pin", &rest)?;
                if !is_pin_mapping(mapping) {
                    return Err(ManifestError::BadArguments {
                        line,
                        detail: format!("pin expects `NAME=PORTBIT`, got `{mapping}`"),
                    });
                }
                pins.push(mapping.to_string());
            }
            "part" => {
                if rest.len() != 2 {
                    return Err(ManifestError::BadArguments {
                        line,
                        detail: format!("part expects `<kind> <pin>`, got {} token(s)", rest.len()),
                    });
                }
                parts.push(OnboardPart {
                    kind: rest[0].to_ascii_lowercase(),
                    pin: rest[1].to_string(),
                });
            }
            other => {
                return Err(ManifestError::UnknownDirective {
                    line,
                    directive: other.to_string(),
                });
            }
        }
    }

    Ok(BoardDef {
        model: model.ok_or(ManifestError::Missing("name"))?,
        mcu: mcu.ok_or(ManifestError::Missing("mcu"))?,
        pins,
        parts,
    })
}

/// Require exactly one argument token for a directive.
fn one_arg<'a>(line: usize, directive: &str, rest: &[&'a str]) -> Result<&'a str, ManifestError> {
    match rest {
        [single] => Ok(single),
        _ => Err(ManifestError::BadArguments {
            line,
            detail: format!("{directive} expects exactly one value, got {}", rest.len()),
        }),
    }
}

/// Whether a token is a well-formed `NAME=PORTBIT` pin mapping (both sides
/// non-empty, exactly one `=`).
fn is_pin_mapping(s: &str) -> bool {
    let mut halves = s.split('=');
    match (halves.next(), halves.next(), halves.next()) {
        (Some(a), Some(b), None) => !a.is_empty() && !b.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{parse, ManifestError, OnboardPart};

    const SAMPLE: &str = "\
# a small custom board
name   alpha
mcu    esp32
pin    D2=GPIO2
pin    LED=GPIO2    # onboard LED
part   led LED
part   servo D4
";

    #[test]
    fn parses_a_well_formed_manifest() {
        let b = parse(SAMPLE).unwrap();
        assert_eq!(b.model, "alpha");
        assert_eq!(b.mcu, "esp32");
        assert_eq!(
            b.pins,
            vec!["D2=GPIO2".to_string(), "LED=GPIO2".to_string()]
        );
        assert_eq!(
            b.parts,
            vec![
                OnboardPart {
                    kind: "led".into(),
                    pin: "LED".into()
                },
                OnboardPart {
                    kind: "servo".into(),
                    pin: "D4".into()
                },
            ]
        );
    }

    #[test]
    fn missing_required_fields_error() {
        assert_eq!(parse("mcu esp32\n"), Err(ManifestError::Missing("name")));
        assert_eq!(parse("name x\n"), Err(ManifestError::Missing("mcu")));
    }

    #[test]
    fn rejects_bad_pin_and_unknown_directive() {
        assert!(matches!(
            parse("name a\nmcu m\npin D2\n"),
            Err(ManifestError::BadArguments { .. })
        ));
        assert!(matches!(
            parse("name a\nmcu m\nwidget foo\n"),
            Err(ManifestError::UnknownDirective { .. })
        ));
    }
}
