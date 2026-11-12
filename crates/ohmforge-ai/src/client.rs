//! The tool-use loop — the unit this checkpoint is really about.
//!
//! Given a [`Transport`], a system prompt, a set of tools, and a user message,
//! [`Client::run`] drives the Messages-API tool loop to completion:
//!
//! ```text
//! send → model replies
//!      → if it asks for tools: run each through the caller's ToolHandler,
//!        append the tool_result blocks as a user turn, resend
//!      → otherwise: stop and return the assembled text
//! ```
//!
//! Nothing here touches the circuit. The [`ToolHandler`] is a generic seam: the
//! actual tools over the circuit API (`add`/`wire`/`set`/`run`/`read`) are the
//! next checkpoint (CP-AF-M1.3). That keeps the loop logic testable in
//! isolation with a trivial handler and a [`MockTransport`](crate::transport::MockTransport).

use crate::config::model_from_env;
use crate::error::AiError;
use crate::message::{
    CacheControl, ContentBlock, Message, ModelRequest, ModelResponse, Role, StopReason,
    StreamEvent, SystemBlock, Tool, Usage,
};
use crate::transport::Transport;

/// The default output-token cap for a request.
pub const DEFAULT_MAX_TOKENS: u32 = 4096;

/// The default ceiling on tool-use round-trips before the loop gives up, as a
/// guard against a model (or mock) that never stops asking for tools.
pub const DEFAULT_MAX_TOOL_TURNS: usize = 16;

/// Dispatches a single tool call for the loop. This is the caller's seam: the
/// loop hands over a tool name and its raw JSON `input`, and the handler
/// returns the result payload the model will see next turn.
///
/// `Ok(content)` becomes a normal tool_result; `Err(message)` becomes a
/// tool_result flagged `is_error`, so a recoverable tool failure is reported to
/// the model (which can retry or explain) rather than aborting the whole loop.
/// To abort instead, a handler can surface the error out-of-band; returning
/// `Err` keeps the conversation going, which is the Messages-API convention.
pub trait ToolHandler {
    /// Run the named tool with the given JSON-text input.
    fn call(&mut self, name: &str, input: &str) -> Result<String, String>;
}

/// Blanket impl so a plain closure can serve as a [`ToolHandler`] in tests and
/// simple callers.
impl<F> ToolHandler for F
where
    F: FnMut(&str, &str) -> Result<String, String>,
{
    fn call(&mut self, name: &str, input: &str) -> Result<String, String> {
        self(name, input)
    }
}

/// The outcome of a completed [`Client::run`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunResult {
    /// The full transcript: the opening user turn, every assistant turn, and
    /// every tool_result turn the loop appended, in order.
    pub messages: Vec<Message>,
    /// The concatenated text of the final assistant turn — the answer.
    pub final_text: String,
    /// Why the final turn stopped (normally [`StopReason::EndTurn`]).
    pub stop_reason: StopReason,
    /// How many tool calls the loop dispatched across all turns.
    pub tool_calls: usize,
    /// Token usage summed over every turn.
    pub usage: Usage,
}

/// The copilot client: a thin driver around a [`Transport`] that assembles
/// requests (with prompt-cache markers), runs the tool loop, and can stream.
#[derive(Debug, Clone)]
pub struct Client<T: Transport> {
    transport: T,
    model: String,
    max_tokens: u32,
    max_tool_turns: usize,
    cache_prompt: bool,
}

impl<T: Transport> Client<T> {
    /// A client over `transport`, with the model resolved from the environment
    /// (`EE_MODEL` / `ANTHROPIC_MODEL`, else [`crate::config::DEFAULT_MODEL`]) and defaults for
    /// everything else.
    pub fn new(transport: T) -> Self {
        Client {
            transport,
            model: model_from_env(),
            max_tokens: DEFAULT_MAX_TOKENS,
            max_tool_turns: DEFAULT_MAX_TOOL_TURNS,
            cache_prompt: true,
        }
    }

    /// Override the model id (otherwise resolved from the environment).
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Override the output-token cap.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Override the tool-turn ceiling.
    pub fn with_max_tool_turns(mut self, turns: usize) -> Self {
        self.max_tool_turns = turns;
        self
    }

