//! # ohmforge-ai
//!
//! The AI copilot. The model works through **tools** that map onto the same
//! circuit API the shell uses (add parts, wire, set params, run analyses, read
//! signals) and **verifies its own proposals by re-running the simulator**
//! before presenting them. Nothing is applied without user approval. Offline
//! mode falls back to deterministic heuristics + a local parts index.
//!
//! See docs/AI.md for the full design (tools, verification loop, guardrails),
//! and `docs/guide/ai-copilot.md` for a usage guide with worked, stub-model
//! walkthroughs of each mode (Ask, Design, Debug, Firmware).
//!
//! ## This checkpoint (CP-AF-M1.2): the Claude client
//!
//! The model is reached through a synchronous [`Transport`] trait, never a
//! hard-wired HTTP client, so the whole copilot loop is unit-testable with **no
//! network** and the crate compiles for the browser without a TLS stack:
//!
//! * [`message`] — Messages-API-shaped types: [`ModelRequest`], [`Message`],
//!   [`ContentBlock`]s (text / tool-use / tool-result), [`Tool`] definitions,
//!   [`ModelResponse`], prompt-cache markers ([`CacheControl`]), and the
//!   streamed-event shape ([`StreamEvent`] + [`StreamAssembler`]).
//! * [`transport`] — the [`Transport`] trait, a [`MockTransport`] that replays
//!   canned responses (including tool-use turns) for tests, and a feature-gated
//!   `HttpTransport` (behind the `http` feature): a **live** Claude Messages
//!   round-trip on native (a `reqwest` blocking client with a request timeout),
//!   with only the `wasm32` `fetch` path left as a documented TODO stub.
//! * [`client`] — the [`Client`] and the **tool-use loop**: send → dispatch any
//!   requested tools through a caller-provided [`ToolHandler`] → feed the
//!   results back → stop on `end_turn`.
//! * [`config`] — the pinned [`DEFAULT_MODEL`], its env overrides, and runtime
//!   API-key handling (never committed).
//!
//! ## This checkpoint (CP-AF-M1.3): tools over the circuit API — grounded ask
//!
//! The generic dispatch seam now carries concrete tools that map onto the same
//! `eesh` shell and simulator a user drives (docs/AI.md § trust model):
//!
//! * [`tools`] — [`CircuitTools`], a [`ToolHandler`] over one live
//!   [`ohmforge_shell::Shell`], exposing an `eesh` tool (run a command line) and a
//!   `read_op` tool (solve the operating point and read a node's voltage — the
//!   grounding path), plus [`ask`] to run the whole loop and return the answer.
//!
//! With a stubbed model but a real sim, [`ask`] answers a question grounded in
//! a real operating-point read.
//!
//! ## This checkpoint (CP-AF-M2.1): the verification loop — the trust mechanism
//!
//! Design/debug answers must be *simulated* before they are shown, and the live
//! circuit must never change without approval (docs/AI.md § The verification
//! loop / Guardrails):
//!
//! * [`verify`](fn@crate::verify) clones the base shell, scratch-applies a [`ProposedPatch`],
//!   re-simulates, and **measures** the [`Spec`] quantities (node voltage, load
//!   current), returning a structured [`Verdict`] that cites the real numbers.
//!   It borrows the base immutably — it cannot touch the live circuit.
//! * [`apply`] is the sole call that commits a patch onto a real shell;
//!   [`reject`] discards one. Applying is always an explicit, separate step.
//! * [`Designer`] + [`design`] tie this to the model: a `propose_patch` tool
//!   stages a patch, the loop verifies it against a clone, and the measured
//!   verdict is fed back so the model iterates on real evidence — nothing
//!   auto-applies.
//!
//! ## This checkpoint (CP-AF-M3.2): offline mode — no API key needed
//!
//! When there is no key, no network, and no model, the copilot still earns its
//! keep through deterministic heuristics:
//!
//! * [`offline`] — design-rule [`check`](offline::check)s over a
//!   [`Circuit`](ohmforge_core::graph::Circuit) (floating node, no path to ground, an
//!   LED with no series resistor, a short across a source), pure value
//!   [`calc`](offline::calc)ulators (divider, RC/RL, series/parallel R, LED
//!   resistor), and a static [`parts`](offline::parts) catalog with keyword
//!   search. It depends on `ohmforge_core` alone — no [`Transport`], no
//!   [`Client`], no key.
//!
//! ## This checkpoint (CP-AF-M3.3): tutor mode — lessons, graded challenges, glossary
//!
//! The teaching surface, graded **deterministically** by the real simulator:
//!
//! * [`tutor`] — a [`Challenge`] pairs a brief with a [`Spec`]; [`Challenge::grade`]
//!   runs the learner's circuit through [`grade`] (the M2 measurement path) and
//!   returns a [`Verdict`] that is PASS against a correct circuit and FAIL,
//!   **naming the miss**, against a wrong one. A [`Lesson`] is ordered [`Step`]s
//!   walked by a [`LessonRunner`] that advances as the circuit meets each step's
//!   goal, and [`define_term`]/[`glossary`] are the contextual term lookup. All
//!   offline; [`coach`] optionally layers the model for prose hints, but the
//!   grade never depends on it.
//!
//! ## This checkpoint (CP-AF-M6.2): AI auto-tune — a closed loop around verify
//!
//! The copilot can now *search* for a component value, not just check one:
//!
//! * [`tune`] — [`auto_tune`] takes an [`AutoTune`] (the part to adjust, the
//!   measured goal, and a value range) and drives a deterministic, gradient-free
//!   **bisection** through the [`verify`](fn@crate::verify) clone-and-measure
//!   path, converging on the value that makes the spec pass and returning a
//!   [`TuneResult`] with the verified [`Verdict`]. Fully offline (real `ohmforge-sim`
//!   solves, no model/key/network) and deterministic — the live circuit is never
//!   touched, only clones.

