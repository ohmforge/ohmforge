//! The native project format — a human-readable, diff-friendly text file that
//! round-trips an [`ee_core::Circuit`] losslessly.
//!
//! This is a sibling of [`crate::netlist`]: same comment handling (`#` or `//`),
//! same `gnd`/`ground`/`0` ground aliases, same 1-based line numbers in errors.
//! The difference is intent — the netlist is a terse front door you *type*, while
//! the project format is what the tool *writes back* so a circuit lives happily
//! in git and reloads bit-for-bit.
//!
//! ```text
//! !ee-project v1            // version marker — always the first real line
//! .name rc-lowpass          // the circuit name (everything after `.name `)
//! # one part per line: REF  KIND  VALUE  NET...
//! V1 V 5V      n1 gnd        // a 5 V source between net 1 and ground
//! R1 R 1000Ω   n1 n2         // kind is R/C/L/V, or other(<name>)
//! C1 C 0.000001F n2 gnd
//! D1 other(led) - n2 gnd     // Other kind by name; `-` means no value
//! ```
//!
//! ## Why it is lossless
//!
//! * **Topology.** [`save`] writes each terminal as its *resolved* net
//!   ([`Circuit::resolve`]), so nets merged with [`Circuit::wire`] collapse to one
//!   canonical name (`n3`) and ground is always `gnd`. Two terminals that share a
//!   net therefore print the same name, and [`load`] maps equal names back to one
//!   net — so parts that shared a node still share it after a reload.
//! * **Values.** A value prints as its base-SI magnitude via `f64`'s default
//!   `Display`, which is the shortest string that parses back to the identical
//!   `f64`, followed by the unit's SI symbol (empty for [`Unit::None`]). The unit
//!   is recovered from that symbol, not guessed from the part kind, so a value's
//!   `(magnitude, unit)` pair round-trips exactly. `None` is written as `-`.
//! * **Kinds.** `R`/`C`/`L`/`V` map to the modeled kinds; anything else is
//!   `other(<name>)`, preserving [`PartKind::Other`] verbatim (any name without a
//!   `)`, a newline, or a comment marker).
//! * **Source waveforms + part metadata (`CP-AF-M5.1`).** State a part line
//!   cannot carry rides on directives emitted right after it: `.wave <ref> …`
//!   for a source's time [`Waveform`] (`sine`/`pulse`/`expr`; `dc` is the default
//!   and omitted), `.tol <ref> <frac>` for a tolerance, and `.tempco <ref> <tc>`
//!   for a temperature coefficient. The circuit's operating `.temp` is written
//!   once near the top when it is off the nominal default. Each directive binds
//!   to the part named just above it, so save→load→save is byte-identical.
//!
//! ```text
//! V1 V 0V n1 gnd
//! .wave V1 sine 5 1000 0 90    // offset+amp·sin(2π·freq·t+phase°)
//! R1 R 1000Ω n1 n2
//! .tol R1 0.05                 // ±5 %
//! .tempco R1 0.01              // 1/°C
//! ```
//!
//! (Transistor Ebers–Moll / level-1 model coefficients still default on load —
//! a separate documented follow-up; everything else round-trips exactly.)
//!
//! Zero dependencies beyond `ee-core`; the parser is hand-written like the
//! netlist's.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt::{self, Write as _};

use ee_core::graph::{
    BjtModel, Circuit, MosfetModel, NetId, PartKind, Polarity, Waveform, NOMINAL_TEMP_C,
};
use ee_core::units::Unit;
use ee_core::value::{ParseError, Value};

/// The marker that opens every project file.
const HEADER_TAG: &str = "!ee-project";
/// The only format version this module reads and writes.
const VERSION: &str = "v1";

/// Known unit symbols, longest/most-specific first so `Hz` is matched before `H`.
/// Mirrors [`Unit::symbol`]; [`Unit::None`] has no symbol and is the fallthrough.
const UNIT_SYMBOLS: &[(&str, Unit)] = &[
    ("Hz", Unit::Hertz),
    ("Ω", Unit::Ohm),
    ("V", Unit::Volt),
    ("A", Unit::Ampere),
    ("F", Unit::Farad),
    ("H", Unit::Henry),
    ("s", Unit::Second),
    ("W", Unit::Watt),
];

/// A load failure, tagged with the 1-based source line it occurred on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectError {
    pub line: usize,
    pub kind: ErrorKind,
}

/// What went wrong on a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    /// The file did not open with `!ee-project <version>`.
    MissingHeader,
    /// The header named a version this build does not understand.
    UnknownVersion(String),
    /// A part line had no reference designator.
    EmptyReference,
    /// A part line had a reference but no kind field.
    MissingKind,
    /// The kind field was not `R`/`C`/`L`/`V` or `other(<name>)`.
    UnknownKind(String),
    /// An `other(` kind was never closed with `)`.
    UnterminatedOther,
    /// A part line had a reference and kind but no value field.
    MissingValue,
    /// The value field could not be parsed.
    BadValue(ParseError),
    /// Two parts share a reference designator.
    DuplicateReference(String),
    /// A `.directive` the format does not recognize.
    UnknownDirective(String),
    /// A metadata directive (`.wave`/`.tol`/`.tempco`) named a part that no
    /// earlier line defined.
    UnknownReference(String),
    /// A directive's argument was missing, mis-shaped, or not a valid number.
    /// The reason is a short human description.
    BadDirective(String),
}

