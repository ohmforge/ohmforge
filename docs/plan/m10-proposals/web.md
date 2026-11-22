# M10 proposals — `ohmforge-web` (browser app)

Scope: `crates/ohmforge-web` (wasm-bindgen bindings + `www/` app). Read-only study of
`src/lib.rs`, `src/braille.rs`, `src/bin/relay.rs`, `www/index.html`, `www/style.css`,
and the crates the web path already reaches (`ohmforge-core`, `ohmforge-sim`,
`ohmforge-fmt`). Proposals are **web-track only**; anything needing another crate's
source is flagged under *Cross-crate coordination*.

---

## What already works (be honest about the baseline)

The browser app is genuinely complete for its current remit, and most of the "hard"
logic is pure, host-tested Rust with a thin `#[cfg(target_arch = "wasm32")]` shell:

- **Simulate**: `run()` parses a netlist, runs an op-point + a transient, renders a
  braille scope, and serializes hand-rolled JSON (`src/lib.rs:46`, binding
  `simulate`/`scope` at `src/lib.rs:2000`/`:2010`). Fixed or adaptive step.
- **Share links**: base64url netlist links (`encode`/`decode`, `src/lib.rs:209`) and
  full-session links with duration/step/probes (`encode_session`, `src/lib.rs:517`).
- **Structured edit**: `CircuitModel` parts form — list/add/remove (`src/lib.rs:294`,
  bindings `src/lib.rs:2037`–`:2070`).
- **Visual canvas**: `Schematic` union-find → netlist, blob round-trip, lift-from-netlist
  (`src/lib.rs:1641`), SVG canvas with R/C/L/V/wire/gnd/move/delete tools
  (`www/index.html:239`–`:248`).
- **Real-time co-edit**: op-based `Replica`/`EditOp` LWW merge (`src/lib.rs:723`), a
  `BroadcastChannel` `CoEdit` client (`src/lib.rs:2153`), and a pure relay router
  (`Relay`, `src/lib.rs:1148`) with a feature-gated native WebSocket `server`
  (`src/lib.rs:1240`).

**The one real ceiling.** The engine is far more capable than the browser exposes. The
web `simulate` path routes through `ohmforge_fmt::netlist::parse`, whose `classify()`
accepts only **R/C/L/V** (`crates/ohmforge-fmt/src/netlist.rs:137`–`:146`), and `run()`
only ever asks the engine for `OperatingPoint` + `Transient` (`src/lib.rs:50`–`:66`).
But `ohmforge-sim` **already models** diodes, BJTs, MOSFETs and current sources
(`PartKind` in `crates/ohmforge-core/src/graph.rs:32`–`:54`) and **already runs** AC/Bode,
DC-sweep, noise, Monte-Carlo and S-parameters (`Analysis` enum,
`crates/ohmforge-sim/src/lib.rs:37`–`:93`). None of that reaches the browser. Closing
that gap is the theme of M10, and most of it is a *web-side* change — the compute
already exists and is wasm-clean.

A decisive enabler: **`ohmforge-fmt` already ships a full SPICE importer**,
`spice::import` / `import_with_notes` (`crates/ohmforge-fmt/src/spice.rs:299`/`:313`),
that parses R/C/L/V/I **plus `.model`-backed D/Q/M** into a `Circuit`. `ohmforge-fmt`
depends only on `ohmforge-core` (`crates/ohmforge-fmt/Cargo.toml`), which the web crate
already compiles against on `wasm32` — so this parser is reachable from the browser today
with **zero new dependencies and zero changes to other crates**.

---

## Ranked proposals

### 1. Unlock diodes / current sources / transistors in the browser via the existing SPICE importer — (M)

