//! Live HTTP transport round-trip, feature-gated behind `http`.
//!
//! No outbound network: a tiny `std::net::TcpListener` on `127.0.0.1` stands in
//! for the Messages API and returns a canned Anthropic response, so the test is
//! deterministic and self-contained. It proves the native `reqwest` path builds
//! the request, POSTs it, and parses the JSON body back into a `ModelResponse`.
#![cfg(feature = "http")]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::{Duration, Instant};

use ee_ai::transport::{HttpTransport, Transport};
use ee_ai::{AiError, ContentBlock, Message, ModelRequest, StopReason, SystemBlock};

/// A canned Anthropic Messages response: one text block, a natural end of turn.
const CANNED_RESPONSE: &str = r#"{
  "id": "msg_test_01",
  "type": "message",
  "role": "assistant",
  "model": "claude-sonnet-5-5",
  "content": [{ "type": "text", "text": "V(out) is 3.28 V, within spec." }],
  "stop_reason": "end_turn",
  "stop_sequence": null,
  "usage": {
    "input_tokens": 12,
    "output_tokens": 9,
    "cache_creation_input_tokens": 0,
    "cache_read_input_tokens": 0
  }
}"#;

#[test]
fn http_transport_round_trips_against_a_local_server() {
    // Bind to an ephemeral port on loopback; the OS picks a free one.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local mock server");
    let addr = listener.local_addr().expect("local addr");

    // One background connection: read the request, return the canned response.
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept one connection");
        // Drain what the client sent (headers + body usually arrive together on
        // loopback); we assert on the response, not the request bytes.
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);

        let response = format!(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n\
             {}",
            CANNED_RESPONSE.len(),
            CANNED_RESPONSE
        );
        stream
            .write_all(response.as_bytes())
            .expect("write response");
        stream.flush().expect("flush response");
    });

    // Point an HttpTransport at the mock server with a dummy key.
    let transport = HttpTransport::new("sk-dummy-key", format!("http://{addr}/v1/messages"));

    let req = ModelRequest {
        model: "claude-sonnet-5-5".to_string(),
        system: vec![SystemBlock::new("You are an EE copilot.").cached()],
        messages: vec![Message::user_text("why is V(out) low?")],
        tools: Vec::new(),
        max_tokens: 256,
    };

    let resp = transport
        .send(req)
        .expect("round-trip parses a ModelResponse");

    assert_eq!(resp.text(), "V(out) is 3.28 V, within spec.");
    assert_eq!(resp.stop_reason, StopReason::EndTurn);
    assert_eq!(resp.content.len(), 1);
    assert!(matches!(resp.content[0], ContentBlock::Text { .. }));
    assert_eq!(resp.usage.input_tokens, 12);
    assert_eq!(resp.usage.output_tokens, 9);

    server.join().expect("mock server thread completes");
}

/// A response carrying a content block we don't model — a `thinking` block,
/// which current models can emit — parses rather than hard-failing: the unknown
/// block is skipped and the real `text` answer survives.
const RESPONSE_WITH_THINKING: &str = r#"{
  "id": "msg_test_02",
  "type": "message",
  "role": "assistant",
  "model": "claude-sonnet-5-5",
  "content": [
    { "type": "thinking", "thinking": "Let me reason about the divider…" },
    { "type": "text", "text": "V(out) is 3.28 V, within spec." }
  ],
  "stop_reason": "end_turn",
  "usage": { "input_tokens": 5, "output_tokens": 7 }
}"#;

#[test]
fn unknown_content_block_is_tolerated() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local mock server");
    let addr = listener.local_addr().expect("local addr");

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept one connection");
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        let response = format!(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n\
             {}",
            RESPONSE_WITH_THINKING.len(),
            RESPONSE_WITH_THINKING
        );
        stream
            .write_all(response.as_bytes())
            .expect("write response");
        stream.flush().expect("flush response");
    });

    let transport = HttpTransport::new("sk-dummy-key", format!("http://{addr}/v1/messages"));
    let req = ModelRequest {
        model: "claude-sonnet-5-5".to_string(),
        system: Vec::new(),
        messages: vec![Message::user_text("why is V(out) low?")],
        tools: Vec::new(),
        max_tokens: 256,
    };

    let resp = transport
        .send(req)
        .expect("thinking block does not fail the parse");
    // The `thinking` block was skipped; only the text answer remains.
    assert_eq!(resp.content.len(), 1);
    assert!(matches!(resp.content[0], ContentBlock::Text { .. }));
    assert_eq!(resp.text(), "V(out) is 3.28 V, within spec.");
    assert_eq!(resp.stop_reason, StopReason::EndTurn);

    server.join().expect("mock server thread completes");
}

/// A server that accepts the connection but never replies must not hang the
/// single-threaded CLI: `send` returns a bounded `AiError::Transport` within a
/// few seconds (the configured request timeout), not block forever.
#[test]
fn stalled_server_times_out_instead_of_hanging() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local mock server");
    let addr = listener.local_addr().expect("local addr");

    // Accept the connection, then sit on it — read nothing, write nothing — so
    // the client's request goes unanswered until its timeout fires.
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept one connection");
        thread::sleep(Duration::from_secs(5));
        drop(stream);
    });

    let transport = HttpTransport::new("sk-dummy-key", format!("http://{addr}/v1/messages"))
        .with_timeout(Duration::from_secs(2));
    let req = ModelRequest {
        model: "claude-sonnet-5-5".to_string(),
        system: Vec::new(),
        messages: vec![Message::user_text("hello?")],
        tools: Vec::new(),
        max_tokens: 64,
    };

    let start = Instant::now();
    let err = transport
        .send(req)
        .expect_err("a stalled server must not succeed");
    let elapsed = start.elapsed();

    assert!(
        matches!(err, AiError::Transport(_)),
        "a stall maps to AiError::Transport, got {err:?}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "send must return bounded (got {elapsed:?}), not hang"
    );

    server.join().expect("mock server thread completes");
}
