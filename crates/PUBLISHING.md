# Publishing ohmforge to crates.io

The workspace is 12 crates with path dependencies. crates.io has no path
dependencies, so each crate must be published **after** every `ohmforge-*` crate
it depends on, and each dependency must be a real published version. This doc
gives the correct order and the pre-publish checklist.

> Before anything else: confirm each `ohmforge-*` name is **available** on
> crates.io. Open `https://crates.io/crates/<name>` for every crate; a 404 means
> the name is free. If any name is taken, stop and resolve it (claim/rename)
> before publishing — the names must be reserved together so the family is
> consistent. Publishing the first crate is what reserves its name, so publish in
> the order below and do not leave gaps.

## Dependency DAG

```
ohmforge-core      (no ohmforge deps)   ── dependency-free, wasm-clean
ohmforge-digital   (no ohmforge deps)   ── dependency-free, wasm-clean
ohmforge-sim       → core               ── dependency-free, wasm-clean
ohmforge-fmt       → core, sim
ohmforge-devices   → core, sim
ohmforge-instruments → core, sim
ohmforge-mcu       → core, digital
ohmforge-tui       → core, instruments
ohmforge-shell     → core, fmt, instruments, sim, tui
ohmforge-ai        → core, shell, sim
ohmforge-web       → core, fmt, sim
ohmforge (binary)  → ai, core, devices, digital, fmt, instruments, mcu, shell, sim, tui
```

## Publish order

Publish strictly top to bottom. Each line only depends on crates already
published above it. The dependency-free core (`core`, `digital`) goes first, then
`sim`, then the rest; the browser crate and the binary go last.

1. `ohmforge-core`
2. `ohmforge-digital`
3. `ohmforge-sim`
4. `ohmforge-fmt`
5. `ohmforge-devices`
6. `ohmforge-instruments`
7. `ohmforge-mcu`
8. `ohmforge-tui`
9. `ohmforge-shell`
10. `ohmforge-ai`
11. `ohmforge-web`
12. `ohmforge` (the `of` / `eesh` binary)

After publishing a crate, wait for the registry index to update (usually
seconds) before publishing the next one that depends on it.

> **Path-vs-version note.** For `cargo publish` to resolve the `ohmforge-*`
> dependencies from crates.io instead of local paths, each dependency must carry
> a `version` as well as its workspace `path`. Confirm the workspace
> `[workspace.dependencies]` entries (or per-crate deps) specify a version before
> the first publish; `cargo publish` will error if a path dependency has no
> version to fall back on.

## Pre-publish checklist (per crate)

Run for each crate before it goes out:

- [ ] `description` is present and truthful (crates.io **requires** it).
- [ ] `license = "MIT OR Apache-2.0"` (inherited from the workspace).
- [ ] `readme = "README.md"` points at the crate's **own** README, and that file exists.
- [ ] `repository` / `homepage` set (inherited from the workspace).
- [ ] `keywords` / `categories` present (inherited) — crates.io caps keywords at 5.
- [ ] The crate builds and its tests pass: `cargo test -p <crate>`.
- [ ] Docs are clean: `cargo doc -p <crate> --no-deps`.
- [ ] Dry run the package: `cargo publish -p <crate> --dry-run`.
- [ ] The bumped version is committed and the release is tagged (e.g. `git tag v0.6.0`).

Keep `ohmforge-core` and `ohmforge-sim` **dependency-free** — do not add runtime
dependencies to satisfy packaging; metadata and docs only.

## Doing the publish

```sh
# One-time: authenticate.
cargo login <crates.io-token>

# Per crate, in the order above:
cargo publish -p ohmforge-core --dry-run   # inspect the packaged files
cargo publish -p ohmforge-core             # then the real thing
# ...repeat down the list, waiting for the index between dependents.
```

A whole-workspace convenience, if the toolchain supports it, is
`cargo publish --workspace`, which computes this same order automatically — but
the per-crate flow above is the reliable fallback and lets you verify each
crate's dry run first.

## Tagging

Tag the release once after all crates are out (the workspace shares one version):

```sh
git tag -a v0.6.0 -m "ohmforge 0.6.0"
git push origin v0.6.0
```
