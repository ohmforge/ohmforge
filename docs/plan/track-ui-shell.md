# Track design: **ui-shell** (`ohmforge-tui`, `ohmforge-shell`, `ohmforge-instruments`)

> How a human drives ohmforge: the `eesh` command language, the terminal UI, and the LabVIEW-style virtual instruments. Everything here mutates the same `ohmforge-core` graph and consumes `Signal`s — "type it," "click it," and "ask for it" all lower to the same API.
>
> Checkpoints: `CP-UX-M0.1 … CP-UX-M2.4`. Full shell spec: [`../SHELL.md`](../SHELL.md). Contracts: [`README.md`](README.md#inter-track-contracts).

## Scope

| Crate | Owns |
|---|---|
| `ohmforge-shell` | `eesh`: the command grammar, parser, the **`Command` AST**, tab-completion, history, and (M1) the embedded scripting engine. |
| `ohmforge-tui` | The terminal UI (`ratatui` + `crossterm`): schematic renderer, braille waveform plots, dockable panels, mouse placement, themes, help. |
| `ohmforge-instruments` | Virtual instruments over the engines: scope, DMM, function generator, logic analyzer, PSU, Bode/spectrum — presented as front panels. |

Until `ohmforge-sim` produces real signals, this track develops against a **stub `Signal` producer** (a sine/RC generator) so the render and instrument work isn't blocked on the solver.

---

## `ohmforge-shell` — the command language

### The `Command` AST (contract)

The parser produces a `Command` enum that the `of` binary dispatches. **The AI emits the same AST**, so anything the copilot does is something you could have typed — and every action is inspectable and replayable.

```rust
pub enum Command {
    Add { kind: PartSpec, reference: String, value: Option<Value>, nodes: Vec<NetRef>, opts: Opts },
    Wire(NetRef, NetRef),
    Set { target: ParamRef, value: Value },
    Probe(SignalRef),
    Analysis(ohmforge_sim::Analysis),        // run/op/ac/sweep/... lower to this
    Show(Selector), List(Kind),
    Board(BoardCmd), Flash(FlashCmd), Mcu(McuCmd),
    Instrument(InstrumentCmd),         // scope/dmm/funcgen/psu/la/decode
    Ai(AiCmd),                         // ask/design/debug/firmware/explain/apply/reject
    Source(PathBuf), Save(Option<PathBuf>), Open(PathBuf), Undo, Redo,
    View(ViewCmd), Help(Option<String>),
}
```

### Parsing & UX (`CP-UX-M0.1`)

- Terse, positional-with-options grammar: `add r R1 4k7 a b`, `set R1.value 10k`, `add led D1 anode=a cathode=b`.
- Values parse through `ohmforge-core::value` everywhere.
- **`run` is the transient sim** (`run 3ms`); **`source <file.eesh>`** runs a script — the two are distinct verbs (this disambiguation was a doc-review fix).
- *Gate:* parser unit tests; a scripted session builds the expected `Circuit` graph.

### Scripting + replay (`CP-UX-M1.5`, milestone **M1**)

Embed a small scripting engine (candidate: `rhai`) for loops/params over commands; a session is itself a script, so **deterministic replay** reproduces the exact circuit and results. Great for tutorials, regression tests, and bug reports. *Gate:* a param-sweep script runs and replays identically. (Snapshots/undo are separate, at `CP-UX-M2.4`/M2.)

### Completion & help

Tab-completes commands, part types, existing refs/nodes, and `k=v` keys; `?` shows valid continuations; errors are specific ("node `ou` unknown — did you mean `out`?").

---

## `ohmforge-tui` — the terminal UI

**`ratatui` + `crossterm`** for a portable, SSH-friendly UI. Rendering is **decoupled from simulation** via a snapshot/observer channel, so a slow redraw never stalls the solver (and vice-versa).

### Schematic view (`CP-UX-M0.2`)

Unicode box-drawing for wires + ASCII component glyphs, named nodes. Start with a list/net view; grow toward a routed 2-D schematic. *Gate:* renders a built circuit; layout snapshot test.

### Braille waveform plot (`CP-UX-M0.3`) — first pixels

Braille sub-cell plotting packs a 2×4 grid of dots per character cell → high vertical resolution in a terminal. A `Plot` widget maps a `Signal`'s `(x,y)` to braille cells with autoscale and cursors; multi-channel stacks come at M1. *Gate:* plotting a known sine matches a golden ASCII snapshot. **This is the M0 "see a wave" moment.**

### Probe → plot pipeline (`CP-UX-M0.4`)

`probe v(out)` then `run` then `scope` consumes the transient `Signal` and shows the RC curve from `CP-SIM-M0.4`. This is the end-to-end M0 demo: type a circuit → simulate → see it.

### Interaction (`CP-UX-M1.3`, `M1.4`)

- **Live overlay:** color nodes by voltage, animate current direction/magnitude on wires — *watch the circuit work*. Values come from the DC op-point / transient frame.
- Dockable/resizable panels (schematic, scope, shell, AI chat, parts palette, instruments), a command palette, keyboard-first with mouse placement/wiring.

---

## `ohmforge-instruments` — LabVIEW-flavored front panels

Instruments are **consumers of `Signal`s + controllers of sources**; the TUI renders their panels.

| Instrument | Checkpoint | Notes |
|---|---|---|
| Oscilloscope | `CP-UX-M1.1` | multi-channel, edge/level trigger (lock a periodic signal), timebase, cursors, math (A−B, FFT) |
| Multimeter | `CP-UX-M1.2` | V/I/R/continuity/diode at a probe point |
| Function generator | `CP-UX-M1.2` | sine/square/tri/PWM/arbitrary **as a source into the circuit** |
| Bench PSU | `CP-UX-M1.2` | programmable, current-limited |
| Logic analyzer + decode | `CP-UX-M2.1` | multi-bit capture; UART/SPI/I²C protocol decode |
| Bode/spectrum | `CP-UX-M2.3` | from AC/noise analyses; scope math/FFT channels |

Breadboard view (`CP-UX-M2.2`) renders the circuit as a breadboard layout — the educational "what it looks like" view. The longer-term **dataflow patcher** (M3) wires instruments + processing blocks into reusable virtual benches.

---

## Testing strategy

- **Snapshot tests** for rendering (schematic, braille plots, panels) against golden ASCII — deterministic by construction.
- **Parser tests** for the full `Command` grammar + error suggestions.
- **Interaction/event tests** for panel docking, trigger locking, cursor math.
- Instrument tests run against the **stub signal producer** first, then real `ohmforge-sim` signals once available.

---

## State & next action

- **Depends on:** the `Signal` type (`CP-SIM-M0.5`) for real data; can start now against the stub producer. The `Command` AST is this track's contract — coordinate additions with `ai-fmt` (the AI emits it).
- **Current checkpoint:** `CP-UX-M0.1` — the eesh parser + core commands (`add`/`wire`/`probe`/`run`/`set`/`show`/`help`) producing the `Command` AST.
- **Immediate next step:** define the `Command` AST + parser with tests (M0.1 gate), then `CP-UX-M0.3` (braille plot widget, golden-snapshot) so there are "first pixels" even before the solver, then wire `CP-UX-M0.4` once `CP-SIM-M0.4` lands.
