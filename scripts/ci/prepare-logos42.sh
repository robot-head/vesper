#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."

if [[ $# != 1 || -z "${VESPER_LEAN:-}" ]]; then
  echo 'usage: VESPER_LEAN=actual-lean prepare-logos42.sh ABSENT_DIRECTORY' >&2
  exit 2
fi

. scripts/bazel-lib.sh
driver="$(bazel_executable //:bin/logos42)"
if [[ -n "${VESPER_CARGO:-}" ]]; then
  checker_cargo="$VESPER_CARGO"
elif command -v rustup >/dev/null 2>&1; then
  checker_cargo="$(rustup which cargo)"
else
  checker_cargo="$(command -v cargo)"
fi
python3 tools/logos42/setup.py \
  --directory "$1" \
  --driver "$driver" \
  --lean "$VESPER_LEAN" \
  --lake "$(dirname "$VESPER_LEAN")/lake" \
  --git "$(command -v git)" \
  --cargo "$checker_cargo"
