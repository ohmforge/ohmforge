//! Copilot **modes** (CP-AF-M2.2) — Design, Debug, Firmware — layered on the M1
//! tool loop and the M2.1 verification loop.
//!
//! Each mode is the same honest machinery wearing a different hat
//! (docs/AI.md § Capabilities):
//!
//! * **Design** — the verification loop you already have ([`verify`](fn@crate::verify)):
//!   propose → scratch-apply → re-simulate → measure against a [`Spec`] →
//!   present; nothing applies without approval. Re-exported here as the mode
//!   surface ([`design`], [`Designer`], [`DesignOutcome`]).
//! * **Debug** — given a circuit with a seeded fault and an *expected*
//!   behavior, the mode reads the real operating point, compares it to the
//!   expectation, and **localizes the faulty part** by finding the single
//!   component whose smallest value correction would restore the spec — the
//!   minimal single-fault explanation, grounded in real re-solves ([`Debugger`],
//!   [`debug_circuit`], [`localize_fault`], [`FaultReport`]).
//! * **Firmware** — the seam for firmware sketches: the model emits a compiled
//!   image as an **Intel HEX** string, which is validated and staged. That
//!   validated string is exactly what the co-sim bridge (`ohmforge-mcu`'s
//!   `load_firmware`) consumes, so wiring it into a live emulator is a thin
//!   follow-up; the mode is fully testable with a stub model today
//!   ([`FirmwareBuilder`], [`firmware`], [`FirmwareImage`], [`parse_intel_hex`]).
//!
//! No new dependencies: Debug re-solves through the `ohmforge-sim` engine the crate
//! already uses, and Firmware's HEX reader is hand-rolled std-only like
//! `ohmforge-fmt`'s parsers — so the whole surface stays network-free and the tests
//! use the [`MockTransport`](crate::transport::MockTransport).

use std::error::Error;
use std::fmt;

use ohmforge_core::graph::{Circuit, PartKind};
use ohmforge_core::units::Unit;
use ohmforge_core::value::Value;
use ohmforge_shell::Shell;
use ohmforge_sim::{Analysis, Engine};

use crate::client::{Client, ToolHandler};
use crate::error::AiError;
use crate::message::{SystemBlock, Tool};
use crate::tools::{json_escape, json_string_field, parse_node_volts};
use crate::transport::Transport;

// ── Design mode ─────────────────────────────────────────────────────────────
//
// Design *is* the verification loop (CP-AF-M2.1). Re-export it under the modes
// umbrella so callers reach every mode through one module, and the "design to a
// spec" framing lives next to its siblings.
pub use crate::verify::{design, DesignOutcome, Designer, Spec};

// ── Debug mode ──────────────────────────────────────────────────────────────

/// The `read_op` tool name, shared with the ask/debug flows: solve the operating
/// point and read one node's voltage.
pub const DEBUG_READ_OP_TOOL: &str = "read_op";
/// The `diagnose` tool name: run fault localization and return the report.
pub const DIAGNOSE_TOOL: &str = "diagnose";

/// One candidate culprit from [`localize_fault`]: the part, its current value,
/// the value it would need to restore the expected behavior on its own, and how
/// large that correction is relative to its current value.
#[derive(Debug, Clone, PartialEq)]
pub struct Suspect {
    /// The part's reference designator, e.g. `"R1"`.
    pub reference: String,
    /// The part's current (faulty-circuit) value magnitude.
    pub current_value: f64,
    /// The value this part alone would need to meet the spec, holding all other
    /// parts fixed.
    pub corrected_value: f64,
    /// `|ln(corrected/current)|` — the size of the single-part correction on a
    /// **log scale**, so a halving and a doubling rank as equally large (a linear
    /// `|corrected/current - 1|` would call a doubling, +1.0, twice as severe as a
    /// halving, −0.5, though both are one factor of two). The smallest such
    /// correction is the most likely fault (Occam's razor).
    pub relative_change: f64,
}

/// The outcome of fault localization: the real measured behavior at the target
/// node, whether it meets the expectation, the ranked suspects, and the single
/// localized culprit (the smallest-correction suspect).
#[derive(Debug, Clone, PartialEq)]
pub struct FaultReport {
    /// The node whose voltage was checked, e.g. `"out"`.
    pub target: String,
    /// The real measured DC voltage at `target` on the (faulty) circuit.
    pub measured: f64,
    /// The expected DC voltage at `target`.
    pub expected: f64,
    /// The absolute tolerance band around `expected`.
    pub tolerance: f64,
    /// `true` when the circuit already meets the expectation (no fault found).
    pub within_spec: bool,
    /// The localized faulty part, if one was found — the suspect needing the
    /// smallest single-value correction. `None` when the circuit is within spec
    /// or no single-part correction reaches the target.
    pub suspect: Option<String>,
    /// Every candidate, ordered by ascending correction size (best first).
    pub suspects: Vec<Suspect>,
    /// A human summary citing the measured gap and the localized part.
    pub summary: String,
}

