# Checkpoint ledger

The single source of truth for **progress**. Each checkpoint has a verifiable **acceptance gate**. States: `[ ]` todo · `[~]` in progress · `[x]` done (gate passing) · `[!]` blocked.

Conventions and how tracks relate: [`README.md`](README.md). Design detail per checkpoint: the matching `track-*.md`.

> Rule: a checkpoint flips to `[x]` **only** when its gate command/test passes on a green `cargo test --workspace`.

---

## Track: sim-core (`ee-core`, `ee-sim`) → design: [`track-sim-core.md`](track-sim-core.md)

### M0 — linear engine + transient  ✅ complete
- [x] **CP-SIM-M0.1** Circuit graph: add/remove parts, `wire` merges nets, name lookup, stamping iteration. — *gate:* `cargo test -p ee-core graph::` covers net-merge + lookup.
- [x] **CP-SIM-M0.2** Units + engineering-notation `Value` parser. — *gate:* `cargo test -p ee-core value::` (done in scaffold).
- [x] **CP-SIM-M0.3** Linear MNA + DC operating point (R + independent V/I sources). — *gate:* `resistive_divider_operating_point` solves to 1e-9; `current_source_into_resistor_operating_point`.
- [x] **CP-SIM-M0.4** Companion models for C/L + Backward-Euler transient. — *gate:* `rc_step_transient_matches_analytic` within 1%; also `rl_step_transient_matches_analytic`, `series_rlc_rings_at_the_damped_frequency`.
- [x] **CP-SIM-M0.5** `Signal` type + results from an analysis. — *gate:* transient returns `Vec<Signal>` with correct units/length; consumed headless by the shell + `ee demo`.

### M1 — nonlinear + frequency domain
- [x] **CP-SIM-M1.1** Nonlinear device framework + Newton–Raphson; diode. — *gate:* `diode_iv` matches Shockley to machine precision; `diode_resistor_op_converges` (Vd=0.6925 V, residual 1e-17); `nonconvergence_is_reported`; linear circuits stay single-iteration. `trait NonlinearDevice` so BJT/MOSFET (M1.2) drop in; SPICE `pnjlim` junction limiting.
- [x] **CP-SIM-M1.2** BJT + MOSFET models. — *gate:* CE amp biases at Vbe≈0.65 V, Ic≈βF·Ib, Vc mid-rail; MOSFET square-law `Id=K(Vgs−Vth)²(1+λVds)`; CE small-signal AC gain |Av|≈gm·Rc. Generalized `NonlinearDevice` to an N-terminal Jacobian companion (BJT/MOSFET 3×3); `PartKind::{Bjt,Mosfet}(Polarity, Model)`.
- [x] **CP-SIM-M1.3** Convergence aids (gmin/source stepping, junction limiting). — *gate:* `stiff_mosfet_needs_convergence_aids` — a diode-connected MOSFET fed only by a current source (cut off → singular at cold start) fails plain NR but the aided solve lands Vd=2.0 V; plain-convergent circuits skip the aids (fast path intact).
- [x] **CP-SIM-M1.4** AC small-signal (complex MNA). — *gate:* `rc_ac_bode` — RC low-pass is −3.010 dB and −45.0° at `fc`, ~−20 dB/decade rolloff. Complex MNA (`mna_complex.rs`), nonlinear devices biased via the NR op-point. **Also landed:** time-varying independent sources (`Waveform::{Dc,Sine,Pulse}`, `Circuit::set_waveform`) evaluated through transient — the M0 source follow-up.
- [x] **CP-SIM-M1.5** DC sweep + parametric sweep. — *gate:* divider swept 0→10 V is linear in the source; diode current sweep matches inverse-Shockley at every point; `Parametric` runs any inner analysis per value. `Circuit::set_value`.
- [x] **CP-SIM-M1.6** Adaptive timestep (LTE control). — *gate:* `adaptive_transient_beats_fixed_on_an_rc_step` — step-doubling LTE over a 20τ RC uses ~182 points vs 10001 fixed (~55×) at equal accuracy; grid coarsens and lands exactly on `duration`. `step: None` now means engine-chosen.

