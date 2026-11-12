# M2 execution plan

**Goal (from [`../ROADMAP.md`](../ROADMAP.md) M2):** run **real firmware against a simulated circuit**, add statistical/mixed-signal analyses, and let the AI *design and debug with verification*. The M2 demo: *wire a Pico + sensor + LED, have the AI write the firmware, co-simulate, watch the logic analyzer decode the I²C traffic, and export a VCD.*

Sequencing + fan-out strategy on top of the M2 checkpoints already in [`CHECKPOINTS.md`](CHECKPOINTS.md). The ledger owns each gate; this doc owns the order.

---

## 1. The risk to retire first

The risk register names it: **co-simulation timing — the MCU clock vs. the analog timestep is fiddly.** Everything in the M2 demo (firmware driving real nodes, the logic analyzer, the Pico) stands on **`CP-BRD-M2.1`**, the lockstep bridge. So the bridge goes first.

**Architectural constraint (locked in [`track-boards.md`](track-boards.md)):** `ohmforge-mcu` does **not** depend on `ohmforge-sim`. The lockstep *loop* lives in the **`of` binary** — the only place that legitimately links both engines. The MCU exposes a pure **`PinIo`** surface (digital drives out, sampled node voltages in) plus `next_event()` / `step_to(t, &mut io)`; the loop reconciles that with the graph:

```
loop:
  t_next = min(core.next_event(), sim.next_event(), next sample)
  core.step_to(t_next, &mut io)   # instructions + peripherals → pin drives
  apply io.drives onto the circuit (boundary elements); sim.advance_to(t_next)
  sample node voltages back into io.inputs (ADC, digital thresholds)
```

So `CP-BRD-M2.1` splits in two: the **bridge surface** (in `ohmforge-mcu`, unit-testable — a boards-track agent) and the **lockstep loop + demo** (in `of` — the integrator, after the surface merges). The gate — *`analogRead` returns a divider's voltage; `digitalWrite` lights a sim LED* — is proven by the loop in the binary.

---

## 2. Dependency graph

```
boards:  M2.1 bridge surface ─▶ [ee binary: co-sim loop] ─▶ M2.2 mixed-signal boundary (thresholds/drive/edges)
         M2.3 RP2040 (Cortex-M0+) ── independent new core (like AVR was)
         M2.4 ATmega2560/ATtiny ── variant maps on the AVR core
         M2.5 external toolchain compile ── independent, skip-gated

sim:     M2.3 controlled sources (VCVS/VCCS/…) ── foundational, enables op-amps/behavioral
         M2.1 noise (builds on AC) · M2.2 Monte-Carlo (seeded) ── independent
         M2.4 sparse solver ── perf, independent (bench-gated)

ui:      M2.3 Bode/spectrum + scope FFT ── builds on AC (have), visible now
         M2.1 logic analyzer + UART/SPI/I²C decode ── pairs with co-sim digital traffic
         M2.2 breadboard view · M2.4 snapshots/undo ── independent

ai:      M2.1 verification loop ── builds on the M1 tool layer; the trust centerpiece
         M2.3 VCD/CSV export ── independent · M2.2 Design/Debug/Firmware modes (needs co-sim for Firmware)

platform: WASM browser front end (core already wasm-clean) · PLAT.2 aarch64 · PLAT.5 determinism harness
```

**Critical path to the demo:** `BRD-M2.1 → co-sim loop → BRD-M2.3 (Pico)` for the firmware-against-circuit half; `UX-M2.1` (I²C decode) and `AF-M2.3` (VCD) to close it; `AF-M2.1/M2.2` for the AI writing/verifying firmware. The Pico core and the verification loop are the long poles and should start early.

---

## 3. Contract changes M2 introduces

| Contract | Change | Owner → affected |
|---|---|---|
| `PinIo` / `McuCore` (`ohmforge-mcu`) | digital drive-out + analog sample-in + `next_event`/`step_to(t, &mut io)` | boards → the `of` co-sim loop |
| Boundary elements | digital↔analog at a pin (threshold in, drive-strength/rise-fall out) | boards/sim → co-sim loop |
| Controlled sources | `PartKind` VCVS/VCCS/CCVS/CCCS (or a behavioral device) | sim → ohmforge-fmt/ohmforge-tui/ohmforge-shell (the usual match reconciliation) |
| `Analysis::Noise`, `Analysis::MonteCarlo` | new variants; MC carries a **seed** (deterministic — an in-crate PRNG, never unseeded) | sim → `Engine` match only |
| VCD/CSV waveform format | new `ohmforge-fmt` export module | fmt → consumers |
| Staged patch / verification | `ohmforge-ai` apply/reject over a scratch circuit | ai (builds on M1 tool layer) |

