#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/bazel-lib.sh
if [[ -n "${VESPER_DOC_LINKS_BINARY:-}" ]]; then
  binary="$VESPER_DOC_LINKS_BINARY"
else
  binary="$(bazel_executable //:bin/vesper)"
fi
if [[ -f "$binary.exe" ]]; then binary="$binary.exe"; fi
python scripts/check-doc-links.py --binary "$binary"
