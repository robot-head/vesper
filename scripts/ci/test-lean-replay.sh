#!/usr/bin/env bash
# Execute the replay test targets without expanding the entire Bazel build.
set -euo pipefail
cd "$(dirname "$0")/../.."

if [[ -n "$(python3 scripts/ci/lean_replay_scope.py || true)" ]]; then
  exit 0
fi

labels="$(bazel query --config=ci 'attr(tags, "(^|[^a-zA-Z0-9_-])lean_replay([^a-zA-Z0-9_-]|$)", tests(//...))')"
if [[ -z "$labels" ]]; then
  echo "No Lean replay test targets found" >&2
  exit 1
fi
mapfile -t replay_targets <<< "$labels"

scripts/ci/install-logos-rust.sh
export VESPER_LEAN="$(cat .ci-lean-path)"
export VESPER_CARGO="$(cat .ci-logos-cargo-path)"
checker_dir="$(mktemp -d /tmp/vesper-logos42-ci.XXXXXX)"
export VESPER_LOGOS42_ROOT="$checker_dir/checker"
scripts/ci/reclaim-disk.sh watch &
disk_watch=$!
trap 'kill "$disk_watch" 2>/dev/null || true; rm -rf "$checker_dir"; scripts/ci/reclaim-disk.sh report' EXIT

scripts/ci/prepare-logos42.sh "$VESPER_LOGOS42_ROOT"
bazel test --config=ci --config=rbe_build --config=ci_tests "${replay_targets[@]}"
