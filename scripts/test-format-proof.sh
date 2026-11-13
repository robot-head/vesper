#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/bazel-lib.sh
if [[ -n "${VESPER_FORMAT_PROOF_BINARY:-}" ]]; then
  binary="$VESPER_FORMAT_PROOF_BINARY"
else
  binary="$(bazel_executable //:bin/vesper)"
fi
if [[ -f "$binary.exe" ]]; then binary="$binary.exe"; fi
scratch="$(mktemp -d "${TMPDIR:-/tmp}/vesper-format-proof.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
python scripts/check-format-proof.py --binary "$binary" --work "$scratch"
