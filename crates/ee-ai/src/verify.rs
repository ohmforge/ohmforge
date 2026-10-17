//! The verification loop (CP-AF-M2.1) — the trust mechanism.
//!
//! A design/debug answer is only trustworthy if the proposed change was
//! *simulated* before it was shown, and if **nothing touches the user's circuit
//! without explicit approval** (docs/AI.md § Guardrails). This module is that
//! loop, built on the M1 client/tools:
//!
//! ```text
//! propose  → a staged patch (a list of eesh commands — a [`ProposedPatch`])
//!          → scratch-apply it to a CLONE of the base shell (never the live one)
//!          → re-simulate the scratch circuit
//!          → measure the spec quantities (node voltage, load current, …)
//!          → a structured [`Verdict`]: per-criterion pass/fail + the real numbers
//!          → present; the user runs `apply` or `reject`
//! ```
//!
//! The split is the whole point:
//!
//! * [`verify`] clones the base shell, replays the patch on that **scratch**
//!   copy, solves it, and returns a [`Verdict`] citing *measured* values. It
//!   borrows the base `&Shell` — it **cannot** mutate the live circuit.
//! * [`apply`] is the one and only call that commits a patch onto a real
//!   `&mut Shell`. [`reject`] consumes a patch and touches nothing.
//!
//! [`Designer`] wires this to the model: it exposes a single `propose_patch`
//! tool. Each proposal is verified against a *clone* of the base and the real
//! measured [`Verdict`] is fed back, so a stubbed model (a
//! [`MockTransport`](crate::transport::MockTransport)) still iterates on *real*
//! simulation evidence. [`design`] runs that loop and returns the staged patch
//! and its verdict for the caller's `apply`/`reject` decision.

use ee_core::graph::{Circuit, NetId, PartKind};
use ee_core::signal::Signal;
use ee_shell::Shell;
use ee_sim::{Analysis, Engine};

use crate::client::{Client, ToolHandler};
use crate::error::AiError;
use crate::message::{SystemBlock, Tool};
use crate::tools::{json_string_array_field, json_string_field, parse_node_volts};
use crate::transport::Transport;
use crate::ProposedPatch;

/// The `propose_patch` tool name: stage a patch for verification.
pub const PROPOSE_PATCH_TOOL: &str = "propose_patch";

/// A single quantity the proposed circuit must hit, with its tolerance.
#[derive(Debug, Clone, PartialEq)]
pub struct Criterion {
    /// What to measure on the scratch circuit.
    pub measurement: Measurement,
    /// The value the measurement must land on.
    pub target: f64,
    /// The absolute tolerance band around `target` (same unit as the
    /// measurement). A measurement passes when `|measured - target| <= tolerance`.
    pub tolerance: f64,
}

impl Criterion {
    /// A node-voltage criterion: `v(node) ≈ target ± tolerance` volts.
    pub fn node_volts(node: impl Into<String>, target: f64, tolerance: f64) -> Self {
        Criterion {
            measurement: Measurement::NodeVolts { node: node.into() },
            target,
            tolerance,
        }
    }

    /// A load-current criterion: the current through resistor `reference`,
    /// `|ΔV|/R ≈ target ± tolerance` amperes.
    pub fn resistor_current(reference: impl Into<String>, target: f64, tolerance: f64) -> Self {
        Criterion {
            measurement: Measurement::ResistorCurrent {
                reference: reference.into(),
            },
            target,
            tolerance,
        }
    }
}

/// A quantity to read off a solved scratch circuit. Each maps onto a real
/// operating-point solve — never a guess (docs/AI.md § Grounded, not guessed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Measurement {
    /// The DC voltage at a named node, read from the shell's `run op` table.
    NodeVolts { node: String },
    /// The DC current magnitude through a resistor, `|v(a) - v(b)| / R`, from a
    /// real operating-point solve. (SPICE computes a resistor's current the same
    /// way — it is measured, not invented.)
    ResistorCurrent { reference: String },
}

impl Measurement {
    /// A short label for the measured quantity, e.g. `v(out)` or `i(RL)`.
    pub fn label(&self) -> String {
        match self {
            Measurement::NodeVolts { node } => format!("v({node})"),
            Measurement::ResistorCurrent { reference } => format!("i({reference})"),
        }
    }

