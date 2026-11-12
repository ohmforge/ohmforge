//! SPICE netlist import/export (`CP-AF-M1.1`) — the industry-standard "deck"
//! front door, a sibling of [`crate::netlist`] and [`crate::project`].
//!
//! ```text
//! * a resistive divider          <- line 1 is always the title (per tradition)
//! V1 in 0 DC 10                   ; 10 V source from `in` to ground (node 0)
//! R1 in  mid 1k                   ; 1 kΩ
//! R2 mid 0   1k
//! .op                             <- dot-directives are parsed and ignored (M1)
//! .end
//! ```
//!
//! ## Subset supported at M1
//!
//! * **Elements**, keyed by the reference's first letter: `R` (ohms), `C`
//!   (farads), `L` (henries), `V` (volt source), `I` (current source). Each is a
//!   two-terminal device written `Ref node1 node2 value`; sources also accept an
//!   optional `DC`/`AC` keyword before the value (`Vxxx n+ n- [DC] value`).
//! * **Ground** is node `0` (the SPICE convention); `gnd`/`ground` are also
//!   accepted. Every other node name allocates a net, and reusing a name wires
//!   parts together (as in the friendly netlist).
//! * **Comments**: a line beginning with `*`, or anything after an inline `;`.
//! * **Title**: the first physical line, kept verbatim as the circuit name.
//! * **Dot-directives** (`.end`, `.ends`, `.tran`, `.op`, …) are recognized and
//!   skipped — they never cause an error, but nothing in them is modeled.
//!
//! ## `.model` cards + model-backed devices (`CP-AF-M4.1`)
//!
//! A `.model <name> <type> (<param>=<value> …)` card (types `D`, `NPN`, `PNP`,
//! `NMOS`, `PMOS`) is parsed into a [`SpiceModel`], and an element that trails a
//! model name is built with that model's parameters applied to the engine's
//! device model:
//!
//! ```text
//! D1 a b 1N4148          ; a diode     — terminals [anode, cathode]
//! Q1 c b e 2N3904        ; an NPN BJT  — terminals [collector, base, emitter]
//! M1 d g s 2N7000        ; an NMOS     — terminals [drain, gate, source]
//! .model 1N4148 D (IS=2.52n N=1.752)
//! ```
//!
//! Parameter names map SPICE → engine: diode `IS` → saturation current; BJT
//! `IS`/`BF`/`BR` → `is`/`βF`/`βR`; MOSFET `VTO`/`KP`/`LAMBDA` → `Vth`/`Kp`/`λ`.
//! Unknown parameters are ignored with a [`Note`] (never an error); a diode's
//! emission coefficient `N` is parsed but noted-unapplied (the engine's diode
//! has no per-part `n`). A model name absent from the deck falls back to the
//! built-in vendor [`models`] catalog (`1N4148`, `2N3904`, …). Line-folding
//! `+` continuations are joined before parsing. Use [`import_with_notes`] to see
//! the notes; [`import`] discards them.
//!
//! ## Device + controlled-source export (`CP-AF-M5.1`)
//!
//! [`export`] emits real SPICE for the model-backed devices and the four
//! controlled sources, so a round-trip through [`import`] preserves them:
//!
//! ```text
//! D1 a 0 Dmod_D1          ; a diode, with a matching `.model` card below
//! Q1 c b e Qmod_Q1        ; an NPN BJT
//! M1 d g s Mmod_M1        ; an NMOS
//! E1 o+ o- c+ c- 10       ; VCVS: `Ref out+ out- ctrl+ ctrl- gain`
//! G1 o+ o- c+ c- 0.01     ; VCCS: same shape, gain is a transconductance
//! H1 o+ o- Vsense 2000    ; CCVS: `Ref out+ out- Vctrl transresistance`
//! F1 o+ o- Vsense 0.5     ; CCCS: `Ref out+ out- Vctrl gain`
//! .model Dmod_D1 D (IS=2.52e-9)
//! .model Qmod_Q1 NPN (IS=2e-14 BF=250 BR=3)
//! .model Mmod_M1 NMOS (VTO=1.5 KP=0.002 LAMBDA=0.02)
//! ```
//!
//! Each `D`/`Q`/`M` element names a per-part `.model` card (`Dmod_<ref>`,
//! `Qmod_<ref>`, `Mmod_<ref>`) carrying the engine parameters the importer reads
//! back: a diode's `IS` (the part's value, in amperes), a BJT's `IS`/`BF`/`BR`,
//! and a MOSFET's `VTO`/`KP`/`LAMBDA` (`VTO` is re-signed by the polarity, so a
//! PMOS writes a negative threshold that `import` reads as a magnitude again).
//! The `H`/`F` control reference is the name of the sensing voltage source, kept
//! verbatim. Only [`PartKind::Other`] is still emitted as a skipped comment.
//!
//! ## Value suffixes (the `m`-vs-`meg` footgun)
//!
//! SPICE scale factors are **case-insensitive**, which is exactly where they bite:
//! `M` means *milli*, not mega — mega is spelled `MEG`. ohmforge-core's own parser uses
//! the opposite convention (`M` = mega), so this module parses SPICE magnitudes
//! itself rather than deferring to [`Value::parse`]. The recognized factors, any
//! trailing letters after them ignored (`10kohm` == `10k`):
//!
//! | suffix        | factor  | | suffix | factor |
//! |---------------|---------|-|--------|--------|
//! | `t`           | 1e12    | | `m`    | 1e-3   |
//! | `g`           | 1e9     | | `u`/`µ`| 1e-6   |
//! | `meg`         | 1e6     | | `n`    | 1e-9   |
//! | `k`           | 1e3     | | `p`    | 1e-12  |
//! | `mil`         | 25.4e-6 | | `f`    | 1e-15  |
//!
//! `meg` and `mil` are matched before the single-letter `m` so neither is read as
//! milli. Plain scientific notation (`1e-6`) is accepted too.
//!
//! Export writes a valid deck: a title line, one element line per part (ground as
//! `0`, every other net as `n<id>` via [`Circuit::resolve`]), and a trailing
//! `.end`. Zero dependencies beyond `ohmforge-core`; the parser is hand-written like its
//! siblings.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt::{self, Write as _};

use ohmforge_core::graph::{BjtModel, Circuit, DiodeModel, MosfetModel, NetId, PartKind, Polarity};
use ohmforge_core::units::Unit;
use ohmforge_core::value::Value;

pub mod models;

/// An import failure, tagged with the 1-based source line it occurred on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpiceError {
    pub line: usize,
    pub kind: ErrorKind,
}

/// What went wrong on a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    /// An element line had fewer than `Ref node1 node2 value` (4 fields).
    TooFewFields { got: usize },
    /// The reference designator was empty.
    EmptyReference,
    /// The reference's first letter is not a known element type.
    UnknownElement(char),
    /// A source line had no numeric value after its nodes (and `DC`/`AC` keyword).
    MissingValue,
    /// The value field could not be parsed as a SPICE magnitude.
    BadValue(String),
    /// Two parts share a reference designator.
    DuplicateReference(String),
    /// A `.model` card could not be parsed (missing name/type, unknown type, or a
    /// parameter that isn't `key=value`). The reason is a short human description.
    MalformedModel(String),
    /// A model-based device line (`D`/`Q`/`M`) wasn't shaped `Ref node… MODEL`.
    BadDeviceLine {
        /// The offending reference designator (e.g. `"D1"`).
        reference: String,
        /// How many whitespace fields the line actually had.
        got: usize,
        /// The expected shape, e.g. `"Ref N1 N2 MODEL"`.
        want: &'static str,
    },
    /// An element referenced a model name defined neither in the deck's `.model`
    /// cards nor the built-in [`models`] catalog.
    UnknownModel(String),
    /// An element's leading letter and its model's type disagree (e.g. a `Q…`
    /// line naming a diode model).
    ModelTypeMismatch {
        /// The offending reference designator.
        reference: String,
        /// The model name that was referenced.
        model: String,
        /// The device type the element's letter requires (e.g. `"NPN/PNP"`).
        expected: &'static str,
    },
}

impl fmt::Display for SpiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.kind)
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorKind::TooFewFields { got } => {
                write!(
                    f,
                    "expected at least 4 fields (Ref N1 N2 VALUE), found {got}"
                )
            }
            ErrorKind::EmptyReference => write!(f, "empty element reference"),
            ErrorKind::UnknownElement(c) => {
                write!(
                    f,
                    "unknown element type '{c}' (expected R, C, L, V, I, D, Q, M, E, G, H, or F)"
                )
            }
            ErrorKind::MissingValue => write!(f, "missing value after element nodes"),
            ErrorKind::BadValue(tok) => write!(f, "bad value {tok:?}"),
            ErrorKind::DuplicateReference(r) => write!(f, "duplicate element reference {r:?}"),
            ErrorKind::MalformedModel(why) => write!(f, "malformed .model card: {why}"),
            ErrorKind::BadDeviceLine {
                reference,
                got,
                want,
            } => write!(f, "{reference}: expected `{want}`, found {got} field(s)"),
            ErrorKind::UnknownModel(name) => {
                write!(
                    f,
                    "unknown model {name:?} (not in the deck or the built-in catalog)"
                )
            }
            ErrorKind::ModelTypeMismatch {
                reference,
                model,
                expected,
            } => write!(
                f,
                "{reference} references model {model:?}, but that is not a {expected} model"
            ),
        }
    }
}

