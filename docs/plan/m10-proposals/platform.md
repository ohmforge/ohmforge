# M10 Platform / Release Engineering Proposals

Author: principal platform/release engineer
Scope: cross-cutting — CI, the digital kernel, benchmarks, packaging/publishing,
determinism, dev-ergonomics, and overall engineering quality.
Status of repo studied: ohmforge v0.8.1, M0–M9 complete.

---

## What is already solid (don't re-litigate)

The engineering baseline here is genuinely strong; several of the "cross-cutting"
boxes are already ticked and should be preserved, not rebuilt:

- **CI breadth is excellent.** `.github/workflows/ci.yml` runs `fmt`, `clippy`
  (`--workspace --all-targets --all-features -D warnings`), `test` on a
  Linux/macOS/Windows matrix, an **MSRV 1.79 build**, a **wasm32** check of the
  compute core, an **aarch64 cross-build**, and a **docs** build with
  `RUSTDOCFLAGS=-D warnings`. Runs cancel superseded refs via `concurrency`, and
  `Swatinem/rust-cache@v2` is wired on every heavy job. This is more
  cross-platform coverage than most 1.0 Rust projects ship.
- **Determinism is a first-class, tested property.** `crates/ohmforge-sim/tests/determinism.rs`
  (`CP-PLAT.5`) is a bit-for-bit golden suite: every analysis runs twice and is
  compared as one whole-waveform value, with cross-platform-portable Monte-Carlo
  constants pinned via the in-crate SplitMix64 PRNG. `.gitattributes` enforces
  `eol=lf` so byte-exact goldens hold on Windows.
- **The compute core is disciplined.** `ohmforge-core`, `ohmforge-sim`, and
  `ohmforge-digital` are dependency-free and wasm-clean by design; dev-deps
  (Criterion) are explicitly kept out of the shipped surface; `unsafe_code` is a
  warn-level workspace lint.
- **Benches exist and are thoughtful.** `crates/ohmforge-sim/benches/solve.rs` is
  a rich Criterion suite (op-point, transient, sparse-vs-dense, fill-reducing
  order, symbolic-refactor reuse, Monte-Carlo parallel) and
  `crates/ohmforge-core/benches/parse.rs` covers the value parser.
- **Publishing is thought through on paper.** `crates/PUBLISHING.md` has the
  verified-free names, the dependency DAG, the strict publish order, the
  path+version note, and a per-crate checklist.
- **Dev ergonomics are good.** The `justfile` mirrors CI task-for-task
  (`just check` == the CI gate) plus worktree helpers.

The gaps below are therefore mostly about **closing the loop** (benches that
nobody runs, a publish order nobody executes, roadmap promises not yet wired),
not about missing foundations.

---

## Ranked proposals

### 1. Performance-regression CI — make the benches a gate, not a museum piece

**What + why.** The Criterion suites in `ohmforge-sim` and `ohmforge-core` are
excellent but run **only by hand** (`just bench`); nothing in `ci.yml` executes
them, stores a baseline, or fails on regression. The ROADMAP's cross-cutting
section explicitly promises "criterion benches gate the hot paths" — today they
measure but do not gate. A silent 2x regression in the MNA stamp/solve or the
`Value::parse` hot path (called on every circuit load and shell edit) would ship
unnoticed. Add a `bench` job that runs a fast, representative subset
(`cargo bench -p ohmforge-sim -p ohmforge-core -- --warm-up-time 0.2
--measurement-time 1.0 --sample-size 20`, the exact quick-pass the solve bench's
doc comment already documents), parses Criterion's JSON output, compares against
the baseline committed on `main`, and comments/fails on regressions beyond a
threshold. Use `benchmark-action/github-action-benchmark` (criterion mode) or
`critcmp` with `--save-baseline`. Because GitHub-hosted runners are noisy, gate
on a **generous threshold (e.g. +15–20% wall-clock, warn below that)** and treat
the number as a trend, not a contract; the asymptotic scaling benches
(`sparse_vs_dense`, `ordered_vs_natural`) are far more robust to noise than
absolute timings and should anchor any hard fail.

