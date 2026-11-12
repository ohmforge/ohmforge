//! The seam between the copilot loop and the outside world.
//!
//! Everything above [`Transport`] is pure, synchronous, and network-free, so
//! the whole tool-use loop is unit-tested against [`MockTransport`] with no I/O
//! and the crate still compiles for `wasm32` without a TLS stack. The live
//! Claude round-trip lives in `HttpTransport`, behind the **opt-in** `http`
//! feature (native `reqwest` blocking client; `wasm32` `fetch` is a documented
//! TODO) — with the feature off, the default build pulls no HTTP/TLS stack.
//!
//! The trait is deliberately **synchronous**. The design sketch showed an
//! `async_trait`, but a blocking `send` keeps tests free of an async runtime
//! and keeps the browser build free of `tokio`; a native transport can block on
//! its own runtime internally, and a wasm transport can drive `fetch` behind
//! the same signature.

use std::cell::RefCell;
use std::collections::VecDeque;

use crate::error::AiError;
use crate::message::{response_to_events, ModelRequest, ModelResponse, StreamEvent};

/// How the client reaches the model. One method is required — [`send`](Transport::send),
/// a blocking request/response — and streaming has a default implementation
/// that replays the finished response as [`StreamEvent`]s, which is all the
/// loop needs to prove streamed assembly. A real transport overrides
/// [`send_streaming`](Transport::send_streaming) to surface deltas as bytes
/// arrive.
pub trait Transport {
    /// Send a request and block until the full response is available.
    fn send(&self, req: ModelRequest) -> Result<ModelResponse, AiError>;

    /// Send a request, delivering the response as a stream of events to
    /// `on_event`, and also returning the assembled whole.
    ///
    /// The default runs [`send`](Transport::send) and replays the result via
    /// [`response_to_events`]; this is enough for tests and for any transport
    /// that cannot stream. Override it to emit real server-sent deltas.
    fn send_streaming(
        &self,
        req: ModelRequest,
        on_event: &mut dyn FnMut(StreamEvent),
    ) -> Result<ModelResponse, AiError> {
        let resp = self.send(req)?;
        for event in response_to_events(&resp) {
            on_event(event);
        }
        Ok(resp)
    }
}

/// A canned transport for tests. Hand it a queue of [`ModelResponse`]s; each
/// [`send`](Transport::send) pops the next one and records the request it was
/// given, so a test can both drive a multi-turn tool loop and assert on exactly
/// what the client sent.
#[derive(Debug, Default)]
pub struct MockTransport {
    responses: RefCell<VecDeque<ModelResponse>>,
    requests: RefCell<Vec<ModelRequest>>,
}

impl MockTransport {
    /// A mock that will answer successive sends with `responses`, in order.
    pub fn new(responses: impl IntoIterator<Item = ModelResponse>) -> Self {
        MockTransport {
            responses: RefCell::new(responses.into_iter().collect()),
            requests: RefCell::new(Vec::new()),
        }
    }

    /// Every request the client has sent so far, in order.
    pub fn requests(&self) -> Vec<ModelRequest> {
        self.requests.borrow().clone()
    }

    /// How many requests have been sent.
    pub fn request_count(&self) -> usize {
        self.requests.borrow().len()
    }

    /// How many canned responses remain unconsumed.
    pub fn remaining(&self) -> usize {
        self.responses.borrow().len()
    }
}

impl Transport for MockTransport {
    fn send(&self, req: ModelRequest) -> Result<ModelResponse, AiError> {
        self.requests.borrow_mut().push(req);
        self.responses
            .borrow_mut()
            .pop_front()
            .ok_or_else(|| AiError::Transport("mock transport ran out of canned responses".into()))
    }
}

/// The real Anthropic HTTP transport, **feature-gated** behind `http`.
///
/// It performs a live Claude Messages API round-trip:
///
/// * **Native** (`cfg(not(target_arch = "wasm32"))`) — a `reqwest` **blocking**
///   client with `rustls` (no OpenSSL / system-TLS dependency). [`send`] builds
///   the Anthropic JSON request (model, `max_tokens`, `system`, `messages`,
///   `tools`, and any `cache_control` markers), sets the `x-api-key`,
///   `anthropic-version`, and `content-type` headers, POSTs to the configured
///   [`base_url`](Self::base_url), and parses the JSON response into a
///   [`ModelResponse`]. Any transport, HTTP-status, or parse failure is mapped
///   to [`AiError::Transport`]; the key is sent but never logged.
/// * **wasm32** (`cfg(target_arch = "wasm32")`) — a `fetch`-based path is not
///   wired yet; [`send`] returns a clear `AiError::Transport("wasm fetch: …")`
///   TODO. The native path is the supported one; the browser build never pulls
///   this stack (the dependency lives in a `cfg(not(wasm32))` table).
///
/// The dependency is **opt-in**: with the `http` feature off (the default), the
/// crate is dependency-free and `wasm32`-clean and this type does not exist.
///
/// [`send`]: Transport::send
#[cfg(feature = "http")]
#[derive(Debug, Clone)]
pub struct HttpTransport {
    api_key: String,
    base_url: String,
    timeout: std::time::Duration,
}

