#!/usr/bin/env bash
# Renders every template's sample.json and checks each PDF with veraPDF 3b.
# Sample photo URLs (http://cdn.rustypdf.test/...) are served from
# tests/fixtures/photos by a local mock server, mapped with the dev-only
# `render --resolve` flag (the service's SSRF guard is unchanged).
set -euo pipefail
cd "$(dirname "$0")/.."

PORT=${MOCK_IMAGE_PORT:-8765}
cargo build --quiet
python3 -m http.server "$PORT" --bind 127.0.0.1 --directory tests/fixtures/photos >/dev/null 2>&1 &
server=$!
trap 'kill $server 2>/dev/null || true' EXIT
for _ in $(seq 1 50); do
  (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null && break
  sleep 0.1
done

out=target/pdfa
rm -rf "$out" && mkdir -p "$out"
pdfs=()
for dir in templates/*/; do
  name=$(basename "$dir")
  [[ $name == _* || ! -f $dir/main.typ || ! -f $dir/sample.json ]] && continue
  FONTS_DIR=fonts ./target/debug/rustypdf render "$dir" "$dir/sample.json" -o "$out/$name.pdf" \
    --resolve "cdn.rustypdf.test=127.0.0.1:$PORT"
  pdfs+=("$out/$name.pdf")
done
[ ${#pdfs[@]} -gt 0 ] || { echo "no templates with sample.json found" >&2; exit 1; }
scripts/verapdf.sh "${pdfs[@]}"
