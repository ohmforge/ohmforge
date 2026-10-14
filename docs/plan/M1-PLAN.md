# M1 execution plan

**Goal (from [`../ROADMAP.md`](../ROADMAP.md) M1):** go from "a narrow pipeline works" to "a genuinely useful analog simulator with instruments, and an AI that can *explain* what you built." Concretely, the M1 demo: *design an amplifier, sweep its Bode plot, load a blink `.hex` on an Uno, and ask "what's the gain at 1 kHz?" and get a grounded answer.*

This doc is the **sequencing and fan-out strategy** on top of the M1 checkpoints already defined in [`CHECKPOINTS.md`](CHECKPOINTS.md). It does not restate each checkpoint's gate — the ledger owns those.

---

## 1. The one risk to retire first

The risk register ([`../ROADMAP.md`](../ROADMAP.md)) is blunt: **SPICE-class convergence is genuinely hard.** Everything analog-nonlinear in M1 (BJT, MOSFET, op-amp, DC sweep of a diode, AC biasing) stands on **`CP-SIM-M1.1`** — the Newton–Raphson framework + the first nonlinear device (diode). If NR convergence isn't solid, the whole sim-core M1 line is quicksand.

**So the keystone goes first and somewhat alone:** prove `CP-SIM-M1.1` (diode I–V matches Shockley within tol, NR converges) before fanning out the rest of the sim-core line (`M1.2` BJT/MOSFET, `M1.3` convergence aids). We get an early, honest read on the hardest part of M1.

The good news: **three of the four tracks don't depend on nonlinear at all** and can start in parallel immediately.

---

## 2. M1 dependency graph

```
sim-core:  M1.1 (NR+diode) ─▶ M1.2 (BJT/MOSFET) ─▶ M1.3 (convergence aids)
           M1.4 (AC, complex MNA) ── independent of NR for linear nets
           M1.5 (DC/parametric sweep) ── needs op-point (have); nonlinear sweep after M1.1
           M1.6 (adaptive timestep) ── builds on transient (have), independent

boards:    M1.1 (digital kernel) ─▶ M1.2 (logic primitives)
           M1.3 (AVR core) ─▶ M1.4 (peripherals) ─▶ M1.5 (Uno + .hex load)
           └── entire track is independent of analog nonlinear

ui-shell:  M1.1 (oscilloscope) ── on existing Plot + Signal
           M1.2 (DMM/funcgen/PSU) ── funcgen needs a time-varying source (see §5)
           M1.3 (live overlay) ── needs DC op-point (have)
           M1.5 (scripting + replay) ── on existing eesh
           M1.4 (dockable panels + mouse) ── biggest UX lift; last

ai-fmt:    M1.1 (SPICE io) ── on circuit API (have) + our netlist infra
           M1.2 (Claude client / mock transport) ── fully self-contained
           M1.3 (AI tool layer) ── needs M1.2 + emits Command AST + reads Signal
```

**Critical path to the M1 demo:** `SIM-M1.1 → SIM-M1.2` (amplifier devices) and `SIM-M1.4` (Bode) for the analog half; `BRD-M1.3 → M1.4 → M1.5` (Uno blink) for the firmware half — the **AVR core is the long pole** and should start on day one in parallel. The AI "ask" (`AF-M1.2 → M1.3`) closes the demo.

---

## 3. Contract changes M1 introduces

These are the cross-track surfaces (per [`README.md`](README.md) §Inter-track contracts) that M1 touches — each is a coordination point, not a silent edit:

| Contract | Change | Owner → affected |
|---|---|---|
| `Signal` (`ee-core`) | add `SignalData::Complex` for AC magnitude/phase | sim-core → tui, ai, instruments |
| `Analysis` (`ee-sim`) | add `DcSweep`, `Ac`, `Parametric` variants (the `Ac` stub already exists) | sim-core → shell, ai |
| nonlinear `Device` trait (`ee-sim`, internal) | load/stamp + Jacobian contribution per NR iteration | sim-core only |
| `Command` AST (`ee-shell`) | add `set <ref> <param> <value>`, instrument + script commands | ui-shell → ai |
| `Transport` trait (`ee-ai`) | new; `send(ModelRequest)->ModelResponse`, native/wasm/mock impls | ai-fmt only |
| `McuCore` trait + pin↔net map (`ee-mcu`) | define the shape now (co-sim wiring is M2) | boards → `ee` co-sim loop |

