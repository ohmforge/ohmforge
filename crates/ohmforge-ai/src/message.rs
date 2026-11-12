//! Messages-API-shaped types: requests, messages, content blocks, tools,
//! responses, and the streamed-event shape that assembles into a full message.
//!
//! These mirror the wire shape of Anthropic's Messages API closely enough that
//! the tool-use loop and prompt-caching markers are faithful, but they carry no
//! serialization: a tool's input and a tool's result are opaque JSON *text*
//! ([`String`]), which keeps `ohmforge-ai` dependency-free and wasm-friendly. The
//! real HTTP transport (a feature-gated stub today) is the only place that
//! would ever turn these into bytes.

use crate::error::AiError;

/// Who authored a [`Message`]. The Messages API only has these two roles; the
/// system prompt lives in its own field on the request, not as a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// Anthropic's `cache_control` marker. Present on a block means "cache the
/// prefix up to and including this block"; we only model the one variant the
/// API offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheControl {
    /// `{"type": "ephemeral"}` — the short-lived prompt cache.
    Ephemeral,
}

/// One piece of a [`Message`]'s content. A single message interleaves these:
/// an assistant turn can carry explanatory [`Text`](ContentBlock::Text) *and*
/// one or more [`ToolUse`](ContentBlock::ToolUse) requests; the following user
/// turn carries the matching [`ToolResult`](ContentBlock::ToolResult)s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentBlock {
    /// Plain text.
    Text { text: String },
    /// The model asking to run a tool. `input` is the raw JSON object text the
    /// model produced for the tool's arguments.
    ToolUse {
        /// Correlates this call with its [`ToolResult`](ContentBlock::ToolResult).
        id: String,
        /// The tool's name, matching a [`Tool::name`].
        name: String,
        /// The tool arguments as JSON object text (opaque to this crate).
        input: String,
    },
    /// The outcome of a tool call, fed back to the model on the next turn.
    ToolResult {
        /// The [`ToolUse::id`](ContentBlock::ToolUse) this answers.
        tool_use_id: String,
        /// The result payload (JSON or plain text — the model reads it either way).
        content: String,
        /// `true` if the tool failed; the model is told so it can recover.
        is_error: bool,
    },
}

impl ContentBlock {
    /// A text block.
    pub fn text(text: impl Into<String>) -> Self {
        ContentBlock::Text { text: text.into() }
    }

    /// A tool-use request block.
    pub fn tool_use(
        id: impl Into<String>,
        name: impl Into<String>,
        input: impl Into<String>,
    ) -> Self {
        ContentBlock::ToolUse {
            id: id.into(),
            name: name.into(),
            input: input.into(),
        }
    }

    /// A successful tool-result block.
    pub fn tool_result(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        ContentBlock::ToolResult {
            tool_use_id: tool_use_id.into(),
            content: content.into(),
            is_error: false,
        }
    }

    /// A failed tool-result block (`is_error = true`).
    pub fn tool_error(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        ContentBlock::ToolResult {
            tool_use_id: tool_use_id.into(),
            content: content.into(),
            is_error: true,
        }
    }
}

/// A conversation turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    /// A user message carrying a single text block — the usual way a turn
    /// starts.
    pub fn user_text(text: impl Into<String>) -> Self {
        Message {
            role: Role::User,
            content: vec![ContentBlock::text(text)],
        }
    }

    /// A message with an explicit role and content blocks.
    pub fn new(role: Role, content: Vec<ContentBlock>) -> Self {
        Message { role, content }
    }

    /// Every [`ToolUse`](ContentBlock::ToolUse) block in this message.
    pub fn tool_uses(&self) -> impl Iterator<Item = &ContentBlock> {
        self.content
            .iter()
            .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
    }

    /// The concatenation of this message's text blocks (tool blocks skipped).
    pub fn text(&self) -> String {
        let mut out = String::new();
        for block in &self.content {
            if let ContentBlock::Text { text } = block {
                out.push_str(text);
            }
        }
        out
    }
}

/// One block of the system prompt. The Messages API accepts the system prompt
/// as an array of text blocks so individual blocks can be marked cacheable; we
/// model exactly that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemBlock {
    pub text: String,
    pub cache_control: Option<CacheControl>,
}

impl SystemBlock {
    /// A plain, un-cached system block.
    pub fn new(text: impl Into<String>) -> Self {
        SystemBlock {
            text: text.into(),
            cache_control: None,
        }
    }

