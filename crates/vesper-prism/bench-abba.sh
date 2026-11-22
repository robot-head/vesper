#!/bin/sh
# ABBA benchmark of the replaced Rust implementation (baseline) against the
# Vesper port (examples/abba.rs, port parity wave lane 3, #951). The Rust
# implementation was deleted in SH6 lane 5 (#1400), so the baseline is built
# from a commit that still has it, in a temporary git worktree.
# Usage: crates/vesper-prism/bench-abba.sh [epochs] [ops per run] [rust commit]
# Pin it to one CPU for tight intervals: `taskset -c 3 sh .../bench-abba.sh`.
set -eu
cd "$(dirname "$0")/../.."
epochs=${1:-15}
ops=${2:-200}
# The last commit with the Rust implementation as the default build.
base=${3:-ab965f2ab475a4b8d3bf31034808c306cdf16975}
target="${CARGO_TARGET_DIR:-$PWD/target}"
# The baseline builds from inside the worktree, so a relative directory
# must be made absolute first.
case "$target" in /*) ;; *) target="$PWD/$target" ;; esac
dir=$(mktemp -d)
tree="$dir/rust-tree"
trap 'git worktree remove --force "$tree" >/dev/null 2>&1 || true; rm -rf "$dir"' EXIT
git worktree add --detach -q "$tree" "$base"
# The baseline runs this commit's benchmark, so both builds time the same
# workloads.
cp crates/vesper-prism/examples/abba.rs "$tree/crates/vesper-prism/examples/abba.rs"
(cd "$tree" && CARGO_TARGET_DIR="$target/prism-rust-baseline" \
    cargo build --locked --release -q -p vesper-prism --example abba)
cp "$target/prism-rust-baseline/release/examples/abba" "$dir/rust"
cargo build --locked --release -q -p vesper-prism --example abba
cp "$target/release/examples/abba" "$dir/port"
"$dir/port" compare "$dir/rust" "$dir/port" "$epochs" "$ops"
