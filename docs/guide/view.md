# Debugging things for the view

This guide is about *looking* at a circuit: the interactive `of tui` and the
virtual instruments you drive from it. Where [getting started](../tutorials/getting-started.md)
built and simulated a circuit, this is how you *inspect* one — trigger a scope
on an edge, drop cursors on it, read a meter, sweep a Bode plot, and decode a
protocol bus — to find out why it does what it does.

All command lists, key bindings, and numbers below are taken from the current
implementation and its tests.

## The interactive TUI (`of tui`)

```console
$ cargo run -p ohmforge tui
```

takes over the terminal (raw mode, alternate screen, mouse capture) and runs the
live app until you quit. It is a pure state machine underneath — focus, resize,
the palette and the shell line are all driven by abstract events — so what you
see is exactly what the unit tests assert.

### Layout

Three docked panels tile the screen:

```text
┌▸ schematic ───────┐┌─ scope ───────────┐
│ V1  V-source  5V  ││  tran            ⡠⠊│
│ R1  Resistor 4.7k ││     ┤        ⡠⠔⠊⠁  │
└───────────────────┘└───────────────────┘
┌─ shell ─────────────────────────────────┐
│ > run tran 1m                            │
│ scope updated                            │
└─────────────────────────────────────────┘
```

- **schematic** (top-left) — the component list, or the op-point overlay when
  toggled. The focused panel is marked with a `▸` in its title.
- **scope** (top-right) — the braille frame from your most recent `run`.
- **shell** (full width, bottom) — a command log plus the input line (`> …`).

### Keys

| Key | Action |
|---|---|
| `Tab` / `Shift-Tab` | focus the next / previous panel (schematic → scope → shell → wrap) |
| `Ctrl-←` / `Ctrl-→` | move the vertical split (shrink / grow the left column) |
| `Ctrl-↑` / `Ctrl-↓` | move the horizontal split (grow / shrink the shell) |
| `Ctrl-P` | open the command palette |
| `Ctrl-C` | quit from anywhere |

With the **shell** focused, printable keys edit the input line, `Backspace`
deletes, and `Enter` runs the line through the shell backend. If the output is a
scope frame (a `run tran`), it lands in the **scope** panel and the log just
notes `scope updated`; any other output is appended to the log.

**Mouse.** A left click focuses whichever panel it lands in. The scroll wheel
moves the selection while the palette is open.

### The command palette

`Ctrl-P` opens a centred, fuzzy-filtered palette. Type to narrow it (whitespace
is ignored, so `foc sch` matches *focus schematic*), `↑`/`↓` to move the
selection, `Enter` to run it, `Esc` to close. The commands are:

| Command | What it does |
|---|---|
| `focus schematic` / `focus scope` / `focus shell` | jump focus to a panel |
| `grow panel` / `shrink panel` | resize the focused panel at its neighbours' expense |
| `toggle overlay` | switch the left panel between the schematic list and the op-point overlay |
| `clear log` | empty the shell log |
| `quit` | exit the app |

### A debugging loop

Type `add …`/`probe …` in the shell pane to build; `run tran …` to push a fresh
frame into the scope; `toggle overlay` to colour the schematic by node voltage
and spot a rail that sits at the wrong level; `Ctrl-↓` to give the scope more
room while you read a waveform. Because the shell backend is the same `eesh`
engine from the tutorial, `undo`/`checkpoint`/`restore` work here too.

### The op-point overlay

`toggle overlay` (or `of amp`) replaces the component list with the DC state
painted onto it. For a common-emitter amplifier, `of amp` renders:

```text
┌─ ce-amp op-point ────────────────────────────────────────────────────┐
│ V1  V-source  12V    [n1=12V, gnd=0V]                                │
│ R1  Resistor  47kΩ   [n1=12V, n2=1.996V]                             │
│ R2  Resistor  10kΩ   [n2=1.996V, gnd=0V]                             │
│ RC  Resistor  2.2kΩ  [n1=12V, n3=9.094V]                             │
│ RE  Resistor  1kΩ    [n4=1.334V, gnd=0V]                             │
│ Q1  BJT              [n3=9.094V, n2=1.996V, n4=1.334V]               │
└──────────────────────────────────────────────────────────────────────┘

  Vb=2.00 V  Vc=9.09 V  Ve=1.33 V   →  Vbe=0.662 V, Ic≈1.33 mA (active region)
```

Live, the net annotations are colour-coded by level (ground dim; higher rails
ramp toward red), so a biasing mistake — a base sitting at a rail, a collector
pinned to Vcc — is visible without reading a single number. The width maths is
done on *visible* characters, so the colours never push a line past its cell
width.

## The virtual instruments

The instruments live in the `ohmforge-instruments` crate: each *consumes* a `Signal`
(or controls a source) and renders a deterministic, dependency-light front panel
to a string, so they are testable with no live terminal and look identical in the
TUI, a demo, and a test. The `of bode` and `of amp` sub-commands are runnable
front ends today; the rest are driven programmatically and from the TUI.

### Oscilloscope — trigger and cursors

A free-running scope starts each capture at an arbitrary phase, so a periodic
waveform appears to drift. A **trigger** fixes that: `Trigger::Edge { source,
level, slope }` finds the first place the source channel crosses `level` with the
chosen `slope` (`Rising`/`Falling`), interpolates the sub-sample crossing time,
and anchors the window there. Two captures of the same periodic signal at
different phases then render **byte-for-byte identically** — that stability is
what makes a waveform readable.

**Cursors** measure between two points on the (trigger-relative) timebase.
`Cursors { t1, t2, source }` reports a readout line:

```text
  Δt=…  1/Δt=…  Δv=…
```

