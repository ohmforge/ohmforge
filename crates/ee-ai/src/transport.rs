//! The seam between the copilot loop and the outside world.
//!
//! Everything above [`Transport`] is pure, synchronous, and network-free, so
//! the whole tool-use loop is unit-tested against [`MockTransport`] with no I/O
//! and the crate still compiles for `wasm32` without a TLS stack. A real HTTP
//! transport is a thin, feature-gated stub for now (see [`HttpTransport`]): the
//! point of this checkpoint is the loop logic, not live calls.
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

/// A placeholder for the real Anthropic HTTP transport.
///
/// It is **feature-gated** behind `http` and intentionally not wired to a
/// network stack at this checkpoint: it compiles, reads the API key from the
/// environment, and returns a clear [`AiError`] if asked to send. Wiring it to
/// an HTTP client (native `reqwest`, wasm `fetch`) is a later checkpoint; doing
/// it here would drag a TLS/async stack into a checkpoint that is only about
/// the loop. No test exercises it, and no test needs a key.
#[cfg(feature = "http")]
#[derive(Debug, Clone)]
pub struct HttpTransport {
    api_key: String,
    base_url: String,
}

#[cfg(feature = "http")]
impl HttpTransport {
    /// The Messages API endpoint.
    pub const DEFAULT_BASE_URL: &'static str = "https://api.anthropic.com/v1/messages";

    /// Build a transport, reading `ANTHROPIC_API_KEY` from the environment.
    /// Returns [`AiError::MissingApiKey`] if it is absent or blank.
    pub fn from_env() -> Result<Self, AiError> {
        Ok(HttpTransport {
            api_key: crate::config::api_key_from_env()?,
            base_url: Self::DEFAULT_BASE_URL.to_string(),
        })
    }

    /// Point the transport at a different base URL (e.g. a proxy).
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
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

#[cfg(feature = "http")]
impl Transport for HttpTransport {
    fn send(&self, _req: ModelRequest) -> Result<ModelResponse, AiError> {
        Err(AiError::Transport(
            "live HTTP transport is not wired yet (CP-AF-M1.2 covers the loop; \
             the network path lands with the UI)"
                .into(),
        ))
    }
}
