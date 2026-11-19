# How the simulation engine works

This is a tour of `ohmforge-sim`, the analog / mixed-signal core of `ohmforge` — what
happens between handing it a circuit and getting back a set of waveforms. It is
written for a curious user who wants to understand the machinery, not just drive
it. Worked, runnable circuits live in [`examples/analog/`](../../examples/analog/).

The engine takes an [`ohmforge_core::Circuit`], an [`Analysis`] describing what to
measure, and returns one [`ohmforge_core::Signal`] per node (a named, unit-carrying
waveform). It is pure compute: no clock, no filesystem, no unseeded randomness,
and `wasm32`-clean — the same solver runs in the TUI, headless, and in the
browser, and the same inputs always give the same outputs.

```rust
use ohmforge_sim::{Analysis, Engine};
let signals = Engine::new().run(&circuit, &Analysis::OperatingPoint)?;
```

---

## 1. The circuit model

A `Circuit` is **parts** joined at **nets** (electrically common nodes). Net 0 is
always ground — the voltage reference. Nets are merged by a **union-find**:
`wire(a, b)` unions two net ids and `resolve(net)` maps any id to its group
representative, with ground kept as the representative of whatever it touches.
Raw ids stay stable for the UI; the solver only ever sees resolved nets.

Each part carries a kind (`R`, `C`, `L`, voltage/current source, diode, BJT,
MOSFET, the four controlled sources), an optional [`ohmforge_core::Value`] (an
engineering-notation magnitude plus a [`ohmforge_core::Unit`] — `4k7` → 4700 Ω), its
terminal nets, and, for sources, a time [`ohmforge_core::Waveform`] (DC, sine, or
pulse).

Results come back as `Signal`s — a real-valued buffer for time-domain and DC
data, a complex buffer for AC/noise — so every consumer (scope, instruments,
file layer, AI) reads results without depending on the solver.

---

## 2. Modified Nodal Analysis (MNA): stamping

Every linear analysis assembles a system `A·x = z` and solves it. The unknowns
`x` are:

1. one **node voltage** per non-ground net, then
2. one **branch current** per element that cannot be written as a conductance —
   each voltage source, each VCVS, each CCVS, and (at DC only) each inductor,
   which is a short.

That extra block of branch-current unknowns is the "modified" in Modified Nodal
Analysis. The row layout is fixed and identical across the DC, transient, and AC
systems so a device stamps the same way in all three:

```
[ node voltages | V-source branches | VCVS branches | CCVS branches | (DC: inductor shorts) ]
```

Each device **stamps** its contribution into `A` and `z`:

- **Resistor** `R` → a conductance `g = 1/R` between its two nodes: `+g` on each
  diagonal, `−g` on the two off-diagonals (`stamp_conductance`). A non-positive
  resistance is rejected as a bad value (a zero-ohm resistor is a short with
  undefined current).
- **Current source** `I` from `a` to `b` → a pure right-hand-side term: `+I` into
  node `b`, `−I` out of node `a` (no new unknown).
- **Voltage source** `V` from `+` to `−` on branch row `k` → the incidence
  pattern `A[+,k]=A[k,+]=+1`, `A[−,k]=A[k,−]=−1`, and `z[k]=V`. The branch row
  enforces `v(+) − v(−) = V`; the branch unknown is the current through the
  source.
- **Controlled sources** (`CP-SIM-M2.3`) stamp their defining relation:
  - **VCVS** (gain `E`) — a voltage-source branch whose row also couples the
    control nodes with `∓E`, enforcing `v(out) = E·v(ctrl)`. This is the ideal
    op-amp building block (see [`opamp_noninverting`](../../examples/analog/opamp_noninverting.md)).
  - **VCCS** (transconductance `G`) — four transconductance entries, no new
    unknown.
  - **CCVS** / **CCCS** — sense the current through a named voltage source's
    branch (a 0 V sense source is the idiom) and couple it into the output
    branch / nodes. A controlled source naming a control element that is not a
    current-sensing voltage source is reported as `SimError::BadControl`.

Ground (net 0) is the reference, so any stamp touching it simply drops that term.

---

## 3. The linear solve

