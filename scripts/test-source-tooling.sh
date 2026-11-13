#!/usr/bin/env bash
# Actual host source tooling/CLI acceptance; elapsed time is never a speed gate.
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/bazel-lib.sh

binary="$(bazel_executable //:bin/vesper)"
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
  bazel_exec "//crates/$package:example/$example"
done