impl FaultReport {
    /// A compact JSON rendering fed back to the model so it can cite the numbers
    /// and name the part.
    pub fn to_json(&self) -> String {
        let mut out = format!(
            "{{\"target\":\"{}\",\"measured\":{},\"expected\":{},\"tolerance\":{},\"within_spec\":{},\"suspect\":",
            json_escape(&self.target),
            self.measured,
            self.expected,
            self.tolerance,
            self.within_spec
        );
        match &self.suspect {
            Some(r) => out.push_str(&format!("\"{}\"", json_escape(r))),
            None => out.push_str("null"),
        }
        out.push_str(",\"suspects\":[");
        for (i, s) in self.suspects.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"reference\":\"{}\",\"current_value\":{},\"corrected_value\":{},\"relative_change\":{}}}",
                json_escape(&s.reference),
                s.current_value,
                s.corrected_value,
                s.relative_change
            ));
        }
        out.push_str("],\"summary\":\"");
        out.push_str(&json_escape(&self.summary));
        out.push_str("\"}");
        out
    }
}

/// Localize a fault: read the real operating point of `base`, compare the
/// voltage at `target` to `expected ± tolerance`, and if it misses, find the
/// single part whose smallest value correction would restore it.
///
/// This is grounded, not guessed: every candidate's corrected value comes from
/// *actually re-solving* a scratch clone with that part's value changed (never
/// the live circuit). `base` is borrowed immutably. The "smallest correction
/// wins" rule is the minimal single-fault explanation — for a divider with one
/// wrong resistor it fingers exactly that resistor.
pub fn localize_fault(
    base: &Shell,
    target: &str,
    expected: f64,
    tolerance: f64,
) -> Result<FaultReport, AiError> {
    // The real measured voltage at the target node (grounds the whole report).
    // `run op` needs a &mut shell, so read it off a throwaway clone — the caller's
    // circuit is never touched.
    let op = base
        .clone()
        .execute("run op")
        .map_err(|e| AiError::Diagnosis(format!("operating-point solve failed: {e}")))?;
    let measured = parse_node_volts(&op, target).ok_or_else(|| {
        AiError::Diagnosis(format!(
            "node {target:?} is not in the operating point — cannot diagnose it"
        ))
    })?;

    let within_spec = (measured - expected).abs() <= tolerance;
    if within_spec {
        let summary = format!(
            "No fault: v({target}) = {measured:.6} V already meets the expected {expected:.6} V (± {tolerance:.6} V)."
        );
        return Ok(FaultReport {
            target: target.to_string(),
            measured,
            expected,
            tolerance,
            within_spec,
            suspect: None,
            suspects: Vec::new(),
            summary,
        });
    }

    // The raw net behind the target node, resolved once (topology is fixed as we
    // perturb values), so perturbation re-solves can read the same node.
    let target_raw = resolve_target_net(base, target, measured)?;

    // Each valued part is a candidate: find the single-value correction (if any)
    // that lands the target on `expected`, and rank by how small that change is.
    let circuit = base.circuit();
    let mut suspects: Vec<Suspect> = Vec::new();
    for part in circuit.parts() {
        let Some(value) = part.value else { continue };
        if value.magnitude == 0.0 || !value.magnitude.is_finite() {
            continue;
        }
        // An independent source is the reference you debug *against*, not a
        // drifting component — and because a node voltage tracks its supply
        // linearly (unity gain), scaling a source always needs a smaller
        // *log-correction* than any passive, so a log-symmetric metric would
        // otherwise blame the supply on nearly every circuit. Skip them.
        if matches!(part.kind, PartKind::VoltageSource | PartKind::CurrentSource) {
            continue;
        }
        if let Some(scale) = required_scale(
            circuit,
            &part.reference,
            value.unit,
            value.magnitude,
            target_raw,
            expected,
        ) {
            suspects.push(Suspect {
                reference: part.reference.clone(),
                current_value: value.magnitude,
                corrected_value: value.magnitude * scale,
                // Log-symmetric: a 2× and a 0.5× correction are equally large.
                // `scale` is a strictly-positive multiplier from `required_scale`,
                // so `ln` is finite; a non-finite guard keeps ranking total.
                relative_change: if scale > 0.0 {
                    scale.ln().abs()
                } else {
                    f64::INFINITY
                },
            });
        }
    }
    // Smallest correction first — the most economical explanation of the fault.
    // The key is quantized before comparison so two corrections that differ only
    // by solver (bisection) noise land in the same bucket and break their tie
    // deterministically by reference: a divider's two resistors are *genuinely*
    // equal single-fault explanations under the log-symmetric metric, so the
    // winner must not hinge on a ~1e-12 float wobble.
    suspects.sort_by(|a, b| {
        let key = |x: f64| (x * 1e6).round() as i64;
        key(a.relative_change)
            .cmp(&key(b.relative_change))
            .then_with(|| a.reference.cmp(&b.reference))
    });

    let suspect = suspects.first().map(|s| s.reference.clone());
    let summary = match &suspect {
        Some(r) => {
            let s = &suspects[0];
            format!(
                "v({target}) = {measured:.6} V misses the expected {expected:.6} V (± {tolerance:.6} V). \
                 Most likely fault: {r} (currently {cur}, would need ~{cor} to restore v({target})).",
                cur = s.current_value,
                cor = s.corrected_value,
            )
        }
        None => format!(
            "v({target}) = {measured:.6} V misses the expected {expected:.6} V (± {tolerance:.6} V), \
             but no single part correction restores it — the fault may be structural."
        ),
    };

    Ok(FaultReport {
        target: target.to_string(),
        measured,
        expected,
        tolerance,
        within_spec,
        suspect,
        suspects,
        summary,
    })
}