- **Effort:** M (one workflow job + a baseline-store strategy; the benches
  already exist and have a documented quick-pass invocation).
- **Risk:** Medium — runner noise causes flaky fails if the threshold is tight.
  Mitigate with a wide threshold, the quick-pass sample settings, `continue-on-error`
  as "warn" initially, and ratio-based scaling assertions over absolute ms.
- **Cross-crate coordination:** Low. Touches `.github/workflows/` and adds a
  `just bench-ci` recipe mirroring the invocation. No crate code changes.
- **Acceptance gate:** A PR that deliberately pessimizes `Value::parse` or the
  dense solve produces a visible CI warning/failure with a before/after delta;
  a no-op PR does not. Baseline JSON is stored (gh-pages branch or cache) and the
  trend is visible.

### 2. crates.io release automation (`cargo-release` + a tag-triggered publish workflow) — **you-gated**

**What + why.** Twelve crates are packaging-ready with names verified free, but
**none are on crates.io** and the entire publish is a manual 12-step walk down
`PUBLISHING.md`. For a project that is already public at v0.8.1 with a live demo,
"not actually installable via `cargo install`" is the single biggest adoption
gap. Automate it: add `cargo-release` config (`release.toml`) to drive the shared
workspace-version bump + changelog + tag in one command, and a
`.github/workflows/release.yml` triggered on `v*` tags that publishes in the
PUBLISHING.md order using `cargo publish --workspace` (which computes that order
automatically — PUBLISHING.md already names this as the convenience path) with
the per-crate dry-run as the fallback. The workflow also verifies the
path-vs-version precondition the doc flags (every `ohmforge-*` dep carries a
`version` — confirmed present in the root `Cargo.toml`
`[workspace.dependencies]`). This is **you-gated**: it requires a
`CARGO_REGISTRY_TOKEN` repository secret and the human decision to burn the
first-publish name reservations. Nothing below the token line can be done by CI
alone.

- **Effort:** M (workflow + `release.toml`; the ordering logic is already
  documented and encoded in `cargo publish --workspace`).
- **Risk:** Medium-High and partly **irreversible** — a published version/name
  cannot be unpublished (only yanked). Mitigate with mandatory `--dry-run` for
  all 12 crates as a required pre-step, a manual `workflow_dispatch` "dry-run
  only" mode, and publishing behind an environment with a required reviewer.
- **Cross-crate coordination:** High — this is inherently all 12 crates at once,
  sharing one workspace version. The shared-version model makes `cargo-release`'s
  workspace mode a natural fit.
- **Acceptance gate:** `workflow_dispatch` dry-run mode green-lights all 12
  crates' `cargo publish --dry-run`; a `v0.8.2` tag publishes the family in order
  with the index-propagation waits honored; `cargo install ohmforge` then yields a
  working `of`/`eesh`. **Blocked on:** the maintainer providing
  `CARGO_REGISTRY_TOKEN` and authorizing the name reservations.

### 3. Prebuilt per-OS binaries for `of` / `eesh` on GitHub Releases — **partly you-gated**

**What + why.** The `ohmforge` crate ships two binaries (`of`, `eesh`) but there
are **no downloadable artifacts** — a user without a Rust toolchain cannot try the
tool, and even with one, `cargo install` builds from source (slow; blocked until
#2 ships). Add a release job (naturally part of or adjacent to #2's
`release.yml`) that builds optimized binaries on a matrix — `x86_64`/`aarch64`
for Linux (gnu), macOS, and Windows — and uploads tarballs/zips + SHA256SUMS to
the GitHub Release. The CI already proves all of these targets build
(`aarch64` cross-job, the macOS/Windows test matrix), so the target list is
pre-validated; this just packages them. Prefer `cargo-dist` (generates the
workflow, checksums, and an installer script) or a hand-rolled matrix + `gh
release upload`. The release `[profile.release]` is already tuned (`lto="thin"`,
`codegen-units=1`).

