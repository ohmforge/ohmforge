# `eesh` — the ohmforge shell

`eesh` is a dedicated REPL for building, wiring, probing, running, and reasoning about circuits. It's keyboard-first, scriptable, and speaks the same API the TUI and AI use — so anything you can click or ask for, you can type, and vice-versa.

## Implemented today

This is the full grammar the shell ships and tests today — it matches the
binary's own `help`. Part kind is inferred from the reference's first letter.
See [`tutorials/getting-started.md`](tutorials/getting-started.md) for a worked
session.

```text
# build
add <ref> <a> <b> [value]            # R/C/L/V/I (2-terminal); D (diode, value = I_s, optional)
add Q<n> <c> <b> <e> [npn|pnp]       # BJT (collector base emitter)
add M<n> <d> <g> <s> [nmos|pmos]     # MOSFET (drain gate source)
add E/G <o+> <o-> <c+> <c-> <k>      # VCVS (V/V) / VCCS (A/V)
add H/F <o+> <o-> <k> <Vctrl>        # CCVS (V/A) / CCCS (A/A)
wire <a> <b>                         # merge two named nodes into one net
remove <ref>                         # delete a placed part
probe <node>                         # mark a node to plot on the next run

# parameters & stimuli
set <ref>.value <value>              # retarget a component value (live-tunable)
set <ref> dc <v>                     # DC source level
set <ref> sine <amp> <freq> [off] [phase]    # attach a sine drive (AC/Bode/transient)
set <ref> pulse <v1> <v2> <d> <w> <per>      # attach a pulse drive
set tol|tempco <ref> <n>             # per-part tolerance (Monte-Carlo) / temperature coefficient
set temp <celsius>                   # circuit operating temperature

# analyses
run tran [<dur> [<step>]]            # transient (default 1 ms); a fixed <step> samples sine/PWM
run op                               # DC operating-point node-voltage table
run ac <f0> <f1> [ppd]               # small-signal AC sweep (Bode per probe)
run dc <src> <start> <stop> <step>   # DC sweep of a source's level
run noise <node> <f0> <f1> [ppd]     # output-referred noise PSD
run mc <node> <runs> [seed]          # Monte-Carlo tolerance spread
run tf <node> <src>                  # transfer function / pole-zero report
run sparams <p1> <p2> [z0] [freq]    # 2-port S-parameters + Smith chart

# session
show                                 # list the placed parts
undo                                 # revert the last add/wire/probe/set
checkpoint <name> · restore <name>   # named snapshot / jump back to it
save <path> · load <path>            # write / replace the whole session
help
```

- **Scripting runs when you pipe a file on stdin.** `#` comments, `set <var>`
  bindings, `$var`/`${var}` expansion, and `repeat <n> [as <v>] { … }` loops are
  handled by the embedded script engine, which expands a script into a flat
  command list for **deterministic replay**:
  `eesh < script.eesh` or `cat script.eesh | eesh`
  (equivalently `cargo run -p ohmforge --bin eesh` from source). The interactive
  REPL runs one command per line.
- **Transformers and transmission lines** are circuit-level relations
  (`Circuit::couple`, `Circuit::add_line`) placed through the engine API and the
  project/SPICE formats, not through an `eesh` verb yet.
- The `of` binary also has non-REPL showcases: `of demo` (RC scope), `of bode`,
  `of amp`, `of cosim`, `of view`, and `of tui` (interactive panels); and the
  copilot (`of review`/`of drc` offline, `of ask` live).

Everything from here down is the broader design the shell is growing toward; the
`add`/`wire`/`set`/`run`/session grammar above is what is wired today, and the
richer project/board/AI verbs below are the design target.

## Design principles

- **Terse but readable.** `add r R1 4k7 a b` beats clicking through menus.
- **Units are first-class.** `4k7`, `1u`, `2n2`, `1meg`, `3.3V`, `20mA`, `1kHz` parse everywhere; results print with units.
- **Everything is inspectable.** Any command that changes the circuit can be `undo`ne; any state can be `show`n.
- **Deterministic & replayable.** A session is a script; re-running it reproduces the exact result.
- **Discoverable.** Tab-completion on commands, part names, node names, and options; `help <cmd>` everywhere.

