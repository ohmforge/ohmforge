//! # ee-ai
//!
//! The AI copilot. The model works through **tools** that map onto the same
//! circuit API the shell uses (add parts, wire, set params, run analyses, read
//! signals) and **verifies its own proposals by re-running the simulator**
//! before presenting them. Nothing is applied without user approval. Offline
//! mode falls back to deterministic heuristics + a local parts index.
//!
//! See docs/AI.md for the full design (tools, verification loop, guardrails).

/// A change the AI proposes. Staged — never auto-applied; the user runs
/// `apply`/`reject`. (docs/AI.md § Guardrails)
#[derive(Debug, Clone)]
pub struct ProposedPatch {
    pub summary: String,
    /// The eesh commands that would enact the change, shown to the user as a diff.
    pub commands: Vec<String>,
}
