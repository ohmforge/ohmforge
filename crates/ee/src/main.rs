//! `ee` — the ee-assist binary.
//!
//! The interactive shell: an `eesh` REPL driven by [`ee_shell::Shell`] (line
//! editing, history, tab completion via `rustyline`) on a TTY, or scripted from
//! a pipe. Built-in showcases: `ee demo` (RC transient scope), `ee bode` (AC
//! Bode plot), `ee amp` (transistor amplifier operating point with a live
//! voltage overlay). The full dockable TUI event loop lands as this grows
//! (docs/ROADMAP.md).

use std::io::{self, BufRead, IsTerminal};
use std::path::PathBuf;

use ee_core::graph::PartKind;
use ee_core::units::Unit;
use ee_core::value::Value;
use ee_core::{BjtModel, Circuit, Polarity, Waveform};
use ee_shell::Shell;
use ee_sim::{Analysis, Engine};
use ee_tui::{NodeVoltages, Overlay, Plot};
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
    match std::env::args().nth(1).as_deref() {
        Some("demo") => run_script(&mut Shell::new(), DEMO.lines()),
        Some("bode") => demo_bode(),
        Some("amp") => demo_amp(),
        _ => {
            let mut shell = Shell::new();
            let stdin = io::stdin();
            if stdin.is_terminal() {
                if let Err(e) = repl(&mut shell) {
                    eprintln!("{RED}repl error:{RESET} {e}");
                }
            } else {
                // Piped / scripted: run each line, output to stdout, errors to stderr.
                run_script(&mut shell, stdin.lock().lines().map_while(Result::ok));
            }
        }
    }
}

/// Parse an engineering-notation value, exiting with a message on failure.
fn val(text: &str, unit: Unit) -> Option<Value> {
    match Value::parse(text, unit) {
        Ok(v) => Some(v),
        Err(e) => {
            eprintln!("bad value {text:?}: {e:?}");
            std::process::exit(1);
        }
    }
}

/// `ee bode` — AC Bode plot of an RC low-pass (fc ≈ 1 kHz).
fn demo_bode() {
    let mut c = Circuit::new("rc-lowpass");
    let vin = c.new_net();
    let out = c.new_net();
    c.add_part(
        "V1",
        PartKind::VoltageSource,
        val("1", Unit::Volt),
        vec![vin, ee_core::NetId::GROUND],
    );
    c.add_part(
        "R1",
        PartKind::Resistor,
        val("1k", Unit::Ohm),
        vec![vin, out],
    );
    c.add_part(
        "C1",
        PartKind::Capacitor,
        val("159n", Unit::Farad),
        vec![out, ee_core::NetId::GROUND],
    );
    // Mark the source as the AC stimulus (1∠0).
    c.set_waveform(
        "V1",
        Waveform::Sine {
            amplitude: 1.0,
            freq: 0.0,
            offset: 0.0,
            phase_deg: 0.0,
        },
    );

    println!("AC analysis of an RC low-pass (R=1k, C=159n → fc≈1 kHz):\n");
    let analysis = Analysis::Ac {
        f_start: 10.0,
        f_stop: 100_000.0,
        points_per_decade: 20,
    };
    let signals = match Engine::new().run(&c, &analysis) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{RED}simulation failed:{RESET} {e}");
            std::process::exit(1);
        }
    };
    let name = format!("v(n{})", c.resolve(out).0);
    let Some(sig) = signals.iter().find(|s| s.name == name) else {
        return;
    };
    let Some(z) = sig.complex() else { return };

    // Bode magnitude: dB vs log10(frequency).
    let xs: Vec<f64> = sig.x.iter().map(|f| f.log10()).collect();
    let db: Vec<f64> = z.iter().map(|c| c.magnitude_db()).collect();
    let plot = Plot::new("|v(out)/v(in)| dB  —  log f, 10 Hz … 100 kHz")
        .units("dB", "")
        .size(72, 16);
    println!("{}", plot.render(&xs, &db));

    // Report the measured −3 dB corner.
    if let Some(i) = db.iter().position(|&d| d <= -3.0) {
        println!(
            "\n  measured −3 dB near {:.0} Hz   (analytic fc = 1001 Hz)",
            sig.x[i]
        );
    }
}

/// `ee amp` — a common-emitter BJT amplifier's DC operating point, drawn with a
/// live color overlay of the node voltages.
fn demo_amp() {
    let mut c = Circuit::new("ce-amp");
    let vcc = c.new_net();
    let base = c.new_net();
    let coll = c.new_net();
    let emit = c.new_net();
    let gnd = ee_core::NetId::GROUND;
    c.add_part(
        "V1",
        PartKind::VoltageSource,
        val("12", Unit::Volt),
        vec![vcc, gnd],
    );
    c.add_part(
        "R1",
        PartKind::Resistor,
        val("47k", Unit::Ohm),
        vec![vcc, base],
    ); // bias top
    c.add_part(
        "R2",
        PartKind::Resistor,
        val("10k", Unit::Ohm),
        vec![base, gnd],
    ); // bias bottom
    c.add_part(
        "RC",
        PartKind::Resistor,
        val("2k2", Unit::Ohm),
        vec![vcc, coll],
    ); // collector load
    c.add_part(
        "RE",
        PartKind::Resistor,
        val("1k", Unit::Ohm),
        vec![emit, gnd],
    ); // emitter degeneration
       // Q1: terminals [collector, base, emitter], NPN with default model.
    c.add_part(
        "Q1",
        PartKind::Bjt(Polarity::N, BjtModel::default()),
        None,
        vec![coll, base, emit],
    );

    println!("Common-emitter amplifier — DC operating point:\n");
    let signals = match Engine::new().run(&c, &Analysis::OperatingPoint) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{RED}simulation failed:{RESET} {e}");
            std::process::exit(1);
        }
    };
    let volts = NodeVoltages::from_signals(&signals);
    println!(
        "{}",
        Overlay::new("ce-amp op-point")
            .color(true)
            .width(72)
            .render(&c, &volts)
    );

    let v = |net| volts.get(c.resolve(net)).unwrap_or(0.0);
    let (vb, vc, ve) = (v(base), v(coll), v(emit));
    let ic_ma = ve / 1000.0 * 1e3; // Ie ≈ Ic = Ve / RE (RE = 1 kΩ), in mA
    println!(
        "\n  Vb={vb:.2} V  Vc={vc:.2} V  Ve={ve:.2} V   →  Vbe={:.3} V, Ic≈{ic_ma:.2} mA (active region)",
        vb - ve,
    );
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

    println!("{DIM}ee-assist — eesh (M1). Tab completes, ↑/↓ history, Ctrl-R search, Ctrl-D quits.{RESET}");
    println!("{DIM}Try: add R1 in out 1k · probe out · run tran 5m · show · help  (or: ee demo|bode|amp){RESET}\n");

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
