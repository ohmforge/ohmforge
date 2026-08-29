# Development Program Plan

This directory turns the vision in [`../ROADMAP.md`](../ROADMAP.md) into **spread-out, resumable work** that several people (or sessions, or worktrees) can push forward in parallel without colliding. It answers three questions:

1. **How is the work split?** → parallel **tracks** (below), each owning a cluster of crates.
2. **How do we know a piece is done?** → **checkpoints** with verifiable acceptance gates → [`CHECKPOINTS.md`](CHECKPOINTS.md).
3. **What exactly are we building in each?** → a **design doc per track** → `track-*.md`.

> **North star:** at every checkpoint the repo is green (`cargo test --workspace`), and each checkpoint's acceptance gate is a *command or test that proves it*, not a feeling.

---

## Tracks

Each track owns a set of crates so day-to-day edits rarely touch another track's files — that's what makes the **worktree-per-track** workflow low-conflict. Cross-track needs go through the **contracts** below, not by editing another track's internals.

| Track | Slug | Owns crates | Worktree branch | Design doc | Depends on |
|---|---|---|---|---|---|
| **Simulation core** | `sim-core` | `ee-core`, `ee-sim` | `track/sim-core` | [`track-sim-core.md`](track-sim-core.md) | — (foundation) |
| **Boards & digital** | `boards` | `ee-digital`, `ee-mcu`, `ee-devices` | `track/boards` | [`track-boards.md`](track-boards.md) | `sim-core` |
| **UI & shell** | `ui-shell` | `ee-tui`, `ee-shell`, `ee-instruments` | `track/ui-shell` | [`track-ui-shell.md`](track-ui-shell.md) | `sim-core` |
| **AI & formats** | `ai-fmt` | `ee-ai`, `ee-fmt` | `track/ai-fmt` | [`track-ai-fmt.md`](track-ai-fmt.md) | `sim-core`, `ui-shell` (eesh AST) |
| **Platform** | `platform` | *(cross-cutting)* | `main` | this doc | all |

`platform` (CI, WASM, benches, release) lives on `main` because it touches every crate; it isn't a worktree.

### Dependency DAG

```
                    ┌───────────────┐
                    │   sim-core    │   ee-core (shared types) + ee-sim
                    └───┬───────┬───┘
             ┌──────────┘       └──────────┐
             ▼                             ▼
      ┌────────────┐                ┌────────────┐
      │  ui-shell  │                │   boards   │
      │ tui/shell/ │                │ digital/   │
      │ instruments│                │ mcu/devices│
      └──────┬─────┘                └─────┬──────┘
             │        eesh AST            │
             └──────────┐   ┌─────────────┘
                        ▼   ▼
                    ┌────────────┐
                    │   ai-fmt   │   copilot emits eesh, verifies via sim
                    └────────────┘
```

**Critical path:** `ee-core` → `ee-sim` (linear MNA + transient) → everything else can proceed against real signals. Until then, `ui-shell` develops against a stub `Signal` producer and `boards` against `ee-core` alone.

---

## Checkpoint conventions

- **ID:** `CP-<TRACK>-M<milestone>.<n>` — e.g. `CP-SIM-M0.3`. Track codes: `SIM`, `BRD`, `UX`, `AF`, `PLAT`.
- **Acceptance gate:** every checkpoint names a concrete way to verify it — a passing test, a shell/`cargo` command, or a golden-file comparison. If it can't be verified, it isn't a checkpoint yet.
- **States** (tracked in [`CHECKPOINTS.md`](CHECKPOINTS.md)): `[ ]` todo · `[~]` in progress · `[x]` done (gate passing) · `[!]` blocked (note the blocker).
- **Granularity:** a checkpoint is ~0.5–2 days of focused work and leaves the workspace green. Bigger than that → split it.
- **Determinism:** acceptance gates must be reproducible (fixed seeds, golden files). A flaky gate is a bug in the gate.

---

## Inter-track contracts

These are the **only** surfaces tracks share. Changing one is a cross-track event: open it as its own checkpoint, update the contract note here, and ping the affected tracks.

