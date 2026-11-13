#!/usr/bin/env bash
# Explicit source expansion only; execute only the checked normal positive artifacts.
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target}"
if [[ -n "${VESPER_SOURCE_MACROS_BINARY:-}" ]]; then
  binary="$VESPER_SOURCE_MACROS_BINARY"
else
  cargo build --locked -p vesper --bin vesper
  binary="$CARGO_TARGET_DIR/debug/vesper"
fi
if [[ -f "$binary.exe" ]]; then
  binary="$binary.exe"
fi
scratch="$(mktemp -d "${TMPDIR:-/tmp}/vesper-source-macros.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
python scripts/check-source-macros.py --binary "$binary" --work "$scratch"