## Command families

### Project & session
```
new project <name>              undo / redo
open <file>                     checkpoint <name>      # named snapshot
save [<file>]                   restore <name>
source <script.eesh>            history / replay       # `run` is reserved for the transient sim (below)
```

### Building the circuit
```
add <type> <ref> [value] <node…> [k=v…]     # add r R1 4k7 a b
                                            # add cap C1 100n a gnd
                                            # add led D1 anode=a cathode=b
wire <nodeA> <nodeB>                         # merge nets
rm <ref>                                     # remove a part/wire
mv <ref> <node…>                             # rewire
rename <old> <new>                           # ref or node
set <ref>.<param> <value>                    # set R1.value 10k ; set V1.freq 1kHz
show <ref|node|net|all>                      # inspect
list [parts|nodes|nets|instruments]
```

### Boards & firmware
```
board add <model> [name]                     # board add uno ; board add pico
board pins <name>                            # show pinout
connect <board>.<pin> <node>                 # or use pins directly as nodes
flash <board> <sketch|hex|elf>               # compile (if needed) + load firmware
mcu <board> <run|halt|step|reset>            # firmware control
break <board> <addr|symbol>                  # breakpoint
regs / mem / disasm <board>                  # debug views
```

### Simulation
```
run <duration>                               # transient, e.g. run 3ms / run 2s
op                                           # DC operating point
sweep dc <src> <start> <stop> <step>
sweep param <ref>.<param> <start> <stop> <step> then <analysis>
ac <fstart> <fstop> [points/decade]          # Bode
noise <out> <src> <fstart> <fstop>
mc <n>                                        # Monte-Carlo, n runs
stop                                          # abort a running analysis
```

### Measurement & instruments
```
probe <node|ref.current>                      # add a probe
scope <sig…> [k=v…]                            # open scope on signals
meas <fn> <sig>                               # meas vpp v(out) ; meas freq v(out)
dmm <node> [mode=v|i|r]
funcgen <node> <wave> <freq> <amp> [offset]
psu <node> <volts> [ilimit=…]
la <sig…>                                      # logic analyzer
decode <uart|spi|i2c> <sig…> [k=v…]
export <csv|vcd|svg|png> <target> <file>
log <sig…> to <file> [every <dt>]
```

### AI
```
ask "<question>"                              # Q&A about the current circuit
design "<spec>"                               # generate a subcircuit (review before apply)
debug                                         # analyze current sim results
firmware "<spec>" [for <board>]               # generate a sketch
explain <ref|node|concept>                    # teach mode
apply / reject                                # accept or discard the last AI proposal
```

### View & help
```
view <schematic|scope|breadboard|bode>
theme <name>
help [<command>]
```

## A worked session

> This illustrates the **design-target** grammar above, not the shipped verbs.
> For a session you can run today, see
> [`tutorials/getting-started.md`](tutorials/getting-started.md).

```eesh
new project rc-lowpass
add v V1 sin amp=1 freq=1kHz  in gnd
add r R1 1k                    in out
add cap C1 159n                out gnd     # fc ≈ 1 kHz
probe v(out)
ac 10Hz 100kHz 50              # Bode sweep, 50 pts/decade
scope v(in) v(out)
meas gain v(out)               # → -3.0 dB at 1 kHz (expected)
ask "confirm the -3 dB corner and suggest values for 2 kHz"
```

## Scripting

`.eesh` scripts are just command lists with parameters and control flow via the embedded scripting engine; run one with `source <file.eesh>`:

```eesh
# sweep an RC corner and log the -3dB point
for fc in [500Hz, 1kHz, 2kHz, 5kHz] {
  set C1.value  = 1 / (2*pi*1k*fc)
  ac 10Hz 1MHz 50
  log meas(corner v(out)) to "corners.csv"
}
```

Scripts make tutorials, regression tests, and reproducible bug reports first-class: attach the `.eesh`, get the exact circuit.

## Completion & help

- **Tab** completes commands, part types, existing refs/nodes, and `k=v` option keys.
- **`?`** after a partial command shows valid continuations.
- Errors are specific and actionable ("node `ou` unknown — did you mean `out`?").