- **Effort:** M (matrix build + upload; cross-compile linkers already proven in
  CI for aarch64).
- **Risk:** Low-Medium — the aarch64-apple and aarch64-linux cross steps need the
  right linkers (the CI aarch64 job shows the `gcc-aarch64-linux-gnu` pattern);
  macOS code-signing/notarization is out of scope (ship unsigned with a documented
  Gatekeeper note).
- **Cross-crate coordination:** Low — only the `ohmforge` binary crate is built;
  depends on tag cadence shared with #2.
- **Acceptance gate:** A tagged release carries six verified artifacts + a
  `SHA256SUMS` file; each unpacks to a runnable `of`/`eesh` on its platform.
  **Gated on** the same tag/release cadence as #2 (no registry token needed for
  binaries, so this can ship independently if desired).

### 4. Fuzzing + property tests for the parsers and solver

**What + why.** The ROADMAP's cross-cutting line promises "property tests on the
solver, fuzzing on parsers" — this is **not yet wired** (no `proptest`,
`arbitrary`, or `cargo-fuzz` anywhere in the tree). The attack/robustness surface
is concrete and hot: `Value::parse` (engineering notation — `4k7`, `2n2`, `1meg`,
signs), the `ohmforge-fmt` netlist parser, and `ohmforge-digital`'s `Expr::parse`
(HDL-lite boolean expressions) all take arbitrary text. Add (a) `proptest`
round-trip and invariant tests — e.g. `format(parse(x)) ≈ x`, parser never panics
on arbitrary UTF-8, and solver invariants like "op-point of a passive resistive
network is bounded by the source rails" / "a symmetric divider is exact"; and (b)
a `cargo-fuzz` target per parser, run in CI for a short bounded time
(`-max_total_time=60`) on PRs and longer on a nightly schedule. Fuzz/proptest
deps are dev-only, so the dependency-free shipped surface is preserved.

- **Effort:** S for `proptest` round-trip/invariant tests (highest value per
  hour); M to add `cargo-fuzz` targets + a scheduled CI job + a seed corpus.
- **Risk:** Low — new tests only; no production code changes. A found crash is a
  real bug surfaced, which is the point. Keep fuzz off the PR-blocking path
  (nightly + short smoke on PRs) to avoid flaky time-based fails.
- **Cross-crate coordination:** Low-Medium — independent additions in
  `ohmforge-core`, `ohmforge-fmt`, `ohmforge-digital`, `ohmforge-sim`; share one
  `proptest` version via `[workspace.dependencies]`.
- **Acceptance gate:** `cargo test` runs proptest cases in the three parser
  crates + solver-invariant tests in `ohmforge-sim`; `cargo fuzz run <target>
  -- -max_total_time=60` is green on a seed corpus in CI; a known-bad input
  (e.g. a pathological numeric literal) is captured as a regression seed.

### 5. CI quality signals: supply-chain hardening + code coverage

**What + why.** Two adjacent, cheap gaps. **Supply-chain:** there is no
`deny.toml`, no `cargo-audit`/`cargo-deny` job, and no `dependabot.yml`. The
runtime dependency footprint is tiny today (the compute core is dependency-free;
the binary pulls only `rustyline`), so the *security* exposure is low — but
`cargo-deny` also enforces **license compatibility** and **banned/duplicate
crates**, which becomes load-bearing the moment #2 publishes to crates.io under
Apache-2.0 (you must prove every transitive dep is license-compatible). Add a
`deny.toml` (advisories + licenses + bans + sources) and a `security` CI job, plus
`dependabot.yml` for `cargo` and `github-actions`. **Coverage:** there is no
coverage measurement. Add `cargo-llvm-cov` producing an lcov report uploaded to
Codecov (or an artifact), reported-not-enforced initially to establish a
baseline for the solver and parsers.

- **Effort:** S for both (two small workflow jobs + `deny.toml` +
  `dependabot.yml`).
