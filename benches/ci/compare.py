#!/usr/bin/env python3
# CP-PLAT-M10.1 — performance-regression gate.
#
# Dependency-light (stdlib only): parse Criterion's per-bench `estimate.json`
# under `target/criterion/<id>/new/`, diff the mean against the committed
# baseline in `benches/baseline.json`, and fail if any key bench is more than
# `threshold_pct` slower. GitHub-hosted runners are noisy, so the threshold is
# deliberately generous and the number is a trend guard, not a hard contract.
#
# Run it from the repo root AFTER running the benches, e.g.:
#   cargo bench -p ohmforge-core -p ohmforge-sim -- \
#       --warm-up-time 0.2 --measurement-time 1.0 --sample-size 20
#   python3 -I benches/ci/compare.py
#
# `--update` rewrites the baseline from the current Criterion output (use after
# an intentional, reviewed performance change), keeping the existing threshold.
#
# Exit codes: 0 = all key benches within threshold (or baseline updated);
#             1 = at least one regression beyond threshold;
#             2 = a key bench has no Criterion result (did you run the benches?).

import argparse
import json
import os
import sys

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BASELINE_PATH = os.path.join(REPO_ROOT, "benches", "baseline.json")
CRITERION_DIR = os.path.join(REPO_ROOT, "target", "criterion")


def estimate_path(bench_id):
    # Criterion sanitizes a bench id like "transient/rc_1000" into a single flat
    # directory ("transient_rc_1000"), with the latest sample's stats in
    # new/estimates.json (nanoseconds).
    sanitized = bench_id.replace("/", "_")
    return os.path.join(CRITERION_DIR, sanitized, "new", "estimates.json")


def read_mean_ns(bench_id):
    path = estimate_path(bench_id)
    if not os.path.isfile(path):
        return None
    with open(path, encoding="utf-8") as f:
        data = json.load(f)
    return float(data["mean"]["point_estimate"])


def load_baseline():
    with open(BASELINE_PATH, encoding="utf-8") as f:
        return json.load(f)


def fmt_ns(ns):
    if ns >= 1e9:
        return f"{ns / 1e9:8.3f} s "
    if ns >= 1e6:
        return f"{ns / 1e6:8.3f} ms"
    if ns >= 1e3:
        return f"{ns / 1e3:8.3f} us"
    return f"{ns:8.3f} ns"


def cmd_update(baseline):
    updated = dict(baseline)
    benches = {}
    missing = []
    for bench_id in baseline["benches"]:
        mean = read_mean_ns(bench_id)
        if mean is None:
            missing.append(bench_id)
        else:
            benches[bench_id] = round(mean, 3)
    if missing:
        print("ERROR: no Criterion result for: " + ", ".join(missing), file=sys.stderr)
        print("Run the benches first (see this script's header).", file=sys.stderr)
        return 2
    updated["benches"] = benches
    with open(BASELINE_PATH, "w", encoding="utf-8") as f:
        json.dump(updated, f, indent=2)
        f.write("\n")
    print(f"Baseline updated ({len(benches)} benches) -> {BASELINE_PATH}")
    return 0


def cmd_compare(baseline):
    threshold = float(baseline["threshold_pct"])
    print(f"Performance-regression gate (threshold: +{threshold:.0f}% slower)\n")
    print(f"  {'bench':<24} {'baseline':>12} {'current':>12} {'delta':>9}  status")
    print("  " + "-" * 70)

    regressions = []
    missing = []
    for bench_id, base_ns in baseline["benches"].items():
        base_ns = float(base_ns)
        cur_ns = read_mean_ns(bench_id)
        if cur_ns is None:
            missing.append(bench_id)
            print(f"  {bench_id:<24} {fmt_ns(base_ns):>12} {'MISSING':>12} {'':>9}  NO DATA")
            continue
        delta_pct = (cur_ns - base_ns) / base_ns * 100.0
        if delta_pct > threshold:
            status = "REGRESSION"
            regressions.append((bench_id, delta_pct))
        else:
            status = "ok"
        print(f"  {bench_id:<24} {fmt_ns(base_ns):>12} {fmt_ns(cur_ns):>12} {delta_pct:>+8.1f}%  {status}")

    print()
    if missing:
        print(f"ERROR: {len(missing)} key bench(es) had no Criterion result; "
              "did you run the benches?", file=sys.stderr)
        return 2
    if regressions:
        print(f"FAIL: {len(regressions)} bench(es) regressed beyond "
              f"+{threshold:.0f}%:", file=sys.stderr)
        for bench_id, delta_pct in regressions:
            print(f"  - {bench_id}: {delta_pct:+.1f}%", file=sys.stderr)
        return 1
    print("PASS: all key benches within threshold.")
    return 0


def main():
    ap = argparse.ArgumentParser(description="Compare Criterion means against the committed baseline.")
    ap.add_argument("--update", action="store_true",
                    help="rewrite the baseline from the current Criterion run")
    args = ap.parse_args()

    baseline = load_baseline()
    if args.update:
        return cmd_update(baseline)
    return cmd_compare(baseline)


if __name__ == "__main__":
    sys.exit(main())
