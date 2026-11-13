#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."

if [[ $# != 1 || -z "${VESPER_LEAN:-}" ]]; then
  echo 'usage: VESPER_LEAN=actual-lean prepare-lean4lean42.sh ABSENT_DIRECTORY' >&2
  exit 2
fi

. scripts/bazel-lib.sh
driver="$(bazel_executable //:bin/lean4lean42)"
python3 tools/lean4lean42/setup.py \
  --directory "$1" \
  --driver "$driver" \
  --lean "$VESPER_LEAN" \
  --lake "$(dirname "$VESPER_LEAN")/lake" \
  --git "$(command -v git)"
