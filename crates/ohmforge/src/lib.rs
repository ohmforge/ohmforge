//! `ohmforge` — the ohmforge workbench crate, shared by two binaries:
//! `of` (the primary CLI, with subcommands) and `eesh` (launches straight into
//! the shell REPL).
//!
//! The interactive shell: an `eesh` REPL driven by [`ohmforge_shell::Shell`] (line
//! editing, history, tab completion via `rustyline`) on a TTY, or scripted from
//! a pipe. Built-in showcases: `of demo` (RC transient scope), `of bode` (AC
//! Bode plot), `of amp` (transistor amplifier operating point with a live
//! voltage overlay). The full dockable TUI event loop lands as this grows
//! (docs/ROADMAP.md).

use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;

use ohmforge_core::graph::{DiodeModel, PartKind};
use ohmforge_core::units::Unit;
use ohmforge_core::value::Value;
use ohmforge_core::{BjtModel, Circuit, Polarity, Waveform};
use ohmforge_shell::Shell;
use ohmforge_sim::{Analysis, Engine};
use ohmforge_tui::{NodeVoltages, Overlay, Plot, Screen};
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
const PROMPT: &str = "\x1b[1;36meesh>\x1b[0m ";
const RED: &str = "\x1b[31m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";
const RESET: &str = "\x1b[0m";

/// The ASCII splash banner (shared with the README and the TUI), shown atop the
/// interactive REPL.
const BANNER: &str = include_str!("../../../assets/ohmforge-ascii.txt");

/// `of` entry point: parse a subcommand and dispatch. With no subcommand (or an
/// unknown one) it drops into the eesh shell, exactly like the `eesh` binary.
pub fn run_of() {
    match std::env::args().nth(1).as_deref() {
        Some("demo") => run_lines(&mut Shell::new(), DEMO.lines()),
        Some("bode") => demo_bode(),
        Some("amp") => demo_amp(),
        Some("cosim") => demo_cosim(),
        Some("view") => demo_view(),
        Some("tui") => {
            if let Err(e) = ohmforge_tui::run(Shell::new()) {
                eprintln!("{RED}tui error:{RESET} {e}");
            }
        }
        Some("review") | Some("drc") => run_review(std::env::args().nth(2)),
        Some("ask") => run_ask(std::env::args().skip(2).collect::<Vec<_>>()),
        Some("help") | Some("--help") | Some("-h") => print_usage(),
        _ => run_shell(),
    }
}

/// `eesh` entry point: launch straight into the shell, with no subcommand
/// parsing. An interactive REPL on a TTY, or the piped `.eesh` script engine.
pub fn run_eesh() {
    run_shell();
}

/// The eesh shell: an interactive REPL on a TTY, otherwise the whole of stdin
/// run through the script engine (so comments, `repeat` loops, and `$vars`
/// work — stdin may be an `.eesh` script).
fn run_shell() {
    let mut shell = Shell::new();
    let stdin = io::stdin();
    if stdin.is_terminal() {
        if let Err(e) = repl(&mut shell) {
            eprintln!("{RED}repl error:{RESET} {e}");
        }
    } else {
        let mut src = String::new();
        if stdin.lock().read_to_string(&mut src).is_ok() {
            match shell.run_script(&src) {
                Ok(run) => {
                    let t = run.transcript;
                    if !t.trim_end().is_empty() {
                        println!("{}", t.trim_end_matches('\n'));
                    }
                }
                Err(e) => eprintln!("error: {e}"),
            }
        }
    }
}

/// `of review [file]` / `of drc [file]` — the **offline, no-key** copilot. Builds
/// a circuit from an `.eesh` script (a file path, else stdin) and prints the
/// grounded design-rule review (DRC findings + sanity checks). Needs no API key
/// and no network — this is the AI surface a user can always reach.
fn run_review(path: Option<String>) {
    let src = match &path {
        Some(p) => match std::fs::read_to_string(p) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{RED}cannot read {p}:{RESET} {e}");
                std::process::exit(1);
            }
        },
        None => {
            let mut s = String::new();
            if io::stdin().read_to_string(&mut s).is_err() {
                eprintln!("{RED}failed to read the script from stdin{RESET}");
                std::process::exit(1);
            }
            s
        }
    };
    let mut shell = Shell::new();
    if let Err(e) = shell.run_script(&src) {
        eprintln!("{RED}script error:{RESET} {e}");
        std::process::exit(1);
    }
    println!("{}", ohmforge_ai::offline_review(shell.circuit()));
}

