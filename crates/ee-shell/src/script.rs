//! The `eesh` scripting engine: loops, parameters, and deterministic replay
//! (`CP-UX-M1.5`).
//!
//! This layer sits *on top of* [`Shell::execute`](crate::Shell::execute): it adds
//! control flow and variables, but it never simulates or mutates a circuit
//! itself — it expands a script into a flat sequence of ordinary command lines
//! and runs each one through the shell. That split is what makes replay exact.
//!
//! # Grammar
//!
//! One statement per line. Blank lines and lines beginning with `#` are ignored.
//!
//! ```text
//! set <name> <value…>        bind a variable to the rest of the line
//! repeat <count> [as <var>] {   loop <count> times; <var>, if named, counts 1..=count
//!   <statements…>
//! }
//! <command>                  any eesh command (add/wire/probe/run/…), run verbatim
//! ```
//!
//! `<count>` may itself be a variable (`repeat $n { … }`). Inside any line,
//! `$name` and `${name}` expand to the variable's current value — so a parameter
//! sweep is just a `repeat … as v` around a command that interpolates `$v`:
//!
//! ```text
//! repeat 3 as v {
//!   add V1 in gnd $v
//!   run op
//! }
//! ```
//!
//! # Deterministic replay
//!
//! [`Shell::run_script`](crate::Shell::run_script) returns both the transcript
//! and a [`Recording`] — the exact expanded command lines it executed, in order.
//! Because the recording is the *already-expanded* list and `execute` is
//! deterministic, [`Recording::replay`] re-runs them against a fresh shell and
//! reproduces a byte-identical transcript. Expansion (loops/params) happens once;
//! replay is pure execution.

use std::collections::HashMap;
use std::error::Error;
use std::fmt;

use crate::{Shell, ShellError};

/// One parsed statement of a script, tagged with its 1-based source line.
#[derive(Debug, Clone, PartialEq)]
enum Stmt {
    /// `set <name> <value>` — bind (or rebind) a variable.
    Set {
        name: String,
        value: String,
        line: usize,
    },
    /// A command line to run verbatim (after `$var` expansion).
    Command { text: String, line: usize },
    /// `repeat <count> [as <var>] { <body> }`.
    Repeat {
        count: String,
        var: Option<String>,
        body: Vec<Stmt>,
        line: usize,
    },
}

/// The ordered list of expanded command lines a script run executed.
///
/// This is the deterministic-replay artifact: [`replay`](Recording::replay) runs
/// exactly these lines, in order, against a fresh [`Shell`] and rebuilds the same
/// transcript. The lines are already fully expanded — no loops or `$vars` remain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recording {
    commands: Vec<String>,
}

impl Recording {
    /// The expanded command lines, in execution order.
    pub fn commands(&self) -> &[String] {
        &self.commands
    }

    /// Replay the recording against a fresh shell, returning the transcript.
    ///
    /// This is the replay half of the determinism contract: `run_script`'s
    /// transcript equals `run_script`'s recording replayed (from the same empty
    /// starting state).
    pub fn replay(&self) -> Result<String, ScriptError> {
        let mut shell = Shell::new();
        self.replay_with(&mut shell)
    }

    /// Replay the recording against a caller-supplied shell.
    ///
    /// For an identical transcript the shell must start in the same state the
    /// original run did (typically empty).
    pub fn replay_with(&self, shell: &mut Shell) -> Result<String, ScriptError> {
        run_lines(shell, &self.commands)
    }
}

/// The result of running a script: what it printed, and how to replay it.
#[derive(Debug, Clone)]
pub struct ScriptRun {
    /// The combined transcript (each command echoed, then its output).
    pub transcript: String,
    /// The expanded command sequence, for deterministic replay.
    pub recording: Recording,
}