`A·x = z` is solved by **dense Gaussian elimination with partial pivoting**
(`mna::System`). Partial pivoting is not optional here: a voltage source's branch
row has a zero on its own diagonal, so a pivot-free elimination would divide by
zero — the solver swaps in the largest-magnitude pivot in each column. If the best
pivot is below `1e-15` the matrix is singular (a floating node or no path to
ground) and the solve returns `SimError::Singular` rather than NaNs.

AC analysis uses a complex twin, `mna_complex::ComplexSystem` — the same
elimination over `ohmforge_core::Complex`, pivoting on complex magnitude. `Complex` is
hand-rolled (no `num-complex` dependency) so the compute core stays dependency-free.

A dense solve is `O(n³)`; it is correct and clear at small sizes. Above a size
threshold the engine switches automatically to a **KLU-class sparse** path — a
fill-reducing ordering plus a *reusable symbolic factorization*, so a transient
or sweep that re-solves the same topology every step pays the ordering cost once.
The sparse path is a bit-for-bit match to the dense one below the threshold, so
nothing about the answer changes with size — only the speed.

---

## 4. Nonlinear devices and Newton–Raphson

Diodes, BJTs, and MOSFETs have no linear stamp — their current is an exponential
or square-law function of terminal voltage. The engine solves them by
**Newton–Raphson**: repeatedly linearize every nonlinear device about the current
voltage guess, stamp the linearization, solve, and iterate until the step
converges.

Each device implements a small contract: given its terminal voltages, return a
**companion** — the terminal currents `I_i`, the Jacobian `g[i][j] = ∂I_i/∂V_j`,
and the equivalent-current sources for the right-hand side. The companion is the
multiport generalization of the two-terminal Norton equivalent `I_eq = g·v − I`:
stamp `g` as a (possibly asymmetric) conductance block and inject `I_eq` into each
node's RHS.

- **Diode** — Shockley `I = I_s·(e^(V/nV_t) − 1)`, `g = dI/dV`. Terminals
  `[anode, cathode]`. See [`diode`](../../examples/analog/diode.md).
- **BJT** — Ebers–Moll transport model: two junction diodes plus a transport
  source, a 3×3 Jacobian in `[collector, base, emitter]`. The [`Polarity`] folds
  PNP onto the NPN equations by flipping controlling voltages and currents. See
  [`common_emitter_bjt`](../../examples/analog/common_emitter_bjt.md).
- **MOSFET** — level-1 square law with the body tied to the source, across
  cutoff / triode / saturation with channel-length modulation, in
  `[drain, gate, source]`; PMOS folds onto NMOS by polarity.

**Junction limiting.** A raw Newton step can push a junction voltage far enough
that `e^(V/V_t)` overflows. The diode and both BJT junctions pass each step
through SPICE's `pnjlim`: past a critical voltage, a step beyond a thermal voltage
is mapped through a logarithm, so the exponential cannot run away between
iterations. Reverse bias and small steps pass through untouched.

**Convergence test.** A step is accepted only when *both* the largest node-voltage
change (`< 1e-9 V`) and the largest device-current change (`< 1e-12 A`) fall below
tolerance, and the solution is finite — a diverging solve that overflows to
`inf`/`NaN` can never read as converged. The iteration cap is 100; hitting it is
reported as `SimError::NonConvergence`.

A **purely linear circuit has no nonlinear devices**, so this path stamps and
solves exactly once — identical to the plain MNA solve, with no iteration cost.

---

## 5. Convergence aids

Some nonlinear biases are too stiff for Newton–Raphson from a cold start (every
node at 0 V) — for example a diode-connected MOSFET fed only by an ideal current
source, whose channel is cut off (and whose Jacobian row is all zeros) at the
starting guess. The engine tries plain Newton–Raphson first (the fast path), and
only if it stalls or returns a singular Jacobian does it fall back to two
continuation methods (`solve_aided`):

1. **gmin stepping** — add a conductance from every node to ground, large enough
   to make the Jacobian trivially solvable, then ramp it geometrically from
   `1e-3` S down to zero over 16 steps, warm-starting each solve from the last.
   The final step sits at gmin = 0 — the true circuit — so the answer carries no
   residual-conductance error.
