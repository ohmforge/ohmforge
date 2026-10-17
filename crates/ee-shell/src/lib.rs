//! # ee-shell (`eesh`)
//!
//! The dedicated shell: a parser for the command language, tab-completion,
//! history, and (from M1) an embedded scripting engine for loops/parameters/
//! automation. The shell mutates the same `ee_core::Circuit` the TUI and AI do.
//! Full command spec in docs/SHELL.md.
//!
//! At M0 this is the pure command engine: a [`Shell`] that holds a circuit and
//! executes one command line at a time, returning the text to print. It is a
//! testable state machine — the crossterm readline loop is a thin wrapper added
//! later. The grammar (one command per line, whitespace-separated):
//!
//! ```text
//! add <ref> <nodeA> <nodeB> <value>   place a part (R/C/L/V from the ref letter)
//! wire <nodeA> <nodeB>                merge two named nodes into one net
//! probe <node>                        mark a node to plot on the next `run`
//! run [tran [<dur>]]                  transient (default 1 ms); plots the probes
//! run op                              DC operating-point node-voltage table
//! show                                list the placed parts
//! help                                show the command list
//! ```
//!
//! ```
//! # use ee_shell::Shell;
//! let mut sh = Shell::new();
//! sh.execute("add V1 in gnd 5").unwrap();
//! sh.execute("add R1 in out 1k").unwrap();
//! sh.execute("add C1 out gnd 1u").unwrap();
//! sh.execute("probe out").unwrap();
//! let frame = sh.execute("run tran 5m").unwrap();
//! assert!(frame.contains('\u{2524}')); // a braille scope frame
//! ```

use std::collections::HashMap;
use std::error::Error;
use std::fmt;

use ee_core::graph::{Circuit, NetId, PartKind};
use ee_core::units::Unit;
use ee_core::value::{ParseError, Value};
use ee_sim::{Analysis, Engine, SimError};
use ee_tui::Plot;

/// The default transient duration when `run`/`run tran` is given no `<dur>`.
const DEFAULT_DURATION: f64 = 1e-3;

/// The interactive-shell state machine.
///
/// Holds the circuit under construction, the node-name → net map, and the set of
/// nodes marked to plot. [`execute`](Shell::execute) runs one command line and
/// returns its text output (never prints), which keeps the whole engine unit
/// testable without a terminal.
#[derive(Debug, Clone)]
pub struct Shell {
    circuit: Circuit,
    /// Node name → net id. Ground aliases never land here (they resolve to
    /// [`NetId::GROUND`]), so this map holds exactly the user's named nodes.
    nets: HashMap<String, NetId>,
    /// Nodes to render on the next transient `run`, in the order probed.
    probes: Vec<String>,
}

impl Default for Shell {
    fn default() -> Self {
        Self::new()
    }
}

impl Shell {
    /// A fresh shell with an empty circuit.
    pub fn new() -> Self {
        Self {
            circuit: Circuit::new("shell"),
            nets: HashMap::new(),
            probes: Vec::new(),
        }
    }

    /// Borrow the circuit the shell is building (for the TUI/AI to render).
    pub fn circuit(&self) -> &Circuit {
        &self.circuit
    }

