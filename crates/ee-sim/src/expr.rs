//! A tiny, dependency-free expression evaluator for behavioral time sources
//! (`CP-SIM-M3.1`).
//!
//! It parses and evaluates a scalar function of the single variable `t` (seconds)
//! so an independent source can carry an arbitrary waveform (see
//! [`Waveform::Expr`](ee_core::graph::Waveform::Expr)). The grammar is a standard
//! recursive descent with the usual precedence — `+ −` loosest, then `* /`, then
//! unary `−`, then right-associative `^` (so `-2^2 = -4` and `2^-3 = 1/8`):
//!
//! ```text
//! expr    := term (('+' | '-') term)*
//! term    := unary (('*' | '/') unary)*
//! unary   := ('-' | '+') unary | power
//! power   := primary ('^' unary)?
//! primary := number | ident | ident '(' expr ')' | '(' expr ')'
//! ```
//!
//! `number` is a decimal literal with an optional `e`/`E` exponent (`1e-3`); a
//! bare letter word is an identifier, so the constant `e` never collides with an
//! exponent. The recognized identifiers are the constants `pi`, `e` and the
//! variable `t`, and the functions `sin cos exp ln sqrt abs`. Everything is pure
//! `f64` arithmetic, so evaluation is deterministic and `wasm32`-clean, and a
//! malformed input returns an [`ExprError`] rather than panicking.

use std::error::Error;
use std::fmt;

/// What can go wrong lexing, parsing, or evaluating an expression. Carried out of
/// the evaluator so the caller can turn it into its own error without a panic.
#[derive(Debug, Clone, PartialEq)]
pub enum ExprError {
    /// The source was empty or all whitespace.
    Empty,
    /// An unrecognized character in the input.
    BadChar(char),
    /// A numeric literal that would not parse as an `f64`.
    BadNumber(String),
    /// An identifier that is neither a known constant/variable nor a function.
    UnknownName(String),
    /// A required token (e.g. a closing parenthesis) was missing.
    Expected(char),
    /// The input ended in the middle of an expression.
    UnexpectedEnd,
    /// Extra tokens remained after a complete expression.
    Trailing,
}

impl fmt::Display for ExprError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExprError::Empty => write!(f, "empty expression"),
            ExprError::BadChar(c) => write!(f, "unexpected character '{c}'"),
            ExprError::BadNumber(s) => write!(f, "invalid number '{s}'"),
            ExprError::UnknownName(s) => write!(f, "unknown name '{s}'"),
            ExprError::Expected(c) => write!(f, "expected '{c}'"),
            ExprError::UnexpectedEnd => write!(f, "unexpected end of expression"),
            ExprError::Trailing => write!(f, "trailing tokens after expression"),
        }
    }
}

impl Error for ExprError {}

/// Evaluate `src` as a function of time `t` (seconds). Deterministic, allocation-
/// light, and panic-free: any malformed input is an [`ExprError`].
pub fn eval(src: &str, t: f64) -> Result<f64, ExprError> {
    let toks = lex(src)?;
    if toks.is_empty() {
        return Err(ExprError::Empty);
    }
    let mut p = Parser {
        toks: &toks,
        pos: 0,
        t,
    };
    let v = p.expr()?;
    if p.pos != toks.len() {
        return Err(ExprError::Trailing);
    }
    Ok(v)
}

/// A lexical token.
#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    LParen,
    RParen,
}