2. **source stepping** — ramp every independent source from 0 % to 100 % of its
   value over 16 steps. At 0 % the circuit is dead (the solution is the origin);
   each small increment keeps the next Newton solve inside its basin of
   attraction.

A node that stays genuinely floating even at gmin = 0 still surfaces as
`SimError::Singular`; only a hopeless nonlinear solve surfaces as
`NonConvergence`. This same aided ladder backs both the DC operating point **and**
the transient's `t = 0` solve and each timestep, so a stiff bias the operating
point can crack never spuriously fails a transient.

---

## 6. Companion models for storage elements

Capacitors and inductors are integrated in time by **Backward Euler**, each
replaced at every step by a resistive **companion** that depends on the timestep
`h` and the previous state:

- **Capacitor** `C` → a conductance `g = C/h` in parallel with a history current
  source `(C/h)·v_prev`. After each solve, `v_prev` is refreshed from the node
  voltages.
- **Inductor** `L` → a conductance `g = h/L` in parallel with a history current
  source carrying `i_prev`, oriented along the element. After each solve the
  current updates as `i = i_prev + (h/L)·v`.

Backward Euler is first-order and unconditionally stable — it will not blow up on
a stiff reactive circuit, at the cost of a little numerical damping (visible as a
slightly trimmed overshoot in the [`rlc_resonator`](../../examples/analog/rlc_resonator.md)
ring). A non-positive capacitance or inductance is rejected as a bad value: it
would give a zero/negative companion conductance that silently poisons the solve.

---

## 7. The analyses

Everything above is driven by one of the [`Analysis`] variants.

### Operating point — [`Analysis::OperatingPoint`]

The steady DC solution: capacitors open, inductors shorted, nonlinear devices
driven to steady state by the aided Newton solve. Returns one single-sample
signal per node. This is the starting point for the AC and noise analyses, which
linearize about it. Examples: [`divider`](../../examples/analog/divider.md),
[`diode`](../../examples/analog/diode.md).

### Transient — [`Analysis::Transient { duration, step }`]

A time-domain run using the Backward-Euler companions. The `t = 0` sample is an
initial-condition solve — capacitors held at their initial voltage (0 V),
inductors open (`i = 0`) — then each step advances the state.

- **`step = Some(h)`** runs a fixed-step march at uniform `t = k·h`.
- **`step = None`** runs the **error-controlled adaptive** driver (`CP-SIM-M1.6`).
  Each attempt estimates the local truncation error by **step doubling** — one
  step of `h` versus two of `h/2`; for a first-order method that difference *is*
  the finer solution's error. The step is accepted when the normalized error is
  ≤ 1 and resized by `safety·err^(−1/2)` (clamped so it never grows past 5× or
  shrinks past 0.2×), within `[duration·1e-6, duration/20]`. The result is a
  non-uniform grid — fine where the solution bends, coarse where it is placid.

Examples: [`rc_step`](../../examples/analog/rc_step.md),
[`rl_step`](../../examples/analog/rl_step.md),
[`rlc_resonator`](../../examples/analog/rlc_resonator.md),
[`half_wave_rectifier`](../../examples/analog/half_wave_rectifier.md).

### AC small-signal — [`Analysis::Ac { f_start, f_stop, points_per_decade }`]

Linearize about the DC operating point, then solve a complex MNA system at each
log-spaced frequency. Reactive devices enter by their admittance — a capacitor as
`jωC`, an inductor as `1/(jωL)` — so, unlike at DC, inductors are admittances here
rather than branch shorts. Nonlinear devices contribute their small-signal
conductance at the bias. The stimulus is a `1∠0` phasor on every source marked as
an AC drive (a source carrying a sine waveform); all other sources are quiet (a
voltage source is a short, a current source an open). Returns one complex signal
per node — the node-voltage phasor versus frequency, from which magnitude (dB) and
phase follow. (Try `cargo run -p ohmforge --bin of -- bode`.)

### Noise — [`Analysis::Noise { output, f_start, f_stop, points_per_decade }`]