/// Resolve a node name to its raw net id, so perturbation re-solves (which read
/// the engine's `v(n{raw})` signals) can address the same node. Ground resolves
/// to net 0; otherwise the raw net is the one whose operating-point voltage
/// matches the shell's `v(name)` reading. Requires a unique match.
fn resolve_target_net(base: &Shell, target: &str, measured: f64) -> Result<u32, AiError> {
    if matches!(target.to_ascii_lowercase().as_str(), "gnd" | "ground" | "0") {
        return Ok(0);
    }
    let signals = Engine::new()
        .run(base.circuit(), &Analysis::OperatingPoint)
        .map_err(|e| AiError::Diagnosis(format!("operating-point solve failed: {e}")))?;

    // Match on the shell's 6-decimal rendering, exactly what `run op` prints, so
    // the comparison is against the same rounded figure the node was read at.
    let want = format!("{measured:.6}");
    let mut hit = None;
    let mut count = 0usize;
    for s in &signals {
        let Some(raw) = s
            .name
            .strip_prefix("v(n")
            .and_then(|r| r.strip_suffix(')'))
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let v = s.real().and_then(|v| v.first().copied()).unwrap_or(0.0);
        if format!("{v:.6}") == want {
            hit = Some(raw);
            count += 1;
        }
    }
    match (hit, count) {
        (Some(raw), 1) => Ok(raw),
        (_, 0) => Err(AiError::Diagnosis(format!(
            "could not map node {target:?} onto a solved net"
        ))),
        _ => Err(AiError::Diagnosis(format!(
            "node {target:?} is ambiguous (several nets share its voltage) — cannot localize"
        ))),
    }
}

/// The DC voltage at a raw net on a solved operating point (ground is 0 V).
fn net_volts_raw(signals: &[ohmforge_core::signal::Signal], raw: u32) -> f64 {
    if raw == 0 {
        return 0.0;
    }
    let name = format!("v(n{raw})");
    signals
        .iter()
        .find(|s| s.name == name)
        .and_then(|s| s.real())
        .and_then(|v| v.first().copied())
        .unwrap_or(0.0)
}

/// Measure `v(target_raw)` on a clone of `circuit` with one part's value scaled.
fn measure_with(
    circuit: &Circuit,
    reference: &str,
    unit: Unit,
    value: f64,
    target_raw: u32,
) -> f64 {
    let mut scratch = circuit.clone();
    scratch.set_value(reference, Value::new(value, unit));
    match Engine::new().run(&scratch, &Analysis::OperatingPoint) {
        Ok(sigs) => net_volts_raw(&sigs, target_raw),
        Err(_) => f64::NAN,
    }
}

/// The scale factor on a part's value that lands `v(target_raw)` exactly on
/// `expected`, or `None` if no single-value change of this part reaches it.
/// Brackets a sign change of `measure(scale) - expected` over a wide geometric
/// range, then bisects tightly to the true crossing — robust for the monotonic
/// response a resistor/source value has. (The crossing is solved exactly, not to
/// the spec tolerance: the tolerance decides *whether* there is a fault, while
/// the correction reports the value that truly restores the expected behavior.)
fn required_scale(
    circuit: &Circuit,
    reference: &str,
    unit: Unit,
    current: f64,
    target_raw: u32,
    expected: f64,
) -> Option<f64> {
    let f =
        |scale: f64| measure_with(circuit, reference, unit, current * scale, target_raw) - expected;

    // Geometric grid from 1/1000× to 1000× the current value.
    const STEPS: usize = 48;
    let grid: Vec<f64> = (0..=STEPS)
        .map(|i| 10f64.powf(-3.0 + 6.0 * i as f64 / STEPS as f64))
        .collect();

    let mut prev_scale = grid[0];
    let mut prev_f = f(prev_scale);
    for &scale in &grid[1..] {
        let cur_f = f(scale);
        if prev_f.is_finite() && cur_f.is_finite() && (prev_f <= 0.0) != (cur_f <= 0.0) {
            // Sign change: bisect [prev_scale, scale] tightly for the crossing.
            let (mut lo, mut hi) = (prev_scale, scale);
            let mut flo = prev_f;
            for _ in 0..80 {
                let mid = 0.5 * (lo + hi);
                if (hi - lo) <= lo * 1e-12 {
                    break;
                }
                let fmid = f(mid);
                if (flo <= 0.0) != (fmid <= 0.0) {
                    hi = mid;
                } else {
                    lo = mid;
                    flo = fmid;
                }
            }
            return Some(0.5 * (lo + hi));
        }
        prev_scale = scale;
        prev_f = cur_f;
    }
    None
}

