#!/bin/sh
# ABBA benchmark of the Rust and Vesper builds of vesper-prism-h3 (see
# examples/abba.rs): builds the example both ways (release), keeps each
# binary, and drives every workload. Not run in CI; the numbers are in
# docs/parallel-results/port-p3b-prism-h3.md. With CALLGRIND=1 it also
# counts instructions (valgrind) for a few batches of each build.
set -eu
cd "$(dirname "$0")/../../.."
dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT HUP INT TERM
exe=""
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) exe=".exe" ;; esac
target=${CARGO_TARGET_DIR:-target}
cargo build --locked --release -p vesper-prism-h3 --example abba
cp "$target/release/examples/abba$exe" "$dir/rust$exe"
cargo build --locked --release -p vesper-prism-h3 --features vesper-port --example abba
cp "$target/release/examples/abba$exe" "$dir/vesper$exe"
for workload in decode encode; do
    "$dir/rust$exe" drive "$workload" "$dir/rust$exe" "$dir/vesper$exe"
    if [ "${CALLGRIND:-}" = 1 ]; then
        for arm in rust vesper; do
            valgrind --tool=callgrind --callgrind-out-file=/dev/null "$dir/$arm$exe" once "$workload" 5 2>&1 |
                sed -n "s/.*Collected : /$workload $arm instructions (5 batches): /p"
        done
    fi
done