/// Split `src` into tokens, rejecting any unrecognized character.
fn lex(src: &str) -> Result<Vec<Tok>, ExprError> {
    let chars: Vec<char> = src.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            _ if c.is_whitespace() => i += 1,
            '+' => {
                toks.push(Tok::Plus);
                i += 1;
            }
            '-' => {
                toks.push(Tok::Minus);
                i += 1;
            }
            '*' => {
                toks.push(Tok::Star);
                i += 1;
            }
            '/' => {
                toks.push(Tok::Slash);
                i += 1;
            }
            '^' => {
                toks.push(Tok::Caret);
                i += 1;
            }
            '(' => {
                toks.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                toks.push(Tok::RParen);
                i += 1;
            }
            _ if c.is_ascii_digit() || c == '.' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                // Optional scientific exponent: `e`/`E`, an optional sign, digits.
                // Only consumed when it is well-formed, so a trailing `e` stays an
                // identifier (the constant) rather than corrupting the number.
                if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
                    let mut j = i + 1;
                    if j < chars.len() && (chars[j] == '+' || chars[j] == '-') {
                        j += 1;
                    }
                    if j < chars.len() && chars[j].is_ascii_digit() {
                        while j < chars.len() && chars[j].is_ascii_digit() {
                            j += 1;
                        }
                        i = j;
                    }
                }
                let s: String = chars[start..i].iter().collect();
                let n: f64 = s.parse().map_err(|_| ExprError::BadNumber(s.clone()))?;
                toks.push(Tok::Num(n));
            }
            _ if c.is_ascii_alphabetic() => {
                let start = i;
                while i < chars.len() && chars[i].is_ascii_alphabetic() {
                    i += 1;
                }
                toks.push(Tok::Ident(chars[start..i].iter().collect()));
            }
            _ => return Err(ExprError::BadChar(c)),
        }
    }
    Ok(toks)
}

/// A recursive-descent parser that evaluates as it goes (`t` is already known, so
/// no AST is needed).
struct Parser<'a> {
    toks: &'a [Tok],
    pos: usize,
    t: f64,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    /// Consume and return the next token.
    fn bump(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn expr(&mut self) -> Result<f64, ExprError> {
        let mut v = self.term()?;
        while let Some(op) = self.peek() {
            match op {
                Tok::Plus => {
                    self.pos += 1;
                    v += self.term()?;
                }
                Tok::Minus => {
                    self.pos += 1;
                    v -= self.term()?;
                }
                _ => break,
            }
        }
        Ok(v)
    }

    fn term(&mut self) -> Result<f64, ExprError> {
        let mut v = self.unary()?;
        while let Some(op) = self.peek() {
            match op {
                Tok::Star => {
                    self.pos += 1;
                    v *= self.unary()?;
                }
                Tok::Slash => {
                    self.pos += 1;
                    v /= self.unary()?;
                }
                _ => break,
            }
        }
        Ok(v)
    }

    fn unary(&mut self) -> Result<f64, ExprError> {
        match self.peek() {
            Some(Tok::Minus) => {
                self.pos += 1;
                Ok(-self.unary()?)
            }
            Some(Tok::Plus) => {
                self.pos += 1;
                self.unary()
            }
            _ => self.power(),
        }
    }

    fn power(&mut self) -> Result<f64, ExprError> {
        let base = self.primary()?;
        if let Some(Tok::Caret) = self.peek() {
            self.pos += 1;
            // Right-associative, and the exponent is a `unary` so `2^-3` parses.
            let exp = self.unary()?;
            Ok(base.powf(exp))
        } else {
            Ok(base)
        }
    }

    fn primary(&mut self) -> Result<f64, ExprError> {
        match self.bump() {
            Some(Tok::Num(n)) => Ok(n),
            Some(Tok::LParen) => {
                let v = self.expr()?;
                match self.bump() {
                    Some(Tok::RParen) => Ok(v),
                    _ => Err(ExprError::Expected(')')),
                }
            }
            Some(Tok::Ident(name)) => {
                if let Some(Tok::LParen) = self.peek() {
                    // Function call: `name ( expr )`.
                    self.pos += 1;
                    let arg = self.expr()?;
                    match self.bump() {
                        Some(Tok::RParen) => apply_fn(&name, arg),
                        _ => Err(ExprError::Expected(')')),
                    }
                } else {
                    constant(&name, self.t)
                }
            }
            _ => Err(ExprError::UnexpectedEnd),
        }
    }
}