/// What a completed [`debug_circuit`] run yields: the model's final text and the
/// grounded fault report it was handed.
#[derive(Debug, Clone)]
pub struct DebugOutcome {
    /// The model's final answer text.
    pub answer: String,
    /// The fault localization report, if the model ran `diagnose`.
    pub report: Option<FaultReport>,
}

/// The debug-mode tool handler: it owns a snapshot of the circuit under
/// investigation plus the expected behavior, and exposes a grounded `read_op`
/// and a `diagnose` tool. Diagnosis runs [`localize_fault`] against the snapshot
/// — never the caller's live shell — and the result is cached in [`report`](Debugger::report).
#[derive(Debug, Clone)]
pub struct Debugger {
    base: Shell,
    target: String,
    expected: f64,
    tolerance: f64,
    report: Option<FaultReport>,
}

impl Debugger {
    /// A debug handler over a snapshot of `base`, checking `target` against
    /// `expected ± tolerance` volts.
    pub fn new(base: Shell, target: impl Into<String>, expected: f64, tolerance: f64) -> Self {
        Debugger {
            base,
            target: target.into(),
            expected,
            tolerance,
            report: None,
        }
    }

    /// The fault report from the last `diagnose` call, if any.
    pub fn report(&self) -> Option<&FaultReport> {
        self.report.as_ref()
    }

    /// The tool definitions to advertise to the model.
    pub fn tool_defs() -> Vec<Tool> {
        vec![
            Tool::new(
                DEBUG_READ_OP_TOOL,
                "Solve the DC operating point of the circuit under investigation and return one \
                 node's voltage as a number. Use it to see what the circuit actually does.",
                r#"{"type":"object","properties":{"node":{"type":"string","description":"the node to read, e.g. \"out\""}},"required":["node"]}"#,
            ),
            Tool::new(
                DIAGNOSE_TOOL,
                "Localize the fault. Reads the real operating point, compares the target node to \
                 its expected voltage, and returns the ranked suspect parts — each with the value \
                 correction that would restore the expected behavior. The smallest correction is \
                 the most likely culprit. Call this, then name the faulty part to the user.",
                r#"{"type":"object","properties":{},"required":[]}"#,
            ),
        ]
    }

    /// The system prompt framing debug mode.
    pub fn system_prompt() -> Vec<SystemBlock> {
        vec![SystemBlock::new(
            "You are the ohmforge debug copilot. A circuit is misbehaving: its target node does \
             not match the expected voltage. Use `read_op` to see the real operating point and \
             `diagnose` to localize the fault — then tell the user which part is faulty, citing \
             the measured value and the correction that would fix it. Ground every claim in the \
             real readings; never guess.",
        )]
    }

    /// Handle the `diagnose` call: localize the fault against the snapshot, cache
    /// it, and hand the report JSON back to the model.
    fn diagnose(&mut self) -> Result<String, String> {
        let report = localize_fault(&self.base, &self.target, self.expected, self.tolerance)
            .map_err(|e| e.to_string())?;
        let json = report.to_json();
        self.report = Some(report);
        Ok(json)
    }

    /// Handle a grounded `read_op` call.
    fn read_op(&mut self, input: &str) -> Result<String, String> {
        let node = json_string_field(input, "node").ok_or_else(|| {
            format!("{DEBUG_READ_OP_TOOL}: missing string field \"node\" in {input}")
        })?;
        let table = self.base.execute("run op").map_err(|e| e.to_string())?;
        let volts = parse_node_volts(&table, &node).ok_or_else(|| {
            format!("{DEBUG_READ_OP_TOOL}: node {node:?} is not in the operating point")
        })?;
        Ok(format!(
            "{{\"node\":\"{}\",\"volts\":{volts},\"display\":\"{volts:.6} V\"}}",
            json_escape(&node)
        ))
    }
}

impl ToolHandler for Debugger {
    fn call(&mut self, name: &str, input: &str) -> Result<String, String> {
        match name {
            DIAGNOSE_TOOL => self.diagnose(),
            DEBUG_READ_OP_TOOL => self.read_op(input),
            other => Err(format!("unknown tool {other:?}")),
        }
    }
}

/// Run debug mode: let the model inspect the faulty circuit and localize the
/// fault, and return its answer plus the grounded [`FaultReport`].
///
/// `base` is borrowed immutably — debugging never changes the user's circuit.
pub fn debug_circuit<T: Transport>(
    client: &Client<T>,
    base: &Shell,
    target: impl Into<String>,
    expected: f64,
    tolerance: f64,
    request: impl Into<String>,
) -> Result<DebugOutcome, AiError> {
    let mut debugger = Debugger::new(base.clone(), target, expected, tolerance);
    let result = client.run(
        &Debugger::system_prompt(),
        &Debugger::tool_defs(),
        request,
        &mut debugger,
    )?;
    Ok(DebugOutcome {
        answer: result.final_text,
        report: debugger.report,
    })
}

// ── Firmware mode ───────────────────────────────────────────────────────────

/// The `emit_firmware` tool name: stage a compiled firmware image (Intel HEX).
pub const EMIT_FIRMWARE_TOOL: &str = "emit_firmware";

