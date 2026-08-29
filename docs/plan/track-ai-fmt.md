# Track design: **ai-fmt** (`ee-ai`, `ee-fmt`)

> The copilot and the file/interop layer. The AI acts through the **same `Command` AST a user types** and must **re-simulate to verify its own suggestions** before presenting them — nothing auto-applies. `ee-fmt` gives every other track durable, diff-friendly persistence and ecosystem interop.
>
> Checkpoints: `CP-AF-M0.1 … CP-AF-M3.2`. Full copilot design: [`../AI.md`](../AI.md). Contracts: [`README.md`](README.md#inter-track-contracts). Cross-cutting rules: [`../ARCHITECTURE.md`](../ARCHITECTURE.md#cross-cutting-design-decisions).

## Scope

| Crate | Owns |
|---|---|
| `ee-fmt` | File formats & interop: the **native project format**, SPICE netlist import/export, KiCad import + BOM export, waveform export (CSV/VCD). |
| `ee-ai` | The copilot: model client, tool definitions over the circuit API, prompt templates, the **verification loop**, RAG over the parts index, and offline heuristics. |

Depends on `sim-core` (to read/verify) and `ui-shell` (it emits the `Command` AST). The AI can do nothing a user couldn't — that's the trust model.

---

## `ee-fmt` — formats & interop

### Native project format (`CP-AF-M0.1`) — foundational

Human-readable, **diff-friendly**, git-able, and **versioned** (a `schema_version` field so old projects still load). `serde`-based. This is the M0 persistence every track needs.
*Gate:* save→load yields an equal `Circuit`; a checked-in golden file stays stable.

```toml
schema_version = 1
[project]  name = "rc-lowpass"
[[part]]   ref = "R1"; kind = "resistor"; value = "1k";   nodes = ["in","out"]
[[part]]   ref = "C1"; kind = "capacitor"; value = "159n"; nodes = ["out","gnd"]
[[probe]]  signal = "v(out)"
```

### SPICE netlist io (`CP-AF-M1.1`)

Import/export standard SPICE netlists — the lingua franca of the wider ecosystem. *Gate:* import a known netlist (sim matches) and export round-trips.

### Waveform export (`CP-AF-M2.3`)

Export `Signal`s to **CSV** and **VCD** (VCD opens in GTKWave etc.). *Gate:* VCD opens in a viewer; CSV matches probe data.

### KiCad import + BOM (`CP-AF-M3.1`, milestone **M3**)

KiCad netlist import; **BOM** export (CSV) listing every part with value/qty. (Moved to M3 to match the roadmap — a doc-review reconciliation.)

---

## `ee-ai` — the copilot

### WASM-safe transport (from the architecture review)

`ee-ai` talks to the model through a **`Transport` trait**, never a hard-wired HTTP client:

```rust
#[async_trait]
pub trait Transport {
    async fn send(&self, req: ModelRequest) -> Result<ModelResponse, AiError>;
}
```

- **Native:** a `reqwest` implementation.
- **WASM:** a `fetch` implementation.
- **Tests:** a **mock** transport with canned responses — so the whole copilot loop is unit-tested **with no network** and the crate compiles for the browser without a native TLS stack.

**Secrets:** the API key is read from env/keyring **at runtime**, never from committed config or the project file (see [`../AI.md`](../AI.md)).

### Claude client (`CP-AF-M1.2`)

Messages API with **tool use**, **streaming**, and **prompt caching**, all over the `Transport` trait. *Gate:* the tool-call loop is unit-tested against the fake transport — no network in tests.

### Tool layer over the circuit API (`CP-AF-M1.3`)

The model's "tools" are exactly the circuit operations: `add`/`wire`/`set`/`run`/`read`. **They emit the `Command` AST** (the `ui-shell` contract) and read `Signal`s — so the model manipulates the *same* graph a user does. *Gate:* "ask" answers a question **grounded in a real sim read** (stub model, real sim).

### The verification loop (`CP-AF-M2.1`) — the trust mechanism

```
propose → stage a patch (Command list)
        → apply to a SCRATCH clone of the circuit
        → re-simulate
        → measure against the spec/question
        → present a diff + the measured numbers
        → wait for `apply` / `reject`   (nothing touches the live circuit without approval)
```

*Gate:* "5 V→3.3 V @ 500 mA" yields a patch whose sim meets spec, and **nothing applies without explicit approval**. This is why release builds use `panic = "unwind"` — a solver panic on a candidate patch must not kill the process (see the architecture decisions).

### Modes (`CP-AF-M2.2`)

- **Ask** — grounded Q&A (M1).
- **Design** — spec → subcircuit (verified before apply).
- **Debug** — reads sim results, localizes a fault ("R3 drops too much").
- **Firmware** — generate a sketch, flash into the emulator (needs `CP-BRD-M2.1`).
- **Explain / tutor** — teaching mode over the live circuit (M3).

*Gate:* Debug localizes a seeded fault; a Firmware sketch flashes and runs.

### Offline mode (`CP-AF-M3.2`, milestone **M3**)

With no API key: a bundled **RAG parts index** + local **heuristics** — value calculators and rule checks (floating node, no ground reference, LED without a series resistor) that fire deterministically. Useful, honest, and network-free. *Gate:* the offline checks fire correctly with no key present.

---

## Testing strategy

- **No network in tests, ever** — the mock `Transport` returns canned tool-call sequences; the loop logic is what's under test, not the model.
- **Verification-loop tests** use a stub model that emits a known patch and assert the scratch-apply → re-sim → measure path gates on the spec.
- **Format round-trips** are golden-file based (native, SPICE, VCD/CSV); the determinism harness (CP-PLAT.5) re-runs them.
- Offline heuristics are pure functions with unit tests per rule.

---

## State & next action

- **Depends on:** the `Circuit` API + `Signal` + `Analysis` (sim-core) to read/verify, and the `Command` AST (ui-shell) to emit. `ee-fmt`'s native format (`CP-AF-M0.1`) has no AI dependency and can land first.
- **Current checkpoint:** `CP-AF-M0.1` — native project format save/load round-trip (the M0 persistence everything needs).
- **Immediate next step:** implement the versioned `serde` project format + round-trip golden test (M0.1 gate); then, once the `Command` AST and a `Signal` read exist, stand up the `Transport` trait + mock and the tool-call loop (`CP-AF-M1.2`/`M1.3`) — network-free from day one.
