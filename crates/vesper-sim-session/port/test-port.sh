#!/bin/sh
# vesper-sim-session under Cargo (self-hosting wave SH5 lane 19, #1342), run
# from the repository root by scripts/test-port-harness.sh. The crate's
# tests, including the transcript differential
# (tests/self_host_sim_session.rs), must pass against both builds, and the
# deliberately divergent port (DIVERGENT_EDITS in build.rs) must change the
# Warp transcripts and fail the link's own tests.
set -eu
# The Rust build runs in the main test jobs.
cargo test --locked -p vesper-sim-session --features vesper-port
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
expect() { # <test target args> -- <tests that must fail>
    args=$1
    shift
    # shellcheck disable=SC2086
    if cargo test --locked -p vesper-sim-session --features vesper-port-divergent $args -- --test-threads=1 >"$log" 2>&1; then
        cat "$log"
        echo "the divergent sim-session port passed $args" >&2
        exit 1
    fi
    for t in "$@"; do
        if ! grep -q "test $t ... FAILED" "$log"; then
            cat "$log"
            echo "divergent sim-session port: expected '$t ... FAILED' in $args" >&2
            exit 1
        fi
    done
}
expect --lib model::link::tests::micro_burst_builds_queue_delay_then_tail_drops
expect "--test self_host_sim_session" transcripts_match_the_rust_build
echo "divergent sim-session port detected (link model caught by its tests and the transcripts)"