    /// The SI unit symbol of the measured quantity.
    pub fn unit(&self) -> &'static str {
        match self {
            Measurement::NodeVolts { .. } => "V",
            Measurement::ResistorCurrent { .. } => "A",
        }
    }
}

/// The design target: the quantities the proposed circuit must meet, with a
/// human label (e.g. `"5 V → 3.3 V @ 500 mA"`).
#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    /// A one-line description of the goal, echoed into the verdict summary.
    pub goal: String,
    /// Each quantity that must be met for the verdict to pass.
    pub criteria: Vec<Criterion>,
}

impl Spec {
    /// A spec with the given goal label and criteria.
    pub fn new(goal: impl Into<String>, criteria: Vec<Criterion>) -> Self {
        Spec {
            goal: goal.into(),
            criteria,
        }
    }
}

/// One criterion's measured result: the real number, the target band, and
/// whether it fell inside it.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// The measured quantity's label, e.g. `v(out)`.
    pub label: String,
    /// The SI unit symbol.
    pub unit: &'static str,
    /// The value measured on the scratch circuit.
    pub measured: f64,
    /// The spec target.
    pub target: f64,
    /// The spec tolerance band.
    pub tolerance: f64,
    /// `true` when `|measured - target| <= tolerance`.
    pub pass: bool,
}

impl Outcome {
    /// How far the measurement sits from its target (always `>= 0`).
    pub fn gap(&self) -> f64 {
        (self.measured - self.target).abs()
    }
}

/// The result of verifying a staged patch: does the *simulated* circuit meet
/// the spec, with the real measured numbers to show for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    /// `true` only when every criterion passed.
    pub pass: bool,
    /// One [`Outcome`] per spec criterion, in order.
    pub outcomes: Vec<Outcome>,
    /// A human summary citing every measured value — the text presented to the
    /// user alongside the diff.
    pub summary: String,
}

impl Verdict {
    /// A compact JSON rendering fed back to the model so it can cite the numbers
    /// and iterate on a miss. Shape:
    /// `{"pass":<bool>,"criteria":[{"label":..,"measured":..,"target":..,
    /// "tolerance":..,"unit":"..","pass":<bool>},..],"summary":".."}`.
    pub fn to_json(&self) -> String {
        let mut out = format!("{{\"pass\":{},\"criteria\":[", self.pass);
        for (i, o) in self.outcomes.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"label\":\"{}\",\"measured\":{},\"target\":{},\"tolerance\":{},\"unit\":\"{}\",\"pass\":{}}}",
                o.label, o.measured, o.target, o.tolerance, o.unit, o.pass
            ));
        }
        out.push_str("],\"summary\":\"");
        out.push_str(&json_escape_min(&self.summary));
        out.push_str("\"}");
        out
    }
}

/// Verify a staged `patch` against `spec` **without touching** `base`: clone the
/// base shell, replay the patch on that scratch copy, re-simulate, and measure.
///
/// The live circuit cannot change here — `base` is borrowed immutably and only
/// the clone is driven. Returns a [`Verdict`] with the real measured numbers, or
/// an [`AiError::Patch`] if a command was rejected or the spec named something
/// the patched circuit does not have.
pub fn verify(base: &Shell, patch: &ProposedPatch, spec: &Spec) -> Result<Verdict, AiError> {
    let mut scratch = base.clone();
    apply_commands(&mut scratch, &patch.commands)?;
    measure(&mut scratch, spec)
}

/// Commit a staged patch onto the real shell. This is the **only** call in the
/// loop that mutates the user's circuit — it runs after an explicit `apply`.
pub fn apply(shell: &mut Shell, patch: &ProposedPatch) -> Result<(), AiError> {
    apply_commands(shell, &patch.commands)
}

/// Discard a staged patch. A no-op on any circuit: propose/verify never touch
/// the live shell, so rejecting is simply declining to [`apply`]. Takes the
/// patch by value so a rejected patch cannot then be applied by mistake.
pub fn reject(patch: ProposedPatch) {
    let _ = patch;
}

