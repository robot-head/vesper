#!/bin/sh
# vesper-warp-umem under Cargo (SH6 lane 14, #1409). Run from the repository
# root by scripts/test-port-harness.sh. The crate's tests must pass against
# both builds, its consumers must pass against the port, and a deliberately
# divergent port (one bug per module, DIVERGENT_EDITS in build.rs) must fail
# the tests that cover each module.
set -eu
cargo test --locked -p vesper-warp-umem
cargo test --locked -p vesper-warp-umem --features vesper-port
cargo test --locked -p vesper-warp -p vesper-sim-session --features vesper-warp-umem/vesper-port
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
expect() { # <test file> <tests that must fail> -- <tests that must pass>
    file=$1
    shift
    if cargo test --locked -p vesper-warp-umem --features vesper-port-divergent --test "$file" -- --test-threads=1 >"$log" 2>&1; then
        cat "$log"
        echo "the divergent warp-umem port passed tests/$file.rs" >&2
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
            echo "divergent warp-umem port: expected '$t ... $want' in tests/$file.rs" >&2
            exit 1
        fi
    done
}
# ring: free_entries off by one; soft: scripted mode skips fault checks
# after a valid descriptor; driver: a reconcile sentinel that never matches.
expect rings indices_wrap_modulo_two_to_the_32 producer_cache_refreshes_only_when_short reserve_is_all_or_nothing_and_peek_is_partial
expect soft_nic loopback_delivers_transmitted_bytes_back_to_rx full_tx_ring_backpressure_returns_the_frame malformed_rx_descriptors_are_rejected_without_losing_frames scripted_lifecycle_harvest_turn_around_submit_complete_retire randomized_runs_exercise_every_path
expect zero_alloc steady_state_loop_is_allocation_free
expect lean_model rust_count_model_replays_the_audited_runs
