//! The copilot's error type.
//!
//! One friendly enum covers every way the client can fail: a transport fault, a
//! malformed stream, a tool-use loop that never settles, and the missing API
//! key the real HTTP transport needs. It implements [`std::fmt::Display`] and
//! [`std::error::Error`] like the rest of the workspace (`SimError`,
//! `ProjectError`), so it drops into `?` chains and `Box<dyn Error>` the same
//! way.

use std::error::Error;
use std::fmt;

/// Anything that can go wrong talking to the model or running the tool loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiError {
    /// The transport could not complete the request (network, HTTP status, or
    /// a stub that is not wired yet). Carries a human-readable reason.
    Transport(String),
    /// A streamed response could not be reassembled — events arrived out of
    /// order, named a block index that was never opened, or ended mid-message.
    MalformedStream(String),
    /// The model kept requesting tools past the configured turn cap, so the
    /// loop bailed out rather than spin forever. Carries the cap that was hit.
    ToolLoopExhausted(usize),
    /// No `ANTHROPIC_API_KEY` is present in the environment. Only the real HTTP
    /// transport needs one; the mock never does.
    MissingApiKey,
}

impl fmt::Display for AiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AiError::Transport(why) => write!(f, "model transport failed: {why}"),
            AiError::MalformedStream(why) => write!(f, "malformed model stream: {why}"),
            AiError::ToolLoopExhausted(cap) => {
                write!(f, "tool-use loop did not settle within {cap} turns")
            }
            AiError::MissingApiKey => {
                write!(
                    f,
                    "no API key found — set ANTHROPIC_API_KEY in the environment"
                )
            }
        }
    }
}

impl Error for AiError {}