/// Replay a patch's commands on `shell`, mapping any shell error into an
/// [`AiError::Patch`] that names the offending line.
fn apply_commands(shell: &mut Shell, commands: &[String]) -> Result<(), AiError> {
    for cmd in commands {
        shell
            .execute(cmd)
            .map_err(|e| AiError::Patch(format!("command {cmd:?} failed: {e}")))?;
    }
    Ok(())
}

/// Solve the scratch circuit and score every criterion against its target.
fn measure(scratch: &mut Shell, spec: &Spec) -> Result<Verdict, AiError> {
    // One `run op` for any node-voltage reads (named-node table), …
    let op_table = if spec
        .criteria
        .iter()
        .any(|c| matches!(c.measurement, Measurement::NodeVolts { .. }))
    {
        Some(
            scratch
                .execute("run op")
                .map_err(|e| AiError::Patch(format!("operating-point solve failed: {e}")))?,
        )
    } else {
        None
    };
    // … and one engine op for any current reads (raw-net voltages + Ohm's law).
    let signals = if spec
        .criteria
        .iter()
        .any(|c| matches!(c.measurement, Measurement::ResistorCurrent { .. }))
    {
        Some(
            Engine::new()
                .run(scratch.circuit(), &Analysis::OperatingPoint)
                .map_err(|e| AiError::Patch(format!("operating-point solve failed: {e}")))?,
        )
    } else {
        None
    };

    let mut outcomes = Vec::with_capacity(spec.criteria.len());
    for c in &spec.criteria {
        let measured = match &c.measurement {
            Measurement::NodeVolts { node } => {
                let table = op_table
                    .as_deref()
                    .expect("op table present for node reads");
                parse_node_volts(table, node).ok_or_else(|| {
                    AiError::Patch(format!(
                        "spec reads node {node:?}, which the patched circuit has no voltage for"
                    ))
                })?
            }
            Measurement::ResistorCurrent { reference } => {
                let sigs = signals
                    .as_deref()
                    .expect("signals present for current reads");
                resistor_current(scratch.circuit(), reference, sigs)?
            }
        };
        let pass = (measured - c.target).abs() <= c.tolerance;
        outcomes.push(Outcome {
            label: c.measurement.label(),
            unit: c.measurement.unit(),
            measured,
            target: c.target,
            tolerance: c.tolerance,
            pass,
        });
    }

    let pass = outcomes.iter().all(|o| o.pass);
    let summary = summarize(&spec.goal, pass, &outcomes);
    Ok(Verdict {
        pass,
        outcomes,
        summary,
    })
}

/// The current magnitude through a resistor from a solved operating point:
/// `|v(a) - v(b)| / R`, with the node voltages read off the engine's raw-net
/// signals. Errors if the part is missing, is not a valued resistor, or lacks
/// two terminals.
fn resistor_current(
    circuit: &Circuit,
    reference: &str,
    signals: &[Signal],
) -> Result<f64, AiError> {
    let part = circuit.part_by_ref(reference).ok_or_else(|| {
        AiError::Patch(format!(
            "spec reads current through {reference:?}, which the patched circuit does not contain"
        ))
    })?;
    if !matches!(part.kind, PartKind::Resistor) {
        return Err(AiError::Patch(format!(
            "part {reference:?} is not a resistor — cannot measure its current by Ohm's law"
        )));
    }
    let r = part
        .value
        .map(|v| v.magnitude)
        .ok_or_else(|| AiError::Patch(format!("resistor {reference:?} has no value")))?;
    if r == 0.0 {
        return Err(AiError::Patch(format!(
            "resistor {reference:?} has zero resistance"
        )));
    }
    let [a, b] = part.terminals.as_slice() else {
        return Err(AiError::Patch(format!(
            "resistor {reference:?} does not have exactly two terminals"
        )));
    };
    let va = net_volts(signals, circuit.resolve(*a));
    let vb = net_volts(signals, circuit.resolve(*b));
    Ok((va - vb).abs() / r)
}

/// The DC voltage at a resolved net, from the engine's `v(n{raw})` signals.
/// Ground is 0 V; a net with no signal row reads 0 V.
fn net_volts(signals: &[Signal], net: NetId) -> f64 {
    if net == NetId::GROUND {
        return 0.0;
    }
    let name = format!("v(n{})", net.0);
    signals
        .iter()
        .find(|s| s.name == name)
        .and_then(|s| s.real())
        .and_then(|v| v.first().copied())
        .unwrap_or(0.0)
}

