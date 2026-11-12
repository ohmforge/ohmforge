# M7 plan — RF, real signals & going live

M0–M6 are complete (84/84, v0.4.0). M7 pushes the **frontier** into the
frequency domain and RF — pole-zero + small-signal transfer-function analysis
and transmission lines — while **paying down the three big documented debts**:
the live HTTP transport (the copilot still can't actually reach the API), a real
ESP32 (Xtensa) core, and the hosted signaling relay behind browser co-edit.
Fanned out one agent per track, the proven merge-and-gate rhythm.

The hardest items are sim-core's pole-zero extraction (a hand-rolled eigenproblem
on the linearized MNA — no external linear-algebra crate is allowed) and the
ESP32 core (a new instruction-set, scoped to a correct supported subset like the
AVR/ARM cores before it). Standing rules hold: `ohmforge-core`/`ohmforge-sim` stay
dependency-free + `wasm32`-clean + deterministic; `ohmforge-mcu` keeps no `ohmforge-sim`
dependency; any new network/TLS dependency is gated behind an **opt-in feature**
so the default workspace stays dependency-free and the `wasm32` core stays clean;
and new multi-terminal elements are added as **circuit-level relations** (like
M6's `Circuit::couple`), not new data-carrying `PartKind` variants, to avoid a
cross-crate match-arm cascade.

---

## Checkpoints (new)

### sim-core — frequency-domain depth
- **CP-SIM-M7.1** **Pole-zero + small-signal transfer function (`.TF`)**: from the
  linearized MNA at the operating point, report the DC small-signal gain, the
  input and output impedances at a chosen port, and the pole (and, where
  tractable, zero) locations. Pole extraction is a hand-rolled eigenproblem on
  the `(G, C)` matrices — **no external linear-algebra crate** (a compact QR /
  shifted-QR iteration in `ohmforge-sim`, bounded to modest system sizes with the
  coverage documented). *Gate:* an RC low-pass reports its pole at
  `-1/(RC)` (≈ the −3 dB corner), and a resistive divider's `.TF` gain +
  input/output impedances match hand analysis.
- **CP-SIM-M7.2** **Transmission lines** (lossless ideal + lossy LC), stamped in
  transient **and** AC, added as a circuit-level relation (e.g.
  `Circuit::add_line(port_a, port_b, z0, delay, …)`) — **no new `PartKind`**.
  *Gate:* a source-matched lossless line delays a step by its propagation time;
  an unterminated/ mismatched line shows the expected reflection; AC magnitude
  shows the line's periodic response.

### boards — ESP32 (Xtensa) core
- **CP-BRD-M7.1** *(debt)* **ESP32 Xtensa LX6 core + board**: an
  instruction-accurate core over a correct **supported subset** of the Xtensa
  ISA (unimplemented encodings decode to an `Unknown`, with coverage + TODOs
  documented, exactly as the AVR/ARM cores do), and an `Esp32` board tying it to
  a GPIO block. `ohmforge-mcu` gains no `ohmforge-sim` dependency. *Gate:* a hand-assembled
  Xtensa blink toggles a GPIO pin; the decoder's covered-opcode set is asserted.

### ai-fmt — live HTTP transport
- **CP-AF-M7.1** *(debt)* **Wire the live HTTP transport** behind the existing
  `http` feature (native `reqwest` blocking client; `wasm32` `fetch` via
  `wasm-bindgen`), so the Claude client actually performs a Messages round-trip.
  The dependency is **opt-in** — the default build stays dependency-free and
  `wasm32`-clean; only `--features http` pulls the stack. *Gate:* a feature-gated
  test drives one real round-trip against a **local mock HTTP server** (a tiny
  `std::net` listener returning a canned Messages response) — still no outbound
  network; `HttpTransport::send` returns a parsed `ModelResponse`.

### ui-shell — transient playback
- **CP-UX-M7.1** **Transient "play" / scrub**: a time-cursor playback over a
  cached transient solution — advance/rewind a playhead and the scope + live
  overlay reflect the sampled instant. Pure, testable state machine (no tty).
  *Gate:* a playhead advanced over a cached transient samples the right value at
  each step and the overlay/scope data tracks it; play/pause/seek are asserted on
  data.

### web — hosted signaling relay
- **CP-WEB.6** *(debt)* **Hosted signaling relay**: the server behind M6's
  co-edit so two browsers on **different machines** converge, not just
  same-origin tabs. Keep the room/broadcast routing **pure and host-tested**; the
  socket server is a thin, feature-gated native binary (prefer a hand-rolled
  `std::net` WebSocket upgrade in the house style; any WS/async dependency stays
  behind that native feature so the default workspace and `wasm32` build stay
  clean). *Gate:* host tests of the relay's room join/leave + broadcast routing
  (two clients in a room receive each other's ops and converge); malformed
  frames are rejected cleanly.

### platform — ship (on `main`, not fanned out)
- **CP-SHIP.3** Polish & tag **v0.5.0**: refresh docs/examples/README for M7,
  full CI-parity gate sweep (including the `http` and relay feature jobs), tag
  the release. Done during reconcile on `main`.

---

## Fan-out
One agent per track (sim-core with two checkpoints, boards, ai-fmt, ui-shell,
web), concurrently in their worktrees. Merge each as it lands (reconcile any new
public API surface), full gate sweep each time; `CP-SHIP.3` tags **v0.5.0** when
M7 closes.

## Still deferred (need a backend / are large standalone efforts)
Full vendor-SPICE `.model` fidelity, the QEMU full-SBC Raspberry-Pi bridge,
harmonic-balance / large-signal RF analysis, and a managed/hosted deployment of
the relay (M7 ships the relay binary + routing, not a run-it-for-you service).
