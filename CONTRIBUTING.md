# Contributing to ee-assist

Thanks for helping build a fast, friendly, AI-assisted EE playground for the terminal.

## TL;DR

1. Read [`DEVELOPMENT.md`](DEVELOPMENT.md) — it explains the track/worktree workflow and the local gate.
2. Pick the next `[ ]` checkpoint for a track in [`docs/plan/CHECKPOINTS.md`](docs/plan/CHECKPOINTS.md).
3. Implement to its **acceptance gate** (add the gate as a test if needed).
4. Make sure `just check` is green (fmt + clippy `-D warnings` + tests + docs).
5. Open a PR; flip the checkpoint to `[x]` and note any contract change in [`docs/plan/README.md`](docs/plan/README.md).

## Ground rules

- **Green means green.** Every PR keeps `cargo test --workspace` passing and adds an acceptance test for what it claims.
- **Respect the contracts.** Cross-track surfaces (`Circuit` API, `Signal`, `Analysis`, `McuCore`, `Command` AST, project schema) change only via their own checkpoint, with the plan updated and affected tracks pinged.
- **Keep the core pure.** No I/O, clock, or unseeded randomness in `ee-core`/`ee-sim`.
- **Docs land with features.** A user-visible feature ships with an example project and a `help` entry.

## Licensing

By contributing you agree that your contributions are dual-licensed under **MIT OR Apache-2.0** (see [`LICENSE-MIT`](LICENSE-MIT) and [`LICENSE-APACHE`](LICENSE-APACHE)), matching the rest of the project.

## Code of conduct

Be kind and constructive. Assume good faith. This is a learning-first project — questions are welcome.
