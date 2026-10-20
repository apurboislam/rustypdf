#!/usr/bin/env bash
# Runs the criterion render benchmarks and compares them with the saved
# `main` baseline (target/criterion/*/*/main, restored from the CI cache).
#
#   scripts/bench-ci.sh            compare with `main`; warn on a slowdown above THRESHOLD
#   scripts/bench-ci.sh --save     run and save the results as the new `main` baseline
#
# A regression is a warning (GitHub `::warning::` annotation), not a failure:
# shared CI runners are too noisy to gate merges on timing. THRESHOLD is the
# relative change of the median, default 0.20 (20%).
set -euo pipefail
cd "$(dirname "$0")/.."

THRESHOLD=${THRESHOLD:-0.20}
args=(--noplot --warm-up-time 2 --measurement-time 5)
if [[ ${1:-} == --save ]]; then
  cargo bench --bench render -- "${args[@]}" --save-baseline main
  exit 0
fi

if ! compgen -G "target/criterion/*/*/main" >/dev/null; then
  echo "::notice::No benchmark baseline from main yet; recording results only."
  cargo bench --bench render -- "${args[@]}"
  exit 0
fi
# Drop comparisons left over from earlier runs. --baseline-lenient: benchmarks
# added since the baseline are not an error.
rm -rf target/criterion/*/*/change
cargo bench --bench render -- "${args[@]}" --baseline-lenient main

python3 - "$THRESHOLD" <<'EOF'
import json, os, pathlib, sys
threshold = float(sys.argv[1])
rows, regressions = [], []
for change in sorted(pathlib.Path("target/criterion").glob("*/*/change/estimates.json")):
    name = "/".join(change.parts[-4:-2])
    median = json.loads(change.read_text())["median"]
    delta, ci = median["point_estimate"], median["confidence_interval"]
    rows.append(f"| {name} | {delta:+.1%} | {ci['lower_bound']:+.1%} … {ci['upper_bound']:+.1%} |")
    # Only when the whole confidence interval is above the threshold.
    if ci["lower_bound"] > threshold:
        regressions.append((name, delta))
summary = ["| Benchmark | Median change vs main | 95% CI |", "|---|---|---|", *rows]
print("\n".join(summary))
out = pathlib.Path(os.environ.get("GITHUB_STEP_SUMMARY", "/dev/null"))
with out.open("a") as f:
    f.write("### Render benchmarks\n\n" + "\n".join(summary) + "\n")
for name, delta in regressions:
    print(f"::warning title=Benchmark regression::{name} is {delta:.0%} slower than main")
EOF
