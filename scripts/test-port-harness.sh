#!/bin/sh
# Port harness (port wave P0 lane 6, #878) under Cargo, for the macOS and
# Windows checks; Linux runs the same checks as Bazel targets
# (crates/vesper-port-pilot/BUILD.bazel). The pilot's API tests must pass
# against its Rust build and its Vesper build (`vesper build --lib`), and
# must fail against a deliberately divergent Vesper build.
set -eu
cd "$(dirname "$0")/.."
cargo test --locked -p vesper-port-abi -p vesper-port-harness
cargo test --locked -p vesper-port-pilot
cargo test --locked -p vesper-port-pilot --features vesper-port
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
if cargo test --locked -p vesper-port-pilot --features vesper-port-divergent --test api -- --test-threads=1 >"$log" 2>&1; then
    cat "$log"
    echo "the divergent Vesper port passed the pilot's API tests" >&2
    exit 1
fi
if ! grep -q 'test bytes_and_booleans ... FAILED' "$log" || ! grep -q 'test gcd_matches_euclid ... ok' "$log"; then
    cat "$log"
    echo "the divergent Vesper port failed for an unexpected reason" >&2
    exit 1
fi
echo "divergent Vesper port detected (bytes_and_booleans failed, the rest agreed)"
