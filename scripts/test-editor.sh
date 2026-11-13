#!/usr/bin/env bash
# Actual framed editor acceptance and self-checking source/protocol examples.
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/bazel-lib.sh
binary="$(bazel_executable //:bin/vesper)"
# MSYS -f may resolve the extensionless alias, while native Python needs .exe.
if [[ -f "$binary.exe" ]]; then
  binary="$binary.exe"
fi
scratch="$(mktemp -d "${TMPDIR:-/tmp}/vesper-editor.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
python scripts/check-editor.py --binary "$binary" --work "$scratch"
bazel_exec "//crates/vesper-span:example/source_edits"
bazel_exec "//crates/vesper-lsp:example/protocol_documents"
