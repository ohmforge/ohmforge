//! Session export/import — a portable text form for a whole [`Shell`](crate::Shell)
//! session (`CP-UX-M4.1`).
//!
//! A session is more than a circuit: it also carries the user's node *names*, the
//! probe list, and any named checkpoints. This module serializes all of that to a
//! self-contained, diff-friendly text and parses it back so a fresh
//! [`Shell`](crate::Shell) restores to an equivalent session.
//!
//! The circuit half of each block reuses [`ohmforge_fmt::project`] — the native project
//! format already round-trips an [`ohmforge_core::Circuit`] losslessly — so this module
//! only has to add the session layer around it. Because the project text carries
//! its own `.`-directives, each embedded circuit is written behind a
//! `.circuit <N>` line count and read back verbatim, which keeps the two grammars
//! from colliding.
//!
//! ```text
//! !ee-session v1
//! .probe out                 // probes, in order
//! .node in V1 0              // user node name → a representative part terminal
//! .node out R1 1             //   (so its net survives the circuit's renumbering)
//! .circuit 4                 // the next 4 lines are native project text, verbatim
//! !ee-project v1
//! .name shell
//! V1 V 5V n1 gnd
//! R1 R 1000Ω n1 n2
//! !checkpoint stable         // a named checkpoint: another full session body
//! .node in V1 0
//! .circuit 3
//! !ee-project v1
//! .name shell
//! V1 V 5V n1 gnd
//! ```
//!
//! ## Why a node is pinned to a part terminal
//!
//! [`ohmforge_fmt::project::load`] preserves topology but not net-id *numbers* — a net
//! labelled `n5` on save may reload as a different id. So a node name cannot be
//! stored as a bare net id. Instead each name is pinned to one part terminal that
//! sits on its net; after the circuit reloads, resolving that terminal recovers
//! the name's (possibly renumbered) net. A node with no part on it — which the
//! shell's own commands never produce — is written as `.node <name> -` and gets a
//! fresh net on load.

use std::collections::HashMap;
use std::error::Error;
use std::fmt::{self, Write as _};

use ohmforge_core::graph::{Circuit, NetId};
use ohmforge_fmt::project::{self, ProjectError};

/// The marker that opens every session file.
const HEADER_TAG: &str = "!ee-session";
/// The only session format version this module reads and writes.
const VERSION: &str = "v1";
/// Opens each named checkpoint block.
const CHECKPOINT_TAG: &str = "!checkpoint";

/// One decoded session body: the circuit, the user node-name → net map rebuilt
/// against that circuit, and the probe list.
#[derive(Debug)]
pub(crate) struct Body {
    pub circuit: Circuit,
    pub nets: HashMap<String, NetId>,
    pub probes: Vec<String>,
}

/// A fully parsed session: the live body plus every named checkpoint, in file
/// order.
#[derive(Debug)]
pub(crate) struct Parsed {
    pub main: Body,
    pub checkpoints: Vec<(String, Body)>,
}

/// Everything that can go wrong parsing a session text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    /// The file did not open with `!ee-session <version>`.
    MissingHeader,
    /// The header named a version this build does not understand.
    UnknownVersion(String),
    /// A line was not one this block's grammar allows (with a short reason).
    Malformed(String),
    /// The embedded circuit's native project text failed to parse.
    Project(ProjectError),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionError::MissingHeader => {
                write!(f, "missing `{HEADER_TAG} {VERSION}` header")
            }
            SessionError::UnknownVersion(v) => {
                write!(f, "unknown session version {v:?} (expected {VERSION:?})")
            }
            SessionError::Malformed(why) => write!(f, "malformed session: {why}"),
            SessionError::Project(e) => write!(f, "bad circuit in session: {e}"),
        }
    }
}

impl Error for SessionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            SessionError::Project(e) => Some(e),
            _ => None,
        }
    }
}

// ── writing ────────────────────────────────────────────────────────────────

/// Write the session header line.
pub(crate) fn write_header(out: &mut String) {
    // Writing to a String is infallible, so these `write!`s cannot fail.
    let _ = writeln!(out, "{HEADER_TAG} {VERSION}");
}

/// Open a named checkpoint block.
pub(crate) fn write_checkpoint_header(out: &mut String, name: &str) {
    let _ = writeln!(out, "{CHECKPOINT_TAG} {name}");
}

