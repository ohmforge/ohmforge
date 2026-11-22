# ohmforge task runner. `just` -> lists tasks. Install: cargo install just
# These mirror CI so "green locally" == "green in CI".

set shell := ["bash", "-uc"]

# List available tasks.
default:
    @just --list

# The full local gate — run before pushing. Mirrors CI.
check: fmt-check clippy test doc
    @echo "all checks passed"

# Format the whole workspace.
fmt:
    cargo fmt --all

# Verify formatting (CI mode).
fmt-check:
    cargo fmt --all --check

# Clippy with warnings-as-errors, across all targets.
clippy:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# Run the whole test suite.
test:
    cargo test --workspace --all-features

# Test a single crate, e.g. `just test-one ohmforge-core`.
test-one crate:
    cargo test -p {{crate}}

# Build docs the way CI does (warnings are errors).
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features

# Confirm the compute core still compiles to WASM (no I/O leak).
wasm:
    cargo check -p ohmforge-core -p ohmforge-sim --target wasm32-unknown-unknown

# Run benchmarks (criterion).
bench:
    cargo bench --workspace

# The perf-regression gate (CP-PLAT-M10.1): run the hot-path benches with the
# quick-pass settings and diff their means against benches/baseline.json. Mirrors
# the `perf` CI workflow. Append `--update` args via `just bench-ci-update`.
bench-ci:
    cargo bench -p ohmforge-core --bench parse -- --warm-up-time 0.2 --measurement-time 1.0 --sample-size 20 'value_parse/mix'
    cargo bench -p ohmforge-sim --bench solve -- --warm-up-time 0.2 --measurement-time 1.0 --sample-size 20 'op_point/ladder_16|transient/'
    python3 -I benches/ci/compare.py

# Regenerate the committed perf baseline from a fresh quick-pass run (use only
# after an intentional, reviewed performance change).
bench-ci-update:
    cargo bench -p ohmforge-core --bench parse -- --warm-up-time 0.2 --measurement-time 1.0 --sample-size 20 'value_parse/mix'
    cargo bench -p ohmforge-sim --bench solve -- --warm-up-time 0.2 --measurement-time 1.0 --sample-size 20 'op_point/ladder_16|transient/'
    python3 -I benches/ci/compare.py --update

# Run the app.
run *ARGS:
    cargo run -p ohmforge -- {{ARGS}}

# Verify against the MSRV toolchain (needs `rustup toolchain add 1.79.0`).
msrv:
    cargo +1.79.0 build --workspace

# --- Worktree helpers (see DEVELOPMENT.md) ---------------------------------

# Create the per-track worktrees under ../ohmforge-worktrees/.
worktrees-init:
    #!/usr/bin/env bash
    set -euo pipefail
    for track in sim-core boards ui-shell ai-fmt; do
        dir="../ohmforge-worktrees/$track"
        if git show-ref --verify --quiet "refs/heads/track/$track"; then
            git worktree add "$dir" "track/$track" 2>/dev/null || echo "exists: $dir"
        else
            git worktree add -b "track/$track" "$dir" 2>/dev/null || echo "exists: $dir"
        fi
    done
    git worktree list

# Show the current worktrees.
worktrees:
    git worktree list
