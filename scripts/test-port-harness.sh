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
# The kit (export ABI v2, #898): one failing test per form, and the leak
# check fails on the build that omits one Vesper-side free.
if cargo test --locked -p vesper-port-pilot --features vesper-port-divergent --test kit -- --test-threads=1 >"$log" 2>&1; then
    cat "$log"
    echo "the divergent kit passed its API tests" >&2
    exit 1
fi
for t in sequence_results slice_parameters owned_handles linear_handles recursive_enum_handles generic_instances vsys_imports; do
    if ! grep -q "test $t ... FAILED" "$log"; then
        cat "$log"
        echo "the divergent kit did not fail $t" >&2
        exit 1
    fi
done
grep -q 'test pattern_sink_closure ... ok' "$log" || { cat "$log"; echo "the divergent kit failed an unchanged test" >&2; exit 1; }
if cargo test --locked -p vesper-port-pilot --features vesper-port-divergent --test kit_memory >"$log" 2>&1; then
    cat "$log"
    echo "the kit leak check passed a build that leaks" >&2
    exit 1
fi
grep -qF 'leaked over 10^5 cycles' "$log" || { cat "$log"; echo "the kit leak check failed for another reason" >&2; exit 1; }
echo "divergent kit detected (one failure per form, and the leak)"
# Port P1 lane 10 (#907): vesper-warp-bbr, both builds, and its divergent
# build fails exactly one unit test per ported module.
cargo test --locked -p vesper-warp-bbr
cargo test --locked -p vesper-warp-bbr --features vesper-port
if cargo test --locked -p vesper-warp-bbr --features vesper-port-divergent --lib -- --test-threads=1 >"$log" 2>&1; then
    cat "$log"
    echo "the divergent warp-bbr port passed its unit tests" >&2
    exit 1
fi
for t in pacing::tests::pacing_delay_is_ceiling_and_defined_at_zero filter::tests::cycle_max_keeps_two_cycles bbr::tests::persistent_congestion_collapses_cwnd_then_floors_at_min_pipe; do
    grep -q "test $t ... FAILED" "$log" || { cat "$log"; echo "the divergent warp-bbr port did not fail $t" >&2; exit 1; }
done
grep -q 'test bbr::tests::inflight_at_loss_solves_for_the_threshold_crossing ... ok' "$log" || { cat "$log"; echo "the divergent warp-bbr port failed an unchanged test" >&2; exit 1; }
echo "divergent warp-bbr port detected (one failure per ported module)"
