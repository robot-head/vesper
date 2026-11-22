#!/bin/sh
# ABBA benchmark of the last Rust build of vesper-prism-h3 against the Vesper
# port (see examples/abba.rs): builds the example both ways (release), keeps
# each binary, and drives every workload. The Rust protocol core was deleted
# in SH6 lane 5 (#1400), so the Rust arm is built from a commit that still
# has it (the first argument, by default the last one), in a temporary git
# worktree, with this commit's benchmark. Not run in CI; the numbers are in
# docs/parallel-results/rust-exit-prism.md. Pin it to one CPU for tight
# intervals: `taskset -c 3 sh .../bench.sh`. With CALLGRIND=1 it also
# counts instructions (valgrind) for five batches of each build, as the
# difference between runs of 6 and 1 batches, so setup (loading the corpus,
# building the inputs and tables) is not counted.
set -eu
cd "$(dirname "$0")/../../.."
# The last commit with the Rust protocol core as the default build.
base=${1:-7dd51a805e50f83ff808f177c926568f86200885}
dir=$(mktemp -d)
tree="$dir/rust-tree"
trap 'git worktree remove --force "$tree" >/dev/null 2>&1 || true; rm -rf "$dir"' EXIT HUP INT TERM
exe=""
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) exe=".exe" ;; esac
target=${CARGO_TARGET_DIR:-$PWD/target}
# The baseline builds from inside the worktree, so a relative directory must
# be made absolute first.
case "$target" in /*) ;; *) target="$PWD/$target" ;; esac
git worktree add --detach -q "$tree" "$base"
cp crates/vesper-prism-h3/examples/abba.rs "$tree/crates/vesper-prism-h3/examples/abba.rs"
(cd "$tree" && CARGO_TARGET_DIR="$target/prism-h3-rust-baseline" \
    cargo build --locked --release -p vesper-prism-h3 --example abba)
cp "$target/prism-h3-rust-baseline/release/examples/abba$exe" "$dir/rust$exe"
cargo build --locked --release -p vesper-prism-h3 --example abba
cp "$target/release/examples/abba$exe" "$dir/vesper$exe"
for workload in decode encode frames messages; do
    "$dir/rust$exe" drive "$workload" "$dir/rust$exe" "$dir/vesper$exe"
    if [ "${CALLGRIND:-}" = 1 ]; then
        for arm in rust vesper; do
            count() {
                valgrind --tool=callgrind --callgrind-out-file=/dev/null "$dir/$arm$exe" once "$workload" "$1" 2>&1 |
                    sed -n "s/.*Collected : //p"
            }
            echo "$workload $arm instructions (5 batches): $(($(count 6) - $(count 1)))"
        done
    fi
done
