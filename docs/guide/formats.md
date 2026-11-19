# File formats & interop (`ohmforge-fmt`)

`ohmforge-fmt` is how a circuit leaves and re-enters ohmforge: a human-readable
**native project** file for git, a terse **netlist** you type, **SPICE** import/
export (with `.model` cards and a vendor catalog) for the wider ecosystem,
**KiCad** netlist import, **Fritzing** and **BOM** export, and **CSV**/**VCD**
waveform export for spreadsheets and GTKWave. Every parser is hand-written with
**no dependency beyond `ohmforge-core`**, every error is tagged with its 1-based
source line, and all output is deterministic (numbers print via `f64`'s shortest
round-trippable form, so files are byte-stable and diff cleanly).

Runnable examples live in [`examples/formats/`](../../examples/formats/) and are
checked by `cargo test -p ohmforge-fmt --test examples`.

| Format | Module | Read | Write | Notes |
|---|---|---|---|---|
| Native project (`.eeproj`) | `project` | `load` | `save` | byte-for-byte round trip (see below) |
| Netlist | `netlist` | `parse` | — | input only |
| SPICE deck (`.cir`) | `spice` | `import` | `export` | R/C/L/V/I **+** D/Q/M + `.model` + E/G/H/F |
| KiCad netlist | `kicad` | `import` | — | input only |
| Fritzing | `fritzing` | — | `export` | export only |
| BOM (CSV) | `bom` | — | `to_csv` | export only |
| CSV waveform | `waveform` | — | `to_csv` | export only |
| VCD waveform | `waveform` | — | `to_vcd` | export only |

---

## Native project format

The format the tool *writes back*, designed to live in git: one part per line,
stable ordering, and a byte-for-byte round trip (`save(load(text)) == text`).

A real file ([`examples/formats/rc-lowpass.eeproj`](../../examples/formats/rc-lowpass.eeproj),
exactly what `save` emits for a 5 V → 1 kΩ → 1 µF low-pass):

```text
!ee-project v1
.name rc-lowpass
V1 V 5V n1 gnd
R1 R 1000Ω n1 n2
C1 C 0.000001F n2 gnd
```

* **First real line is the version marker** `!ee-project v1`; loading anything
  else is a `MissingHeader`/`UnknownVersion` error so an unversioned file can
  never be silently misread.
* **Each part is `REF KIND VALUE NET…`.** The kind is explicit (`R`/`C`/`L`/`V`/
  `I`/`D`, `Qn`/`Qp`, `Mn`/`Mp`, the controlled sources `vcvs(E)`/`vccs(G)`/
  `ccvs(R,ctrl)`/`cccs(F,ctrl)`, or `other(<name>)`), not guessed from the
  reference's first letter.
* **Why it round-trips.** Terminals are written as their *resolved* net
  (`Circuit::resolve`), so two terminals on one electrical node print the same
  name and reload onto one net — topology survives even across `wire()` merges.
  A value prints as its base-SI magnitude plus the unit's SI symbol (`1000Ω`,
  `0.000001F`), and the unit is recovered from that symbol, so the
  `(magnitude, unit)` pair returns bit-for-bit; `-` means "no value".
* **Comments** are `#` or `//`; `gnd`/`ground`/`0` all mean ground.

Documented limits (by design, not loss you'll hit silently): transistor model
parameters default on load (the discriminant and polarity round-trip; custom
Ebers–Moll/level-1 coefficients are a follow-up), and an `other(<name>)` name
must not contain `)`, a newline, or a comment marker. Source time-waveforms
(`Sine`/`Pulse`) are not yet serialized — a `Dc` source round-trips exactly.

## Netlist

The terse "type a circuit" front door — input only, feeds the engine directly.
Fields are `REF A B VALUE`; the part type comes from the reference's first
letter (`R`/`C`/`L`/`V`). Values use `ohmforge-core`'s engineering notation, where —
note — **`M` means mega** (`1M` = 1 000 000), the opposite of SPICE.

```text
# an RC low-pass (comments start with # or //)
.title rc-lowpass
V1 in  gnd 5
R1 in  out 1k
C1 out gnd 1u
```

## SPICE deck

Import/export of the industry-standard deck. A real deck that imports and solves
([`examples/formats/divider.cir`](../../examples/formats/divider.cir)):

```text
* ohmforge example: a 10 V resistive divider (DC operating point)
V1 in 0 DC 10
R1 in out 1k
R2 out 0 1k
.op
.end
```

* **Line 1 is always the title** (SPICE tradition), kept verbatim as the circuit
  name. **Node `0`** (also `gnd`/`ground`) is ground. Sources accept an optional
  `DC`/`AC` keyword before the value. `*` starts a full-line comment, `;` an
  inline one, and the flow directives (`.op`, `.tran`, …) are recognized and
  skipped.
* **`.model` cards and model-backed devices.** A `D`/`Q`/`M` element names a
  `.model <name> <type> (<param>=<value> …)` card (types `D`, `NPN`, `PNP`,
  `NMOS`, `PMOS`), and the parameters map onto the engine's device models
  (diode `IS`/`N`; BJT `IS`/`BF`/`BR`; MOSFET `VTO`/`KP`/`LAMBDA`). A built-in
  **vendor catalog** resolves common names (1N4148 / 2N3904 / 2N2222 /
  1N4007 / 2N7000) when the deck references a model it does not define; unknown
  parameters are noted, not errored.
* **Export writes a valid, re-importable deck**: title, one line per part, a
  trailing `.end`. Diodes, BJTs and MOSFETs emit real `D`/`Q`/`M` element lines
  **plus** a matching per-part `.model` card (`Dmod_<ref>`, `Qmod_<ref>`,
  `Mmod_<ref>`), and the four controlled sources emit `E`/`G`/`H`/`F` lines, so
  an export→import round-trips the kind and model parameters. Only the opaque
  `other(<name>)` kind is emitted as a `*` comment.

### The `m`-vs-`meg` gotcha

SPICE scale factors are **case-insensitive**, and that is exactly where they
bite: **`M` (or `m`) means *milli*, not mega — mega is spelled `MEG`.**

| suffix | factor | | suffix | factor |
|---|---|-|---|---|
| `t` | 1e12 | | `m` | **1e-3** |
| `g` | 1e9 | | `u`/`µ` | 1e-6 |
| `meg` | **1e6** | | `n` | 1e-9 |
| `k` | 1e3 | | `p` | 1e-12 |
| `mil` | 25.4e-6 | | `f` | 1e-15 |

So `R1 … 1m` is a **1 milliohm** resistor and `R1 … 1meg` is a **1 megohm** one
— a factor of a billion apart. `meg` and `mil` are matched before the single
letter `m` so neither is misread, and trailing unit text is ignored
(`10kohm` == `10k`). Because `ohmforge-core`'s own netlist uses the opposite
convention (`M` = mega), the SPICE module parses magnitudes itself rather than
deferring to the shared parser.

`export` sidesteps the trap entirely: it never writes a suffix, only a plain
base-SI decimal (`0.001`, `1000000`), so a re-`import` is unambiguous and a
milliohm or megohm survives the round trip exactly.

---

## KiCad netlist import (`kicad::import`)

Reads the s-expression netlist KiCad's *Tools → Generate Netlist* emits — the
`(export (components …) (nets …))` form — into a `Circuit`. Components become
parts (kind from the reference's first letter, value parsed in engineering
notation) and each `(net …)` block merges the pins on it into one electrical
node, so the imported topology matches the schematic. Input only.

## Fritzing export (`fritzing::export`)

Writes the `.fz` XML document that lives at the centre of a Fritzing sketch: a
`<module>` with one `<instance>` per placed part and per-connector `<connects>`
lists for the wiring. It emits the XML directly — no zip, no external `.fzp`
part files — keeping the crate's single-dependency rule (`ohmforge-core` only).

## BOM export (`bom::to_csv`)

Turns a circuit into the CSV a purchaser or assembler reads: one line per
distinct part *type* (same kind **and** value), with a quantity and the
reference designators that share it.

```text
kind,value,quantity,references
Resistor,1kΩ,2,R1 R2
Resistor,2.2kΩ,1,R3
Capacitor,0.0000001F,1,C1
Voltage source,5V,1,V1
```

Lines sort by kind (a fixed, schematic-friendly order), then value, then the
first reference; designators within a line sort *naturally* (`R1 R2 R10`). The
output is byte-stable for identical input.

---

## Waveform export — CSV & VCD

Both turn a slice of `ohmforge-sim` `Signal`s (one analysis run) into a file. All
signals must share one x-axis (same length and sample points) — the normal case
for a single run. Numbers print in `f64`'s shortest round-trippable form.

### CSV (`to_csv`)

A header row then one row per sample: the independent axis first (`time_s` or
`freq_hz`), then one column per real signal. A **complex** (AC) signal splits
into `<name>_mag` and `<name>_phase_deg`. Fields are quoted per RFC 4180 only
when they contain a comma, quote, or newline.

```text
time_s,v(out),clk
0,0,0
0.001,1.5,1
0.002,3.3,0
0.003,3.3,1
```

### VCD (`to_vcd`)

A well-formed Value Change Dump for GTKWave et al.: a deterministic
`$date`/`$version`/`$timescale`/`$scope`/`$var` header, then time-ordered value
changes. The timescale is derived from the smallest sample spacing so every
sample lands on a **distinct, strictly increasing integer tick** (equal
timestamps are nudged apart; a backwards time axis is a `NonMonotonic` error).
A signal whose every sample is exactly `0` or `1` is dumped as a one-bit scalar
(`wire`); everything else is a `real` var. Only *changed* values are written at
each tick (plus every var's initial value at the first), the standard compact
dump. Complex (AC) signals are rejected — use CSV for those.

```text
$date ohmforge $end
$version ohmforge-fmt waveform export $end
$timescale 1 ms $end
$scope module ee $end
$var real 1 ! v(out) $end
$var wire 1 " clk $end
$upscope $end
$enddefinitions $end
#0
r0 !
0"
#1
r1.5 !
1"
#2
r3.3 !
0"
#3
1"
```

At `#2` the analog `v(out)` is unchanged (3.3 V) so it is **not** re-emitted;
only `clk` toggles. Each var's identifier code is drawn from printable ASCII
(`!`..=`~`), encoded base-94 so any number of signals gets a distinct, printable
id — past 94 signals ids become two characters rather than running off the end
of the printable range.