impl Error for SpiceError {}

/// A non-fatal remark from [`import_with_notes`]: a `.model` parameter that was
/// recognized but could not be applied to the engine's model, or an unknown
/// parameter that was ignored. Tagged with the 1-based source line like
/// [`SpiceError`], so a caller can surface it the same way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// The 1-based line of the `.model` card the remark came from.
    pub line: usize,
    /// A short human-readable description.
    pub message: String,
}

impl fmt::Display for Note {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

/// The device type named on a SPICE `.model` card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelType {
    /// A junction diode (`D`).
    Diode,
    /// An NPN bipolar transistor (`NPN`).
    Npn,
    /// A PNP bipolar transistor (`PNP`).
    Pnp,
    /// An n-channel MOSFET (`NMOS`).
    Nmos,
    /// A p-channel MOSFET (`PMOS`).
    Pmos,
}

impl ModelType {
    /// Parse the type token of a `.model` card (case-insensitive).
    fn parse(tok: &str) -> Option<ModelType> {
        Some(match tok.to_ascii_uppercase().as_str() {
            "D" => ModelType::Diode,
            "NPN" => ModelType::Npn,
            "PNP" => ModelType::Pnp,
            "NMOS" => ModelType::Nmos,
            "PMOS" => ModelType::Pmos,
            _ => return None,
        })
    }

    /// The canonical SPICE spelling of this type.
    pub fn label(self) -> &'static str {
        match self {
            ModelType::Diode => "D",
            ModelType::Npn => "NPN",
            ModelType::Pnp => "PNP",
            ModelType::Nmos => "NMOS",
            ModelType::Pmos => "PMOS",
        }
    }
}

/// A parsed SPICE `.model` card: its name, device type, and ordered parameters.
///
/// Parameter keys are upper-cased (SPICE is case-insensitive) and magnitudes are
/// already scaled to base SI (so `IS=2.52n` is stored as `2.52e-9`). Order is
/// preserved for deterministic iteration. Build one from a deck via
/// [`import_with_notes`], or fetch a vendor part from [`models::lookup`].
#[derive(Debug, Clone, PartialEq)]
pub struct SpiceModel {
    /// The model name, as written (e.g. `"1N4148"`).
    pub name: String,
    /// The device type (`D`/`NPN`/`PNP`/`NMOS`/`PMOS`).
    pub model_type: ModelType,
    /// The `key=value` parameters in source order (keys upper-cased, values SI).
    pub params: Vec<(String, f64)>,
}

impl SpiceModel {
    /// The value of a parameter by name (case-insensitive), or `None` if absent.
    pub fn param(&self, key: &str) -> Option<f64> {
        self.params
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| *v)
    }
}

/// Import a SPICE deck into a [`Circuit`] ready for the engine. See the module
/// docs for the supported subset (R/C/L/V/I plus `.model`-backed `D`/`Q`/`M`).
///
/// This discards the [`Note`]s about ignored/unsupported model parameters; call
/// [`import_with_notes`] to see them.
pub fn import(src: &str) -> Result<Circuit, SpiceError> {
    import_with_notes(src).map(|(circuit, _)| circuit)
}

/// Like [`import`], but also returns the [`Note`]s raised while applying
/// `.model` parameters — an unknown parameter that was ignored, or one the
/// engine's model can't represent yet (e.g. a diode emission coefficient `N`).
///
/// A `.model <name> <type> (<param>=<value> …)` card is parsed into a
/// [`SpiceModel`], and an element that trails a model name (`D1 a b 1N4148`,
/// `Q1 c b e 2N3904`, `M1 d g s 2N7000`) is built with that model's parameters
/// mapped onto the engine's device model. Models defined anywhere in the deck
/// are visible to every element (forward references are fine); a name absent
/// from the deck falls back to the built-in [`models`] catalog.
pub fn import_with_notes(src: &str) -> Result<(Circuit, Vec<Note>), SpiceError> {
    let mut circuit = Circuit::new("");
    let mut nets: HashMap<String, NetId> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut notes: Vec<Note> = Vec::new();

    let logical = logical_lines(src, &mut circuit);

    // Pass 1: collect every `.model` card so elements can reference a model
    // declared anywhere in the deck (SPICE allows forward references).
    let mut deck_models: HashMap<String, SpiceModel> = HashMap::new();
    for &(line, ref text) in &logical {
        if first_word_is_dot_model(text) {
            let model = parse_model(text, line)?;
            deck_models.insert(model.name.to_ascii_uppercase(), model);
        }
    }

    // Pass 2: build elements, resolving each device's trailing model name.
    let mut ctx = Ctx {
        circuit: &mut circuit,
        nets: &mut nets,
        seen: &mut seen,
        deck_models: &deck_models,
        notes: &mut notes,
    };
    for &(line, ref text) in &logical {
        if text.starts_with('.') {
            continue; // `.model` (handled above) and every other dot-directive
        }
        parse_element(&mut ctx, text, line)?;
    }

    Ok((circuit, notes))
}

/// The mutable state threaded through element parsing: the growing circuit, the
/// node-name → net map, the set of claimed references, the deck's `.model`
/// cards, and the accumulating [`Note`]s.
struct Ctx<'a> {
    circuit: &'a mut Circuit,
    nets: &'a mut HashMap<String, NetId>,
    seen: &'a mut HashSet<String>,
    deck_models: &'a HashMap<String, SpiceModel>,
    notes: &'a mut Vec<Note>,
}

/// Fold a deck into comment-free logical lines paired with their 1-based start
/// line, joining SPICE `+` continuation lines onto the line they extend. The
/// first physical line is consumed as the circuit title (SPICE tradition) and
/// written straight into `circuit.name`.
fn logical_lines(src: &str, circuit: &mut Circuit) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut current: Option<(usize, String)> = None;

    for (i, raw) in src.lines().enumerate() {
        let line = i + 1;
        if i == 0 {
            // The title is kept verbatim (comment markers and all), never joined.
            circuit.name = raw.trim().to_string();
            continue;
        }
        // A `*` full-line comment is skipped without breaking a continuation.
        if raw.trim_start().starts_with('*') {
            continue;
        }
        let text = strip_comment(raw).trim();
        if text.is_empty() {
            continue;
        }
        if let Some(rest) = text.strip_prefix('+') {
            // Continuation: append to the line being built (or stand alone if
            // there is nothing to continue, which a malformed deck might do).
            match current.as_mut() {
                Some((_, buf)) => {
                    buf.push(' ');
                    buf.push_str(rest.trim());
                }
                None => current = Some((line, rest.trim().to_string())),
            }
            continue;
        }
        if let Some(done) = current.take() {
            out.push(done);
        }
        current = Some((line, text.to_string()));
    }
    if let Some(done) = current {
        out.push(done);
    }
    out
}

/// Whether a logical line's first whitespace token is `.model` (case-insensitive).
fn first_word_is_dot_model(text: &str) -> bool {
    text.split_whitespace()
        .next()
        .is_some_and(|w| w.eq_ignore_ascii_case(".model"))
}

fn err(line: usize, kind: ErrorKind) -> SpiceError {
    SpiceError { line, kind }
}

/// Everything before the first inline `;` comment marker.
fn strip_comment(line: &str) -> &str {
    match line.find(';') {
        Some(i) => &line[..i],
        None => line,
    }
}

/// What a reference's leading letter says an element is.
enum ElemClass {
    /// A two-terminal part whose value field carries a magnitude (`R`/`C`/`L`/
    /// `V`/`I`), with the [`Unit`] its magnitude is in.
    Passive(PartKind, Unit),
    /// A diode (`D`): `Ref anode cathode MODEL`.
    Diode,
    /// A bipolar transistor (`Q`): `Ref collector base emitter MODEL`.
    Bjt,
    /// A MOSFET (`M`): `Ref drain gate source [bulk] MODEL`.
    Mosfet,
    /// A voltage-controlled voltage source (`E`): `Ref out+ out- ctrl+ ctrl- GAIN`.
    Vcvs,
    /// A voltage-controlled current source (`G`): `Ref out+ out- ctrl+ ctrl- GAIN`.
    Vccs,
    /// A current-controlled voltage source (`H`): `Ref out+ out- VCTRL GAIN`.
    Ccvs,
    /// A current-controlled current source (`F`): `Ref out+ out- VCTRL GAIN`.
    Cccs,
}