impl Shell {
    /// Run an `eesh` script against this shell, expanding loops and `$vars`.
    ///
    /// Returns the transcript plus a [`Recording`] for deterministic replay. The
    /// script mutates this shell's circuit, so the circuit is available afterward
    /// via [`circuit`](Shell::circuit).
    pub fn run_script(&mut self, src: &str) -> Result<ScriptRun, ScriptError> {
        let program = parse(src)?;
        let mut env: HashMap<String, String> = HashMap::new();
        let mut commands = Vec::new();
        flatten(&program, &mut env, &mut commands)?;
        let transcript = run_lines(self, &commands)?;
        Ok(ScriptRun {
            transcript,
            recording: Recording { commands },
        })
    }
}

/// Execute a flat list of expanded command lines, building the transcript.
///
/// Shared by the initial run and by replay so both produce the same text. Each
/// command is echoed (`> line`) and followed by its non-empty output.
fn run_lines(shell: &mut Shell, lines: &[String]) -> Result<String, ScriptError> {
    let mut out = String::new();
    for line in lines {
        let result = shell.execute(line).map_err(|source| ScriptError::Exec {
            line: line.clone(),
            source,
        })?;
        out.push_str("> ");
        out.push_str(line);
        out.push('\n');
        if !result.is_empty() {
            out.push_str(&result);
            out.push('\n');
        }
    }
    Ok(out)
}

// ── expansion ─────────────────────────────────────────────────────────────

/// Flatten a parsed program into expanded command lines, maintaining `env`.
///
/// `set` updates the environment and emits nothing; `repeat` binds its loop
/// variable (saving and restoring any shadowed binding) and expands its body once
/// per iteration; a command is `$var`-substituted and pushed.
fn flatten(
    stmts: &[Stmt],
    env: &mut HashMap<String, String>,
    out: &mut Vec<String>,
) -> Result<(), ScriptError> {
    for stmt in stmts {
        match stmt {
            Stmt::Set { name, value, line } => {
                let expanded = substitute(value, env, *line)?;
                env.insert(name.clone(), expanded);
            }
            Stmt::Command { text, line } => {
                out.push(substitute(text, env, *line)?);
            }
            Stmt::Repeat {
                count,
                var,
                body,
                line,
            } => {
                let count_str = substitute(count, env, *line)?;
                let n: usize = count_str.parse().map_err(|_| ScriptError::BadCount {
                    line: *line,
                    value: count_str.clone(),
                })?;
                // Save any binding this loop variable shadows, to restore after.
                let shadowed = var.as_ref().and_then(|v| env.get(v).cloned());
                for i in 1..=n {
                    if let Some(v) = var {
                        env.insert(v.clone(), i.to_string());
                    }
                    flatten(body, env, out)?;
                }
                if let Some(v) = var {
                    match &shadowed {
                        Some(prev) => env.insert(v.clone(), prev.clone()),
                        None => env.remove(v),
                    };
                }
            }
        }
    }
    Ok(())
}

/// Expand `$name` and `${name}` references in `text` against `env`.
///
/// A `$` not introducing an identifier is left literal. An undefined variable is
/// a friendly [`ScriptError::UndefinedVar`] rather than a silent blank.
fn substitute(
    text: &str,
    env: &HashMap<String, String>,
    line: usize,
) -> Result<String, ScriptError> {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '$' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        // A `$`: read `${name}` or `$name`, else treat the `$` as a literal.
        let (name, next) = if chars.get(i + 1) == Some(&'{') {
            let mut j = i + 2;
            let mut name = String::new();
            while j < chars.len() && chars[j] != '}' {
                name.push(chars[j]);
                j += 1;
            }
            if j >= chars.len() {
                return Err(ScriptError::Parse {
                    line,
                    msg: "unterminated '${' variable reference".to_string(),
                });
            }
            (name, j + 1) // skip the closing '}'
        } else {
            let mut j = i + 1;
            let mut name = String::new();
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                name.push(chars[j]);
                j += 1;
            }
            (name, j)
        };

        if name.is_empty() {
            out.push('$'); // a lone '$' is literal
            i += 1;
            continue;
        }
        match env.get(&name) {
            Some(value) => out.push_str(value),
            None => {
                return Err(ScriptError::UndefinedVar {
                    line,
                    name: name.clone(),
                })
            }
        }
        i = next;
    }
    Ok(out)
}

