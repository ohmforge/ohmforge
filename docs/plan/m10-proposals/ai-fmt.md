# M10 proposals — AI copilot & file-format layer

Scope: `crates/ohmforge-ai` + `crates/ohmforge-fmt` (v0.8.1, M0–M9 complete).
Read-only study of the real code; this doc proposes work, it changes none.

Standing rules honored throughout: the offline path stays network/key-free and
deterministic; `ohmforge-core`/`-sim` stay dependency-free; any new network/TLS
dep stays behind the opt-in `http` feature.

---

## What is already solid (don't re-litigate)

- **The tool-use loop.** `client.rs:178` (`run_streaming`) is clean: it records
  each assistant turn verbatim so `tool_use` ids round-trip, turns recoverable
  tool failures into `is_error` results (`client.rs:249`), and hands back a
  partial `RunResult` on the turn-cap instead of dropping work
  (`client.rs:223`). The `Transport` seam (`transport.rs:28`) keeps the whole
  loop unit-testable with no I/O.
- **The verify/trust split.** `verify.rs:200` clones the base shell, scratch-
  applies, re-solves, and measures; `apply` (`verify.rs:208`) is the *only*
  mutation path. Grounded, not guessed.
- **Fault localization.** `modes.rs:149` `localize_fault` ranks suspects by a
  log-symmetric correction metric from *real* re-solves, with deterministic
  tie-breaking (`modes.rs:235`). Genuinely good.
- **auto_tune bisection** (`tune.rs:109`) is deterministic, bracketed, and
  rides the same clone-measure path as verify.
- **SPICE import.** The `m`-vs-`meg` handling, `+` line-folding
  (`spice.rs:364`), `.model` two-pass resolution with vendor-catalog fallback
  (`spice.rs:313`), and E/G/H/F controlled-source round-trip are mature.
- **The HEX firmware reader** (`modes.rs:605`) validates checksums/EOF properly.

The gaps below are about *reach*, not correcting defects.

---

## Ranked proposals

### 1. Touchstone `.sNp` export for the new S-parameters (fmt) — **TOP**

**What + why.** The M9 S-parameter engine (`ohmforge-sim/src/lib.rs:3169`,
`Engine::s_parameters`) computes a full 2-port `SParams` at **one** frequency,
and the shell renders only `S11` on a Smith chart (`ohmforge-shell/src/lib.rs:1366`).
That data is otherwise stranded: every RF tool (ADS, scikit-rf, VNAs, AWR)
speaks **Touchstone** `.s2p`. A Touchstone exporter is the natural sibling of
the existing CSV/VCD writers (`waveform.rs`) and turns a one-frequency point
into a frequency *sweep* — which is just a loop that calls `s_parameters` per
frequency. Add an `ohmforge_fmt::touchstone::export` that writes the `# HZ S RI
R <z0>` option line, a `!freq S11 S21 S12 S22` comment, and one row per
frequency (2-port column order is `S11 S21 S12 S22`, the Touchstone quirk).

**Effort:** M. **Risk:** Low — additive, pure formatting; the only subtlety is
the 2-port column transposition (well-specified) and RI-vs-MA formatting.

**Cross-crate coordination.** `ohmforge-fmt` depends on `ohmforge-core` only
(`ohmforge-sim` is a dev-dep). Keep it that way: the exporter takes a
dependency-free view — `&[(f64 /*freq*/, [[Complex; 2]; 2] /*S*/)]` using
`ohmforge_core::Complex` (re-exported at `ohmforge-core/src/lib.rs:20`), **not**
the sim `SParams` type. The **shell** owns the sweep: loop `Engine::s_parameters`
over a frequency grid, collect `(freq, s_matrix)`, hand the slice to fmt. This
keeps `-sim` out of fmt's dependency tree and mirrors how `waveform` takes
`Signal`s, not an engine.

**Acceptance gate (deterministic, network-free).** Golden-file test in
`crates/ohmforge-fmt/tests/examples.rs`: a known series-R/shunt-C 2-port at a
fixed 3-point grid exports a byte-exact `.s2p` (eol=lf, matching the existing
`.gitattributes` golden-test discipline). Plus a round-trip unit check: parse
the emitted rows back and assert `|S|` within 1e-9 of a hand-computed reference.

---

### 2. Deeper offline heuristics — DRC rules, calculators, richer catalog (ai) — **TOP**

**What + why.** Offline mode is the honest no-key fallback and a standing-rule
guarantee, yet it is thin: **4** DRC rules (`offline.rs:119`), **7** calculators
(`offline.rs:350`), and a **9-entry** catalog (`offline.rs:516`). All of it is
pure `f64`/graph code over `ohmforge_core` with zero deps — the cheapest,
lowest-risk value in the whole crate. Concretely add:

