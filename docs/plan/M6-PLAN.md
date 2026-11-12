# M6 plan — coupling & closed loops

M0–M5 are complete (76/76, v0.3.0). M6 pushes the **frontier** toward things that
couple and feed back — magnetic coupling between inductors, firmware that reads a
real analog node and reacts, a schematic you tune live and watch re-solve, an AI
that closes a loop around the verify pass to hit a spec — and in the same pass
**pays down the last two documented debts** (native-project model-param round-trip,
browser real-time co-edit). Fanned out one agent per track, the proven
merge-and-gate rhythm.

The hardest items are the two sim-core pieces: mutual inductance has to stamp
correctly in both transient and AC, and the convergence aids must help hard
circuits **without** perturbing the golden bit-for-bit suite on the easy ones.
Standing rules hold: `ohmforge-core`/`ohmforge-sim` stay dependency-free + `wasm32`-clean +
deterministic; `ohmforge-mcu` keeps no `ohmforge-sim` dependency (the ADC sample crosses the
existing co-sim bridge); browser-only deps gated to `wasm32`; the host workspace
stays green.

---

## Checkpoints (new)

### sim-core — magnetic coupling + convergence robustness
- **CP-SIM-M6.1** Mutual inductance / **transformers**: coupled inductors via a
  coupling coefficient K, stamped in transient **and** AC. Model it as a
  circuit-level relation (e.g. `Circuit::couple(l1, l2, k)`) rather than a new
  data-carrying `PartKind`, to avoid a cross-crate match-arm cascade. *Gate:* an
  ideal 2:1 transformer (K=1) steps a 10 Vpp primary to ~5 Vpp on the secondary
  in transient; K<1 shows leakage; AC transfer matches the turns ratio.
- **CP-SIM-M6.2** Nonlinear **convergence aids**: gmin stepping + source
  (supply) ramping as fallbacks when direct Newton fails to find the op-point.
  *Gate:* a circuit that fails plain NR converges via gmin stepping to the
  correct solution; easy circuits still take the direct path with no extra
  iterations, and the golden determinism suite stays bit-for-bit.

### boards — closed-loop MCU ↔ analog
- **CP-BRD-M6.1** **Closed-loop co-sim**: the AVR ADC samples a real `ohmforge-sim`
  analog node and firmware reacts (threshold → drives a GPIO), plus external
  pin-change interrupts. The ADC sample is injected through the existing
  `PinIo`/co-sim bridge — `ohmforge-mcu` gains no `ohmforge-sim` dependency. *Gate:* firmware
  reads an ADC value tracking an analog divider and sets a GPIO when it crosses a
  threshold; a pin-change ISR fires on an input edge.

### ai-fmt — close the model-param debt + AI auto-tune
- **CP-AF-M6.1** *(debt)* Native-project **device-model param round-trip**:
  diode `IS/N/RS`, BJT `IS/BF/BR`, MOSFET `VTO/KP/LAMBDA` survive `save`→`load`
  byte-identically via a `.model`-style directive (today they default on load).
  *Gate:* a circuit with a custom diode/BJT/MOSFET model round-trips exactly.
- **CP-AF-M6.2** *(frontier)* **AI auto-tune**: closed-loop component-value
  optimization against a measured spec, driven through the existing verify loop
  with a deterministic gradient-free search, fully offline. *Gate:* given a
  target (e.g. `v(out)=3.3 V`), the tuner adjusts a divider resistor and
  converges to the measured target; no network, no key.

### ui-shell — live tuning + current flow
- **CP-UX-M6.1** **Live interactive tuning + current-flow animation**: adjust a
  selected component's value through the app event loop and re-solve live;
  animate branch-current direction/magnitude on the overlay (the current-flow
  animation deferred since M1). Keep the event logic a pure, testable state
  machine. *Gate:* a tune event changes a component and the recomputed
  op-point/overlay reflects it; current-flow arrows are computed from branch
  currents; all asserted on data, no tty.

### web — real-time co-edit relay
- **CP-WEB.5** *(debt)* **Real-time co-edit relay**: an op-based merge layer so
  two browsers converge on one circuit — pure, deterministic conflict resolution
  (host-tested) with a thin `BroadcastChannel`/WebSocket binding gated to
  `wasm32`. *Gate:* two `CircuitModel` replicas exchange ops and converge to an
  identical netlist; conflicting edits resolve deterministically; garbage ops →
  clean `Err`. (The signaling transport stays a thin documented binding.)

### platform — ship (on `main`, not fanned out)
- **CP-SHIP.2** Polish & tag **v0.4.0**: refresh docs/examples/README for M6,
  full CI-parity gate sweep, tag the release. Done during reconcile on `main`.

---

## Fan-out
One agent per track (sim-core, boards, ai-fmt, ui-shell, web), concurrently in
their worktrees. Merge each as it lands (reconcile any new public API surface),
full gate sweep each time; `CP-SHIP.2` tags **v0.4.0** when M6 closes.

## Still deferred (need a backend / are large standalone efforts)
The signaling/relay **server** behind web co-edit (M6 ships the merge logic + a
thin client binding, not a hosted relay), the QEMU full-SBC Raspberry-Pi bridge,
a real Xtensa (ESP32) core, and full vendor-SPICE `.model` parameter fidelity.
