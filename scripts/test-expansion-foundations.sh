#!/usr/bin/env bash
# Host-library acceptance examples; no device/server/platform performance claim.
set -euo pipefail
cd "$(dirname "$0")/.."
for pair in \
  'vesper-serde bounded_json' \
  'vesper-beacon bounded_trace' \
  'vesper-quarry quarry' \
  'vesper-tensor cpu_fusion' \
  'vesper-flux incremental_join' \
  'vesper-aether snapshot_pages'; do
  read -r package example <<< "$pair"
  cargo run --locked -p "$package" --example "$example"
done
