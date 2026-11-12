#!/usr/bin/env bash
# Host-library acceptance examples; no device/server/platform performance claim.
set -euo pipefail
cd "$(dirname "$0")/.."
for pair in \
  'vesper-serde bounded_json' \
  'vesper-serde binary_codecs' \
  'vesper-beacon bounded_trace' \
  'vesper-beacon-arrow bounded_export' \
  'vesper-quarry quarry' \
  'vesper-quarry quarry_pages' \
  'vesper-tensor cpu_fusion' \
  'vesper-tensor kv_pages' \
  'vesper-flux incremental_join' \
  'vesper-flux checkpoint_resume' \
  'vesper-aether snapshot_pages' \
  'vesper-aether packed_queue'; do
  read -r package example <<< "$pair"
  cargo run --locked -p "$package" --example "$example"
done
