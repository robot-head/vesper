#!/usr/bin/env bash
# Actual locked compiler/stdio acceptance, with an explicit read-only binary option.
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target}"
if [[ -n "${VESPER_RECOVERY_BINARY:-}" ]]; then
  binary="$VESPER_RECOVERY_BINARY"
else
  cargo build --locked -p vesper --bin vesper
  binary="$CARGO_TARGET_DIR/debug/vesper"
fi
# Prefer the native executable even when MSYS resolves an extensionless alias.
if [[ -f "$binary.exe" ]]; then
  binary="$binary.exe"
fi
scratch="$(mktemp -d "${TMPDIR:-/tmp}/vesper-recovery.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
python scripts/check-recovery.py --binary "$binary" --work "$scratch"