/// Build the human verdict line, citing every measured value against its target.
fn summarize(goal: &str, pass: bool, outcomes: &[Outcome]) -> String {
    let head = if pass { "PASS" } else { "FAIL" };
    let mut s = format!("{head} — {goal}.");
    for o in outcomes {
        if o.pass {
            s.push_str(&format!(
                " {} = {:.6} {} (target {:.6} ± {:.6} {}).",
                o.label, o.measured, o.unit, o.target, o.tolerance, o.unit
            ));
        } else {
            s.push_str(&format!(
                " {} = {:.6} {} misses target {:.6} ± {:.6} {} (off by {:.6} {}).",
                o.label,
                o.measured,
                o.unit,
                o.target,
                o.tolerance,
                o.unit,
                o.gap(),
                o.unit
            ));
        }
    }
    s
}

/// Minimal JSON string escaping for the summary embedded in [`Verdict::to_json`].
fn json_escape_min(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// The copilot's design handler: it stages patches from the model and verifies
/// each against a **clone** of the base circuit, so the tools can never mutate
/// the user's live shell. Construct it with the base circuit and the [`Spec`],
/// drive it with [`Client::run`](crate::client::Client::run), then read
/// [`staged`](Designer::staged) / [`verdict`](Designer::verdict).
#[derive(Debug, Clone)]
pub struct Designer {
    base: Shell,
    spec: Spec,
    staged: Option<ProposedPatch>,
    verdict: Option<Verdict>,
}

impl Designer {
    /// A design handler over a snapshot of `base` and the target `spec`. The
    /// snapshot is the handler's own copy — the caller's shell is untouched.
    pub fn new(base: Shell, spec: Spec) -> Self {
        Designer {
            base,
            spec,
            staged: None,
            verdict: None,
        }
    }

    /// The last patch the model proposed, if any (the staged diff to `apply`).
    pub fn staged(&self) -> Option<&ProposedPatch> {
        self.staged.as_ref()
    }

    /// The verdict from verifying the last proposed patch, if any.
    pub fn verdict(&self) -> Option<&Verdict> {
        self.verdict.as_ref()
    }

    /// The `propose_patch` tool definition to advertise to the model.
    pub fn tool_defs() -> Vec<Tool> {
        vec![Tool::new(
            PROPOSE_PATCH_TOOL,
            "Stage a circuit change for verification. Provide a one-line `summary` and the \
             `commands` (eesh lines like \"add R1 in out 3.4\") that enact it. The patch is \
             applied to a SCRATCH copy of the circuit, simulated, and the spec quantities are \
             measured; you get back the real measured numbers and per-criterion pass/fail. \
             Nothing is applied to the user's circuit — iterate until it passes, then present it.",
            r#"{"type":"object","properties":{"summary":{"type":"string","description":"one line describing the change"},"commands":{"type":"array","items":{"type":"string"},"description":"the eesh commands that enact the patch"}},"required":["summary","commands"]}"#,
        )]
    }

    /// The system prompt framing the verify-before-present contract.
    pub fn system_prompt() -> Vec<SystemBlock> {
        vec![SystemBlock::new(
            "You are the ee-assist design copilot. Propose a circuit change only through the \
             `propose_patch` tool: it scratch-applies your patch, simulates it, and returns the \
             REAL measured spec quantities. Never present a design you have not verified passes. \
             If a proposal misses the spec, read the measured gap and propose a corrected patch. \
             Nothing is applied to the user's circuit without their approval.",
        )]
    }

    /// Handle one `propose_patch` call: stage the patch, verify it against a
    /// clone of the base, record it, and return the verdict JSON to the model.
    fn propose(&mut self, input: &str) -> Result<String, String> {
        let summary = json_string_field(input, "summary").unwrap_or_default();
        let commands = json_string_array_field(input, "commands").ok_or_else(|| {
            format!("{PROPOSE_PATCH_TOOL}: missing array field \"commands\" in {input}")
        })?;
        let patch = ProposedPatch { summary, commands };
        let verdict = verify(&self.base, &patch, &self.spec).map_err(|e| e.to_string())?;
        let json = verdict.to_json();
        self.staged = Some(patch);
        self.verdict = Some(verdict);
        Ok(json)
    }
}

impl ToolHandler for Designer {
    fn call(&mut self, name: &str, input: &str) -> Result<String, String> {
        match name {
            PROPOSE_PATCH_TOOL => self.propose(input),
            other => Err(format!("unknown tool {other:?}")),
        }
    }
}

/// What a completed [`design`] run yields: the model's final text, and the
/// staged patch plus its verdict for the caller's `apply`/`reject` decision.
#[derive(Debug, Clone)]
pub struct DesignOutcome {
    /// The model's final answer text.
    pub answer: String,
    /// The last patch the model staged, if it proposed one.
    pub proposal: Option<ProposedPatch>,
    /// The verification verdict for that patch, if one was proposed.
    pub verdict: Option<Verdict>,
}

impl DesignOutcome {
    /// `true` when a patch was proposed and its verdict passed — i.e. there is a
    /// verified change ready for the user to `apply`.
    pub fn is_verified(&self) -> bool {
        self.verdict.as_ref().is_some_and(|v| v.pass)
    }
}

/// Run the design loop: let the model propose and iterate on patches (each
/// verified against a clone of `base`), and return the staged patch + verdict.
///
/// `base` is borrowed immutably — this function **cannot** change the user's
/// circuit. Committing the result is a separate, explicit [`apply`] by the
/// caller after the user approves.
pub fn design<T: Transport>(
    client: &Client<T>,
    base: &Shell,
    spec: Spec,
    request: impl Into<String>,
) -> Result<DesignOutcome, AiError> {
    let mut designer = Designer::new(base.clone(), spec);
    let result = client.run(
        &Designer::system_prompt(),
        &Designer::tool_defs(),
        request,
        &mut designer,
    )?;
    Ok(DesignOutcome {
        answer: result.final_text,
        proposal: designer.staged,
        verdict: designer.verdict,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Client;
    use crate::message::{ContentBlock, ModelResponse, StopReason, Usage};
    use crate::transport::MockTransport;

    fn resp(blocks: Vec<ContentBlock>, stop: StopReason) -> ModelResponse {
        ModelResponse {
            content: blocks,
            stop_reason: stop,
            usage: Usage::default(),
        }
    }

    /// The user's starting circuit: a bare 5 V rail on node `in`.
    fn user_shell() -> Shell {
        let mut sh = Shell::new();
        sh.execute("add V1 in gnd 5").unwrap();
        sh
    }

    /// The spec under test: `out` at 3.3 V (±50 mV) while the load draws
    /// 500 mA (±20 mA). The 6.6 Ω load pulls exactly 500 mA at 3.3 V, so a
    /// patch that holds 3.3 V also hits the current target.
    fn spec_3v3_500ma() -> Spec {
        Spec::new(
            "5 V -> 3.3 V @ 500 mA",
            vec![
                Criterion::node_volts("out", 3.3, 0.05),
                Criterion::resistor_current("RL", 0.5, 0.02),
            ],
        )
    }

    /// A known-GOOD patch: a 3.4 Ω series dropper into a 6.6 Ω load. From 5 V:
    /// I = 5 / (3.4 + 6.6) = 500 mA, v(out) = 0.5 · 6.6 = 3.3 V.
    fn good_patch() -> ProposedPatch {
        ProposedPatch {
            summary: "drop 5 V to 3.3 V with a 3.4 Ω series R into the 6.6 Ω (500 mA) load".into(),
            commands: vec!["add RL out gnd 6.6".into(), "add R1 in out 3.4".into()],
        }
    }

    /// A known-BAD patch: the series resistor is 10 Ω, far too large. v(out)
    /// sags to ~1.99 V and the load current to ~0.30 A — both miss.
    fn bad_patch() -> ProposedPatch {
        ProposedPatch {
            summary: "drop 5 V with a 10 Ω series R (wrong value)".into(),
            commands: vec!["add RL out gnd 6.6".into(), "add R1 in out 10".into()],
        }
    }

    /// Meets-spec: scratch-apply a good patch, re-sim, measure — the verdict is
    /// PASS citing the real ~3.3 V / ~0.5 A, and the base shell is UNCHANGED
    /// until `apply`, after which it reflects the patch.
    #[test]
    fn verify_passes_a_good_patch_without_touching_the_base() {
        let base = user_shell();
        let spec = spec_3v3_500ma();
        let patch = good_patch();

        let verdict = verify(&base, &patch, &spec).expect("verification runs");

        // PASS, with real measured numbers near the targets.
        assert!(verdict.pass, "good patch meets spec: {}", verdict.summary);
        let vout = &verdict.outcomes[0];
        let iload = &verdict.outcomes[1];
        assert!(
            (vout.measured - 3.3).abs() < 0.05,
            "v(out) = {}",
            vout.measured
        );
        assert!(
            (iload.measured - 0.5).abs() < 0.02,
            "i(RL) = {}",
            iload.measured
        );
        assert!(verdict.summary.contains("PASS"), "{}", verdict.summary);
        assert!(
            verdict.summary.contains("3.3"),
            "cites v(out): {}",
            verdict.summary
        );

        // The base circuit was NOT mutated by verification — still just V1.
        assert_eq!(
            base.part_refs(),
            vec!["V1"],
            "verify left the base untouched"
        );

        // Only an explicit `apply` commits the patch onto the real shell.
        let mut live = base.clone();
        apply(&mut live, &patch).expect("apply commits the patch");
        let mut refs = live.part_refs();
        refs.sort();
        assert_eq!(
            refs,
            vec!["R1", "RL", "V1"],
            "after apply the circuit holds the patch"
        );
    }

    /// Fails-spec: a wrong-value patch → FAIL with the measured-vs-target gap,
    /// and the base is still untouched (nothing auto-applies).
    #[test]
    fn verify_fails_a_bad_patch_and_reports_the_gap() {
        let base = user_shell();
        let spec = spec_3v3_500ma();
        let patch = bad_patch();

        let verdict = verify(&base, &patch, &spec).expect("verification runs");

        assert!(!verdict.pass, "bad patch misses spec: {}", verdict.summary);
        let vout = &verdict.outcomes[0];
        assert!(!vout.pass, "v(out) criterion fails");
        assert!(
            vout.measured < 2.5,
            "v(out) sagged well below 3.3 V: {}",
            vout.measured
        );
        assert!(
            vout.gap() > 1.0,
            "the reported gap is the real miss: {}",
            vout.gap()
        );
        assert!(verdict.summary.contains("FAIL"), "{}", verdict.summary);
        assert!(
            verdict.summary.contains("misses target"),
            "{}",
            verdict.summary
        );

        // Still nothing applied.
        assert_eq!(
            base.part_refs(),
            vec!["V1"],
            "a failing verify never mutates the base"
        );
    }

    /// Reject leaves the base circuit untouched — rejecting is declining to
    /// apply, and the base was never in reach of propose/verify anyway.
    #[test]
    fn reject_leaves_the_base_untouched() {
        let base = user_shell();
        let verdict = verify(&base, &good_patch(), &spec_3v3_500ma()).unwrap();
        assert!(verdict.pass);

        reject(good_patch());

        assert_eq!(base.part_refs(), vec!["V1"], "reject changed nothing");
    }

    /// The full model-driven loop: a STUB model proposes the good patch via the
    /// `propose_patch` tool; the loop scratch-applies, re-sims, and feeds the
    /// REAL verdict back. The user's shell is unchanged until the caller applies
    /// the returned, verified patch.
    #[test]
    fn design_loop_verifies_then_the_caller_applies() {
        // Scripted model: turn 1 proposes the good patch; turn 2 presents it.
        let propose = resp(
            vec![ContentBlock::tool_use(
                "p1",
                PROPOSE_PATCH_TOOL,
                r#"{"summary":"3.4R dropper into 6.6R load","commands":["add RL out gnd 6.6","add R1 in out 3.4"]}"#,
            )],
            StopReason::ToolUse,
        );
        let present = resp(
            vec![ContentBlock::text(
                "Verified: v(out) = 3.300000 V at 0.500000 A. Apply to add R1 and RL.",
            )],
            StopReason::EndTurn,
        );
        let client = Client::new(MockTransport::new([propose, present])).with_prompt_cache(false);

        let mut live = user_shell();
        let outcome = design(
            &client,
            &live,
            spec_3v3_500ma(),
            "Give me 3.3 V at 500 mA from the 5 V rail.",
        )
        .expect("design loop runs");

        // The loop verified a real patch and it passed.
        assert!(outcome.is_verified(), "the staged patch is verified");
        let verdict = outcome.verdict.as_ref().unwrap();
        assert!(
            verdict.pass && verdict.summary.contains("PASS"),
            "{}",
            verdict.summary
        );
        assert!(
            (verdict.outcomes[0].measured - 3.3).abs() < 0.05,
            "grounded v(out) = {}",
            verdict.outcomes[0].measured
        );

        // The user's live shell is STILL untouched after propose + verify.
        assert_eq!(
            live.part_refs(),
            vec!["V1"],
            "design never mutated the live circuit"
        );

        // The explicit, user-driven apply is what finally commits it.
        let patch = outcome.proposal.as_ref().expect("a patch was staged");
        apply(&mut live, patch).expect("apply commits");
        let mut refs = live.part_refs();
        refs.sort();
        assert_eq!(refs, vec!["R1", "RL", "V1"]);
    }

    /// The model iterates: a first BAD proposal comes back FAIL with the real
    /// gap, the model reads it and proposes a corrected patch that PASSES. The
    /// staged result is the good one, and the live circuit is untouched until
    /// apply.
    #[test]
    fn design_loop_lets_the_model_iterate_on_a_failed_verdict() {
        let first = resp(
            vec![ContentBlock::tool_use(
                "p1",
                PROPOSE_PATCH_TOOL,
                r#"{"summary":"10R dropper (too big)","commands":["add RL out gnd 6.6","add R1 in out 10"]}"#,
            )],
            StopReason::ToolUse,
        );
        let second = resp(
            vec![ContentBlock::tool_use(
                "p2",
                PROPOSE_PATCH_TOOL,
                r#"{"summary":"corrected 3.4R dropper","commands":["add RL out gnd 6.6","add R1 in out 3.4"]}"#,
            )],
            StopReason::ToolUse,
        );
        let present = resp(
            vec![ContentBlock::text("Now it passes at 3.3 V.")],
            StopReason::EndTurn,
        );
        let client =
            Client::new(MockTransport::new([first, second, present])).with_prompt_cache(false);

        let live = user_shell();
        let outcome = design(&client, &live, spec_3v3_500ma(), "3.3 V at 500 mA please.").unwrap();

        // Two proposals were verified; the staged/winning one passes.
        assert_eq!(
            outcome.proposal.as_ref().unwrap().commands[1],
            "add R1 in out 3.4"
        );
        assert!(outcome.is_verified(), "final staged patch passes");
        assert_eq!(
            live.part_refs(),
            vec!["V1"],
            "iteration never touched the live circuit"
        );
    }

    /// A patch with a broken command surfaces as `AiError::Patch`, not a bogus
    /// verdict or a panic.
    #[test]
    fn a_broken_patch_command_is_a_patch_error() {
        let base = user_shell();
        let patch = ProposedPatch {
            summary: "typo".into(),
            commands: vec!["add RL out gnd 6.6".into(), "frobnicate out".into()],
        };
        let err = verify(&base, &patch, &spec_3v3_500ma()).unwrap_err();
        assert!(matches!(err, AiError::Patch(_)), "{err}");
        assert!(err.to_string().contains("frobnicate"), "{err}");
    }

    /// `to_json` carries the pass flag, each criterion's measured number, and
    /// the summary — the payload the model reads to iterate.
    #[test]
    fn verdict_json_carries_the_measured_numbers() {
        let verdict = verify(&user_shell(), &good_patch(), &spec_3v3_500ma()).unwrap();
        let json = verdict.to_json();
        assert!(json.contains("\"pass\":true"), "{json}");
        assert!(json.contains("\"label\":\"v(out)\""), "{json}");
        assert!(json.contains("\"label\":\"i(RL)\""), "{json}");
        assert!(
            json.contains("\"unit\":\"V\"") && json.contains("\"unit\":\"A\""),
            "{json}"
        );
    }
}