#[cfg(feature = "http")]
impl HttpTransport {
    /// The Messages API endpoint.
    pub const DEFAULT_BASE_URL: &'static str = "https://api.anthropic.com/v1/messages";

    /// The `anthropic-version` header value the Messages API expects.
    pub const ANTHROPIC_VERSION: &'static str = "2023-06-01";

    /// The default whole-request timeout. A stalled server that accepts the
    /// connection but never replies cannot hang the single-threaded CLI past
    /// this; override it with [`with_timeout`](Self::with_timeout).
    pub const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

    /// Build a transport from an explicit key and endpoint. Mainly for tests
    /// (point it at a local mock server); production code usually calls
    /// [`from_env`](Self::from_env). The key is held but never logged.
    pub fn new(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        HttpTransport {
            api_key: api_key.into(),
            base_url: base_url.into(),
            timeout: Self::DEFAULT_TIMEOUT,
        }
    }

    /// Build a transport from the environment: the API key from
    /// `ANTHROPIC_API_KEY` and the endpoint from `EE_BASE_URL` (then
    /// `ANTHROPIC_BASE_URL`, then [`DEFAULT_BASE_URL`](Self::DEFAULT_BASE_URL)),
    /// so a proxy or gateway can be selected without a code change. Returns
    /// [`AiError::MissingApiKey`] if no key is present or it is blank.
    pub fn from_env() -> Result<Self, AiError> {
        Ok(HttpTransport {
            api_key: crate::config::api_key_from_env()?,
            base_url: crate::config::base_url_from_env(Self::DEFAULT_BASE_URL),
            timeout: Self::DEFAULT_TIMEOUT,
        })
    }

    /// Point the transport at a different base URL (e.g. a proxy).
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Override the whole-request timeout (default
    /// [`DEFAULT_TIMEOUT`](Self::DEFAULT_TIMEOUT)). The connect timeout is this,
    /// capped at 10 s. Mainly for tests that need a short bound.
    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The configured endpoint.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The key is held but never logged; this only reports whether one is set.
    pub fn has_key(&self) -> bool {
        !self.api_key.is_empty()
    }
}

// --- Native path: a reqwest blocking round-trip. ---------------------------

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
impl Transport for HttpTransport {
    fn send(&self, req: ModelRequest) -> Result<ModelResponse, AiError> {
        let body = serde_json::to_vec(&http_json::request_to_json(&req))
            .map_err(|e| AiError::Transport(format!("serializing request: {e}")))?;

        // Bound the single-threaded CLI: without a timeout a stalled server (one
        // that accepts the connection but never replies) hangs `send` forever. A
        // whole-request cap plus a short connect cap turn that into a bounded
        // `AiError::Transport` instead.
        let client = reqwest::blocking::Client::builder()
            .timeout(self.timeout)
            .connect_timeout(self.timeout.min(std::time::Duration::from_secs(10)))
            .build()
            .map_err(|e| AiError::Transport(format!("building HTTP client: {e}")))?;

        let resp = client
            .post(&self.base_url)
            .header("x-api-key", self.api_key.as_str())
            .header("anthropic-version", Self::ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .body(body)
            .send()
            .map_err(|e| AiError::Transport(format!("sending request: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let detail = resp.text().unwrap_or_default();
            return Err(AiError::Transport(format!("HTTP {status}: {detail}")));
        }

        let value: serde_json::Value = resp
            .json()
            .map_err(|e| AiError::Transport(format!("reading response body: {e}")))?;
        http_json::parse_response(&value)
    }
}

// --- wasm32 path: a documented TODO stub (no dependency pulled). -----------

#[cfg(all(feature = "http", target_arch = "wasm32"))]
impl Transport for HttpTransport {
    fn send(&self, _req: ModelRequest) -> Result<ModelResponse, AiError> {
        Err(AiError::Transport(
            "wasm fetch: TODO — the browser `fetch` transport is not wired yet; \
             native builds use the reqwest blocking client"
                .into(),
        ))
    }
}

/// JSON (de)serialization for the native HTTP transport, isolated so the rest
/// of the crate stays serde-free. Only compiled for native `http` builds.
#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
mod http_json {
    use serde_json::{json, Map, Value};

    use crate::error::AiError;
    use crate::message::{
        CacheControl, ContentBlock, Message, ModelRequest, ModelResponse, Role, StopReason, Tool,
        Usage,
    };

    /// Build the Anthropic Messages request body from a [`ModelRequest`].
    pub(super) fn request_to_json(req: &ModelRequest) -> Value {
        let mut root = Map::new();
        root.insert("model".into(), json!(req.model));
        root.insert("max_tokens".into(), json!(req.max_tokens));
        root.insert(
            "messages".into(),
            Value::Array(req.messages.iter().map(message_to_json).collect()),
        );
        if !req.system.is_empty() {
            let blocks = req
                .system
                .iter()
                .map(|b| {
                    let mut m = Map::new();
                    m.insert("type".into(), json!("text"));
                    m.insert("text".into(), json!(b.text));
                    insert_cache_control(&mut m, b.cache_control);
                    Value::Object(m)
                })
                .collect();
            root.insert("system".into(), Value::Array(blocks));
        }
        if !req.tools.is_empty() {
            root.insert(
                "tools".into(),
                Value::Array(req.tools.iter().map(tool_to_json).collect()),
            );
        }
        Value::Object(root)
    }

    fn tool_to_json(tool: &Tool) -> Value {
        let mut m = Map::new();
        m.insert("name".into(), json!(tool.name));
        m.insert("description".into(), json!(tool.description));
        m.insert("input_schema".into(), parse_json_object(&tool.input_schema));
        insert_cache_control(&mut m, tool.cache_control);
        Value::Object(m)
    }

    fn message_to_json(msg: &Message) -> Value {
        let role = match msg.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        json!({
            "role": role,
            "content": msg.content.iter().map(block_to_json).collect::<Vec<_>>(),
        })
    }

    fn block_to_json(block: &ContentBlock) -> Value {
        match block {
            ContentBlock::Text { text } => json!({ "type": "text", "text": text }),
            ContentBlock::ToolUse { id, name, input } => json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": parse_json_object(input),
            }),
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": content,
                "is_error": is_error,
            }),
        }
    }

