#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/bazel-lib.sh
if [[ -n "${VESPER_SOURCE_MACRO_TUPLES_BINARY:-}" ]]; then
  binary="$VESPER_SOURCE_MACRO_TUPLES_BINARY"
else
  binary="$(bazel_executable //:bin/vesper)"
fi
if [[ -f "$binary.exe" ]]; then binary="$binary.exe"; fi
scratch="$(mktemp -d "${TMPDIR:-/tmp}/vesper-source-macro-tuples.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
python scripts/check-source-macro-tuples.py --binary "$binary" --work "$scratch"