For a 1 kHz sine, cursors one period apart read **Δt = 1 ms**, **1/Δt = 1 kHz**,
and **Δv ≈ 0** (a full period returns to the same value); placed half a period
apart they read **2 kHz**. *Debugging tip:* trigger on the clock edge and park
cursors on two features to read a propagation delay or a period directly, instead
of eyeballing it.

**Math channels** derive a new trace from two others — `A−B`, `A+B`, `A·B` — with
`B` resampled onto `A`'s time grid, so a differential pair's difference plots like
a measured channel. (A product is reported dimensionless, since it mixes units.)

### Multimeter (DMM)

One pass over a signal yields every reading; the mode picks which one the panel
shows big:

| Mode | Reading |
|---|---|
| `DcVolts` | the mean (for a one-sample op-point, the node voltage itself) |
| `Rms` | true RMS, √mean(y²) |
| `PeakToPeak` | max − min |
| `Frequency` | from rising mean-crossings |

Verified against a 2 V sine: **RMS ≈ 1.414 V** (= 2/√2), **Vpp ≈ 4 V**, and a DC
offset comes back as the mean. *Debugging tip:* RMS vs. mean tells ripple from
level at a glance; the crossing-based frequency confirms an oscillator is running
at the rate you expect.

### Function generator

A stimulus source that synthesises a `Signal` you can feed the scope (or, later,
a live source): **sine / square / triangle / pulse**, with amplitude, frequency,
offset, duty (square/pulse) and phase. A 25 %-duty square spends a quarter of
each period high; a triangle rises from 0 through ±amplitude. *Debugging tip:*
drive a filter with a square and watch the scope to see its step and ringing;
switch to a sine sweep to read its response.

### Bench PSU

A programmable DC supply with a current limit, modelling the **CV/CC crossover**:

- **Constant voltage (CV)** while the load current stays under the limit — it
  holds `v_set`.
- **Constant current (CC)** once the load would pull more — it clamps the current
  at the limit and the terminal voltage falls to `I·R`.

Verified: 5 V / 1 A limit into 100 Ω → CV, 5 V at 50 mA; into 1 Ω → CC, 1 A at
1 V. A short reads 0 V at the limit; an open reads `v_set` at 0 A. *Debugging
tip:* if the rail sags below its set point, the panel's `CC` tag tells you the
load tripped the current limit rather than the supply failing.

### Bode, spectrum, and FFT

**`of bode`** renders an AC sweep as magnitude (dB) and phase (°) against a
**log** frequency axis with decade ticks, and reports the −3 dB corner. For an
RC low-pass (R = 1 k, C = 159 n):

```text
  |v(out)/v(in)| dB  —  log f, 10 Hz … 100 kHz
     -0dB ┤⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠓⠒⠒⠒⠲⠤⠤⣄⣀
          ┤                                  ⠈⠙⠒⠦⣄
          ┤                                      ⠈⠙⠲⣄
                                                     … (−20 dB/decade roll-off) …
          └────────────────────────────────────────────────────────────────────────
           1                                                                      5

  measured −3 dB near 1000 Hz   (analytic fc = 1001 Hz)
```

The **spectrum** view plots a one-sided magnitude spectrum (dB vs *linear*
frequency) with a peak readout and a configurable noise floor. It is fed by the
scope's **FFT** math channel: a hand-rolled radix-2 FFT that windows the record
(**rectangular** for integer-cycle tones, **Hann** to suppress leakage otherwise)
and zero-pads to the next power of two, returning bins from DC to Nyquist. An
8 Hz tone sampled at 64 Hz over 64 samples lands cleanly in a single bin.
*Debugging tip:* Bode (log axis, swept AC) answers "what is the frequency
response?"; the FFT/spectrum (linear axis, from a captured transient) answers
"what frequencies are actually present?" — use the latter to catch an unexpected
harmonic or an oscillation.

### Logic analyzer + protocol decode

The logic analyzer renders **digital** channels — a stack of 0/1/X timing
waveforms with vertical bars at level changes — and annotates decoded protocol
frames beneath the line that carries them. A real `Signal` becomes a channel by
thresholding (`DigitalChannel::from_signal`: ≥ threshold → high, non-finite → X).
Three transport decoders turn channels into frames, each degrading gracefully
(returning what it could recover, flagging framing errors) rather than panicking:

- **UART** — one line plus a baud; samples the mid-point of the start, data,
  parity and stop bits. Round-trips `"Hi!"` at 115200 8-N-1 with every frame
  cleanly framed; a wrong baud does *not* reproduce the message (so a garbled
  decode is a baud clue).
- **SPI** — clock + MOSI (+ optional CS); samples on the edge selected by
  `CPOL`/`CPHA` (rising when `CPOL == CPHA`, falling otherwise — the four modes).
  Mode-0 decodes a known byte stream exactly.
- **I²C** — SCL + SDA; detects START/STOP as SDA moving while SCL is high,
  samples bits on SCL's rising edge, and reads the 7-bit address + R/W, the ACK,
  and each data byte. A START/`0x48` write/`0xDE`,`0xAD`/STOP capture decodes to
  exactly those events.

*Debugging tip:* if a decode comes back as noise, suspect the *parameters* first
(baud, SPI mode, bit order) — the decoders are exact on a correct capture, so a
wrong setting is usually the cause, and the framing/ACK flags point at where the
bus actually broke.

## See also

- [`docs/tutorials/getting-started.md`](../tutorials/getting-started.md) — build,
  probe, simulate, script.
- `cargo run -p ohmforge-tui --example scope` — the standalone braille-plot demo.
- [`docs/SHELL.md`](../SHELL.md) — the command-language reference.
</content>
