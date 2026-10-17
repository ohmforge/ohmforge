//! `ee` — the ee-assist binary.
//!
//! At M0 this is the interactive shell: an `eesh` REPL driven by
//! [`ee_shell::Shell`], which parses commands, mutates the circuit, runs the
//! engine, and renders waveforms in the terminal.
//!
//! Interactive on a TTY (line editing, history, tab completion via `rustyline`)
//! or scripted from a pipe — the same `Shell` path either way. `ee demo` runs a
//! built-in RC low-pass script. The full TUI event loop lands as this grows
//! (docs/ROADMAP.md).

use std::io::{self, BufRead, IsTerminal};
use std::path::PathBuf;

use ee_shell::Shell;
use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{CompletionType, Config, Context, Editor, Helper};

/// The built-in demo: an RC low-pass, typed as shell commands.
const DEMO: &str = "\
add V1 in  gnd 5
add R1 in  out 1k
add C1 out gnd 1u
probe out
run tran 5m
show";

// ANSI styling for the interactive prompt (only used on a TTY).
const PROMPT: &str = "\x1b[1;36mee>\x1b[0m ";
const RED: &str = "\x1b[31m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

fn main() {
    let mut shell = Shell::new();

    if std::env::args().nth(1).as_deref() == Some("demo") {
        run_script(&mut shell, DEMO.lines());
        return;
    }

    let stdin = io::stdin();
    if stdin.is_terminal() {
        if let Err(e) = repl(&mut shell) {
            eprintln!("{RED}repl error:{RESET} {e}");
        }
    } else {
        // Piped / scripted: execute each line, output to stdout, errors to stderr.
        run_script(&mut shell, stdin.lock().lines().map_while(Result::ok));
    }
}

/// Run a sequence of command lines, printing each command's output.
fn run_script<I, S>(shell: &mut Shell, lines: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    for line in lines {
        match shell.execute(line.as_ref()) {
            Ok(out) if !out.is_empty() => println!("{out}"),
            Ok(_) => {}
            Err(e) => eprintln!("error: {e}"),
        }
    }
}

/// Interactive read-eval-print loop with line editing, history, and completion.
fn repl(shell: &mut Shell) -> rustyline::Result<()> {
    let config = Config::builder()
        .completion_type(CompletionType::List)
        .auto_add_history(true)
        .build();
    let mut rl: Editor<EeHelper, DefaultHistory> = Editor::with_config(config)?;
    rl.set_helper(Some(EeHelper::new()));
    let history = history_path();
    let _ = rl.load_history(&history);

    println!("{DIM}ee-assist — eesh (M0). Tab completes, ↑/↓ history, Ctrl-R search, Ctrl-D quits.{RESET}");
    println!("{DIM}Try: add R1 in out 1k · probe out · run tran 5m · show · help{RESET}\n");

    loop {
        match rl.readline(PROMPT) {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match shell.execute(line) {
                    Ok(out) if !out.is_empty() => println!("{out}"),
                    Ok(_) => {}
                    Err(e) => eprintln!("{RED}error:{RESET} {e}"),
                }
                // Refresh completion with any nodes the command introduced.
                if let Some(helper) = rl.helper_mut() {
                    helper.refresh(shell);
                }
            }
            Err(ReadlineError::Interrupted) => continue, // Ctrl-C clears the line
            Err(ReadlineError::Eof) => {
                println!("{DIM}bye.{RESET}");
                break;
            }
            Err(e) => {
                eprintln!("{RED}input error:{RESET} {e}");
                break;
            }
        }
    }

    let _ = rl.save_history(&history);
    Ok(())
}

/// Where the persistent command history lives (`~/.ee_history`).
fn history_path() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".ee_history")
}

/// Tab completion: commands in the first position, known node names after.
struct EeHelper {
    commands: Vec<String>,
    nodes: Vec<String>,
}

impl EeHelper {
    fn new() -> Self {
        Self {
            commands: Shell::COMMANDS.iter().map(|s| (*s).to_string()).collect(),
            nodes: Vec::new(),
        }
    }

    /// Pull the latest named nodes from the shell so completion stays current.
    fn refresh(&mut self, shell: &Shell) {
        self.nodes = shell
            .node_names()
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        self.nodes.sort();
    }
}

impl Completer for EeHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let start = line[..pos].rfind(char::is_whitespace).map_or(0, |i| i + 1);
        let word = &line[start..pos];
        // First token → a command; later tokens → a node name.
        let pool: &[String] = if start == 0 {
            &self.commands
        } else {
            &self.nodes
        };
        let candidates = pool
            .iter()
            .filter(|c| c.starts_with(word))
            .map(|c| Pair {
                display: c.clone(),
                replacement: c.clone(),
            })
            .collect();
        Ok((start, candidates))
    }
}

// The remaining Helper facets use their default (no-op) behaviour.
impl Hinter for EeHelper {
    type Hint = String;
}
impl Highlighter for EeHelper {}
impl Validator for EeHelper {}
impl Helper for EeHelper {}
