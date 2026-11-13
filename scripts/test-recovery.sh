#!/usr/bin/env bash
# Actual locked compiler/stdio acceptance, with an explicit read-only binary option.
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/bazel-lib.sh
if [[ -n "${VESPER_RECOVERY_BINARY:-}" ]]; then
  binary="$VESPER_RECOVERY_BINARY"
else
  binary="$(bazel_executable //:bin/vesper)"
fi
# Prefer the native executable even when MSYS resolves an extensionless alias.
if [[ -f "$binary.exe" ]]; then
  binary="$binary.exe"
fi
scratch="$(mktemp -d "${TMPDIR:-/tmp}/vesper-recovery.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
python scripts/check-recovery.py --binary "$binary" --work "$scratch"
