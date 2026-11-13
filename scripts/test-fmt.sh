#!/usr/bin/env bash
# Check that every tracked .ves file (minus scripts/fmt-exclude.txt) is
# canonically formatted by the real `vesper fmt --check`.
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/bazel-lib.sh
if [[ -n "${VESPER_FMT_BINARY:-}" ]]; then
  binary="$VESPER_FMT_BINARY"
else
  binary="$(bazel_executable //:bin/vesper)"
fi
if [[ -f "$binary.exe" ]]; then binary="$binary.exe"; fi
python scripts/check-fmt.py --binary "$binary"