// ── parsing ─────────────────────────────────────────────────────────────

/// A lexed line item before nesting is resolved.
enum Item {
    Set {
        name: String,
        value: String,
        line: usize,
    },
    Open {
        count: String,
        var: Option<String>,
        line: usize,
    },
    Close {
        line: usize,
    },
    Cmd {
        text: String,
        line: usize,
    },
}

/// Parse script source into a nested statement program.
fn parse(src: &str) -> Result<Vec<Stmt>, ScriptError> {
    let items = lex(src)?;
    let mut iter = items.into_iter().peekable();
    let (stmts, trailing) = parse_block(&mut iter)?;
    if let Some(line) = trailing {
        return Err(ScriptError::Parse {
            line,
            msg: "unexpected '}' with no matching 'repeat'".to_string(),
        });
    }
    Ok(stmts)
}

/// Parse statements until a `}` (returned as `Some(line)`) or end of input.
fn parse_block(
    iter: &mut std::iter::Peekable<std::vec::IntoIter<Item>>,
) -> Result<(Vec<Stmt>, Option<usize>), ScriptError> {
    let mut stmts = Vec::new();
    while let Some(item) = iter.next() {
        match item {
            Item::Close { line } => return Ok((stmts, Some(line))),
            Item::Set { name, value, line } => stmts.push(Stmt::Set { name, value, line }),
            Item::Cmd { text, line } => stmts.push(Stmt::Command { text, line }),
            Item::Open { count, var, line } => {
                let (body, closed) = parse_block(iter)?;
                if closed.is_none() {
                    return Err(ScriptError::Parse {
                        line,
                        msg: "'repeat' block is missing its closing '}'".to_string(),
                    });
                }
                stmts.push(Stmt::Repeat {
                    count,
                    var,
                    body,
                    line,
                });
            }
        }
    }
    Ok((stmts, None))
}

/// Lex source into line items, skipping blanks and `#` comments.
fn lex(src: &str) -> Result<Vec<Item>, ScriptError> {
    let mut items = Vec::new();
    for (idx, raw) in src.lines().enumerate() {
        let line = idx + 1;
        let t = raw.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if t == "}" {
            items.push(Item::Close { line });
            continue;
        }
        if let Some(head) = t.strip_suffix('{') {
            items.push(parse_repeat_head(head.trim(), line)?);
            continue;
        }
        if let Some(rest) = t.strip_prefix("set ") {
            let rest = rest.trim();
            let (name, value) =
                rest.split_once(char::is_whitespace)
                    .ok_or_else(|| ScriptError::Parse {
                        line,
                        msg: "`set` needs a name and a value: set <name> <value>".to_string(),
                    })?;
            items.push(Item::Set {
                name: name.to_string(),
                value: value.trim().to_string(),
                line,
            });
            continue;
        }
        items.push(Item::Cmd {
            text: t.to_string(),
            line,
        });
    }
    Ok(items)
}

/// Parse the head of a `repeat … {` opener: `repeat <count> [as <var>]`.
fn parse_repeat_head(head: &str, line: usize) -> Result<Item, ScriptError> {
    let words: Vec<&str> = head.split_whitespace().collect();
    let bad = |msg: &str| ScriptError::Parse {
        line,
        msg: msg.to_string(),
    };
    match words.as_slice() {
        ["repeat", count] => Ok(Item::Open {
            count: (*count).to_string(),
            var: None,
            line,
        }),
        ["repeat", count, "as", var] => Ok(Item::Open {
            count: (*count).to_string(),
            var: Some((*var).to_string()),
            line,
        }),
        ["repeat", ..] => Err(bad("usage: repeat <count> [as <var>] {")),
        _ => Err(bad("expected a `repeat <count> [as <var>] {` block header")),
    }
}

