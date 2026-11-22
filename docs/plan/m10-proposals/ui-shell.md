# M10 proposals — TUI, shell & instruments

Principal-engineer + UX-lead review of `ohmforge-tui`, `ohmforge-shell`, and
`ohmforge-instruments` at v0.8.1 (M0–M9 complete). Read-only study of the live
code; this document proposes work, it does not do it.

## What already works well (so we don't regress it)

Before the gaps, an honest accounting of the parts that are genuinely good and
should be preserved as-is:

- **The pure/tty-free split is real and complete.** `App` is a state machine over
  `AppEvent` with `render(w,h) -> String` (`crates/ohmforge-tui/src/app.rs:463`,
  `:1153`); the only terminal code is the thin `run` wrapper
  (`crates/ohmforge-tui/src/run.rs:41`), whose translators `map_key`/`map_mouse`
  are themselves pure and tested. Every feature below can be gated by a data
  assertion with no tty. Keep this discipline.
- **One mutation path.** Live tune, mouse capture, edit-mode prompts, and typed
  shell lines all lower to the same `add/wire/remove/set` grammar via
  `EditAction::to_command` (`app.rs:258`) → `Backend::execute`. The circuit the
  schematic draws is the one the sim runs. Do not add a second edit path.
- **Shell grammar breadth and error quality.** `cmd_run` reaches tran/op/ac/dc/
  noise/mc/tf/sparams (`crates/ohmforge-shell/src/lib.rs:1051`); `ShellError`
  messages are specific and actionable (`:1687`). The absurd-duration *warning*
  rather than failure (`:1182`) is exactly the right instinct.
- **Scripting.** Loops, `$vars`, nesting, and byte-exact deterministic replay
  (`crates/ohmforge-shell/src/script.rs`) with an expansion budget guard.
- **Instrument fidelity already exists — it just isn't wired to the live app.**
  `Oscilloscope` has edge trigger + sub-sample interpolation, timebase, and
  `Cursors`/`CursorReadout` (Δt/Δv/freq) (`oscilloscope.rs:36`–`:116`); `Bode`
  computes the −3 dB corner and passband peak (`bode.rs:50`,`:163`); `SmithChart`
  already renders a *locus* from `&[Complex]` (`smith.rs:102`); FFT and DMM are
  fully formed. This is the crux: much of the M10 "instrument" value is
  *integration*, not new math.
- **Efficient render loop:** frame-diffed repaint and animation-gated ticks
  (`run.rs:69`,`:91`) mean an idle screen never churns.

The recurring theme of the gaps: the dockable live app only has three panels
(`PanelId` = Schematic/Scope/Shell, `app.rs:99`) and its scope pane draws with the
bare `Plot` widget (`app.rs:1288`), so none of the real instrument intelligence
reaches the interactive user. The dependency `ohmforge-tui → ohmforge-instruments`
**already exists and is legal** (`crates/ohmforge-tui/Cargo.toml:18`; the standing
rule only forbids the reverse), so wiring instruments into panels needs no new
architecture.

---

## Ranked proposals

### 1. Wire the real `Oscilloscope` into the live scope pane (cursors, trigger, measurements)

**What + why.** The live scope pane re-plots cached transient series with the
dependency-light `Plot` widget (`app.rs:1267` `scope_body`, `:1288`
`replot_transient`) and a transport line that shows only `t` and each signal's
value at the playhead (`app.rs:1317`). Meanwhile `ohmforge-instruments` already
ships a full `Oscilloscope` with an edge/level `Trigger`, a `Timebase`, and
`Cursors` yielding a `CursorReadout` of Δt/Δv and implied `1/Δt` frequency
(`oscilloscope.rs:36`–`:116`), plus the DMM's `measure` → Vpp/mean/RMS
(`dmm.rs:76`). None of it is reachable interactively — the `use` list in `app.rs`
(`:35`–`:45`) names no instrument. This is the single biggest UX-depth +
fidelity win available, and it is mostly plumbing: feed the playback's
`signals()` into `Oscilloscope`, add two draggable/steppable cursors and a
toggleable trigger to the scope transport sub-state, and render the readout line.
Gives bench users the Δt-for-frequency and Vpp measurements they expect.

**Effort:** M. **Risk:** Low–medium — the instrument logic is tested already; risk
is confined to pane-height budgeting (the readout steals rows from an already
tight `replot_transient` chrome budget) and new transport keys colliding with the
existing space/`,`/`.`/arrows set (`app.rs:518`).

**Cross-crate coordination:** none structurally — `ohmforge-tui` already depends on
`ohmforge-instruments`. Keep the panel calling `Oscilloscope::render`/cursor APIs;
do not pull scope state back into the shell.

