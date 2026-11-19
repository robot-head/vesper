#!/bin/sh
# ABBA benchmark of the replaced Rust implementation (baseline) against the
# Vesper library (examples/abba.rs). The Rust implementation was deleted in
# port wave P3b lane 6 (#1068), so the baseline is built from the last
# commit that still has it as the default build, in a temporary git
# worktree. Not run in CI; the numbers are in
# docs/parallel-results/port-p3b-chorus.md.
# Usage: crates/vesper-chorus/scripts/bench.sh [rust commit]
set -eu
cd "$(dirname "$0")/../../.."
base=${1:-a2adca200824728bfb06a9a27e673b4d488da5c7}
target="${CARGO_TARGET_DIR:-$PWD/target}"
dir=$(mktemp -d)
tree="$dir/rust-tree"
trap 'git worktree remove --force "$tree" >/dev/null 2>&1 || true; rm -rf "$dir"' EXIT HUP INT TERM
exe=""
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) exe=".exe" ;; esac
git worktree add --detach -q "$tree" "$base"
(cd "$tree" && CARGO_TARGET_DIR="$target/chorus-rust-baseline" \
    cargo build --locked --release -q -p vesper-chorus --example abba)
cp "$target/chorus-rust-baseline/release/examples/abba$exe" "$dir/rust$exe"
cargo build --locked --release -q -p vesper-chorus --example abba
cp "$target/release/examples/abba$exe" "$dir/vesper$exe"
for workload in project explore; do
    "$dir/rust$exe" drive "$workload" "$dir/rust$exe" "$dir/vesper$exe"
done
