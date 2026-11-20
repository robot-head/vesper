#!/usr/bin/env bash
# Free disk on a recycled BuildBuddy runner before "Test all targets" (#1200).
#
# Runners are reused between runs ("Syncing existing repo"), but nothing
# removes what earlier runs left in the temporary directory: the Logos42
# checker each run builds there, and the artifact roots of tests that were
# killed (time limit, full disk) or that keep their root on failure. Bazel does
# not set TMPDIR for tests, so std::env::temp_dir() roots land there too. Each
# failed run then left more behind, and the next run on that runner ran out of
# space sooner.
#
# This removes those leftovers. If free space is still below
# VESPER_CI_MIN_FREE_GB, it also drops Bazel's output tree with `bazel clean`;
# the remote cache refills it. Nothing here skips or changes a test.
#
# Usage: reclaim-disk.sh [report]   (`report` only prints disk use)
set -uo pipefail
cd "$(dirname "$0")/../.."

min_free_gb="${VESPER_CI_MIN_FREE_GB:-30}"
tmp="${TMPDIR:-/tmp}"

free_gb() {
  df -Pk . | awk 'NR == 2 { printf "%d", $4 / 1048576 }'
}

report() {
  echo "reclaim-disk: $1"
  df -h . "$tmp" 2>/dev/null | awk '!seen[$0]++'
  output_base="$(bazel info output_base 2>/dev/null || true)"
  timeout 120 du -sh "$tmp" "$HOME" ${output_base:+"$output_base"} 2>/dev/null || true
}

if [[ "${1:-}" == report ]]; then
  report "disk use"
  exit 0
fi

report "before cleanup"

# Only the names this repository's CI and tests create, and only the current
# user's: the Logos42 checker (vesper-logos42-ci.*), test roots (vesper-*,
# stage1-*, blake3-*, native-*), and tempfile-crate directories (.tmp*).
find "$tmp" -mindepth 1 -maxdepth 1 -user "$(id -u)" \
  \( -name 'vesper-*' -o -name 'stage1-*' -o -name 'blake3-*' \
     -o -name 'native-*' -o -name '.tmp*' \) \
  -exec rm -rf {} + 2>/dev/null || true

free="$(free_gb)"
if (( free < min_free_gb )); then
  echo "reclaim-disk: ${free} GB free, below ${min_free_gb} GB; running bazel clean"
  bazel clean || true
fi

report "after cleanup"
echo "reclaim-disk: $(free_gb) GB free"
