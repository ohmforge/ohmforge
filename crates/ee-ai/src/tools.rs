//! The tool layer over the circuit API — the grounding path (CP-AF-M1.3).
//!
//! Last checkpoint built the tool-use *loop* ([`crate::client`]) with a generic
//! [`ToolHandler`] seam. This checkpoint fills that
//! seam with the concrete tools the model actually gets, and they map onto the
//! **same `eesh` shell and simulator a user drives** — the trust model from
//! docs/AI.md: *the AI can only do what a user could.*
//!
//! Two focused tools:
//!
//! * [`EESH_TOOL`] (`eesh`) — run one `eesh` command line (`add`/`wire`/`probe`/
//!   `run`/`show`/`help`) against the working [`Shell`]; returns the shell's own
//!   text output. A [`ShellError`](ee_shell::ShellError) comes back as a
//!   tool-error so the model can recover, not as a dead loop.
//! * [`READ_OP_TOOL`] (`read_op`) — solve the DC operating point and return one
//!   node's voltage **as a number**. This is the *grounding* path: the figure
//!   the model quotes comes from a real solve of the real circuit, not a guess.
//!
//! [`CircuitTools`] holds the one [`Shell`] the whole conversation mutates and
//! implements [`ToolHandler`], so the loop's tool calls land on a live engine.
//! [`ask`] wires it all together: it runs the client's tool-use loop with this
//! handler and returns the final answer.
//!
//! Tool I/O stays opaque JSON *text* (as the rest of `ee-ai` does), so the
//! module hand-rolls the tiny bit of JSON it needs — a string-field reader for
//! inputs and an escaper for outputs — rather than pulling in `serde`.

use ee_shell::Shell;

use crate::client::{Client, ToolHandler};
use crate::error::AiError;
use crate::message::{SystemBlock, Tool};
use crate::transport::Transport;

/// The `eesh` tool name: run one shell command line against the circuit.
pub const EESH_TOOL: &str = "eesh";

/// The `read_op` tool name: solve the operating point, read one node's voltage.
pub const READ_OP_TOOL: &str = "read_op";

/// The copilot's tool handler: one [`Shell`] the whole tool-use loop builds and
/// reads, exactly as an interactive user would. Construct it, hand `&mut` of it
/// to [`Client::run`](crate::client::Client::run) alongside [`tool_defs`](CircuitTools::tool_defs),
/// and inspect [`shell`](CircuitTools::shell) afterwards to see the real circuit
/// the model drove.
#[derive(Debug, Default)]
pub struct CircuitTools {
    shell: Shell,
}

impl CircuitTools {
    /// A handler over a fresh, empty circuit.
    pub fn new() -> Self {
        CircuitTools {
            shell: Shell::new(),
        }
    }

    /// Borrow the circuit-backing shell — for a caller (or test) to confirm the
    /// tools drove a real engine, and to read the resulting circuit.
    pub fn shell(&self) -> &Shell {
        &self.shell
    }

    /// The tool definitions to advertise to the model. Schemas are JSON-Schema
    /// *text*, matching [`Tool`]'s opaque-schema contract.
    pub fn tool_defs() -> Vec<Tool> {
        vec![
            Tool::new(
                EESH_TOOL,
                "Run one eesh command line against the working circuit, exactly as a user \
                 would type it. Grammar: `add <ref> <a> <b> <value>` (R/C/L/V/I from the \
                 ref's first letter, e.g. `add R1 in out 1k`), `wire <a> <b>`, `probe \
                 <node>`, `run op`, `run [tran [<dur>]]`, `show`, `help`. Returns the \
                 shell's text output; a bad command comes back as an error you can fix.",
                r#"{"type":"object","properties":{"command":{"type":"string","description":"a single eesh line, e.g. \"add R1 in out 1k\" or \"run op\""}},"required":["command"]}"#,
            ),
            Tool::new(
                READ_OP_TOOL,
                "Solve the DC operating point of the current circuit and return one node's \
                 voltage as a number. Use this to ground any numeric claim in a real solve \
                 rather than guessing. The node must already exist (introduced by an `add`).",
                r#"{"type":"object","properties":{"node":{"type":"string","description":"the node name to read, e.g. \"out\""}},"required":["node"]}"#,
            ),
        ]
    }

    /// The system prompt that frames the trust model for the copilot.
    pub fn system_prompt() -> Vec<SystemBlock> {
        vec![SystemBlock::new(
            "You are the ee-assist copilot. You act only through the provided tools, which \
             drive the same `eesh` shell and SPICE-style engine a user drives — you can do \
             nothing a user could not. Build or inspect the circuit with the `eesh` tool, \
             then ground every numeric answer in a real `read_op` reading. Never invent a \
             voltage: measure it.",
        )]
    }

    /// `eesh { command }` — run the line, returning the shell's text output.
    fn run_eesh(&mut self, input: &str) -> Result<String, String> {
        let command = json_string_field(input, "command")
            .ok_or_else(|| format!("{EESH_TOOL}: missing string field \"command\" in {input}"))?;
        match self.shell.execute(&command) {
            Ok(text) if text.is_empty() => Ok("(ok)".to_string()),
            Ok(text) => Ok(text),
            Err(e) => Err(e.to_string()),
        }
    }