/// Parse one element line and add it to the circuit. Passives (`R/C/L/V/I`)
/// carry a magnitude; model-based devices (`D/Q/M`) carry a trailing model name
/// resolved against `deck_models` then the built-in [`models`] catalog.
fn parse_element(ctx: &mut Ctx, text: &str, line: usize) -> Result<(), SpiceError> {
    let fields: Vec<&str> = text.split_whitespace().collect();
    let reference = *fields.first().unwrap_or(&"");
    let class = classify(reference).map_err(|k| err(line, k))?;

    match class {
        ElemClass::Passive(kind, unit) => {
            if fields.len() < 4 {
                return Err(err(line, ErrorKind::TooFewFields { got: fields.len() }));
            }
            claim_reference(ctx.seen, reference, line)?;
            let a = net_for(ctx.circuit, ctx.nets, fields[1]);
            let b = net_for(ctx.circuit, ctx.nets, fields[2]);
            // Everything after the two nodes is the value, past an optional
            // `DC`/`AC` keyword that sources may carry (`V1 n+ n- DC 10`).
            let mut rest = &fields[3..];
            if let Some(first) = rest.first() {
                if first.eq_ignore_ascii_case("DC") || first.eq_ignore_ascii_case("AC") {
                    rest = &rest[1..];
                }
            }
            let value_tok = rest
                .first()
                .ok_or_else(|| err(line, ErrorKind::MissingValue))?;
            let magnitude = spice_magnitude(value_tok)
                .ok_or_else(|| err(line, ErrorKind::BadValue(value_tok.to_string())))?;
            ctx.circuit.add_part(
                reference,
                kind,
                Some(Value::new(magnitude, unit)),
                vec![a, b],
            );
            Ok(())
        }
        // Diode is 2-terminal, BJT/MOSFET 3-terminal (a MOSFET may add a bulk node).
        ElemClass::Diode => parse_device(ctx, &fields, line, 2),
        ElemClass::Bjt | ElemClass::Mosfet => parse_device(ctx, &fields, line, 3),
        ElemClass::Vcvs => parse_vcontrolled(ctx, &fields, line, PartKind::Vcvs),
        ElemClass::Vccs => parse_vcontrolled(ctx, &fields, line, PartKind::Vccs),
        ElemClass::Ccvs => parse_ccontrolled(ctx, &fields, line, true),
        ElemClass::Cccs => parse_ccontrolled(ctx, &fields, line, false),
    }
}

/// Parse a voltage-controlled source line (`E`/`G`):
/// `Ref out+ out- ctrl+ ctrl- GAIN` — four nodes then the gain. `make` wraps the
/// parsed gain into [`PartKind::Vcvs`] or [`PartKind::Vccs`].
fn parse_vcontrolled(
    ctx: &mut Ctx,
    fields: &[&str],
    line: usize,
    make: fn(f64) -> PartKind,
) -> Result<(), SpiceError> {
    let reference = fields[0];
    if fields.len() != 6 {
        return Err(err(
            line,
            ErrorKind::BadDeviceLine {
                reference: reference.to_string(),
                got: fields.len(),
                want: "Ref N+ N- NC+ NC- GAIN",
            },
        ));
    }
    claim_reference(ctx.seen, reference, line)?;
    let terminals: Vec<NetId> = fields[1..5]
        .iter()
        .map(|n| net_for(ctx.circuit, ctx.nets, n))
        .collect();
    let gain = spice_magnitude(fields[5])
        .ok_or_else(|| err(line, ErrorKind::BadValue(fields[5].to_string())))?;
    ctx.circuit.add_part(reference, make(gain), None, terminals);
    Ok(())
}

/// Parse a current-controlled source line (`H`/`F`): `Ref out+ out- VCTRL GAIN` —
/// two output nodes, the name of the voltage source whose branch current controls
/// it (kept verbatim), then the gain. `is_ccvs` picks [`PartKind::Ccvs`] (a
/// transresistance) vs. [`PartKind::Cccs`] (a current gain).
fn parse_ccontrolled(
    ctx: &mut Ctx,
    fields: &[&str],
    line: usize,
    is_ccvs: bool,
) -> Result<(), SpiceError> {
    let reference = fields[0];
    if fields.len() != 5 {
        return Err(err(
            line,
            ErrorKind::BadDeviceLine {
                reference: reference.to_string(),
                got: fields.len(),
                want: "Ref N+ N- VCTRL GAIN",
            },
        ));
    }
    claim_reference(ctx.seen, reference, line)?;
    let terminals: Vec<NetId> = fields[1..3]
        .iter()
        .map(|n| net_for(ctx.circuit, ctx.nets, n))
        .collect();
    let ctrl = fields[3].to_string();
    let gain = spice_magnitude(fields[4])
        .ok_or_else(|| err(line, ErrorKind::BadValue(fields[4].to_string())))?;
    let kind = if is_ccvs {
        PartKind::Ccvs(gain, ctrl)
    } else {
        PartKind::Cccs(gain, ctrl)
    };
    ctx.circuit.add_part(reference, kind, None, terminals);
    Ok(())
}

/// Reserve a reference designator, erroring if it was already used.
fn claim_reference(
    seen: &mut HashSet<String>,
    reference: &str,
    line: usize,
) -> Result<(), SpiceError> {
    if !seen.insert(reference.to_string()) {
        return Err(err(
            line,
            ErrorKind::DuplicateReference(reference.to_string()),
        ));
    }
    Ok(())
}

/// Parse a model-based device line (`D`/`Q`/`M`): `Ref node… MODEL`. `min_nodes`
/// is the engine's terminal count (2 diode, 3 BJT/MOSFET); a MOSFET may also
/// carry a 4th bulk node, which is noted and dropped (the engine ties body to
/// source). The trailing field is always the model name.
fn parse_device(
    ctx: &mut Ctx,
    fields: &[&str],
    line: usize,
    min_nodes: usize,
) -> Result<(), SpiceError> {
    let reference = fields[0];
    // A MOSFET accepts an optional 4th (bulk) node; every other device is exact.
    let is_mosfet = reference.starts_with(['M', 'm']);
    let max_nodes = if is_mosfet { min_nodes + 1 } else { min_nodes };
    // fields = reference + nodes + model name.
    let node_count = fields.len().saturating_sub(2);
    let shape = device_shape(reference, min_nodes);
    if fields.len() < 2 || node_count < min_nodes || node_count > max_nodes {
        return Err(err(
            line,
            ErrorKind::BadDeviceLine {
                reference: reference.to_string(),
                got: fields.len(),
                want: shape,
            },
        ));
    }
    claim_reference(ctx.seen, reference, line)?;

    let model_name = fields[fields.len() - 1];
    let model = resolve_model(model_name, ctx.deck_models)
        .ok_or_else(|| err(line, ErrorKind::UnknownModel(model_name.to_string())))?;

    // The engine's MOSFET is 3-terminal (body tied to source); a 4th node is a
    // bulk terminal we can't model separately, so note it and drop it.
    let used_nodes = &fields[1..1 + min_nodes];
    if node_count > min_nodes {
        ctx.notes.push(Note {
            line,
            message: format!(
                "{reference}: bulk node {:?} ignored — the engine ties the MOSFET body to its source",
                fields[1 + min_nodes]
            ),
        });
    }
    let terminals: Vec<NetId> = used_nodes
        .iter()
        .map(|n| net_for(ctx.circuit, ctx.nets, n))
        .collect();

    let (kind, value) =
        build_device(reference, &model, ctx.notes, line).map_err(|k| err(line, k))?;
    ctx.circuit.add_part(reference, kind, value, terminals);
    Ok(())
}

/// The expected-shape string for a device line's error message.
fn device_shape(reference: &str, min_nodes: usize) -> &'static str {
    match (
        reference.chars().next().map(|c| c.to_ascii_uppercase()),
        min_nodes,
    ) {
        (Some('D'), _) => "Ref N+ N- MODEL",
        (Some('Q'), _) => "Ref NC NB NE MODEL",
        (Some('M'), _) => "Ref ND NG NS [NB] MODEL",
        _ => "Ref node… MODEL",
    }
}

/// Find a model by name (case-insensitive): the deck's `.model` cards first,
/// then the built-in vendor [`models`] catalog.
fn resolve_model(name: &str, deck_models: &HashMap<String, SpiceModel>) -> Option<SpiceModel> {
    deck_models
        .get(&name.to_ascii_uppercase())
        .cloned()
        .or_else(|| models::lookup(name))
}

