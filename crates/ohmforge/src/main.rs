//! `of` — the primary ohmforge CLI. Parses a subcommand and dispatches; with no
//! subcommand it drops into the eesh shell. All logic lives in the crate lib so
//! the `eesh` binary can share it.

fn main() {
    ohmforge::run_of();
}