### M2 — statistical + scale
- [x] **CP-SIM-M2.1** Noise analysis (output spectral density). — *gate:* `resistor_thermal_noise` — a 1 kΩ output floor = 4kTR within 1e-6, flat over frequency; a divider gives 4kT·(R1∥R2). `Analysis::Noise`; transimpedance via the complex-MNA network. (`4kT` at 300.15 K.)
- [x] **CP-SIM-M2.2** Monte-Carlo + component tolerances (seeded, deterministic). — *gate:* `monte_carlo_is_deterministic_under_a_fixed_seed` — seed 42 twice → bit-identical 400-sample vectors; ±5% divider mean ≈ 5 V. In-crate SplitMix64 (no dep), `Circuit::set_tolerance`, `Analysis::MonteCarlo`.
- [x] **CP-SIM-M2.3** Controlled sources (VCVS/VCCS/CCVS/CCCS). — *gate:* gain-10 VCVS exact at op-point and flat in AC; VCCS `Vout=G·Vin·R`; an ideal op-amp (huge-gain VCVS) non-inverting config → 1+Rf/Rg=10; CCVS/CCCS current-sense. Stamped in real + complex MNA. `PartKind::{Vcvs,Vccs,Ccvs,Cccs}` (native format round-trips the gain/ref; SPICE export defers).
- [x] **CP-SIM-M2.4** Sparse solver path for large nets. — *gate:* row-sparse LU with partial pivoting, auto-routed above N=64; `sparse_matches_dense_bit_for_bit`; bench `sparse_vs_dense` shows 2.6×/4.5×/8.1× at N=250/500/1000 (sparse near-linear vs dense quadratic, diverging with N).

---

## Track: boards (`ee-digital`, `ee-mcu`, `ee-devices`) → design: [`track-boards.md`](track-boards.md)

### M1 — digital kernel + AVR
- [x] **CP-BRD-M1.1** Event-driven digital kernel (event heap, delays, 0/1/X/Z). — *gate:* a 3-inverter ring oscillates at exactly 2·N·d (60 fs); integer-femtosecond queue with FIFO tie-break (deterministic). Dependency-free + wasm-clean.
- [x] **CP-BRD-M1.2** Logic primitives (gates, D/JK flip-flops, counters, mux). — *gate:* NAND/NOR/XNOR/buf/tri-state truth tables, D & JK behavior, 2:1/4:1 mux, a 4-bit ripple counter 0→15→0 with async clear.
- [x] **CP-BRD-M1.3** AVR core (ATmega328P): decode + execute + interrupts. — *gate:* table-driven decoder + per-instruction execute tests over a broad subset (ALU/branches/memory/bit-ops/flow), a count-down loop + array-sum program, interrupt dispatch + RETI, raw & Intel-HEX loaders. Documented TODO: MUL/LPM/ADIW/SBIW/SLEEP decode to `Instr::Unknown`.
- [x] **CP-BRD-M1.4** AVR peripherals (GPIO, timers/PWM, ADC, UART). — *gate:* Fast-PWM duty on OC0A within 0.02 of 64/256; USART TX stream decodes to the written bytes at 9600 baud; ADC half-ref → code 512. I/O window routes through `Peripherals`; added ADIW/SBIW opcodes.
- [x] **CP-BRD-M1.5** Uno board def + firmware load (`.hex`). — *gate:* `blink.hex` (committed fixture, regenerated from a documented assembly listing) loads via Intel-HEX; D13/PB5 toggles at 1.000 s/toggle (~1 Hz). ELF still `UnsupportedFormat`.

### M2 — co-simulation + more cores
- [x] **CP-BRD-M2.1** Co-sim bridge: pin↔net, clock sync. — *gate:* `analogRead` of 2.5 V → ADC code 512 (cosim tests); **`ee cosim`** runs real AVR firmware whose `digitalWrite(13,HIGH)` drives the lockstep bridge → a simulated LED lights at 19.4 mA. Surface (`PinIo`, `McuCore::{next_event, step_to_cycle}`) in `ee-mcu`; lockstep loop in the `ee` binary.
- [x] **CP-BRD-M2.2** Mixed-signal boundary elements (threshold, drive strength, rise/fall). — *gate:* Schmitt-hysteresis input resolution (`ee_digital::Logic`), Thévenin output (drive high/low/HighZ → v_oc + series R) that sags under load; slew limiting. Rides `PinIo`.
- [x] **CP-BRD-M2.3** Cortex-M0+ core + RP2040 (Raspberry Pi Pico). — *gate:* ARMv6-M (Thumb) per-instruction + flag vectors; NVIC-lite exceptions; a `Pico` (RP2040 SIO GPIO) blink toggles GP25. Implements the same `McuCore`/co-sim surface as AVR.
- [x] **CP-BRD-M2.4** ATmega2560 / ATtiny variants. — *gate:* `AvrVariant` parameterizes flash/SRAM/ports/timers/pins/vector-stride off the shared 328P core (328P/2560/ATtiny85); a Mega board; a GPIO program runs on each; high-SRAM round-trips on the 2560. (Per-part native register maps deferred.)
- [x] **CP-BRD-M2.5** External toolchain compile orchestration (`arduino-cli`/`avr-gcc`). — *gate:* behind an off-by-default `toolchain` feature + runtime presence check; `--features toolchain` compiles `blink.c` → HEX → runs (D13 high), SKIPPING cleanly when no compiler is on PATH (as here). Default `cargo test` pulls in no `std::process` path.

