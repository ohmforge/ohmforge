# The AI copilot (`ee-ai`)

The copilot's one rule (see [`docs/AI.md`](../AI.md)): **the AI works through the
same API you do, and verifies its own claims by running the simulator.** It does
not emit prose a human must trust and transcribe — it *acts* through tools that
drive the same `eesh` shell and `ee-sim` engine you drive, reads the real
results back, and iterates. Nothing it proposes touches your circuit until you
approve it.

This guide covers the four modes, the `Transport` seam that keeps the whole loop
network-free and testable, and how secrets are handled. The worked examples use
the real public API with a **stub model** (`MockTransport`) so they run with no
network and no key — exactly how the crate's own tests exercise the loop.

## From the command line (`ee`)

The copilot is reachable from the binary:

| Command | Needs a key? | What it does |
|---|---|---|
| `ee review [file.eesh]` / `ee drc …` | **No** | Builds the circuit from an `.eesh` script (a path, else stdin) and prints the grounded, offline design-rule review (DRC + sanity checks). |
| `ee ask <question>` | Yes | The live copilot — drives a real shell+engine through the tool loop and answers. Build with `--features http` and set `ANTHROPIC_API_KEY`. |

```sh
# Offline, no key — the AI surface you can always reach:
printf 'add V1 in gnd 5\nadd R1 in out 1k\nadd D1 out gnd\n' | ee review

# Live copilot (opt-in network transport):
cargo run -p ee --features http -- ask "design a 5 V to 3.3 V divider and check it"
```

The live path reads the endpoint from `EE_BASE_URL`/`ANTHROPIC_BASE_URL` (below),
so it works through a proxy or gateway unchanged.

## The trust model, in code

Everything above the model lives behind one synchronous trait:

```rust
pub trait Transport {
    fn send(&self, req: ModelRequest) -> Result<ModelResponse, AiError>;
    // `send_streaming` has a default that replays the finished response as
    // StreamEvents; a real transport overrides it to surface deltas as bytes
    // arrive.
}
```

* **`MockTransport`** is handed a queue of canned `ModelResponse`s; each `send`
  pops the next and records the request, so a test both drives a multi-turn tool
  loop and asserts on exactly what the client sent. Every example below uses it.
* **`HttpTransport`** (behind the `http` feature) is the real Anthropic path. On
  **native** it is **live**: a `reqwest` blocking client (rustls, so no
  OpenSSL/system-TLS dependency) that reads the key from the environment, POSTs
  the Messages request with a bounded request + connect timeout, and parses the
  JSON response back — proven by `tests/http_transport.rs`. Only the **`wasm32`
  `fetch`** path is still a documented TODO stub. With the feature **off** (the
  default) the crate pulls in no third-party crates and compiles for `wasm32`.

`Client::run` drives the Messages-API tool loop to completion: send → if the
model asks for tools, run each through the caller's `ToolHandler` and feed the
`tool_result` blocks back → otherwise stop and return the assembled text. A
`max_tool_turns` cap (default 16) means a model that never stops asking for
tools fails with `ToolLoopExhausted` rather than spinning forever. The static
prefix (system prompt + tool schema) is marked for **prompt caching** by
default.

## Secrets & model selection

Resolved from the environment at runtime, never hard-coded or committed:

| Variable | Purpose |
|---|---|
| `EE_MODEL`, then `ANTHROPIC_MODEL` | override the pinned `DEFAULT_MODEL` (first non-blank wins) |
| `ANTHROPIC_API_KEY` | the key the real HTTP transport reads; the mock never needs one |
| `EE_BASE_URL`, then `ANTHROPIC_BASE_URL` | override the API endpoint — point the client at a proxy or gateway (first non-blank wins; else the stock `api.anthropic.com` endpoint) |

The precedence lives in pure resolvers (`resolve_model`, `resolve_api_key`,
`resolve_base_url`) so it is unit-tested without mutating the process
environment. A missing/blank key is a clear `AiError::MissingApiKey`; the key is
held, never logged. The base URL is read by `HttpTransport::from_env` alongside
the key, so a proxy or gateway is selected without a code change.

---

## Ask — grounded in a real sim read