/// A failure parsing an Intel HEX firmware image, tagged with the 1-based record
/// line it occurred on (like the format parsers' line-tagged errors).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HexError {
    /// The image had no records at all.
    Empty,
    /// A record did not begin with the `:` start code.
    MissingStart { line: usize },
    /// A record's hex payload had an odd number of nibbles.
    OddLength { line: usize },
    /// A record contained a non-hex-digit character.
    BadDigit { line: usize },
    /// A record was shorter than the mandatory count/address/type/checksum bytes.
    TooShort { line: usize },
    /// A record's declared byte count did not match its data length.
    CountMismatch {
        line: usize,
        declared: usize,
        got: usize,
    },
    /// A record's checksum byte was wrong.
    BadChecksum { line: usize, expected: u8, got: u8 },
    /// The image never terminated with an end-of-file (type 01) record.
    MissingEof,
    /// A record followed the end-of-file record.
    AfterEof { line: usize },
}

impl fmt::Display for HexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HexError::Empty => write!(f, "empty firmware image (no HEX records)"),
            HexError::MissingStart { line } => {
                write!(f, "line {line}: record does not start with ':'")
            }
            HexError::OddLength { line } => write!(f, "line {line}: odd number of hex digits"),
            HexError::BadDigit { line } => write!(f, "line {line}: non-hex digit in record"),
            HexError::TooShort { line } => write!(f, "line {line}: record too short"),
            HexError::CountMismatch {
                line,
                declared,
                got,
            } => write!(
                f,
                "line {line}: byte count says {declared} but found {got} data bytes"
            ),
            HexError::BadChecksum {
                line,
                expected,
                got,
            } => write!(
                f,
                "line {line}: bad checksum (expected {expected:#04x}, got {got:#04x})"
            ),
            HexError::MissingEof => write!(f, "missing end-of-file record (:00000001FF)"),
            HexError::AfterEof { line } => write!(f, "line {line}: record after end-of-file"),
        }
    }
}

impl Error for HexError {}

/// Parse and validate an Intel HEX image, returning the concatenated data bytes
/// (every type-00 record's payload, in order) — the raw firmware the emulator
/// would flash. Validates the start code, hex digits, per-record byte count, the
/// two's-complement checksum, and that the image ends with an EOF record.
///
/// This is the same format `ohmforge-mcu`'s `load_firmware` accepts (an image that
/// begins with `:`), so a validated string here drops straight into the co-sim
/// bridge — the full flash-into-emulator wiring is a thin follow-up.
pub fn parse_intel_hex(src: &str) -> Result<Vec<u8>, HexError> {
    let mut data = Vec::new();
    let mut saw_record = false;
    let mut saw_eof = false;

    for (i, raw) in src.lines().enumerate() {
        let line = i + 1;
        let text = raw.trim();
        if text.is_empty() {
            continue;
        }
        if saw_eof {
            return Err(HexError::AfterEof { line });
        }
        let body = text
            .strip_prefix(':')
            .ok_or(HexError::MissingStart { line })?;
        let bytes = hex_bytes(body, line)?;
        // :LL AAAA TT [data..] CC → at least count+addr(2)+type+checksum = 5 bytes.
        if bytes.len() < 5 {
            return Err(HexError::TooShort { line });
        }
        let count = bytes[0] as usize;
        let rtype = bytes[3];
        let payload = &bytes[4..bytes.len() - 1];
        if payload.len() != count {
            return Err(HexError::CountMismatch {
                line,
                declared: count,
                got: payload.len(),
            });
        }
        // Checksum: two's complement of the sum of every byte but the checksum.
        let sum: u8 = bytes[..bytes.len() - 1]
            .iter()
            .fold(0u8, |acc, &b| acc.wrapping_add(b));
        let expected = 0u8.wrapping_sub(sum);
        let got = bytes[bytes.len() - 1];
        if expected != got {
            return Err(HexError::BadChecksum {
                line,
                expected,
                got,
            });
        }

        saw_record = true;
        match rtype {
            0x00 => data.extend_from_slice(payload),
            0x01 => saw_eof = true,
            _ => {} // extended-address / segment records carry no data we flatten
        }
    }

    if !saw_record {
        return Err(HexError::Empty);
    }
    if !saw_eof {
        return Err(HexError::MissingEof);
    }
    Ok(data)
}

/// Decode a record body (after the `:`) into bytes, validating hex digits.
fn hex_bytes(body: &str, line: usize) -> Result<Vec<u8>, HexError> {
    let chars: Vec<char> = body.chars().collect();
    if chars.len() % 2 != 0 {
        return Err(HexError::OddLength { line });
    }
    let mut out = Vec::with_capacity(chars.len() / 2);
    for pair in chars.chunks(2) {
        let hi = pair[0].to_digit(16).ok_or(HexError::BadDigit { line })?;
        let lo = pair[1].to_digit(16).ok_or(HexError::BadDigit { line })?;
        out.push((hi * 16 + lo) as u8);
    }
    Ok(out)
}

/// A staged firmware image: the Arduino-style source the model wrote, the
/// validated Intel HEX string, and the flat data bytes it decodes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareImage {
    /// The sketch source the model produced (for the user to review).
    pub source: String,
    /// The validated Intel HEX image — ready for `ohmforge-mcu`'s `load_firmware`.
    pub hex: String,
    /// The flat firmware bytes the HEX decodes to.
    pub bytes: Vec<u8>,
}