    /// The command keywords — for help text and shell tab-completion.
    pub const COMMANDS: &'static [&'static str] = &["add", "wire", "probe", "run", "show", "help"];

    /// The user's named nodes so far (for tab-completion). Ground aliases are
    /// not included — they resolve directly to the ground reference.
    pub fn node_names(&self) -> Vec<&str> {
        self.nets.keys().map(String::as_str).collect()
    }

    /// The placed part reference designators (for tab-completion).
    pub fn part_refs(&self) -> Vec<&str> {
        self.circuit
            .parts()
            .iter()
            .map(|p| p.reference.as_str())
            .collect()
    }

    /// Execute one line of input, returning the text to show the user.
    ///
    /// A blank line is a no-op (empty output). Anything the grammar rejects comes
    /// back as a [`ShellError`] with a friendly, specific message.
    pub fn execute(&mut self, line: &str) -> Result<String, ShellError> {
        let line = line.trim();
        if line.is_empty() {
            return Ok(String::new());
        }
        let mut words = line.split_whitespace();
        let command = words.next().expect("non-empty line has a first word");
        let args: Vec<&str> = words.collect();
        match command {
            "add" => self.cmd_add(&args),
            "wire" => self.cmd_wire(&args),
            "probe" => self.cmd_probe(&args),
            "run" => self.cmd_run(&args),
            "show" => Ok(self.render_show()),
            "help" => Ok(Self::help_text()),
            other => Err(ShellError::UnknownCommand(other.to_string())),
        }
    }

    // ── commands ──────────────────────────────────────────────────────────

    /// `add <ref> <nodeA> <nodeB> <value>` — place a part.
    fn cmd_add(&mut self, args: &[&str]) -> Result<String, ShellError> {
        let [reference, a, b, value] = args else {
            return Err(ShellError::Usage {
                command: "add",
                usage: "add <ref> <nodeA> <nodeB> <value>",
            });
        };
        let (kind, unit) = classify(reference)?;
        if self.circuit.part_by_ref(reference).is_some() {
            return Err(ShellError::DuplicateRef((*reference).to_string()));
        }
        let value = Value::parse(value, unit).map_err(|source| ShellError::BadValue {
            field: format!("value {value:?}"),
            source,
        })?;
        let na = self.net_for(a);
        let nb = self.net_for(b);
        self.circuit
            .add_part(*reference, kind, Some(value), vec![na, nb]);
        Ok(format!("added {reference}"))
    }

    /// `wire <nodeA> <nodeB>` — merge two already-named nodes into one net.
    fn cmd_wire(&mut self, args: &[&str]) -> Result<String, ShellError> {
        let [a, b] = args else {
            return Err(ShellError::Usage {
                command: "wire",
                usage: "wire <nodeA> <nodeB>",
            });
        };
        let na = self.net_known(a)?;
        let nb = self.net_known(b)?;
        self.circuit.wire(na, nb);
        Ok(format!("wired {a} — {b}"))
    }

    /// `probe <node>` — remember a node to plot on the next transient `run`.
    fn cmd_probe(&mut self, args: &[&str]) -> Result<String, ShellError> {
        let [node] = args else {
            return Err(ShellError::Usage {
                command: "probe",
                usage: "probe <node>",
            });
        };
        self.net_known(node)?; // reject a typo'd node up front
        let node = (*node).to_string();
        let msg = format!("probing {node}");
        if !self.probes.contains(&node) {
            self.probes.push(node);
        }
        Ok(msg)
    }

    /// `run [tran [<dur>]]` / `run op` — simulate and render.
    fn cmd_run(&self, args: &[&str]) -> Result<String, ShellError> {
        match args {
            [] | ["tran"] => self.run_transient(DEFAULT_DURATION),
            ["tran", dur] => {
                let d = Value::parse(dur, Unit::Second).map_err(|source| ShellError::BadValue {
                    field: format!("duration {dur:?}"),
                    source,
                })?;
                self.run_transient(d.magnitude)
            }
            ["op"] => self.run_op(),
            _ => Err(ShellError::Usage {
                command: "run",
                usage: "run [tran [<dur>]] | run op",
            }),
        }
    }

    // ── run: parse → sim → plot ───────────────────────────────────────────

    /// Simulate a transient and render each probed node as a braille scope.
    fn run_transient(&self, duration: f64) -> Result<String, ShellError> {
        if self.probes.is_empty() {
            return Ok("nothing probed — add `probe <node>` to see a waveform.".to_string());
        }
        let signals = Engine::new()
            .run(
                &self.circuit,
                &Analysis::Transient {
                    duration,
                    step: None, // engine default: duration / 1000
                },
            )
            .map_err(ShellError::Sim)?;

        let mut out = String::new();
        for (i, node) in self.probes.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            let name = self.signal_name(node);
            match signals.iter().find(|s| s.name == name) {
                Some(sig) => {
                    let y = sig.real().unwrap_or(&[]);
                    let title = format!("v({node})");
                    out.push_str(&Plot::new(&title).units("V", "s").render(&sig.x, y));
                }
                // The only named-but-signal-less node is ground (0 V, no row).
                None => out.push_str(&format!("  v({node}) is ground (0 V) — nothing to plot.\n")),
            }
        }
        Ok(out)
    }

    /// Solve the DC operating point and tabulate every named node's voltage.
    fn run_op(&self) -> Result<String, ShellError> {
        let signals = Engine::new()
            .run(&self.circuit, &Analysis::OperatingPoint)
            .map_err(ShellError::Sim)?;

        let mut names: Vec<&String> = self.nets.keys().collect();
        names.sort();
        let mut out = String::from("operating point:\n");
        for name in names {
            let raw = self.circuit.resolve(self.nets[name]).0;
            let volts = if raw == NetId::GROUND.0 {
                0.0
            } else {
                signals
                    .iter()
                    .find(|s| s.name == format!("v(n{raw})"))
                    .and_then(|s| s.real())
                    .and_then(|v| v.first().copied())
                    .unwrap_or(0.0)
            };
            out.push_str(&format!("  v({name}) = {volts:.6} V\n"));
        }
        Ok(out)
    }

    // ── rendering helpers ─────────────────────────────────────────────────

    /// `show` — list the placed parts: reference, kind, value, and its nodes.
    fn render_show(&self) -> String {
        let parts = self.circuit.parts();
        let mut out = format!(
            "{} part{}:\n",
            parts.len(),
            if parts.len() == 1 { "" } else { "s" }
        );
        for p in parts {
            let value = match &p.value {
                Some(v) => format!("{}{}", v.magnitude, v.unit.symbol()),
                None => "-".to_string(),
            };
            let nodes: Vec<String> = p.terminals.iter().map(|&t| self.node_label(t)).collect();
            out.push_str(&format!(
                "  {:<4} {:<10} {:>12}  {}\n",
                p.reference,
                kind_label(&p.kind),
                value,
                nodes.join(" — ")
            ));
        }
        out
    }

    fn help_text() -> String {
        [
            "commands:",
            "  add <ref> <a> <b> <value>   place a part (R/C/L/V from the ref's first letter)",
            "  wire <a> <b>                merge two named nodes into one net",
            "  probe <node>                mark a node to plot on the next run",
            "  run [tran [<dur>]]          transient (default 1 ms), plotting each probe",
            "  run op                      DC operating-point node-voltage table",
            "  show                        list the placed parts",
            "  help                        show this help",
        ]
        .join("\n")
    }

    // ── net bookkeeping ───────────────────────────────────────────────────

    /// The net for a node name, allocating a fresh one the first time it is seen.
    /// `gnd`/`ground`/`0` always resolve to [`NetId::GROUND`].
    fn net_for(&mut self, name: &str) -> NetId {
        if is_ground(name) {
            return NetId::GROUND;
        }
        if let Some(&id) = self.nets.get(name) {
            return id;
        }
        let id = self.circuit.new_net();
        self.nets.insert(name.to_string(), id);
        id
    }

    /// The net for an *existing* node name — a ground alias, or one already
    /// introduced by `add`. Rejects anything else so typos surface early.
    fn net_known(&self, name: &str) -> Result<NetId, ShellError> {
        if is_ground(name) {
            return Ok(NetId::GROUND);
        }
        self.nets
            .get(name)
            .copied()
            .ok_or_else(|| ShellError::UnknownNode(name.to_string()))
    }

    /// The engine signal name for a named node: `v(n{resolved raw net})`.
    fn signal_name(&self, name: &str) -> String {
        let net = self.net_known(name).unwrap_or(NetId::GROUND);
        format!("v(n{})", self.circuit.resolve(net).0)
    }

    /// A human label for a raw net id (its first-seen name, or `gnd`).
    fn node_label(&self, net: NetId) -> String {
        if net == NetId::GROUND {
            return "gnd".to_string();
        }
        self.nets
            .iter()
            .find(|(_, &id)| id == net)
            .map(|(n, _)| n.clone())
            .unwrap_or_else(|| format!("n{}", net.0))
    }
}

