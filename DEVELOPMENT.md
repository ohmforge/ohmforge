# Developing ohmforge

This project is built as **parallel tracks** that rarely touch each other's files, so several people (or sessions, or a single person across days) can push it forward without merge pain. The full program plan lives in [`docs/plan/`](docs/plan/README.md); this file is the practical "how do I work on it" guide.

## Prerequisites

- A recent stable Rust toolchain (via [rustup](https://rustup.rs)). `rust-toolchain.toml` selects `stable` + `rustfmt`/`clippy` automatically. The **MSRV is 1.79** (checked in CI).
- Optional: [`just`](https://github.com/casey/just) (`cargo install just`) for the task runner.

## The local gate

Everything CI enforces, in one command:

```sh
just check      # fmt-check + clippy(-D warnings) + test + doc
```

or without `just`:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

**Rule:** a checkpoint only flips to done in [`docs/plan/CHECKPOINTS.md`](docs/plan/CHECKPOINTS.md) when its acceptance gate passes on a green `cargo test --workspace`.

## Tracks & crate ownership

| Track | Crates | Branch | Design doc |
|---|---|---|---|
| sim-core | `ohmforge-core`, `ohmforge-sim` | `track/sim-core` | [`docs/plan/track-sim-core.md`](docs/plan/track-sim-core.md) |
| boards | `ohmforge-digital`, `ohmforge-mcu`, `ohmforge-devices` | `track/boards` | [`docs/plan/track-boards.md`](docs/plan/track-boards.md) |
| ui-shell | `ohmforge-tui`, `ohmforge-shell`, `ohmforge-instruments` | `track/ui-shell` | [`docs/plan/track-ui-shell.md`](docs/plan/track-ui-shell.md) |
| ai-fmt | `ohmforge-ai`, `ohmforge-fmt` | `track/ai-fmt` | [`docs/plan/track-ai-fmt.md`](docs/plan/track-ai-fmt.md) |
| platform | *(cross-cutting)* | `main` | [`docs/plan/README.md`](docs/plan/README.md) |

Cross-track needs go through the **contracts** in the plan (the `Circuit` API, `Signal`, the `Analysis` enum, `McuCore`, the `Command` AST, the project schema) — never by editing another track's internals. Changing a contract is its own checkpoint.

## Worktree-per-track workflow

Each track gets its own git **worktree** so you can have all of them checked out at once, building independently, with no branch-switching churn:

```sh
just worktrees-init          # creates ../ohmforge-worktrees/{sim-core,boards,ui-shell,ai-fmt}
just worktrees               # list them
```

Under the hood that's just:

```sh
git worktree add -b track/sim-core ../ohmforge-worktrees/sim-core
```

Work in the worktree for your track, commit to its branch, and merge into `main` when a checkpoint's gate passes. `platform` work (CI, WASM, benches) happens directly on `main`.

## Resuming cold (new day / new session / new worktree)

1. `cargo test --workspace` — confirm the baseline is green.
2. Open [`docs/plan/CHECKPOINTS.md`](docs/plan/CHECKPOINTS.md); find your track's first `[ ]`/`[~]`/`[!]`.
3. Read that checkpoint's section in the track's `track-*.md` design doc.
4. Implement to the **acceptance gate**; add it as a test if it isn't one.
5. Flip the ledger entry to `[x]`, commit (`<track>: CP-XXX-Mn.k <summary>`), record any contract change in the plan.

Each `track-*.md` ends with a **"State & next action"** block naming the current checkpoint and the immediate next step — so no session has to reverse-engineer where things stand.

## Conventions

- **Errors:** `thiserror` in libraries, `anyhow` at the binary/shell edge. No `unwrap`/`panic!` on user-reachable paths.
- **The compute core is I/O-free.** `ohmforge-core`/`ohmforge-sim` take no clock, no filesystem, no network, no `rand` without an explicit seed — that's what keeps them deterministic and WASM-clean. Verify with `just wasm`.
- **Determinism:** golden-file tests and fixed seeds. A flaky test is a bug in the test.
- **Commits:** `<track-or-crate>: <summary>`, e.g. `sim-core: CP-SIM-M0.3 linear MNA + DC op`.

See [`docs/ARCHITECTURE.md` §Cross-cutting design decisions](docs/ARCHITECTURE.md#cross-cutting-design-decisions) for the full rationale behind these.
