#!/usr/bin/env bash
# Load-tests the Docker image with k6 (BUILD.md P8; results in
# docs/performance.md).
#
#   scripts/loadtest.sh [warm|cold|soak]
#
#   warm  the four sector invoices in turn. Retail product photos use the
#         preloaded fallback file, which costs what a cached URL image costs
#         (bytes already in memory).
#   cold  the retail invoice with its photos sent as data: URIs, which are
#         decoded and resized on every request (a cache miss without the
#         network). Real URL fetches cannot be load-tested against the
#         container: the SSRF guard rightly refuses every local mock server.
#         `cargo bench -- images/cold` measures that path in process.
#   soak  `warm` for DURATION (default 30m), recording the container's memory
#         every 30 s to target/loadtest/memory.csv.
#
# Env: IMAGE (rustypdf:latest), CPUS (1; also MAX_CONCURRENCY), VUS (1),
# DURATION (30s, soak 30m), PORT (18080). Needs docker, k6 and python3.
set -euo pipefail
cd "$(dirname "$0")/.."

scenario=${1:-warm}
IMAGE=${IMAGE:-rustypdf:latest}
CPUS=${CPUS:-1}
VUS=${VUS:-1}
PORT=${PORT:-18080}
case $scenario in
  warm | cold) DURATION=${DURATION:-30s} ;;
  soak) DURATION=${DURATION:-30m} ;;
  *) echo "usage: $0 [warm|cold|soak]" >&2; exit 2 ;;
esac
command -v k6 >/dev/null || { echo "k6 not found: https://grafana.com/docs/k6/latest/set-up/install-k6/" >&2; exit 1; }

out=target/loadtest
mkdir -p "$out"
python3 - "$out" <<'EOF'
import base64, json, pathlib, sys
out = pathlib.Path(sys.argv[1])
def sample(name):
    return json.loads(pathlib.Path(f"templates/{name}/sample.json").read_text())
warm = {n: sample(n) for n in ["invoice-retail", "invoice-healthcare", "invoice-logistics", "invoice-construction"]}
for item in warm["invoice-retail"]["items"]:
    item.pop("photo", None)
cold = sample("invoice-retail")
for item in cold["items"]:
    url = item.get("photo", "")
    path = pathlib.Path("tests/fixtures/photos") / url.removeprefix("http://cdn.rustypdf.test/")
    if url and path.is_file():
        item["photo"] = "data:image/jpeg;base64," + base64.b64encode(path.read_bytes()).decode()
    else:
        item.pop("photo", None)
(out / "warm.json").write_text(json.dumps(warm))
(out / "cold.json").write_text(json.dumps({"invoice-retail": cold}))
EOF
bodies=$out/warm.json
[[ $scenario == cold ]] && bodies=$out/cold.json

name=rustypdf-loadtest
docker rm -f "$name" >/dev/null 2>&1 || true
trap 'docker rm -f "$name" >/dev/null 2>&1 || true' EXIT
docker run -d --name "$name" --read-only --cpus="$CPUS" -e MAX_CONCURRENCY="$CPUS" \
  -e HOT_RELOAD=false -p "$PORT:8080" "$IMAGE" >/dev/null
for _ in $(seq 1 500); do
  curl -sf "localhost:$PORT/readyz" >/dev/null && break
  sleep 0.02
done
# Cold start: process start ("starting rustypdf") to "listening", from the
# service's own JSON log timestamps (excludes container creation).
docker logs "$name" 2>&1 | python3 -c '
import json, sys
from datetime import datetime
ts = {}
for line in sys.stdin:
    try: e = json.loads(line)
    except ValueError: continue
    msg = e.get("fields", {}).get("message")
    if msg in ("starting rustypdf", "listening"):
        ts[msg] = datetime.fromisoformat(e["timestamp"].replace("Z", "+00:00"))
ms = (ts["listening"] - ts["starting rustypdf"]).total_seconds() * 1000
print(f"cold start: {ms:.0f} ms")
'
docker stats --no-stream --format 'memory after start: {{.MemUsage}}' "$name"

if [[ $scenario == soak ]]; then
  echo "elapsed_s,mem" >"$out/memory.csv"
  (
    start=$(date +%s)
    while docker inspect "$name" >/dev/null 2>&1; do
      echo "$(($(date +%s) - start)),$(docker stats --no-stream --format '{{.MemUsage}}' "$name" | cut -d/ -f1)" >>"$out/memory.csv"
      sleep 30
    done
  ) &
fi

BASE_URL="http://localhost:$PORT" BODIES="$PWD/$bodies" VUS=$VUS DURATION=$DURATION \
  k6 run --quiet scripts/loadtest.js
docker stats --no-stream --format 'memory at the end: {{.MemUsage}}' "$name"
[[ $scenario == soak ]] && cat "$out/memory.csv"
exit 0