/// Map a reference's leading letter to a part kind and its value's unit —
/// `R`→Ω, `C`→F, `L`→H, `V`→V, `I`→A. (Mirrors `ee-fmt`'s private netlist classifier.)
fn classify(reference: &str) -> Result<(PartKind, Unit), ShellError> {
    let c = reference.chars().next().ok_or(ShellError::EmptyReference)?;
    Ok(match c.to_ascii_uppercase() {
        'R' => (PartKind::Resistor, Unit::Ohm),
        'C' => (PartKind::Capacitor, Unit::Farad),
        'L' => (PartKind::Inductor, Unit::Henry),
        'V' => (PartKind::VoltageSource, Unit::Volt),
        'I' => (PartKind::CurrentSource, Unit::Ampere),
        'D' => (PartKind::Diode, Unit::Ampere), // value (if given) = saturation current Is
        other => return Err(ShellError::UnknownPartKind(other)),
    })
}

/// `gnd`, `ground`, and `0` all name the ground reference.
fn is_ground(name: &str) -> bool {
    matches!(name.to_ascii_lowercase().as_str(), "gnd" | "ground" | "0")
}

/// A short human label for a part kind, for `show`.
fn kind_label(kind: &PartKind) -> &str {
    match kind {
        PartKind::Resistor => "resistor",
        PartKind::Capacitor => "capacitor",
        PartKind::Inductor => "inductor",
        PartKind::VoltageSource => "source",
        PartKind::CurrentSource => "current source",
        PartKind::Diode => "diode",
        PartKind::Bjt(..) => "BJT",
        PartKind::Mosfet(..) => "MOSFET",
        PartKind::Other(name) => name,
    }
}

