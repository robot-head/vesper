#!/usr/bin/env bash
# Actual negative check/JSON/terminal and framed-LSP acceptance; no source fixes.
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/bazel-lib.sh
binary="$(bazel_executable //:bin/vesper)"
# Check the .exe candidate first even if MSYS resolves the bare-name alias.
if [[ -f "$binary.exe" ]]; then
  binary="$binary.exe"
fi
scratch="$(mktemp -d "${TMPDIR:-/tmp}/vesper-diagnostics.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
python scripts/check-diagnostics.py --binary "$binary" --work "$scratch"
bazel_exec "//crates/vesper-diagnostics:example/causal_diagnostics"
