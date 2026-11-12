//! # ohmforge-shell (`eesh`)
//!
//! The dedicated shell: a parser for the command language, tab-completion,
//! history, and (from M1) an embedded scripting engine for loops/parameters/
//! automation. The shell mutates the same `ohmforge_core::Circuit` the TUI and AI do.
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
//! # use ohmforge_shell::Shell;
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

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;

use ohmforge_core::graph::{
    BjtModel, Circuit, DiodeModel, MosfetModel, NetId, PartKind, Polarity, Waveform,
};
use ohmforge_core::signal::Complex;
use ohmforge_core::units::Unit;
use ohmforge_core::value::{ParseError, Value};
use ohmforge_instruments::Bode;
use ohmforge_sim::{Analysis, Engine, SimError};
use ohmforge_tui::Plot;

/// The default transient duration when `run`/`run tran` is given no `<dur>`.
const DEFAULT_DURATION: f64 = 1e-3;

/// Default AC/noise sweep density (points per decade) when none is given.
const DEFAULT_PPD: u32 = 20;

/// Default Monte-Carlo seed when none is given (deterministic across runs).
const DEFAULT_MC_SEED: u64 = 0xEE_A5_51_57;

/// A transient longer than this (seconds) draws an "absurd duration?" warning —
/// `run tran 1k` is 1000 s, almost always a slipped unit (`CP-UX-M8.2`).
const ABSURD_DURATION: f64 = 100.0;

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
    /// Reverse operations recorded for each mutating command, newest last; `undo`
    /// pops the top to revert the last command (`CP-UX-M2.4`). Most commands record
    /// a cheap reverse op (an `add` just notes its reference) rather than a full
    /// circuit clone, so a long batch of edits stays O(N) overall rather than
    /// O(N²); the depth is bounded by [`UNDO_CAP`] (`CP-UX-M8.2`).
    undo_stack: Vec<UndoOp>,
    /// Named checkpoints the user can `restore` to at any time.
    checkpoints: HashMap<String, Snapshot>,
    /// The set of placed part references, mirroring the circuit's parts, so `add`
    /// can reject a duplicate reference in O(1) instead of scanning every part —
    /// which kept a long build (100k parts) from being O(N²) (`CP-UX-M8.2`).
    placed_refs: HashSet<String>,
    /// The signals from the most recent transient `run`, cached so the live TUI can
    /// scrub them as a playback (`CP-UX-M7.1`). Not part of a [`Snapshot`] — it is a
    /// derived result, like the op-point, so `undo`/`restore` leave it untouched.
    last_transient: Vec<ohmforge_core::signal::Signal>,
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

/// The reverse of one mutating command, recorded so `undo` can revert exactly
/// that command without ever deep-cloning the whole circuit on the common edits.
///
/// `add`/`set`/`probe` record a cheap, O(1)-capture reverse; `wire` and `remove`
/// (whose inverses need the prior topology) keep a full [`Snapshot`]. Because the
/// undo stack is strictly LIFO, the names an `add` introduced can only be used by
/// later commands, all of which are reverted first — so dropping them on undo is
/// safe (`CP-UX-M8.2`).
#[derive(Debug, Clone)]
enum UndoOp {
    /// Restore the whole session (used where a compact inverse is impractical).
    /// Boxed so this large variant does not bloat every cheap reverse op.
    Full(Box<Snapshot>),
    /// Reverse of `add <reference> …`: drop the part and the node names this
    /// command newly introduced.
    Add {
        reference: String,
        added_names: Vec<String>,
    },
    /// Reverse of `set <reference>.value …`: restore the prior value.
    SetValue { reference: String, old_value: Value },
    /// Reverse of `probe <node>`: drop the node this command appended to the probe
    /// list (recorded only when the probe actually changed the list).
    Probe { node: String },
}