/// Everything that can go wrong executing a shell command.
#[derive(Debug, Clone, PartialEq)]
pub enum ShellError {
    /// The first word was not a known command.
    UnknownCommand(String),
    /// A known command received the wrong shape of arguments.
    Usage {
        command: &'static str,
        usage: &'static str,
    },
    /// A reference's first letter is not a known part type.
    UnknownPartKind(char),
    /// The reference designator was empty.
    EmptyReference,
    /// A value (or duration) field could not be parsed. `field` quotes the
    /// offending token, e.g. `value "nope"`.
    BadValue { field: String, source: ParseError },
    /// Two parts would share a reference designator.
    DuplicateRef(String),
    /// A node named in `probe`/`wire` was never introduced by `add`.
    UnknownNode(String),
    /// The simulation engine rejected or could not solve the circuit.
    Sim(SimError),
}

impl fmt::Display for ShellError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShellError::UnknownCommand(c) => {
                write!(
                    f,
                    "unknown command {c:?} — type `help` for the command list"
                )
            }
            ShellError::Usage { command, usage } => {
                write!(f, "bad arguments for `{command}` — usage: {usage}")
            }
            ShellError::UnknownPartKind(c) => {
                write!(f, "unknown part type '{c}' (expected R, C, L, or V)")
            }
            ShellError::EmptyReference => write!(f, "empty part reference"),
            ShellError::BadValue { field, source } => {
                let why = match source {
                    ParseError::Empty => "is empty",
                    ParseError::NotANumber => "is not a valid number",
                };
                write!(f, "{field} {why}")
            }
            ShellError::DuplicateRef(r) => write!(f, "a part named {r:?} already exists"),
            ShellError::UnknownNode(n) => {
                write!(f, "unknown node {n:?} — add a part that uses it first")
            }
            ShellError::Sim(e) => write!(f, "simulation failed: {e}"),
        }
    }
}

