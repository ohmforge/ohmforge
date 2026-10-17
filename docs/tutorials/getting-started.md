# Getting started with `eesh`

Your first session in the `eesh` shell: build a circuit by typing, probe a node,
simulate it, and read the result off a braille scope and an operating-point
table. Then automate it with a `.eesh` script, and learn how `undo` and named
checkpoints let you explore without fear.

Everything here is real, verified output from the current build. The runnable
scripts live in [`examples/tutorial/`](../../examples/tutorial); each command
block was produced by piping the matching file through the `ee` binary.

## Starting the shell

Run the binary with no arguments for an interactive prompt:

```console
$ cargo run -p ee
ee> add R1 in out 1k
added R1
ee> help
...
```

`eesh` is also *pipeable* — feed it a file of commands on stdin and it runs each
line in order, printing output to stdout:

```console
$ cat examples/tutorial/divider.eesh | cargo run -p ee
```

That is exactly how every transcript below was captured.

## Building a circuit: `add`

The core verb is `add`:

```text
add <ref> <nodeA> <nodeB> <value>
```

- **`<ref>`** is the part's reference designator. Its **first letter picks the
  part type**: `R`→resistor (Ω), `C`→capacitor (F), `L`→inductor (H),
  `V`→voltage source (V), `I`→current source (A), `D`→diode.
- **`<nodeA> <nodeB>`** are node names. Any new name becomes a fresh net the
  first time you use it; the names `gnd`, `ground`, and `0` all mean the ground
  reference (0 V).
- **`<value>`** parses in engineering notation: `1k`, `4k7`, `1u`, `2n2`,
  `10meg`, `3.3`, `5` — units come from the part type, so a resistor's `1k`
  is 1 kΩ and a capacitor's `1u` is 1 µF.

### A resistive divider

Build 10 V across two equal 1 kΩ resistors
([`divider.eesh`](../../examples/tutorial/divider.eesh)):

```text
add V1 in gnd 10
add R1 in mid 1k
add R2 mid gnd 1k
run op
show
```

```console
$ cat examples/tutorial/divider.eesh | cargo run -p ee
added V1
added R1
added R2
operating point:
  v(in) = 10.000000 V
  v(mid) = 5.000000 V

3 parts:
  V1   source              10V  in — gnd
  R1   resistor          1000Ω  in — mid
  R2   resistor          1000Ω  mid — gnd
```

`run op` solves the **DC operating point** and tabulates every named node's
voltage. The midpoint reads 5 V — exactly what a 1:1 divider should give. `show`
lists the parts you have placed, with their resolved nodes.