impl fmt::Display for ProjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.kind)
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorKind::MissingHeader => {
                write!(f, "missing `{HEADER_TAG} {VERSION}` header")
            }
            ErrorKind::UnknownVersion(v) => {
                write!(f, "unknown format version {v:?} (expected {VERSION:?})")
            }
            ErrorKind::EmptyReference => write!(f, "empty part reference"),
            ErrorKind::MissingKind => {
                write!(f, "missing part kind (expected R, C, L, V, or other(..))")
            }
            ErrorKind::UnknownKind(k) => {
                write!(
                    f,
                    "unknown part kind {k:?} (expected R, C, L, V, or other(..))"
                )
            }
            ErrorKind::UnterminatedOther => write!(f, "unterminated `other(` — missing `)`"),
            ErrorKind::MissingValue => write!(f, "missing value (use `-` for no value)"),
            ErrorKind::BadValue(e) => {
                let why = match e {
                    ParseError::Empty => "value is empty",
                    ParseError::NotANumber => "not a valid number",
                };
                write!(f, "bad value: {why}")
            }
            ErrorKind::DuplicateReference(r) => write!(f, "duplicate part reference {r:?}"),
            ErrorKind::UnknownDirective(d) => write!(f, "unknown directive .{d}"),
            ErrorKind::UnknownReference(r) => {
                write!(f, "directive references unknown part {r:?}")
            }
            ErrorKind::BadDirective(why) => write!(f, "malformed directive: {why}"),
        }
    }
}

impl Error for ProjectError {}

/// Serialize a [`Circuit`] to the native project text. Round-trips through
/// [`load`] losslessly (see the module docs for exactly what is preserved).
pub fn save(circuit: &Circuit) -> String {
    let mut out = String::new();
    // Writing to a String is infallible, so these `write!`s cannot fail.
    let _ = writeln!(out, "{HEADER_TAG} {VERSION}");
    if !circuit.name.is_empty() {
        let _ = writeln!(out, ".name {}", circuit.name);
    }
    // The operating temperature is circuit-level state; emit it only when it has
    // moved off the nominal default, so an untouched circuit stays byte-identical.
    if circuit.temperature() != NOMINAL_TEMP_C {
        let _ = writeln!(out, ".temp {}", circuit.temperature());
    }
    for part in circuit.parts() {
        let _ = write!(
            out,
            "{} {} {}",
            part.reference,
            write_kind(&part.kind),
            write_value(part.value.as_ref())
        );
        for &terminal in &part.terminals {
            let _ = write!(out, " {}", write_net(circuit.resolve(terminal)));
        }
        out.push('\n');
        // Per-part metadata the `REF KIND VALUE NET...` line cannot carry: a
        // source's time waveform, and the part's tolerance/tempco. Each is emitted
        // right after its part so load sees the part first, in a fixed order so
        // save→load→save stays byte-identical. Defaults (`Dc`, no tolerance, no
        // tempco) emit nothing.
        write_waveform(&mut out, &part.reference, &part.waveform);
        if let Some(tol) = circuit.tolerance(&part.reference) {
            let _ = writeln!(out, ".tol {} {}", part.reference, tol);
        }
        if let Some(tc) = circuit.tempco(&part.reference) {
            let _ = writeln!(out, ".tempco {} {}", part.reference, tc);
        }
    }
    out
}

/// Emit a source's time [`Waveform`] as a `.wave <ref> <kind> …` directive.
/// [`Waveform::Dc`] is the default and writes nothing; the others write their
/// fields as base-SI decimals (shortest-round-trip `f64` `Display`), and
/// [`Waveform::Expr`] writes its expression verbatim as the line's tail.
fn write_waveform(out: &mut String, reference: &str, waveform: &Waveform) {
    match waveform {
        Waveform::Dc => {}
        Waveform::Sine {
            amplitude,
            freq,
            offset,
            phase_deg,
        } => {
            let _ = writeln!(
                out,
                ".wave {reference} sine {amplitude} {freq} {offset} {phase_deg}"
            );
        }
        Waveform::Pulse {
            v1,
            v2,
            delay,
            width,
            period,
        } => {
            let _ = writeln!(
                out,
                ".wave {reference} pulse {v1} {v2} {delay} {width} {period}"
            );
        }
        Waveform::Expr(expr) => {
            let _ = writeln!(out, ".wave {reference} expr {expr}");
        }
    }
}

/// Parse native project text back into a [`Circuit`] whose topology and values
/// match the one [`save`] was given.
pub fn load(src: &str) -> Result<Circuit, ProjectError> {
    let mut circuit = Circuit::new("");
    let mut nets: HashMap<String, NetId> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut header_seen = false;

    for (i, raw) in src.lines().enumerate() {
        let line = i + 1;
        let text = strip_comment(raw).trim();
        if text.is_empty() {
            continue;
        }

        // The version marker must be the first meaningful line.
        if !header_seen {
            parse_header(text, line)?;
            header_seen = true;
            continue;
        }

        if let Some(rest) = text.strip_prefix('.') {
            apply_directive(&mut circuit, rest, line)?;
            continue;
        }

        parse_part(&mut circuit, &mut nets, &mut seen, text, line)?;
    }

    if !header_seen {
        return Err(err(1, ErrorKind::MissingHeader));
    }
    Ok(circuit)
}