**What.** Add a second parse entry point to `run()` that accepts the richer SPICE deck
and routes through `ohmforge_fmt::spice::import` instead of `netlist::parse`, then expose
it as a new `simulate_spice(deck, duration, step)` binding alongside the existing
`simulate`. Keep R/C/L/V netlists working unchanged (either auto-detect a `.model`/`D`/`Q`/`M`
line, or add an explicit mode toggle in the demo panel). Widen the web-side validators
(`kind_label`, `src/lib.rs:304`) in lockstep so the parts form and canvas stop rejecting
`D`/`Q`/`M`/`I` prefixes.

**Why.** This is the headline limitation and the single highest-leverage change on the
track. The engine already solves diode/BJT/MOSFET networks (`graph.rs:32`–`:54`); the only
reason the browser can't is that `classify()` is R/C/L/V-locked
(`crates/ohmforge-fmt/src/netlist.rs:137`). The SPICE importer that lifts those parts is
already wasm-reachable (`spice.rs:299`, core-only deps), so a rectifier, a BJT amp or a
MOSFET switch becomes browser-simulatable with **no cross-crate code and no new deps** —
it turns the demo from "linear-RC toy" into a real analog playground.

**Risk.** Low–Medium. The DC/transient path is solid, but **the SPICE importer sets
DC-valued sources only** — a grep of `spice.rs` finds no `SINE`/`PULSE`/`Waveform`
stimulus parsing (that lives in the shell builder, `crates/ohmforge-shell/src/lib.rs:841`).
So a diode *rectifier* driven by a time-varying source won't rectify from a plain imported
deck until waveform stimulus is parsed (see #2). Diode/BJT/MOSFET **DC and op-point** work
immediately; nonlinear transient convergence should be spot-checked (the engine handles it,
but the browser has never exercised it).

**Cross-crate coordination.** *None required for the core win* — `spice::import` is used
as-is. **Flag:** time-varying source stimulus (`SINE`/`PULSE`/`PWL`) for imported decks
needs waveform parsing added to `ohmforge-fmt` (owned by the fmt/sim track). Propose it
there; the web track consumes it when ready.

**Acceptance gate (host-testable).** New `#[test]` in `src/lib.rs`: a diode-clamp deck
with a `.model D` card op-points through the new path and pins the clamped node near one
diode drop, e.g.
```rust
let deck = ".model Dm D (IS=2.52e-9)\nV1 in gnd 5\nR1 in a 1k\nD1 a gnd Dm\n";
let r = run_spice(deck, 1e-3, Some(1e-5)).unwrap();
// node `a` sits near a forward diode drop (~0.4–0.8 V), not at the 5 V rail.
```
Plus an existing R/C/L/V netlist still simulates through the unchanged path (no regression).

---

### 2. In-browser AC / Bode analysis — (M)

**What.** Add an `ac(deck, f_start, f_stop, points_per_decade)` binding that calls
`Engine::run` with `Analysis::Ac { .. }` (`crates/ohmforge-sim/src/lib.rs:47`), reads the
complex per-node signals (`sig.complex()`, used at `crates/ohmforge-sim/src/lib.rs:5031`),
and serializes magnitude (dB) + phase (deg) vs. frequency as JSON. Render a log-x Bode plot
in the demo panel reusing the existing hand-rolled SVG/scope style (`www/index.html:118`,
canvas SVG at `:258`).

**Why.** The engine already computes AC/Bode, noise and S-params
(`Analysis`, `sim/src/lib.rs:37`–`:93`; `SParams` at `:3130`), yet the browser only ever
asks for op-point + transient (`src/lib.rs:50`–`:66`). Frequency-domain response is the
canonical "show me it's a real EDA tool" demo (an RC low-pass Bode knee, a resonant LC
peak) and it is nearly free on the compute side — the work is a binding + a plot.

**Risk.** Medium. AC needs a source marked as an AC drive — the engine keys off
`Waveform::Sine` (`sim/src/lib.rs:46`, `:467`) — but neither `netlist::parse` nor
`spice::import` sets a waveform, so a plain deck has no 1∠0 stimulus to sweep. Options:
(a) a web-side convention (mark the first `V` source as the AC drive) via a direct
`Circuit` edit after import, or (b) depend on the waveform-parsing work flagged in #1.
Bode plotting itself is low-risk (reuses existing SVG patterns).

**Cross-crate coordination.** None for the solve. **Flag (shared with #1):** clean
AC-drive marking is best done by waveform parsing in `ohmforge-fmt` (fmt/sim track). The
web track can ship a pragmatic "first V source is the AC stimulus" rule meanwhile, entirely
in `src/lib.rs`.

**Acceptance gate.** Host `#[test]`: sweep the RC low-pass and assert the magnitude is
≈ −3 dB within tolerance at `f = 1/(2πRC)` and rolls off ≈ −20 dB/decade above it.

---

### 3. PWA / offline app shell — (S)

**What.** Add a `manifest.json` and a small hand-rolled service worker to `www/` that
precaches `index.html`, `style.css`, the SVGs, and `pkg/ohmforge_web.js` +
`pkg/ohmforge_web_bg.wasm`, serving them cache-first. Register it from the existing inline
module (`www/index.html:380`).

**Why.** The README already claims the page "renders identically offline"
(`www/README.md`), but that is only true for a page already in memory — there is **no
service worker or manifest** anywhere in `www/` (verified), so a reload with no network
fails to fetch the wasm and the app dies. A simulator that runs *entirely client-side*
(the wasm is self-contained, no CDNs, no backend) is the ideal PWA: once cached it is a
genuine offline EDA tool and an installable app. This honours a claim already made.

**Risk.** Low. Service workers are standard; the only real cost is cache-version
discipline so a new wasm bundle isn't served stale (bump a cache name keyed to the build).
Dependency-light and fully in the house style (hand-rolled JS, no framework).

**Cross-crate coordination.** None — pure `www/` assets.

**Acceptance gate (host-testable).** A static check in CI asserting `www/manifest.json`
and the service worker exist and that the SW's precache list names the wasm + JS bundle
paths. (Full offline behaviour is a manual/e2e check; the precache-manifest assertion is
the host-testable proxy.)

---

### 4. Canvas editor: richer palette, part inspector, and labels — (M)

**What.** Once #1 lands, extend `Schematic`/`CanvasPart` (`src/lib.rs:1617`–`:1651`) and
the toolbar (`www/index.html:239`–`:248`) with diode and current-source tools and a ground
already present; add on-canvas **value + reference labels** next to each part and a small
**inspector** to retune a selected part's value/model inline (the `set_value`/`move_part`
plumbing already exists, `src/lib.rs:1706`–`:1722`). Lift the hardcoded "R/C/L/V + ground"
restriction documented at `src/lib.rs:1593`–`:1596` and enforced via `kind_label` in
`place_part` (`src/lib.rs:1678`).

**Why.** The canvas is the most tactile surface but is deliberately capped at R/C/L/V
because "a canvas part the engine could not simulate would be a lie" (`src/lib.rs:1594`).
#1 removes that reason, so the palette can honestly grow. Labels and an inspector are the
biggest UX gaps — today you cannot see a part's value on the grid or edit it without the
separate parts form.

**Risk.** Medium. Strictly gated on #1 (the netlist a richer canvas emits must actually
simulate). SVG label layout and hit-testing add front-end complexity; keep it incremental.