> **Merging nodes with `wire`.** If you build two nodes separately and later want
> them to be the same net, `wire <a> <b>` merges them. Both must already exist
> (it rejects a typo'd node up front), which keeps accidental shorts from a
> mistyped name out of your circuit.

## Seeing a waveform: `probe` then `run tran`

`run op` is a single DC snapshot. To watch the circuit move over time, mark a
node with `probe`, then run a **transient**. This RC low-pass charges a 1 µF
capacitor through 1 kΩ from a 5 V step
([`rc_lowpass.eesh`](../../examples/tutorial/rc_lowpass.eesh)):

```text
add V1 in gnd 5
add R1 in out 1k
add C1 out gnd 1u
probe out
run tran 5m
show
```

`run tran 5m` simulates 5 ms (durations parse in engineering notation; bare
`run` or `run tran` defaults to 1 ms). Each probed node renders as a
high-resolution **braille scope**:

```console
$ cat examples/tutorial/rc_lowpass.eesh | cargo run -p ee
added V1
added R1
added C1
probing out
  v(out)
4.963V ┤⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⣀⣀⣀⣀⣠⠤⠤⠤⠤⠤⠴⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠋⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉
       ┤⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⣀⣀⡤⠤⠴⠒⠒⠋⠉⠉⠁
       ┤⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⣀⣠⠤⠖⠚⠉⠁
       ┤⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⣀⡤⠖⠋⠁
       ┤⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⣠⠴⠋⠁
       ┤⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⣀⡴⠋⠁
       ┤⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⣠⠞⠁
       ┤⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⣠⠞⠁
       ┤⠀⠀⠀⠀⠀⠀⠀⠀⢀⡞⠁
       ┤⠀⠀⠀⠀⠀⠀⠀⡴⠋
       ┤⠀⠀⠀⠀⠀⢀⡞⠁
       ┤⠀⠀⠀⠀⢠⠏
       ┤⠀⠀⠀⣰⠋
       ┤⠀⠀⣰⠃
       ┤⠀⣰⠃
    0V ┤⣰⠃
       └────────────────────────────────────────────────────────────────────────
        0s                                                                0.005s
```

That is the classic RC charging curve: a steep rise that eases toward the 5 V
rail, reaching ~4.96 V by 5 ms (about five time constants, τ = RC = 1 ms). The
left gutter auto-scales to the signal's range; the bottom axis spans the
simulated window.

### Reading the operating-point overlay

`run op` gives you the numbers; the TUI's **op-point overlay** paints them back
onto the schematic so you can see the DC state at a glance. In plain (snapshot)
form it looks like this for the same kind of divider:

```text
┌─ op-point ───────────────────────────────┐
│ V1  V-source  5V    [n1=5V, gnd=0V]      │
│ R1  Resistor  10kΩ  [n1=5V, n2=2.5V]     │
│ R2  Resistor  10kΩ  [n2=2.5V, gnd=0V]    │
└──────────────────────────────────────────┘
```

Each part's nets are annotated with their node voltage; ground is always 0 V. In
the live TUI (and in `ee amp`) the voltages are colour-coded by level — ground
dim, higher rails toward red — so a probe point that is not where you expect it
jumps out. See [the view guide](../guide/view.md) for the interactive version.

## Exploring safely: `undo`, `checkpoint`, `restore`

Every command that changes the circuit (`add`, `wire`, `probe`) is reversible.
`undo` reverts the last one. A named `checkpoint` saves the whole session; a
later `restore` jumps back to it, however far you have wandered
([`checkpoints.eesh`](../../examples/tutorial/checkpoints.eesh)):

```text
add V1 in gnd 5
add R1 in out 1k
add C1 out gnd 1u
show
undo
show
checkpoint rc
add R2 out gnd 2k
show
restore rc
show
```

```console
$ cat examples/tutorial/checkpoints.eesh | cargo run -p ee
added V1
added R1
added C1
3 parts:
  V1   source               5V  in — gnd
  R1   resistor          1000Ω  in — out
  C1   capacitor     0.000001F  out — gnd

undone — 2 parts remain
2 parts:
  V1   source               5V  in — gnd
  R1   resistor          1000Ω  in — out

checkpoint "rc" saved
added R2
3 parts:
  V1   source               5V  in — gnd
  R1   resistor          1000Ω  in — out
  R2   resistor          2000Ω  out — gnd

restored checkpoint "rc"
2 parts:
  V1   source               5V  in — gnd
  R1   resistor          1000Ω  in — out
```

`undo` peeled off the `C1` you just added; the `checkpoint`/`restore` pair let
you try a second resistor (`R2`) and then rewind to the saved two-part state in
one step. (`restore` does not disturb the earlier `undo` history.)

## Scripting: loops and parameters

A session *is* a script, so re-running it reproduces the exact circuit and
results. On top of the plain command list, the eesh scripting engine adds
variables and loops — enough to turn a parameter sweep into a few lines
([`sweep.eesh`](../../examples/tutorial/sweep.eesh)):

```text
# Sweep the transient window: 1 ms, 2 ms, 3 ms over the same RC low-pass.
# Loops and $vars run through the eesh scripting engine (Shell::run_script).
set r 1k
add V1 in gnd 5
add R1 in out $r
add C1 out gnd 1u
probe out
repeat 3 as win {
  run tran ${win}m
}
```

- `set <name> <value>` binds a variable; `$name` or `${name}` expands it on any
  later line (so `add R1 in out $r` places a 1 kΩ resistor).
- `repeat <count> [as <var>] { … }` runs its body `<count>` times; the loop
  variable, if named, counts `1..=count`. `<count>` can itself be a variable.
- Lines starting with `#` are comments; blank lines are ignored.

The engine *expands* the script into a flat list of ordinary commands before
running them, so this sweep becomes exactly:

```text
add V1 in gnd 5
add R1 in out 1k
add C1 out gnd 1u
probe out
run tran 1m
run tran 2m
run tran 3m
```

Because the expanded list is deterministic and `execute` is pure, the run is
**replayable**: the engine returns both a transcript and a recording, and
replaying the recording against a fresh shell reproduces a byte-identical
transcript — ideal for tutorials, regression tests, and bug reports.

> **Note.** Loop/parameter scripts run through the scripting engine
> (`Shell::run_script`). The plain command-only scripts above (`divider.eesh`,
> `rc_lowpass.eesh`, `checkpoints.eesh`) also run line-by-line straight through
> the `ee` binary's stdin; a `.eesh` file that uses `#` comments, `set`,
> `repeat`, or `$vars` needs the scripting engine. (A `source <file>` verb that
> routes a piped file through the engine is on the roadmap.)

## Where to next

- [`docs/guide/view.md`](../guide/view.md) — the interactive `ee tui` and the
  virtual instruments (scope trigger/cursors, DMM, funcgen, PSU, Bode/spectrum,
  logic analyzer + protocol decode) for *debugging* a circuit.
- [`docs/SHELL.md`](../SHELL.md) — the full command-language reference.
</content>
</invoke>