/// Everything that can go wrong parsing or running a script.
#[derive(Debug, Clone, PartialEq)]
pub enum ScriptError {
    /// The script could not be parsed (unbalanced braces, bad header, …).
    Parse { line: usize, msg: String },
    /// A `repeat` count was not a non-negative integer.
    BadCount { line: usize, value: String },
    /// A `$var` referenced a variable that was never `set` (nor a loop binding).
    UndefinedVar { line: usize, name: String },
    /// A command the script expanded to was rejected by the shell.
    Exec { line: String, source: ShellError },
}

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScriptError::Parse { line, msg } => write!(f, "script line {line}: {msg}"),
            ScriptError::BadCount { line, value } => {
                write!(
                    f,
                    "script line {line}: repeat count {value:?} is not a whole number"
                )
            }
            ScriptError::UndefinedVar { line, name } => {
                write!(
                    f,
                    "script line {line}: undefined variable ${name} — `set {name} …` first"
                )
            }
            ScriptError::Exec { line, source } => {
                write!(f, "while running {line:?}: {source}")
            }
        }
    }
}

impl Error for ScriptError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ScriptError::Exec { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A parameter sweep over the transient window: simulate the same RC for
    /// 1 ms, 2 ms, then 3 ms. Each iteration renders a distinct, deterministic
    /// waveform frame. This is the acceptance gate — it must replay identically.
    const SWEEP: &str = "\
# sweep the transient window from 1 ms to 3 ms
add V1 in gnd 5
add R1 in out 1k
add C1 out gnd 1u
probe out
repeat 3 as win {
  run tran ${win}m
}";

    #[test]
    fn param_sweep_runs_and_replays_identically() {
        let mut shell = Shell::new();
        let run = shell.run_script(SWEEP).unwrap();

        // The loop expanded the window parameter into three distinct runs, and a
        // real scope frame rendered for each.
        for dur in ["> run tran 1m", "> run tran 2m", "> run tran 3m"] {
            assert!(
                run.transcript.contains(dur),
                "swept {dur}:\n{}",
                run.transcript
            );
        }
        assert!(
            run.transcript.contains('\u{2524}'),
            "a braille scope frame rendered:\n{}",
            run.transcript
        );

        // The gate: the recording replays to a byte-identical transcript, which
        // means the deterministic *simulation* output reproduces too.
        let replayed = run.recording.replay().unwrap();
        assert_eq!(
            run.transcript, replayed,
            "replay must reproduce the transcript exactly"
        );
    }

    /// The shipped tutorial sweep script (`examples/tutorial/sweep.eesh`) must
    /// expand, run, and replay identically — this both verifies the documented
    /// example and guards it against drift.
    #[test]
    fn tutorial_sweep_example_expands_and_replays() {
        const SWEEP_EESH: &str = include_str!("../../../examples/tutorial/sweep.eesh");
        let mut shell = Shell::new();
        let run = shell.run_script(SWEEP_EESH).unwrap();
        // `$r` and the `repeat … as win` loop expand to three distinct runs.
        assert_eq!(
            run.recording.commands(),
            &[
                "add V1 in gnd 5".to_string(),
                "add R1 in out 1k".to_string(),
                "add C1 out gnd 1u".to_string(),
                "probe out".to_string(),
                "run tran 1m".to_string(),
                "run tran 2m".to_string(),
                "run tran 3m".to_string(),
            ],
            "sweep.eesh expansion drifted:\n{:#?}",
            run.recording.commands()
        );
        assert!(
            run.transcript.contains('\u{2524}'),
            "a braille scope frame rendered:\n{}",
            run.transcript
        );
        assert_eq!(
            run.transcript,
            run.recording.replay().unwrap(),
            "the sweep must replay to a byte-identical transcript"
        );
    }