    fn insert_cache_control(m: &mut Map<String, Value>, cc: Option<CacheControl>) {
        if let Some(CacheControl::Ephemeral) = cc {
            m.insert("cache_control".into(), json!({ "type": "ephemeral" }));
        }
    }

    /// Opaque JSON object *text* → a `Value`. Falls back to an empty object for
    /// blank or unparseable text, so a malformed tool input never aborts a send.
    fn parse_json_object(text: &str) -> Value {
        serde_json::from_str::<Value>(text).unwrap_or_else(|_| json!({}))
    }

    /// Parse an Anthropic Messages response into a [`ModelResponse`].
    pub(super) fn parse_response(value: &Value) -> Result<ModelResponse, AiError> {
        let blocks = value
            .get("content")
            .and_then(Value::as_array)
            .ok_or_else(|| AiError::Transport("response missing a `content` array".into()))?;

        let mut content = Vec::with_capacity(blocks.len());
        for block in blocks {
            let ty = block
                .get("type")
                .and_then(Value::as_str)
                .ok_or_else(|| AiError::Transport("content block missing `type`".into()))?;
            match ty {
                "text" => {
                    let text = block
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    content.push(ContentBlock::text(text));
                }
                "tool_use" => {
                    let id = block.get("id").and_then(Value::as_str).unwrap_or_default();
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    // Re-serialize the input object back to opaque JSON text.
                    let input = block
                        .get("input")
                        .map_or_else(|| "{}".to_string(), Value::to_string);
                    content.push(ContentBlock::tool_use(id, name, input));
                }
                // Tolerate blocks we don't model (e.g. `thinking`, which current
                // models can emit): skip them rather than failing the whole
                // response. We keep only `text`/`tool_use`, the blocks the loop
                // acts on.
                _ => {}
            }
        }

        let stop_reason = match value.get("stop_reason").and_then(Value::as_str) {
            Some("end_turn") | None => StopReason::EndTurn,
            Some("tool_use") => StopReason::ToolUse,
            Some("max_tokens") => StopReason::MaxTokens,
            Some("stop_sequence") => StopReason::StopSequence,
            Some(other) => {
                return Err(AiError::Transport(format!("unknown stop_reason `{other}`")));
            }
        };

        let usage = value.get("usage");
        let count = |key: &str| -> u32 {
            usage
                .and_then(|u| u.get(key))
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32
        };
        let usage = Usage {
            input_tokens: count("input_tokens"),
            output_tokens: count("output_tokens"),
            cache_creation_input_tokens: count("cache_creation_input_tokens"),
            cache_read_input_tokens: count("cache_read_input_tokens"),
        };

        Ok(ModelResponse {
            content,
            stop_reason,
            usage,
        })
    }
}