/// What a completed [`firmware`] run yields: the model's final text and the
/// staged, validated image (if it emitted one).
#[derive(Debug, Clone)]
pub struct FirmwareOutcome {
    /// The model's final answer text.
    pub answer: String,
    /// The staged firmware image, if the model emitted one.
    pub staged: Option<FirmwareImage>,
}

/// The firmware-mode tool handler: the model emits a sketch and its compiled
/// Intel HEX image, which this validates and stages. It touches no emulator —
/// that is the co-sim follow-up — so it is fully testable with a stub model.
#[derive(Debug, Clone, Default)]
pub struct FirmwareBuilder {
    staged: Option<FirmwareImage>,
}

impl FirmwareBuilder {
    /// A fresh builder with nothing staged yet.
    pub fn new() -> Self {
        FirmwareBuilder::default()
    }

    /// The staged image, if the model emitted one.
    pub fn staged(&self) -> Option<&FirmwareImage> {
        self.staged.as_ref()
    }

    /// The `emit_firmware` tool definition.
    pub fn tool_defs() -> Vec<Tool> {
        vec![Tool::new(
            EMIT_FIRMWARE_TOOL,
            "Stage a firmware sketch for the board. Provide the `source` (the Arduino-style \
             sketch) and `hex` (its compiled image as Intel HEX, starting with ':' and ending \
             with the :00000001FF end record). The HEX is validated and staged for the user to \
             flash into the co-simulated emulator; nothing runs without their approval.",
            r#"{"type":"object","properties":{"source":{"type":"string","description":"the sketch source"},"hex":{"type":"string","description":"the compiled image as Intel HEX"}},"required":["source","hex"]}"#,
        )]
    }

    /// The system prompt framing firmware mode.
    pub fn system_prompt() -> Vec<SystemBlock> {
        vec![SystemBlock::new(
            "You are the ohmforge firmware copilot. Write a short sketch for the user's board \
             and emit it with the `emit_firmware` tool as compiled Intel HEX. The image is \
             validated and staged for the user to flash into the co-simulated emulator — it does \
             not run until they approve.",
        )]
    }

    /// Handle `emit_firmware`: validate the HEX and stage the image.
    fn emit(&mut self, input: &str) -> Result<String, String> {
        let source = json_string_field(input, "source").unwrap_or_default();
        let hex = json_string_field(input, "hex").ok_or_else(|| {
            format!("{EMIT_FIRMWARE_TOOL}: missing string field \"hex\" in {input}")
        })?;
        let bytes = parse_intel_hex(&hex).map_err(|e| e.to_string())?;
        let n = bytes.len();
        self.staged = Some(FirmwareImage { source, hex, bytes });
        Ok(format!(
            "{{\"staged\":true,\"bytes\":{n},\"note\":\"validated Intel HEX; ready to flash into the emulator\"}}"
        ))
    }
}

impl ToolHandler for FirmwareBuilder {
    fn call(&mut self, name: &str, input: &str) -> Result<String, String> {
        match name {
            EMIT_FIRMWARE_TOOL => self.emit(input),
            other => Err(format!("unknown tool {other:?}")),
        }
    }
}

