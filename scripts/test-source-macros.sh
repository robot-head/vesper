#!/usr/bin/env bash
# Explicit source expansion only; execute only the checked normal positive artifacts.
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/bazel-lib.sh
if [[ -n "${VESPER_SOURCE_MACROS_BINARY:-}" ]]; then
  binary="$VESPER_SOURCE_MACROS_BINARY"
else
  binary="$(bazel_executable //:bin/vesper)"
fi
if [[ -f "$binary.exe" ]]; then
  binary="$binary.exe"
fi
scratch="$(mktemp -d "${TMPDIR:-/tmp}/vesper-source-macros.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
python scripts/check-source-macros.py --binary "$binary" --work "$scratch"
