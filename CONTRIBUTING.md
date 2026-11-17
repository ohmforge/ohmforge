# Contributing to ohmforge

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
- **Keep the core pure.** No I/O, clock, or unseeded randomness in `ohmforge-core`/`ohmforge-sim`.
- **Docs land with features.** A user-visible feature ships with an example project and a `help` entry.

## Licensing

By contributing you agree that your contributions are licensed under the **Apache License 2.0** (see [`LICENSE-APACHE`](LICENSE-APACHE)), matching the rest of the project.

## Developer Certificate of Origin (DCO)

ohmforge uses the [Developer Certificate of Origin](https://developercertificate.org/) (DCO) to certify that you wrote, or otherwise have the right to submit, the code you contribute. It is lightweight: no paperwork, just a sign-off line on each commit.

**Sign off every commit** by adding a `Signed-off-by` trailer with your real name and email:

```text
Signed-off-by: Jane Developer <jane@example.com>
```

The easiest way is to pass `-s` to git:

```sh
git commit -s -m "fix: correct the AC sweep corner frequency"
```

By signing off you agree to the DCO (reproduced below):

> **Developer Certificate of Origin 1.1** — By making a contribution to this project, I certify that: (a) the contribution was created in whole or in part by me and I have the right to submit it under the open source license indicated in the file; or (b) the contribution is based upon previous work that, to the best of my knowledge, is covered under an appropriate open source license and I have the right under that license to submit that work with modifications; or (c) the contribution was provided directly to me by some other person who certified (a), (b) or (c) and I have not modified it. (d) I understand and agree that this project and the contribution are public and that a record of the contribution (including all personal information I submit with it, including my sign-off) is maintained indefinitely and may be redistributed consistent with this project and the open source license(s) involved.

The DCO is our bar today. A **Contributor License Agreement (CLA)** may be introduced before accepting large external contributions as the project's governance matures; if that happens it will be announced here with clear instructions. For now, a DCO sign-off is all that is required.

## Code of conduct

Be kind and constructive. Assume good faith. This is a learning-first project — questions are welcome.