impl Error for ShellError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ShellError::Sim(e) => Some(e),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an RC low-pass with `add`, probe the output, and run a transient —
    /// the output must be a real braille scope frame, and `show` must list 3 parts.
    #[test]
    fn rc_lowpass_transient_renders_a_scope() {
        let mut sh = Shell::new();
        sh.execute("add V1 in gnd 5").unwrap();
        sh.execute("add R1 in out 1k").unwrap();
        sh.execute("add C1 out gnd 1u").unwrap();
        sh.execute("probe out").unwrap();

        let frame = sh.execute("run tran 5m").unwrap();
        assert!(
            frame.contains('\u{2524}'),
            "each scope row carries the ┤ axis:\n{frame}"
        );
        assert!(
            frame
                .chars()
                .any(|c| ('\u{2801}'..='\u{28FF}').contains(&c)),
            "the curve drew at least one braille dot:\n{frame}"
        );

        let shown = sh.execute("show").unwrap();
        assert!(
            shown.starts_with("3 parts:"),
            "show lists 3 parts:\n{shown}"
        );
        for r in ["V1", "R1", "C1"] {
            assert!(shown.contains(r), "show names {r}:\n{shown}");
        }
    }

    /// `run` with nothing probed says so rather than simulating silently.
    #[test]
    fn run_without_probes_says_so() {
        let mut sh = Shell::new();
        sh.execute("add V1 in gnd 5").unwrap();
        sh.execute("add R1 in out 1k").unwrap();
        sh.execute("add C1 out gnd 1u").unwrap();
        let out = sh.execute("run").unwrap();
        assert!(out.contains("nothing probed"), "{out}");
    }

    /// Each error path returns its specific, friendly `ShellError`.
    #[test]
    fn friendly_errors_for_each_bad_input() {
        let mut sh = Shell::new();
        sh.execute("add R1 a b 1k").unwrap();

        // Duplicate reference.
        let dup = sh.execute("add R1 a c 2k").unwrap_err();
        assert!(matches!(dup, ShellError::DuplicateRef(_)));
        assert!(dup.to_string().contains("R1"), "{dup}");

        // Probe of a node that was never introduced.
        let probe = sh.execute("probe nowhere").unwrap_err();
        assert!(matches!(probe, ShellError::UnknownNode(_)));
        assert!(probe.to_string().contains("nowhere"), "{probe}");

        // Unknown command.
        let unknown = sh.execute("frobnicate x").unwrap_err();
        assert!(matches!(unknown, ShellError::UnknownCommand(_)));
        assert!(unknown.to_string().contains("frobnicate"), "{unknown}");

        // Unknown part type and bad value, too.
        assert!(matches!(
            sh.execute("add X9 a b 1k").unwrap_err(),
            ShellError::UnknownPartKind('X')
        ));
        assert!(matches!(
            sh.execute("add R2 a b nope").unwrap_err(),
            ShellError::BadValue { .. }
        ));
    }

    /// `run op` tabulates node voltages — a resistive divider reads 5 V midpoint.
    #[test]
    fn operating_point_table_has_divider_midpoint() {
        let mut sh = Shell::new();
        sh.execute("add V1 in gnd 10").unwrap();
        sh.execute("add R1 in out 1k").unwrap();
        sh.execute("add R2 out gnd 1k").unwrap();

        let table = sh.execute("run op").unwrap();
        assert!(table.contains("v(out)"), "names the output node:\n{table}");
        // 10 V across two equal 1 kΩ resistors → 5 V at the midpoint.
        assert!(table.contains("5.000000 V"), "v(out) ≈ 5 V:\n{table}");
        assert!(
            table.contains("v(in)") && table.contains("10.000000 V"),
            "{table}"
        );
    }

    /// `wire` merges existing nodes and rejects an unknown one.
    #[test]
    fn wire_merges_known_nodes_and_rejects_typos() {
        let mut sh = Shell::new();
        sh.execute("add R1 a b 1k").unwrap();
        assert!(sh.execute("wire a b").is_ok());
        assert!(matches!(
            sh.execute("wire a ghost").unwrap_err(),
            ShellError::UnknownNode(_)
        ));
    }

    /// A blank line is a no-op; `help` lists every command.
    #[test]
    fn blank_is_empty_and_help_lists_commands() {
        let mut sh = Shell::new();
        assert_eq!(sh.execute("   ").unwrap(), "");
        let help = sh.execute("help").unwrap();
        for c in ["add", "wire", "probe", "run", "show", "help"] {
            assert!(help.contains(c), "help names `{c}`:\n{help}");
        }
    }

    /// Durations parse in engineering notation (`5m` → 5 ms) and drive the sim.
    #[test]
    fn transient_duration_parses_in_engineering_notation() {
        let mut sh = Shell::new();
        sh.execute("add V1 in gnd 5").unwrap();
        sh.execute("add R1 in out 1k").unwrap();
        sh.execute("add C1 out gnd 1u").unwrap();
        sh.execute("probe out").unwrap();
        // Both the explicit `5m` and the default-duration path must render a frame.
        assert!(sh.execute("run tran 5m").unwrap().contains('\u{2524}'));
        assert!(sh.execute("run tran").unwrap().contains('\u{2524}'));
        // A nonsense duration is a friendly bad-value error, not a panic.
        assert!(matches!(
            sh.execute("run tran wat").unwrap_err(),
            ShellError::BadValue { .. }
        ));
    }
}
