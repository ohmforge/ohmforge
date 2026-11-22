# M10 proposals — `ohmforge-core` + `ohmforge-sim`

Principal-engineer review of the dependency-free, wasm-clean, deterministic compute
core (`crates/ohmforge-core`) and SPICE-class engine (`crates/ohmforge-sim`), at
v0.8.1 (M0–M9 complete). Every item below is grounded in the code as it exists
today and respects the standing rules: core/sim stay **dependency-free, wasm32-clean,
deterministic**; the **golden/determinism suite stays bit-for-bit**; capability is
added through **circuit-level relations** (the `couple`/`add_line`/`add_switch`
pattern) and additive analyses, never new data-carrying `PartKind` variants.

---

## What is already strong — do not touch

- **The real sparse / KLU-class solver** (`mna.rs`). The dense→sparse→ordered
  dispatch (`SPARSE_THRESHOLD = 64`, `KLU_THRESHOLD = 256`, `mna.rs:37,56`), the
  min-degree fill-reducing ordering (`min_degree_order`, `mna.rs:824`), the reusable
  `Symbolic`/`Numeric`/`ReusableSolver` with pivot-sequence reuse and transparent
  re-analysis on a collapsed pivot (`mna.rs:524-802`) are carefully engineered and
  *proven bit-for-bit against the dense path* (`sparse_matches_dense_bit_for_bit`,
  `mna.rs:980`). The threshold argument that keeps every golden circuit on the exact
  natural-order path (`mna.rs:42-56`) is exactly right. Leave this alone.
- **Newton robustness ladder**: `pnjlim` junction limiting (`lib.rs:872`), gmin
  stepping, source stepping, residual-GMIN fallback, and the plain→gmin→source
  cascade in `solve_aided` (`lib.rs:1319`) are SPICE-grade and well-reasoned.
- **`.TF` already extracts zeros.** `transmission_zeros` + `finite_poles`
  (`lib.rs:3388,3413`) and the `det(G + sC) = 0` pencil (`assemble_pencil`,
  `lib.rs:3272`) mean transfer-function *zeros* are a solved problem, not a gap.
- **Determinism discipline**: in-tree LCG / SplitMix64 (`mna.rs:941`,
  `lib.rs:3662`), the backing/threshold design, and the careful
  "accumulate-then-subtract" back-substitution ordering (`mna.rs:418-437`) are
  exemplary. Any new path must inherit this discipline, not erode it.

---

## Ranked proposals

### 1. Reusable ordered **complex** solver for AC and noise sweeps  *(performance; top pick)*

**What / why.** `mna_complex.rs` is a dense `O(N³)` Gaussian elimination and says so
outright: *"A sparse path is future work, as in `mna`"* (`mna_complex.rs:14`;
`ComplexSystem::solve`, `mna_complex.rs:104-154`). The AC driver re-assembles a fresh
`ComplexSystem` and **re-factors it from scratch at every frequency** (`ac`,
`lib.rs:2810-2824`: `net.assemble(w)` then `sys.solve()` inside the `for &f in &freqs`
loop); `noise` sweeps frequencies the same way (`lib.rs:3503+`). Yet the matrix
*structure* is identical at every frequency — only the `jωC` / `1/jωL` numbers change
(`assemble`, `lib.rs:2735-2774`). This is precisely the repeated-structure situation
the real side already solves: analyze the fill-reducing ordering **once**, then
`refactor` + back-solve per frequency. A log sweep is routinely 100–1000 points, so
on a mid/large network this turns `points × O(N³)` into `O(N³)` once + `points ×
O(nnz)`.

The design is a direct port of the proven real path: a `ComplexSparseMatrix`
mirroring `SparseMatrix`'s stamp API, a complex `Symbolic`/`Numeric` reusing the
*same* `min_degree_order` (the ordering is purely structural — it already works
unchanged), and pivoting on `Complex::magnitude()` exactly as the dense complex
solver already does (`mna_complex.rs:113`). Gate it by size with a complex analogue
of `KLU_THRESHOLD` so small circuits keep the exact dense path.

