//! `eesh` — launches straight into the ohmforge eesh shell REPL (an interactive
//! prompt on a TTY, or the piped `.eesh` script engine), skipping the `of`
//! subcommand layer. Thin wrapper over the shared crate lib.

fn main() {
    ohmforge::run_eesh();
}