fn err(line: usize, kind: ErrorKind) -> ProjectError {
    ProjectError { line, kind }
}

/// Everything before the first `#` or `//` comment marker (as in the netlist).
fn strip_comment(line: &str) -> &str {
    let hash = line.find('#').unwrap_or(line.len());
    let slashes = line.find("//").unwrap_or(line.len());
    &line[..hash.min(slashes)]
}

/// Validate the `!ee-project <version>` marker.
fn parse_header(text: &str, line: usize) -> Result<(), ProjectError> {
    let rest = text
        .strip_prefix(HEADER_TAG)
        .ok_or_else(|| err(line, ErrorKind::MissingHeader))?;
    match rest.trim() {
        VERSION => Ok(()),
        other => Err(err(line, ErrorKind::UnknownVersion(other.to_string()))),
    }
}

/// Handle a `.directive` line: `.name`, the circuit `.temp`, and the per-part
/// `.wave`/`.tol`/`.tempco` metadata.
fn apply_directive(circuit: &mut Circuit, rest: &str, line: usize) -> Result<(), ProjectError> {
    let mut it = rest.splitn(2, char::is_whitespace);
    let name = it.next().unwrap_or("");
    let arg = it.next().unwrap_or("").trim();
    match name.to_ascii_lowercase().as_str() {
        "name" => {
            circuit.name = arg.to_string();
            Ok(())
        }
        "temp" => {
            let celsius = parse_f64(arg, line, "`.temp` expects a temperature in °C")?;
            if !circuit.set_temperature(celsius) {
                return Err(err(
                    line,
                    ErrorKind::BadDirective(format!("`.temp` value {celsius} is not finite")),
                ));
            }
            Ok(())
        }
        "wave" => apply_wave(circuit, arg, line),
        "tol" => apply_scalar(circuit, arg, line, Scalar::Tolerance),
        "tempco" => apply_scalar(circuit, arg, line, Scalar::Tempco),
        other => Err(err(line, ErrorKind::UnknownDirective(other.to_string()))),
    }
}

/// Which per-part scalar a `.tol`/`.tempco` directive sets.
#[derive(Clone, Copy)]
enum Scalar {
    Tolerance,
    Tempco,
}

/// Apply a `.tol <ref> <value>` or `.tempco <ref> <value>` directive. The part
/// must already exist (metadata is emitted right after its part line).
fn apply_scalar(
    circuit: &mut Circuit,
    arg: &str,
    line: usize,
    which: Scalar,
) -> Result<(), ProjectError> {
    let name = match which {
        Scalar::Tolerance => "tol",
        Scalar::Tempco => "tempco",
    };
    let mut it = arg.split_whitespace();
    let reference = it.next().ok_or_else(|| {
        err(
            line,
            ErrorKind::BadDirective(format!("`.{name}` expects `<ref> <value>`")),
        )
    })?;
    let value_tok = it.next().ok_or_else(|| {
        err(
            line,
            ErrorKind::BadDirective(format!("`.{name}` expects `<ref> <value>`")),
        )
    })?;
    let value = parse_f64(value_tok, line, &format!("`.{name}` value is not a number"))?;
    if circuit.part_by_ref(reference).is_none() {
        return Err(err(
            line,
            ErrorKind::UnknownReference(reference.to_string()),
        ));
    }
    let ok = match which {
        Scalar::Tolerance => circuit.set_tolerance(reference, value),
        Scalar::Tempco => circuit.set_tempco(reference, value),
    };
    if !ok {
        return Err(err(
            line,
            ErrorKind::BadDirective(format!("`.{name}` value {value} is out of range")),
        ));
    }
    Ok(())
}

/// Apply a `.wave <ref> <kind> …` directive, attaching a [`Waveform`] to an
/// existing part. The part must already be defined earlier in the file.
fn apply_wave(circuit: &mut Circuit, arg: &str, line: usize) -> Result<(), ProjectError> {
    let mut it = arg.splitn(3, char::is_whitespace);
    let reference = it.next().unwrap_or("");
    let kind = it.next().unwrap_or("");
    let params = it.next().unwrap_or("").trim();
    if reference.is_empty() || kind.is_empty() {
        return Err(err(
            line,
            ErrorKind::BadDirective("`.wave` expects `<ref> <kind> …`".to_string()),
        ));
    }
    if circuit.part_by_ref(reference).is_none() {
        return Err(err(
            line,
            ErrorKind::UnknownReference(reference.to_string()),
        ));
    }
    let waveform = parse_waveform(kind, params, line)?;
    circuit.set_waveform(reference, waveform);
    Ok(())
}

