#!/bin/sh
# ABBA benchmark of the deleted Rust code against the Vesper port, on one
# workload (examples/<workload>_bench.rs):
#
# - runtime: the Rust runtime over ash, at 0787bb71 (SH6 lane 15 part 2c);
# - broadphase, generation: the Rust broadphase and tensor drivers, at
#   6628b18a (part 2b).
#
# Each baseline is the example built at the last commit that had the Rust
# code, from the example as it was at 086e3684 (the last commit whose
# Context::kernel took WGSL; the examples use only the crate's public API),
# in a git worktree under the target directory, kept between runs. Needs a
# Vulkan device (Mesa lavapipe in CI).
# Usage: crates/vesper-gpu-compute/port/bench-abba.sh <workload> [pairs]
set -eu
cd "$(dirname "$0")/../../.."
workload=$1
case "$workload" in
    runtime) commit=${GPU_RUST_RUNTIME_COMMIT:-0787bb712cde55149c3b2f98fcee93af6f1d8892} ;;
    broadphase | generation) commit=${GPU_RUST_DRIVER_COMMIT:-6628b18a31135f0ba3c4d6c59eeefa85f84122e8} ;;
    *) echo "unknown workload $workload" >&2; exit 2 ;;
esac
example="${workload}_bench"
target="${CARGO_TARGET_DIR:-$PWD/target}"
case "$target" in /*) ;; *) target="$PWD/$target" ;; esac
baseline="$target/gpu-compute-rust-baseline"
rust="$baseline/release/examples/$example.$commit"
if [ ! -x "$rust" ]; then
    tree="$baseline/tree-$commit"
    git worktree prune
    [ -d "$tree" ] || git worktree add --detach -q "$tree" "$commit"
    git show "${GPU_EXAMPLE_COMMIT:-086e3684361b70921b5492f0274bf20f92f9c97d}:crates/vesper-gpu-compute/examples/$example.rs" \
        > "$tree/crates/vesper-gpu-compute/examples/$example.rs"
    (cd "$tree" && CARGO_TARGET_DIR="$baseline" \
        cargo build --locked --release -q -p vesper-gpu-compute --example "$example") >&2
    cp "$baseline/release/examples/$example" "$rust"
fi
cargo build --locked --release -q -p vesper-gpu-compute --example "$example" >&2
python3 crates/vesper-gpu-compute/port/abba.py "$rust" "$target/release/examples/$example" "${2:-10}" "$workload"
