#!/bin/sh
# ABBA benchmark of the Rust build against the Vesper port (examples/abba.rs).
# Usage: crates/vesper-aura/bench-abba.sh [epochs] [ops per run]
set -eu
cd "$(dirname "$0")/../.."
epochs=${1:-15}
ops=${2:-200}
out="${CARGO_TARGET_DIR:-$PWD/target}/release/examples"
dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
cargo build --locked --release -q -p vesper-aura --example abba
cp "$out/abba" "$dir/rust"
cargo build --locked --release -q -p vesper-aura --features vesper-port --example abba
cp "$out/abba" "$dir/port"
"$dir/rust" compare "$dir/rust" "$dir/port" "$epochs" "$ops"
