#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/bazel-lib.sh
if [[ -n "${VESPER_MACRO_PREVIEW_BINARY:-}" ]]; then
  binary="$VESPER_MACRO_PREVIEW_BINARY"
else
  binary="$(bazel_executable //:bin/vesper)"
fi
if [[ -f "$binary.exe" ]]; then binary="$binary.exe"; fi
scratch="$(mktemp -d "${TMPDIR:-/tmp}/vesper-macro-preview.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
python scripts/check-macro-preview.py --binary "$binary" --work "$scratch"
