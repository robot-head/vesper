#!/usr/bin/env bash
# Actual host source tooling/CLI acceptance; elapsed time is never a speed gate.
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target/source-tooling}"

cargo build --locked -p vesper --bin vesper
binary="$CARGO_TARGET_DIR/debug/vesper"
# MSYS can report an extensionless executable path as existing via its .exe
# fallback, while native Python needs the actual filename. Prefer that file.
if [[ -f "$binary.exe" ]]; then
  binary="$binary.exe"
fi
scratch="$(mktemp -d "${TMPDIR:-/tmp}/vesper-source-tooling.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
python scripts/check-source-tooling.py --binary "$binary" --root "$PWD" --work "$scratch"

for pair in \
  'vesper-grammar grammar' \
  'vesper-fmt canonical' \
  'vesper-bench paired_callbacks'; do
  read -r package example <<< "$pair"
  cargo run --locked -p "$package" --example "$example"
done
