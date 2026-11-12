#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target}"
if [[ -n "${VESPER_MACRO_PREVIEW_BINARY:-}" ]]; then
  binary="$VESPER_MACRO_PREVIEW_BINARY"
else
  cargo build --locked -p vesper --bin vesper
  binary="$CARGO_TARGET_DIR/debug/vesper"
fi
if [[ -f "$binary.exe" ]]; then binary="$binary.exe"; fi
scratch="$(mktemp -d "${TMPDIR:-/tmp}/vesper-macro-preview.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
python scripts/check-macro-preview.py --binary "$binary" --work "$scratch"
