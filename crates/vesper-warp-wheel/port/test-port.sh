#!/bin/sh
# vesper-warp-wheel under Cargo (port wave P1 lane 3, #900), for the macOS
# and Windows checks; Linux runs the same checks as Bazel targets
# (crates/vesper-warp-wheel/BUILD.bazel). Run from the repository root by
# scripts/test-port-harness.sh. The crate's tests must pass against both
# builds, vesper-warp must pass against the port, and a deliberately
# divergent port (one bug per module, DIVERGENT_EDITS in build.rs) must fail
# exactly the tests that cover each module.
set -eu
cargo test --locked -p vesper-warp-wheel
cargo test --locked -p vesper-warp-wheel --features vesper-port
cargo test --locked -p vesper-warp --features vesper-warp-wheel/vesper-port
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
expect() { # <test file> <tests that must fail> -- <tests that must pass>
    file=$1
    shift
    if cargo test --locked -p vesper-warp-wheel --features vesper-port-divergent --test "$file" -- --test-threads=1 >"$log" 2>&1; then
        cat "$log"
        echo "the divergent warp-wheel port passed tests/$file.rs" >&2
        exit 1
    fi
    want=FAILED
    for t in "$@"; do
        if [ "$t" = -- ]; then
            want=ok
            continue
        fi
        if ! grep -q "test $t ... $want" "$log"; then
            cat "$log"
            echo "divergent warp-wheel port: expected '$t ... $want' in tests/$file.rs" >&2
            exit 1
        fi
    done
}
expect wheel clock_never_moves_backwards -- invalid_configs_are_rejected
expect model nothing_fires_early_within_a_tick -- equal_deadlines_fire_fifo_across_cascades
expect layers park_refusals_return_the_lease edt_horizon_and_order_rejections_return_packet -- discard_space_and_index_stay_consistent_under_churn
echo "divergent warp-wheel port detected (wheel, leases, edt and clock each caught)"