/// Map a resolved [`SpiceModel`] onto the engine's device model for a given
/// reference, applying the parameters it understands and pushing a [`Note`] for
/// every parameter it ignores or can't represent. Errors only on a type
/// mismatch (e.g. a `Q…` line naming a diode model).
fn build_device(
    reference: &str,
    model: &SpiceModel,
    notes: &mut Vec<Note>,
    line: usize,
) -> Result<(PartKind, Option<Value>), ErrorKind> {
    let c = reference
        .chars()
        .next()
        .ok_or(ErrorKind::EmptyReference)?
        .to_ascii_uppercase();
    match c {
        'D' => {
            if model.model_type != ModelType::Diode {
                return Err(mismatch(reference, model, "diode (D)"));
            }
            Ok((PartKind::Diode(apply_diode(model, notes, line)), None))
        }
        'Q' => {
            let polarity = match model.model_type {
                ModelType::Npn => Polarity::N,
                ModelType::Pnp => Polarity::P,
                _ => return Err(mismatch(reference, model, "NPN/PNP")),
            };
            Ok((PartKind::Bjt(polarity, apply_bjt(model, notes, line)), None))
        }
        'M' => {
            let polarity = match model.model_type {
                ModelType::Nmos => Polarity::N,
                ModelType::Pmos => Polarity::P,
                _ => return Err(mismatch(reference, model, "NMOS/PMOS")),
            };
            Ok((
                PartKind::Mosfet(polarity, apply_mosfet(model, notes, line)),
                None,
            ))
        }
        other => Err(ErrorKind::UnknownElement(other)),
    }
}

fn mismatch(reference: &str, model: &SpiceModel, expected: &'static str) -> ErrorKind {
    ErrorKind::ModelTypeMismatch {
        reference: reference.to_string(),
        model: model.name.clone(),
        expected,
    }
}

/// Note that a model parameter was ignored (not part of the engine's model).
fn note_ignored(notes: &mut Vec<Note>, line: usize, model: &str, key: &str) {
    notes.push(Note {
        line,
        message: format!("model {model}: parameter {key} ignored (not modeled by the engine)"),
    });
}

/// Diode: `IS` → the engine's saturation current (carried as the part's value in
/// amperes). `N` is parsed but can't be applied — the engine's diode has no
/// per-part emission coefficient and uses `n = 1` — so it is noted. Everything
/// else is noted as ignored.
fn apply_diode(model: &SpiceModel, notes: &mut Vec<Note>, line: usize) -> DiodeModel {
    let mut m = DiodeModel::default();
    for (key, val) in &model.params {
        match key.as_str() {
            "IS" if *val > 0.0 && val.is_finite() => m.is = *val,
            "N" if *val > 0.0 && val.is_finite() => m.n = *val,
            "RS" if *val >= 0.0 && val.is_finite() => m.rs = *val,
            "IS" | "N" | "RS" => notes.push(Note {
                line,
                message: format!(
                    "model {}: {key}={val} is not usable; left at default",
                    model.name
                ),
            }),
            other => note_ignored(notes, line, &model.name, other),
        }
    }
    m
}

/// BJT: `IS`/`BF`/`BR` → the Ebers–Moll `is`/`βF`/`βR`. Other parameters (`VAF`,
/// `RB`, junction capacitances, …) are noted as ignored; `VT` is temperature-
/// derived in the engine and left at its default.
fn apply_bjt(model: &SpiceModel, notes: &mut Vec<Note>, line: usize) -> BjtModel {
    let mut m = BjtModel::default();
    for (key, val) in &model.params {
        match key.as_str() {
            "IS" if *val > 0.0 && val.is_finite() => m.is = *val,
            "BF" if val.is_finite() => m.bf = *val,
            "BR" if val.is_finite() => m.br = *val,
            "IS" | "BF" | "BR" => notes.push(Note {
                line,
                message: format!(
                    "model {}: {key}={val} is not usable; left at default",
                    model.name
                ),
            }),
            other => note_ignored(notes, line, &model.name, other),
        }
    }
    m
}

/// MOSFET: `VTO` → threshold `V_th` (magnitude; the polarity sets the sign),
/// `KP` → transconductance `K`, `LAMBDA` → channel-length modulation `λ`. Other
/// parameters (`GAMMA`, `PHI`, `W`, `L`, …) are noted as ignored.
fn apply_mosfet(model: &SpiceModel, notes: &mut Vec<Note>, line: usize) -> MosfetModel {
    let mut m = MosfetModel::default();
    for (key, val) in &model.params {
        match key.as_str() {
            // VTO is signed in SPICE; the engine stores the magnitude and lets
            // the polarity carry the sign, so take the absolute value.
            "VTO" if val.is_finite() => m.vth = val.abs(),
            "KP" if *val > 0.0 && val.is_finite() => m.kp = *val,
            "LAMBDA" if val.is_finite() => m.lambda = *val,
            "VTO" | "KP" | "LAMBDA" => notes.push(Note {
                line,
                message: format!(
                    "model {}: {key}={val} is not usable; left at default",
                    model.name
                ),
            }),
            other => note_ignored(notes, line, &model.name, other),
        }
    }
    m
}

/// Parse a `.model <name> <type> (<key>=<value> …)` card. Parameters may be
/// wrapped in parentheses, separated by whitespace and/or commas, with optional
/// spaces around `=`; magnitudes accept the same SPICE suffixes as element
/// values. Returns a [`SpiceModel`] or a [`ErrorKind::MalformedModel`].
fn parse_model(text: &str, line: usize) -> Result<SpiceModel, SpiceError> {
    // Drop the leading `.model` keyword (already matched case-insensitively).
    let body = &text[".model".len()..];
    // Normalize punctuation so a tokenizer sees `name type key = value …`
    // regardless of parens/commas or spacing around `=` (`D(IS=1e-14)` too).
    let normalized = body.replace(['(', ')', ','], " ").replace('=', " = ");
    let toks: Vec<&str> = normalized.split_whitespace().collect();
    if toks.len() < 2 {
        return Err(err(
            line,
            ErrorKind::MalformedModel("expected `.model NAME TYPE (…)`".into()),
        ));
    }
    let name = toks[0].to_string();
    let model_type = ModelType::parse(toks[1]).ok_or_else(|| {
        err(
            line,
            ErrorKind::MalformedModel(format!(
                "unknown model type {:?} (expected D, NPN, PNP, NMOS, or PMOS)",
                toks[1]
            )),
        )
    })?;

    // The rest is a stream of `key = value` triples.
    let mut params = Vec::new();
    let mut i = 2;
    while i < toks.len() {
        let key = toks[i];
        if i + 2 >= toks.len() || toks[i + 1] != "=" {
            return Err(err(
                line,
                ErrorKind::MalformedModel(format!("parameter {key:?} is not `key=value`")),
            ));
        }
        let value_tok = toks[i + 2];
        let magnitude = spice_magnitude(value_tok).ok_or_else(|| {
            err(
                line,
                ErrorKind::MalformedModel(format!("parameter {key} has a bad value {value_tok:?}")),
            )
        })?;
        params.push((key.to_ascii_uppercase(), magnitude));
        i += 3;
    }

    Ok(SpiceModel {
        name,
        model_type,
        params,
    })
}

/// Map a reference's leading letter to its element class.
fn classify(reference: &str) -> Result<ElemClass, ErrorKind> {
    let c = reference.chars().next().ok_or(ErrorKind::EmptyReference)?;
    Ok(match c.to_ascii_uppercase() {
        'R' => ElemClass::Passive(PartKind::Resistor, Unit::Ohm),
        'C' => ElemClass::Passive(PartKind::Capacitor, Unit::Farad),
        'L' => ElemClass::Passive(PartKind::Inductor, Unit::Henry),
        'V' => ElemClass::Passive(PartKind::VoltageSource, Unit::Volt),
        'I' => ElemClass::Passive(PartKind::CurrentSource, Unit::Ampere),
        'D' => ElemClass::Diode,
        'Q' => ElemClass::Bjt,
        'M' => ElemClass::Mosfet,
        'E' => ElemClass::Vcvs,
        'G' => ElemClass::Vccs,
        'H' => ElemClass::Ccvs,
        'F' => ElemClass::Cccs,
        other => return Err(ErrorKind::UnknownElement(other)),
    })
}

/// The net for a node name, allocating a fresh one the first time it is seen.
/// `0`/`gnd`/`ground` always resolve to [`NetId::GROUND`].
fn net_for(circuit: &mut Circuit, nets: &mut HashMap<String, NetId>, name: &str) -> NetId {
    if is_ground(name) {
        return NetId::GROUND;
    }
    if let Some(&id) = nets.get(name) {
        return id;
    }
    let id = circuit.new_net();
    nets.insert(name.to_string(), id);
    id
}

fn is_ground(name: &str) -> bool {
    matches!(name.to_ascii_lowercase().as_str(), "0" | "gnd" | "ground")
}

/// Parse a SPICE magnitude: a number (optionally with an exponent) followed by an
/// optional case-insensitive scale factor, with any trailing unit text ignored
/// (`10kohm` == `10k`). Returns the base-SI magnitude, or `None` if not numeric.
fn spice_magnitude(tok: &str) -> Option<f64> {
    let (number, suffix) = split_number(tok);
    let mantissa: f64 = number.parse().ok()?;
    Some(mantissa * scale_factor(suffix))
}

/// Split a token into its leading numeric text (sign, digits, decimal point, and
/// an optional `e`/`E` exponent) and the trailing suffix (the scale factor plus
/// any ignored unit letters).
fn split_number(s: &str) -> (&str, &str) {
    let bytes = s.as_bytes();
    let mut i = 0;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
        i += 1;
    }
    // An exponent is only part of the number if it is actually `e[sign]digits`;
    // otherwise the `e` belongs to a trailing suffix.
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        let mut j = i + 1;
        if j < bytes.len() && (bytes[j] == b'+' || bytes[j] == b'-') {
            j += 1;
        }
        if j < bytes.len() && bytes[j].is_ascii_digit() {
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            i = j;
        }
    }
    (&s[..i], &s[i..])
}