- **DRC rules** (extend `Rule` at `offline.rs:88` and `check` at `offline.rs:119`):
  electrolytic/polarized-cap reverse-bias; a capacitor with no DC discharge path;
  resistor power dissipation over a rating when the op-point is known; a node
  driven by two voltage sources (contention); a value far off the E12/E24
  series. Each is a graph/op-point walk like the existing ones.
- **Calculators** (extend `calc` at `offline.rs:350`): 555 astable
  frequency/duty, op-amp inverting/non-inverting gain, LC resonance, inductive/
  capacitive reactance, dBV↔ratio, thermal-noise voltage, regulator dropout
  headroom. Same `Result<f64, CalcError>` shape, same `NotFinite`/`NonPositive`
  guards (`offline.rs:390`).
- **Catalog** (extend `CATALOG` at `offline.rs:516`): more regulators (LM317,
  LM1117 family), op-amps (TL072, TLV9062), common MOSFETs (IRLZ44N, AO3400),
  TVS/zener, and crystals — keeping the keyword-scoring search (`offline.rs:578`)
  unchanged.

**Effort:** S–M (incremental; each rule/calculator is independent).
**Risk:** Low — pure, deterministic, no new deps, no network.

**Cross-crate coordination.** None for calculators/catalog. The power-
dissipation DRC rule wants an operating point; keep DRC pure by having it accept
an *optional* pre-solved op-point slice from the caller rather than reaching into
`-sim` (preserves the "DRC imports no Transport/Client" structural property noted
at `offline.rs:12`).

**Acceptance gate.** Unit tests beside the existing ones (`offline.rs:610`):
each calculator has a known-value assertion and a degenerate-input rejection;
each DRC rule has a tripping circuit and a cleared circuit; `check` determinism
re-asserted (`offline.rs:884`). All run with no key/transport
(`offline.rs:980` pattern).

---

### 3. Real token streaming in `HttpTransport` + wasm `fetch` (ai) — **TOP**

**What + why.** The streaming *seam* is built but hollow: the default
`send_streaming` (`transport.rs:38`) just replays an already-finished response
through `response_to_events`, and `HttpTransport` (`transport.rs:190`) never
overrides it — it blocks for the whole reply, so the CLI shows nothing until the
turn completes. A copilot that feels live needs real server-sent-event deltas.
Override `send_streaming` on the native `HttpTransport` to POST with
`"stream": true`, read the SSE body line-by-line, and emit `StreamEvent`s as
`content_block_delta` text arrives — assembling the final `ModelResponse` the
loop already expects. Separately, retire the wasm stub (`transport.rs:229`) by
wiring a `fetch`-based path (still behind `http`), so the browser build can
actually talk to the API.

**Effort:** M (SSE) + M (wasm fetch). **Risk:** Medium — SSE parsing is
fiddly (partial lines across chunks, `event:`/`data:` framing, the terminal
`message_stop`), and it only runs under the opt-in `http` feature, so default/
wasm-clean builds are unaffected. wasm `fetch` is harder to test in CI.

**Cross-crate coordination.** Contained to `ohmforge-ai` under `http`; the
`reqwest` dep already exists (`Cargo.toml`), need its streaming reader. The CLI
(`ohmforge-shell`/binary) gets a visible win via the existing `on_event` sink
(`client.rs:184`) — no signature change.

**Acceptance gate.** Deterministic, network-free: a test `Transport` that
replays a **canned SSE byte stream** (recorded Anthropic frames) through the
same line-parser the live path uses, asserting the emitted `StreamEvent`
sequence and the reassembled `ModelResponse` match a fixture. The parser is
factored out of the HTTP client so it is testable without a socket. wasm path
gated by a `wasm-bindgen-test` smoke test (not in the default gate).

---

### 4. SPICE subcircuits: `.subckt`/`.ends` + `X` instances (fmt)

**What + why.** Today `.subckt`/`.ends` are silently skipped
(`spice.rs:340`) and an `X` instance line is a hard error
(`UnknownElement 'X'`, exercised at `spice.rs:1828`). Real-world decks and
vendor model libraries are full of subcircuits (op-amps, regulators, every
`.lib` part). Support a third import pass that (a) collects `.subckt NAME
nodes…` / `.ends` blocks, then (b) **flattens** each `X<ref> nodes… NAME`
instance by cloning the subckt body with its internal nets uniquified
(`<ref>.<internalnode>`) and its formal ports bound to the caller's nodes —
reusing the existing element parser per inner line. Export (`spice.rs:949`) can
later emit a `.subckt` for a repeated composite, but import-flatten is the
high-value half and composes with `ohmforge-devices`' existing
expand-into-primitives subcircuits (`ohmforge-devices/src/subcircuits.rs`).

