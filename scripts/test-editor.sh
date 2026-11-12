#!/usr/bin/env bash
# Actual framed editor acceptance and self-checking source/protocol examples.
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target}"
cargo build --locked -p vesper --bin vesper
binary="$CARGO_TARGET_DIR/debug/vesper"
# MSYS -f may resolve the extensionless alias, while native Python needs .exe.
if [[ -f "$binary.exe" ]]; then
  binary="$binary.exe"
fi
scratch="$(mktemp -d "${TMPDIR:-/tmp}/vesper-editor.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
python scripts/check-editor.py --binary "$binary" --work "$scratch"
cargo run --locked -p vesper-span --example source_edits
cargo run --locked -p vesper-lsp --example protocol_documents