- **Risk:** Low — `cargo-deny` may flag an existing advisory/license on first run
  (that's the value); start advisories as `warn` then promote to `deny`. Coverage
  is non-gating, so no flake risk.
- **Cross-crate coordination:** Low — repo-root config; no crate code changes.
- **Acceptance gate:** `cargo deny check` passes (or warns with a documented
  allowlist) in CI; Dependabot opens its first dependency PR; a coverage report is
  published per run with a visible workspace number. Promote `cargo-deny`
  advisories to a hard fail as a precondition of #2's first publish.

### 6. Expand the `ohmforge-digital` event-driven kernel

**What + why.** The digital kernel (`crates/ohmforge-digital`, ~2.7k LOC) is
already substantial and clean: 4-valued `Logic`, an integer-femtosecond
`EventQueue`, gates, flip-flops, ripple counters, tri-state buses, `TruthTable`,
an `Expr` parser, and `Rom`/`Ram` — most of the lib.rs roadmap's M1–M3. The
highest-leverage *correctness and usability* extensions remaining are: (a)
**inertial delay + pulse rejection** — the kernel is documented as transport-delay
only, so runt/glitch pulses shorter than a gate's delay propagate where real logic
would swallow them; (b) **setup/hold timing checks** on the sequential elements
(`seq.rs`) to flag timing violations as `X`, the standard gate-level-sim behavior;
(c) **VCD waveform export** so digital runs can be inspected in GTKWave/Surfer and
diffed — a big observability win and a natural golden-test substrate; and (d) a
**Criterion bench** for the event loop (there is none — benches today are
analog-only), which also feeds proposal #1. Keep the crate dependency-free (VCD
is a trivial text writer; the manifest already anticipates an `ohmforge-core` dep
only if buses need it).

- **Effort:** M for inertial delay + VCD export; L if setup/hold checks and a
  vector/bus-aware `Expr` are included.
- **Risk:** Medium — inertial delay changes event-scheduling semantics and could
  perturb existing digital test expectations; land it behind explicit per-gate
  delay-mode selection so default behavior is opt-in and existing tests stay green.
- **Cross-crate coordination:** Medium — `ohmforge-mcu` depends on
  `ohmforge-digital`; any `GateKind`/scheduling change must be validated against
  the MCU co-sim closed-loop tests (`crates/ohmforge-mcu/tests/closed_loop.rs`).
- **Acceptance gate:** a runt pulse shorter than a gate delay is demonstrably
  rejected under inertial mode and passed under transport mode; a digital run
  emits a VCD that opens in GTKWave and is added to a golden/determinism check;
  the MCU closed-loop tests remain green; a new digital event-loop bench is in
  the suite.

---

## Honorable mention / fold-in: MSRV & edition currency

`edition = "2021"` and `rust-version = "1.79"` are both conservative. Rust edition
**2024** is stable (since 1.85) and the toolchain has moved well past 1.79. This
isn't urgent — a low MSRV is a *feature* for a tool meant to run on Raspberry Pi
and older distros — but M10 should adopt an **explicit, documented MSRV policy**
(e.g. "MSRV = stable minus N releases, bumped only with a CHANGELOG note") rather
than leaving 1.79 as an accident. Evaluate an edition-2024 migration
(`cargo fix --edition`) as a separate, low-risk PR once the MSRV covers it. Effort
S; risk Low (raising MSRV excludes some users — hence the policy, not a reflex
bump). Fold the policy doc into `DEVELOPMENT.md`; the dedicated `msrv` CI job
already exists to enforce whatever number is chosen.

---

## Dependency / sequencing notes

- **#1 (perf CI)** and **#4 (fuzz/proptest)** and **#5 (deny/coverage)** are
  independent, low-risk, and shippable immediately — do these first.
- **#5's `cargo-deny` license gate is a precondition for #2** (don't publish to
  crates.io until license compatibility is proven).
- **#2 and #3 share a tag/release cadence** and should land together; **#2 is
  you-gated** on `CARGO_REGISTRY_TOKEN` + name-reservation authorization.
- **#6 must be validated against the `ohmforge-mcu` co-sim** and feeds a digital
  bench into **#1**.
