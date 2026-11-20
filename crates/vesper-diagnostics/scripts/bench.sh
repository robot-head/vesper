#!/bin/sh
# ABBA and callgrind numbers for the vesper-diagnostics port against the Rust
# seed (examples/abba.rs). Not run in CI; the numbers are in
# docs/parallel-results/self-host-diagnostics.md. With CALLGRIND=1 it also
# counts instructions for five batches of each arm, as the difference
# between runs of 6 and 1 batches, so setup is not counted.
set -eu
cd "$(dirname "$0")/../../.."
target=${CARGO_TARGET_DIR:-target}
cargo build --locked --release -p vesper-diagnostics --example abba
bin="$target/release/examples/abba"
for workload in ${WORKLOADS:-prepare terminal colored json lsp}; do
    "$bin" abba "$workload"
    if [ "${CALLGRIND:-}" = 1 ]; then
        for arm in rust vesper; do
            count() {
                valgrind --tool=callgrind --callgrind-out-file=/dev/null "$bin" once "$workload" "$arm" "$1" 2>&1 |
                    sed -n "s/.*Collected : //p"
            }
            echo "$workload $arm instructions (5 batches): $(($(count 6) - $(count 1)))"
        done
    fi
done