/// `of ask <question>` — the **live** copilot: drives a real shell+engine through
/// the tool loop and answers. Needs the `http` feature (network transport) and
/// `ANTHROPIC_API_KEY`; without the feature it points the user at the offline
/// `review`/`drc` commands.
#[cfg(feature = "http")]
fn run_ask(args: Vec<String>) {
    if args.is_empty() {
        eprintln!("{RED}usage:{RESET} of ask <question>");
        std::process::exit(1);
    }
    let transport = match ohmforge_ai::transport::HttpTransport::from_env() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{RED}{e}{RESET}");
            std::process::exit(1);
        }
    };
    let client = ohmforge_ai::Client::new(transport);
    match ohmforge_ai::ask(&client, args.join(" ")) {
        Ok(answer) => println!("{answer}"),
        Err(e) => {
            eprintln!("{RED}ask failed:{RESET} {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(not(feature = "http"))]
fn run_ask(_args: Vec<String>) {
    eprintln!(
        "{DIM}`of ask` (live copilot) needs the network transport — rebuild with \
         `cargo run -p ohmforge --features http` and set ANTHROPIC_API_KEY.{RESET}"
    );
    eprintln!(
        "{DIM}No key needed for the offline copilot: `of review <file.eesh>` or \
         `of drc <file.eesh>` (DRC + sanity checks).{RESET}"
    );
    std::process::exit(1);
}

/// Print the subcommand overview (also the `_`-arm fallback for an unknown verb
/// would land in the REPL, so this is only shown on explicit `help`).
fn print_usage() {
    println!(
        "of — the ohmforge EE workbench\n\
         \n\
         USAGE:\n\
         \x20 of                      interactive eesh REPL (or pipe a script via stdin)\n\
         \x20 of tui                  the live dockable TUI\n\
         \x20 of demo | bode | amp | cosim | view    built-in showcases\n\
         \x20 of review [file.eesh]   offline copilot: design-rule review (no key)\n\
         \x20 of drc    [file.eesh]   alias for review\n\
         \x20 of ask <question>       live copilot (needs --features http + ANTHROPIC_API_KEY)\n\
         \x20 of help                 this message\n"
    );
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

/// `of bode` — AC Bode plot of an RC low-pass (fc ≈ 1 kHz).
fn demo_bode() {
    let mut c = Circuit::new("rc-lowpass");
    let vin = c.new_net();
    let out = c.new_net();
    c.add_part(
        "V1",
        PartKind::VoltageSource,
        val("1", Unit::Volt),
        vec![vin, ohmforge_core::NetId::GROUND],
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
        vec![out, ohmforge_core::NetId::GROUND],
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

/// `of amp` — a common-emitter BJT amplifier's DC operating point, drawn with a
/// live color overlay of the node voltages.
fn demo_amp() {
    let mut c = Circuit::new("ce-amp");
    let vcc = c.new_net();
    let base = c.new_net();
    let coll = c.new_net();
    let emit = c.new_net();
    let gnd = ohmforge_core::NetId::GROUND;
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

/// `of cosim` — co-simulation: real AVR firmware drives pin D13, and the
/// lockstep bridge lights a simulated LED in the analog circuit.
fn demo_cosim() {
    use ohmforge_mcu::avr::Uno;

    // Tiny firmware: make D13 (PB5) an output and drive it high, then spin.
    //   SBI DDRB,5  (0x9A25) · SBI PORTB,5 (0x9A2D) · RJMP -1 (0xCFFF)
    let firmware: [u8; 6] = [0x25, 0x9A, 0x2D, 0x9A, 0xFF, 0xCF];
    let mut uno = Uno::new();
    if uno.load_firmware(&firmware).is_err() {
        eprintln!("{RED}failed to load firmware{RESET}");
        std::process::exit(1);
    }
    let d13 = Uno::digital_pin_id(13).expect("D13");

    // Circuit on the D13 pin: a 220 Ω resistor into an LED (diode) to ground.
    // `VPIN` is the boundary element — the pin's drive, pushed in by the loop.
    let mut c = Circuit::new("cosim-led");
    let pin = c.new_net();
    let anode = c.new_net();
    let gnd = ohmforge_core::NetId::GROUND;
    c.add_part(
        "VPIN",
        PartKind::VoltageSource,
        val("0", Unit::Volt),
        vec![pin, gnd],
    );
    c.add_part(
        "R1",
        PartKind::Resistor,
        val("220", Unit::Ohm),
        vec![pin, anode],
    );
    c.add_part(
        "D1",
        PartKind::Diode(DiodeModel::default()),
        None,
        vec![anode, gnd],
    );

    println!("Co-simulation: AVR firmware drives D13 → a simulated LED.\n");
    let engine = Engine::new();
    let mut io = uno.pin_io();
    for step in 1..=6 {
        uno.step_with_io(&mut io); // sample circuit → run one instruction → publish pin drives
        let high = io.drive(d13).level() == Some(true);
        c.set_value("VPIN", Value::new(if high { 5.0 } else { 0.0 }, Unit::Volt));
        let signals = match engine.run(&c, &Analysis::OperatingPoint) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{RED}simulation failed:{RESET} {e}");
                std::process::exit(1);
            }
        };
        let v = NodeVoltages::from_signals(&signals);
        let (vpin, va) = (
            v.get(c.resolve(pin)).unwrap_or(0.0),
            v.get(c.resolve(anode)).unwrap_or(0.0),
        );
        let i_ma = (vpin - va) / 220.0 * 1e3;
        let state = if high {
            "HIGH → LED lit"
        } else {
            "low  → LED off"
        };
        println!("  step {step}: D13 {state}   v(d13)={vpin:.2} V, I(LED)={i_ma:.1} mA");
        if high {
            println!(
                "\n  digitalWrite(13, HIGH) in firmware → {i_ma:.1} mA through the simulated LED."
            );
            break;
        }
    }
}

/// `of view` — a static snapshot of the composed TUI: the op-point overlay, the
/// scope, and a shell pane, laid out like the live `of tui` (which needs a real
/// terminal). Uses an RC low-pass.
fn demo_view() {
    let mut c = Circuit::new("rc-lowpass");
    let vin = c.new_net();
    let out = c.new_net();
    let gnd = ohmforge_core::NetId::GROUND;
    c.add_part(
        "V1",
        PartKind::VoltageSource,
        val("5", Unit::Volt),
        vec![vin, gnd],
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
        val("1u", Unit::Farad),
        vec![out, gnd],
    );

    let engine = Engine::new();
    // Scope: v(out) transient.
    let tran = engine.run(
        &c,
        &Analysis::Transient {
            duration: 5e-3,
            step: Some(20e-6),
        },
    );
    let scope = match &tran {
        Ok(sigs) => {
            let name = format!("v(n{})", c.resolve(out).0);
            match sigs
                .iter()
                .find(|s| s.name == name)
                .and_then(|s| s.real().map(|y| (s, y)))
            {
                Some((s, y)) => {
                    let x_ms: Vec<f64> = s.x.iter().map(|t| t * 1e3).collect();
                    Plot::new("v(out)")
                        .units("V", "ms")
                        .size(44, 12)
                        .render(&x_ms, y)
                }
                None => String::new(),
            }
        }
        Err(_) => String::new(),
    };
    // Overlay: DC operating point.
    let volts = match engine.run(&c, &Analysis::OperatingPoint) {
        Ok(op) => NodeVoltages::from_signals(&op),
        Err(_) => NodeVoltages::new(),
    };
    let shell = [
        "eesh> add C1 out gnd 1u",
        "added C1",
        "eesh> probe out",
        "eesh> run tran 5m",
    ];
    println!("Composed view (the live, interactive version is `of tui`):\n");
    println!(
        "{}",
        Screen::new()
            .size(96, 22)
            .render_overlay(&c, &volts, &scope, &shell, true)
    );
}

/// Run a sequence of command lines, printing each command's output.
fn run_lines<I, S>(shell: &mut Shell, lines: I)
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

    println!("{CYAN}{}{RESET}", BANNER.trim_end());
    println!(
        "{DIM}ohmforge — eesh. Tab completes, ↑/↓ history, Ctrl-R search, Ctrl-D quits.{RESET}"
    );
    println!("{DIM}Try: add R1 in out 1k · probe out · run tran 5m · show · help  (or: of demo|bode|amp|cosim|view|tui){RESET}\n");

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

/// The analysis keywords completed after `run` (mirrors the shell's `run` verbs).
const RUN_ANALYSES: &[&str] = &["tran", "op", "ac", "dc", "noise", "mc", "tf"];

/// Tab completion: commands in the first position; after that, node names, part
/// references, checkpoint names, or `run`'s analysis keywords, as the context
/// warrants (`CP-UX-M8.2`).
struct EeHelper {
    commands: Vec<String>,
    nodes: Vec<String>,
    parts: Vec<String>,
    checkpoints: Vec<String>,
}

impl EeHelper {
    fn new() -> Self {
        Self {
            commands: Shell::COMMANDS.iter().map(|s| (*s).to_string()).collect(),
            nodes: Vec::new(),
            parts: Vec::new(),
            checkpoints: Vec::new(),
        }
    }

    /// Pull the latest nodes, part references, and checkpoint names from the shell
    /// so completion stays current after every command.
    fn refresh(&mut self, shell: &Shell) {
        let sorted = |mut v: Vec<String>| {
            v.sort();
            v
        };
        self.nodes = sorted(shell.node_names().iter().map(|s| s.to_string()).collect());
        self.parts = sorted(shell.part_refs().iter().map(|s| s.to_string()).collect());
        self.checkpoints = sorted(
            shell
                .checkpoint_names()
                .iter()
                .map(|s| s.to_string())
                .collect(),
        );
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
        // Which token are we on, and what was the command (first token)?
        let before: Vec<&str> = line[..start].split_whitespace().collect();
        let token_index = before.len();
        let command = before.first().copied().unwrap_or("");

        // Build the candidate pool from the context.
        let pool: Vec<&str> = if token_index == 0 {
            self.commands.iter().map(String::as_str).collect()
        } else if command == "run" && token_index == 1 {
            RUN_ANALYSES.to_vec()
        } else if command == "restore" {
            self.checkpoints.iter().map(String::as_str).collect()
        } else {
            // add/wire/probe/remove/set operate on nodes and part references.
            self.nodes
                .iter()
                .chain(&self.parts)
                .map(String::as_str)
                .collect()
        };
        let candidates = pool
            .iter()
            .filter(|c| c.starts_with(word))
            .map(|c| Pair {
                display: (*c).to_string(),
                replacement: (*c).to_string(),
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
