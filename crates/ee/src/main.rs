//! `ee` — the ee-assist binary.
//!
//! At M0 this is the interactive shell: an `eesh` REPL driven by
//! [`ee_shell::Shell`], which parses commands, mutates the circuit, runs the
//! engine, and renders waveforms in the terminal. It reads commands from a TTY
//! (interactive) or a pipe (scripted), so the same path is used either way.
//! `ee demo` runs a built-in RC low-pass script. The full TUI event loop lands
//! as this grows (docs/ROADMAP.md).

use std::io::{self, BufRead, IsTerminal, Write};

use ee_shell::Shell;

/// The built-in demo: an RC low-pass, typed as shell commands.
const DEMO: &str = "\
add V1 in  gnd 5
add R1 in  out 1k
add C1 out gnd 1u
probe out
run tran 5m
show";

fn main() {
    let mut shell = Shell::new();

    if std::env::args().nth(1).as_deref() == Some("demo") {
        run_script(&mut shell, DEMO.lines());
        return;
    }

    let stdin = io::stdin();
    if stdin.is_terminal() {
        repl(&mut shell);
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

/// Interactive read-eval-print loop with a prompt.
fn repl(shell: &mut Shell) {
    println!("ee-assist — eesh (M0). Type `help`, or `add R1 in out 1k`. Ctrl-D to quit.\n");
    let stdin = io::stdin();
    let mut line = String::new();
    loop {
        print!("ee> ");
        let _ = io::stdout().flush();
        line.clear();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => {
                println!();
                break; // EOF (Ctrl-D)
            }
            Ok(_) => match shell.execute(line.trim_end()) {
                Ok(out) if !out.is_empty() => println!("{out}"),
                Ok(_) => {}
                Err(e) => eprintln!("error: {e}"),
            },
            Err(e) => {
                eprintln!("input error: {e}");
                break;
            }
        }
    }
}