/// Serialize one session body: its probes, its node-name map (each pinned to a
/// representative part terminal), then the circuit as line-counted project text.
pub(crate) fn write_body(
    out: &mut String,
    circuit: &Circuit,
    nets: &HashMap<String, NetId>,
    probes: &[String],
) {
    for node in probes {
        let _ = writeln!(out, ".probe {node}");
    }
    // Sorted for a deterministic, diff-friendly file.
    let mut names: Vec<&String> = nets.keys().collect();
    names.sort();
    for name in names {
        let net = circuit.resolve(nets[name]);
        match representative(circuit, net) {
            Some((reference, idx)) => {
                let _ = writeln!(out, ".node {name} {reference} {idx}");
            }
            None => {
                let _ = writeln!(out, ".node {name} -");
            }
        }
    }
    let text = project::save(circuit);
    let lines: Vec<&str> = text.lines().collect();
    let _ = writeln!(out, ".circuit {}", lines.len());
    for line in lines {
        let _ = writeln!(out, "{line}");
    }
}

/// A part terminal that sits on `net`: the first `(reference, terminal index)`
/// found, or `None` if no part touches the net.
fn representative(circuit: &Circuit, net: NetId) -> Option<(String, usize)> {
    for part in circuit.parts() {
        for (idx, &t) in part.terminals.iter().enumerate() {
            if circuit.resolve(t) == net {
                return Some((part.reference.clone(), idx));
            }
        }
    }
    None
}

// ── reading ────────────────────────────────────────────────────────────────

/// Parse a session text into its live body and ordered checkpoints.
pub(crate) fn parse(text: &str) -> Result<Parsed, SessionError> {
    let mut lines = text.lines().peekable();

    // The header must be the first non-blank line.
    let header = loop {
        match lines.next() {
            Some(l) if l.trim().is_empty() => continue,
            Some(l) => break l.trim(),
            None => return Err(SessionError::MissingHeader),
        }
    };
    parse_header(header)?;

    let main = read_body(&mut lines)?;

    let mut checkpoints = Vec::new();
    // After the main body, the only thing that may follow is checkpoint blocks.
    while let Some(line) = lines.next() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let name = line
            .strip_prefix(CHECKPOINT_TAG)
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .ok_or_else(|| {
                SessionError::Malformed(format!("expected `{CHECKPOINT_TAG} <name>`, got {line:?}"))
            })?;
        let body = read_body(&mut lines)?;
        checkpoints.push((name.to_string(), body));
    }

    Ok(Parsed { main, checkpoints })
}

/// Validate the `!ee-session <version>` marker.
fn parse_header(text: &str) -> Result<(), SessionError> {
    let rest = text
        .strip_prefix(HEADER_TAG)
        .ok_or(SessionError::MissingHeader)?;
    match rest.trim() {
        VERSION => Ok(()),
        other => Err(SessionError::UnknownVersion(other.to_string())),
    }
}

/// A node name pinned either to a part terminal or to `-` (a fresh net).
enum NodePin {
    Terminal { reference: String, idx: usize },
    Fresh,
}