**Do early, before the dependents need them:** `SignalData::Complex` and the `Analysis` variants (small, unblock AC + sweeps + the AI's run requests). I recommend landing these as a tiny first sim-core change so other tracks compile against the final shape.

---

## 4. Proposed fan-out waves

One agent per track per wave, each in its own worktree (`track/sim-core`, `track/boards`, `track/ui-shell`, `track/ai-fmt`) — the same low-conflict pattern that just worked for M0. Gates are the ledger's.

### Wave 1 — retire the risk, start the long poles
| Agent | Worktree | Checkpoint | Why first |
|---|---|---|---|
| sim | `track/sim-core` | **CP-SIM-M1.1** NR + diode (+ the `Signal`/`Analysis` contract bump) | the keystone risk |
| boards | `track/boards` | **CP-BRD-M1.1** event-driven digital kernel | long pole, fully independent |
| ui-shell | `track/ui-shell` | **CP-UX-M1.1** oscilloscope (multi-ch, trigger, cursors) | builds on Plot/Signal we have |
| ai-fmt | `track/ai-fmt` | **CP-AF-M1.1** SPICE netlist import/export | self-contained, reuses netlist infra |

*Checkpoint before Wave 2:* confirm NR converges on the diode and that the contract bump is merged — everything downstream keys off it.

### Wave 2 — build on the proven base
| Agent | Checkpoint |
|---|---|
| sim | **CP-SIM-M1.4** AC (complex MNA → Bode) — parallel-safe once `Complex` exists |
| boards | **CP-BRD-M1.2** logic primitives → then start **CP-BRD-M1.3** AVR core |
| ui-shell | **CP-UX-M1.2** DMM + function generator + PSU panels |
| ai-fmt | **CP-AF-M1.2** Claude client over the mock `Transport` |

### Wave 3 — complete the M1 demo surface
| Agent | Checkpoint |
|---|---|
| sim | **CP-SIM-M1.2** BJT/MOSFET → **M1.3** convergence aids → **M1.5** sweeps → **M1.6** adaptive step |
| boards | **CP-BRD-M1.4** peripherals → **CP-BRD-M1.5** Uno + `.hex` (blink) |
| ui-shell | **CP-UX-M1.3** live overlay, **M1.5** scripting; **M1.4** dockable/mouse last |
| ai-fmt | **CP-AF-M1.3** AI tool layer — grounded "ask" |

Waves are a dependency ordering, not a schedule: within a track, an agent runs its checkpoints in sequence; across tracks they run concurrently. The AVR line (BRD-M1.3→M1.5) is the longest and may span all three waves on its own.

---

## 5. Risks & how each is handled

- **NR convergence (highest):** gmin stepping + source stepping + junction-voltage limiting (per [`track-sim-core.md`](track-sim-core.md) §Nonlinear). Keep a growing corpus of known-stiff netlists as regression gates (`CP-SIM-M1.3`). Surface *which node/device stalled* in plain language — a differentiator, not just an error string.
- **AVR accuracy (deep well):** instruction-accurate first; cycle-accuracy only where it matters (timers/PWM). Validate against existing AVR test-vector suites (`CP-BRD-M1.3` gate).
- **AC needs complex numbers:** add a minimal `SignalData::Complex` to `ee-core`; the compute core stays dependency-free (hand-rolled complex or a tiny inline type — **no `num-complex` dep** to preserve the wasm/zero-dep rule). Decide at `CP-SIM-M1.4`.
- **Function generator ↔ time-varying sources:** the funcgen panel (`CP-UX-M1.2`) needs sources whose value varies with time (sine/pulse). That's the M0 follow-up we deferred. Land a small "source waveform" addition in sim-core as a prerequisite — fold it into Wave 2 sim work.
- **AI determinism & secrets:** no network in any test — the mock `Transport` returns canned tool-call sequences ([`track-ai-fmt.md`](track-ai-fmt.md)). API key is read from env/keyring **at runtime**, never committed; `.env` stays gitignored. The AI can only do what a user could (emits the `Command` AST, reads `Signal`s).

---

## 6. Definition of done for M1

The repo stays green (`cargo test --workspace`, clippy `-D warnings`, fmt, wasm core) and the roadmap demo runs:
1. Build a transistor amplifier via `eesh`; **DC op-point** biases it (NR).
2. **AC sweep** renders a Bode plot with the expected mid-band gain and roll-off.
3. Load `blink.hex` onto a simulated **Uno**; D13 toggles at ~1 Hz.
4. Ask the copilot *"what's the gain at 1 kHz?"* and get an answer **grounded in a real AC read** (stub model in tests; real model at runtime).

---

## 7. Decisions needed before Wave 1 fan-out

1. **First MCU = ATmega328P / Arduino Uno?** (the ledger's default, and the best-documented AVR case). Confirm or redirect.
2. **Land the `SignalData::Complex` + `Analysis` variant bump first** as a tiny standalone sim-core change, so AC/sweeps/AI compile against the final contract? (Recommended.)
3. **Fan-out breadth for Wave 1:** all four track agents at once (recommended — three are NR-independent), or sim-core's NR keystone solo first, then the rest?
4. **Default Claude model** for `ee-ai` (configurable; key is runtime-only) — pick when we reach `CP-AF-M1.2`, or pin a default now.
