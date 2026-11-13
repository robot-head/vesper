#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/bazel-lib.sh
if [[ -n "${VESPER_SOURCE_MACRO_PARAMETERS_BINARY:-}" ]]; then
  binary="$VESPER_SOURCE_MACRO_PARAMETERS_BINARY"
else
  binary="$(bazel_executable //:bin/vesper)"
fi
if [[ -f "$binary.exe" ]]; then binary="$binary.exe"; fi
scratch="$(mktemp -d "${TMPDIR:-/tmp}/vesper-source-macro-parameters.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
python scripts/check-source-macro-parameters.py --binary "$binary" --work "$scratch"