/// Read one body — probes and `.node` lines, then the `.circuit <N>` block — up
/// to (but not consuming) the next `!checkpoint` or end of input.
fn read_body<'a>(
    lines: &mut std::iter::Peekable<impl Iterator<Item = &'a str>>,
) -> Result<Body, SessionError> {
    let mut probes = Vec::new();
    let mut pins: Vec<(String, NodePin)> = Vec::new();

    // Metadata lines, until the `.circuit <N>` block opener.
    let count = loop {
        let Some(raw) = lines.next() else {
            return Err(SessionError::Malformed(
                "session body ended before its `.circuit` block".to_string(),
            ));
        };
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix(".circuit") {
            break parse_count(rest)?;
        } else if let Some(rest) = line.strip_prefix(".probe") {
            let node = rest.trim();
            if node.is_empty() {
                return Err(SessionError::Malformed("`.probe` needs a node".to_string()));
            }
            probes.push(node.to_string());
        } else if let Some(rest) = line.strip_prefix(".node") {
            pins.push(parse_node(rest)?);
        } else {
            return Err(SessionError::Malformed(format!(
                "unexpected line in session body: {line:?}"
            )));
        }
    };

    // Exactly `count` verbatim lines of native project text follow.
    let mut circuit_text = String::new();
    for _ in 0..count {
        match lines.next() {
            Some(l) => {
                circuit_text.push_str(l);
                circuit_text.push('\n');
            }
            None => {
                return Err(SessionError::Malformed(format!(
                    "`.circuit {count}` promised more lines than the file holds"
                )));
            }
        }
    }
    let mut circuit = project::load(&circuit_text).map_err(SessionError::Project)?;

    // Rebuild the node-name → net map against the freshly loaded circuit.
    let mut nets = HashMap::new();
    for (name, pin) in pins {
        let net = match pin {
            NodePin::Fresh => circuit.new_net(),
            NodePin::Terminal { reference, idx } => {
                let part = circuit.part_by_ref(&reference).ok_or_else(|| {
                    SessionError::Malformed(format!(
                        "node {name:?} pins to unknown part {reference:?}"
                    ))
                })?;
                let &t = part.terminals.get(idx).ok_or_else(|| {
                    SessionError::Malformed(format!(
                        "node {name:?} pins to terminal {idx} of {reference:?}, which has {} terminal(s)",
                        part.terminals.len()
                    ))
                })?;
                circuit.resolve(t)
            }
        };
        nets.insert(name, net);
    }

    Ok(Body {
        circuit,
        nets,
        probes,
    })
}

/// Parse the integer line count after `.circuit`.
fn parse_count(rest: &str) -> Result<usize, SessionError> {
    rest.trim().parse::<usize>().map_err(|_| {
        SessionError::Malformed(format!("`.circuit` needs a line count, got {rest:?}"))
    })
}

/// Parse a `.node <name> <ref> <idx>` or `.node <name> -` line.
fn parse_node(rest: &str) -> Result<(String, NodePin), SessionError> {
    let mut it = rest.split_whitespace();
    let name = it
        .next()
        .ok_or_else(|| SessionError::Malformed("`.node` needs a name".to_string()))?;
    let reference = it
        .next()
        .ok_or_else(|| SessionError::Malformed(format!("`.node {name}` needs a part or `-`")))?;
    if reference == "-" {
        return Ok((name.to_string(), NodePin::Fresh));
    }
    let idx_tok = it.next().ok_or_else(|| {
        SessionError::Malformed(format!("`.node {name} {reference}` needs a terminal index"))
    })?;
    let idx = idx_tok.parse::<usize>().map_err(|_| {
        SessionError::Malformed(format!(
            "`.node {name}` terminal index {idx_tok:?} is not a number"
        ))
    })?;
    Ok((
        name.to_string(),
        NodePin::Terminal {
            reference: reference.to_string(),
            idx,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_header_and_unknown_version_are_reported() {
        assert_eq!(parse("nope\n").unwrap_err(), SessionError::MissingHeader);
        assert_eq!(parse("").unwrap_err(), SessionError::MissingHeader);
        assert_eq!(
            parse("!ee-session v9\n.circuit 0\n").unwrap_err(),
            SessionError::UnknownVersion("v9".to_string())
        );
    }

    #[test]
    fn a_truncated_circuit_block_is_malformed() {
        // `.circuit 3` promises three lines but only one follows.
        let err = parse("!ee-session v1\n.circuit 3\n!ee-project v1\n").unwrap_err();
        assert!(matches!(err, SessionError::Malformed(_)), "{err:?}");
    }

    #[test]
    fn a_bad_embedded_circuit_surfaces_the_project_error() {
        // A one-line circuit block whose text lacks the project header.
        let err = parse("!ee-session v1\n.circuit 1\nR1 R 1k a b\n").unwrap_err();
        assert!(matches!(err, SessionError::Project(_)), "{err:?}");
    }

    #[test]
    fn an_empty_session_parses_to_an_empty_body() {
        // Header + an empty circuit block (just the project header line).
        let parsed = parse("!ee-session v1\n.circuit 1\n!ee-project v1\n").unwrap();
        assert!(parsed.main.probes.is_empty());
        assert!(parsed.main.nets.is_empty());
        assert!(parsed.checkpoints.is_empty());
        assert_eq!(parsed.main.circuit.parts().len(), 0);
    }
}