    /// `read_op { node }` — solve the operating point and read one node, as JSON
    /// `{"node":...,"volts":...,"display":"... V"}`. The number is the grounded
    /// reading; `display` echoes the shell's six-decimal rendering.
    fn run_read_op(&mut self, input: &str) -> Result<String, String> {
        let node = json_string_field(input, "node")
            .ok_or_else(|| format!("{READ_OP_TOOL}: missing string field \"node\" in {input}"))?;
        let table = self.shell.execute("run op").map_err(|e| e.to_string())?;
        let volts = parse_node_volts(&table, &node).ok_or_else(|| {
            format!("{READ_OP_TOOL}: node {node:?} is not in the operating point")
        })?;
        Ok(format!(
            "{{\"node\":\"{}\",\"volts\":{volts},\"display\":\"{volts:.6} V\"}}",
            json_escape(&node)
        ))
    }
}

impl ToolHandler for CircuitTools {
    fn call(&mut self, name: &str, input: &str) -> Result<String, String> {
        match name {
            EESH_TOOL => self.run_eesh(input),
            READ_OP_TOOL => self.run_read_op(input),
            other => Err(format!("unknown tool {other:?}")),
        }
    }
}

/// Ask the copilot `question`, letting it build and read a circuit through the
/// tools, and return its final answer. This is the whole grounded "ask": the
/// client's tool-use loop runs against a real [`Shell`], so a stubbed model
/// (a [`MockTransport`](crate::transport::MockTransport)) still gets *real*
/// measurements fed back. For a caller that needs the transcript or the built
/// circuit, drive [`Client::run`](crate::client::Client::run) directly with a
/// [`CircuitTools`] instead.
pub fn ask<T: Transport>(
    client: &Client<T>,
    question: impl Into<String>,
) -> Result<String, AiError> {
    let mut tools = CircuitTools::new();
    let result = client.run(
        &CircuitTools::system_prompt(),
        &CircuitTools::tool_defs(),
        question,
        &mut tools,
    )?;
    Ok(result.final_text)
}

// ── tiny JSON helpers (tool I/O is opaque JSON text; no serde) ──────────────

/// Read a string-valued field `key` out of a JSON object `src`. Returns `None`
/// if the key is absent or its value is not a string. Handles standard string
/// escapes in the value. Deliberately minimal — enough for a tool's flat input
/// object, not a general JSON parser.
fn json_string_field(src: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let bytes = src.as_bytes();
    let mut from = 0;
    while let Some(rel) = src[from..].find(&needle) {
        let mut i = from + rel + needle.len();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b':' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'"' {
                return Some(parse_json_string(&src[i..]));
            }
            return None; // key present but value is not a string
        }
        from = from + rel + needle.len();
    }
    None
}

/// Decode a JSON string literal starting at its opening quote in `s`, stopping
/// at the matching close quote.
fn parse_json_string(s: &str) -> String {
    let mut chars = s.chars();
    debug_assert_eq!(chars.next(), Some('"'));
    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('/') => out.push('/'),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('b') => out.push('\u{0008}'),
                Some('f') => out.push('\u{000C}'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        out.push(ch);
                    }
                }
                Some(other) => out.push(other),
                None => break,
            },
            _ => out.push(c),
        }
    }
    out
}