**Effort:** M. **Risk:** Low. The determinism suite's AC/noise fixtures (`rc_ac`,
`determinism.rs:86`) are tiny and stay on the dense path below the threshold, so they
do not move a bit; AC/noise golden checks are tolerance-based
(`determinism.rs:331-367`), and the ordered complex LU matches dense to a few ULP
(the real `ordered_matches_dense` holds ≤1e-12). Stays dependency-free and wasm-clean
(reuses `ohmforge_core::Complex`). **Cross-crate:** none — entirely within
`ohmforge-sim`.

**Acceptance gate.** (a) A new `ac_ordered_matches_dense` test: AC magnitude/phase of
a mid-size RC/RLC ladder equals the dense-path result to ≤1e-12 at every swept
frequency. (b) A `bench_ac_scaling` (Criterion, in `benches/solve.rs`) showing the
per-frequency cost flattening from cubic toward near-linear as `N` grows. (c) The
existing `ac_deterministic_and_golden` and `noise_deterministic_and_golden` tests
still pass unchanged.

---

### 2. Second-order integration (trapezoidal / Gear-2) as an **opt-in** transient method  *(accuracy / device fidelity)*

**What / why.** Transient integration is **Backward Euler only** — first order. The
step companions are `g_eq = C/h` for capacitors (`lib.rs:2116`) and `h/L` for
inductors (`lib.rs:2126`), and the adaptive driver's own comment states *"Backward
Euler is first order, that difference is the finer solution's LTE"*
(`lib.rs:1527-1532`). BE is unconditionally stable but **heavily, artificially
damped**: it smears LC ringing, under-predicts overshoot, and loses amplitude on
oscillators and resonant / switching circuits — the single most visible accuracy gap
versus a production SPICE, which defaults to trapezoidal. Adding an optional
second-order companion (trapezoidal, or TR-BDF2 / Gear-2 to avoid the trapezoidal
"ringing" artefact on hard switching) closes it. Trapezoidal keeps the same MNA
assembly but the companion carries the previous step's *branch current/derivative*
(e.g. capacitor `g_eq = 2C/h`, history source `2C/h·v_prev + i_prev`), a localized
change to the cap/inductor stamping in `TransientModel::step` (`lib.rs:2096-2252`)
plus the reactive-state update.

**Effort:** M (trapezoidal) → L (TR-BDF2 with its own LTE control). **Risk:
HIGH to the golden suite if it becomes the default** — it *will* move every
fixed-step transient result. This must ship as an **opt-in** selector (a new
`method` field on `Analysis::Transient`, or a parallel `Analysis` variant) with
**Backward Euler remaining the default**, so `transient_deterministic_and_golden`
(`determinism.rs:285`) stays bit-for-bit. This is the one item on the list whose
whole safety rests on *not* changing the default path. Deterministic and wasm-clean
(pure arithmetic). **Cross-crate:** `Analysis` lives in `sim` (`lib.rs:37`), so the
API addition is sim-local; it is an analysis option, not a `PartKind`, so it honors
the standing rule.

**Acceptance gate.** (a) A lossless LC tank: trapezoidal conserves energy / sustains
the oscillation where BE visibly decays — assert the trapezoidal amplitude at
`t = duration` stays within a few percent of the initial, and that BE (unchanged)
still decays. (b) An analytic RC step compared to BE and trapezoidal, asserting
trapezoidal's error shrinks as `O(h²)` vs BE's `O(h)` under step refinement. (c) The
entire existing fixed- and adaptive-step golden transient suite passes unchanged
(proving BE is still the default, bit-for-bit).

---

### 3. DC / AC **sensitivity** analysis (`.SENS`) by the adjoint method  *(new capability)*

