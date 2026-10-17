# `eesh` — the ee-assist shell

`eesh` is a dedicated REPL for building, wiring, probing, running, and reasoning about circuits. It's keyboard-first, scriptable, and speaks the same API the TUI and AI use — so anything you can click or ask for, you can type, and vice-versa.

## Implemented today (M1/M2)

The grammar below (under "Command families") is the **design target**. What ships and is tested today is a focused subset — see [`tutorials/getting-started.md`](tutorials/getting-started.md) for a worked session:

```
add <ref> <nodeA> <nodeB> <value>   # kind from the ref's first letter: R C L V I D
wire <nodeA> <nodeB>                 # merge two named nets
probe <node>                         # mark a node to plot on the next run
run [tran [<dur>]] | run op          # transient (braille scope) or DC operating point
show                                 # list placed parts
undo · checkpoint <name> · restore <name>
help
```

- **Part kind is inferred from the reference's first letter** (`R1`→resistor, `C1`→capacitor, `V1`→source, …); there is no `<type>` token yet. 3-/4-terminal and controlled devices (BJT/MOSFET/VCVS/…) are modeled by the engine but not yet placeable from this grammar — build those via the netlist/project formats or the engine API.
- **Scripting** (`set`, `repeat … { }`, `$vars`, deterministic replay) and **comments** (`#`) run through the script engine: pipe a file in — `ee < script.eesh` or `cat script.eesh | ee`. The interactive REPL runs one command per line.
- The binary also has non-REPL showcases: `ee demo` (RC scope), `ee bode`, `ee amp`, `ee cosim`, and `ee tui` (interactive panels).

Everything from here down is the broader design the shell is growing toward; not all of it is wired yet.

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
