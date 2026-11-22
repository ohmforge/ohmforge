# M10 plan — improvements across every area

M0–M9 complete (v0.8.1). M10 is a breadth push: a planning fan-out (one agent per
track, read-only, grounded in `file:line`) produced ranked proposals in
`docs/plan/m10-proposals/*.md`; this plan takes the top items across all six
tracks — performance, numerical fidelity, MCU correctness, UX, capability, and
engineering quality — including the core engine.

Standing rules hold: `ohmforge-core`/`ohmforge-sim` stay dependency-free +
wasm32-clean + deterministic; the **golden/determinism suite stays bit-for-bit**
(anything that would move it ships **opt-in**); `ohmforge-mcu` keeps no
`ohmforge-sim` dependency; new deps stay behind opt-in/dev-only; crate ownership
is disjoint per track so the waves don't collide.

## Execution waves (dependency-ordered)
- **Wave 1 (parallel):** sim-core · boards · ui-shell · ai-fmt — disjoint crates.
- **Wave 2:** web — starts after **AF-1** (netlist waveform parsing in `ohmforge-fmt`) merges, which unblocks in-browser AC/transient.
- **Wave 3:** platform — last, so its fuzz/property tests land on top of the final feature code (they touch the same crates).

---

## Checkpoints

### sim-core (`ohmforge-core`, `ohmforge-sim`)
- **CP-SIM-M10.1** Reuse the ordered symbolic/numeric factorization on the **complex AC/noise solver** (today `mna_complex` is dense and re-factors from scratch at every swept frequency though the structure is identical). Size-gated so small golden fixtures keep the dense path. *Gate:* AC/noise results bit-for-bit vs. today; a swept-AC benchmark is markedly faster at scale; golden suite unchanged.
- **CP-SIM-M10.2** **Opt-in trapezoidal / Gear-2 transient integration** (Backward-Euler stays the default, so every transient golden is untouched). *Gate:* an LC tank rings with trap where BE over-damps; BE path bit-for-bit; selectable via the analysis/engine API.

### boards (`ohmforge-mcu`, `ohmforge-devices`)
- **CP-BRD-M10.1** **Cortex-M Thumb-2 correctness** — implement the encodings ordinary compiled C hits immediately (`CBZ/CBNZ`, `UDIV/SDIV`, load/store writeback), and make an `Unknown` opcode a **graceful** stop/diagnostic instead of a `debug_assert!` panic. *Gate:* a hand-assembled Thumb-2 program exercising CBZ/UDIV/writeback runs to the right result; an unknown opcode no longer panics in test builds.
- **CP-BRD-M10.2** **AVR timer + USART interrupt vectors** — source the Timer0/1 overflow/compare and USART RX/TX-complete interrupts the controller already has flags/`TIMSK` storage for, so `millis()`/`delay()`/`Serial`-style ISR firmware runs. *Gate:* a program with a timer-overflow ISR advances a counter at the expected rate; a USART-RX ISR fires on a received byte.

### ui-shell (`ohmforge-tui`, `ohmforge-shell`, `ohmforge-instruments`, + the `ohmforge` REPL completer)
- **CP-UX-M10.1** Wire the real **`Oscilloscope`** (trigger, cursors Δt/Δv/freq, Vpp/mean/RMS) into the live scope pane, and sweep **`run sparams`** into an **S11 locus on the Smith chart**. Reuses already-tested instrument logic (`ohmforge-tui` already depends on `ohmforge-instruments`). *Gate:* no-tty data-asserted tests — cursors read Δt/Δv on a known capture; the Smith pane plots a swept S11 locus.
- **CP-UX-M10.2** **Discoverability + fixes:** an in-TUI help/keymap overlay (`?`/F1); fix the tab-completion `RUN_ANALYSES` omission of `sparams` (`crates/ohmforge/src/lib.rs`); surface FFT/DMM as shell verbs. *Gate:* completion offers `sparams`; the overlay renders the keymap (asserted on data).

### ai-fmt (`ohmforge-ai`, `ohmforge-fmt`)
- **CP-AF-M10.1** **Touchstone `.sNp` export** (sweep `Engine::s_parameters` → the standard RF format; the exporter takes `ohmforge_core::Complex` so `ohmforge-fmt` stays core-only), **and SINE/PULSE waveform parsing in the netlist importer** (unblocks Wave-2 web AC/transient on imported decks). *Gate:* a byte-exact Touchstone golden; a netlist with a `SINE(...)`/`PULSE(...)` source imports and simulates.
- **CP-AF-M10.2** **Deeper offline (no-key) heuristics** — more DRC rules, more value calculators, a richer parts catalog (pure, deterministic, `core`-only). *Gate:* new rules/calculators each unit-tested against hand analysis; `of review` surfaces them with no key/network.

### web (`ohmforge-web`) — Wave 2
- **CP-WEB-M10.1** **Unlock diodes / transistors / current sources in the browser** by routing the web `run()` through the existing wasm-clean `ohmforge_fmt::spice::import` (handles `.model` D/Q/M). *Gate:* host-test — a diode/BJT deck imports and solves an op-point/transient through the web path.
- **CP-WEB-M10.2** **In-browser AC/Bode** (bind `Analysis::Ac` + an SVG Bode plot) and **PWA/offline** (service worker + manifest). *Gate:* host-test for the AC binding; a registered service worker + valid manifest so the page is installable/offline.

### platform (cross-cutting) — Wave 3
- **CP-PLAT-M10.1** **Performance-regression CI** — a job that runs the Criterion `solve`/`parse` benches against a stored baseline and flags regressions beyond a threshold. *Gate:* the job runs in CI and compares to a committed baseline.
- **CP-PLAT-M10.2** **Fuzzing + property tests** — `proptest`/`cargo-fuzz` (dev-only) on the parsers (`Value::parse`, netlist, digital `Expr`) and a solver property (e.g. KCL residual). *Gate:* proptest suites pass; fuzz targets build and run a short smoke in CI; shipped crates stay dependency-free.

### ship
- **CP-SHIP.6** Polish & tag **v0.9.0**: full CI-parity gate sweep, README/docs/CHANGELOG refreshed to M0–M10, tag.

## Deferred / you-gated
crates.io publish automation (needs `CARGO_REGISTRY_TOKEN` + your go), a RISC-V
core, Xtensa windowed-ABI, ELF loading, Gerber export, and a hosted co-edit
service — all tracked in the per-track proposals for a later round.
