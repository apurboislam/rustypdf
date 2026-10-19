#!/usr/bin/env bash
# Renders every template's sample.json and checks each PDF with veraPDF 3b.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --quiet
out=target/pdfa
rm -rf "$out" && mkdir -p "$out"
pdfs=()
for dir in templates/*/; do
  name=$(basename "$dir")
  [[ $name == _* || ! -f $dir/main.typ || ! -f $dir/sample.json ]] && continue
  FONTS_DIR=fonts ./target/debug/rustypdf render "$dir" "$dir/sample.json" -o "$out/$name.pdf"
  pdfs+=("$out/$name.pdf")
done
[ ${#pdfs[@]} -gt 0 ] || { echo "no templates with sample.json found" >&2; exit 1; }
scripts/verapdf.sh "${pdfs[@]}"