**What / why.** The engine already carries everything a sensitivity analysis needs
and exposes none of it. Per-component tolerance metadata exists in core
(`Circuit::set_tolerance`, used at `determinism.rs:137`) and already drives
Monte-Carlo; the reusable factorization retains the LU factors
(`Numeric`, `mna.rs:677-739`). Adjoint sensitivity of an output to every parameter
costs **one extra transpose-solve** that reuses the *already-computed* factorization:
`∂out/∂pₖ = −λᵀ (∂A/∂pₖ) x`, where `λ` solves `Aᵀλ = e_out`. For the DC/linear case
each `∂A/∂pₖ` is a trivial rank-structured stamp (a resistor's is `±1/R²` at four
entries), so the whole sensitivity vector is one back-solve plus `O(parts)` cheap
dot products — far cheaper than the finite-difference "bump each part and re-solve"
a user would otherwise script. This is what EDA users need for worst-case / tolerance
/ design-centering work, and it directly leverages the tolerance data already in the
model.

Implementation adds a transpose-solve to `Numeric` (the factors are present; only the
forward/back order flips) and a new additive `Analysis::Sensitivity { output }`
returning one row per parameter. No new `PartKind`; it reads existing part values and
tolerances.

**Effort:** M. **Risk:** Low — purely additive (a new analysis + a new `Numeric`
method), touches no existing path, so the golden suite cannot move. Deterministic and
wasm-clean. **Cross-crate:** new `Analysis` variant in `sim`; may want a small
accessor on `core`'s `Circuit` to enumerate parameterizable parts (read-only, no new
variant).

**Acceptance gate.** A resistive divider's `∂v(mid)/∂R1` and `∂v(mid)/∂R2` from the
adjoint match both the closed-form derivative and a central finite-difference to
≤1e-6; a multi-node ladder's full sensitivity vector matches finite-difference to
≤1e-6 while performing exactly **one** factorization (asserted via a solve counter in
`bench_support`).

---

### 4. Eigenvalue **balancing** before the Hessenberg/QR reduction  *(numerical robustness)*

**What / why.** `eig.rs` says plainly *"no balancing"* (`eig.rs:17-18`): `eigenvalues`
goes straight from the raw matrix into `elmhes` + `hqr` (`eig.rs:139-140`). The
pole/zero and `.TF` analyses build `G`/`C` pencils whose rows span many orders of
magnitude (a circuit mixing pF reactances with MΩ resistances, or kHz with GHz time
constants), and an **unbalanced** nonsymmetric eigensolver loses precision on exactly
such badly-scaled matrices — the classic failure Parlett–Reinsch balancing (`balanc`)
was designed to fix. It is a short, well-known diagonal similarity scaling inserted
before `elmhes`; it changes no external dependency, is pure deterministic arithmetic,
and is the natural companion to the existing hand-rolled Numerical-Recipes kernels.
Because the pole/zero system is bounded (`PZ_MAX_SYSTEM = 128`, `lib.rs:2944`) the
cost is negligible.

**Effort:** S. **Risk:** Low. Pole/zero golden checks are tolerance-based, and
balancing *improves* accuracy; it will shift ill-scaled eigenvalues toward their true
values (by design) and leave well-scaled ones essentially unchanged. Confirm no
pole/zero test is pinned bit-for-bit on a badly-scaled fixture before landing (the
determinism suite does not currently exercise `.TF`). Deterministic, wasm-clean.
**Cross-crate:** none.

**Acceptance gate.** A deliberately ill-scaled 2nd/3rd-order fixture (poles separated
by ~1e9 in magnitude) whose analytic poles the *current* code resolves to only a few
digits; after balancing, assert recovery to ≤1e-9 relative. All existing `eig.rs`
spectrum tests (`eig.rs:407-493`) still pass.

---

### 5. Allocation reduction in the Newton / transient hot loop  *(performance / memory)*

**What / why.** `solve_nr` allocates a brand-new `System::new(size)` on **every Newton
iteration** (`lib.rs:1235`), and for a nonlinear transient that is
`steps × iterations` full matrix rebuilds. On the sparse backing each rebuild
reallocates a `Vec<BTreeMap<usize, f64>>` (one `BTreeMap` per row, `mna.rs:103`) and
re-inserts every stamp; on top of that each iteration rebuilds the per-device
`nodes` / `v` / `i_now` vectors and re-resolves `layout.node_of(circuit, t)` for every
terminal of every device (`lib.rs:1246-1252`), none of which change across iterations.
The structure is invariant within a solve, so the system can be assembled once and
**cleared-and-restamped** (keeping the allocated row maps / dense buffer and the same
key order), and device node indices can be hoisted out of the iteration loop. This
cuts allocator traffic and BTreeMap churn in the engine's single hottest path —
exactly the "memory/alloc reduction in the hot loop" lever — benefiting every
nonlinear op-point, sweep point, and transient step.