/// Parse a waveform spelling (`dc`/`sine`/`pulse`/`expr`) and its parameter tail
/// into a [`Waveform`]. Numeric forms expect an exact count of base-SI numbers;
/// `expr` keeps the remaining text verbatim as the expression.
fn parse_waveform(kind: &str, params: &str, line: usize) -> Result<Waveform, ProjectError> {
    match kind.to_ascii_lowercase().as_str() {
        "dc" => Ok(Waveform::Dc),
        "sine" => {
            let n = wave_numbers(params, 4, "sine", line)?;
            Ok(Waveform::Sine {
                amplitude: n[0],
                freq: n[1],
                offset: n[2],
                phase_deg: n[3],
            })
        }
        "pulse" => {
            let n = wave_numbers(params, 5, "pulse", line)?;
            Ok(Waveform::Pulse {
                v1: n[0],
                v2: n[1],
                delay: n[2],
                width: n[3],
                period: n[4],
            })
        }
        "expr" => {
            if params.is_empty() {
                return Err(err(
                    line,
                    ErrorKind::BadDirective("`.wave … expr` needs an expression".to_string()),
                ));
            }
            Ok(Waveform::Expr(params.to_string()))
        }
        other => Err(err(
            line,
            ErrorKind::BadDirective(format!("unknown waveform {other:?}")),
        )),
    }
}

/// Parse exactly `count` whitespace-separated base-SI numbers from a waveform's
/// parameter tail.
fn wave_numbers(
    params: &str,
    count: usize,
    kind: &str,
    line: usize,
) -> Result<Vec<f64>, ProjectError> {
    let toks: Vec<&str> = params.split_whitespace().collect();
    if toks.len() != count {
        return Err(err(
            line,
            ErrorKind::BadDirective(format!(
                "`.wave … {kind}` expects {count} numbers, found {}",
                toks.len()
            )),
        ));
    }
    toks.iter()
        .map(|t| {
            parse_f64(
                t,
                line,
                &format!("`.wave … {kind}` parameter is not a number"),
            )
        })
        .collect()
}

/// Parse a plain `f64` (as written by `f64`'s `Display`), mapping a failure to a
/// [`ErrorKind::BadDirective`] carrying `why`.
fn parse_f64(tok: &str, line: usize, why: &str) -> Result<f64, ProjectError> {
    tok.parse::<f64>().map_err(|_| {
        err(
            line,
            ErrorKind::BadDirective(format!("{why} (got {tok:?})")),
        )
    })
}

/// Parse one `REF KIND VALUE NET...` part line and add it to the circuit.
fn parse_part(
    circuit: &mut Circuit,
    nets: &mut HashMap<String, NetId>,
    seen: &mut HashSet<String>,
    text: &str,
    line: usize,
) -> Result<(), ProjectError> {
    let (reference, rest) = split_first_token(text);
    if reference.is_empty() {
        return Err(err(line, ErrorKind::EmptyReference));
    }
    let (kind, rest) = take_kind(rest).map_err(|k| err(line, k))?;
    if !seen.insert(reference.to_string()) {
        return Err(err(
            line,
            ErrorKind::DuplicateReference(reference.to_string()),
        ));
    }

    let mut fields = rest.split_whitespace();
    let value_tok = fields
        .next()
        .ok_or_else(|| err(line, ErrorKind::MissingValue))?;
    let value = parse_value(value_tok).map_err(|k| err(line, k))?;
    let terminals: Vec<NetId> = fields.map(|name| net_for(circuit, nets, name)).collect();

    circuit.add_part(reference, kind, value, terminals);
    Ok(())
}

/// Split a `REF KIND VALUE NET...` body at the first whitespace run: the leading
/// token and the trimmed remainder.
fn split_first_token(text: &str) -> (&str, &str) {
    match text.find(char::is_whitespace) {
        Some(i) => (&text[..i], text[i..].trim_start()),
        None => (text, ""),
    }
}

/// Read the kind field, returning it and the trimmed remainder of the line.
/// `other(<name>)` keeps its name verbatim (and may contain whitespace).
fn take_kind(rest: &str) -> Result<(PartKind, &str), ErrorKind> {
    if rest.is_empty() {
        return Err(ErrorKind::MissingKind);
    }
    // `other(<name>)`, case-insensitive on the `other(` lead-in. Compare bytes so
    // a leading multibyte char can never slice mid-UTF-8; `other(` is pure ASCII,
    // so a match guarantees byte 6 is a valid char boundary.
    let bytes = rest.as_bytes();
    if bytes.len() >= 6 && bytes[..6].eq_ignore_ascii_case(b"other(") {
        let after = &rest[6..];
        let close = after.find(')').ok_or(ErrorKind::UnterminatedOther)?;
        let name = &after[..close];
        let remainder = after[close + 1..].trim_start();
        return Ok((PartKind::Other(name.to_string()), remainder));
    }
    let (tok, remainder) = split_first_token(rest);
    // Controlled sources carry a gain (and, for the CC pair, a case-sensitive
    // control reference), so parse them off the raw token before upper-casing.
    if let Some(result) = parse_controlled(tok) {
        return Ok((result?, remainder));
    }
    let kind = match tok.to_ascii_uppercase().as_str() {
        "R" => PartKind::Resistor,
        "C" => PartKind::Capacitor,
        "L" => PartKind::Inductor,
        "V" => PartKind::VoltageSource,
        "I" => PartKind::CurrentSource,
        "D" => PartKind::Diode,
        // Transistors: polarity preserved; model params default on load
        // (custom-param round-trip is a documented follow-up).
        "QN" => PartKind::Bjt(Polarity::N, BjtModel::default()),
        "QP" => PartKind::Bjt(Polarity::P, BjtModel::default()),
        "MN" => PartKind::Mosfet(Polarity::N, MosfetModel::default()),
        "MP" => PartKind::Mosfet(Polarity::P, MosfetModel::default()),
        _ => return Err(ErrorKind::UnknownKind(tok.to_string())),
    };
    Ok((kind, remainder))
}