---

## Track: ui-shell (`ee-tui`, `ee-shell`, `ee-instruments`) → design: [`track-ui-shell.md`](track-ui-shell.md)

### M0 — shell + first pixels
- [x] **CP-UX-M0.1** eesh parser + core commands (`add`/`wire`/`probe`/`run`/`show`/`help`). — *gate:* parse/dispatch tests; a scripted session builds a circuit graph. _(`set <param>`: follow-up.)_
- [x] **CP-UX-M0.2** TUI shell pane + schematic list/net view. — *gate:* `Schematic` + `Screen`/`Pane` render-to-string widgets; layout tests assert panes compose, fit width, and list every part. _(Live crossterm event loop + docking is M1: `CP-UX-M1.4`.)_
- [x] **CP-UX-M0.3** Braille waveform plot widget. — *gate:* plotting a known sine matches a golden ASCII snapshot.
- [x] **CP-UX-M0.4** probe → plot pipeline (consume `Signal`). — *gate:* shell `run` + `ee demo` plot the RC curve from `CP-SIM-M0.4`.

### M1 — instruments + interaction
- [x] **CP-UX-M1.1** Oscilloscope (multi-channel, edge/level trigger, cursors). — *gate:* `trigger_locks_a_periodic_signal_regardless_of_phase` (3 capture phases → byte-identical golden frame); cursors read Δt/Δv/frequency. Self-contained in `ee-instruments` (no `ee-tui` dep — that cycle is forbidden).
- [x] **CP-UX-M1.2** Multimeter + function generator + bench PSU panels. — *gate:* DMM reads a node voltage (DC/RMS/Vpp/freq); funcgen's generated sine renders on the scope as a stable frame; PSU CV↔CC crossover. Self-contained in `ee-instruments`. _(Wiring the funcgen into a live circuit source pairs with sim-core's time-varying source — a later integration.)_
- [x] **CP-UX-M1.3** Live overlay (per-net voltage + color ramp). — *gate:* `nets_are_annotated_with_their_voltage_and_ground_reads_zero` — a divider renders n1=5V, n2=2.5V, gnd=0V on the right rows; 5-band ANSI color ramp with visible-width-correct layout + plain golden snapshot; composes into `Screen`. _(current-flow animation: later.)_
- [x] **CP-UX-M1.4** Dockable panels + mouse placement + command palette. — *gate:* pure `handle_event` state machine (focus cycle, resize geometry, palette fuzzy-filter, mouse hit-test) — 16 app + 7 crossterm-translation tests, no tty. `ee_tui::run<B: Backend>` thin wrapper; wired to `ee tui`.
- [x] **CP-UX-M1.5** Scripting engine (`.eesh`, loops/params) + deterministic replay. — *gate:* `param_sweep_runs_and_replays_identically` — `repeat N as win { run tran ${win}m }` runs and `Recording::replay()` reproduces a byte-identical transcript. `Shell::run_script`.

### M2 — analysis views
- [x] **CP-UX-M2.1** Logic analyzer + protocol-decode view (UART/SPI/I²C). — *gate:* an 8-N-1 capture of `"Hi!"` decodes to `[0x48,0x69,0x21]`; SPI mode-0 + I²C start/addr/ack/data/stop decoders; multi-channel timing view with annotated frames, fits width.
- [x] **CP-UX-M2.2** Breadboard view. — *gate:* `plain_render_matches_golden_snapshot` — nets→columns (union-find-collapsed, ground leftmost), parts→rows with standard designators (R/C/L/V/I/D/Q/M/E/G/H/F), power rails + trench; composes full-width into `Screen`.
- [x] **CP-UX-M2.3** Bode/spectrum panel + scope math/FFT channels. — *gate:* Bode panel renders magnitude+phase vs log-f with the −3 dB corner; hand-rolled radix-2 FFT validated against a direct DFT (<1e-9), a pure sine → one dominant bin; scope math channels (A±B, A·B).
- [x] **CP-UX-M2.4** Session snapshots / undo / named checkpoints. — *gate:* `undo` reverts the last mutating command (auto-snapshot before add/wire/probe); named `checkpoint`/`restore`; friendly errors on nothing-to-undo / unknown checkpoint.