**Acceptance gate (no tty):** drive `App` with the existing `PlayBackend`
(`app.rs:2237`, a known 0→3 V / 0→1.5 V ramp), place cursors at two sample
indices, and assert a pure `App::cursor_readout() -> CursorReadout` returns the
exact `dt`, `dv`, and `freq` for that span; separately assert `render(w,h)`
contains the formatted measurement line. All numeric, no terminal.

---

### 2. Sweep `run sparams` into an S11 locus on the Smith chart (the noted follow-up)

**What + why.** `SmithChart::render` takes `&[Complex]` and already plots a
multi-point locus (`smith.rs:102`,`:125`), but `run_sparams` solves at exactly one
frequency and hands it a one-element slice: `SmithChart::new(...).render(&[sp.s11])`
(`crates/ohmforge-shell/src/lib.rs:1385`). `Engine::s_parameters` takes a scalar
`freq` (`crates/ohmforge-sim/src/lib.rs:3169`), so a frequency sweep is simply
calling it in a loop and collecting each `S11`. Extend the grammar to
`run sparams <p1> <p2> [z0] [f0] [f1] [ppd]`: when a stop frequency is given,
solve across the decade sweep and render the full Γ trajectory (matching
circle/arc you can read off the grid) instead of a lone dot — the behaviour the
panel was built for and the explicitly-noted thin follow-up. The single-frequency
form stays as-is.

**Effort:** S. **Risk:** Low — additive grammar arm; existing `["sparams", p1, p2]`
/ `[.. z0]` / `[.. z0 f]` arms (`lib.rs:1084`–`:1093`) are untouched.

**Cross-crate coordination:** shell-only; consumes the existing
`ohmforge-sim` `s_parameters` and the existing `ohmforge-instruments`
`SmithChart`. No signature changes needed. (If a *live* dockable Smith panel is
also wanted, that rides on proposal #5's panel registry; keep it out of scope
here.)

**Acceptance gate (no tty):** build a known mismatched load (e.g. a series R into
ground off a port), run the swept form over a decade, and assert the shell output
contains ≥ N `●` point glyphs (one per swept frequency) and that two chosen
frequencies map to the `SmithChart::cell_of` cells their computed Γ predict —
asserting the locus, not a single point.

---

### 3. In-TUI help / keymap overlay (`?` or `F1`)

**What + why.** The live app has a rich hidden keymap — Tab/BackTab focus,
Ctrl-P palette, Ctrl-arrows resize (`app.rs:483`–`:496`), `e` edit mode,
`a/w/d/+/-` in edit (`app.rs:1000`–`:1005`), space/`,`/`.`/arrows scope transport
(`app.rs:518`) — but the only on-screen hint is the edit-mode line
`[a]dd [w]ire [d]el [+/-]tune [esc]` (`app.rs:1254`). A new user cannot discover
focus cycling, resize, capture, or transport without reading source. The command
palette (`COMMANDS`, `app.rs:321`) lists actions but not their keys. Add a
toggleable, centred help overlay (reuse the `palette_rect`/`blit` overlay
machinery, `app.rs:1363`,`:1463`) listing every binding grouped by context, and a
`help` palette entry. Cheap, high-discoverability.

**Effort:** S. **Risk:** Very low — a pure render overlay plus one boolean in
`App`; no mutation path touched. Keep the binding list in one table so it cannot
drift from the handlers (ideally the handlers read labels from the same table).

**Cross-crate coordination:** none — entirely within `ohmforge-tui`.

**Acceptance gate (no tty):** toggle help on via the event the `?` key maps to,
then assert `render(80,24)` contains representative rows ("Tab", "Ctrl-P",
"edit", "space") and that toggling off removes them; assert `is_animating()`
stays false so the overlay never forces repaints.

---

### 4. Surface FFT and DMM in the shell (`run fft`, `measure`) + fix completion staleness

**What + why.** Two fully-built instruments have no shell verb: `magnitude_spectrum`/
`peak_bin` (`fft.rs:98`,`:190`) and `measure` → Vpp/mean/RMS/frequency
(`dmm.rs:76`). A bench user who can `run ac` cannot ask for a spectrum of a
transient or a numeric measurement of a probe. Add `run fft <node>` (render the
`Spectrum` of the cached/last transient at that node) and `measure <node> [mode]`
(print the DMM reading). Separately, two concrete completion defects: `RUN_ANALYSES`
(`crates/ohmforge/src/lib.rs:570`) lists tran/op/ac/dc/noise/mc/tf but **omits
`sparams`**, which `cmd_run` has supported since it was added (`lib.rs:1084`) — so
Tab never offers it; and `set`'s sub-verbs (`sine`/`pulse`/`dc`/`tol`/`tempco`/
`temp`, `lib.rs:758`–`:771`) get no completion at all. Add `sparams`/`fft`/
`measure` to the keyword pool and a `set`-context branch in `complete`
(`crates/ohmforge/src/lib.rs:628`).

