# ohmforge — the `of` / `eesh` binary

The [ohmforge](https://github.com/ohmforge/ohmforge) workbench binary. One small
Rust executable ships two entry points that orchestrate the engines, the `eesh`
shell, the TUI, and the AI copilot:

- **`of`** — the umbrella CLI, with subcommands.
- **`eesh`** — launches straight into the shell REPL.

This is the top of the workspace: it wires together the dependency-free compute
core (`ohmforge-core`, `ohmforge-sim`), the digital/MCU engines, instruments,
formats, the TUI, the shell, and the copilot into the thing you actually run.

## Usage

Install the binary (from a checkout of the repo):

```sh
cargo install --path crates/ohmforge
```

Then drive it from the terminal:

```sh
of               # drop into the eesh shell REPL (same as `eesh`)
of demo          # RC low-pass transient, plotted as a braille scope
of bode          # AC Bode plot of a filter
of amp           # transistor amplifier operating point with a voltage overlay
of cosim         # MCU firmware + circuit lockstep co-simulation
of tui           # the full dockable terminal UI
of drc <file>    # offline design-rule check of a netlist/project
of ask "<question>"   # ask the copilot a grounded question
of help          # list subcommands
```

With no subcommand (or an unrecognized one) `of` drops into the `eesh` shell,
exactly like the `eesh` binary. From the shell, build a circuit a line at a
time:

```text
eesh> add V1 in  gnd 5
eesh> add R1 in  out 1k
eesh> add C1 out gnd 1u
eesh> probe out
eesh> run tran 5m
```

---

Part of [**ohmforge**](https://github.com/ohmforge/ohmforge) — the AI-native
circuit & firmware workbench.

License: **MIT OR Apache-2.0**.