/// Apply a supported unary function.
fn apply_fn(name: &str, x: f64) -> Result<f64, ExprError> {
    Ok(match name {
        "sin" => x.sin(),
        "cos" => x.cos(),
        "exp" => x.exp(),
        "ln" => x.ln(),
        "sqrt" => x.sqrt(),
        "abs" => x.abs(),
        _ => return Err(ExprError::UnknownName(name.to_string())),
    })
}

/// Resolve a constant or the time variable.
fn constant(name: &str, t: f64) -> Result<f64, ExprError> {
    Ok(match name {
        "t" => t,
        "pi" => std::f64::consts::PI,
        "e" => std::f64::consts::E,
        _ => return Err(ExprError::UnknownName(name.to_string())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() <= 1e-12 + 1e-9 * b.abs(), "{a} != {b}");
    }

    #[test]
    fn arithmetic_and_precedence() {
        approx(eval("1+2*3", 0.0).unwrap(), 7.0);
        approx(eval("(1+2)*3", 0.0).unwrap(), 9.0);
        approx(eval("10-2-3", 0.0).unwrap(), 5.0); // left-associative
        approx(eval("8/2/2", 0.0).unwrap(), 2.0); // left-associative
                                                  // `^` is right-associative and binds tighter than unary minus.
        approx(eval("2^3^2", 0.0).unwrap(), 512.0);
        approx(eval("-2^2", 0.0).unwrap(), -4.0);
        approx(eval("2^-2", 0.0).unwrap(), 0.25);
    }

    #[test]
    fn constants_variable_and_functions() {
        approx(eval("pi", 0.0).unwrap(), std::f64::consts::PI);
        approx(eval("e", 0.0).unwrap(), std::f64::consts::E);
        approx(eval("t", 1.5).unwrap(), 1.5);
        approx(eval("2*t+1", 3.0).unwrap(), 7.0);
        approx(eval("sin(0)", 0.0).unwrap(), 0.0);
        approx(eval("cos(0)", 0.0).unwrap(), 1.0);
        approx(eval("abs(0-5)", 0.0).unwrap(), 5.0);
        approx(eval("sqrt(9)", 0.0).unwrap(), 3.0);
        approx(eval("ln(e)", 0.0).unwrap(), 1.0);
        // The constant `e` and a scientific exponent coexist unambiguously.
        approx(eval("1e-3", 0.0).unwrap(), 1e-3);
        approx(eval("2*e", 0.0).unwrap(), 2.0 * std::f64::consts::E);
    }

    #[test]
    fn compound_time_waveform() {
        // exp(-t/τ)·sin(2π·f·t) at a representative instant.
        let (t, tau, f) = (0.25e-3, 1e-3, 1000.0);
        let got = eval("exp(-t/0.001)*sin(2*pi*1000*t)", t).unwrap();
        let want = (-t / tau).exp() * (2.0 * std::f64::consts::PI * f * t).sin();
        approx(got, want);
    }

    #[test]
    fn malformed_inputs_are_errors_not_panics() {
        assert_eq!(eval("", 0.0), Err(ExprError::Empty));
        assert_eq!(eval("   ", 0.0), Err(ExprError::Empty));
        assert!(matches!(eval("sin(2*", 0.0), Err(ExprError::UnexpectedEnd)));
        assert!(matches!(eval("(1+2", 0.0), Err(ExprError::Expected(')'))));
        assert!(matches!(eval("1+2)", 0.0), Err(ExprError::Trailing)));
        assert!(matches!(eval("1 2", 0.0), Err(ExprError::Trailing)));
        assert!(matches!(
            eval("foo(1)", 0.0),
            Err(ExprError::UnknownName(_))
        ));
        assert!(matches!(eval("x", 0.0), Err(ExprError::UnknownName(_))));
        assert!(matches!(eval("1 % 2", 0.0), Err(ExprError::BadChar('%'))));
        assert!(matches!(eval("1..2", 0.0), Err(ExprError::BadNumber(_))));
    }
}