/// Escape a string for embedding in JSON output.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Pull a node's voltage out of the shell's `run op` table, whose rows read
/// `  v(<node>) = <volts> V`.
fn parse_node_volts(table: &str, node: &str) -> Option<f64> {
    let needle = format!("v({node}) = ");
    for line in table.lines() {
        if let Some(idx) = line.find(&needle) {
            let rest = &line[idx + needle.len()..];
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Client;
    use crate::message::{ContentBlock, Message, ModelResponse, StopReason, Usage};
    use crate::transport::MockTransport;

    fn resp(blocks: Vec<ContentBlock>, stop: StopReason) -> ModelResponse {
        ModelResponse {
            content: blocks,
            stop_reason: stop,
            usage: Usage::default(),
        }
    }

    /// Dig the `volts` number out of the `read_op` tool_result recorded for
    /// `tool_use_id` in the loop transcript.
    fn measured_volts(messages: &[Message], tool_use_id: &str) -> Option<f64> {
        for m in messages {
            for b in &m.content {
                if let ContentBlock::ToolResult {
                    tool_use_id: id,
                    content,
                    ..
                } = b
                {
                    if id == tool_use_id {
                        let key = "\"volts\":";
                        let start = content.find(key)? + key.len();
                        let rest = &content[start..];
                        let end = rest.find([',', '}']).unwrap_or(rest.len());
                        return rest[..end].trim().parse().ok();
                    }
                }
            }
        }
        None
    }

    /// The grounding gate: a STUB model, a REAL sim.
    ///
    /// The canned assistant turns (1) build a 12 V resistive divider with the
    /// `eesh` tool, (2) read `v(out)` with `read_op`, then (3) answer with the
    /// measured value. The model never computes anything — yet the number it is
    /// handed, and quotes, is produced by a real solve of a real circuit. We
    /// prove that by checking the handler's live shell really holds the parts,
    /// that the fed-back reading equals an independent solve (12·2k/3k = 8 V),
    /// and that the final answer carries that real figure.
    #[test]
    fn ask_is_grounded_in_a_real_sim_read() {
        let build = resp(
            vec![
                ContentBlock::tool_use("t1", EESH_TOOL, r#"{"command":"add V1 in gnd 12"}"#),
                ContentBlock::tool_use("t2", EESH_TOOL, r#"{"command":"add R1 in out 1k"}"#),
                ContentBlock::tool_use("t3", EESH_TOOL, r#"{"command":"add R2 out gnd 2k"}"#),
            ],
            StopReason::ToolUse,
        );
        let read = resp(
            vec![ContentBlock::tool_use(
                "t4",
                READ_OP_TOOL,
                r#"{"node":"out"}"#,
            )],
            StopReason::ToolUse,
        );
        // Canned final answer — includes the value the real solve must produce.
        let answer = resp(
            vec![ContentBlock::text(
                "The divider's output node settles at 8.000000 V.",
            )],
            StopReason::EndTurn,
        );

        let client =
            Client::new(MockTransport::new([build, read, answer])).with_prompt_cache(false);

        let mut tools = CircuitTools::new();
        let result = client
            .run(
                &CircuitTools::system_prompt(),
                &CircuitTools::tool_defs(),
                "What voltage does the divider output settle at?",
                &mut tools,
            )
            .expect("the grounded ask loop completes");

        // (1) The tools drove a REAL shell — the circuit genuinely holds 3 parts.
        let mut refs = tools.shell().part_refs();
        refs.sort();
        assert_eq!(
            refs,
            vec!["R1", "R2", "V1"],
            "live circuit built by the tools"
        );

        // (2) The reading fed back to the model is the REAL engine value, not a
        //     canned one: it matches an independent solve of the same divider.
        let mut check = Shell::new();
        for cmd in ["add V1 in gnd 12", "add R1 in out 1k", "add R2 out gnd 2k"] {
            check.execute(cmd).unwrap();
        }
        assert!(
            check
                .execute("run op")
                .unwrap()
                .contains("v(out) = 8.000000 V"),
            "sanity: the divider really solves to 8 V"
        );
        let measured = measured_volts(&result.messages, "t4").expect("read_op fed a number back");
        assert!(
            (measured - 8.0).abs() < 1e-9,
            "grounded read = {measured}, expected 8 V"
        );

        // (3) The final answer reflects that real number.
        assert!(
            result.final_text.contains("8.000000"),
            "answer quotes the measured value: {}",
            result.final_text
        );
        assert_eq!(result.stop_reason, StopReason::EndTurn);
        assert_eq!(result.tool_calls, 4, "3 eesh builds + 1 read_op");
    }

    /// `ask` is the same loop as a one-liner: it returns the stubbed final text.
    #[test]
    fn ask_helper_returns_the_final_answer() {
        let build = resp(
            vec![ContentBlock::tool_use(
                "t1",
                EESH_TOOL,
                r#"{"command":"add R1 a b 1k"}"#,
            )],
            StopReason::ToolUse,
        );
        let answer = resp(
            vec![ContentBlock::text("Done — one resistor placed.")],
            StopReason::EndTurn,
        );
        let client = Client::new(MockTransport::new([build, answer]));
        let text = ask(&client, "Place a resistor.").unwrap();
        assert_eq!(text, "Done — one resistor placed.");
    }

    /// A shell error and an unknown node come back as recoverable tool-errors
    /// (`Err`), which the loop turns into `is_error` results rather than aborting.
    #[test]
    fn tool_errors_are_recoverable_not_fatal() {
        let mut tools = CircuitTools::new();

        // An unknown eesh command → Err carrying the shell's friendly message.
        let bad = tools
            .call(EESH_TOOL, r#"{"command":"frobnicate x"}"#)
            .unwrap_err();
        assert!(bad.contains("frobnicate"), "{bad}");

        // Build a real divider, then read a node that does not exist.
        for cmd in ["add V1 in gnd 10", "add R1 in out 1k", "add R2 out gnd 1k"] {
            tools
                .call(EESH_TOOL, &format!(r#"{{"command":"{cmd}"}}"#))
                .unwrap();
        }
        let ghost = tools.call(READ_OP_TOOL, r#"{"node":"ghost"}"#).unwrap_err();
        assert!(ghost.contains("ghost"), "{ghost}");

        // A real read on the same circuit grounds to the divider midpoint, 5 V.
        let out = tools.call(READ_OP_TOOL, r#"{"node":"out"}"#).unwrap();
        assert!(out.contains("\"volts\":5"), "{out}");
    }

    /// A missing required field is reported, not silently defaulted.
    #[test]
    fn missing_input_field_is_reported() {
        let mut tools = CircuitTools::new();
        assert!(tools.call(EESH_TOOL, "{}").unwrap_err().contains("command"));
        assert!(tools.call(READ_OP_TOOL, "{}").unwrap_err().contains("node"));
    }
}