    /// This block, marked for the ephemeral prompt cache.
    pub fn cached(mut self) -> Self {
        self.cache_control = Some(CacheControl::Ephemeral);
        self
    }
}

/// A tool the model may call. The `input_schema` is JSON Schema *text*; this
/// crate never inspects it, it only passes it through to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub input_schema: String,
    pub cache_control: Option<CacheControl>,
}

impl Tool {
    /// A tool definition with no cache marker.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: impl Into<String>,
    ) -> Self {
        Tool {
            name: name.into(),
            description: description.into(),
            input_schema: input_schema.into(),
            cache_control: None,
        }
    }

    /// This tool, marked for the ephemeral prompt cache.
    pub fn cached(mut self) -> Self {
        self.cache_control = Some(CacheControl::Ephemeral);
        self
    }
}

/// A request to the Messages API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRequest {
    pub model: String,
    pub system: Vec<SystemBlock>,
    pub messages: Vec<Message>,
    pub tools: Vec<Tool>,
    pub max_tokens: u32,
}

/// Why the model stopped generating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// A natural end of turn — the loop is done.
    EndTurn,
    /// The model wants one or more tools run before it continues.
    ToolUse,
    /// The output hit `max_tokens`.
    MaxTokens,
    /// A stop sequence was produced.
    StopSequence,
}

/// Token accounting, including the two prompt-cache counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_creation_input_tokens: u32,
    pub cache_read_input_tokens: u32,
}

impl Usage {
    /// Field-wise sum, for totalling usage across the turns of one loop.
    pub fn saturating_add(self, other: Usage) -> Usage {
        Usage {
            input_tokens: self.input_tokens.saturating_add(other.input_tokens),
            output_tokens: self.output_tokens.saturating_add(other.output_tokens),
            cache_creation_input_tokens: self
                .cache_creation_input_tokens
                .saturating_add(other.cache_creation_input_tokens),
            cache_read_input_tokens: self
                .cache_read_input_tokens
                .saturating_add(other.cache_read_input_tokens),
        }
    }
}

/// A full (assembled) response from the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelResponse {
    pub content: Vec<ContentBlock>,
    pub stop_reason: StopReason,
    pub usage: Usage,
}

impl ModelResponse {
    /// The concatenation of the response's text blocks.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for block in &self.content {
            if let ContentBlock::Text { text } = block {
                out.push_str(text);
            }
        }
        out
    }

    /// Whether the response is asking for at least one tool call.
    pub fn wants_tools(&self) -> bool {
        self.content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { .. }))
    }
}

/// An incremental delta inside a content block, as streamed by the API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentDelta {
    /// Appended text for a [`ContentBlock::Text`].
    Text(String),
    /// Appended JSON fragment for a [`ContentBlock::ToolUse`]'s `input`.
    InputJson(String),
}

/// One server-sent event in a streamed response. This is the shape the real
/// transport emits as bytes arrive; the mock replays a finished [`ModelResponse`]
/// as this same sequence (see [`response_to_events`]), so the assembler is
/// exercised identically either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    /// Opens the message.
    MessageStart,
    /// Opens a content block at `index`. The block arrives empty (text `""` or
    /// a tool-use with no input yet) and is filled by deltas.
    ContentBlockStart { index: usize, block: ContentBlock },
    /// Appends to the block at `index`.
    ContentBlockDelta { index: usize, delta: ContentDelta },
    /// Closes the block at `index`.
    ContentBlockStop { index: usize },
    /// Carries the final `stop_reason` and `usage` for the message.
    MessageDelta {
        stop_reason: StopReason,
        usage: Usage,
    },
    /// Closes the message.
    MessageStop,
}

/// Turn a finished response into the event sequence that would have streamed
/// it. Each block is start → (one delta carrying its payload) → stop; the whole
/// run is wrapped in `MessageStart` / `MessageDelta` / `MessageStop`. Feeding
/// this through [`StreamAssembler`] reproduces the original response exactly,
/// which is how the mock transport proves streamed assembly with no network.
pub fn response_to_events(resp: &ModelResponse) -> Vec<StreamEvent> {
    let mut events = vec![StreamEvent::MessageStart];
    for (index, block) in resp.content.iter().enumerate() {
        match block {
            ContentBlock::Text { text } => {
                events.push(StreamEvent::ContentBlockStart {
                    index,
                    block: ContentBlock::text(""),
                });
                events.push(StreamEvent::ContentBlockDelta {
                    index,
                    delta: ContentDelta::Text(text.clone()),
                });
            }
            ContentBlock::ToolUse { id, name, input } => {
                events.push(StreamEvent::ContentBlockStart {
                    index,
                    block: ContentBlock::tool_use(id.clone(), name.clone(), ""),
                });
                events.push(StreamEvent::ContentBlockDelta {
                    index,
                    delta: ContentDelta::InputJson(input.clone()),
                });
            }
            // Tool results are only ever produced by the caller, never streamed
            // back by the model, so they pass through as a single start block.
            ContentBlock::ToolResult { .. } => {
                events.push(StreamEvent::ContentBlockStart {
                    index,
                    block: block.clone(),
                });
            }
        }
        events.push(StreamEvent::ContentBlockStop { index });
    }
    events.push(StreamEvent::MessageDelta {
        stop_reason: resp.stop_reason,
        usage: resp.usage,
    });
    events.push(StreamEvent::MessageStop);
    events
}