/// Run firmware mode: let the model write a sketch and emit its compiled Intel
/// HEX, validate and stage it, and return the staged image for the user to
/// flash. The real flash-into-emulator step (via `ohmforge-mcu`) is a thin follow-up.
pub fn firmware<T: Transport>(
    client: &Client<T>,
    request: impl Into<String>,
) -> Result<FirmwareOutcome, AiError> {
    let mut builder = FirmwareBuilder::new();
    let result = client.run(
        &FirmwareBuilder::system_prompt(),
        &FirmwareBuilder::tool_defs(),
        request,
        &mut builder,
    )?;
    Ok(FirmwareOutcome {
        answer: result.final_text,
        staged: builder.staged,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{ContentBlock, ModelResponse, StopReason, Usage};
    use crate::transport::MockTransport;
    use crate::verify::{Criterion, Spec};

    fn resp(blocks: Vec<ContentBlock>, stop: StopReason) -> ModelResponse {
        ModelResponse {
            content: blocks,
            stop_reason: stop,
            usage: Usage::default(),
        }
    }

    /// A 10 V divider whose top resistor is wrong: R1 = 3k (should be 1k) over a
    /// 1k bottom leg, so v(out) sags to 2.5 V where 5 V is expected.
    fn faulty_divider() -> Shell {
        let mut sh = Shell::new();
        sh.execute("add V1 in gnd 10").unwrap();
        sh.execute("add R1 in out 3k").unwrap(); // the seeded fault (nominal 1k)
        sh.execute("add R2 out gnd 1k").unwrap();
        sh
    }

    // ── Debug mode ───────────────────────────────────────────────────────────

    /// The localization gate: a real sim reads v(out) ≈ 2.5 V against an
    /// expected 5 V, and the minimal-correction rule fingers R1 — the seeded
    /// faulty resistor — not R2 or V1.
    #[test]
    fn localize_fault_fingers_the_wrong_resistor() {
        let base = faulty_divider();
        let report = localize_fault(&base, "out", 5.0, 0.05).expect("localization runs");

        assert!(!report.within_spec, "the divider is out of spec");
        assert!(
            (report.measured - 2.5).abs() < 1e-6,
            "real measured v(out) = {}",
            report.measured
        );
        assert_eq!(report.suspect.as_deref(), Some("R1"), "{}", report.summary);

        // R1's corrected value is ~1k (what restores v(out)=5), and it is the
        // smallest correction of all candidates.
        let r1 = report
            .suspects
            .iter()
            .find(|s| s.reference == "R1")
            .expect("R1 is a candidate");
        assert!(
            (r1.corrected_value - 1000.0).abs() < 1.0,
            "R1 would need ~1k, got {}",
            r1.corrected_value
        );
        assert_eq!(report.suspects[0].reference, "R1", "R1 ranks first");
    }

    /// The fault-ranking metric is **log-symmetric**: each suspect's
    /// `relative_change` is `|ln(corrected/current)|`, not the asymmetric linear
    /// `|corrected/current - 1|`. A halving and a doubling of the same part thus
    /// carry equal weight, so the ranking does not systematically favor faults
    /// that need a *decrease* over ones that need an *increase*.
    #[test]
    fn fault_ranking_metric_is_log_symmetric() {
        let base = faulty_divider();
        let report = localize_fault(&base, "out", 5.0, 0.05).expect("localization runs");
        assert!(!report.suspects.is_empty(), "the divider is out of spec");

        // The independent supply is not a fault candidate (see the skip in
        // `localize_fault`); only the passives are ranked.
        assert!(
            report.suspects.iter().all(|s| s.reference != "V1"),
            "the supply is not a drift-fault candidate: {:?}",
            report.suspects
        );

        // Each suspect's metric is exactly |ln(corrected/current)|.
        for s in &report.suspects {
            let expected = (s.corrected_value / s.current_value).ln().abs();
            assert!(
                (s.relative_change - expected).abs() < 1e-9,
                "{} relative_change {} should be |ln(corrected/current)| = {}",
                s.reference,
                s.relative_change,
                expected
            );
        }

        // The symmetry that the old `|scale - 1|` broke: R1 must shrink 3k -> ~1k
        // (scale 1/3) and R2 must grow 1k -> ~3k (scale 3) to restore v(out). Both
        // are one factor of three, so the log-metric scores them **equally**
        // (~ln 3) — where the old linear metric scored the decrease 0.667 but the
        // increase 2.0, three times larger, purely for being an increase.
        let r1 = report
            .suspects
            .iter()
            .find(|s| s.reference == "R1")
            .expect("R1 is a candidate");
        let r2 = report
            .suspects
            .iter()
            .find(|s| s.reference == "R2")
            .expect("R2 is a candidate");
        assert!(
            (r1.relative_change - 3f64.ln()).abs() < 1e-2,
            "R1's log-correction should be ~ln(3), got {}",
            r1.relative_change
        );
        assert!(
            (r1.relative_change - r2.relative_change).abs() < 1e-2,
            "a decrease and an equal-factor increase must score equally: R1 {} vs R2 {}",
            r1.relative_change,
            r2.relative_change
        );
    }

    /// A circuit already within spec reports no fault.
    #[test]
    fn localize_fault_reports_no_fault_when_within_spec() {
        let mut sh = Shell::new();
        sh.execute("add V1 in gnd 10").unwrap();
        sh.execute("add R1 in out 1k").unwrap();
        sh.execute("add R2 out gnd 1k").unwrap();
        let report = localize_fault(&sh, "out", 5.0, 0.05).unwrap();
        assert!(report.within_spec);
        assert_eq!(report.suspect, None);
        assert!(report.summary.contains("No fault"), "{}", report.summary);
    }

    /// The full debug loop: a STUB model calls `diagnose`, is handed the REAL
    /// report (localizing R1 from a real solve), and names R1 in its answer. The
    /// caller's shell is never touched.
    #[test]
    fn debug_loop_localizes_a_seeded_fault_with_stub_model_and_real_sim() {
        let diagnose = resp(
            vec![ContentBlock::tool_use("d1", DIAGNOSE_TOOL, "{}")],
            StopReason::ToolUse,
        );
        let answer = resp(
            vec![ContentBlock::text(
                "The fault is R1: v(out) measures 2.5 V but should be 5 V, and R1 would need ~1k.",
            )],
            StopReason::EndTurn,
        );
        let client = Client::new(MockTransport::new([diagnose, answer])).with_prompt_cache(false);

        let live = faulty_divider();
        let outcome = debug_circuit(
            &client,
            &live,
            "out",
            5.0,
            0.05,
            "The output voltage is wrong — which part is bad?",
        )
        .expect("debug loop runs");

        // The mode localized the seeded fault from a real sim.
        let report = outcome.report.as_ref().expect("diagnose ran");
        assert_eq!(report.suspect.as_deref(), Some("R1"));
        assert!((report.measured - 2.5).abs() < 1e-6, "{}", report.measured);
        // The model's answer names the localized part.
        assert!(outcome.answer.contains("R1"), "{}", outcome.answer);

        // Debugging never mutated the caller's circuit.
        let mut refs = live.part_refs();
        refs.sort();
        assert_eq!(refs, vec!["R1", "R2", "V1"]);
    }

    // ── Design mode (reuses the verify gate) ──────────────────────────────────

    /// Design mode is the verification loop, reachable through the modes surface:
    /// a stub model proposes a patch, it is verified against a clone, and the
    /// staged result passes without touching the live circuit.
    #[test]
    fn design_mode_runs_the_verification_loop() {
        let mut live = Shell::new();
        live.execute("add V1 in gnd 5").unwrap();
        let spec = Spec::new(
            "5 V -> 3.3 V @ 500 mA",
            vec![
                Criterion::node_volts("out", 3.3, 0.05),
                Criterion::resistor_current("RL", 0.5, 0.02),
            ],
        );
        let propose = resp(
            vec![ContentBlock::tool_use(
                "p1",
                crate::verify::PROPOSE_PATCH_TOOL,
                r#"{"summary":"3.4R dropper into 6.6R load","commands":["add RL out gnd 6.6","add R1 in out 3.4"]}"#,
            )],
            StopReason::ToolUse,
        );
        let present = resp(
            vec![ContentBlock::text("Verified at 3.3 V / 0.5 A.")],
            StopReason::EndTurn,
        );
        let client = Client::new(MockTransport::new([propose, present])).with_prompt_cache(false);

        let outcome = design(&client, &live, spec, "3.3 V at 500 mA please.").unwrap();
        assert!(outcome.is_verified(), "the staged patch is verified");
        assert_eq!(live.part_refs(), vec!["V1"], "live circuit untouched");
    }

    // ── Firmware mode ─────────────────────────────────────────────────────────

    /// A tiny valid Intel HEX image: one 11-byte data record then the EOF record
    /// (the classic `:0B...` "Hello world" fragment from the Intel HEX spec).
    const HELLO_HEX: &str = ":0B0010006164647265737320676170A7\n:00000001FF\n";

    #[test]
    fn parse_intel_hex_accepts_a_valid_image_and_returns_its_bytes() {
        let bytes = parse_intel_hex(HELLO_HEX).expect("valid Intel HEX");
        assert_eq!(bytes.len(), 11, "the single data record's 11 payload bytes");
        assert_eq!(&bytes[0..2], b"ad", "payload decodes to the record data");
    }

    #[test]
    fn parse_intel_hex_rejects_malformed_images() {
        assert_eq!(parse_intel_hex("\n  \n").unwrap_err(), HexError::Empty);
        // No EOF record.
        assert_eq!(
            parse_intel_hex(":0B0010006164647265737320676170A7\n").unwrap_err(),
            HexError::MissingEof
        );
        // Corrupt the checksum (last byte A7 → A8).
        let bad = ":0B0010006164647265737320676170A8\n:00000001FF\n";
        assert!(matches!(
            parse_intel_hex(bad).unwrap_err(),
            HexError::BadChecksum { .. }
        ));
        // Missing start code.
        assert!(matches!(
            parse_intel_hex("0B0010006164647265737320676170A7\n:00000001FF\n").unwrap_err(),
            HexError::MissingStart { .. }
        ));
    }

    /// The firmware seam end-to-end with a stub model: it emits a sketch + HEX
    /// via the tool, the HEX validates, and the image is staged with its bytes.
    #[test]
    fn firmware_loop_stages_a_validated_image() {
        let emit = resp(
            vec![ContentBlock::tool_use(
                "f1",
                EMIT_FIRMWARE_TOOL,
                r#"{"source":"void setup(){pinMode(13,OUTPUT);} void loop(){digitalWrite(13,HIGH);}","hex":":0B0010006164647265737320676170A7\n:00000001FF\n"}"#,
            )],
            StopReason::ToolUse,
        );
        let answer = resp(
            vec![ContentBlock::text(
                "Sketch compiled and staged — approve to flash.",
            )],
            StopReason::EndTurn,
        );
        let client = Client::new(MockTransport::new([emit, answer])).with_prompt_cache(false);

        let outcome = firmware(&client, "Blink the LED on pin 13.").expect("firmware loop runs");
        let image = outcome.staged.as_ref().expect("an image was staged");
        assert_eq!(
            image.bytes.len(),
            11,
            "the validated image carries its bytes"
        );
        assert!(
            image.source.contains("pinMode"),
            "the sketch source is kept"
        );
        assert!(outcome.answer.contains("staged"), "{}", outcome.answer);
    }

    /// A malformed HEX comes back as a recoverable tool error, not a panic or a
    /// bogus staged image.
    #[test]
    fn firmware_rejects_bad_hex_as_a_tool_error() {
        let mut builder = FirmwareBuilder::new();
        let err = builder
            .call(EMIT_FIRMWARE_TOOL, r#"{"source":"x","hex":"nope"}"#)
            .unwrap_err();
        assert!(
            err.contains("':'") || err.to_lowercase().contains("start"),
            "{err}"
        );
        assert!(builder.staged().is_none(), "nothing staged on a bad image");
    }
}