| Contract | Defined in | Produced by | Consumed by | Notes |
|---|---|---|---|---|
| `Circuit` mutation API (`add_part`, `wire`, `set_param`, iteration for stamping) | `ee-core::graph` | shell, tui, ai | sim, digital, mcu | The single source of truth; everyone mutates the same graph. |
| `Signal` (typed time/freq series) | `ee-core` *(promote here so consumers needn't depend on `ee-sim`)* | sim, mcu | instruments, tui, ai, fmt | Carries `Unit`; the universal analysis result. |
| `Analysis` request enum | `ee-sim` | shell, ai | sim | What to run (op/transient/AC/sweep). |
| `McuCore` trait + pin↔net mapping | `ee-mcu` | boards | co-sim loop in `ee` | Bridges MCU clock to sim time. |
| `Command` AST (eesh) | `ee-shell` | shell parser, ai | `ee` dispatcher | The AI emits these so "type it" == "ask for it". |
| Native project schema | `ee-fmt` | fmt | all (load/save) | Human-readable, diff-friendly, versioned. |

**Design rule that falls out of this (finalized in the architecture review):** `Signal` and `Unit` live in `ee-core` (not `ee-sim`) so instruments/tui/ai depend only on the tiny core, keeping the dependency DAG acyclic and the WASM surface small. This and the other project-wide conventions (error handling, panic strategy, determinism, WASM-safe AI transport, engine access) are pinned in [`../ARCHITECTURE.md`](../ARCHITECTURE.md#cross-cutting-design-decisions) — changing any of them is a cross-track event.

---

## Resume protocol (how to pick up cold)

Any session — a new worktree, a new day, a new contributor — resumes work like this:

1. `cargo test --workspace` — confirm the baseline is green.
2. Open [`CHECKPOINTS.md`](CHECKPOINTS.md); find the track's first `[ ]`/`[~]`/`[!]` checkpoint.
3. Read that track's `track-*.md` design doc section for the checkpoint (data structures, algorithm, interface).
4. Implement to the checkpoint's **acceptance gate**; add the gate as a test if it isn't one yet.
5. Flip the ledger entry to `[x]`, commit (`<track>: CP-XXX-Mn.k <summary>`), and record any contract change here.

Each `track-*.md` ends with a **"State & next action"** block that names the current checkpoint and the immediate next step, so no session has to reverse-engineer where things stand.

---

## Milestone alignment

Tracks advance through the same milestones as [`../ROADMAP.md`](../ROADMAP.md); the roadmap is *when features ship*, the checkpoints are *the steps to get there*.

- **M0** — `SIM` (core graph, linear MNA, transient) + `UX` (eesh, schematic, braille scope) + `AF` (native save/load). First "type a circuit, see a wave."
- **M1** — `SIM` nonlinear + AC/sweep; `BRD` digital + AVR/Uno; `UX` instruments + live overlay; `AF` SPICE io + Claude "ask".
- **M2** — `BRD` co-sim + Pico; `SIM` noise/Monte-Carlo; `UX` logic analyzer + breadboard; `AF` verification loop + design/debug/firmware; `PLAT` WASM.
- **M3+** — breadth: patcher, custom boards, HDL-lite, lessons, offline AI (see roadmap).

---

## Cross-cutting (owned by `platform`, enforced everywhere)

- **Performance:** criterion benches gate the hot paths (`stamp/solve`, event queue, co-sim step). A regression fails CI.
- **Determinism & repro:** golden-file sim tests; fixed-seed Monte-Carlo; no wall-clock/`rand` in the compute core.
- **Portability:** Linux/macOS/Windows + `aarch64` + `wasm32` stay green.
- **Docs discipline:** every checkpoint that adds a feature also adds an example project and a `help` entry.

---

## Index of design docs

- [`track-sim-core.md`](track-sim-core.md) — MNA formulation, companion models, solver, analyses, `Signal`.
- [`track-boards.md`](track-boards.md) — event-driven digital kernel, AVR/Cortex-M cores, peripherals, co-sim bridge, board manifests.
- [`track-ui-shell.md`](track-ui-shell.md) — eesh grammar & dispatch, TUI layout, schematic + braille rendering, instruments.
- [`track-ai-fmt.md`](track-ai-fmt.md) — Claude client, tool layer, verification loop, native/SPICE/KiCad/VCD formats.

*(The four `track-*.md` design docs are authored on top of the reviewed/corrected architecture — see the scaffold-hardening review — and each maps 1:1 to a worktree branch.)*