    /// Enable or disable prompt-cache markers on the system/tools prefix
    /// (on by default).
    pub fn with_prompt_cache(mut self, on: bool) -> Self {
        self.cache_prompt = on;
        self
    }

    /// The model id this client will send.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Borrow the underlying transport (useful for inspecting a mock in tests).
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Build the request for one turn, applying prompt-cache markers to the
    /// static prefix (the last system block and the last tool) when caching is
    /// on. Marking the final block of the prefix caches everything up to it,
    /// which is the whole system prompt and tool schema — the costly,
    /// unchanging part of a multi-turn session.
    pub fn build_request(
        &self,
        system: &[SystemBlock],
        tools: &[Tool],
        messages: &[Message],
    ) -> ModelRequest {
        let mut system = system.to_vec();
        let mut tools = tools.to_vec();
        if self.cache_prompt {
            if let Some(last) = system.last_mut() {
                last.cache_control = Some(CacheControl::Ephemeral);
            }
            if let Some(last) = tools.last_mut() {
                last.cache_control = Some(CacheControl::Ephemeral);
            }
        }
        ModelRequest {
            model: self.model.clone(),
            system,
            messages: messages.to_vec(),
            tools,
            max_tokens: self.max_tokens,
        }
    }

    /// Run the tool-use loop to completion, without streaming.
    pub fn run(
        &self,
        system: &[SystemBlock],
        tools: &[Tool],
        user_message: impl Into<String>,
        handler: &mut dyn ToolHandler,
    ) -> Result<RunResult, AiError> {
        self.run_streaming(system, tools, user_message, handler, &mut |_| {})
    }

    /// Run the tool-use loop to completion, forwarding every [`StreamEvent`] of
    /// every turn to `on_event`. The non-streaming [`run`](Client::run) is this
    /// with a no-op sink.
    pub fn run_streaming(
        &self,
        system: &[SystemBlock],
        tools: &[Tool],
        user_message: impl Into<String>,
        handler: &mut dyn ToolHandler,
        on_event: &mut dyn FnMut(StreamEvent),
    ) -> Result<RunResult, AiError> {
        let mut messages = vec![Message::user_text(user_message)];
        let mut tool_calls = 0usize;
        let mut usage = Usage::default();
        // The most recent assistant turn's text and stop reason, so the partial
        // run surfaced on a cap hit carries what the model actually produced.
        let mut last_text = String::new();
        let mut last_stop = StopReason::EndTurn;

        for _turn in 0..self.max_tool_turns {
            let req = self.build_request(system, tools, &messages);
            let resp = self.transport.send_streaming(req, on_event)?;
            usage = usage.saturating_add(resp.usage);
            last_text = resp.text();
            last_stop = resp.stop_reason;

            // Record the assistant turn verbatim so the model sees its own
            // tool_use ids when we feed results back.
            messages.push(Message::new(Role::Assistant, resp.content.clone()));

            if resp.wants_tools() {
                let results = self.dispatch_tools(&resp, handler, &mut tool_calls)?;
                messages.push(Message::new(Role::User, results));
                continue;
            }

            return Ok(RunResult {
                final_text: last_text,
                stop_reason: last_stop,
                messages,
                tool_calls,
                usage,
            });
        }

        // Hit the turn cap. Hand back the partial transcript (and any text the
        // model produced so far) alongside the cap signal, rather than dropping
        // everything the run gathered.
        Err(AiError::ToolLoopExhausted {
            turns: self.max_tool_turns,
            partial: Box::new(RunResult {
                final_text: last_text,
                stop_reason: last_stop,
                messages,
                tool_calls,
                usage,
            }),
        })
    }

    /// Run every tool_use block in `resp` through the handler, producing the
    /// matching tool_result blocks for the next user turn.
    fn dispatch_tools(
        &self,
        resp: &ModelResponse,
        handler: &mut dyn ToolHandler,
        tool_calls: &mut usize,
    ) -> Result<Vec<ContentBlock>, AiError> {
        let mut results = Vec::new();
        for block in &resp.content {
            let ContentBlock::ToolUse { id, name, input } = block else {
                continue;
            };
            *tool_calls += 1;
            let result = match handler.call(name, input) {
                Ok(content) => ContentBlock::tool_result(id.clone(), content),
                Err(message) => ContentBlock::tool_error(id.clone(), message),
            };
            results.push(result);
        }
        Ok(results)
    }
}