/// Maximum number of undo steps retained. Older steps are dropped in batches so a
/// very long edit session (or script) keeps undo memory bounded (`CP-UX-M8.2`).
const UNDO_CAP: usize = 256;

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
            placed_refs: HashSet::new(),
            last_transient: Vec::new(),
        }
    }

    /// Rebuild the placed-reference set from the circuit after a wholesale replace
    /// (undo of a snapshot, checkpoint restore, or session import).
    fn refresh_refs(&mut self) {
        self.placed_refs = self
            .circuit
            .parts()
            .iter()
            .map(|p| p.reference.clone())
            .collect();
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
            // Mutating commands record their own reverse op (on success only) so
            // `undo` can revert exactly the last command. A successful mutation also
            // invalidates the cached transient — the stored waveform no longer
            // reflects the changed circuit — so the live playback is cleared.
            "add" | "wire" | "probe" | "remove" | "set" => {
                let result = match command {
                    "add" => self.cmd_add(&args),
                    "wire" => self.cmd_wire(&args),
                    "remove" => self.cmd_remove(&args),
                    "set" => self.cmd_set(&args),
                    _ => self.cmd_probe(&args),
                };
                if result.is_ok() {
                    self.last_transient.clear();
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
        self.refresh_refs();
    }

    /// Push a reverse op onto the undo stack, dropping the oldest steps in batches
    /// once the stack grows past twice [`UNDO_CAP`] so undo memory stays bounded.
    fn push_undo(&mut self, op: UndoOp) {
        self.undo_stack.push(op);
        if self.undo_stack.len() > UNDO_CAP * 2 {
            let drop = self.undo_stack.len() - UNDO_CAP;
            self.undo_stack.drain(0..drop);
        }
    }

    /// Push a full-snapshot reverse op (the boxed-variant path).
    fn push_full(&mut self, snap: Snapshot) {
        self.push_undo(UndoOp::Full(Box::new(snap)));
    }

    /// Push a manual snapshot of the session onto the undo stack. Mutating
    /// commands do this automatically; call it to mark an extra undo point.
    pub fn snapshot(&mut self) {
        let snap = self.capture();
        self.push_full(snap);
    }

    /// Revert the most recent recorded command (the last mutating command). Returns
    /// a [`ShellError::NothingToUndo`] when the undo stack is empty.
    pub fn undo(&mut self) -> Result<String, ShellError> {
        match self.undo_stack.pop() {
            Some(op) => {
                self.apply_undo(op);
                self.last_transient.clear();
                let n = self.circuit.parts().len();
                Ok(format!(
                    "undone — {n} part{} remain",
                    if n == 1 { "" } else { "s" }
                ))
            }
            None => Err(ShellError::NothingToUndo),
        }
    }

    /// Apply one reverse op, restoring the session to just before the command that
    /// recorded it.
    fn apply_undo(&mut self, op: UndoOp) {
        match op {
            UndoOp::Full(snap) => self.restore_from(*snap),
            UndoOp::Add {
                reference,
                added_names,
            } => {
                for name in &added_names {
                    self.nets.remove(name);
                }
                self.placed_refs.remove(&reference);
                self.circuit = self.rebuild_circuit_without(&reference);
            }
            UndoOp::SetValue {
                reference,
                old_value,
            } => {
                self.circuit.set_value(&reference, old_value);
            }
            UndoOp::Probe { node } => {
                self.probes.retain(|n| n != &node);
            }
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
        self.refresh_refs();
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

    /// `add <ref> <nodes…> [value] [extras…]` — place a part.
    ///
    /// The reference's leading letter selects the device and, with it, the shape of
    /// the rest of the line (`CP-UX-M8.2`):
    ///
    /// ```text
    /// R/C/L/V/I  add R1 a b 1k              two-terminal, value required
    /// D          add D1 a b [Is]            diode, value optional (model Is)
    /// Q          add Q1 c b e [npn|pnp]     BJT, terminals [c,b,e]
    /// M          add M1 d g s [nmos|pmos]   MOSFET, terminals [d,g,s]
    /// E/G        add E1 o+ o- c+ c- <gain>  VCVS (V/V) / VCCS (A/V)
    /// H/F        add H1 o+ o- <gain> <Vctrl> CCVS (V/A) / CCCS (A/A)
    /// ```
    fn cmd_add(&mut self, args: &[&str]) -> Result<String, ShellError> {
        let reference = *args.first().ok_or(ShellError::Usage {
            command: "add",
            usage: "add <ref> <nodeA> <nodeB> <value>",
        })?;
        let letter = reference
            .chars()
            .next()
            .ok_or(ShellError::EmptyReference)?
            .to_ascii_uppercase();
        if self.placed_refs.contains(reference) {
            return Err(ShellError::DuplicateRef(reference.to_string()));
        }
        let rest = &args[1..];
        match letter {
            'R' | 'C' | 'L' | 'V' | 'I' => self.add_two_terminal(reference, rest),
            'D' => self.add_diode(reference, rest),
            'Q' => self.add_bjt(reference, rest),
            'M' => self.add_mosfet(reference, rest),
            'E' | 'G' => self.add_vc_source(reference, letter, rest),
            'H' | 'F' => self.add_cc_source(reference, letter, rest),
            other => Err(ShellError::UnknownPartKind(other)),
        }
    }

    /// Resolve `node_names` to nets, place `kind`/`value`, and record the undo.
    /// The single placement path for every device shape, so the bookkeeping
    /// (new-name tracking, duplicate ref set, undo op) is identical everywhere.
    fn place(
        &mut self,
        reference: &str,
        kind: PartKind,
        value: Option<Value>,
        node_names: &[&str],
    ) -> Result<String, ShellError> {
        let added_names = self.new_node_names(node_names);
        let terminals: Vec<NetId> = node_names.iter().map(|n| self.net_for(n)).collect();
        self.circuit
            .add_part(reference.to_string(), kind, value, terminals);
        self.placed_refs.insert(reference.to_string());
        self.push_undo(UndoOp::Add {
            reference: reference.to_string(),
            added_names,
        });
        Ok(format!("added {reference}"))
    }

    /// Parse a value field into the given unit, quoting the token on failure.
    fn parse_value(&self, value: &str, unit: Unit) -> Result<Value, ShellError> {
        Value::parse(value, unit).map_err(|source| ShellError::BadValue {
            field: format!("value {value:?}"),
            source,
        })
    }

    /// `add R/C/L/V/I <a> <b> <value>` — a two-terminal part with a required value.
    fn add_two_terminal(&mut self, reference: &str, rest: &[&str]) -> Result<String, ShellError> {
        let (kind, unit) = classify(reference)?;
        let [a, b, value] = rest else {
            return Err(ShellError::Usage {
                command: "add",
                usage: "add <ref> <nodeA> <nodeB> <value>",
            });
        };
        // A source shorted to itself (both terminals on one net) is degenerate.
        if matches!(kind, PartKind::VoltageSource | PartKind::CurrentSource) && self.same_net(a, b)
        {
            return Err(ShellError::DegenerateSource(reference.to_string()));
        }
        let value = self.parse_value(value, unit)?;
        self.place(reference, kind, Some(value), &[a, b])
    }

    /// `add D <a> <b> [Is]` — a diode. The value is optional: the core allows a
    /// value-less diode (the model carries `Is`), so a bare `add D1 a b` is valid
    /// and a given value routes to the saturation current (`CP-UX-M8.2`).
    fn add_diode(&mut self, reference: &str, rest: &[&str]) -> Result<String, ShellError> {
        let (a, b, value) = match rest {
            [a, b] => (*a, *b, None),
            [a, b, v] => (*a, *b, Some(self.parse_value(v, Unit::Ampere)?)),
            _ => {
                return Err(ShellError::Usage {
                    command: "add",
                    usage: "add <Dref> <anode> <cathode> [Is]",
                })
            }
        };
        let mut model = DiodeModel::default();
        if let Some(v) = value {
            model.is = v.magnitude;
        }
        self.place(reference, PartKind::Diode(model), value, &[a, b])
    }

    /// `add Q <c> <b> <e> [npn|pnp]` — a BJT (terminals [collector, base, emitter]).
    fn add_bjt(&mut self, reference: &str, rest: &[&str]) -> Result<String, ShellError> {
        let (c, b, e, pol) = match rest {
            [c, b, e] => (*c, *b, *e, Polarity::N),
            [c, b, e, p] => (*c, *b, *e, parse_polarity(p, true)?),
            _ => {
                return Err(ShellError::Usage {
                    command: "add",
                    usage: "add <Qref> <collector> <base> <emitter> [npn|pnp]",
                })
            }
        };
        self.place(
            reference,
            PartKind::Bjt(pol, BjtModel::default()),
            None,
            &[c, b, e],
        )
    }

    /// `add M <d> <g> <s> [nmos|pmos]` — a MOSFET (terminals [drain, gate, source]).
    fn add_mosfet(&mut self, reference: &str, rest: &[&str]) -> Result<String, ShellError> {
        let (d, g, s, pol) = match rest {
            [d, g, s] => (*d, *g, *s, Polarity::N),
            [d, g, s, p] => (*d, *g, *s, parse_polarity(p, false)?),
            _ => {
                return Err(ShellError::Usage {
                    command: "add",
                    usage: "add <Mref> <drain> <gate> <source> [nmos|pmos]",
                })
            }
        };
        self.place(
            reference,
            PartKind::Mosfet(pol, MosfetModel::default()),
            None,
            &[d, g, s],
        )
    }

    /// `add E/G <o+> <o-> <c+> <c-> <gain>` — a voltage-controlled source: VCVS
    /// (`E`, gain in V/V) or VCCS (`G`, transconductance in A/V).
    fn add_vc_source(
        &mut self,
        reference: &str,
        letter: char,
        rest: &[&str],
    ) -> Result<String, ShellError> {
        let [op, om, cp, cm, gain] = rest else {
            return Err(ShellError::Usage {
                command: "add",
                usage: "add <ref> <out+> <out-> <ctrl+> <ctrl-> <gain>",
            });
        };
        let g = self.parse_value(gain, Unit::None)?.magnitude;
        let kind = if letter == 'E' {
            PartKind::Vcvs(g)
        } else {
            PartKind::Vccs(g)
        };
        self.place(reference, kind, None, &[op, om, cp, cm])
    }

    /// `add H/F <o+> <o-> <gain> <Vctrl>` — a current-controlled source: CCVS (`H`,
    /// transresistance in V/A) or CCCS (`F`, current gain in A/A). The controlling
    /// current is sensed through the named voltage source's branch.
    fn add_cc_source(
        &mut self,
        reference: &str,
        letter: char,
        rest: &[&str],
    ) -> Result<String, ShellError> {
        let [op, om, gain, ctrl] = rest else {
            return Err(ShellError::Usage {
                command: "add",
                usage: "add <ref> <out+> <out-> <gain> <Vctrl>",
            });
        };
        let g = self.parse_value(gain, Unit::None)?.magnitude;
        let kind = if letter == 'H' {
            PartKind::Ccvs(g, (*ctrl).to_string())
        } else {
            PartKind::Cccs(g, (*ctrl).to_string())
        };
        self.place(reference, kind, None, &[op, om])
    }

    /// Whether two node names resolve to the same net (ground-aware).
    fn same_net(&self, a: &str, b: &str) -> bool {
        match (self.net_known(a), self.net_known(b)) {
            (Ok(na), Ok(nb)) => self.circuit.resolve(na) == self.circuit.resolve(nb),
            // One or both are new names — new names are distinct unless textually
            // equal (e.g. `add V1 x x`).
            _ => a == b,
        }
    }

    /// The subset of `names` that are not ground and not yet in the net map, each
    /// listed once in first-seen order — the node names a placement will newly
    /// introduce (used to record a precise, clone-free `add` undo).
    fn new_node_names(&self, names: &[&str]) -> Vec<String> {
        let mut added: Vec<String> = Vec::new();
        for &n in names {
            if !is_ground(n) && !self.nets.contains_key(n) && !added.iter().any(|a| a == n) {
                added.push(n.to_string());
            }
        }
        added
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
        // A union-find merge has no cheap inverse, so keep a full snapshot (wires
        // are rare next to `add`/`set`, so this does not reintroduce the O(N²)).
        let before = self.capture();
        self.circuit.wire(na, nb);
        self.push_full(before);
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
        // Only record an undo step when the probe actually changes the list: a
        // duplicate `probe` is a no-op and must not leave a dead undo entry behind
        // (`CP-UX-M8.2`).
        if !self.probes.contains(&node) {
            self.probes.push(node.clone());
            self.push_undo(UndoOp::Probe { node });
        }
        Ok(msg)
    }

    /// `set …` — retarget a value, attach a source waveform, or set a part/circuit
    /// parameter. One verb, several shapes (`CP-UX-M8.2`):
    ///
    /// ```text
    /// set <ref>.value <v> | set <ref> <v>     retarget the primary value (live-tune)
    /// set <ref> sine <amp> <freq> [off] [ph]  attach a sine drive (AC/Bode stimulus)
    /// set <ref> pulse <v1> <v2> <d> <w> <per> attach a pulse drive
    /// set <ref> dc <v>                         revert to a constant (DC) level
    /// set tol <ref> <frac>                     fractional tolerance (Monte-Carlo)
    /// set tempco <ref> <tc>                    temperature coefficient (1/°C)
    /// set temp <celsius>                       circuit operating temperature
    /// ```
    fn cmd_set(&mut self, args: &[&str]) -> Result<String, ShellError> {
        // Global (non-per-part) verbs first — their keywords ("temp"/"tol"/"tempco")
        // are not valid part references, so they never shadow a `set <ref> …`.
        match args {
            ["temp", c] => return self.set_temperature(c),
            ["tol", r, frac] | ["tolerance", r, frac] => return self.set_tolerance(r, frac),
            ["tempco", r, tc] => return self.set_tempco(r, tc),
            _ => {}
        }
        // Per-part waveform verbs, keyed on the second token.
        match args {
            [r, "sine", a, fr] => return self.set_sine(r, a, fr, "0", "0"),
            [r, "sine", a, fr, off] => return self.set_sine(r, a, fr, off, "0"),
            [r, "sine", a, fr, off, ph] => return self.set_sine(r, a, fr, off, ph),
            [r, "pulse", v1, v2, d, w, per] => return self.set_pulse(r, v1, v2, d, w, per),
            [r, "dc", v] => return self.set_dc(r, v),
            _ => {}
        }
        let [target, value] = args else {
            return Err(ShellError::Usage {
                command: "set",
                usage:
                    "set <ref>.value <value> (or: set <ref> sine|pulse|dc …, set temp|tol|tempco …)",
            });
        };
        self.set_value(target, value)
    }

    /// The `set <ref>.value <value>` / `set <ref> <value>` value-retarget path — the
    /// shared mutation the live TUI's `+`/`-` tuning lowers to (`CP-UX-M6.1`).
    fn set_value(&mut self, target: &str, value: &str) -> Result<String, ShellError> {
        let (reference, param) = match target.split_once('.') {
            Some((r, p)) => (r, p),
            None => (target, "value"),
        };
        if !param.eq_ignore_ascii_case("value") {
            return Err(ShellError::Usage {
                command: "set",
                usage: "set <ref>.value <value>",
            });
        }
        let old_value = match self.circuit.part_by_ref(reference) {
            Some(p) => p.value,
            None => return Err(ShellError::UnknownPart(reference.to_string())),
        };
        let unit = old_value
            .map(|v| v.unit)
            .or_else(|| classify(reference).ok().map(|(_, u)| u))
            .unwrap_or(Unit::None);
        let parsed = Value::parse(value, unit).map_err(|source| ShellError::BadValue {
            field: format!("value {value:?}"),
            source,
        })?;
        // Reverse of a value change is just the prior value — unless there was none
        // (a value-less diode), which `set_value` cannot clear again, so snapshot.
        let undo = match old_value {
            Some(old) => UndoOp::SetValue {
                reference: reference.to_string(),
                old_value: old,
            },
            None => UndoOp::Full(Box::new(self.capture())),
        };
        self.circuit.set_value(reference, parsed);
        self.push_undo(undo);
        Ok(format!("set {reference}.value = {value}"))
    }

    /// Require that `reference` names a placed part, for the `set` sub-verbs.
    fn require_part(&self, reference: &str) -> Result<(), ShellError> {
        if self.placed_refs.contains(reference) {
            Ok(())
        } else {
            Err(ShellError::UnknownPart(reference.to_string()))
        }
    }

    /// Attach a [`Waveform`] to an existing source, snapshotting for undo. Only
    /// independent sources act on it; other parts carry it inertly.
    fn set_part_waveform(
        &mut self,
        reference: &str,
        waveform: Waveform,
        msg: String,
    ) -> Result<String, ShellError> {
        self.require_part(reference)?;
        let before = self.capture();
        self.circuit.set_waveform(reference, waveform);
        self.push_full(before);
        Ok(msg)
    }

    /// `set <ref> sine <amp> <freq> [off] [phase]` — a sinusoidal drive (also the
    /// AC/Bode small-signal stimulus).
    fn set_sine(
        &mut self,
        reference: &str,
        amp: &str,
        freq: &str,
        off: &str,
        phase: &str,
    ) -> Result<String, ShellError> {
        let amplitude = self.parse_value(amp, Unit::None)?.magnitude;
        let freq = self.parse_value(freq, Unit::Hertz)?.magnitude;
        let offset = self.parse_value(off, Unit::None)?.magnitude;
        let phase_deg = self.parse_value(phase, Unit::None)?.magnitude;
        self.set_part_waveform(
            reference,
            Waveform::Sine {
                amplitude,
                freq,
                offset,
                phase_deg,
            },
            format!("set {reference} sine {amplitude}@{freq}Hz"),
        )
    }

    /// `set <ref> pulse <v1> <v2> <delay> <width> <period>` — a pulse drive.
    fn set_pulse(
        &mut self,
        reference: &str,
        v1: &str,
        v2: &str,
        delay: &str,
        width: &str,
        period: &str,
    ) -> Result<String, ShellError> {
        let v1 = self.parse_value(v1, Unit::None)?.magnitude;
        let v2 = self.parse_value(v2, Unit::None)?.magnitude;
        let delay = self.parse_value(delay, Unit::Second)?.magnitude;
        let width = self.parse_value(width, Unit::Second)?.magnitude;
        let period = self.parse_value(period, Unit::Second)?.magnitude;
        self.set_part_waveform(
            reference,
            Waveform::Pulse {
                v1,
                v2,
                delay,
                width,
                period,
            },
            format!("set {reference} pulse {v1}→{v2}"),
        )
    }

    /// `set <ref> dc <value>` — revert a source to a constant level (clears any
    /// waveform and retargets the primary value).
    fn set_dc(&mut self, reference: &str, value: &str) -> Result<String, ShellError> {
        self.require_part(reference)?;
        let before = self.capture();
        self.circuit.set_waveform(reference, Waveform::Dc);
        self.push_full(before);
        // Reuse the value path's parsing/formatting (it records its own undo op too,
        // which is fine — two LIFO undo steps revert the one `set … dc` cleanly).
        self.set_value(reference, value)
    }

    /// `set tol <ref> <frac>` — fractional component tolerance (±frac) for
    /// Monte-Carlo. Rejected (as a clean error) for an unknown part or a negative /
    /// non-finite fraction.
    fn set_tolerance(&mut self, reference: &str, frac: &str) -> Result<String, ShellError> {
        self.require_part(reference)?;
        let tol = self.parse_value(frac, Unit::None)?.magnitude;
        let before = self.capture();
        if !self.circuit.set_tolerance(reference, tol) {
            return Err(ShellError::BadValue {
                field: format!("tolerance {frac:?}"),
                source: ParseError::NotANumber,
            });
        }
        self.push_full(before);
        Ok(format!("set {reference} tolerance ±{tol}"))
    }

    /// `set tempco <ref> <tc>` — a part's temperature coefficient (1/°C).
    fn set_tempco(&mut self, reference: &str, tc: &str) -> Result<String, ShellError> {
        self.require_part(reference)?;
        let tc = self.parse_value(tc, Unit::None)?.magnitude;
        let before = self.capture();
        if !self.circuit.set_tempco(reference, tc) {
            return Err(ShellError::BadValue {
                field: format!("tempco {tc:?}"),
                source: ParseError::NotANumber,
            });
        }
        self.push_full(before);
        Ok(format!("set {reference} tempco {tc}/°C"))
    }

    /// `set temp <celsius>` — the circuit's operating temperature.
    fn set_temperature(&mut self, celsius: &str) -> Result<String, ShellError> {
        let t = self.parse_value(celsius, Unit::None)?.magnitude;
        let before = self.capture();
        if !self.circuit.set_temperature(t) {
            return Err(ShellError::BadValue {
                field: format!("temperature {celsius:?}"),
                source: ParseError::NotANumber,
            });
        }
        self.push_full(before);
        Ok(format!("set temperature {t}°C"))
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
    /// `ohmforge_core::Circuit` has no in-place part removal, so this rebuilds an
    /// equivalent circuit without the target part — the same nets (ids and
    /// union-find topology preserved via [`resolve`](Circuit::resolve)), the same
    /// remaining parts with their values, waveforms, tolerances, and tempcos, and
    /// the same operating temperature. A native `Circuit::remove_part` in `ohmforge-core`
    /// would be cleaner; see the cross-crate note in the M5 report.
    pub fn remove(&mut self, reference: &str) -> Result<String, ShellError> {
        if self.circuit.part_by_ref(reference).is_none() {
            return Err(ShellError::UnknownPart(reference.to_string()));
        }
        // Restoring a removed part (with its metadata and net topology) has no
        // compact inverse, so snapshot. `remove` already rebuilds the circuit
        // (O(N)), so the extra clone does not change its complexity.
        let before = self.capture();
        self.push_full(before);
        self.circuit = self.rebuild_circuit_without(reference);
        self.placed_refs.remove(reference);
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

    /// `run <analysis> …` — simulate and render. Reaches every engine analysis:
    /// `tran`, `op`, `ac`, `dc`, `noise`, `mc`, and `tf` (`CP-UX-M8.2`).
    fn cmd_run(&mut self, args: &[&str]) -> Result<String, ShellError> {
        match args {
            [] | ["tran"] => self.run_transient(DEFAULT_DURATION),
            ["tran", dur] => {
                let d = self.parse_duration(dur)?;
                self.run_transient(d)
            }
            ["op"] => self.run_op(),
            ["ac", f0, f1] => self.run_ac(f0, f1, DEFAULT_PPD),
            ["ac", f0, f1, ppd] => {
                let ppd = self.parse_count(ppd, "points-per-decade")?;
                self.run_ac(f0, f1, ppd)
            }
            ["dc", source, start, stop, step] => self.run_dc(source, start, stop, step),
            ["noise", out, f0, f1] => self.run_noise(out, f0, f1, DEFAULT_PPD),
            ["noise", out, f0, f1, ppd] => {
                let ppd = self.parse_count(ppd, "points-per-decade")?;
                self.run_noise(out, f0, f1, ppd)
            }
            ["mc", out, runs] => self.run_mc(out, runs, DEFAULT_MC_SEED),
            ["mc", out, runs, seed] => {
                let seed = seed.parse::<u64>().map_err(|_| ShellError::BadValue {
                    field: format!("seed {seed:?}"),
                    source: ParseError::NotANumber,
                })?;
                self.run_mc(out, runs, seed)
            }
            ["tf", out, source] => self.run_tf(out, source),
            _ => Err(ShellError::Usage {
                command: "run",
                usage: "run tran [<dur>] | op | ac <f0> <f1> [ppd] | dc <src> <start> <stop> <step> | noise <node> <f0> <f1> [ppd] | mc <node> <runs> [seed] | tf <node> <src>",
            }),
        }
    }

    /// Parse and validate a transient duration: reject ≤0 (a token-named error) and
    /// leave an "absurd duration" note to be surfaced by the caller via a warning.
    fn parse_duration(&self, dur: &str) -> Result<f64, ShellError> {
        let d = self.parse_value(dur, Unit::Second)?.magnitude;
        if !d.is_finite() || d <= 0.0 {
            return Err(ShellError::BadDuration(dur.to_string()));
        }
        Ok(d)
    }

    /// Parse a non-negative integer count field (points-per-decade, runs).
    fn parse_count(&self, token: &str, field: &str) -> Result<u32, ShellError> {
        token.parse::<u32>().map_err(|_| ShellError::BadValue {
            field: format!("{field} {token:?}"),
            source: ParseError::NotANumber,
        })
    }

    /// The NetId of a named output node for an analysis that takes one.
    fn output_net(&self, name: &str) -> Result<NetId, ShellError> {
        self.net_known(name)
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
        let cached: Vec<ohmforge_core::signal::Signal> = self
            .probes
            .iter()
            .map(|node| self.signal_name(node))
            .filter_map(|name| signals.iter().find(|s| s.name == name).cloned())
            .collect();
        self.last_transient = cached;
        // An absurdly long window is almost always a slipped unit — warn, don't fail.
        if duration > ABSURD_DURATION {
            out = format!(
                "warning: transient window is {duration} s — did you mean a smaller unit?\n{out}"
            );
        }
        Ok(out)
    }

    /// `run ac <f0> <f1> [ppd]` — small-signal AC sweep, rendered as a Bode plot per
    /// probed node. The drive comes from any source carrying a `sine` waveform
    /// (`set <ref> sine …`); with no AC drive the response is flat zero.
    fn run_ac(&mut self, f0: &str, f1: &str, ppd: u32) -> Result<String, ShellError> {
        let f_start = self.parse_value(f0, Unit::Hertz)?.magnitude;
        let f_stop = self.parse_value(f1, Unit::Hertz)?.magnitude;
        let signals = Engine::new()
            .run(
                &self.circuit,
                &Analysis::Ac {
                    f_start,
                    f_stop,
                    points_per_decade: ppd,
                },
            )
            .map_err(ShellError::Sim)?;
        if self.probes.is_empty() {
            return Ok("nothing probed — add `probe <node>` to see a Bode plot.".to_string());
        }
        let mut out = String::new();
        for (i, node) in self.probes.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            let name = self.signal_name(node);
            match signals.iter().find(|s| s.name == name) {
                Some(sig) => {
                    let title = format!("AC: v({node})");
                    out.push_str(&Bode::new(&title).size(72, 8).render(sig));
                    out.push('\n');
                }
                None => out.push_str(&format!("  v({node}) is ground (0 V) — nothing to plot.\n")),
            }
        }
        Ok(out)
    }

    /// `run dc <source> <start> <stop> <step>` — a DC sweep of one source's level,
    /// rendered as each probed node's response versus the swept level.
    fn run_dc(
        &mut self,
        source: &str,
        start: &str,
        stop: &str,
        step: &str,
    ) -> Result<String, ShellError> {
        // Sweep bounds carry the source's natural unit (V or A) for nice x labels.
        let unit = self
            .circuit
            .part_by_ref(source)
            .and_then(|p| p.value.map(|v| v.unit))
            .unwrap_or(Unit::Volt);
        let start_v = self.parse_value(start, unit)?.magnitude;
        let stop_v = self.parse_value(stop, unit)?.magnitude;
        let step_v = self.parse_value(step, unit)?.magnitude;
        let signals = Engine::new()
            .run(
                &self.circuit,
                &Analysis::DcSweep {
                    source: source.to_string(),
                    start: start_v,
                    stop: stop_v,
                    step: step_v,
                },
            )
            .map_err(ShellError::Sim)?;
        if self.probes.is_empty() {
            return Ok("nothing probed — add `probe <node>` to see a sweep.".to_string());
        }
        let x_unit = unit.symbol();
        let mut out = String::new();
        for (i, node) in self.probes.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            let name = self.signal_name(node);
            match signals.iter().find(|s| s.name == name) {
                Some(sig) => {
                    let y = sig.real().unwrap_or(&[]);
                    let title = format!("v({node}) vs {source}");
                    out.push_str(&Plot::new(&title).units("V", x_unit).render(&sig.x, y));
                }
                None => out.push_str(&format!("  v({node}) is ground (0 V) — nothing to plot.\n")),
            }
        }
        Ok(out)
    }

    /// `run noise <node> <f0> <f1> [ppd]` — output-referred noise PSD at `node`.
    fn run_noise(
        &mut self,
        node: &str,
        f0: &str,
        f1: &str,
        ppd: u32,
    ) -> Result<String, ShellError> {
        let output = self.output_net(node)?;
        let f_start = self.parse_value(f0, Unit::Hertz)?.magnitude;
        let f_stop = self.parse_value(f1, Unit::Hertz)?.magnitude;
        let signals = Engine::new()
            .run(
                &self.circuit,
                &Analysis::Noise {
                    output,
                    f_start,
                    f_stop,
                    points_per_decade: ppd,
                },
            )
            .map_err(ShellError::Sim)?;
        let sig = signals.first().ok_or(ShellError::Sim(SimError::Singular))?;
        let y = sig.real().unwrap_or(&[]);
        let peak = y.iter().copied().fold(0.0_f64, f64::max);
        let title = format!("noise PSD at v({node})");
        let mut out = format!("  {}  peak {:.3e} V²/Hz\n", sig.name, peak);
        out.push_str(&Plot::new(&title).units("V²/Hz", "Hz").render(&sig.x, y));
        Ok(out)
    }

    /// `run mc <node> <runs> [seed]` — Monte-Carlo tolerance spread of the DC
    /// operating-point voltage at `node`, summarized as mean / σ / min / max.
    fn run_mc(&mut self, node: &str, runs: &str, seed: u64) -> Result<String, ShellError> {
        let output = self.output_net(node)?;
        let runs = self.parse_count(runs, "runs")? as usize;
        let signals = Engine::new()
            .run(
                &self.circuit,
                &Analysis::MonteCarlo {
                    seed,
                    runs,
                    output,
                    sample: 0,
                    inner: Box::new(Analysis::OperatingPoint),
                },
            )
            .map_err(ShellError::Sim)?;
        let samples = signals.first().and_then(|s| s.real()).unwrap_or(&[]);
        let (mean, std, lo, hi) = stats(samples);
        Ok(format!(
            "monte-carlo v({node}) over {runs} runs (seed {seed:#x}):\n  \
             mean {mean:.6} V  σ {std:.6} V  min {lo:.6} V  max {hi:.6} V\n"
        ))
    }

    /// `run tf <node> <source>` — small-signal transfer-function / pole-zero report
    /// from an independent `source` to `node`.
    fn run_tf(&mut self, node: &str, source: &str) -> Result<String, ShellError> {
        let output = self.output_net(node)?;
        let tf = Engine::new()
            .transfer_function(&self.circuit, output, source)
            .map_err(ShellError::Sim)?;
        let mut out = format!(
            "transfer function v({node})/{source}:\n  \
             DC gain {:.6}\n  Rin {:.4} Ω\n  Rout {:.4} Ω\n",
            tf.dc_gain, tf.input_impedance, tf.output_impedance
        );
        out.push_str(&format!("  poles ({}):\n", tf.poles.len()));
        for p in &tf.poles {
            out.push_str(&format!("    {}\n", fmt_complex_rad(p)));
        }
        match &tf.zeros {
            Some(zeros) => {
                out.push_str(&format!("  zeros ({}):\n", zeros.len()));
                for z in zeros {
                    out.push_str(&format!("    {}\n", fmt_complex_rad(z)));
                }
            }
            None => out.push_str("  zeros: (not tractable for this circuit)\n"),
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
            "  add <ref> <a> <b> <value>   place R/C/L/V/I (2-terminal) or D (value optional)",
            "  add Q<n> <c> <b> <e> [npn|pnp]    place a BJT (collector base emitter)",
            "  add M<n> <d> <g> <s> [nmos|pmos]  place a MOSFET (drain gate source)",
            "  add E/G <o+> <o-> <c+> <c-> <k>   VCVS (V/V) / VCCS (A/V)",
            "  add H/F <o+> <o-> <k> <Vctrl>     CCVS (V/A) / CCCS (A/A)",
            "  wire <a> <b>                merge two named nodes into one net",
            "  probe <node>                mark a node to plot on the next run",
            "  remove <ref>                delete a placed part",
            "  set <ref>.value <value>     retarget a component's value (live-tunable)",
            "  set <ref> sine <amp> <freq> [off] [phase]   attach a sine drive (AC/Bode)",
            "  set <ref> pulse <v1> <v2> <d> <w> <per>      attach a pulse drive",
            "  set tol|tempco <ref> <n>    tolerance (Monte-Carlo) / temperature coefficient",
            "  set temp <celsius>          circuit operating temperature",
            "  run tran [<dur>]            transient (default 1 ms), plotting each probe",
            "  run op                      DC operating-point node-voltage table",
            "  run ac <f0> <f1> [ppd]      small-signal AC sweep (Bode per probe)",
            "  run dc <src> <start> <stop> <step>   DC sweep of a source's level",
            "  run noise <node> <f0> <f1> [ppd]     output-referred noise PSD",
            "  run mc <node> <runs> [seed]  Monte-Carlo tolerance spread",
            "  run tf <node> <src>         transfer function / pole-zero report",
            "  show                        list the placed parts",
            "  undo                        revert the last add/wire/probe/set",
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
/// This is the one seam between `ohmforge-shell` and `ohmforge-tui`'s interactive app
/// (`CP-UX-M1.4`): the TUI calls [`execute`](Shell::execute) for each line the
/// user runs and reads [`circuit`](Shell::circuit) to draw the schematic, while
/// the dependency arrow stays `ohmforge-shell → ohmforge-tui`. The binary wires the live app
/// with `ohmforge_tui::run(Shell::new())`.
impl ohmforge_tui::Backend for Shell {
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
    fn op_point(&self) -> ohmforge_tui::NodeVoltages {
        match Engine::new().run(&self.circuit, &Analysis::OperatingPoint) {
            Ok(signals) => ohmforge_tui::NodeVoltages::from_signals(&signals),
            Err(_) => ohmforge_tui::NodeVoltages::new(),
        }
    }

    /// Hand the live TUI the signals from the most recent transient `run` so it can
    /// scrub them as a playback (`CP-UX-M7.1`). Empty until a transient has run (or
    /// after one that probed nothing); the cache is refreshed by `run_transient`.
    fn transient(&self) -> Vec<ohmforge_core::signal::Signal> {
        self.last_transient.clone()
    }
}

/// Map a reference's leading letter to a part kind and its value's unit —
/// `R`→Ω, `C`→F, `L`→H, `V`→V, `I`→A. (Mirrors `ohmforge-fmt`'s private netlist classifier.)
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

/// Parse a transistor polarity token, defaulting to the n-type. `bjt` selects the
/// accepted words: `npn`/`n`/`pnp`/`p` for a BJT, `nmos`/`n`/`pmos`/`p` for a
/// MOSFET. An unrecognized token is a friendly [`ShellError::BadPolarity`].
fn parse_polarity(token: &str, bjt: bool) -> Result<Polarity, ShellError> {
    match token.to_ascii_lowercase().as_str() {
        "n" | "npn" if bjt => Ok(Polarity::N),
        "p" | "pnp" if bjt => Ok(Polarity::P),
        "n" | "nmos" if !bjt => Ok(Polarity::N),
        "p" | "pmos" if !bjt => Ok(Polarity::P),
        _ => Err(ShellError::BadPolarity(token.to_string())),
    }
}

/// Mean, population standard deviation, min, and max of a sample set (all `0.0`
/// for an empty set) — the Monte-Carlo spread readout.
fn stats(samples: &[f64]) -> (f64, f64, f64, f64) {
    if samples.is_empty() {
        return (0.0, 0.0, 0.0, 0.0);
    }
    let n = samples.len() as f64;
    let mean = samples.iter().sum::<f64>() / n;
    let var = samples.iter().map(|&v| (v - mean).powi(2)).sum::<f64>() / n;
    let lo = samples.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = samples.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    (mean, var.sqrt(), lo, hi)
}

/// Format a Laplace-plane root (rad/s) compactly: a real root as a scalar, a
/// complex pair as `re ± im·j`.
fn fmt_complex_rad(z: &Complex) -> String {
    if z.im.abs() < 1e-9 * z.re.abs().max(1.0) {
        format!("{:.4} rad/s", z.re)
    } else {
        format!(
            "{:.4} {} {:.4}j rad/s",
            z.re,
            if z.im < 0.0 { "-" } else { "+" },
            z.im.abs()
        )
    }
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
    /// A transistor polarity token was neither the n- nor the p-type.
    BadPolarity(String),
    /// An independent source was placed with both terminals on one net.
    DegenerateSource(String),
    /// A transient duration was zero, negative, or non-finite.
    BadDuration(String),
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
                write!(
                    f,
                    "unknown part type '{c}' — expected R, C, L, V, I, D, Q, M, E, G, H, or F"
                )
            }
            ShellError::BadPolarity(p) => {
                write!(f, "unknown polarity {p:?} — expected npn/pnp or nmos/pmos")
            }
            ShellError::DegenerateSource(r) => {
                write!(f, "source {r:?} has both terminals on the same node")
            }
            ShellError::BadDuration(d) => {
                write!(f, "duration {d:?} must be a positive time (e.g. 5m, 1u)")
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
        use ohmforge_tui::{App, EditAction};

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
        use ohmforge_tui::{App, AppEvent, KeyCode, KeyEvent, PanelId};

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
        use ohmforge_tui::{App, AppEvent, KeyCode, KeyEvent, PanelId};

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
            app.apply_edit(ohmforge_tui::EditAction::Place {
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

        // The overlay render reflects it too. Leave edit mode first — edit-mode
        // chrome now takes precedence over the overlay (`CP-UX-M8.1`) — then toggle
        // the overlay on via the palette; the frame carries the animated flow arrows.
        key(&mut app, KeyCode::Esc);
        assert!(
            !app.edit_mode(),
            "left edit mode before viewing the overlay"
        );
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

    /// Regression (`CP-UX-M8.2`): a large build must stay roughly linear, not the
    /// old O(N²) (every `add` deep-cloned the whole circuit for undo and scanned
    /// every part for a duplicate reference). 100k parts must place quickly; the
    /// generous time bound only catches a return of the quadratic blow-up.
    #[test]
    fn large_build_script_is_not_quadratic() {
        let src = "repeat 100000 as i {\n  add R$i a b 1k\n}";
        let start = std::time::Instant::now();
        let mut sh = Shell::new();
        let run = sh.run_script(src).expect("the 100k build runs");
        let elapsed = start.elapsed();
        assert_eq!(sh.circuit().parts().len(), 100_000, "all parts placed");
        assert_eq!(run.recording.commands().len(), 100_000);
        assert!(
            elapsed < std::time::Duration::from_secs(30),
            "100k-part build took {elapsed:?} — the O(N²) undo/dup-check is back"
        );
        // One undo reverts exactly the last part — cheap even at this size.
        sh.undo().unwrap();
        assert_eq!(sh.circuit().parts().len(), 99_999);
    }

    /// Regression (`CP-UX-M8.2`): an absurd `repeat` count is rejected up front with
    /// a typed error rather than expanding forever.
    #[test]
    fn runaway_repeat_is_rejected_fast() {
        let src = "repeat 100000000 as i {\n  add R$i a b 1k\n}";
        let start = std::time::Instant::now();
        let mut sh = Shell::new();
        let err = sh.run_script(src).unwrap_err();
        assert!(
            matches!(err, ScriptError::TooLarge { .. }),
            "expected TooLarge, got {err}"
        );
        assert!(
            start.elapsed() < std::time::Duration::from_secs(30),
            "a runaway repeat must fail fast, not hang"
        );
    }

    /// Regression (`CP-UX-M8.2`): a duplicate `probe` changes nothing, so it must
    /// not push a dead undo step. After `probe out` twice, a single `undo` clears
    /// the probe and the next `undo` reaches the `add` (not a probe no-op).
    #[test]
    fn duplicate_probe_records_no_undo_step() {
        let mut sh = Shell::new();
        sh.execute("add R1 in out 1k").unwrap();
        sh.execute("probe out").unwrap();
        sh.execute("probe out").unwrap(); // duplicate: a no-op
        assert_eq!(sh.probes.len(), 1, "probed once");
        sh.undo().unwrap(); // reverts the (single) probe
        assert!(sh.probes.is_empty(), "the one probe is gone");
        // The next undo is the add, not a second probe no-op.
        let msg = sh.undo().unwrap();
        assert!(msg.contains("undone"), "{msg}");
        assert_eq!(sh.circuit().parts().len(), 0, "R1 removed by the next undo");
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
    /// same canonical project text (same parts/values/topology, by ohmforge-fmt's
    /// lossless round-trip), and they carry the same node names, probes, and
    /// checkpoint names.
    fn assert_equivalent(a: &Shell, b: &Shell) {
        assert_eq!(
            ohmforge_fmt::project::save(a.circuit()),
            ohmforge_fmt::project::save(b.circuit()),
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
            ohmforge_fmt::project::save(sh.circuit()),
            ohmforge_fmt::project::save(fresh.circuit()),
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

    // ── CP-UX-M8.2: shell reach + correctness ──────────────────────────────

    /// Regression: `run ac` reaches the AC engine and renders a Bode plot per
    /// probe. The sine drive (`set V1 sine …`) is the small-signal stimulus.
    #[test]
    fn run_ac_renders_a_bode_plot() {
        let mut sh = Shell::new();
        sh.execute("add V1 in gnd 1").unwrap();
        sh.execute("set V1 sine 1 1k").unwrap();
        sh.execute("add R1 in out 1k").unwrap();
        sh.execute("add C1 out gnd 159n").unwrap(); // fc ≈ 1 kHz
        sh.execute("probe out").unwrap();
        let out = sh.execute("run ac 10 100k").unwrap();
        assert!(out.contains("AC: v(out)"), "names the probe:\n{out}");
        assert!(out.contains("|H| (dB)"), "a Bode magnitude plot:\n{out}");
        assert!(out.contains("f(-3dB)"), "a corner readout:\n{out}");
    }

    /// Regression: `run dc` sweeps a source and plots each probe versus the level.
    #[test]
    fn run_dc_sweeps_a_source() {
        let mut sh = Shell::new();
        sh.execute("add V1 in gnd 0").unwrap();
        sh.execute("add R1 in out 1k").unwrap();
        sh.execute("add R2 out gnd 1k").unwrap();
        sh.execute("probe out").unwrap();
        let out = sh.execute("run dc V1 0 10 1").unwrap();
        assert!(out.contains("v(out) vs V1"), "titled sweep:\n{out}");
        assert!(out.contains('\u{2524}'), "a braille frame:\n{out}");
    }

    /// Regression: `run tf` reports the DC gain and the pole set.
    #[test]
    fn run_tf_reports_gain_and_poles() {
        let mut sh = Shell::new();
        sh.execute("add V1 in gnd 1").unwrap();
        sh.execute("add R1 in out 1k").unwrap();
        sh.execute("add C1 out gnd 1u").unwrap();
        let out = sh.execute("run tf out V1").unwrap();
        assert!(out.contains("DC gain"), "{out}");
        assert!(out.contains("poles"), "{out}");
        // A first-order RC low-pass has exactly one real pole at −1/RC ≈ −1000 rad/s.
        assert!(out.contains("poles (1)"), "one pole expected:\n{out}");
    }

    /// Regression: `run noise` reaches the noise engine and reports a PSD readout.
    #[test]
    fn run_noise_reports_psd() {
        let mut sh = Shell::new();
        sh.execute("add V1 in gnd 1").unwrap();
        sh.execute("add R1 in out 1k").unwrap();
        sh.execute("add R2 out gnd 1k").unwrap();
        let out = sh.execute("run noise out 1 1k").unwrap();
        assert!(out.contains("V²/Hz"), "a PSD readout:\n{out}");
    }

    /// Gate: a BJT common-emitter amplifier is placeable and solvable from eesh —
    /// the 3-terminal `Q` part reaches the nonlinear engine and the bias solves.
    #[test]
    fn bjt_common_emitter_amp_places_and_solves() {
        let mut sh = Shell::new();
        sh.execute("add V1 vcc gnd 10").unwrap();
        sh.execute("add RC vcc c 1k").unwrap();
        sh.execute("add RB vcc b 470k").unwrap();
        sh.execute("add Q1 c b e npn").unwrap();
        sh.execute("add RE e gnd 100").unwrap();
        // The BJT placed with three terminals and the right kind.
        let q = sh.circuit().part_by_ref("Q1").expect("Q1 placed");
        assert!(matches!(q.kind, PartKind::Bjt(Polarity::N, _)));
        assert_eq!(q.terminals.len(), 3, "collector, base, emitter");
        // The bias point solves and sits between the rails (the stage is active).
        let table = sh.execute("run op").unwrap();
        let vc = parse_op_voltage(&table, "c").expect("v(c) in the table");
        assert!(
            vc > 0.1 && vc < 9.9,
            "collector should sit between the rails, got {vc} V:\n{table}"
        );
    }

    /// A MOSFET and the controlled sources place with the right terminal counts.
    #[test]
    fn mosfet_and_controlled_sources_place() {
        let mut sh = Shell::new();
        sh.execute("add M1 d g s nmos").unwrap();
        assert!(matches!(
            sh.circuit().part_by_ref("M1").unwrap().kind,
            PartKind::Mosfet(Polarity::N, _)
        ));
        sh.execute("add V1 cp gnd 1").unwrap();
        sh.execute("add E1 op gnd cp gnd 100").unwrap(); // VCVS, 4 terminals
        let e = sh.circuit().part_by_ref("E1").unwrap();
        assert!(matches!(e.kind, PartKind::Vcvs(_)));
        assert_eq!(e.terminals.len(), 4);
        sh.execute("add H1 oh gnd 50 V1").unwrap(); // CCVS, 2 terminals + control
        let h = sh.circuit().part_by_ref("H1").unwrap();
        assert!(matches!(h.kind, PartKind::Ccvs(_, _)));
        assert_eq!(h.terminals.len(), 2);
    }

    /// `set <ref> sine …`, tolerance, tempco, and temperature all reach the core.
    #[test]
    fn waveform_and_parameter_verbs_reach_the_core() {
        let mut sh = Shell::new();
        sh.execute("add V1 in gnd 1").unwrap();
        sh.execute("add R1 in gnd 1k").unwrap();
        sh.execute("set V1 sine 2 1k 0.5 90").unwrap();
        assert!(matches!(
            sh.circuit().part_by_ref("V1").unwrap().waveform,
            Waveform::Sine { .. }
        ));
        sh.execute("set tol R1 0.05").unwrap();
        assert_eq!(sh.circuit().tolerance("R1"), Some(0.05));
        sh.execute("set tempco R1 0.001").unwrap();
        assert_eq!(sh.circuit().tempco("R1"), Some(0.001));
        sh.execute("set temp 85").unwrap();
        assert_eq!(sh.circuit().temperature(), 85.0);
        // Undo reverts the temperature change (one full-snapshot step).
        sh.undo().unwrap();
        assert_eq!(sh.circuit().temperature(), 27.0);
    }

    /// Edge cases: degenerate source, non-positive duration, and the absurd-window
    /// warning each behave (clean typed errors / a non-fatal warning).
    #[test]
    fn run_and_add_edge_cases_are_clean() {
        let mut sh = Shell::new();
        // A source shorted to itself is degenerate.
        assert!(matches!(
            sh.execute("add V1 a a 5").unwrap_err(),
            ShellError::DegenerateSource(_)
        ));
        sh.execute("add V1 in gnd 5").unwrap();
        sh.execute("add R1 in out 1k").unwrap();
        sh.execute("add C1 out gnd 1u").unwrap();
        sh.execute("probe out").unwrap();
        // Zero / negative durations are rejected with a typed error.
        assert!(matches!(
            sh.execute("run tran 0").unwrap_err(),
            ShellError::BadDuration(_)
        ));
        assert!(matches!(
            sh.execute("run tran -1").unwrap_err(),
            ShellError::BadDuration(_)
        ));
        // An absurd window (1k = 1000 s) warns but still runs.
        let out = sh.execute("run tran 1k").unwrap();
        assert!(out.starts_with("warning"), "absurd-window warning:\n{out}");
        assert!(out.contains('\u{2524}'), "still renders a frame:\n{out}");
    }

    /// The unknown-part-type message lists the real set and names the letter.
    #[test]
    fn unknown_part_type_message_lists_the_real_set() {
        let mut sh = Shell::new();
        let err = sh.execute("add Z1 a b 1k").unwrap_err();
        assert!(matches!(err, ShellError::UnknownPartKind('Z')));
        let msg = err.to_string();
        assert!(msg.contains('Z'), "names the letter: {msg}");
        assert!(
            msg.contains('Q') && msg.contains('D'),
            "lists Q and D: {msg}"
        );
    }

    /// A value-less diode is valid (`add D1 a b`); a given value routes to `Is`.
    #[test]
    fn diode_value_is_optional() {
        let mut sh = Shell::new();
        sh.execute("add D1 a b")
            .expect("a value-less diode is valid");
        assert!(matches!(
            sh.circuit().part_by_ref("D1").unwrap().kind,
            PartKind::Diode(_)
        ));
        sh.execute("add D2 a b 2e-14")
            .expect("a diode with an Is value is valid");
        if let PartKind::Diode(m) = sh.circuit().part_by_ref("D2").unwrap().kind {
            assert!((m.is - 2e-14).abs() < 1e-20, "Is routed to the model");
        } else {
            panic!("D2 is not a diode");
        }
    }

    /// Pull a `v(<name>) = <volts> V` reading out of a `run op` table, for tests.
    fn parse_op_voltage(table: &str, node: &str) -> Option<f64> {
        let needle = format!("v({node}) = ");
        table.lines().find_map(|l| {
            let l = l.trim();
            let rest = l.strip_prefix(&needle)?;
            rest.trim_end_matches(" V").trim().parse::<f64>().ok()
        })
    }
}
