#!/usr/bin/env bash
# Actual negative check/JSON/terminal and framed-LSP acceptance; no source fixes.
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target}"
cargo build --locked -p vesper --bin vesper
binary="$CARGO_TARGET_DIR/debug/vesper"
# Check the .exe candidate first even if MSYS resolves the bare-name alias.
if [[ -f "$binary.exe" ]]; then
  binary="$binary.exe"
fi
scratch="$(mktemp -d "${TMPDIR:-/tmp}/vesper-diagnostics.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
python scripts/check-diagnostics.py --binary "$binary" --work "$scratch"
cargo run --locked -p vesper-diagnostics --example causal_diagnostics
