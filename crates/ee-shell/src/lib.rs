//! # ee-shell (`eesh`)
//!
//! The dedicated shell: a parser for the command language, tab-completion,
//! history, and (from M1) an embedded scripting engine for loops/parameters/
//! automation. The shell mutates the same `ee_core::Circuit` the TUI and AI do.
//! Full command spec in docs/SHELL.md.

/// A parsed shell command. `TODO(M0)`: grow the real grammar (`add`, `wire`,
/// `probe`, `run`, `show`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help(Option<String>),
    /// Anything not yet recognized, kept verbatim for error reporting.
    Unknown(String),
}

/// Parse one line of shell input.
pub fn parse(line: &str) -> Command {
    let line = line.trim();
    match line.split_whitespace().next() {
        Some("help") => Command::Help(line.split_whitespace().nth(1).map(str::to_owned)),
        _ => Command::Unknown(line.to_owned()),
    }
}
