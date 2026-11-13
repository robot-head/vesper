#!/usr/bin/env bash
# Real codec/policy/PMU CLI acceptance. No performance or PMU-availability gate.
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/bazel-lib.sh

binary="$(bazel_executable //:bin/vesper)"
# MSYS -f can resolve the extensionless .exe alias. Native Python cannot; always
# prefer the actual executable filename whenever it exists.
if [[ -f "$binary.exe" ]]; then
  binary="$binary.exe"
fi
scratch="$(mktemp -d "${TMPDIR:-/tmp}/vesper-canonical-protocols.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
python scripts/check-canonical-protocols.py --binary "$binary" --root "$PWD" --work "$scratch"

for example in protobuf der ipc; do
  bazel_exec "//crates/vesper-grammar:example/$example"
done
if bazel_exec "//crates/vesper-bench-pmu:example/probe"; then
  echo 'PMU probe completed with actual checked positive counters; no speed threshold applied.'
else
  code=$?
  if [[ "$code" -ne 2 ]]; then
    echo "PMU probe failed unexpectedly (exit $code)." >&2
    exit "$code"
  fi
  echo 'PMU probe reported unavailable (exit 2); no hardware-success or zero-count claim.'
fi