`ask` builds or inspects a circuit through two tools over a live shell — `eesh`
(run one command line) and `read_op` (solve the operating point and return one
node's voltage **as a number**) — then answers. The number the model quotes
comes from a real solve, not a guess.

```rust
use ee_ai::{ask, Client, MockTransport, ContentBlock, ModelResponse, StopReason,
            Usage, EESH_TOOL, READ_OP_TOOL};

fn resp(blocks: Vec<ContentBlock>, stop: StopReason) -> ModelResponse {
    ModelResponse { content: blocks, stop_reason: stop, usage: Usage::default() }
}

// A stub model: build a 12 V divider, read v(out), then answer with the figure.
let build = resp(vec![
    ContentBlock::tool_use("t1", EESH_TOOL, r#"{"command":"add V1 in gnd 12"}"#),
    ContentBlock::tool_use("t2", EESH_TOOL, r#"{"command":"add R1 in out 1k"}"#),
    ContentBlock::tool_use("t3", EESH_TOOL, r#"{"command":"add R2 out gnd 2k"}"#),
], StopReason::ToolUse);
let read = resp(vec![
    ContentBlock::tool_use("t4", READ_OP_TOOL, r#"{"node":"out"}"#),
], StopReason::ToolUse);
let answer = resp(vec![
    ContentBlock::text("The divider's output settles at 8.000000 V."),
], StopReason::EndTurn);

let client = Client::new(MockTransport::new([build, read, answer]));
let text = ask(&client, "What does the divider output settle at?")?;
// The 8 V in `text` is what a real operating-point solve of the real circuit
// produced (12·2k/3k) and was fed back to the model — a stub model, a real sim.
```

## Design — propose → verify → `apply`/`reject`

Design is the **verification loop**, the crate's trust mechanism. The model
proposes a patch (a list of `eesh` commands) through the `propose_patch` tool;
`ee-ai` **scratch-applies it to a clone of your circuit**, re-simulates, and
measures the spec quantities, feeding the model a structured verdict with the
real numbers. The live circuit is borrowed immutably throughout — `design`
*cannot* change it. Committing is a separate, explicit `apply` you make after
the user approves.

Worked example — "give me 3.3 V at 500 mA from a 5 V rail":

```rust
use ee_ai::{design, apply, Client, MockTransport, Criterion, Spec,
            ContentBlock, ModelResponse, StopReason, Usage, PROPOSE_PATCH_TOOL};
use ee_shell::Shell;

// The user's starting circuit: a bare 5 V rail on node `in`.
let mut live = Shell::new();
live.execute("add V1 in gnd 5").unwrap();

// The spec: out at 3.3 V (±50 mV) while the 6.6 Ω load draws 500 mA (±20 mA).
let spec = Spec::new("5 V -> 3.3 V @ 500 mA", vec![
    Criterion::node_volts("out", 3.3, 0.05),
    Criterion::resistor_current("RL", 0.5, 0.02),
]);

// Stub model: propose a 3.4 Ω series dropper into the 6.6 Ω load, then present.
let propose = ModelResponse {
    content: vec![ContentBlock::tool_use("p1", PROPOSE_PATCH_TOOL,
        r#"{"summary":"3.4R dropper into 6.6R load","commands":["add RL out gnd 6.6","add R1 in out 3.4"]}"#)],
    stop_reason: StopReason::ToolUse, usage: Usage::default(),
};
let present = ModelResponse {
    content: vec![ContentBlock::text("Verified: v(out) = 3.300000 V at 0.500000 A.")],
    stop_reason: StopReason::EndTurn, usage: Usage::default(),
};
let client = Client::new(MockTransport::new([propose, present]));

let outcome = design(&client, &live, spec, "3.3 V at 500 mA from the 5 V rail")?;

assert!(outcome.is_verified());          // a patch was staged AND its verdict passed
assert_eq!(live.part_refs(), vec!["V1"]); // the live circuit is STILL untouched

// Only now, after the user approves, does anything commit:
if outcome.is_verified() {
    apply(&mut live, outcome.proposal.as_ref().unwrap())?;
}
// `reject(patch)` instead consumes the patch by value so it can't be applied by
// mistake. If a proposal misses the spec the verdict reports the measured gap,
// and the model iterates on that real evidence — still touching nothing.
```

## Debug — localize a wrong resistor

Given a misbehaving circuit and the *expected* voltage at a target node, Debug
reads the real operating point and **localizes the fault**: it finds the single
component whose smallest value correction would restore the spec (the minimal
single-fault explanation), grounded in real re-solves of a scratch clone.

Worked example — a 10 V divider whose top resistor is wrong (3 kΩ where 1 kΩ was
intended), so `v(out)` sags to 2.5 V where 5 V is expected:

```rust
use ee_ai::{debug_circuit, Client, MockTransport,
            ContentBlock, ModelResponse, StopReason, Usage};
use ee_shell::Shell;

let mut faulty = Shell::new();
faulty.execute("add V1 in gnd 10").unwrap();
faulty.execute("add R1 in out 3k").unwrap();  // the seeded fault (nominal 1k)
faulty.execute("add R2 out gnd 1k").unwrap();

// Stub model: call `diagnose`, then name the culprit from the real report.
let diagnose = ModelResponse {
    content: vec![ContentBlock::tool_use("d1", "diagnose", "{}")],
    stop_reason: StopReason::ToolUse, usage: Usage::default(),
};
let answer = ModelResponse {
    content: vec![ContentBlock::text(
        "Fault is R1: v(out) measures 2.5 V but should be 5 V; R1 needs ~1k.")],
    stop_reason: StopReason::EndTurn, usage: Usage::default(),
};
let client = Client::new(MockTransport::new([diagnose, answer]));

// Check v(out) against an expected 5 V (±50 mV).
let outcome = debug_circuit(&client, &faulty, "out", 5.0, 0.05,
    "The output voltage is wrong — which part is bad?")?;

let report = outcome.report.unwrap();
assert_eq!(report.suspect.as_deref(), Some("R1")); // fingered from a real solve
// report.measured ≈ 2.5, and R1's corrected_value ≈ 1k — the minimal correction,
// ranked ahead of R2 and V1. Debugging never mutated the caller's circuit.
```

## Firmware

The seam for firmware sketches: the model writes a short Arduino-style sketch
and emits its compiled image as an **Intel HEX** string via `emit_firmware`. The
HEX is validated (start code, hex digits, per-record byte count, two's-
complement checksum, EOF record — each error tagged with its record line) and
staged. The validated string is exactly what the co-sim bridge
(`ee-mcu`'s `load_firmware`) consumes, so flashing it into a live emulator is a
thin follow-up; the mode is fully testable with a stub model today.

```rust
use ee_ai::{firmware, Client, MockTransport,
            ContentBlock, ModelResponse, StopReason, Usage};

let emit = ModelResponse {
    content: vec![ContentBlock::tool_use("f1", "emit_firmware",
        r#"{"source":"void setup(){pinMode(13,OUTPUT);}","hex":":0B0010006164647265737320676170A7\n:00000001FF\n"}"#)],
    stop_reason: StopReason::ToolUse, usage: Usage::default(),
};
let answer = ModelResponse {
    content: vec![ContentBlock::text("Sketch compiled and staged — approve to flash.")],
    stop_reason: StopReason::EndTurn, usage: Usage::default(),
};
let client = Client::new(MockTransport::new([emit, answer]));

let outcome = firmware(&client, "Blink the LED on pin 13.")?;
let image = outcome.staged.unwrap();   // validated Intel HEX + its decoded bytes
// A malformed image comes back as a recoverable tool error — nothing is staged.
```

---

## Guardrails (from [`docs/AI.md`](../AI.md))

* **Nothing is applied without approval.** Design/debug output is a *staged*
  patch; `apply`/`reject` is always the user's call. `verify` borrows the base
  circuit immutably and only ever drives a clone, so it cannot mutate the live
  one.
* **Grounded, not guessed.** Every numeric claim is read back from a real solve
  — `read_op` for a voltage, a measured verdict for a design, a re-solved
  correction for a fault.
* **Scoped tools.** The AI can only do what a user could: the tools drive the
  same `eesh` shell and engine. There is no filesystem, network, or toolchain
  access except through explicit, user-visible actions.
* **Network-free core.** The whole loop runs against `MockTransport` with no
  I/O; only the feature-gated HTTP transport ever reaches the wire.
