#!/bin/sh
# ABBA benchmark of the deleted Rust parsers and policy check (baseline)
# against the Vesper port (examples/abba.rs). SH6 lane 8 (#1403) deleted the
# Rust, so the baseline is examples/abba.rs built at the last commit that
# had it, in a temporary git worktree with its own target directory (kept
# between runs, so a second run only rebuilds the port side).
# Usage: crates/vesper-aegis/bench-abba.sh                  (compare, 9 epochs)
#        crates/vesper-aegis/bench-abba.sh flip-gate <workload>
set -eu
cd "$(dirname "$0")/../.."
# The last commit with the Rust implementation, whose results
# tests/data/port_differential.sha256 records.
base=${AEGIS_RUST_COMMIT:-a8570f2857d85dfb8fdbbdda74977b41f478c49d}
target="${CARGO_TARGET_DIR:-$PWD/target}"
case "$target" in /*) ;; *) target="$PWD/$target" ;; esac
baseline="$target/aegis-rust-baseline"
rust="$baseline/release/examples/abba"
if [ ! -x "$rust.$base" ]; then
    dir=$(mktemp -d)
    tree="$dir/rust-tree"
    trap 'git worktree remove --force "$tree" >/dev/null 2>&1 || true; rm -rf "$dir"' EXIT
    git worktree add --detach -q "$tree" "$base"
    (cd "$tree" && CARGO_TARGET_DIR="$baseline" \
        cargo build --locked --release -q -p vesper-aegis --example abba)
    cp "$rust" "$rust.$base"
fi
cargo build --locked --release -q -p vesper-aegis --example abba
port="$target/release/examples/abba"
if [ "${1:-}" = flip-gate ]; then
    "$port" flip-gate "$2" "$rust.$base"
else
    "$port" compare "$rust.$base"
fi
