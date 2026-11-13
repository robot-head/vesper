#!/usr/bin/env bash
# Builds every executable the acceptance scripts run, in one Bazel invocation,
# and records their paths in FILE (default .ci-bazel-executables) for
# scripts/bazel-lib.sh. Point VESPER_BAZEL_EXECUTABLES at FILE afterwards.
#
# This only saves time: if it fails, the scripts build each label themselves.
set -uo pipefail
cd "$(dirname "$0")/../.."
. scripts/bazel-lib.sh

file="${1:-.ci-bazel-executables}"
rm -f "$file"
# Every crate example, plus the CLI binaries. (No mapfile: macOS ships bash 3.)
# shellcheck disable=SC2086 # BAZEL_FLAGS is a list of options.
examples="$("$BAZEL" query $BAZEL_FLAGS "filter(':example/', //crates/...)" 2>/dev/null)"
# shellcheck disable=SC2086 # One label per word.
if ! bazel_prebuild "$file" //:bin/vesper //:bin/lean4lean42 $examples; then
  echo "warning: prebuilding acceptance executables failed; each script will build its own" >&2
  rm -f "$file"
  exit 0
fi
echo "Prebuilt $(wc -l < "$file") executables into $file"