/// Parse the value field: `-` is [`None`]; otherwise a magnitude plus an optional
/// unit symbol (`1000Ω`, `0.000001F`, or a bare number for [`Unit::None`]).
fn parse_value(tok: &str) -> Result<Option<Value>, ErrorKind> {
    if tok == "-" {
        return Ok(None);
    }
    let (magnitude, unit) = split_unit(tok);
    let value = Value::parse(magnitude, unit).map_err(ErrorKind::BadValue)?;
    Ok(Some(value))
}

/// Split a value token into its magnitude text and the [`Unit`] named by its
/// trailing SI symbol (defaulting to [`Unit::None`] when no symbol is present).
fn split_unit(tok: &str) -> (&str, Unit) {
    for (symbol, unit) in UNIT_SYMBOLS {
        if let Some(magnitude) = tok.strip_suffix(symbol) {
            return (magnitude, *unit);
        }
    }
    (tok, Unit::None)
}

/// The net for a node name, allocating a fresh one the first time it is seen.
/// `gnd`/`ground`/`0` always resolve to [`NetId::GROUND`] (as in the netlist).
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
    matches!(name.to_ascii_lowercase().as_str(), "gnd" | "ground" | "0")
}

/// The kind field for a part: `R`/`C`/`L`/`V`/`I`, or `other(<name>)`.
fn write_kind(kind: &PartKind) -> String {
    match kind {
        PartKind::Resistor => "R".to_string(),
        PartKind::Capacitor => "C".to_string(),
        PartKind::Inductor => "L".to_string(),
        PartKind::VoltageSource => "V".to_string(),
        PartKind::CurrentSource => "I".to_string(),
        PartKind::Diode => "D".to_string(),
        PartKind::Bjt(pol, _) => format!("Q{}", polarity_letter(*pol)),
        PartKind::Mosfet(pol, _) => format!("M{}", polarity_letter(*pol)),
        PartKind::Vcvs(e) => format!("vcvs({e})"),
        PartKind::Vccs(g) => format!("vccs({g})"),
        PartKind::Ccvs(r, ctrl) => format!("ccvs({r},{ctrl})"),
        PartKind::Cccs(f, ctrl) => format!("cccs({f},{ctrl})"),
        PartKind::Other(name) => format!("other({name})"),
    }
}

/// Parse a controlled-source kind token like `vcvs(10)` or `ccvs(2000,Vsense)`,
/// case-preserving (the control reference is case-sensitive). `None` if the token
/// isn't a controlled source.
fn parse_controlled(tok: &str) -> Option<Result<PartKind, ErrorKind>> {
    let open = tok.find('(')?;
    let head = tok[..open].to_ascii_lowercase();
    if !matches!(head.as_str(), "vcvs" | "vccs" | "ccvs" | "cccs") {
        return None;
    }
    let bad = || ErrorKind::UnknownKind(tok.to_string());
    let Some(inner) = tok.strip_suffix(')').map(|t| &t[open + 1..]) else {
        return Some(Err(ErrorKind::UnterminatedOther));
    };
    Some(match head.as_str() {
        "vcvs" => inner.trim().parse().map(PartKind::Vcvs).map_err(|_| bad()),
        "vccs" => inner.trim().parse().map(PartKind::Vccs).map_err(|_| bad()),
        _ => match inner.split_once(',') {
            Some((g, ctrl)) => match g.trim().parse::<f64>() {
                Ok(gain) => {
                    let ctrl = ctrl.trim().to_string();
                    Ok(if head == "ccvs" {
                        PartKind::Ccvs(gain, ctrl)
                    } else {
                        PartKind::Cccs(gain, ctrl)
                    })
                }
                Err(_) => Err(bad()),
            },
            None => Err(bad()),
        },
    })
}

/// `n` for an N-type transistor, `p` for P-type.
fn polarity_letter(pol: Polarity) -> &'static str {
    match pol {
        Polarity::N => "n",
        Polarity::P => "p",
    }
}

/// The value field for a part: `-` for [`None`], else magnitude + unit symbol.
fn write_value(value: Option<&Value>) -> String {
    match value {
        None => "-".to_string(),
        // `f64`'s default Display is the shortest string that parses back to the
        // same bits; the symbol carries the unit (empty for `Unit::None`).
        Some(v) => format!("{}{}", v.magnitude, v.unit.symbol()),
    }
}