**Effort:** M. **Risk:** Medium, and entirely about staying bit-for-bit. Clearing a
`BTreeMap`-backed row preserves ascending key order, so restamping accumulates in the
identical order and reproduces the same floating-point sums — but this must be
asserted, because the whole value of the engine is its bit-exact determinism. Keep the
dense backing's zero-and-restamp equally order-preserving. No API change, no
dependency, wasm-clean. **Cross-crate:** none.

**Acceptance gate.** (a) `transient_deterministic_and_golden` and the full
`full_golden_suite_runs_twice_identically` (`determinism.rs:228`) pass **bit-for-bit**
— this is the hard gate. (b) `transient/rc_1000` and `transient/rlc_500`
(`benches/solve.rs:180,189`) show a measurable allocation / wall-time drop; optionally
a `#[global_allocator]` counting harness in a test asserts allocations per step fall
to ~`O(1)` from `O(iterations)`.

---

### 6. MOSFET (and general nonlinear) Newton **step limiting**  *(convergence)*

**What / why.** The level-1 MOSFET declares `state_len() = 0` and does **no limiting
at all** (`lib.rs:1061-1078`); `mos_chan` evaluates the square law at the raw Newton
voltages (`lib.rs:1114`). The diode and BJT damp their junctions through `pnjlim`, but
the MOSFET's gate/drain steps are unbounded, so on a stiff MOS stage a single large
Newton step can overshoot across the triode/saturation knee and stall the iteration —
pushing the solve onto the (slower) gmin/source-stepping fallback or to
`NonConvergence`. Adding SPICE-style `fetlim` / `limvds` step limiting (bounding the
per-iteration change in `Vgs`/`Vds`, carried in a now-nonzero `state`) tightens
convergence on hard MOS circuits with no change to the converged operating point.

**Effort:** S–M. **Risk:** Medium. Limiting changes the Newton *trajectory*, not the
converged point (to tolerance), but it can therefore change the final bits *within*
tolerance on a circuit that currently converges — so any MOSFET fixture pinned in the
golden suite must be checked (the current determinism suite has no MOSFET transient
golden, which lowers the risk). Deterministic, wasm-clean. **Cross-crate:** none —
contained in the `Mosfet` model.

**Acceptance gate.** A stiff common-source / current-mirror MOS stage that currently
requires the stepping fallback (or hits `NR_MAX_ITERS`, `lib.rs:731`) converges under
plain Newton in markedly fewer iterations after limiting; assert the final operating
point matches the pre-change solution to ≤`NR_V_TOL`/`NR_I_TOL`
(`lib.rs:734,737`), and that every existing nonlinear golden result is unchanged.

---

## Summary table

| # | Item | Axis | Effort | Determinism risk |
|---|------|------|--------|------------------|
| 1 | Reusable ordered complex solver (AC/noise) | Performance | M | Low (size-gated; tolerance golden) |
| 2 | Trapezoidal / Gear-2 integration (opt-in) | Accuracy | M–L | **High unless opt-in; BE stays default** |
| 3 | Adjoint sensitivity analysis (`.SENS`) | Capability | M | Low (purely additive) |
| 4 | Eigenvalue balancing before `elmhes`/`hqr` | Robustness | S | Low (improves accuracy; `.TF` not bit-pinned) |
| 5 | Hot-loop allocation reduction | Perf / memory | M | Medium (must prove bit-for-bit) |
| 6 | MOSFET Newton step limiting | Convergence | S–M | Medium (trajectory change, within tol) |

All six stay dependency-free, wasm32-clean, and deterministic, and none introduces a
data-carrying `PartKind` variant: #2/#3 extend the `Analysis` enum (sim-local), the
rest are internal to the solver/eig kernels.