    #[test]
    fn set_rebinds_a_variable_between_lines() {
        // A later `set` overrides the earlier binding; each `add` interpolates the
        // value current at its point in the script.
        let mut shell = Shell::new();
        let run = shell
            .run_script("set v 2\nadd R1 a b ${v}k\nset v 5\nadd R2 a b ${v}k")
            .unwrap();
        assert_eq!(
            run.recording.commands(),
            &["add R1 a b 2k".to_string(), "add R2 a b 5k".to_string()]
        );
    }

    #[test]
    fn loops_expand_to_the_right_command_sequence() {
        let mut shell = Shell::new();
        let run = shell
            .run_script("repeat 2 as i {\n  add R$i a b 1k\n}")
            .unwrap();
        assert_eq!(
            run.recording.commands(),
            &["add R1 a b 1k".to_string(), "add R2 a b 1k".to_string()]
        );
    }

    #[test]
    fn nested_loops_bind_independent_variables() {
        let mut shell = Shell::new();
        let run = shell
            .run_script("repeat 2 as i {\n  repeat 2 as j {\n    add R${i}_${j} a b 1k\n  }\n}")
            .unwrap();
        // The expansion is the Cartesian product of the two loop variables, in
        // order — each inner `$j` sweeps fully before `$i` advances.
        assert_eq!(
            run.recording.commands(),
            &[
                "add R1_1 a b 1k".to_string(),
                "add R1_2 a b 1k".to_string(),
                "add R2_1 a b 1k".to_string(),
                "add R2_2 a b 1k".to_string(),
            ]
        );
    }

    #[test]
    fn repeat_count_can_come_from_a_variable() {
        let mut shell = Shell::new();
        let run = shell
            .run_script("set n 3\nrepeat $n as i {\n  add R$i a b 1k\n}")
            .unwrap();
        assert_eq!(run.recording.commands().len(), 3);
    }

    #[test]
    fn undefined_variable_is_a_friendly_error() {
        let mut shell = Shell::new();
        let err = shell.run_script("probe $missing").unwrap_err();
        assert!(matches!(err, ScriptError::UndefinedVar { .. }));
        assert!(err.to_string().contains("missing"), "{err}");
    }

    #[test]
    fn unbalanced_braces_are_rejected() {
        let mut shell = Shell::new();
        assert!(matches!(
            shell.run_script("repeat 2 {\n  show").unwrap_err(),
            ScriptError::Parse { .. }
        ));
        assert!(matches!(
            shell.run_script("show\n}").unwrap_err(),
            ScriptError::Parse { .. }
        ));
    }

    #[test]
    fn a_bad_repeat_count_is_reported() {
        let mut shell = Shell::new();
        let err = shell.run_script("repeat lots {\n  show\n}").unwrap_err();
        assert!(matches!(err, ScriptError::BadCount { .. }), "{err}");
    }

    #[test]
    fn a_shell_error_inside_a_script_names_the_offending_line() {
        let mut shell = Shell::new();
        let err = shell.run_script("add R1 a b 1k\nprobe ghost").unwrap_err();
        match err {
            ScriptError::Exec { line, source } => {
                assert_eq!(line, "probe ghost");
                assert!(matches!(source, ShellError::UnknownNode(_)));
            }
            other => panic!("expected an Exec error, got {other:?}"),
        }
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let mut shell = Shell::new();
        let run = shell
            .run_script("# a comment\n\n  # indented comment\nadd R1 a b 1k\n")
            .unwrap();
        assert_eq!(run.recording.commands(), &["add R1 a b 1k".to_string()]);
    }

    #[test]
    fn lone_dollar_is_literal() {
        let env = HashMap::new();
        assert_eq!(substitute("cost is $ 5", &env, 1).unwrap(), "cost is $ 5");
    }
}
