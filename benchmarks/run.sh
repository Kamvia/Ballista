#!/usr/bin/env bash
# File: ballista/benchmarks/run.sh
# Same workload for both tools: N users, ramp over 5s, 20s total, 1 request per user per second.
# Requires: release builds (cargo build --release --bin ballista --example demo_target), locust, psutil.
set -euo pipefail
cd "$(dirname "$0")"
ROOT=..
BIN=$ROOT/target/release/ballista
TARGET=http://127.0.0.1:8080
USERS=${USERS:-"100 1000 5000 10000"}
OUT=${OUT:-results}
mkdir -p "$OUT"

"$ROOT/target/release/examples/demo_target" > "$OUT/target.log" 2>&1 &
TARGET_PID=$!
trap 'kill $TARGET_PID 2>/dev/null || true' EXIT
sleep 1

for N in $USERS; do
  R=$((N / 5))
  echo "## users=$N"
  python3 measure.py ballista 16 "$BIN" run plan.toml -u "$N" -r "$R" -t 20s --quiet --json "$OUT/ballista_$N.json"
  python3 report_stats.py "$OUT/ballista_$N.json"
  for variant in "locust-http Reader" "locust-fast FastReader" "locust-fast-2proc FastReader --processes 2"; do
    set -- $variant
    label=$1; shift
    python3 measure.py "$label" 16 locust -f locustfile.py "$@" --headless -H "$TARGET" -u "$N" -r "$R" -t 20s --csv "$OUT/${label}_$N" --only-summary
    python3 locust_stats.py "$OUT/${label}_${N}_stats.csv"
  done
done
