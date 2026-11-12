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

pub mod script;
pub mod session;

pub use script::{Recording, ScriptError, ScriptRun};
pub use session::SessionError;

use std::collections::HashMap;
use std::error::Error;
use std::fmt;

use ee_core::graph::{Circuit, DiodeModel, NetId, PartKind, Waveform};
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
    /// Session-state snapshots taken before each mutating command, newest last;
    /// `undo` pops the top to revert the last command (`CP-UX-M2.4`).
    undo_stack: Vec<Snapshot>,
    /// Named checkpoints the user can `restore` to at any time.
    checkpoints: HashMap<String, Snapshot>,
    /// The signals from the most recent transient `run`, cached so the live TUI can
    /// scrub them as a playback (`CP-UX-M7.1`). Not part of a [`Snapshot`] — it is a
    /// derived result, like the op-point, so `undo`/`restore` leave it untouched.
    last_transient: Vec<ee_core::signal::Signal>,
}

/// A restorable capture of the shell's mutable session state.
///
/// Holds everything a command can change — the circuit graph, the node-name map,
/// and the probe list — but **not** the undo stack or checkpoint map themselves,
/// so a snapshot is a shallow, non-recursive clone of the session.
#[derive(Debug, Clone)]
struct Snapshot {
    circuit: Circuit,
    nets: HashMap<String, NetId>,
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
            undo_stack: Vec::new(),
            checkpoints: HashMap::new(),
            last_transient: Vec::new(),
        }
    }

    /// Borrow the circuit the shell is building (for the TUI/AI to render).
    pub fn circuit(&self) -> &Circuit {
        &self.circuit
    }

    /// The command keywords — for help text and shell tab-completion.
    pub const COMMANDS: &'static [&'static str] = &[
        "add",
        "wire",
        "probe",
        "remove",
        "set",
        "run",
        "show",
        "undo",
        "checkpoint",
        "restore",
        "save",
        "load",
        "help",
    ];

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
            // Mutating commands snapshot the session first (on success only) so
            // `undo` can revert exactly the last command.
            "add" | "wire" | "probe" | "remove" | "set" => {
                let before = self.capture();
                let result = match command {
                    "add" => self.cmd_add(&args),
                    "wire" => self.cmd_wire(&args),
                    "remove" => self.cmd_remove(&args),
                    "set" => self.cmd_set(&args),
                    _ => self.cmd_probe(&args),
                };
                if result.is_ok() {
                    self.undo_stack.push(before);
                }
                result
            }
            "run" => self.cmd_run(&args),
            "show" => Ok(self.render_show()),
            "undo" => self.cmd_undo(&args),
            "checkpoint" => self.cmd_checkpoint(&args),
            "restore" => self.cmd_restore(&args),
            "save" => self.cmd_save(&args),
            "load" => self.cmd_load(&args),
            "help" => Ok(Self::help_text()),
            other => Err(ShellError::UnknownCommand(other.to_string())),
        }
    }

    // ── snapshots / undo / named checkpoints (CP-UX-M2.4) ──────────────────

    /// Capture the current restorable session state.
    fn capture(&self) -> Snapshot {
        Snapshot {
            circuit: self.circuit.clone(),
            nets: self.nets.clone(),
            probes: self.probes.clone(),
        }
    }

    /// Replace the session state from a snapshot (the undo/restore primitive).
    fn restore_from(&mut self, snap: Snapshot) {
        self.circuit = snap.circuit;
        self.nets = snap.nets;
        self.probes = snap.probes;
    }

    /// Push a manual snapshot of the session onto the undo stack. Mutating
    /// commands do this automatically; call it to mark an extra undo point.
    pub fn snapshot(&mut self) {
        let snap = self.capture();
        self.undo_stack.push(snap);
    }

    /// Revert the most recent snapshot (the last mutating command). Returns a
    /// [`ShellError::NothingToUndo`] when the undo stack is empty.
    pub fn undo(&mut self) -> Result<String, ShellError> {
        match self.undo_stack.pop() {
            Some(snap) => {
                self.restore_from(snap);
                let n = self.circuit.parts().len();
                Ok(format!(
                    "undone — {n} part{} remain",
                    if n == 1 { "" } else { "s" }
                ))
            }
            None => Err(ShellError::NothingToUndo),
        }
    }

    /// Save the current session under `name`, overwriting any existing one.
    pub fn checkpoint(&mut self, name: impl Into<String>) -> String {
        let name = name.into();
        let snap = self.capture();
        self.checkpoints.insert(name.clone(), snap);
        format!("checkpoint {name:?} saved")
    }

    /// Restore the session to a named checkpoint. Does **not** disturb the undo
    /// stack's prior entries. Unknown names yield [`ShellError::UnknownCheckpoint`].
    pub fn restore(&mut self, name: &str) -> Result<String, ShellError> {
        match self.checkpoints.get(name).cloned() {
            Some(snap) => {
                self.restore_from(snap);
                Ok(format!("restored checkpoint {name:?}"))
            }
            None => Err(ShellError::UnknownCheckpoint(name.to_string())),
        }
    }

    /// The names of saved checkpoints (for tab-completion), unordered.
    pub fn checkpoint_names(&self) -> Vec<&str> {
        self.checkpoints.keys().map(String::as_str).collect()
    }

    /// `undo` — revert the last mutating command.
    fn cmd_undo(&mut self, args: &[&str]) -> Result<String, ShellError> {
        if !args.is_empty() {
            return Err(ShellError::Usage {
                command: "undo",
                usage: "undo",
            });
        }
        self.undo()
    }

    /// `checkpoint <name>` — save the session under a name.
    fn cmd_checkpoint(&mut self, args: &[&str]) -> Result<String, ShellError> {
        let [name] = args else {
            return Err(ShellError::Usage {
                command: "checkpoint",
                usage: "checkpoint <name>",
            });
        };
        Ok(self.checkpoint(*name))
    }

    /// `restore <name>` — return the session to a named checkpoint.
    fn cmd_restore(&mut self, args: &[&str]) -> Result<String, ShellError> {
        let [name] = args else {
            return Err(ShellError::Usage {
                command: "restore",
                usage: "restore <name>",
            });
        };
        self.restore(name)
    }

    // ── session export / import (CP-UX-M4.1) ───────────────────────────────

    /// Serialize the whole session — circuit, node names, probes, and every named
    /// checkpoint — to the portable text form parsed by [`Self::import_session`].
    ///
    /// The undo stack is deliberately *not* exported; it is session-local history,
    /// not part of the saved document. Checkpoints are written in sorted order so
    /// the output is deterministic.
    pub fn export_session(&self) -> String {
        let mut out = String::new();
        session::write_header(&mut out);
        session::write_body(&mut out, &self.circuit, &self.nets, &self.probes);
        let mut names: Vec<&String> = self.checkpoints.keys().collect();
        names.sort();
        for name in names {
            let snap = &self.checkpoints[name];
            session::write_checkpoint_header(&mut out, name);
            session::write_body(&mut out, &snap.circuit, &snap.nets, &snap.probes);
        }
        out
    }

    /// Replace this shell's session with one decoded from [`Self::export_session`] text.
    ///
    /// On success the circuit, node names, probes, and checkpoints all come from
    /// the text and the undo stack is cleared (the imported session is a fresh
    /// starting point). On a parse error the shell is left untouched.
    pub fn import_session(&mut self, text: &str) -> Result<(), SessionError> {
        let parsed = session::parse(text)?;
        self.circuit = parsed.main.circuit;
        self.nets = parsed.main.nets;
        self.probes = parsed.main.probes;
        self.undo_stack.clear();
        self.checkpoints = parsed
            .checkpoints
            .into_iter()
            .map(|(name, body)| {
                (
                    name,
                    Snapshot {
                        circuit: body.circuit,
                        nets: body.nets,
                        probes: body.probes,
                    },
                )
            })
            .collect();
        Ok(())
    }

    /// `save <path>` — write the exported session to a file.
    fn cmd_save(&self, args: &[&str]) -> Result<String, ShellError> {
        let [path] = args else {
            return Err(ShellError::Usage {
                command: "save",
                usage: "save <path>",
            });
        };
        std::fs::write(path, self.export_session()).map_err(|e| ShellError::Io {
            path: (*path).to_string(),
            detail: e.to_string(),
        })?;
        Ok(format!("saved session to {path}"))
    }

    /// `load <path>` — read a session file and replace the current session.
    fn cmd_load(&mut self, args: &[&str]) -> Result<String, ShellError> {
        let [path] = args else {
            return Err(ShellError::Usage {
                command: "load",
                usage: "load <path>",
            });
        };
        let text = std::fs::read_to_string(path).map_err(|e| ShellError::Io {
            path: (*path).to_string(),
            detail: e.to_string(),
        })?;
        self.import_session(&text).map_err(ShellError::Session)?;
        let n = self.circuit.parts().len();
        Ok(format!(
            "loaded session from {path} — {n} part{}",
            if n == 1 { "" } else { "s" }
        ))
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

    /// `set <ref>.value <value>` — retarget a placed component's primary value.
    ///
    /// This is the shared mutation path the live TUI's component tuning lowers to
    /// (`CP-UX-M6.1`): a `+`/`-` nudge in the editor and a typed `set` both end up
    /// here, so re-solving against the changed value is identical either way. A
    /// bare `<ref>` target means its `.value`; any other dotted param is rejected
    /// (only the primary value is settable today). The value's unit follows the
    /// part's existing value, falling back to the kind implied by the reference.
    fn cmd_set(&mut self, args: &[&str]) -> Result<String, ShellError> {
        let [target, value] = args else {
            return Err(ShellError::Usage {
                command: "set",
                usage: "set <ref>.value <value>",
            });
        };
        let (reference, param) = match target.split_once('.') {
            Some((r, p)) => (r, p),
            None => (*target, "value"),
        };
        if !param.eq_ignore_ascii_case("value") {
            return Err(ShellError::Usage {
                command: "set",
                usage: "set <ref>.value <value>",
            });
        }
        let unit = match self.circuit.part_by_ref(reference) {
            Some(p) => p
                .value
                .map(|v| v.unit)
                .or_else(|| classify(reference).ok().map(|(_, u)| u))
                .unwrap_or(Unit::None),
            None => return Err(ShellError::UnknownPart(reference.to_string())),
        };
        let parsed = Value::parse(value, unit).map_err(|source| ShellError::BadValue {
            field: format!("value {value:?}"),
            source,
        })?;
        self.circuit.set_value(reference, parsed);
        Ok(format!("set {reference}.value = {value}"))
    }

    /// `remove <ref>` — delete a placed part by reference designator.
    fn cmd_remove(&mut self, args: &[&str]) -> Result<String, ShellError> {
        let [reference] = args else {
            return Err(ShellError::Usage {
                command: "remove",
                usage: "remove <ref>",
            });
        };
        self.remove(reference)
    }

    /// Delete a part from the circuit by reference designator, leaving the node
    /// names and probes in place. Unknown references yield
    /// [`ShellError::UnknownPart`].
    ///
    /// `ee_core::Circuit` has no in-place part removal, so this rebuilds an
    /// equivalent circuit without the target part — the same nets (ids and
    /// union-find topology preserved via [`resolve`](Circuit::resolve)), the same
    /// remaining parts with their values, waveforms, tolerances, and tempcos, and
    /// the same operating temperature. A native `Circuit::remove_part` in `ee-core`
    /// would be cleaner; see the cross-crate note in the M5 report.
    pub fn remove(&mut self, reference: &str) -> Result<String, ShellError> {
        if self.circuit.part_by_ref(reference).is_none() {
            return Err(ShellError::UnknownPart(reference.to_string()));
        }
        self.circuit = self.rebuild_circuit_without(reference);
        let n = self.circuit.parts().len();
        Ok(format!(
            "removed {reference} — {n} part{} remain",
            if n == 1 { "" } else { "s" }
        ))
    }

    /// Build a fresh circuit identical to the current one minus `reference`.
    fn rebuild_circuit_without(&self, reference: &str) -> Circuit {
        let old = &self.circuit;
        // The highest net id referenced anywhere — terminals or the name map — so
        // the rebuilt circuit allocates net ids 1:1 with the original (both are
        // dense from 0, so equal counts give equal ids).
        let mut max_id = 0u32;
        for p in old.parts() {
            for t in &p.terminals {
                max_id = max_id.max(t.0);
            }
        }
        for id in self.nets.values() {
            max_id = max_id.max(id.0);
        }

        let mut c = Circuit::new(old.name.clone());
        for _ in 1..=max_id {
            c.new_net();
        }
        // Reproduce the union-find groups: wire each net to its old representative.
        for id in 1..=max_id {
            let root = old.resolve(NetId(id));
            if root.0 != id {
                c.wire(NetId(id), root);
            }
        }
        // Re-add every part except the removed one, carrying all its metadata.
        for p in old.parts() {
            if p.reference == reference {
                continue;
            }
            c.add_part(
                p.reference.clone(),
                p.kind.clone(),
                p.value,
                p.terminals.clone(),
            );
            if p.waveform != Waveform::Dc {
                c.set_waveform(&p.reference, p.waveform.clone());
            }
            if let Some(tol) = old.tolerance(&p.reference) {
                c.set_tolerance(&p.reference, tol);
            }
            if let Some(tc) = old.tempco(&p.reference) {
                c.set_tempco(&p.reference, tc);
            }
        }
        c.set_temperature(old.temperature());
        c
    }

    /// `run [tran [<dur>]]` / `run op` — simulate and render.
    fn cmd_run(&mut self, args: &[&str]) -> Result<String, ShellError> {
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

    /// Simulate a transient and render each probed node as a braille scope. The
    /// solved signals are also cached in `last_transient` so the live TUI can scrub
    /// them as a playback (`CP-UX-M7.1`).
    fn run_transient(&mut self, duration: f64) -> Result<String, ShellError> {
        if self.probes.is_empty() {
            self.last_transient.clear();
            return Ok("nothing probed — add `probe <node>` to see a waveform.".to_string());
        }
        let signals = Engine::new()
            .run(
                &self.circuit,
                &Analysis::Transient {
                    duration,
                    step: None, // None = engine-chosen adaptive timestep (LTE control)
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
        // Cache the probed-node series for the scrubbable playback, keeping only the
        // signals the user probed (the overlay annotates them by `v(n<net>)` name).
        let cached: Vec<ee_core::signal::Signal> = self
            .probes
            .iter()
            .map(|node| self.signal_name(node))
            .filter_map(|name| signals.iter().find(|s| s.name == name).cloned())
            .collect();
        self.last_transient = cached;
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
            "  remove <ref>                delete a placed part",
            "  set <ref>.value <value>     retarget a component's value (live-tunable)",
            "  run [tran [<dur>]]          transient (default 1 ms), plotting each probe",
            "  run op                      DC operating-point node-voltage table",
            "  show                        list the placed parts",
            "  undo                        revert the last add/wire/probe",
            "  checkpoint <name>           save the session under a name",
            "  restore <name>              restore the session to a checkpoint",
            "  save <path>                 write the whole session to a file",
            "  load <path>                 replace the session from a file",
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

/// Drive the live TUI with the shell as its command backend.
///
/// This is the one seam between `ee-shell` and `ee-tui`'s interactive app
/// (`CP-UX-M1.4`): the TUI calls [`execute`](Shell::execute) for each line the
/// user runs and reads [`circuit`](Shell::circuit) to draw the schematic, while
/// the dependency arrow stays `ee-shell → ee-tui`. The binary wires the live app
/// with `ee_tui::run(Shell::new())`.
impl ee_tui::Backend for Shell {
    fn execute(&mut self, line: &str) -> Result<String, String> {
        Shell::execute(self, line).map_err(|e| e.to_string())
    }

    fn circuit(&self) -> &Circuit {
        Shell::circuit(self)
    }

    /// Solve the DC operating point so the live overlay reflects the current
    /// circuit — the data behind the voltage colouring and the current-flow
    /// animation (`CP-UX-M6.1`). An unsolvable circuit (e.g. mid-edit) yields an
    /// empty solve rather than an error, so the overlay degrades to `?` instead of
    /// tearing down the app.
    fn op_point(&self) -> ee_tui::NodeVoltages {
        match Engine::new().run(&self.circuit, &Analysis::OperatingPoint) {
            Ok(signals) => ee_tui::NodeVoltages::from_signals(&signals),
            Err(_) => ee_tui::NodeVoltages::new(),
        }
    }

    /// Hand the live TUI the signals from the most recent transient `run` so it can
    /// scrub them as a playback (`CP-UX-M7.1`). Empty until a transient has run (or
    /// after one that probed nothing); the cache is refreshed by `run_transient`.
    fn transient(&self) -> Vec<ee_core::signal::Signal> {
        self.last_transient.clone()
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
        'D' => (PartKind::Diode(DiodeModel::default()), Unit::Ampere), // Is from the diode model
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
        PartKind::Diode(_) => "diode",
        PartKind::Bjt(..) => "BJT",
        PartKind::Mosfet(..) => "MOSFET",
        PartKind::Vcvs(_) => "VCVS",
        PartKind::Vccs(_) => "VCCS",
        PartKind::Ccvs(..) => "CCVS",
        PartKind::Cccs(..) => "CCCS",
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
    /// `remove` named a part reference that is not in the circuit.
    UnknownPart(String),
    /// `undo` was called with an empty undo stack — a friendly no-op.
    NothingToUndo,
    /// `restore` named a checkpoint that was never saved.
    UnknownCheckpoint(String),
    /// A `save`/`load` file could not be written or read.
    Io {
        /// The path that failed.
        path: String,
        /// The underlying OS error.
        detail: String,
    },
    /// A `load` file was not a valid session.
    Session(SessionError),
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
            ShellError::UnknownPart(r) => {
                write!(f, "no part named {r:?} — nothing to remove")
            }
            ShellError::NothingToUndo => {
                write!(f, "nothing to undo — no commands have been run yet")
            }
            ShellError::UnknownCheckpoint(n) => {
                write!(f, "no checkpoint named {n:?} — `checkpoint {n}` saves one")
            }
            ShellError::Io { path, detail } => {
                write!(f, "could not access {path:?}: {detail}")
            }
            ShellError::Session(e) => write!(f, "{e}"),
            ShellError::Sim(e) => write!(f, "simulation failed: {e}"),
        }
    }
}

impl Error for ShellError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ShellError::Sim(e) => Some(e),
            ShellError::Session(e) => Some(e),
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

    /// `remove` deletes a part by reference, preserving the rest of the circuit
    /// (topology and values), and is undoable like any other mutating command.
    #[test]
    fn remove_drops_a_part_preserving_topology_and_is_undoable() {
        let mut sh = Shell::new();
        sh.execute("add V1 in gnd 10").unwrap();
        sh.execute("add R1 in out 1k").unwrap();
        sh.execute("add R2 out gnd 1k").unwrap();
        assert_eq!(sh.circuit().parts().len(), 3);

        let msg = sh.execute("remove R1").unwrap();
        assert!(msg.contains("removed R1"), "{msg}");
        assert!(sh.circuit().part_by_ref("R1").is_none(), "R1 gone");
        assert!(sh.circuit().part_by_ref("V1").is_some(), "V1 kept");
        assert!(sh.circuit().part_by_ref("R2").is_some(), "R2 kept");

        // The surviving parts keep their values and node topology: the op-point
        // still resolves (V1 drives `in`, R2 bridges `out`→gnd).
        let table = sh.execute("run op").unwrap();
        assert!(
            table.contains("v(in)") && table.contains("10.000000 V"),
            "{table}"
        );

        // Undo brings R1 back exactly.
        sh.execute("undo").unwrap();
        assert_eq!(sh.circuit().parts().len(), 3, "undo restores R1");
        assert!(sh.circuit().part_by_ref("R1").is_some());
    }

    /// Removing a part that was wired keeps the merged net intact for the rest.
    #[test]
    fn remove_preserves_wired_nets() {
        let mut sh = Shell::new();
        sh.execute("add R1 a b 1k").unwrap();
        sh.execute("add R2 b c 2k").unwrap();
        sh.execute("wire a c").unwrap();
        let before = sh.circuit().resolve(*sh.nets.get("a").unwrap());
        assert_eq!(before, sh.circuit().resolve(*sh.nets.get("c").unwrap()));

        sh.execute("remove R1").unwrap();
        // `a` and `c` are still the same net after R1 is gone.
        assert_eq!(
            sh.circuit().resolve(*sh.nets.get("a").unwrap()),
            sh.circuit().resolve(*sh.nets.get("c").unwrap()),
            "the wire survives the removal"
        );
    }

    /// `remove` of an unknown reference, and with the wrong arity, fail friendly.
    #[test]
    fn remove_unknown_and_bad_arity_fail_friendly() {
        let mut sh = Shell::new();
        sh.execute("add R1 a b 1k").unwrap();
        let err = sh.execute("remove R9").unwrap_err();
        assert!(matches!(err, ShellError::UnknownPart(_)), "{err}");
        assert!(err.to_string().contains("R9"), "{err}");
        assert!(matches!(
            sh.execute("remove").unwrap_err(),
            ShellError::Usage {
                command: "remove",
                ..
            }
        ));
    }

    // ── interactive editing in the live TUI app (CP-UX-M5.1) ───────────────

    /// The TUI app's edit actions drive the real `Shell` circuit: placing adds a
    /// part, wiring merges nets (verified via `resolve`), removing drops a part —
    /// all asserted on the app's own `Circuit`, no tty.
    #[test]
    fn app_edit_actions_drive_the_shell_circuit() {
        use ee_tui::{App, EditAction};

        let mut app = App::new(Shell::new());
        app.apply_edit(EditAction::Place {
            reference: "V1".into(),
            node_a: "in".into(),
            node_b: "gnd".into(),
            value: "5".into(),
        });
        app.apply_edit(EditAction::Place {
            reference: "R1".into(),
            node_a: "in".into(),
            node_b: "out".into(),
            value: "1k".into(),
        });
        assert_eq!(app.backend().circuit().parts().len(), 2, "two parts placed");
        assert!(app.backend().circuit().part_by_ref("R1").is_some());

        // Wiring `out` to `in` must merge the two nets (same resolved id).
        app.apply_edit(EditAction::Wire {
            node_a: "out".into(),
            node_b: "in".into(),
        });
        let r1 = app.backend().circuit().part_by_ref("R1").unwrap();
        let (ta, tb) = (r1.terminals[0], r1.terminals[1]);
        assert_eq!(
            app.backend().circuit().resolve(ta),
            app.backend().circuit().resolve(tb),
            "wire merged R1's two nets into one"
        );

        // Removing R1 drops it from the app's circuit.
        app.apply_edit(EditAction::Remove {
            reference: "R1".into(),
        });
        assert!(
            app.backend().circuit().part_by_ref("R1").is_none(),
            "R1 removed"
        );
        assert!(
            app.backend().circuit().part_by_ref("V1").is_some(),
            "V1 remains"
        );
    }

    /// The interactive keystroke path (no tty): enter edit mode, place two parts
    /// through the add prompt, then select and delete one — asserted on the app's
    /// circuit. The schematic pane reflects each change, and a malformed add is a
    /// clean logged error, not a panic.
    #[test]
    fn app_edit_keystrokes_place_select_and_remove() {
        use ee_tui::{App, AppEvent, KeyCode, KeyEvent, PanelId};

        let mut app = App::new(Shell::new());
        let key = |app: &mut App<Shell>, code| app.handle_event(AppEvent::Key(KeyEvent::new(code)));
        let typ = |app: &mut App<Shell>, s: &str| {
            for c in s.chars() {
                app.handle_event(AppEvent::Key(KeyEvent::char(c)));
            }
        };

        // Focus the schematic (Tab from the shell) and enter edit mode with 'e'.
        key(&mut app, KeyCode::Tab);
        assert_eq!(app.focus(), PanelId::Schematic);
        key(&mut app, KeyCode::Char('e'));
        assert!(app.edit_mode(), "'e' enters edit mode on the schematic");

        // 'a' opens the add prompt; type the fields; Enter commits.
        key(&mut app, KeyCode::Char('a'));
        typ(&mut app, "R1 a b 1k");
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Char('a'));
        typ(&mut app, "R2 b c 2k");
        key(&mut app, KeyCode::Enter);
        assert_eq!(
            app.backend().circuit().parts().len(),
            2,
            "both parts placed"
        );

        // The schematic pane reflects the edited circuit.
        let frame = app.render(80, 24);
        assert!(
            frame.contains("R1") && frame.contains("R2"),
            "schematic lists parts:\n{frame}"
        );
        assert!(frame.contains("[edit]"), "edit-mode title:\n{frame}");

        // Select the first part and delete it with 'd'.
        key(&mut app, KeyCode::Up);
        key(&mut app, KeyCode::Up);
        assert_eq!(app.selected_part().as_deref(), Some("R1"), "cursor on R1");
        key(&mut app, KeyCode::Char('d'));
        assert!(
            app.backend().circuit().part_by_ref("R1").is_none(),
            "R1 deleted"
        );
        assert!(
            app.backend().circuit().part_by_ref("R2").is_some(),
            "R2 kept"
        );

        // A malformed add (too few fields) is a clean logged error, no panic.
        key(&mut app, KeyCode::Char('a'));
        typ(&mut app, "oops");
        key(&mut app, KeyCode::Enter);
        assert_eq!(
            app.backend().circuit().parts().len(),
            1,
            "bad add placed nothing"
        );
        let frame = app.render(80, 24);
        assert!(
            frame.contains("error"),
            "malformed add surfaces an error:\n{frame}"
        );

        // Esc leaves edit mode cleanly.
        key(&mut app, KeyCode::Esc);
        assert!(!app.edit_mode(), "Esc exits edit mode");
    }

    /// `set <ref>.value` retargets a component value through the shared mutation
    /// path, the op-point reflects it, and it is undoable like any mutation.
    #[test]
    fn set_retargets_a_value_reflects_in_op_point_and_is_undoable() {
        let mut sh = Shell::new();
        sh.execute("add V1 in gnd 10").unwrap();
        sh.execute("add R1 in out 1k").unwrap();
        sh.execute("add R2 out gnd 1k").unwrap();
        // Equal divider: v(out) = 5 V.
        assert!(sh.execute("run op").unwrap().contains("5.000000 V"));

        // Double R1; the midpoint drops to 10·R2/(R1+R2) = 10·1k/3k = 3.333 V.
        let msg = sh.execute("set R1.value 2k").unwrap();
        assert!(msg.contains("set R1.value"), "{msg}");
        assert_eq!(
            sh.circuit()
                .part_by_ref("R1")
                .unwrap()
                .value
                .unwrap()
                .magnitude,
            2000.0
        );
        assert!(
            sh.execute("run op").unwrap().contains("3.333333 V"),
            "op-point reflects the new R1"
        );

        // A bare `<ref>` target also means `.value`.
        sh.execute("set R1 4k").unwrap();
        assert_eq!(
            sh.circuit()
                .part_by_ref("R1")
                .unwrap()
                .value
                .unwrap()
                .magnitude,
            4000.0
        );

        // Undo reverts the last set.
        sh.execute("undo").unwrap();
        assert_eq!(
            sh.circuit()
                .part_by_ref("R1")
                .unwrap()
                .value
                .unwrap()
                .magnitude,
            2000.0,
            "undo restored the prior value"
        );

        // Friendly errors: unknown part, unsupported param, bad value.
        assert!(matches!(
            sh.execute("set R9.value 1k").unwrap_err(),
            ShellError::UnknownPart(_)
        ));
        assert!(matches!(
            sh.execute("set R1.freq 1k").unwrap_err(),
            ShellError::Usage { command: "set", .. }
        ));
        assert!(matches!(
            sh.execute("set R1.value nope").unwrap_err(),
            ShellError::BadValue { .. }
        ));
    }

    /// The CP-UX-M6.1 gate: a live tune through the app event loop re-solves the
    /// circuit so the op-point and the current-flow animation both reflect the new
    /// value, and the derived branch currents have the correct direction — current
    /// leaves the source's `+` terminal and runs through the load. All on data.
    #[test]
    fn app_live_tuning_resolves_op_point_and_current_flow() {
        use ee_tui::{App, AppEvent, KeyCode, KeyEvent, PanelId};

        let key = |app: &mut App<Shell>, code| app.handle_event(AppEvent::Key(KeyEvent::new(code)));
        let typ = |app: &mut App<Shell>, s: &str| {
            for c in s.chars() {
                app.handle_event(AppEvent::Key(KeyEvent::char(c)));
            }
        };

        // Build a divider V1=10 (in→gnd), R1 1k (in→out), R2 1k (out→gnd) through
        // the shared edit path. The empty-circuit solve at construction is refreshed
        // after each placement.
        let mut app = App::new(Shell::new());
        for (r, a, b, v) in [
            ("V1", "in", "gnd", "10"),
            ("R1", "in", "out", "1k"),
            ("R2", "out", "gnd", "1k"),
        ] {
            app.apply_edit(ee_tui::EditAction::Place {
                reference: r.into(),
                node_a: a.into(),
                node_b: b.into(),
                value: v.into(),
            });
        }

        // Resolve the nets for assertions straight off the app's circuit.
        let (in_net, out_net) = {
            let c = app.backend().circuit();
            let r1 = c.part_by_ref("R1").unwrap();
            (c.resolve(r1.terminals[0]), c.resolve(r1.terminals[1]))
        };

        // Op-point before tuning: equal divider → v(out) = 5 V.
        let v_out_before = app.op_voltages().get(out_net).unwrap();
        assert!(
            (v_out_before - 5.0).abs() < 1e-6,
            "v(out)=5 V, got {v_out_before}"
        );

        // Current flow before tuning: it leaves V1's + terminal (in) and runs
        // through R1 from `in` to `out` (the load).
        let v1 = app.branch_currents().get("V1").unwrap();
        assert_eq!(v1.from, in_net, "current leaves V1's + terminal (in)");
        assert!(
            (v1.amps - 5e-3).abs() < 1e-9,
            "V1 sources 5 mA, got {}",
            v1.amps
        );
        let r1_before = app.branch_currents().get("R1").unwrap().clone();
        assert_eq!(r1_before.from, in_net, "R1 current runs from in");
        assert_eq!(r1_before.to, out_net, "…through the load to out");

        // Tune R1 up via the keystroke path: focus schematic, edit mode, select R1,
        // press '+'. The divider midpoint must drop and R1's current must fall.
        key(&mut app, KeyCode::Tab);
        assert_eq!(app.focus(), PanelId::Schematic);
        key(&mut app, KeyCode::Char('e'));
        key(&mut app, KeyCode::Down); // V1 -> R1
        assert_eq!(app.selected_part().as_deref(), Some("R1"));
        key(&mut app, KeyCode::Char('+'));

        // R1 grew by one 10% step → v(out) = 10·1k/(1.1k+1k) = 4.7619 V (< 5 V).
        let v_out_after = app.op_voltages().get(out_net).unwrap();
        assert!(
            v_out_after < v_out_before && (v_out_after - 4.7619).abs() < 1e-3,
            "op-point re-solved: {v_out_before} -> {v_out_after}"
        );
        let r1_after = app.branch_currents().get("R1").unwrap();
        assert!(
            r1_after.amps < r1_before.amps,
            "a bigger R1 carries less current: {} -> {}",
            r1_before.amps,
            r1_after.amps
        );
        assert_eq!(r1_after.from, in_net, "direction unchanged (in → out)");

        // The overlay render reflects it too: toggle it on via the palette and the
        // frame carries the animated flow arrows.
        app.handle_event(AppEvent::Key(KeyEvent::char('p').with_ctrl()));
        typ(&mut app, "toggle overlay");
        key(&mut app, KeyCode::Enter);
        let frame = app.render(120, 30);
        assert!(
            frame.contains('→') || frame.contains('←'),
            "overlay animates current flow:\n{frame}"
        );
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

    /// `undo` reverts the last mutating command: a part added, then undone, is
    /// gone again — part count and references restored.
    #[test]
    fn undo_reverts_the_last_command() {
        let mut sh = Shell::new();
        sh.execute("add R1 a b 1k").unwrap();
        assert_eq!(sh.circuit().parts().len(), 1);

        // Add a second part, then undo it away.
        sh.execute("add R2 b c 2k").unwrap();
        assert_eq!(sh.circuit().parts().len(), 2);
        let msg = sh.execute("undo").unwrap();
        assert!(msg.contains("undone"), "{msg}");
        assert_eq!(sh.circuit().parts().len(), 1, "R2 is gone");
        assert!(
            sh.circuit().part_by_ref("R2").is_none(),
            "R2 reference gone"
        );
        assert!(sh.circuit().part_by_ref("R1").is_some(), "R1 remains");

        // Undo again removes R1 too, back to the empty circuit.
        sh.undo().unwrap();
        assert_eq!(sh.circuit().parts().len(), 0);
    }

    /// A probe can be undone, and a failed command leaves no undo entry behind.
    #[test]
    fn undo_tracks_probes_and_skips_failed_commands() {
        let mut sh = Shell::new();
        sh.execute("add R1 in out 1k").unwrap();
        sh.execute("probe out").unwrap();
        assert_eq!(sh.node_names().len(), 2);

        // A rejected `add` (bad value) must not push an undo point.
        assert!(sh.execute("add R2 out gnd nope").is_err());
        // One undo clears the probe; the next clears R1; the next is the error.
        assert!(sh.undo().unwrap().contains("undone")); // probe
        assert!(sh.undo().unwrap().contains("undone")); // add R1
        assert_eq!(sh.circuit().parts().len(), 0);
        assert!(matches!(sh.undo().unwrap_err(), ShellError::NothingToUndo));
    }

    /// A named checkpoint restores the session to exactly that point.
    #[test]
    fn named_checkpoint_restores_its_state() {
        let mut sh = Shell::new();
        sh.execute("add V1 in gnd 5").unwrap();
        sh.execute("add R1 in out 1k").unwrap();
        sh.execute("checkpoint stable").unwrap();
        assert_eq!(sh.circuit().parts().len(), 2);

        // Diverge from the checkpoint...
        sh.execute("add C1 out gnd 1u").unwrap();
        sh.execute("add R2 out gnd 2k").unwrap();
        assert_eq!(sh.circuit().parts().len(), 4);

        // ...then restore it.
        let msg = sh.execute("restore stable").unwrap();
        assert!(msg.contains("stable"), "{msg}");
        assert_eq!(sh.circuit().parts().len(), 2, "back to the checkpoint");
        assert!(sh.circuit().part_by_ref("V1").is_some());
        assert!(sh.circuit().part_by_ref("R1").is_some());
        assert!(sh.circuit().part_by_ref("C1").is_none());
        assert!(sh.circuit().part_by_ref("R2").is_none());
    }

    /// `undo` on a fresh shell, and `restore` of an unknown name, are friendly
    /// errors rather than panics.
    #[test]
    fn undo_and_restore_fail_friendly() {
        let mut sh = Shell::new();
        assert!(matches!(sh.undo().unwrap_err(), ShellError::NothingToUndo));
        assert!(matches!(
            sh.execute("undo").unwrap_err(),
            ShellError::NothingToUndo
        ));
        let err = sh.execute("restore ghost").unwrap_err();
        assert!(matches!(err, ShellError::UnknownCheckpoint(_)));
        assert!(err.to_string().contains("ghost"), "{err}");

        // Programmatic API matches the command path.
        sh.execute("add R1 a b 1k").unwrap();
        sh.snapshot();
        sh.execute("add R2 a c 2k").unwrap();
        sh.undo().unwrap(); // reverts the add (auto-snapshot)
        assert_eq!(sh.circuit().parts().len(), 1);
        sh.undo().unwrap(); // the manual snapshot
        assert_eq!(sh.circuit().parts().len(), 1, "manual snapshot was post-R1");
    }

    // ── session export / import (CP-UX-M4.1) ───────────────────────────────

    /// Two shells are session-equivalent when their circuits serialize to the
    /// same canonical project text (same parts/values/topology, by ee-fmt's
    /// lossless round-trip), and they carry the same node names, probes, and
    /// checkpoint names.
    fn assert_equivalent(a: &Shell, b: &Shell) {
        assert_eq!(
            ee_fmt::project::save(a.circuit()),
            ee_fmt::project::save(b.circuit()),
            "circuits differ"
        );
        let mut an = a.node_names();
        let mut bn = b.node_names();
        an.sort();
        bn.sort();
        assert_eq!(an, bn, "node names differ");
        assert_eq!(a.probes, b.probes, "probes differ");
        let mut ac = a.checkpoint_names();
        let mut bc = b.checkpoint_names();
        ac.sort();
        bc.sort();
        assert_eq!(ac, bc, "checkpoint names differ");
    }

    /// A built session (parts, a wire, probes, a checkpoint) exports and imports
    /// into a fresh shell as an equivalent session: same topology, probes, node
    /// names, and checkpoints — and the restored checkpoint still works.
    #[test]
    fn session_round_trip_is_equivalent() {
        let mut sh = Shell::new();
        sh.execute("add V1 in gnd 5").unwrap();
        sh.execute("add R1 in out 1k").unwrap();
        sh.execute("add C1 out gnd 1u").unwrap();
        sh.execute("add R2 out spare 2k").unwrap();
        sh.execute("wire spare gnd").unwrap(); // merge a named node into ground
        sh.execute("probe out").unwrap();
        sh.execute("probe in").unwrap();
        sh.execute("checkpoint stable").unwrap();
        // Diverge after the checkpoint so the main body and the checkpoint differ.
        sh.execute("add R3 out gnd 3k").unwrap();

        let text = sh.export_session();
        assert!(text.starts_with("!ee-session v1\n"), "{text}");
        assert!(text.contains("!checkpoint stable"), "{text}");

        let mut fresh = Shell::new();
        fresh.import_session(&text).unwrap();
        assert_equivalent(&sh, &fresh);

        // The probed node names actually resolve in the restored shell: run op
        // tabulates them (the divider midpoint `out` is present).
        let table = fresh.execute("run op").unwrap();
        assert!(
            table.contains("v(out)"),
            "restored probe resolves:\n{table}"
        );

        // The imported checkpoint restores to the pre-R3 state in the fresh shell.
        fresh.execute("restore stable").unwrap();
        assert!(
            fresh.circuit().part_by_ref("R3").is_none(),
            "R3 rolled back"
        );
        assert!(fresh.circuit().part_by_ref("R1").is_some());
        // And it matches the original restored to the same checkpoint.
        sh.execute("restore stable").unwrap();
        assert_eq!(
            ee_fmt::project::save(sh.circuit()),
            ee_fmt::project::save(fresh.circuit()),
        );
    }

    /// Exporting then importing is idempotent at the text level: a second export
    /// of the imported session is byte-identical.
    #[test]
    fn session_export_is_stable_across_a_round_trip() {
        let mut sh = Shell::new();
        sh.execute("add V1 in gnd 5").unwrap();
        sh.execute("add R1 in out 1k").unwrap();
        sh.execute("probe out").unwrap();
        sh.execute("checkpoint a").unwrap();

        let once = sh.export_session();
        let mut fresh = Shell::new();
        fresh.import_session(&once).unwrap();
        assert_eq!(once, fresh.export_session(), "export not idempotent");
    }

    /// The `save`/`load` commands round-trip through a real file and error
    /// cleanly on a missing file and on malformed text.
    #[test]
    fn save_and_load_commands_round_trip_and_fail_friendly() {
        let mut sh = Shell::new();
        sh.execute("add V1 in gnd 5").unwrap();
        sh.execute("add R1 in out 1k").unwrap();
        sh.execute("add C1 out gnd 1u").unwrap();
        sh.execute("probe out").unwrap();
        sh.execute("checkpoint stable").unwrap();

        let path = std::env::temp_dir().join(format!(
            "ee-session-cmd-{}-{}.eesh",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = path.to_str().unwrap();

        let saved = sh.execute(&format!("save {path}")).unwrap();
        assert!(saved.contains("saved session"), "{saved}");

        let mut fresh = Shell::new();
        let loaded = fresh.execute(&format!("load {path}")).unwrap();
        assert!(loaded.contains("loaded session"), "{loaded}");
        assert_equivalent(&sh, &fresh);

        // A missing file is a clean Io error, not a panic.
        let missing = fresh
            .execute("load /no/such/ee-session/file.eesh")
            .unwrap_err();
        assert!(matches!(missing, ShellError::Io { .. }), "{missing}");

        // Malformed session text is a clean Session error.
        let garbage = path.replace(".eesh", "-garbage.eesh");
        std::fs::write(&garbage, "not a session at all\n").unwrap();
        let bad = fresh.execute(&format!("load {garbage}")).unwrap_err();
        assert!(matches!(bad, ShellError::Session(_)), "{bad}");

        // A bad load left the shell untouched (still equivalent to the original).
        assert_equivalent(&sh, &fresh);

        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(&garbage);
    }

    /// `save`/`load` with the wrong argument count is a friendly usage error.
    #[test]
    fn save_load_usage_errors() {
        let mut sh = Shell::new();
        assert!(matches!(
            sh.execute("save").unwrap_err(),
            ShellError::Usage {
                command: "save",
                ..
            }
        ));
        assert!(matches!(
            sh.execute("load a b").unwrap_err(),
            ShellError::Usage {
                command: "load",
                ..
            }
        ));
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
