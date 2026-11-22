#!/bin/sh
# ABBA benchmark of the deleted Rust runtime (ash) against the Vesper
# runtime, on the runtime workload (examples/runtime_bench.rs). SH6 lane 15
# (#1410) deleted the Rust runtime, so the baseline is the example built at
# the last commit that had it, in a git worktree under the target directory
# (kept between runs). Needs a Vulkan device (Mesa lavapipe in CI).
# Usage: crates/vesper-gpu-compute/port/bench-abba.sh [pairs]
set -eu
cd "$(dirname "$0")/../../.."
rust_commit=${GPU_RUST_COMMIT:-0787bb712cde55149c3b2f98fcee93af6f1d8892}
target="${CARGO_TARGET_DIR:-$PWD/target}"
case "$target" in /*) ;; *) target="$PWD/$target" ;; esac
baseline="$target/gpu-compute-rust-baseline"
rust="$baseline/release/examples/runtime_bench.$rust_commit"
if [ ! -x "$rust" ]; then
    tree="$baseline/tree-$rust_commit"
    git worktree prune
    [ -d "$tree" ] || git worktree add --detach -q "$tree" "$rust_commit"
    (cd "$tree" && CARGO_TARGET_DIR="$baseline" \
        cargo build --locked --release -q -p vesper-gpu-compute --example runtime_bench) >&2
    cp "$baseline/release/examples/runtime_bench" "$rust"
fi
cargo build --locked --release -q -p vesper-gpu-compute --example runtime_bench >&2
python3 crates/vesper-gpu-compute/port/abba.py "$rust" "$target/release/examples/runtime_bench" "${1:-10}"
