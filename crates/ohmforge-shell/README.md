# ohmforge-shell — the `eesh` command language & REPL

The `eesh` shell for [ohmforge](https://github.com/ohmforge/ohmforge): a parser
for the command language, tab-completion, history, and an embedded scripting
engine for loops, parameters, and automation. The shell mutates the same
`ohmforge_core::Circuit` the TUI and AI copilot do, so a circuit built by typing
commands, by clicking in the TUI, or by the AI is one and the same.

At its core is a `Shell` — a testable state machine that holds a circuit and
executes one command line at a time, returning the text to print. The crossterm
readline loop is a thin wrapper over it. The grammar is one command per line,
whitespace-separated (`add`, `wire`, `probe`, `run`, `show`, `help`, …); the
full command spec lives in `docs/SHELL.md`.

## Usage

```rust
use ohmforge_shell::Shell;

// Build and simulate an RC low-pass entirely through shell commands.
let mut sh = Shell::new();
sh.execute("add V1 in gnd 5").unwrap();
sh.execute("add R1 in out 1k").unwrap();
sh.execute("add C1 out gnd 1u").unwrap();
sh.execute("probe out").unwrap();

// `run` returns the text to print — here a braille scope frame.
let frame = sh.execute("run tran 5m").unwrap();
assert!(frame.contains('\u{2524}'));
```

---

Part of [**ohmforge**](https://github.com/ohmforge/ohmforge) — the AI-native
circuit & firmware workbench.

License: **Apache-2.0**.
