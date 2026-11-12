//! Network-free tests for the Claude client: the tool-use loop, streamed
//! assembly, the request shape (prompt-cache markers + default model), and
//! the missing-key error. Everything runs against `MockTransport`.

use ee_ai::config::{resolve_api_key, resolve_base_url, resolve_model};
use ee_ai::{
    response_to_events, AiError, CacheControl, Client, ContentBlock, Message, ModelResponse, Role,
    StopReason, StreamAssembler, StreamEvent, SystemBlock, Tool, Usage, DEFAULT_MODEL,
};

/// A response whose single text block is `text`, ending the turn.
fn final_text(text: &str) -> ModelResponse {
    ModelResponse {
        content: vec![ContentBlock::text(text)],
        stop_reason: StopReason::EndTurn,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Usage::default()
        },
    }
}

/// A response asking to run one tool.
fn asks_tool(id: &str, name: &str, input: &str) -> ModelResponse {
    ModelResponse {
        content: vec![
            ContentBlock::text("let me check"),
            ContentBlock::tool_use(id, name, input),
        ],
        stop_reason: StopReason::ToolUse,
        usage: Usage {
            input_tokens: 8,
            output_tokens: 12,
            ..Usage::default()
        },
    }
}

fn sample_tools() -> Vec<Tool> {
    vec![
        Tool::new("read_signal", "read a node voltage", r#"{"type":"object"}"#),
        Tool::new("list_parts", "list the parts", r#"{"type":"object"}"#),
    ]
}

fn sample_system() -> Vec<SystemBlock> {
    vec![
        SystemBlock::new("You are an EE copilot."),
        SystemBlock::new("Always ground your answers in a real sim read."),
    ]
}

#[test]
fn tool_use_loop_runs_to_completion() {
    // assistant asks for a tool → handler answers → assistant emits final text.
    let transport = ee_ai::MockTransport::new([
        asks_tool("tu_1", "read_signal", r#"{"signal":"v(out)"}"#),
        final_text("V(out) is 3.28 V, within spec."),
    ]);
    let client = Client::new(transport);

    let mut seen_input = String::new();
    let mut handler = |name: &str, input: &str| -> Result<String, String> {
        assert_eq!(name, "read_signal");
        seen_input = input.to_string();
        Ok(r#"{"value":3.28,"unit":"V"}"#.to_string())
    };

    let result = client
        .run(
            &sample_system(),
            &sample_tools(),
            "why is V(out) low?",
            &mut handler,
        )
        .expect("loop completes");

    // The loop issued exactly one tool call and reached a natural end.
    assert_eq!(result.tool_calls, 1);
    assert_eq!(result.stop_reason, StopReason::EndTurn);
    assert_eq!(result.final_text, "V(out) is 3.28 V, within spec.");
    assert_eq!(seen_input, r#"{"signal":"v(out)"}"#);

    // Usage summed across both turns.
    assert_eq!(result.usage.input_tokens, 18);
    assert_eq!(result.usage.output_tokens, 17);

    // Two round-trips happened, and the tool result was fed back as a user turn.
    let client_transport = client.transport();
    assert_eq!(client_transport.request_count(), 2);
    assert_eq!(client_transport.remaining(), 0);

    let second = &client_transport.requests()[1];
    let last = second.messages.last().expect("a tool_result turn");
    assert_eq!(last.role, Role::User);
    assert_eq!(
        last.content,
        vec![ContentBlock::tool_result(
            "tu_1",
            r#"{"value":3.28,"unit":"V"}"#
        )]
    );

    // The transcript is user → assistant(tool_use) → user(tool_result) → assistant(final).
    assert_eq!(result.messages.len(), 4);
    assert_eq!(result.messages[0].role, Role::User);
    assert_eq!(result.messages[1].role, Role::Assistant);
    assert_eq!(result.messages[2].role, Role::User);
    assert_eq!(result.messages[3].role, Role::Assistant);
}

#[test]
fn loop_dispatches_multiple_tools_in_one_turn() {
    let mut multi = asks_tool("tu_a", "read_signal", r#"{"signal":"v(a)"}"#);
    multi
        .content
        .push(ContentBlock::tool_use("tu_b", "list_parts", "{}"));
    let transport = ee_ai::MockTransport::new([multi, final_text("done")]);
    let client = Client::new(transport);

    let mut calls = Vec::new();
    let mut handler = |name: &str, _input: &str| -> Result<String, String> {
        calls.push(name.to_string());
        Ok("ok".to_string())
    };

    let result = client
        .run(&sample_system(), &sample_tools(), "status?", &mut handler)
        .expect("completes");

    assert_eq!(result.tool_calls, 2);
    assert_eq!(calls, vec!["read_signal", "list_parts"]);
    // Both results ride in a single user turn.
    let fed_back = &client.transport().requests()[1];
    let last = fed_back.messages.last().unwrap();
    assert_eq!(last.content.len(), 2);
}

#[test]
fn tool_error_is_fed_back_and_loop_continues() {
    let transport = ee_ai::MockTransport::new([
        asks_tool("tu_1", "read_signal", r#"{"signal":"v(nope)"}"#),
        final_text("no such node; try v(out)."),
    ]);
    let client = Client::new(transport);

    let mut handler =
        |_name: &str, _input: &str| -> Result<String, String> { Err("unknown signal".to_string()) };

    let result = client
        .run(
            &sample_system(),
            &sample_tools(),
            "read v(nope)",
            &mut handler,
        )
        .expect("completes despite tool error");

    assert_eq!(result.final_text, "no such node; try v(out).");
    let fed_back = &client.transport().requests()[1];
    let last = fed_back.messages.last().unwrap();
    assert_eq!(
        last.content,
        vec![ContentBlock::tool_error("tu_1", "unknown signal")]
    );
}

#[test]
fn runaway_tool_loop_is_capped() {
    // A transport that only ever asks for tools must not spin forever.
    let always_tool: Vec<ModelResponse> = (0..10)
        .map(|i| asks_tool(&format!("tu_{i}"), "read_signal", "{}"))
        .collect();
    let transport = ee_ai::MockTransport::new(always_tool);
    let client = Client::new(transport).with_max_tool_turns(3);

    let mut handler = |_: &str, _: &str| -> Result<String, String> { Ok("ok".to_string()) };
    let err = client
        .run(&sample_system(), &sample_tools(), "loop", &mut handler)
        .expect_err("should hit the cap");

    // The cap signal carries the partial run rather than discarding it: the turn
    // count, the text the model produced, the dispatched tool calls, and the full
    // transcript are all recoverable.
    match err {
        AiError::ToolLoopExhausted { turns, partial } => {
            assert_eq!(turns, 3);
            assert_eq!(partial.tool_calls, 3, "three turns each dispatched a tool");
            assert_eq!(partial.final_text, "let me check");
            assert_eq!(partial.stop_reason, StopReason::ToolUse);
            assert!(
                !partial.messages.is_empty(),
                "the partial transcript is preserved"
            );
            // 8 input + 12 output per turn, summed over the three capped turns.
            assert_eq!(partial.usage.input_tokens, 24);
            assert_eq!(partial.usage.output_tokens, 36);
        }
        other => panic!("expected ToolLoopExhausted, got {other:?}"),
    }
}

#[test]
fn request_marks_system_and_tools_cacheable() {
    let transport = ee_ai::MockTransport::new([final_text("hi")]);
    let client = Client::new(transport).with_model("claude-sonnet-5-5");
    let mut handler = |_: &str, _: &str| -> Result<String, String> { Ok(String::new()) };

    client
        .run(&sample_system(), &sample_tools(), "hello", &mut handler)
        .unwrap();

    let req = &client.transport().requests()[0];
    // The last block of the static prefix carries the ephemeral cache marker.
    assert_eq!(
        req.system.last().unwrap().cache_control,
        Some(CacheControl::Ephemeral)
    );
    assert_eq!(
        req.tools.last().unwrap().cache_control,
        Some(CacheControl::Ephemeral)
    );
    // Earlier blocks are not themselves marked (caching the last caches the prefix).
    assert_eq!(req.system[0].cache_control, None);
    assert_eq!(req.model, "claude-sonnet-5-5");
}

#[test]
fn prompt_cache_can_be_disabled() {
    let transport = ee_ai::MockTransport::new([final_text("hi")]);
    let client = Client::new(transport).with_prompt_cache(false);
    let mut handler = |_: &str, _: &str| -> Result<String, String> { Ok(String::new()) };
    client
        .run(&sample_system(), &sample_tools(), "hello", &mut handler)
        .unwrap();
    let req = &client.transport().requests()[0];
    assert_eq!(req.system.last().unwrap().cache_control, None);
    assert_eq!(req.tools.last().unwrap().cache_control, None);
}

#[test]
fn default_model_is_pinned_sonnet_and_env_overrides() {
    assert_eq!(DEFAULT_MODEL, "claude-sonnet-5-5");
    // No override → the pinned default.
    assert_eq!(resolve_model(None, None), "claude-sonnet-5-5");
    // EE_MODEL wins over ANTHROPIC_MODEL.
    assert_eq!(
        resolve_model(Some("ee-pick"), Some("anthropic-pick")),
        "ee-pick"
    );
    // ANTHROPIC_MODEL used when EE_MODEL is absent.
    assert_eq!(
        resolve_model(None, Some("anthropic-pick")),
        "anthropic-pick"
    );
    // Blank values are ignored and fall through.
    assert_eq!(
        resolve_model(Some("   "), Some("anthropic-pick")),
        "anthropic-pick"
    );
    assert_eq!(resolve_model(Some(""), None), "claude-sonnet-5-5");
}

#[test]
fn base_url_defaults_and_env_overrides() {
    const DEFAULT: &str = "https://api.anthropic.com/v1/messages";
    // No override → the stock endpoint.
    assert_eq!(resolve_base_url(None, None, DEFAULT), DEFAULT);
    // EE_BASE_URL wins over ANTHROPIC_BASE_URL.
    assert_eq!(
        resolve_base_url(
            Some("https://ee.proxy/v1"),
            Some("https://anthropic.proxy/v1"),
            DEFAULT
        ),
        "https://ee.proxy/v1"
    );
    // ANTHROPIC_BASE_URL used when EE_BASE_URL is absent — the var the user asked for.
    assert_eq!(
        resolve_base_url(None, Some("https://gateway.internal/v1/messages"), DEFAULT),
        "https://gateway.internal/v1/messages"
    );
    // Blank/whitespace values are ignored and fall through to the default.
    assert_eq!(
        resolve_base_url(Some("   "), Some("https://anthropic.proxy/v1"), DEFAULT),
        "https://anthropic.proxy/v1"
    );
    assert_eq!(resolve_base_url(Some(""), None, DEFAULT), DEFAULT);
}

#[test]
fn missing_api_key_is_a_clear_error() {
    assert_eq!(resolve_api_key(None), Err(AiError::MissingApiKey));
    assert_eq!(resolve_api_key(Some("   ")), Err(AiError::MissingApiKey));
    assert_eq!(resolve_api_key(Some("sk-abc")), Ok("sk-abc".to_string()));

    let shown = AiError::MissingApiKey.to_string();
    assert!(shown.contains("ANTHROPIC_API_KEY"), "message was: {shown}");
}

#[test]
fn streamed_events_reassemble_into_the_full_response() {
    // A response mixing text and a tool-use block, streamed and rebuilt.
    let original = ModelResponse {
        content: vec![
            ContentBlock::text("thinking… "),
            ContentBlock::tool_use("tu_7", "read_signal", r#"{"signal":"v(out)"}"#),
        ],
        stop_reason: StopReason::ToolUse,
        usage: Usage {
            input_tokens: 3,
            output_tokens: 9,
            ..Usage::default()
        },
    };

    let mut asm = StreamAssembler::new();
    for ev in response_to_events(&original) {
        asm.push(ev).expect("well-formed event");
    }
    let rebuilt = asm.finish().expect("stream closed cleanly");
    assert_eq!(rebuilt, original);
}

#[test]
fn text_deltas_concatenate_across_chunks() {
    // Prove the assembler accumulates multiple deltas into one block.
    let mut asm = StreamAssembler::new();
    asm.push(StreamEvent::MessageStart).unwrap();
    asm.push(StreamEvent::ContentBlockStart {
        index: 0,
        block: ContentBlock::text(""),
    })
    .unwrap();
    for chunk in ["Hel", "lo, ", "world"] {
        asm.push(StreamEvent::ContentBlockDelta {
            index: 0,
            delta: ee_ai::ContentDelta::Text(chunk.to_string()),
        })
        .unwrap();
    }
    asm.push(StreamEvent::ContentBlockStop { index: 0 })
        .unwrap();
    asm.push(StreamEvent::MessageDelta {
        stop_reason: StopReason::EndTurn,
        usage: Usage::default(),
    })
    .unwrap();
    asm.push(StreamEvent::MessageStop).unwrap();

    let resp = asm.finish().unwrap();
    assert_eq!(resp.text(), "Hello, world");
}

#[test]
fn out_of_order_stream_is_rejected() {
    let mut asm = StreamAssembler::new();
    // A delta before any block is opened is malformed.
    let err = asm
        .push(StreamEvent::ContentBlockDelta {
            index: 0,
            delta: ee_ai::ContentDelta::Text("x".to_string()),
        })
        .expect_err("should reject");
    assert!(matches!(err, AiError::MalformedStream(_)));
}

#[test]
fn run_streaming_forwards_events_that_reassemble_per_turn() {
    let transport = ee_ai::MockTransport::new([final_text("streamed answer")]);
    let client = Client::new(transport);
    let mut handler = |_: &str, _: &str| -> Result<String, String> { Ok(String::new()) };

    let mut events: Vec<StreamEvent> = Vec::new();
    let result = client
        .run_streaming(
            &sample_system(),
            &sample_tools(),
            "hi",
            &mut handler,
            &mut |ev| events.push(ev),
        )
        .expect("completes");

    assert_eq!(result.final_text, "streamed answer");

    // The forwarded events for the single turn reassemble to that turn.
    let mut asm = StreamAssembler::new();
    for ev in events {
        asm.push(ev).unwrap();
    }
    let rebuilt = asm.finish().unwrap();
    assert_eq!(rebuilt.text(), "streamed answer");
    assert_eq!(rebuilt.stop_reason, StopReason::EndTurn);
}

#[test]
fn message_user_text_builds_a_single_text_block() {
    let m = Message::user_text("hello");
    assert_eq!(m.role, Role::User);
    assert_eq!(m.content, vec![ContentBlock::text("hello")]);
    assert_eq!(m.text(), "hello");
}
