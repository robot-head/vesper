#!/bin/sh
# ABBA benchmark of the Rust build (vesper-crucible-seed) against the Vesper
# port (vesper-crucible); see examples/abba.rs.
# Usage: crates/vesper-crucible/bench-abba.sh [epochs] [runs per measurement]
set -eu
cd "$(dirname "$0")/../.."
epochs=${1:-11}
ops=${2:-20}
out="${CARGO_TARGET_DIR:-$PWD/target}/release/examples"
dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
cargo build --locked --release -q -p vesper-crucible-seed --example abba
cp "$out/abba" "$dir/rust"
cargo build --locked --release -q -p vesper-crucible --example abba
cp "$out/abba" "$dir/port"
"$dir/rust" compare "$dir/rust" "$dir/port" "$epochs" "$ops"