Small-signal noise referred to an output node. Each independent noise generator
contributes `|Z(f)|²·S` incoherently, where `S` is its intrinsic spectral density
and `Z(f) = v(output)/I` is the transimpedance from that generator to the output.
At present the modeled generator is **resistor thermal noise**: resistor `R`
carries a noise-current source of PSD `S = 4kT/R` (A²/Hz) across its terminals, so
its output contribution is `|Z|²·4kT/R` (V²/Hz). The transimpedance reuses the AC
complex network with every independent source quiet; a unit current injected
across a resistor's nodes reads `Z` straight off the output. A lone resistor to
ground therefore shows the familiar `4kTR` thermal floor. (`k = 1.380649e−23 J/K`,
`T = 300.15 K`.) The result is one frequency-domain signal of the output PSD.

### DC sweep and parametric — [`Analysis::DcSweep`], [`Analysis::Parametric`]

- **DC sweep** steps one independent source's level from `start` to `stop` by
  `step` (endpoints included) and records the operating point at each sample, with
  the swept level on the x axis. The swept source is driven as a plain DC source,
  so any waveform it carried is ignored for the sweep. The topology never changes,
  so the MNA layout is built once.
- **Parametric** repeats an *inner* analysis — any analysis, including a transient
  or AC sweep — once per value, retargeting a source's DC level each time. Every
  returned signal is tagged with the swept value (`"v(n2) @ V1=3"`) so the families
  stay distinguishable.

### Monte-Carlo — [`Analysis::MonteCarlo { seed, runs, output, sample, inner }`]

Tolerance analysis. Each part may carry a fractional tolerance (`0.05` = ±5 %);
the Monte-Carlo runner is the only consumer of it — every other analysis is
unaffected. Each run clones the nominal circuit and perturbs every toleranced part
to `v·(1 + t·u)`, where `u` is uniform in `[−1, 1)`, then runs `inner` and records
the `output` node's value at sample index `sample`.

**Determinism is the whole point.** A single in-crate SplitMix64 PRNG is seeded
from `seed` and advanced once per toleranced part, in part-index order, across the
whole run sequence. The set of toleranced parts, their order, and the draw count
are fixed by the circuit, so the same seed replays the identical samples bit for
bit on every platform; a different seed gives a different-but-reproducible draw.
Because each run perturbs from the original nominal values, draws never compound.

### Transfer function / pole-zero and S-parameters

Two further entry points are **methods on `Engine`** rather than `Analysis`
variants, because they return structured reports rather than per-node signals:

- **`Engine::transfer_function(circuit, output, source)`** linearizes about the
  operating point and returns a SPICE-`.TF`-style report: the DC gain from the
  named source to the output node, the circuit's **poles** (the roots of
  `det(G + sC) = 0`, intrinsic to the circuit), and — where tractable — the
  **transmission zeros** of the input→output path. In the shell: `run tf <node> <src>`.
- **`Engine::s_parameters(circuit, port1, port2, z0, freq)`** computes the 2-port
  **scattering parameters** at a reference impedance `z0` and frequency, and the
  equivalent open-circuit `z` matrix. S11 feeds the Smith-chart panel. In the
  shell: `run sparams <p1> <p2> [z0] [freq]`.

## 8. Transformers and transmission lines

Two structural relations extend the linear core without new data-carrying part
kinds:

- **Mutual inductance / transformers** — `Circuit::couple(l1, l2, k)` couples two
  existing inductors with `M = k·√(L1·L2)`. It is stamped with a branch-current
  row (robust even at the ideal `k = 1`, where an admittance companion's
  inverse-`L` is singular) in **transient and AC**; isolated inductors keep their
  exact companion, so the golden suite stays bit-for-bit.
- **Transmission lines** — `Circuit::add_line(a, b, z0, delay)` adds a lossless
  (Branin) or lossy line. These resolve in the **fixed-step** transient (a
  transmission line needs a fixed timestep; the adaptive driver is never used
  with lines) and in AC.

---

## 9. Determinism and portability

The engine takes no wall-clock, no filesystem, and no OS entropy; its only
randomness is the seeded Monte-Carlo PRNG. Node ordering is derived from a sorted
map, device iteration follows part-index order, and nothing depends on hash
iteration order. The result: the same circuit, analysis, and seed produce the same
`Signal`s everywhere — in CI, on an ARM SBC, and in a `wasm32` browser build —
which is what makes the results trustworthy to teach from, test against, and have
the AI verify.