/// The SPICE scale factor named by a suffix (1.0 when it names none). `meg` and
/// `mil` are checked before the single-letter `m` (milli) so neither is misread.
fn scale_factor(suffix: &str) -> f64 {
    let low = suffix.to_ascii_lowercase();
    if low.starts_with("meg") {
        return 1e6;
    }
    if low.starts_with("mil") {
        return 25.4e-6;
    }
    match suffix.chars().next() {
        Some('t') | Some('T') => 1e12,
        Some('g') | Some('G') => 1e9,
        Some('k') | Some('K') => 1e3,
        Some('m') | Some('M') => 1e-3,
        Some('u') | Some('U') | Some('µ') => 1e-6,
        Some('n') | Some('N') => 1e-9,
        Some('p') | Some('P') => 1e-12,
        Some('f') | Some('F') => 1e-15,
        _ => 1.0,
    }
}

/// Export a [`Circuit`] as a SPICE deck. Round-trips through [`import`]: each
/// part's reference/kind/value, its model/gain parameters, and the shared-net
/// topology survive (see the module docs). Model-backed devices (`D`/`Q`/`M`)
/// also emit a matching `.model` card; only [`PartKind::Other`] is emitted as a
/// comment rather than a bogus element.
pub fn export(circuit: &Circuit) -> String {
    let mut out = String::new();
    // Writing to a String is infallible, so these `write!`s cannot fail.
    // The title line is mandatory in SPICE; fall back to a stable default so the
    // deck stays valid even for an unnamed circuit.
    let title = if circuit.name.is_empty() {
        "ohmforge circuit"
    } else {
        circuit.name.as_str()
    };
    let _ = writeln!(out, "{title}");

    // `.model` cards accumulate here and flush beneath the element lines, so a
    // reader sees the netlist first and its model library after.
    let mut models = String::new();

    for part in circuit.parts() {
        let reference = &part.reference;
        match &part.kind {
            PartKind::Resistor
            | PartKind::Capacitor
            | PartKind::Inductor
            | PartKind::VoltageSource
            | PartKind::CurrentSource => {
                write_nodes(&mut out, reference, &part.terminals, circuit);
                let _ = writeln!(out, " {}", export_value(part.value.as_ref()));
            }
            PartKind::Diode(m) => {
                let model = format!("Dmod_{reference}");
                write_nodes(&mut out, reference, &part.terminals, circuit);
                let _ = writeln!(out, " {model}");
                emit_diode_model(&mut models, &model, m);
            }
            PartKind::Bjt(polarity, m) => {
                let model = format!("Qmod_{reference}");
                write_nodes(&mut out, reference, &part.terminals, circuit);
                let _ = writeln!(out, " {model}");
                let ty = match polarity {
                    Polarity::N => "NPN",
                    Polarity::P => "PNP",
                };
                let _ = writeln!(
                    models,
                    ".model {model} {ty} (IS={} BF={} BR={})",
                    m.is, m.bf, m.br
                );
            }
            PartKind::Mosfet(polarity, m) => {
                let model = format!("Mmod_{reference}");
                write_nodes(&mut out, reference, &part.terminals, circuit);
                let _ = writeln!(out, " {model}");
                let ty = match polarity {
                    Polarity::N => "NMOS",
                    Polarity::P => "PMOS",
                };
                // VTO is signed in SPICE; the engine keeps the magnitude and lets
                // the polarity carry the sign, so re-apply it on the way out.
                let vto = m.vth * polarity.sign();
                let _ = writeln!(
                    models,
                    ".model {model} {ty} (VTO={} KP={} LAMBDA={})",
                    vto, m.kp, m.lambda
                );
            }
            // E/G: `Ref out+ out- ctrl+ ctrl- gain` (four nodes then the gain).
            PartKind::Vcvs(gain) | PartKind::Vccs(gain) => {
                write_nodes(&mut out, reference, &part.terminals, circuit);
                let _ = writeln!(out, " {gain}");
            }
            // H/F: `Ref out+ out- Vctrl gain` (two nodes, the sense source, gain).
            PartKind::Ccvs(gain, ctrl) | PartKind::Cccs(gain, ctrl) => {
                write_nodes(&mut out, reference, &part.terminals, circuit);
                let _ = writeln!(out, " {ctrl} {gain}");
            }
            PartKind::Other(_) => {
                let _ = writeln!(out, "* {reference} skipped (unsupported in SPICE)");
            }
        }
    }

    out.push_str(&models);
    let _ = writeln!(out, ".end");
    out
}

/// Write an element's reference and its resolved terminal nodes (ground as `0`,
/// every other net as `n<id>`), with no trailing newline so the caller can append
/// the element's value or model name.
fn write_nodes(out: &mut String, reference: &str, terminals: &[NetId], circuit: &Circuit) {
    let _ = write!(out, "{reference}");
    for &terminal in terminals {
        let _ = write!(out, " {}", node_name(circuit.resolve(terminal)));
    }
}

/// Emit a diode's `.model` card from its [`DiodeModel`]: the saturation current
/// `IS` (amperes), the emission coefficient `N`, and the series resistance `RS`
/// (ohms). A round-trip through [`import`] recovers the same model.
fn emit_diode_model(out: &mut String, name: &str, model: &DiodeModel) {
    let _ = writeln!(
        out,
        ".model {name} D (IS={} N={} RS={})",
        model.is, model.n, model.rs
    );
}

/// The node name for a resolved net: `0` for ground, else `n<id>`.
fn node_name(net: NetId) -> String {
    if net == NetId::GROUND {
        "0".to_string()
    } else {
        format!("n{}", net.0)
    }
}