---

## Track: ai-fmt (`ee-ai`, `ee-fmt`) → design: [`track-ai-fmt.md`](track-ai-fmt.md)

### M0 — persistence
- [x] **CP-AF-M0.1** Native project format save/load (round-trip). — *gate:* save→load yields an equal circuit; golden file stable. _(Bonus: a friendly netlist import format also landed — partial progress toward `CP-AF-M1.1`, though that one is SPICE-specific.)_

### M1 — interop + first AI
- [x] **CP-AF-M1.1** SPICE netlist import/export. — *gate:* `imported_divider_simulates_to_the_midpoint` (import→`ee-sim` op-point = 5 V) + `round_trip_preserves_parts_and_topology`; handles the SPICE `m`(milli)-vs-`meg`(mega) footgun.
- [x] **CP-AF-M1.2** Claude client (Messages API, streaming, tool schema, prompt caching) over a mockable transport. — *gate:* tool-use loop driven to completion via `MockTransport` (tool_use→result→final text), streaming reassembly, cache markers, `claude-sonnet-5-5` default + env override, key runtime-only — 13 tests, no network, no new deps.
- [x] **CP-AF-M1.3** AI tool layer over the circuit API (add/wire/set/run/read). — *gate:* `ask_is_grounded_in_a_real_sim_read` — stub model drives a REAL `Shell`+engine via `eesh`/`read_op` tools, builds a 12 V divider, and its answer (8.000000 V) matches the real solve (12·2k/3k). The AI does only what a user could; no network.

### M2 — verified design + waveform export
- [x] **CP-AF-M2.1** Verification loop: staged patch → scratch apply → re-sim → measure → present diff; `apply`/`reject` gate. — *gate:* `verify` clones the base shell (`&Shell` → never mutates the live circuit), scratch-applies, measures; "5 V→3.3 V @ 500 mA" → good patch PASSes citing measured 3.3 V / 0.5 A, bad patch FAILs; live circuit unchanged until explicit `apply`.
- [x] **CP-AF-M2.2** Design / Debug / Firmware modes. — *gate:* Debug localizes a seeded fault — on a 10 V divider with wrong R1=3k it reads v(out)=2.5 V and fingers R1 (smallest correction); Design reuses the verify loop; Firmware validates Intel-HEX ready for `load_firmware` (live flash is a thin follow-up).
- [x] **CP-AF-M2.3** VCD + CSV waveform export. — *gate:* CSV matches the probe data byte-for-byte (complex → mag/phase columns); VCD is well-formed ($timescale/$var, strictly-increasing ticks, change-only records; analog `real` vars, 0/1 → scalar bits).

### M3 — breadth interop + offline
- [x] **CP-AF-M3.1** KiCad netlist import; BOM export (CSV). — *gate:* a KiCad s-expr netlist imports and solves to the 5 V divider midpoint; BOM groups by kind+value with quantities + sorted references (summed qty = part count). `import_kicad` / `to_bom_csv`.
- [x] **CP-AF-M3.2** Offline mode: RAG parts index + heuristics/tutor. — *gate:* `offline_needs_no_api_key_or_transport` — DRC rules (short-across-source, no-path-to-ground, floating node, LED-without-resistor), value calculators (divider, RC/RL τ, cutoff, series/parallel, LED resistor), and a keyword-scored parts catalog — all pure, deterministic, no model/network.

---