Land the small foundational ones early (controlled sources; the `PinIo` surface), so dependents compile against the final shape — same discipline as M1's `Signal::Complex` bump.

---

## 4. Proposed fan-out waves

One agent per track per wave, each in its worktree; merge + reconcile + full gate sweep as each lands (the rhythm that's held through M0/M1).

### Wave 1 — retire the risk, start the long poles, land the trust piece
| Agent | Worktree | Checkpoint |
|---|---|---|
| boards | `track/boards` | **CP-BRD-M2.1** bridge *surface* in `ohmforge-mcu` (PinIo, event scheduling, unit-tested) |
| sim | `track/sim-core` | **CP-SIM-M2.3** controlled/behavioral sources (VCVS/VCCS → op-amp) |
| ui-shell | `track/ui-shell` | **CP-UX-M2.3** Bode/spectrum panel + scope FFT (makes AC visible) |
| ai-fmt | `track/ai-fmt` | **CP-AF-M2.1** verification loop (propose → scratch-apply → re-sim → measure → apply/reject) |

*Integrator, after Wave 1 merges:* build the **lockstep co-sim loop in `of`** on the merged `PinIo` surface, with the `analogRead`/`digitalWrite` demo — the `CP-BRD-M2.1` gate.

### Wave 2 — build on the bridge
| Agent | Checkpoint |
|---|---|
| boards | **CP-BRD-M2.3** RP2040 / Cortex-M0+ core (the long pole) |
| sim | **CP-SIM-M2.1** noise + **M2.2** Monte-Carlo (seeded) |
| ui-shell | **CP-UX-M2.1** logic analyzer + UART/SPI/I²C decode |
| ai-fmt | **CP-AF-M2.3** VCD/CSV export → then **CP-BRD-M2.2** mixed-signal boundary (sim/boards) |

### Wave 3 — breadth + platform
| Agent | Checkpoint |
|---|---|
| boards | **CP-BRD-M2.4** ATmega2560/ATtiny · **M2.5** toolchain compile (skip-gated) |
| sim | **CP-SIM-M2.4** sparse solver (bench-gated scaling) |
| ui-shell | **CP-UX-M2.2** breadboard view · **M2.4** snapshots/undo |
| ai-fmt | **CP-AF-M2.2** Design/Debug/Firmware modes |
| platform | **WASM browser front end** · `CP-PLAT.2` aarch64 · `CP-PLAT.5` determinism harness |

---

## 5. Risks & handling

- **Co-sim timing (highest):** lockstep to the nearest event (never fixed-ratio); start with the well-documented AVR/Uno case and validate pin+node golden traces against reference sketches; keep the loop deterministic (shared `ohmforge-core::time` clock, no wall-clock).
- **MCU accuracy (RP2040):** ARMv6-M (Thumb) instruction-accurate first; validate against published test vectors; cycle-accuracy only for timing-critical peripherals.
- **Monte-Carlo determinism:** an in-crate seeded PRNG (xorshift/PCG), seed in the request; a fixed seed reproduces bit-for-bit (the compute core never uses unseeded `rand`).
- **AI trust:** nothing auto-applies — the verification loop stages a patch on a **scratch** circuit, re-simulates, cites measured values, and gates on explicit `apply`/`reject` ([`../AI.md`](../AI.md)). Tests use a stub model + real sim.
- **Toolchain tests:** feature/skip-gated so default `cargo test --workspace` needs no external compiler.

---

## 6. Definition of done for M2

Repo stays green (tests, clippy `-D warnings`, fmt, wasm core, docs) and the demo runs: a Pico + sensor + LED circuit; firmware (AI-written, verified) co-simulated so `digitalWrite`/`analogRead` move real nodes; the logic analyzer decodes the I²C traffic; a VCD exports and opens in GTKWave.

---

## 7. Decisions (defaults chosen; override anytime)

1. **Co-sim loop in `of`**, bridge surface in `ohmforge-mcu` — per the architecture review. (Locked.)
2. **Second MCU = RP2040 / Pico** (Cortex-M0+), per the ledger. (Default.)
3. **Monte-Carlo PRNG:** in-crate seeded (no dependency), seed in the `Analysis`. (Default.)
4. **WASM front end** scoped to Wave 3 as a thin browser shell over the already-wasm-clean core (not a full UI port). (Default.)