/// Reassembles [`StreamEvent`]s into a [`ModelResponse`]. Push every event in
/// order, then [`finish`](StreamAssembler::finish). Out-of-order or dangling
/// events yield an [`AiError::MalformedStream`] rather than a silent wrong
/// answer.
#[derive(Debug, Default)]
pub struct StreamAssembler {
    started: bool,
    stopped: bool,
    blocks: Vec<ContentBlock>,
    stop_reason: Option<StopReason>,
    usage: Usage,
}

impl StreamAssembler {
    /// A fresh assembler.
    pub fn new() -> Self {
        StreamAssembler::default()
    }

    /// Feed one event. Returns an error the moment the stream is inconsistent.
    pub fn push(&mut self, event: StreamEvent) -> Result<(), AiError> {
        match event {
            StreamEvent::MessageStart => {
                if self.started {
                    return Err(AiError::MalformedStream("duplicate MessageStart".into()));
                }
                self.started = true;
            }
            StreamEvent::ContentBlockStart { index, block } => {
                self.require_started("ContentBlockStart")?;
                if index != self.blocks.len() {
                    return Err(AiError::MalformedStream(format!(
                        "ContentBlockStart index {index} out of order (expected {})",
                        self.blocks.len()
                    )));
                }
                self.blocks.push(block);
            }
            StreamEvent::ContentBlockDelta { index, delta } => {
                self.require_started("ContentBlockDelta")?;
                let block = self.blocks.get_mut(index).ok_or_else(|| {
                    AiError::MalformedStream(format!(
                        "ContentBlockDelta for unopened block {index}"
                    ))
                })?;
                apply_delta(block, delta)?;
            }
            StreamEvent::ContentBlockStop { index } => {
                self.require_started("ContentBlockStop")?;
                if index >= self.blocks.len() {
                    return Err(AiError::MalformedStream(format!(
                        "ContentBlockStop for unopened block {index}"
                    )));
                }
            }
            StreamEvent::MessageDelta { stop_reason, usage } => {
                self.require_started("MessageDelta")?;
                self.stop_reason = Some(stop_reason);
                self.usage = self.usage.saturating_add(usage);
            }
            StreamEvent::MessageStop => {
                self.require_started("MessageStop")?;
                self.stopped = true;
            }
        }
        Ok(())
    }

    /// Finish and produce the response. Errors if the stream never carried a
    /// `MessageStop` or a `stop_reason`.
    pub fn finish(self) -> Result<ModelResponse, AiError> {
        if !self.stopped {
            return Err(AiError::MalformedStream(
                "stream ended before MessageStop".into(),
            ));
        }
        let stop_reason = self
            .stop_reason
            .ok_or_else(|| AiError::MalformedStream("stream carried no stop_reason".into()))?;
        Ok(ModelResponse {
            content: self.blocks,
            stop_reason,
            usage: self.usage,
        })
    }

    fn require_started(&self, what: &str) -> Result<(), AiError> {
        if self.started {
            Ok(())
        } else {
            Err(AiError::MalformedStream(format!(
                "{what} before MessageStart"
            )))
        }
    }
}

/// Apply one delta to the block it extends; the delta kind must match the block
/// kind.
fn apply_delta(block: &mut ContentBlock, delta: ContentDelta) -> Result<(), AiError> {
    match (block, delta) {
        (ContentBlock::Text { text }, ContentDelta::Text(more)) => {
            text.push_str(&more);
            Ok(())
        }
        (ContentBlock::ToolUse { input, .. }, ContentDelta::InputJson(more)) => {
            input.push_str(&more);
            Ok(())
        }
        _ => Err(AiError::MalformedStream(
            "delta kind does not match its content block".into(),
        )),
    }
}
