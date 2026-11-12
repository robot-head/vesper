#!/usr/bin/env bash
# Real codec/policy/PMU CLI acceptance. No performance or PMU-availability gate.
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
# Share the ordinary CI cache; callers can still select a private build target.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target}"

cargo build --locked -p vesper --bin vesper
binary="$CARGO_TARGET_DIR/debug/vesper"
# MSYS -f can resolve the extensionless .exe alias. Native Python cannot; always
# prefer the actual executable filename whenever it exists.
if [[ -f "$binary.exe" ]]; then
  binary="$binary.exe"
fi
scratch="$(mktemp -d "${TMPDIR:-/tmp}/vesper-canonical-protocols.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
python scripts/check-canonical-protocols.py --binary "$binary" --root "$PWD" --work "$scratch"

for example in protobuf der ipc; do
  cargo run --locked -p vesper-grammar --example "$example"
done
if cargo run --locked -p vesper-bench-pmu --example probe; then
  echo 'PMU probe completed with actual checked positive counters; no speed threshold applied.'
else
  code=$?
  if [[ "$code" -ne 2 ]]; then
    echo "PMU probe failed unexpectedly (exit $code)." >&2
    exit "$code"
  fi
  echo 'PMU probe reported unavailable (exit 2); no hardware-success or zero-count claim.'
fi
