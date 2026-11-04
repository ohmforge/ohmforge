# M5 plan — close the debt + extend the frontier

M0–M4 are complete (67/67, v0.2.0). M5 does two things at once: **pay down the documented integration/model debt** so the capabilities we built are fully realized, and **extend reach** (edit in the browser, share whole sessions, richer peripherals + device models). Fanned out one agent per track + web, the proven merge-and-gate rhythm.

No single keystone; the hardest item is wiring the KLU solver into the engine's default path without perturbing the golden bit-for-bit suite. Standing rules hold: `ee-core`/`ee-sim` stay dependency-free + `wasm32`-clean + deterministic; browser-only deps gated to `wasm32`; the host workspace stays green.

---

## Checkpoints (new)

### sim-core — realize the perf win + finish the diode
- **CP-SIM-M5.1** Wire the KLU-class ordered/reusable sparse solver (built in M4.2) into the engine's **default** hot loop — the NR iterations, transient steps, and sweep points — so large circuits actually get the near-linear solve. Keep results bit-for-bit vs the current solver and the golden determinism suite exact (ordered solve on by default above a size threshold; small circuits unchanged). *Gate:* the golden suite + `large_ladder_uses_sparse_and_matches_dense` still pass bit-for-bit, and a transient/op on a large net measurably speeds up.
- **CP-SIM-M5.2** `DiodeModel` in `ee-core` (Is, n, Rs) consumed by the engine's diode so SPICE `.model` `N`/`RS` fully apply (not just `Is`). Keep default-diode results unchanged. *Gate:* a diode with `N`≠1 and `RS`>0 shifts the I–V curve as the model predicts; the existing `.model` import test now applies `N`.

### ai-fmt — close the format round-trips
- **CP-AF-M5.1** SPICE **device export**: emit real `D`/`Q`/`M` element lines + `.model` cards (and `E/G/H/F` controlled sources) instead of skipped-comments, round-tripping with the importer. *Gate:* export→import of a circuit with a diode/BJT/MOSFET preserves kind + model params.
- **CP-AF-M5.2** Native project **waveform round-trip**: serialize a source's `Waveform` (Dc/Sine/Pulse/Expr) + per-part tolerances/tempco so `save`→`load` is truly lossless. *Gate:* a circuit with a sine/expr source and a tempco round-trips exactly.

### boards — richer peripherals + device models
- **CP-BRD-M5.1** On-chip **SPI + I²C (TWI) master** on the AVR core (and the exchange surface so the logic analyzer sees the bus). *Gate:* an AVR program clocks out SPI/I²C bytes that the logic-analyzer decoder recovers correctly.
- **CP-BRD-M5.2** Composite **device subcircuits** in `ee-devices` (expanded into existing primitives — no new `PartKind`): an op-amp macromodel (finite gain + output R via VCVS+RC), an analog switch (voltage-controlled resistance), a relay. *Gate:* the op-amp macromodel in a non-inverting config hits the expected closed-loop gain; the switch's on/off resistance gates a signal.

### ui-shell — edit in the TUI
- **CP-UX-M5.1** Interactive **schematic editing** in the live TUI app: place/remove/wire parts through the app's event loop (not only the shell), driving the same `Circuit`. Keep the event logic a pure, testable state machine. *Gate:* app-level edit events add/wire/remove parts (tested without a tty); the schematic/overlay pane reflects them.

### web — edit + share
- **CP-WEB.3** Browser **schematic editor**: build/edit a circuit in the page (a form or lightweight canvas over the netlist), not just paste text; re-simulate on change. *Gate:* host-testable edit→netlist logic round-trips; the page edits + re-runs.
- **CP-WEB.4** **Full-session shareable links** (async collaboration): encode the whole session — circuit + probes + scope settings — into a link so someone opens your exact workbench. *Gate:* session encode→decode round-trip (host test); the page restores a shared session. (Real-time co-editing needs a relay/CRDT backend — documented as the deeper follow-up.)

---

## Fan-out
One agent per track + web, concurrently. Merge each as it lands (reconcile new public structs/`PartKind`/`.model` surfaces), full gate sweep each time; re-tag (v0.3.0) when M5 closes.

## Still deferred (need a backend / are large standalone efforts)
Real-time collaborative co-editing (needs a signaling/relay server + CRDT), the QEMU full-SBC Raspberry-Pi bridge, and full vendor-SPICE `.model` parameter fidelity.
