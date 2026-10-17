//! # ee-ai
//!
//! The AI copilot. The model works through **tools** that map onto the same
//! circuit API the shell uses (add parts, wire, set params, run analyses, read
//! signals) and **verifies its own proposals by re-running the simulator**
//! before presenting them. Nothing is applied without user approval. Offline
//! mode falls back to deterministic heuristics + a local parts index.
//!
//! See docs/AI.md for the full design (tools, verification loop, guardrails).
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
//!   `HttpTransport` (behind the `http` feature) stub for the real network path.
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
//!   [`ee_shell::Shell`], exposing an `eesh` tool (run a command line) and a
//!   `read_op` tool (solve the operating point and read a node's voltage — the
//!   grounding path), plus [`ask`] to run the whole loop and return the answer.
//!
//! With a stubbed model but a real sim, [`ask`] answers a question grounded in
//! a real operating-point read.

pub mod client;
pub mod config;
pub mod error;
pub mod message;
pub mod tools;
pub mod transport;

pub use client::{Client, RunResult, ToolHandler, DEFAULT_MAX_TOKENS, DEFAULT_MAX_TOOL_TURNS};
pub use config::{api_key_from_env, model_from_env, DEFAULT_MODEL};
pub use error::AiError;
pub use message::{
    response_to_events, CacheControl, ContentBlock, ContentDelta, Message, ModelRequest,
    ModelResponse, Role, StopReason, StreamAssembler, StreamEvent, SystemBlock, Tool, Usage,
};
pub use tools::{ask, CircuitTools, EESH_TOOL, READ_OP_TOOL};
pub use transport::{MockTransport, Transport};

/// A change the AI proposes. Staged — never auto-applied; the user runs
/// `apply`/`reject`. (docs/AI.md § Guardrails)
#[derive(Debug, Clone)]
pub struct ProposedPatch {
    pub summary: String,
    /// The eesh commands that would enact the change, shown to the user as a diff.
    pub commands: Vec<String>,
}