## Track: platform (cross-cutting, on `main`) → design: [`README.md`](README.md) §Cross-cutting
- [x] **CP-PLAT.1** CI green: fmt + `clippy -D warnings` + test on linux/macOS/windows (+ MSRV 1.79, docs). — *gate:* `.github/workflows/ci.yml` runs the matrix; gates pass locally.
- [x] **CP-PLAT.2** `aarch64` build (Raspberry Pi / Apple Silicon). — *gate:* Apple Silicon (`aarch64-apple-darwin`) is the dev host — the whole suite builds + passes on it natively; CI adds an `aarch64-unknown-linux-gnu` (Raspberry Pi) cross-build job.
- [x] **CP-PLAT.3** `wasm32-unknown-unknown` build of the compute core. — *gate:* `ee-core`+`ee-sim` compile to wasm (CI `wasm` job + verified locally).
- [x] **CP-PLAT.4** Criterion benches wired for `stamp/solve` (+ parser). — *gate:* `cargo bench` runs; recorded op-point/transient/N-ladder-scaling + parse numbers. _(Event-queue bench deferred until the digital kernel exists — `CP-BRD-M1.1`.)_
- [x] **CP-PLAT.5** Determinism harness: golden sims reproduce bit-for-bit. — *gate:* `full_golden_suite_runs_twice_identically` — op/transient(fixed+adaptive)/AC/noise/seeded-MC run twice are bit-identical, with committed golden values (exact where representable, incl. the leading MC samples) and a seed-divergence check.

---

## M3 — breadth (workstream) + WASM (webstream)  → plan: [`M3-PLAN.md`](M3-PLAN.md)

- [x] **CP-SIM-M3.1** (sim-core) Behavioral/expression sources + richer device models (tempco). — *gate:* `expr_source_reproduces_a_sine_in_transient` (`sin(2*pi*1000*t)`) + a compound waveform; `resistor_tempco_shifts_a_divider` (5 V at 27 °C → 3.333 V at 127 °C). `Waveform::Expr` (hand-rolled evaluator), `Circuit::set_temperature`/`set_tempco`.
- [x] **CP-BRD-M3.1** (boards) HDL-lite: truth-table/expression logic blocks, ROM/RAM, tri-state bus. — *gate:* a 2-bit adder from a `TruthTable` and a `Rom` lookup simulate; `Expr` boolean blocks; `Ram` write-then-read; a tri-state bus resolves (one→value, both→X, none→Z).
- [x] **CP-BRD-M3.2** (boards) Custom-board manifests + drop-in modules (7-seg/LED/servo/WS2812). — *gate:* `from_manifest` builds a custom board (pin map + onboard parts); ESP32/STM32 pin-map stubs; 7-seg segments, WS2812 color decode, servo pulse→angle, LED lit-when-driven all verified.
- [x] **CP-UX-M3.1** (ui-shell) Dataflow patcher: processing blocks → reusable virtual instruments + pass/fail test sequences. — *gate:* `gain_then_rms_is_twice_input_rms` + topo-eval on a branching patch, cycles reported (no hang); `VirtualInstrument` re-runs; `TestSequence` names the failing check. Blocks: Gain/Offset/Abs/Clamp/Limit/Add/Sub/Mul/Filter/Rms/Mean/Peak/Log.
- [x] **CP-AF-M3.3** (ai-fmt) Tutor mode + guided lessons/graded challenges + glossary. — *gate:* `challenge_passes_a_correct_divider_via_real_sim` (3.3 V PASS) / `..._fails_a_wrong_divider_and_names_the_miss`; `LessonRunner` steps part-by-part; glossary define/search; deterministic grader (model path optional).
- [x] **CP-AF-M3.4** (ai-fmt) Fritzing-style export. — *gate:* well-formed `.fz` XML — one `<instance>` per part (reference/kind/value) + per-connector `<connects>` encoding resolved nets; structurally validated; deterministic.
- [x] **CP-WEB.1** (web) `ee-web`: wasm-bindgen engine bindings + a minimal browser app (build/parse → run → render). — *gate:* `cargo build -p ee-web --target wasm32-unknown-unknown` + `wasm-pack build` (~196 KB) succeed; host-side pipeline test (netlist → op + transient → charges to rail); `index.html` runs a sim and shows a braille scope + op-point table; wasm deps gated to `wasm32` so host `build/clippy/test --workspace` stays green (468 tests).
- [x] **CP-SHIP.1** (platform) Polish & ship: expand examples/README, refresh docs for M3 + the browser build, full CI-parity sweep, tag a release. — *gate:* ~500 tests + clippy `-D warnings` + fmt + `wasm32` (core **and** `ee-web`) + docs all green; README/docs hub updated; **v0.1.0** tagged (MIT OR Apache-2.0). Documented non-cheap follow-ups kept open: SPICE diode/transistor export, native waveform round-trip, real Xtensa/Cortex-M3 cores.

