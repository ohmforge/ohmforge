# M3 plan — breadth (workstream) + WASM (webstream)

M0–M2 (+ the M3 interop items KiCad/BOM and offline mode) are complete — 52 ledger checkpoints, 427 tests. M3 is the **"make-anything" breadth phase** plus the deferred **browser front end**. Two concurrent streams:

- **Workstream — M3 breadth:** the LabVIEW-style dataflow patcher, HDL-lite, custom boards + drop-in modules, behavioral sources + richer device models, and a tutor/lessons system. Fanned out across the four tracks (same low-conflict worktree rhythm as M1/M2).
- **Webstream — WASM front end:** the already-wasm-clean compute core, driven from a browser page. A single focused effort in a new `ee-web` crate on a new `track/web` worktree.

Then **polish & ship**: close the documented follow-ups, round out examples + README, keep everything green, tag a release.

This is breadth, not risky-core, so there's no single keystone to retire first — the streams are largely independent. The one hard constraint is the webstream must **not break the host workspace build** (wasm-only deps gated to `wasm32` or `ee-web` excluded from the default members), so `cargo build/clippy/test --workspace` stays green on the host.

---

## Checkpoints (new)

### Workstream — M3 breadth
- **CP-SIM-M3.1** (sim-core) — behavioral/arbitrary-expression sources (a small expression evaluator driving a source) + a richer device-model touch (e.g. resistor tempco / temperature-aware diode saturation). *Gate:* an expression source matches the evaluated waveform; a temp-shifted model moves the operating point the expected direction.
- **CP-BRD-M3.1** (boards) — HDL-lite: truth-table / expression-defined logic blocks, a ROM/RAM memory, and a tri-state bus, on the digital kernel. *Gate:* a ROM-lookup or small adder block simulates correctly; a tri-state bus resolves multiple drivers.
- **CP-BRD-M3.2** (boards) — custom-board manifests (a declarative board: MCU + pin map + onboard parts; an ESP32/STM32-class **stub** core or manifest-only is fine) + drop-in modules (7-seg display, LED, servo, WS2812) as device blocks. *Gate:* load a manifest board; a 7-seg/WS2812 module behaves per its inputs.
- **CP-UX-M3.1** (ui-shell) — dataflow patcher: processing blocks (gain/filter/math/log/limit) wired into a graph that transforms `Signal`s into reusable "virtual instruments", + a pass/fail test sequence. *Gate:* a patch (source → block → measure) computes the expected output; a pass/fail check evaluates correctly.
- **CP-AF-M3.3** (ai-fmt) — tutor mode + guided lessons / graded challenges run against the sim (score pass/fail), with a contextual glossary. *Gate:* a graded challenge auto-scores against a real sim read; a lesson steps.
- **CP-AF-M3.4** (ai-fmt) — Fritzing-style export (and round out BOM/interop). *Gate:* export produces a well-formed artifact for a known circuit.

### Webstream — WASM
- **CP-WEB.1** (web) — a new `ee-web` crate: `wasm-bindgen` bindings exposing the engine (build a circuit / parse a netlist → run op/transient → get a `Signal`), plus a minimal `index.html` + JS shell that runs a sim in the browser and renders the result (canvas or DOM). *Gate:* `wasm-pack`/`cargo build --target wasm32-unknown-unknown -p ee-web` produces a module; a headless-or-documented check runs a sim through the bindings; **host `cargo build/clippy/test --workspace` stays green** (wasm-only deps gated).

### Polish & ship
- **CP-SHIP.1** (platform) — close documented follow-ups where cheap (SPICE diode/transistor export, waveform-source round-trip), expand `examples/` + README/demo, run the full CI-parity sweep, and tag a release (MIT OR Apache-2.0). *Gate:* a clean release checklist; everything green.

---

## Fan-out
Wave 1 — the four M3-breadth track agents + the webstream agent, concurrently. Merge each as it lands (reconcile new `PartKind`/`Analysis`/device variants), full gate sweep each time. Then the polish/ship pass once both streams are in.
