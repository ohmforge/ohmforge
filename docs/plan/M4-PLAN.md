# M4 plan — vision / stretch

M0–M3 are complete (60/60, v0.1.0). M4 is the roadmap's **vision** tier: scale, extensibility, reach. It's deliberately ambitious, so each item is scoped to a **solid, tested subset** (deliver a correct partial + document what remains, exactly as the AVR / Cortex-M0+ cores were landed) rather than all-or-nothing.

Three roadmap M4 items are intentionally **deferred** (heaviest + fuzziest, poor cost/value right now): real-time **collaborative** sessions, the **QEMU full-SBC** Raspberry Pi bridge, and full vendor-SPICE `.model` fidelity. M4 here takes the tractable, high-value slice of each theme.

No single keystone — the streams are independent. The one standing rule still holds: `ee-core`/`ee-sim` stay dependency-free + `wasm32`-clean + deterministic, so anything native-only (threads) is **feature-gated and off on wasm**, and parallel results must match single-threaded bit-for-bit.

---

## Checkpoints (new)

### sim-core — scale + extensibility
- **CP-SIM-M4.1** Plugin **device-model API**: make the nonlinear-device contract public and add a registry so an external crate can implement a device (its `evaluate`/Jacobian stamp) and have the engine run it. *Gate:* a custom device defined **outside** `ee-sim` (in the test) participates in a real solve and matches hand analysis. (The extensibility headline — boards/instruments/lessons plugin surfaces build on this pattern.)
- **CP-SIM-M4.2** KLU-class sparse: a fill-reducing ordering (approximate-minimum-degree-style) + reuse of the symbolic factorization across repeated solves (transient steps / sweep points). *Gate:* correctness vs the current solver (bit-for-bit or ~1e-12) + a bench showing improved scaling on a large (≈10k-node) net vs the M2 sparse path.
- **CP-SIM-M4.3** Multithreaded analyses: parallelize **independent** runs (Monte-Carlo samples, DC/parametric sweep points) across threads, behind a native-only feature (no threads on wasm), **deterministic** (identical results to single-threaded). *Gate:* same results single- vs multi-threaded; a measured speedup.

### boards — richer cores
- **CP-BRD-M4.1** Cortex-M3/M4 (Thumb-2) — extend the `cortexm` core to a real STM32-class core: the Thumb-2 32-bit instruction subset (data-processing, load/store multiple, MUL/MLA, bitfield, etc.) on top of the ARMv6-M base, plus an STM32 board def. *Gate:* Thumb-2 instruction vectors + an STM32 blink toggles a pin. (Shares the `McuCore`/co-sim surface.)

### ai-fmt — model interop
- **CP-AF-M4.1** SPICE `.model` import + a small vendor device-model library: parse `.model` cards (diode/BJT/MOSFET parameter sets) and apply the params to the engine's models; ship a catalog of a few real parts (e.g. 1N4148, 2N3904, 2N7000). *Gate:* a `.model`-parameterized device simulates with the specified parameters (not the defaults); the catalog resolves known parts.

### ui-shell — extensible instruments + sharing
- **CP-UX-M4.1** Instrument/processing-block **plugin surface** + session export: a trait so external processing blocks/instruments slot into the patcher, and a session export (circuit + patches + probes) to a portable file that reloads identically. *Gate:* a custom block registered via the trait runs in a patch; a session round-trips (export→import identical).

### web — reach
- **CP-WEB.2** Shareable / publishable circuit: encode a circuit (netlist) into a URL fragment the `ee-web` page loads on open, so a circuit is a link you can share — "open in a browser, poke it." *Gate:* encode→decode round-trip; the page loads a circuit from a URL fragment and simulates it.

---

## Fan-out
One agent per track + the web agent, concurrently (the proven worktree rhythm). Merge each as it lands (reconcile any public-API/trait changes), full gate sweep each time. M4's plugin APIs (CP-SIM-M4.1, CP-UX-M4.1) are the throughline — they turn ee-assist from "complete" into "extensible."

## Deferred (documented, not this milestone)
Real-time collaborative editing; the QEMU-hosted full-SBC Raspberry Pi bridge; full vendor-SPICE `.model` fidelity (beyond the parameter subset above). Each is a sizable standalone effort for a later milestone.
