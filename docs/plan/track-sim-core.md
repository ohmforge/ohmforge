# Track design: **sim-core** (`ee-core`, `ee-sim`)

> The foundation. Everything else runs against the types in `ee-core` and the signals produced by `ee-sim`. Get this right and the other tracks have real data to build on.
>
> Checkpoints: `CP-SIM-M0.1 … CP-SIM-M2.4` in [`CHECKPOINTS.md`](CHECKPOINTS.md). Contracts: [`README.md`](README.md#inter-track-contracts). Rationale for the cross-cutting rules: [`../ARCHITECTURE.md`](../ARCHITECTURE.md#cross-cutting-design-decisions).

## Scope

| Crate | Owns |
|---|---|
| `ee-core` | The shared data model — **no solver, no I/O**: circuit graph, `Unit`/`Value`, simulation time, event queue, and the `Signal` result type. This is the crate every other track depends on. |
| `ee-sim` | The analog/mixed-signal engine: MNA assembly, linear solve, Newton–Raphson, numerical integration, and the `Analysis` requests (op/transient/DC-sweep/AC/noise/Monte-Carlo/parametric). |

**Hard rule:** `ee-core` and `ee-sim` take no clock, no filesystem, no network, and no unseeded randomness. They must stay `wasm32-unknown-unknown`-clean (`just wasm`). This is what makes golden-file determinism and the browser build possible.

---

## `ee-core` data model

### The circuit graph (`graph.rs`) — the single source of truth

Already scaffolded: `Circuit` owns `Vec<Part>` and allocates `NetId`/`PartId`. Net 0 is `NetId::GROUND`; `Circuit::default()` reserves it (`next_net = 1`) — a regression test guards this.

```rust
pub struct NetId(pub u32);          // 0 = GROUND
pub struct PartId(pub u32);
pub enum  PartKind { Resistor, Capacitor, Inductor, VoltageSource, Other(String) }
pub struct Part { id, reference, kind, value: Option<Value>, terminals: Vec<NetId> }
pub struct Circuit { name, parts, next_net, next_part }
```

**To build at `CP-SIM-M0.1`:**
- `wire(a, b)` — merge two nets into one electrically-common node. Use a **union-find** over net ids so `wire` is near-O(1) and merges are order-independent; expose a `resolve(NetId) -> NetId` the solver calls when stamping. Keep the raw ids stable for the UI; resolve only at stamp time.
- `part_by_ref(&str)`, `remove_part(PartId)`, `set_param(PartId, Value)`.
- An **iteration API for stamping**: `parts()` (exists) plus a `node_count()` / dense-index mapping so the solver can turn resolved nets into matrix rows without knowing graph internals.
- *Gate:* `cargo test -p ee-core graph::` covers a net-merge and a name lookup.

### Units & values (`units.rs`, `value.rs`) — **done** (`CP-SIM-M0.2`)

`Value { magnitude: f64, unit: Unit }` with the engineering-notation parser (`4k7`, `2n2`, `1meg`, `3.3V`, `20mA`). Signed values and lone-prefix rejection are handled and tested. `Unit` is the typed quantity (Volt/Ampere/Ohm/Farad/Henry/Hertz/Second/Watt/…). New units get added here as devices need them.

### Time & events (`time.rs`) — for the digital/co-sim tracks

`Seconds(f64)` newtype for sim time, and an `Event`/event-queue primitive (a binary heap keyed by time) that `ee-digital` and the co-sim loop reuse. Lives in core so both the analog and digital sides share one clock type.

### `Signal` — **the universal result type** (contract; promote to `ee-core` at `CP-SIM-M0.5`)

Every analysis emits `Signal`s; every consumer (instruments, TUI, AI, fmt) reads them **without depending on `ee-sim`**. That's why it lives in `ee-core`.

```rust
pub struct Signal {
    pub name: String,            // "v(out)", "i(R1)"
    pub unit: Unit,
    pub domain: Domain,          // Time(Seconds) | Freq(Hz)
    pub x: Vec<f64>,             // sample points (t or f)
    pub y: SignalData,           // Real(Vec<f64>) | Complex(Vec<Complex>)  (AC is complex)
}
```

Design notes: keep it a plain owned buffer (no borrows into the solver) so it can cross the snapshot channel to the TUI and be serialized by `ee-fmt`. Streaming variants (an iterator/callback for long transients) come later; the M0 shape is a finished buffer.

---

## `ee-sim` engine

### MNA assembly (`CP-SIM-M0.3`)

Build `A·x = z` where `x` = unknown node voltages plus the branch currents MNA adds for voltage sources/inductors. Each device **stamps** its contribution:

- Resistor between resolved nets `a,b`: conductance `g = 1/R` added to `A[a][a], A[b][b]` (+g) and `A[a][b], A[b][a]` (−g).
- Independent voltage source: adds a branch-current unknown + two ±1 entries (the "modified" part of MNA) and its value into `z`.
- Ground row/column is dropped (net 0 is the reference).

Represent `A` sparsely from the start (COO→CSR, or lean on `faer`), but a dense path is fine for the first golden test.
*Gate:* a resistive divider solves to 1e-9 (`rc_divider_op`).

### Companion models + transient (`CP-SIM-M0.4`)

Discretize reactive devices each timestep into a **companion model** (a conductance in parallel with a current source) via the integration rule:

- **Backward Euler** (start here — robust): capacitor → `g = C/h`, `I_eq = g·v(t−h)`.
- Trapezoidal (more accurate, add later) and Gear-2 for stiff circuits.

Transient loop: for each step `h`, refresh companions from the previous solution, assemble, solve, store into `Signal`s.
*Gate:* RC step matches `1−e^(−t/RC)` within 1% (`rc_step_transient`).

### Nonlinear + Newton–Raphson (`CP-SIM-M1.1`–`M1.3`)

Nonlinear devices (diode/LED, BJT, MOSFET) linearize at the operating point; iterate Newton–Raphson, re-stamping each iteration until `Δx` and residual are within tolerance. Convergence aids: **gmin stepping**, **source stepping**, **junction voltage limiting**. Surface non-convergence in plain language (which node/device stalled) — that's a product differentiator, not just an error.

### Frequency & statistical (`CP-SIM-M1.4`, `M2.1`–`M2.3`)

- **AC:** complex MNA at each frequency point → complex `Signal` (magnitude/phase). *Gate:* RC low-pass hits −3 dB at `fc`, −45° phase.
- **Noise:** integrate device noise sources → output spectral density; resistor floor matches `4kTR`.
- **Monte-Carlo:** perturb component values within tolerances using a **seeded** RNG so a fixed seed reproduces bit-for-bit.
- **Controlled/behavioral sources:** VCVS/VCCS/CCVS/CCCS stamps; behavioral via expression evaluation.

### Solver & scale (`CP-SIM-M2.4`)

Dependable dense/`faer`-backed LU with partial pivoting first; move hot/large circuits to a true sparse (KLU-style) path. Adaptive timestep (LTE-controlled) bounds step count while holding accuracy. Criterion benches gate `stamp`/`solve` (CP-PLAT.4).

---

## The `Analysis` request contract

The shell and AI never call the solver directly — they submit an `Analysis` and get `Signal`s back (dispatched by the `ee` binary). Defining this enum is part of `CP-SIM-M0.5`.

```rust
pub enum Analysis {
    OperatingPoint,
    Transient { stop: Seconds, step: Option<Seconds> },
    DcSweep   { source: PartId, start: f64, stop: f64, step: f64 },
    Ac        { f_start: f64, f_stop: f64, per_decade: u32 },
    Noise     { out: NetId, source: PartId, f_start: f64, f_stop: f64 },
    MonteCarlo{ runs: u32, seed: u64, inner: Box<Analysis> },
    Parametric{ target: (PartId, Param), grid: Sweep, inner: Box<Analysis> },
}
```

Adding a variant is a cross-track event (shell + AI must learn to emit it).

---

## Testing strategy

- **Golden files** for every analysis (fixed seed, checked-in reference `Signal`s); the determinism harness (CP-PLAT.5) runs them twice and diffs.
- **Property tests** on the solver (e.g. a resistor network's node voltages satisfy KCL to tolerance).
- **Fuzz** the value parser and (later) the SPICE importer.
- Keep every test `no-network`, `no-clock`.

---

## State & next action

- **Done:** `CP-SIM-M0.2` (units + engineering-notation `Value`, with signed/lone-prefix tests) and the graph scaffold (`Circuit`, ground-reservation invariant test).
- **Current checkpoint:** `CP-SIM-M0.1` — finish the graph: `wire` (union-find net merge), removal, name lookup, and the stamping-iteration API.
- **Immediate next step:** implement union-find net merge + `resolve()`, add `graph::` merge/lookup tests (the CP-SIM-M0.1 gate), then move to `CP-SIM-M0.3` (linear MNA + DC op-point) so `ui-shell` and `boards` get real signals to build against.