---

## M4 — vision / stretch  → plan: [`M4-PLAN.md`](M4-PLAN.md)

- [x] **CP-SIM-M4.1** (sim-core) Plugin device-model API + registry. — *gate:* `plugin_square_law_device_matches_hand_analysis` — an I(V)=k·V² device defined in the test (outside the built-ins) registers via `DeviceRegistry` and solves to Vn=1 V exactly (KCL ≤1e-12), stamping in NR/op/transient/AC. `NonlinearDevice`/`Companion` now public; `Engine::register_device`.
- [x] **CP-SIM-M4.2** (sim-core) KLU-class sparse: fill-reducing ordering + symbolic-factorization reuse. — *gate:* `ordered_matches_dense` (≤1e-12 on arrow/grid/ladder) + `refactor_reuses_symbolic`; bench `ordered_vs_natural` ~5300× at 1k on the shared-hub pathology (near-linear). _(Solver + reuse + scaling complete; wiring into the engine's default hot loop is a documented remaining step, kept out so the golden bit-for-bit suite stays exact.)_
- [x] **CP-SIM-M4.3** (sim-core) Multithreaded analyses (MC/sweep), native-gated, deterministic. — *gate:* `parallel_monte_carlo_matches_sequential` bit-for-bit over 512 runs (via `SplitMix64::at_offset` O(1) per-run seek); golden suite identical under `--features parallel`; ~3× at 8k MC runs. `std::thread`, no deps, off on wasm.
- [x] **CP-BRD-M4.1** (boards) Cortex-M3/M4 (Thumb-2) STM32-class core. — *gate:* Thumb-2 vectors (MOVW/MOVT, MUL/MLA, UBFX/BFI, IT blocks, LDM/STM.W, B.W, MRS/MSR) + a runnable `Stm32` board whose hand-assembled blink toggles PC13. 16-vs-32-bit decode via `w0[15:11]`; STM32 GPIO on a disjoint co-sim pin block. 106 ee-mcu tests.
- [x] **CP-AF-M4.1** (ai-fmt) SPICE `.model` import + a small vendor model library. — *gate:* `diode_model_is_overrides_default_and_changes_junction_drop` (IS=1n → 0.357 V vs default 0.655 V); BJT `IS/BF/BR` + MOSFET `VTO/KP/LAMBDA` mapped; catalog resolves 1N4148/2N3904/2N2222/1N4007/2N7000; unknown params noted, not errors. _(Diode `N` needs an ee-core `DiodeModel` — documented follow-up; only `IS` reaches the diode today.)_
- [x] **CP-UX-M4.1** (ui-shell) Instrument/block plugin surface + session export. — *gate:* `plugin_block_evaluates_and_composes_with_builtins` (a `ProcessingBlock` trait obj in a patch) + `session_round_trip_is_equivalent` (circuit+probes+checkpoints export→import identical; `save`/`load` commands). ee-shell reuses ee-fmt's project format (acyclic).
- [x] **CP-WEB.2** (web) Shareable/publishable circuit via a URL fragment. — *gate:* hand-rolled URL-safe base64url `encode`/`decode` round-trips exactly (comments/CRLF/unicode; garbage → clean Err); page reads `location.hash` → loads+runs, "Copy link" sets+copies it; decoded link runs through `run()`. 11 host tests, no dep.

---

## M5 — close the debt + extend the frontier  → plan: [`M5-PLAN.md`](M5-PLAN.md)

- [ ] **CP-SIM-M5.1** (sim-core) Wire KLU ordered/reusable solver into the engine's default hot loop. — *gate:* golden suite + large-ladder bit-for-bit intact; large net measurably faster.
- [ ] **CP-SIM-M5.2** (sim-core) `DiodeModel` (Is/n/Rs) so SPICE `N`/`RS` apply. — *gate:* `N`≠1 + `RS`>0 shift the I–V as predicted; default diode unchanged.
- [x] **CP-AF-M5.1** (ai-fmt) SPICE device export (D/Q/M + `.model`, E/G/H/F). — *gate:* `device_and_controlled_source_round_trip` (diode+NPN+NMOS+VCVS kind/params/topology) + `pmos_vto_sign_round_trips`; importer extended to parse E/G/H/F.
- [x] **CP-AF-M5.2** (ai-fmt) Native project waveform + tolerance/tempco round-trip. — *gate:* `waveform_tolerance_tempco_and_temp_round_trip_exactly` (Sine+Expr source, ±5% tol, tempco, 85 °C → byte-identical re-save); `.wave`/`.tol`/`.tempco`/`.temp` directives.
- [ ] **CP-BRD-M5.1** (boards) AVR SPI + I²C (TWI) master. — *gate:* an AVR SPI/I²C transfer decodes correctly on the logic analyzer.
- [ ] **CP-BRD-M5.2** (boards) Composite device subcircuits: op-amp macromodel, analog switch, relay. — *gate:* the op-amp hits its closed-loop gain; the switch gates a signal.
- [x] **CP-UX-M5.1** (ui-shell) Interactive schematic editing in the live TUI. — *gate:* `edit_actions_place_wire_and_remove_on_the_real_circuit` + keystroke-driven selection/delete; `EditAction`→shell grammar (shared mutation path); new `remove` command. All asserted on the `Circuit`, no tty.
- [x] **CP-WEB.3** (web) Browser schematic editor (edit, not just paste). — *gate:* `add_part_changes_the_netlist_and_still_simulates` / `remove_part…` — a `CircuitModel` (preserving node names + raw value notation) round-trips to netlist and re-simulates; page has a parts form that re-runs on edit.
- [x] **CP-WEB.4** (web) Full-session shareable links (async collaboration). — *gate:* `session_round_trips_exactly` (netlist + probes + duration/step, URL-safe, garbage→clean Err); page restores every field on `hashchange` + falls back to M4 netlist-only links. Real-time co-edit still needs a relay (documented).

---

### Progress summary

| Track | Done | In progress | Total |
|---|---|---|---|
| sim-core | 19 | 0 | 21 |
| boards | 13 | 0 | 15 |
| ui-shell | 16 | 0 | 16 |
| ai-fmt | 14 | 0 | 14 |
| platform | 6 | 0 | 6 |
| web | 4 | 0 | 4 |
| **Total** | **72** | **0** | **76** |

_**67/67 — M0 through M4 all complete** (M0–M3 tagged v0.1.0; M4 adds the rest). M4 delivered the **plugin device-model API** + an **instrument/block plugin trait**, a **KLU-class ordered/reusable sparse solver** and **multithreaded analyses**, a **Cortex-M3/M4 (Thumb-2)** core with an STM32 board, **SPICE `.model` import** + a vendor model library, and **shareable circuit links** in the browser. Deferred by design: real-time collaboration, the QEMU full-SBC bridge, and full vendor-SPICE `.model` fidelity — the standing M-next horizon._

*Update this table when flipping a checkpoint.*

**🎉 Every checkpoint is complete — M0, M1, M2, and the two M3 interop items — 52/52.**

- **M0** proved the loop: type a circuit → simulate → see a wave.
- **M1** made it a real simulator: nonlinear devices (diode/BJT/MOSFET) with Newton–Raphson + convergence aids, AC/Bode, DC/parametric sweeps, adaptive timestep; an event-driven digital kernel + an instruction-accurate **AVR (ATmega328P)** core running `blink.hex`; oscilloscope/DMM/funcgen/PSU + a live overlay + a dockable TUI + eesh scripting; SPICE I/O; and a grounded Claude copilot.
- **M2** made it co-simulate and analyze: the **lockstep co-sim bridge** (AVR *and* RP2040/Pico drive real circuit nodes — `ee cosim`), mixed-signal boundary, controlled sources, **noise** + seeded **Monte-Carlo**, a **sparse solver** (near-linear on large nets), the logic analyzer with **UART/SPI/I²C decode**, Bode/spectrum/FFT, breadboard view, **VCD/CSV** export, the AI **verification loop** + Design/Debug/Firmware modes, AVR variants + gated toolchain compile, KiCad import + BOM, an offline (no-key) mode, aarch64 CI, and a bit-for-bit **determinism harness**.

**427 workspace tests green**; `clippy -D warnings`, `rustfmt`, `wasm32` core, and `-D warnings` docs all clean; an audit pass fixed 6 latent bugs and shipped user guides + run-verified examples.

The one roadmap item intentionally left beyond the ledger: a **WASM browser front end** (the compute core is already wasm-clean — `CP-PLAT.3`). M3+ breadth (dataflow patcher, custom-board manifests, HDL-lite, guided lessons) is the next horizon.