/// The node name for a resolved net: `gnd` for ground, else `n<id>`.
fn write_net(net: NetId) -> String {
    if net == NetId::GROUND {
        "gnd".to_string()
    } else {
        format!("n{}", net.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a circuit that exercises every tricky case: a named circuit, an
    /// inductor, an `Other` kind, a `None` value, two parts that share a plain
    /// node, and a `wire()`-merged pair of nets.
    fn sample() -> (Circuit, NetId, NetId) {
        let mut c = Circuit::new("rc demo");
        let n_in = c.new_net();
        let n_mid = c.new_net();
        let n_out = c.new_net();
        let n_alias = c.new_net();
        // Merge `n_out` and `n_alias` into one electrical node.
        c.wire(n_out, n_alias);

        // V1 and R1 share `n_in`; R1 and C1 share `n_mid`.
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            Some(Value::new(5.0, Unit::Volt)),
            vec![n_in, NetId::GROUND],
        );
        c.add_part(
            "R1",
            PartKind::Resistor,
            Some(Value::new(1000.0, Unit::Ohm)),
            vec![n_in, n_mid],
        );
        c.add_part(
            "C1",
            PartKind::Capacitor,
            Some(Value::new(1e-6, Unit::Farad)),
            vec![n_mid, NetId::GROUND],
        );
        c.add_part(
            "L1",
            PartKind::Inductor,
            Some(Value::new(1e-4, Unit::Henry)),
            // Uses `n_alias`; after save it must resolve to the same net as n_out.
            vec![n_mid, n_alias],
        );
        c.add_part(
            "D1",
            PartKind::Other("led".into()),
            None,
            // Uses `n_out`, the other half of the wired pair.
            vec![n_out, NetId::GROUND],
        );
        (c, n_out, n_alias)
    }

    #[test]
    fn round_trip_preserves_name_parts_values_and_topology() {
        let (orig, _, _) = sample();
        let text = save(&orig);
        let back = load(&text).unwrap();

        // Name and part count survive.
        assert_eq!(back.name, "rc demo");
        assert_eq!(back.parts().len(), orig.parts().len());

        // Each part's reference, kind, and value match exactly (f64 equality).
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
        let d1 = back.part_by_ref("D1").unwrap();

        // `n_in` shared by V1 and R1.
        assert_eq!(back.resolve(v1.terminals[0]), back.resolve(r1.terminals[0]));
        // `n_mid` shared by R1, C1, L1.
        assert_eq!(back.resolve(r1.terminals[1]), back.resolve(c1.terminals[0]));
        assert_eq!(back.resolve(r1.terminals[1]), back.resolve(l1.terminals[0]));
        // The wire()-merged pair (L1's n_alias and D1's n_out) collapsed to one net.
        assert_eq!(back.resolve(l1.terminals[1]), back.resolve(d1.terminals[0]));
        // Ground stayed ground everywhere.
        assert_eq!(back.resolve(v1.terminals[1]), NetId::GROUND);
        assert_eq!(back.resolve(c1.terminals[1]), NetId::GROUND);
        assert_eq!(back.resolve(d1.terminals[1]), NetId::GROUND);

        // The `None` value and `Other` name specifically survived.
        assert_eq!(d1.value, None);
        assert!(matches!(&d1.kind, PartKind::Other(n) if n == "led"));

        // Idempotent: saving the reloaded circuit yields byte-identical text.
        assert_eq!(save(&back), text);
    }

    /// The controlled-source and transistor encodings round-trip: kind
    /// discriminant, gain/transresistance (exact `f64`), the case-sensitive
    /// control reference, and transistor polarity all survive save→load, and a
    /// second save is byte-identical. (Transistor *model params* default on load
    /// by documented design, so this checks the discriminant + polarity, not the
    /// Ebers–Moll/level-1 coefficients.)
    #[test]
    fn round_trip_preserves_controlled_sources_and_transistors() {
        let mut c = Circuit::new("controlled + active");
        let a = c.new_net();
        let b = c.new_net();
        let ctrlp = c.new_net();
        let ctrln = c.new_net();
        // Awkward gains to prove exact f64 round-trip (not just round numbers).
        c.add_part(
            "E1",
            PartKind::Vcvs(1.0 / 3.0),
            None,
            vec![a, b, ctrlp, ctrln],
        );
        c.add_part("G1", PartKind::Vccs(-2.5), None, vec![a, b, ctrlp, ctrln]);
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
        c.add_part(
            "Q1",
            PartKind::Bjt(Polarity::N, BjtModel::default()),
            None,
            vec![a, b, ctrln],
        );
        c.add_part(
            "Q2",
            PartKind::Bjt(Polarity::P, BjtModel::default()),
            None,
            vec![a, b, ctrln],
        );
        c.add_part(
            "M1",
            PartKind::Mosfet(Polarity::N, MosfetModel::default()),
            None,
            vec![a, b, ctrln],
        );
        c.add_part(
            "M2",
            PartKind::Mosfet(Polarity::P, MosfetModel::default()),
            None,
            vec![a, b, ctrln],
        );

        let text = save(&c);
        let back = load(&text).unwrap();
        assert_eq!(back.parts().len(), c.parts().len());

        // Exact kind reconstruction, including gains and control references.
        let e1 = &back.part_by_ref("E1").unwrap().kind;
        assert!(matches!(e1, PartKind::Vcvs(g) if *g == 1.0 / 3.0), "{e1:?}");
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
        // Transistor discriminant + polarity survive.
        assert!(matches!(
            back.part_by_ref("Q1").unwrap().kind,
            PartKind::Bjt(Polarity::N, _)
        ));
        assert!(matches!(
            back.part_by_ref("Q2").unwrap().kind,
            PartKind::Bjt(Polarity::P, _)
        ));
        assert!(matches!(
            back.part_by_ref("M1").unwrap().kind,
            PartKind::Mosfet(Polarity::N, _)
        ));
        assert!(matches!(
            back.part_by_ref("M2").unwrap().kind,
            PartKind::Mosfet(Polarity::P, _)
        ));

        // The VCVS's four terminals and control nets survive as distinct nets.
        let e1p = back.part_by_ref("E1").unwrap();
        assert_eq!(e1p.terminals.len(), 4, "VCVS keeps its 4 terminals");

        // Idempotent: re-saving the reloaded circuit is byte-identical.
        assert_eq!(save(&back), text);
    }

    #[test]
    fn exact_float_round_trip_for_awkward_magnitudes() {
        // Values that are not round decimals must come back bit-for-bit.
        let mut c = Circuit::new("floats");
        let n = c.new_net();
        for (i, m) in [2.2e-9_f64, 4.7e3, 1.0 / 3.0, 123.456789e-12]
            .into_iter()
            .enumerate()
        {
            c.add_part(
                format!("R{i}"),
                PartKind::Resistor,
                Some(Value::new(m, Unit::Ohm)),
                vec![n, NetId::GROUND],
            );
        }
        let back = load(&save(&c)).unwrap();
        for p in c.parts() {
            let bp = back.part_by_ref(&p.reference).unwrap();
            assert_eq!(bp.value.unwrap().magnitude, p.value.unwrap().magnitude);
        }
    }

    #[test]
    fn save_output_is_stable_and_readable() {
        let (c, _, _) = sample();
        let text = save(&c);
        let lines: Vec<&str> = text.lines().collect();

        // Header first, then the name directive.
        assert_eq!(lines[0], "!ee-project v1");
        assert_eq!(lines[1], ".name rc demo");

        // Specific human-readable part lines are present and exact.
        assert!(text.contains("\nV1 V 5V n1 gnd\n"), "{text}");
        assert!(text.contains("\nR1 R 1000Ω n1 n2\n"), "{text}");
        assert!(text.contains("\nC1 C 0.000001F n2 gnd\n"), "{text}");
        assert!(text.contains("\nD1 other(led) - n3 gnd\n"), "{text}");
    }

    #[test]
    fn load_accepts_comments_and_ground_aliases() {
        let src = "\
!ee-project v1
# a comment line
.name mixed
R1 R 1k a gnd    // trailing comment
R2 R 1k a 0      # `0` is ground
R3 R 1k a ground
";
        let c = load(src).unwrap();
        assert_eq!(c.name, "mixed");
        assert_eq!(c.parts().len(), 3);
        for r in ["R1", "R2", "R3"] {
            assert_eq!(c.part_by_ref(r).unwrap().terminals[1], NetId::GROUND);
        }
        // `a` is shared by all three.
        let a1 = c.resolve(c.part_by_ref("R1").unwrap().terminals[0]);
        let a3 = c.resolve(c.part_by_ref("R3").unwrap().terminals[0]);
        assert_eq!(a1, a3);
    }

    #[test]
    fn errors_point_at_the_offending_line() {
        // Missing header entirely.
        let no_header = load("R1 R 1k a b").unwrap_err();
        assert_eq!(no_header, err(1, ErrorKind::MissingHeader));

        // Wrong version.
        let bad_ver = load("!ee-project v9\n").unwrap_err();
        assert_eq!(bad_ver, err(1, ErrorKind::UnknownVersion("v9".into())));

        // Bad value on line 3 (header + blank + part).
        let bad_value = load("!ee-project v1\n\nC1 C nope a b").unwrap_err();
        assert_eq!(bad_value.line, 3);
        assert!(matches!(bad_value.kind, ErrorKind::BadValue(_)));

        // Unknown kind.
        let bad_kind = load("!ee-project v1\nX1 Q 1k a b").unwrap_err();
        assert_eq!(bad_kind, err(2, ErrorKind::UnknownKind("Q".into())));

        // Missing value field.
        let no_value = load("!ee-project v1\nR1 R").unwrap_err();
        assert_eq!(no_value, err(2, ErrorKind::MissingValue));

        // Unterminated other(.
        let open = load("!ee-project v1\nD1 other(led - a b").unwrap_err();
        assert_eq!(open, err(2, ErrorKind::UnterminatedOther));

        // Duplicate reference.
        let dup = load("!ee-project v1\nR1 R 1k a b\nR1 R 2k b c").unwrap_err();
        assert_eq!(dup, err(3, ErrorKind::DuplicateReference("R1".into())));

        // Unknown directive.
        let dir = load("!ee-project v1\n.nope foo").unwrap_err();
        assert_eq!(dir, err(2, ErrorKind::UnknownDirective("nope".into())));
    }

    #[test]
    fn display_includes_line_number() {
        let e = err(7, ErrorKind::MissingValue);
        assert!(e.to_string().starts_with("line 7:"), "{e}");
    }

    // ── waveform + tolerance/tempco round-trip (CP-AF-M5.2) ──────────────────

    #[test]
    fn waveform_tolerance_tempco_and_temp_round_trip_exactly() {
        // The M5.2 gate: a Sine source, an Expr source, a ±5 % tolerance, and a
        // tempco (plus a non-nominal temperature) survive save→load exactly, and a
        // second save is byte-identical.
        let mut c = Circuit::new("waveforms");
        let n1 = c.new_net();
        let n2 = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            Some(Value::new(0.0, Unit::Volt)),
            vec![n1, NetId::GROUND],
        );
        c.add_part(
            "V2",
            PartKind::VoltageSource,
            Some(Value::new(0.0, Unit::Volt)),
            vec![n2, NetId::GROUND],
        );
        c.add_part(
            "R1",
            PartKind::Resistor,
            Some(Value::new(1000.0, Unit::Ohm)),
            vec![n1, n2],
        );
        let sine = Waveform::Sine {
            amplitude: 5.0,
            freq: 1000.0,
            offset: 0.5,
            phase_deg: 90.0,
        };
        assert!(c.set_waveform("V1", sine.clone()));
        let expr = Waveform::Expr("sin(2*pi*1000*t)".to_string());
        assert!(c.set_waveform("V2", expr.clone()));
        assert!(c.set_tolerance("R1", 0.05));
        assert!(c.set_tempco("R1", 0.01));
        assert!(c.set_temperature(85.0));

        let text = save(&c);
        let back = load(&text).unwrap();

        // save→load→save is byte-identical.
        assert_eq!(
            save(&back),
            text,
            "re-save must reproduce the file exactly:\n{text}"
        );

        // The reloaded circuit is equivalent in every serialized field.
        assert_eq!(back.part_by_ref("V1").unwrap().waveform, sine);
        assert_eq!(back.part_by_ref("V2").unwrap().waveform, expr);
        assert_eq!(back.tolerance("R1"), Some(0.05));
        assert_eq!(back.tempco("R1"), Some(0.01));
        assert_eq!(back.temperature(), 85.0);
        // Parts without metadata stay at their defaults.
        assert_eq!(back.part_by_ref("R1").unwrap().waveform, Waveform::Dc);
        assert_eq!(back.tolerance("V1"), None);
    }

    #[test]
    fn pulse_waveform_round_trips() {
        let mut c = Circuit::new("pulse");
        let n = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            Some(Value::new(0.0, Unit::Volt)),
            vec![n, NetId::GROUND],
        );
        let pulse = Waveform::Pulse {
            v1: 0.0,
            v2: 3.3,
            delay: 1e-6,
            width: 5e-6,
            period: 1e-5,
        };
        assert!(c.set_waveform("V1", pulse.clone()));
        let back = load(&save(&c)).unwrap();
        assert_eq!(back.part_by_ref("V1").unwrap().waveform, pulse);
        assert_eq!(save(&back), save(&c));
    }

    #[test]
    fn default_metadata_emits_nothing_extra() {
        // A circuit with no waveforms/tolerance/tempco and the nominal temperature
        // serializes with no directive lines beyond `.name` — the existing shape.
        let (c, _, _) = sample();
        let text = save(&c);
        assert!(!text.contains(".wave"), "{text}");
        assert!(!text.contains(".tol"), "{text}");
        assert!(!text.contains(".tempco"), "{text}");
        assert!(!text.contains(".temp"), "{text}");
    }

    #[test]
    fn metadata_directive_errors_point_at_the_line() {
        // A `.tol` naming an unknown part.
        let e = load("!ee-project v1\nR1 R 1k a b\n.tol R9 0.05").unwrap_err();
        assert_eq!(e, err(3, ErrorKind::UnknownReference("R9".into())));

        // A `.wave` with a bad kind.
        let e = load("!ee-project v1\nV1 V 1V a gnd\n.wave V1 ramp 1 2").unwrap_err();
        assert_eq!(e.line, 3);
        assert!(matches!(e.kind, ErrorKind::BadDirective(_)), "{e}");

        // A `.wave sine` with the wrong number of parameters.
        let e = load("!ee-project v1\nV1 V 1V a gnd\n.wave V1 sine 1 2 3").unwrap_err();
        assert_eq!(e.line, 3);
        assert!(matches!(e.kind, ErrorKind::BadDirective(_)), "{e}");

        // A non-numeric `.temp`.
        let e = load("!ee-project v1\n.temp hot\nR1 R 1k a b").unwrap_err();
        assert_eq!(e.line, 2);
        assert!(matches!(e.kind, ErrorKind::BadDirective(_)), "{e}");
    }

    /// A kind discriminant string for equality checks in tests.
    fn kind_tag(kind: &PartKind) -> String {
        match kind {
            PartKind::Resistor => "R".into(),
            PartKind::Capacitor => "C".into(),
            PartKind::Inductor => "L".into(),
            PartKind::VoltageSource => "V".into(),
            PartKind::CurrentSource => "I".into(),
            PartKind::Diode => "D".into(),
            PartKind::Bjt(..) => "Q".into(),
            PartKind::Mosfet(..) => "M".into(),
            PartKind::Vcvs(_) => "E".into(),
            PartKind::Vccs(_) => "G".into(),
            PartKind::Ccvs(..) => "H".into(),
            PartKind::Cccs(..) => "F".into(),
            PartKind::Other(n) => format!("other:{n}"),
        }
    }
}
