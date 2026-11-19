<!--
Thanks for contributing to ohmforge! Please read CONTRIBUTING.md first.
Keep the PR focused; a small, reviewable change is easier to land.
-->

## Summary

What does this PR do, and why? Link any related issue (e.g. `Closes #123`).

## Changes

- <!-- bullet the key changes -->

## Checkpoint / contract notes

<!-- If this touches a checkpoint in docs/plan/CHECKPOINTS.md, name it and flip it to [x]. -->
<!-- If it changes a cross-track contract (Circuit API, Signal, Analysis, McuCore, Command AST, project schema), say so and update the plan. -->

## Testing

How did you verify this? Note any new acceptance test that proves what the PR claims.

## Checklist

- [ ] Commits are **signed off** for the DCO (`git commit -s`) — see [CONTRIBUTING.md](../CONTRIBUTING.md).
- [ ] `cargo fmt --all` leaves no changes.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean.
- [ ] `cargo test --workspace` passes (added/updated tests for the change).
- [ ] `cargo doc --workspace --no-deps` builds with `-D warnings` (if docs/APIs changed).
- [ ] Docs and examples updated for any user-visible change (a feature ships with an example + a `help` entry).
- [ ] `just check` is green locally.

<!-- Tip: `just check` runs fmt + clippy -D warnings + tests + docs in one go. -->