pub mod client;
pub mod config;
pub mod error;
pub mod message;
pub mod modes;
pub mod offline;
pub mod tools;
pub mod transport;
pub mod tune;
pub mod tutor;
pub mod verify;

pub use client::{Client, RunResult, ToolHandler, DEFAULT_MAX_TOKENS, DEFAULT_MAX_TOOL_TURNS};
pub use config::{api_key_from_env, base_url_from_env, model_from_env, DEFAULT_MODEL};
pub use error::AiError;
pub use message::{
    response_to_events, CacheControl, ContentBlock, ContentDelta, Message, ModelRequest,
    ModelResponse, Role, StopReason, StreamAssembler, StreamEvent, SystemBlock, Tool, Usage,
};
pub use modes::{
    debug_circuit, firmware, localize_fault, parse_intel_hex, DebugOutcome, Debugger, FaultReport,
    FirmwareBuilder, FirmwareImage, FirmwareOutcome, HexError, Suspect,
};
pub use offline::{
    calc::CalcError, check as drc_check, review as offline_review, CatalogEntry, Finding, Rule,
    Severity,
};
pub use tools::{ask, CircuitTools, EESH_TOOL, READ_OP_TOOL};
pub use transport::{MockTransport, Transport};
pub use tune::{auto_tune, AutoTune, TuneResult};
pub use tutor::{
    coach, define as define_term, glossary, search_terms, Challenge, Lesson, LessonRunner, Step,
    StepGoal, StepReport, Term,
};
pub use verify::{
    apply, design, grade, reject, verify, Criterion, DesignOutcome, Designer, Measurement, Outcome,
    Spec, Verdict, PROPOSE_PATCH_TOOL,
};

/// A change the AI proposes. Staged — never auto-applied; the user runs
/// `apply`/`reject`. (docs/AI.md § Guardrails)
#[derive(Debug, Clone)]
pub struct ProposedPatch {
    pub summary: String,
    /// The eesh commands that would enact the change, shown to the user as a diff.
    pub commands: Vec<String>,
}