/// The value field for an element: the base-SI magnitude via `f64`'s default
/// `Display` (the shortest string that parses back to the same bits), so a plain
/// re-[`import`] recovers it exactly. `None` is written as a neutral `0`.
fn export_value(value: Option<&Value>) -> String {
    match value {
        Some(v) => format!("{}", v.magnitude),
        None => "0".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIVIDER: &str = "\
divider demo
* a 10 V source across two equal resistors
V1 in 0 DC 10
R1 in  mid 1k    ; the top leg
R2 mid 0   1k
.op
.end
";

    #[test]
    fn imports_kinds_values_and_title() {
        let c = import(DIVIDER).unwrap();
        assert_eq!(c.name, "divider demo");
        assert_eq!(c.parts().len(), 3);

        let v1 = c.part_by_ref("V1").unwrap();
        assert!(matches!(v1.kind, PartKind::VoltageSource));
        assert_eq!(v1.value.as_ref().unwrap().magnitude, 10.0);

        let r1 = c.part_by_ref("R1").unwrap();
        assert!(matches!(r1.kind, PartKind::Resistor));
        assert_eq!(r1.value.as_ref().unwrap().magnitude, 1000.0);
    }

    #[test]
    fn shared_node_names_wire_parts_together() {
        let c = import(DIVIDER).unwrap();
        let v1 = c.part_by_ref("V1").unwrap();
        let r1 = c.part_by_ref("R1").unwrap();
        let r2 = c.part_by_ref("R2").unwrap();
        // `in` is shared by V1 and R1; `mid` by R1 and R2.
        assert_eq!(v1.terminals[0], r1.terminals[0], "node `in` is one net");
        assert_eq!(r1.terminals[1], r2.terminals[0], "node `mid` is one net");
        // Node `0` collapses to ground everywhere.
        assert_eq!(v1.terminals[1], NetId::GROUND);
        assert_eq!(r2.terminals[1], NetId::GROUND);
    }

    #[test]
    fn ground_aliases_are_all_ground() {
        let c = import("title\nR1 a 0 1k\nR2 a gnd 1k\nR3 a ground 1k\n.end").unwrap();
        for r in ["R1", "R2", "R3"] {
            assert_eq!(c.part_by_ref(r).unwrap().terminals[1], NetId::GROUND);
        }
    }

    #[test]
    fn imported_divider_simulates_to_the_midpoint() {
        // The gate's "sim matches" half: import → solve → the midpoint sits at
        // half the rail (two equal resistors, 10 V → 5 V).
        use ohmforge_sim::{Analysis, Engine};
        let c = import(DIVIDER).unwrap();
        let mid = c.part_by_ref("R1").unwrap().terminals[1]; // the `mid` net
        let sigs = Engine::new().run(&c, &Analysis::OperatingPoint).unwrap();
        let name = format!("v(n{})", c.resolve(mid).0);
        let sig = sigs
            .iter()
            .find(|s| s.name == name)
            .expect("v(mid) present");
        let v = sig.real().unwrap();
        assert!((v[0] - 5.0).abs() < 1e-9, "midpoint = {}", v[0]);
    }

    #[test]
    fn suffix_magnitudes_including_m_versus_meg() {
        let cases = [
            ("1k", 1e3),
            ("1meg", 1e6),
            ("1MEG", 1e6),
            ("1m", 1e-3), // milli — NOT mega
            ("1M", 1e-3), // case-insensitive: still milli in SPICE
            ("1u", 1e-6),
            ("1µ", 1e-6),
            ("1n", 1e-9),
            ("1p", 1e-12),
            ("1f", 1e-15),
            ("1g", 1e9),
            ("1t", 1e12),
            ("4.7k", 4700.0),
            ("1mil", 25.4e-6),
            ("10kohm", 1e4),    // trailing unit text ignored
            ("2.2e-9", 2.2e-9), // plain scientific notation
            ("-5", -5.0),
        ];
        for (tok, want) in cases {
            let got = spice_magnitude(tok).unwrap_or_else(|| panic!("{tok} did not parse"));
            assert!(
                (got - want).abs() <= want.abs() * 1e-9 + 1e-18,
                "{tok}: got {got}, want {want}"
            );
        }
        // Non-numeric tokens (e.g. a stray keyword) are rejected.
        assert!(spice_magnitude("DC").is_none());
        assert!(spice_magnitude("").is_none());
    }

    /// The `m`-vs-`meg` gotcha, both directions. On import `m` is milli and `meg`
    /// is mega (case-insensitively), a 1e9 apart. On export we sidestep the trap
    /// entirely by writing a plain base-SI decimal (never a suffix), so a
    /// milliohm and a megohm both survive the round-trip unambiguously.
    #[test]
    fn milli_versus_mega_round_trips_without_suffix_ambiguity() {
        // Import: `m` and `meg` are a billion apart.
        let c = import("t\nR1 a 0 1m\nR2 a 0 1meg\n.end").unwrap();
        assert_eq!(
            c.part_by_ref("R1")
                .unwrap()
                .value
                .as_ref()
                .unwrap()
                .magnitude,
            1e-3
        );
        assert_eq!(
            c.part_by_ref("R2")
                .unwrap()
                .value
                .as_ref()
                .unwrap()
                .magnitude,
            1e6
        );

        // Export writes plain decimals (no `m`/`meg`), so re-import is exact.
        let deck = export(&c);
        assert!(
            deck.contains("R1 a 0 0.001\n") || deck.contains(" 0.001\n"),
            "{deck}"
        );
        assert!(deck.contains(" 1000000\n"), "{deck}");
        assert!(
            !deck.to_lowercase().contains("meg"),
            "export avoids suffixes: {deck}"
        );
        let back = import(&deck).unwrap();
        assert_eq!(
            back.part_by_ref("R1").unwrap().value,
            c.part_by_ref("R1").unwrap().value
        );
        assert_eq!(
            back.part_by_ref("R2").unwrap().value,
            c.part_by_ref("R2").unwrap().value
        );
    }

    /// Build a circuit exercising every exportable kind plus a shared node and a
    /// `wire()`-merged net, to prove topology survives the round-trip.
    fn sample() -> Circuit {
        let mut c = Circuit::new("roundtrip rc");
        let n_in = c.new_net();
        let n_mid = c.new_net();
        let n_out = c.new_net();
        let n_alias = c.new_net();
        c.wire(n_out, n_alias); // one electrical node spelled two ways

        c.add_part(
            "V1",
            PartKind::VoltageSource,
            Some(Value::new(5.0, Unit::Volt)),
            vec![n_in, NetId::GROUND],
        );
        c.add_part(
            "R1",
            PartKind::Resistor,
            Some(Value::new(2200.0, Unit::Ohm)),
            vec![n_in, n_mid],
        );
        c.add_part(
            "C1",
            PartKind::Capacitor,
            Some(Value::new(2.2e-9, Unit::Farad)),
            vec![n_mid, NetId::GROUND],
        );
        c.add_part(
            "L1",
            PartKind::Inductor,
            Some(Value::new(1e-4, Unit::Henry)),
            vec![n_mid, n_alias],
        );
        c.add_part(
            "I1",
            PartKind::CurrentSource,
            Some(Value::new(1e-3, Unit::Ampere)),
            vec![n_out, NetId::GROUND],
        );
        c
    }

    #[test]
    fn round_trip_preserves_parts_and_topology() {
        let orig = sample();
        let deck = export(&orig);
        let back = import(&deck).unwrap();

        assert_eq!(back.name, "roundtrip rc");
        assert_eq!(back.parts().len(), orig.parts().len());

        // Reference, kind, and value survive exactly (f64 equality).
        for op in orig.parts() {
            let bp = back.part_by_ref(&op.reference).unwrap();
            assert_eq!(kind_tag(&bp.kind), kind_tag(&op.kind), "{}", op.reference);
            assert_eq!(bp.value, op.value, "value of {}", op.reference);
        }

        // Connectivity: parts that shared a net still resolve to one net.
        let v1 = back.part_by_ref("V1").unwrap();
        let r1 = back.part_by_ref("R1").unwrap();
        let c1 = back.part_by_ref("C1").unwrap();
        let l1 = back.part_by_ref("L1").unwrap();
        let i1 = back.part_by_ref("I1").unwrap();
        // `n_in` shared by V1 and R1.
        assert_eq!(back.resolve(v1.terminals[0]), back.resolve(r1.terminals[0]));
        // `n_mid` shared by R1, C1, L1.
        assert_eq!(back.resolve(r1.terminals[1]), back.resolve(c1.terminals[0]));
        assert_eq!(back.resolve(r1.terminals[1]), back.resolve(l1.terminals[0]));
        // The wire()-merged pair (L1's alias and I1's node) is one net.
        assert_eq!(back.resolve(l1.terminals[1]), back.resolve(i1.terminals[0]));
        // Ground stayed ground.
        assert_eq!(back.resolve(v1.terminals[1]), NetId::GROUND);
        assert_eq!(back.resolve(c1.terminals[1]), NetId::GROUND);

        // Idempotent: re-exporting the reloaded circuit is byte-identical.
        assert_eq!(export(&back), deck);
    }

    #[test]
    fn export_output_is_stable_and_readable() {
        let deck = export(&sample());
        let lines: Vec<&str> = deck.lines().collect();

        // Title first, `.end` last.
        assert_eq!(lines[0], "roundtrip rc");
        assert_eq!(*lines.last().unwrap(), ".end");

        // Specific element lines are present and exact (ground as `0`).
        assert!(deck.contains("\nV1 n1 0 5\n"), "{deck}");
        assert!(deck.contains("\nR1 n1 n2 2200\n"), "{deck}");
        assert!(deck.contains("\nC1 n2 0 0.0000000022\n"), "{deck}");
    }

    #[test]
    fn unsupported_kinds_export_as_a_comment() {
        let mut c = Circuit::new("has-other");
        let n = c.new_net();
        c.add_part(
            "D1",
            PartKind::Other("led".into()),
            None,
            vec![n, NetId::GROUND],
        );
        let deck = export(&c);
        assert!(
            deck.contains("* D1 skipped (unsupported in SPICE)"),
            "{deck}"
        );
        // And re-importing the deck simply drops it (comment line).
        assert_eq!(import(&deck).unwrap().parts().len(), 0);
    }

    // ── device + controlled-source export (CP-AF-M5.1) ───────────────────────

    #[test]
    fn device_and_controlled_source_round_trip() {
        // The M5.1 gate: a diode, an NPN BJT, an NMOS, and a VCVS survive
        // export→import with their kind and model/gain parameters intact.
        let mut c = Circuit::new("devices");
        let a = c.new_net();
        let b = c.new_net();
        let col = c.new_net();
        let emi = c.new_net();
        let cp = c.new_net();
        let cn = c.new_net();

        c.add_part(
            "D1",
            PartKind::Diode(DiodeModel {
                is: 2.52e-9,
                ..DiodeModel::default()
            }),
            None,
            vec![a, NetId::GROUND],
        );
        c.add_part(
            "Q1",
            PartKind::Bjt(
                Polarity::N,
                BjtModel {
                    is: 2e-14,
                    bf: 250.0,
                    br: 3.0,
                    vt: BjtModel::default().vt,
                },
            ),
            None,
            vec![col, b, emi],
        );
        c.add_part(
            "M1",
            PartKind::Mosfet(
                Polarity::N,
                MosfetModel {
                    vth: 1.5,
                    kp: 2e-3,
                    lambda: 0.02,
                },
            ),
            None,
            vec![a, b, emi],
        );
        c.add_part("E1", PartKind::Vcvs(1.0 / 3.0), None, vec![a, b, cp, cn]);

        let deck = export(&c);
        let back = import(&deck).unwrap();

        // Diode: IS carried in the model (amperes).
        let d1 = back.part_by_ref("D1").unwrap();
        match &d1.kind {
            PartKind::Diode(m) => assert_eq!(m.is, 2.52e-9),
            other => panic!("expected diode, got {other:?}"),
        }

        // NPN BJT: IS/BF/BR survive exactly; VT stays the engine default.
        match &back.part_by_ref("Q1").unwrap().kind {
            PartKind::Bjt(Polarity::N, m) => {
                assert_eq!(m.is, 2e-14);
                assert_eq!(m.bf, 250.0);
                assert_eq!(m.br, 3.0);
                assert_eq!(m.vt, BjtModel::default().vt);
            }
            other => panic!("expected NPN BJT, got {other:?}"),
        }

        // NMOS: VTO/KP/LAMBDA survive exactly.
        match &back.part_by_ref("M1").unwrap().kind {
            PartKind::Mosfet(Polarity::N, m) => {
                assert_eq!(m.vth, 1.5);
                assert_eq!(m.kp, 2e-3);
                assert_eq!(m.lambda, 0.02);
            }
            other => panic!("expected NMOS, got {other:?}"),
        }

        // VCVS: an awkward gain comes back bit-for-bit.
        let e1 = &back.part_by_ref("E1").unwrap().kind;
        assert!(matches!(e1, PartKind::Vcvs(g) if *g == 1.0 / 3.0), "{e1:?}");

        // Topology: nets shared in the original are still one net after the trip.
        let d1 = back.part_by_ref("D1").unwrap();
        let q1 = back.part_by_ref("Q1").unwrap();
        let m1 = back.part_by_ref("M1").unwrap();
        let e1 = back.part_by_ref("E1").unwrap();
        // `a` is shared by D1[0], M1[0], E1[0].
        assert_eq!(back.resolve(d1.terminals[0]), back.resolve(m1.terminals[0]));
        assert_eq!(back.resolve(d1.terminals[0]), back.resolve(e1.terminals[0]));
        // `b` is shared by Q1[1], M1[1], E1[1].
        assert_eq!(back.resolve(q1.terminals[1]), back.resolve(m1.terminals[1]));
        assert_eq!(back.resolve(q1.terminals[1]), back.resolve(e1.terminals[1]));
        // `emi` is shared by Q1[2] and M1[2]; D1's cathode is ground.
        assert_eq!(back.resolve(q1.terminals[2]), back.resolve(m1.terminals[2]));
        assert_eq!(back.resolve(d1.terminals[1]), NetId::GROUND);
    }

    #[test]
    fn pmos_vto_sign_round_trips() {
        // A PMOS writes a negative VTO (the engine stores the magnitude), which
        // `import` reads back as the same positive threshold magnitude.
        let mut c = Circuit::new("pmos");
        let (d, g, s) = (c.new_net(), c.new_net(), c.new_net());
        c.add_part(
            "M1",
            PartKind::Mosfet(
                Polarity::P,
                MosfetModel {
                    vth: 2.0,
                    kp: 1e-3,
                    lambda: 0.0,
                },
            ),
            None,
            vec![d, g, s],
        );
        let deck = export(&c);
        assert!(
            deck.contains("VTO=-2"),
            "PMOS VTO is negative in SPICE:\n{deck}"
        );
        match &import(&deck).unwrap().part_by_ref("M1").unwrap().kind {
            PartKind::Mosfet(Polarity::P, m) => assert_eq!(m.vth, 2.0),
            other => panic!("expected PMOS, got {other:?}"),
        }
    }

    #[test]
    fn all_controlled_sources_round_trip() {
        // E/G carry four nodes + a gain; H/F two nodes, a sense-source name, and a
        // gain. Kind, gain (exact f64), and the control reference all survive.
        let mut c = Circuit::new("controlled");
        let a = c.new_net();
        let b = c.new_net();
        let cp = c.new_net();
        let cn = c.new_net();
        c.add_part("E1", PartKind::Vcvs(10.0), None, vec![a, b, cp, cn]);
        c.add_part("G1", PartKind::Vccs(-2.5), None, vec![a, b, cp, cn]);
        c.add_part(
            "H1",
            PartKind::Ccvs(2000.0, "Vsense".into()),
            None,
            vec![a, b],
        );
        c.add_part(
            "F1",
            PartKind::Cccs(0.75, "Vsense".into()),
            None,
            vec![a, b],
        );

        let back = import(&export(&c)).unwrap();
        let e1 = &back.part_by_ref("E1").unwrap().kind;
        assert!(matches!(e1, PartKind::Vcvs(g) if *g == 10.0), "{e1:?}");
        let g1 = &back.part_by_ref("G1").unwrap().kind;
        assert!(matches!(g1, PartKind::Vccs(g) if *g == -2.5), "{g1:?}");
        let h1 = &back.part_by_ref("H1").unwrap().kind;
        assert!(
            matches!(h1, PartKind::Ccvs(r, ctrl) if *r == 2000.0 && ctrl == "Vsense"),
            "{h1:?}"
        );
        let f1 = &back.part_by_ref("F1").unwrap().kind;
        assert!(
            matches!(f1, PartKind::Cccs(f, ctrl) if *f == 0.75 && ctrl == "Vsense"),
            "{f1:?}"
        );
    }

    #[test]
    fn errors_point_at_the_offending_line() {
        // Line 1 is the title; the bad element is on line 2.
        let few = import("title\nR1 a b").unwrap_err();
        assert_eq!(few, err(2, ErrorKind::TooFewFields { got: 3 }));

        let unknown = import("title\nR1 a b 1k\nX9 a b 2k").unwrap_err();
        assert_eq!(unknown, err(3, ErrorKind::UnknownElement('X')));

        let bad_value = import("title\n\nC1 a b nope").unwrap_err();
        assert_eq!(bad_value.line, 3);
        assert_eq!(bad_value.kind, ErrorKind::BadValue("nope".to_string()));

        let missing = import("title\nV1 a b DC").unwrap_err();
        assert_eq!(missing, err(2, ErrorKind::MissingValue));

        let dup = import("title\nR1 a b 1k\nR1 b c 2k").unwrap_err();
        assert_eq!(dup, err(3, ErrorKind::DuplicateReference("R1".into())));
    }

    #[test]
    fn display_includes_line_number() {
        let e = err(7, ErrorKind::MissingValue);
        assert!(e.to_string().starts_with("line 7:"), "{e}");
    }

    /// A kind discriminant string for equality checks in tests.
    fn kind_tag(kind: &PartKind) -> String {
        match kind {
            PartKind::Resistor => "R".into(),
            PartKind::Capacitor => "C".into(),
            PartKind::Inductor => "L".into(),
            PartKind::VoltageSource => "V".into(),
            PartKind::CurrentSource => "I".into(),
            PartKind::Diode(_) => "D".into(),
            PartKind::Bjt(..) => "Q".into(),
            PartKind::Mosfet(..) => "M".into(),
            PartKind::Vcvs(_) => "E".into(),
            PartKind::Vccs(_) => "G".into(),
            PartKind::Ccvs(..) => "H".into(),
            PartKind::Cccs(..) => "F".into(),
            PartKind::Other(n) => format!("other:{n}"),
        }
    }

    // ── `.model` cards + model-backed devices (CP-AF-M4.1) ───────────────────

    #[test]
    fn model_card_parses_name_type_and_params() {
        // Parens, mixed spacing around `=`, and a comma separator all parse.
        let m = parse_model(".model 1N4148 D (IS=2.52n N = 1.752, RS=0.568)", 7).unwrap();
        assert_eq!(m.name, "1N4148");
        assert_eq!(m.model_type, ModelType::Diode);
        // Suffix magnitudes are scaled to base SI; keys are upper-cased.
        assert!((m.param("IS").unwrap() - 2.52e-9).abs() < 1e-18);
        assert!((m.param("n").unwrap() - 1.752).abs() < 1e-9);
        assert!((m.param("RS").unwrap() - 0.568).abs() < 1e-9);
        // The no-parens form with a type stuck to `(` also works.
        let q = parse_model(".model Q2N NPN(BF=250 IS=1e-14)", 1).unwrap();
        assert_eq!(q.model_type, ModelType::Npn);
        assert_eq!(q.param("BF"), Some(250.0));
    }

    #[test]
    fn diode_model_is_overrides_default_and_changes_junction_drop() {
        // The gate's headline proof: a `.model` IS reaches the engine's diode and
        // gives a *different* junction voltage than the default diode. A current
        // source forces 1 mA into the anode; v(anode) is the forward drop.
        use ohmforge_sim::{Analysis, Engine};

        fn anode_volts(c: &Circuit) -> f64 {
            let anode = c.part_by_ref("D1").unwrap().terminals[0];
            let sigs = Engine::new().run(c, &Analysis::OperatingPoint).unwrap();
            let name = format!("v(n{})", c.resolve(anode).0);
            sigs.iter()
                .find(|s| s.name == name)
                .unwrap()
                .real()
                .unwrap()[0]
        }

        // Imported diode carrying a specified IS = 1 nA.
        let deck = "\
diode IS override
I1 0 a DC 1m
D1 a 0 BIGD
.model BIGD D (IS=1n)
.end
";
        let modeled = import(deck).unwrap();
        // The model's IS landed on the diode model (in amperes), not the default.
        let d1 = modeled.part_by_ref("D1").unwrap();
        match &d1.kind {
            PartKind::Diode(m) => assert!((m.is - 1e-9).abs() < 1e-18),
            other => panic!("expected diode, got {other:?}"),
        }

        // The same topology built with a *default* diode (no value → engine's
        // IS = 1e-14) for a head-to-head comparison.
        let mut dflt = Circuit::new("default diode");
        let a = dflt.new_net();
        dflt.add_part(
            "I1",
            PartKind::CurrentSource,
            Some(Value::new(1e-3, Unit::Ampere)),
            vec![NetId::GROUND, a],
        );
        dflt.add_part(
            "D1",
            PartKind::Diode(DiodeModel::default()),
            None,
            vec![a, NetId::GROUND],
        );

        let v_model = anode_volts(&modeled);
        let v_default = anode_volts(&dflt);

        // A larger IS ⇒ a smaller forward drop; the two differ substantially.
        assert!(
            v_default - v_model > 0.2,
            "model drop {v_model} V vs default {v_default} V — IS not applied?"
        );
        // And the modeled drop matches the inverse-Shockley prediction with the
        // specified IS (emission n = 1, Vt ≈ 0.025852 V as the engine uses).
        let vt = 0.025_852;
        let expected = vt * (1e-3_f64 / 1e-9 + 1.0).ln();
        assert!(
            (v_model - expected).abs() < 1e-3,
            "v_model {v_model} vs Shockley {expected}"
        );
    }

    #[test]
    fn bjt_model_maps_is_bf_br_onto_the_engine_model() {
        // SPICE IS/BF/BR → the engine's Ebers–Moll is/βF/βR; the NPN type selects
        // polarity. These struct fields are exactly what the solver consumes.
        let deck = "\
npn model
Q1 c b e AMP
.model AMP NPN (IS=2e-14 BF=250 BR=3)
.end
";
        let c = import(deck).unwrap();
        let q1 = c.part_by_ref("Q1").unwrap();
        match &q1.kind {
            PartKind::Bjt(Polarity::N, m) => {
                assert!((m.is - 2e-14).abs() < 1e-28);
                assert_eq!(m.bf, 250.0);
                assert_eq!(m.br, 3.0);
                // Differs from the default (βF = 100).
                assert_ne!(m.bf, BjtModel::default().bf);
            }
            other => panic!("expected NPN BJT, got {other:?}"),
        }
        // Terminals are [collector, base, emitter] in order.
        assert_eq!(q1.terminals.len(), 3);
    }

    #[test]
    fn mosfet_model_maps_vto_kp_lambda_and_polarity() {
        // A PMOS, with a signed VTO: the engine stores the threshold *magnitude*
        // (the polarity carries the sign), so −2 V VTO → vth = 2.
        let deck = "\
pmos model
M1 d g s PCH
.model PCH PMOS (VTO=-2 KP=2m LAMBDA=0.02)
.end
";
        let c = import(deck).unwrap();
        match &c.part_by_ref("M1").unwrap().kind {
            PartKind::Mosfet(Polarity::P, m) => {
                assert_eq!(m.vth, 2.0);
                assert_eq!(m.kp, 2e-3);
                assert_eq!(m.lambda, 0.02);
            }
            other => panic!("expected PMOS, got {other:?}"),
        }
    }

    #[test]
    fn unknown_params_are_noted_not_errors() {
        // A diode with recognized IS/N/RS and an unknown CJO still imports: the
        // three Shockley parameters land on the model, and the unhandled CJO
        // yields a Note, never an error.
        let deck = "\
noted params
D1 a 0 DM
.model DM D (IS=2n N=1.8 RS=0.5 CJO=4p)
.end
";
        let (c, notes) = import_with_notes(deck).unwrap();
        match &c.part_by_ref("D1").unwrap().kind {
            PartKind::Diode(m) => {
                assert!((m.is - 2e-9).abs() < 1e-18, "IS applied");
                assert!((m.n - 1.8).abs() < 1e-12, "N applied");
                assert!((m.rs - 0.5).abs() < 1e-12, "RS applied");
            }
            other => panic!("expected diode, got {other:?}"),
        }
        let text = notes
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("CJO"), "CJO should be noted ignored:\n{text}");
        // Notes are tagged with the element's line — where the model is applied
        // (the `D1` line, line 2) — not the `.model` card's.
        assert!(notes.iter().all(|n| n.line == 2), "{text}");
    }

    #[test]
    fn catalog_fallback_resolves_without_a_model_card() {
        // No `.model 1N4148` in the deck ⇒ the built-in vendor catalog supplies it.
        let c = import("catalog fallback\nD1 a 0 1N4148\n.end").unwrap();
        let d1 = c.part_by_ref("D1").unwrap();
        match &d1.kind {
            PartKind::Diode(m) => assert!((m.is - 2.52e-9).abs() < 1e-18),
            other => panic!("expected diode, got {other:?}"),
        }

        // A catalogued NPN resolves to its polarity + gain, too.
        let c = import("catalog npn\nQ1 c b e 2N3904\n.end").unwrap();
        match &c.part_by_ref("Q1").unwrap().kind {
            PartKind::Bjt(Polarity::N, m) => assert!((m.bf - 416.4).abs() < 1e-6),
            other => panic!("expected NPN, got {other:?}"),
        }
    }

    #[test]
    fn deck_model_shadows_the_catalog() {
        // A `.model` with a catalogued name wins over the built-in entry.
        let c = import("shadow\nD1 a 0 1N4148\n.model 1N4148 D (IS=9n)\n.end").unwrap();
        match &c.part_by_ref("D1").unwrap().kind {
            PartKind::Diode(m) => assert!((m.is - 9e-9).abs() < 1e-18),
            other => panic!("expected diode, got {other:?}"),
        }
    }

    #[test]
    fn unknown_model_and_type_mismatch_report_a_line() {
        // An element naming a model that exists nowhere is a hard error.
        let e = import("t\nD1 a 0 NOSUCH\n.end").unwrap_err();
        assert_eq!(e.line, 2);
        assert_eq!(e.kind, ErrorKind::UnknownModel("NOSUCH".into()));

        // A Q line pointing at a diode model is a type mismatch on its line.
        let e = import("t\nQ1 c b e DD\n.model DD D (IS=1n)\n.end").unwrap_err();
        assert_eq!(e.line, 2);
        assert!(matches!(e.kind, ErrorKind::ModelTypeMismatch { .. }), "{e}");
    }

    #[test]
    fn malformed_model_card_reports_its_line() {
        // Unknown type.
        let e = import("t\n.model X FOO (IS=1)\n.end").unwrap_err();
        assert_eq!(e.line, 2);
        assert!(matches!(e.kind, ErrorKind::MalformedModel(_)), "{e}");
        // A parameter that isn't key=value.
        let e = import("t\n.model Y NPN (BF)\n.end").unwrap_err();
        assert_eq!(e.line, 2);
        assert!(matches!(e.kind, ErrorKind::MalformedModel(_)), "{e}");
    }

    #[test]
    fn device_line_shape_errors() {
        // A diode with no model name.
        let e = import("t\nD1 a b\n.end").unwrap_err();
        assert!(matches!(e.kind, ErrorKind::BadDeviceLine { .. }), "{e}");
    }

    #[test]
    fn continuation_lines_join_before_parsing() {
        // A `+`-folded `.model` is one logical card; the element on the previous
        // physical line is numbered by its own start.
        let deck = "\
folded
Q1 c b e PART
.model PART NPN (IS=1e-14
+ BF=300
+ BR=2)
.end
";
        let c = import(deck).unwrap();
        match &c.part_by_ref("Q1").unwrap().kind {
            PartKind::Bjt(Polarity::N, m) => {
                assert_eq!(m.bf, 300.0);
                assert_eq!(m.br, 2.0);
            }
            other => panic!("expected NPN, got {other:?}"),
        }
    }

    #[test]
    fn mosfet_bulk_node_is_accepted_and_noted() {
        // The SPICE 4-node form `M nd ng ns nb model` imports; the bulk node is
        // dropped (engine ties body to source) with a Note.
        let deck = "\
4-node mosfet
M1 d g s bulk NCH
.model NCH NMOS (VTO=1.5 KP=1m)
.end
";
        let (c, notes) = import_with_notes(deck).unwrap();
        let m1 = c.part_by_ref("M1").unwrap();
        assert_eq!(m1.terminals.len(), 3, "only [drain, gate, source] kept");
        assert!(
            notes.iter().any(|n| n.message.contains("bulk")),
            "a bulk-node note is expected"
        );
    }

    #[test]
    fn passive_elements_and_errors_still_behave() {
        // The R/C/L/V/I path and its errors are unchanged by the model additions.
        let c = import(DIVIDER).unwrap();
        assert_eq!(c.parts().len(), 3);
        let unknown = import("title\nX9 a b 2k").unwrap_err();
        assert_eq!(unknown.kind, ErrorKind::UnknownElement('X'));
    }
}