**Cross-crate coordination.** None beyond inheriting #1's status.

**Acceptance gate.** Host `#[test]` placing a diode on the canvas and asserting
`Schematic::to_netlist()` emits a `D…` line that `run_spice` simulates; plus a
`to_blob`/`from_blob` byte-exact round-trip for the new part kinds (mirrors the existing
`canvas_place_wire_remove_round_trips` test, `src/lib.rs:3011`).

---

### 5. Performance: shrink the wasm bundle — (S)

**What.** Add a wasm-targeted release profile and a `wasm-opt -Oz` pass to the documented
build (`www/README.md` step 1). The bundle is **320 KB** (`pkg/ohmforge_web_bg.wasm`) +
26 KB JS; the workspace release profile is `opt-level = 3`, `lto = "thin"`,
`panic = "unwind"` (`Cargo.toml:64`–`:72`) — tuned for the native solver, not for size.
A size-oriented wasm profile (`opt-level = "z"`/`"s"`, `panic = "abort"` for the wasm
build only, `strip`) plus `wasm-opt` typically cuts a wasm this size by 30–50%.

**Why.** First-paint-to-interactive is gated on downloading + compiling the wasm; it is the
one asset the PWA (#3) must cache and every first-time visitor must fetch. Smaller wasm
means faster cold start and a smaller offline cache.

**Risk.** Low. `panic = "abort"` must stay **scoped to the wasm build only** — the
workspace deliberately uses `unwind` so the TUI/AI hosts can recover from a solver panic
(`Cargo.toml:68`–`:72`), so this cannot become a workspace-wide change. Measure before/after;
guard against a profile that regresses solve speed in the browser.

**Cross-crate coordination.** None if done as a crate-local/`wasm32`-scoped profile +
build-doc change. **Flag:** if the knob has to live in the root `[profile]`, coordinate so
it is `cfg`-scoped and never flips the native hosts to `abort`.

**Acceptance gate.** A CI size budget: `ohmforge_web_bg.wasm` must stay under a committed
byte ceiling (e.g. assert the built artifact is ≤ 0.7× the current 320 KB). Byte-measurable
on the host.

---

### 6. Hosted co-edit: a WebSocket transport client + deploy story — (L)

**What.** Add a hand-rolled WebSocket `CoEdit` transport alongside the existing
`BroadcastChannel` one (`src/lib.rs:2153`). The lib.rs comment already pinpoints the seam:
"would slot in exactly here by swapping the transport for a socket whose `send`/`onmessage`
carry the very same op tokens" (`src/lib.rs:2131`–`:2135`). The pure `Relay` router
(`src/lib.rs:1148`) and native `server` (`src/lib.rs:1240`, `--features relay`) already
exist and are host-tested — the browser just has no way to *reach* them. Add the JS/wasm
client binding, a `JOIN <room>` handshake, and a documented deploy (the `relay` binary +
a reverse proxy / TLS note).

**Why.** Cross-machine co-edit is *built but unreachable*: the relay server and its routing
are done, yet the only shipping client is `BroadcastChannel`, which is same-origin tabs on
one machine. One WebSocket binding turns a dead feature into a real shared-session product.

**Risk.** Medium–High. The client binding is small and matches the frame protocol
(`Frame`, `src/lib.rs:1078`). The weight is **operational, not code**: hosting a public
relay introduces an availability/abuse surface (room squatting, flooding, unbounded
connections) the current server doesn't guard, plus TLS/`wss://` and a deploy target — a
real departure from the "static files, no backend" posture. Needs a product decision on
whether ohmforge wants to *run* a service.

**Cross-crate coordination.** None (relay already lives in this crate). The coordination is
with ops/infra and project direction, not another code track.

**Acceptance gate.** Extend the existing relay round-trip test (`src/lib.rs:2878`,
`two_clients_in_a_room_converge_via_the_relay`) with a host-level loopback: start the native
`server` on `127.0.0.1`, connect two raw WebSocket clients, exchange one `EditOp` token via
`OP`, and assert both converge to an identical netlist — provable on the host with the
`relay` feature, no browser needed.

---

## Dependency map

- **#1 is the keystone**: #4 (canvas palette) is gated on it, and #2 (AC drive) shares the
  same waveform-parsing flag.
- **#3 and #5 are independent and cheap** — ship them any time; #5 makes #3's cache smaller.
- **#6 is self-contained but heavy** (ops decision), independent of the rest.
- The **only cross-crate asks** are to the fmt/sim track: time-varying source **waveform
  parsing** in `ohmforge-fmt` (unblocks real transient/AC for imported decks in #1/#2).
  Everything else is web-track-local and keeps the standing rules (wasm32-gated browser deps,
  clean default + wasm builds, hand-rolled/dependency-light).