**Effort:** S (completion fix) + M (`run fft`/`measure` rendering). **Risk:** Low —
additive verbs and a stale-list fix; `measure`'s only subtlety is choosing which
cached signal to read (reuse `signal_name`, `lib.rs:1506`).

**Cross-crate coordination:** shell consumes `ohmforge-instruments` FFT/DMM (already
a dependency, `lib.rs:50`). Completion lives in the `ohmforge` binary crate and
only reads `Shell::COMMANDS`/`node_names`/`part_refs`.

**Acceptance gate (no tty):** feed a pure sine via `set V1 sine`, `run tran` with a
fixed step, then `run fft out` and assert the peak bin is at the drive frequency
(±1 bin); `measure out` and assert Vpp ≈ 2·amplitude. For completion, call the
`Completer` directly on `"run "` and assert `"sparams"`/`"fft"` appear, and on
`"set R1 "` assert `"sine"`/`"pulse"` appear.

---

### 5. A fourth, swappable instrument panel (dockable Bode / FFT / Smith / DMM)

**What + why.** `PanelId` is a fixed three-variant enum and `PANELS` a fixed array
(`app.rs:99`,`:109`); `geometry` hard-codes a 2-up-over-1 tiling (`app.rs:1108`).
AC/noise/sparams results only ever appear as scrollback text in the shell log,
never as a persistent live panel, so you cannot watch a Bode plot while tuning a
component with `+/-`. Introduce an "instrument" panel slot whose *contents* the
user picks (Bode / FFT / Smith / DMM / eye) from the palette, rendered from the
last matching `run`. This is the structural home for a live Smith panel (the other
half of #2) and for live measurements. It is the largest item because it touches
layout math, focus cycling, and the palette.

**Effort:** L. **Risk:** Medium — `geometry` and every `geometry()[i]` index
consumer (mouse hit-testing `app.rs:600`, capture origin `app.rs:933`) assume
exactly three rects; generalising tiling must preserve the "tile exactly, no gaps"
invariant the renderer relies on (`app.rs:1156`). Phase it: add the slot with a
fixed position first, make tiling dynamic second.

**Cross-crate coordination:** `ohmforge-tui` renders via `ohmforge-instruments`
(legal, already depended on). The backend must expose the last AC/FFT/sparams
`Signal`s to the panel — extend the `Backend` trait with a defaulted accessor
(mirroring `transient()`, `app.rs:92`) so the mock stays trivial and the arrow
direction is unchanged.

**Acceptance gate (no tty):** with a real `Shell` backend, `run ac` then select the
Bode instrument into the new slot; assert `render` places a Bode frame (contains
its `f(-3dB)` readout from `bode.rs:98`) in the fourth pane's rect and that focus
cycling now visits four panels in order.

---

### 6. More math channels and a colourised, legended multi-channel scope

**What + why.** Math channels are limited to `Add`/`Sub`/`Mul` of two signals
(`math.rs:19`), and there is no shell verb to create one — the capability is
library-only. The live scope renders every channel in the same monochrome braille
with no legend and no per-channel identity (`app.rs:1300`, `overlay.color(false)`
at `app.rs:1206`), so two probes are hard to tell apart. Add (a) a `math` verb /
instrument op set extended with divide, scale, and derivative/integral (useful for
V·I → power and di/dt), and (b) a per-channel glyph/label legend in the scope pane,
with optional colour behind a capability flag kept out of the pure render string.

**Effort:** M. **Risk:** Low–medium — new math ops are pure and unit-testable;
colour must not leak into the deterministic `render()` contract (keep it a
post-process in `run.rs`, or encode channel identity with distinct glyphs so the
golden tests stay colour-free).

**Cross-crate coordination:** math lives in `ohmforge-instruments`; the shell gains
a thin verb; the TUI legend is `ohmforge-tui`-local. No arrow changes.

**Acceptance gate (no tty):** `math_channel` divide/derivative return exact values
on a known ramp (e.g. d/dt of a line is its constant slope); and a two-probe
`render` contains both channel labels in a legend row with distinct leading
glyphs, asserted as substrings.

---

## Suggested sequencing

- **Land first (high value / low risk):** #2 (Smith locus — the noted follow-up),
  #3 (help overlay), and the #4 completion fix. All small, all independently
  shippable, no layout churn.
- **Then the flagship:** #1 (Oscilloscope in the scope pane) — the headline M10
  UX+fidelity gain, and the prerequisite intelligence for #5.
- **Then structural:** #5 (swappable instrument panel), which gives #2 and #1 a
  permanent live home, followed by #6 polish.