**Effort:** L — recursive expansion, node-scoping/uniquification, nested
subckts, and clear errors (unknown subckt, port-count mismatch, recursion
cycle). **Risk:** Medium — scoping bugs are subtle; contain by flattening only
(no hierarchy kept) at M10.

**Cross-crate coordination.** `ohmforge-fmt` stays core-only; a flattened
subckt is just more `Part`s. Optionally reconcile with
`ohmforge-devices` so a known subckt name maps to a built-in composite instead
of textual expansion — a follow-up, not a blocker.

**Acceptance gate.** Golden + simulate test (`tests/examples.rs` uses the
`-sim` dev-dep already): import a deck with a 2-resistor divider `.subckt`
instanced twice, assert the flattened circuit has the right part count with
uniquified nets, and that it *solves* to the expected node voltages — proving
the flatten is electrically correct, not just textual.

---

### 5. Multi-parameter, gradient-free auto-tune (ai)

**What + why.** `auto_tune` (`tune.rs:109`) is powerful but narrow: it tunes
**one** two-terminal part (`candidate_patch` at `tune.rs:197`) against a single
monotone criterion via bisection, and bails if the target isn't bracketed
(`tune.rs:160`). Many real goals are 2-D and non-monotone (a divider's two
resistors for both a voltage *and* a source-impedance target; an RC for a cutoff
*and* a Q). Add a `MultiTune` that adjusts N parameters against a `Spec`'s
criteria, minimizing summed normalized criterion error with a **deterministic,
gradient-free** search — coordinate descent with golden-section line search, or
a fixed-seed Nelder–Mead — still riding the existing `verify` clone-measure path
(`tune.rs:129`) so every evaluation is a real solve and the live circuit is
never touched.

**Effort:** M. **Risk:** Medium — convergence/termination on flat or multi-
modal landscapes; mitigate with a hard evaluation cap (like `max_iter` at
`tune.rs:52`) and a documented "best-so-far" return. Keep the existing 1-D
bisection as the fast path for a single monotone criterion.

**Cross-crate coordination.** None — reuses `verify`/`Spec`/`Shell` already in
the crate; no new dep (the PRNG for a seeded simplex restart can mirror the
in-crate seeded PRNG `-sim` uses for Monte-Carlo, kept local).

**Acceptance gate.** Deterministic unit test: a 2-resistor divider tuned for a
target `v(out)` *and* a target `i` through the load converges to the analytic
pair within tolerance, lands bit-identically on a second run (mirroring
`tune.rs:230`), and leaves the base shell untouched — all offline, no key.

---

### 6. Grounding + autonomy: offline DRC as a copilot tool, KiCad export (smaller wins)

**What + why.** Two modest, independent reach items:

- **Wire offline DRC into the live loop** as a `review` tool alongside `eesh`/
  `read_op` (`tools.rs:67`). The deterministic findings from `offline::review`
  (`offline.rs:39`) become grounding the model can *cite* ("R1 has no series
  resistor") instead of re-deriving — strengthens the design/debug modes'
  grounding at near-zero cost. Effort S, risk low.
- **KiCad *export*.** Import exists (`kicad.rs:1`, `import` only — no exporter in
  the `pub use` at `lib.rs:27`); a netlist exporter closes the round trip for
  users who live in KiCad. Effort M, risk low, additive. A golden-file +
  re-import round-trip test is the gate.

**On Gerber (asked, answered honestly):** out of scope for M10. Gerber is a PCB
*layout/geometry* format (apertures, copper polygons, drill files); the
`ohmforge-core` data model is a schematic-level netlist with no placement,
footprints, or board geometry. Emitting Gerber would mean inventing a layout
engine first — a milestone of its own, not a format-layer add. Recommend
deferring until there is a board model to export.

---

## Suggested sequencing

Ship **#1 (Touchstone)** and **#2 (offline heuristics)** first — highest value,
lowest risk, each fully gated by a deterministic network-free test and neither
touches `-core`/`-sim` deps. **#3 (real streaming)** is the biggest felt-UX win
but carries the most test-harness work; do it in parallel behind `http`. **#4**
and **#5** are deeper and can follow. **#6** items are opportunistic fillers.
</content>
</invoke>
