#!/bin/sh
# vesper-sim-session under Cargo (self-hosting wave SH5 lane 19, #1342), run
# from the repository root by scripts/test-port-harness.sh. The crate's
# tests, including the transcript differential
# (tests/self_host_sim_session.rs), must pass against the port build (the
# Rust build runs in the main test jobs), and each deliberately divergent
# module (DIVERGENT_EDITS in build.rs, picked by VESPER_SIM_DIVERGE) must
# fail its own tests and change its transcripts.
set -eu
cargo test --locked -p vesper-sim-session --features vesper-port
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
expect() { # <module> <test target args> <text that must appear>...
    module=$1
    args=$2
    shift 2
    # shellcheck disable=SC2086
    if VESPER_SIM_DIVERGE=$module cargo test --locked -p vesper-sim-session --features vesper-port-divergent $args -- --test-threads=1 >"$log" 2>&1; then
        cat "$log"
        echo "the divergent sim-session port ($module) passed $args" >&2
        exit 1
    fi
    for text in "$@"; do
        if ! grep -q "$text" "$log"; then
            cat "$log"
            echo "divergent sim-session port ($module): expected '$text' in $args" >&2
            exit 1
        fi
    done
}
expect link --lib "test model::link::tests::micro_burst_builds_queue_delay_then_tail_drops ... FAILED"
expect link "--test self_host_sim_session" "^warp_wan: "
# The WAL's UTF-8 check: the scan wrapper refuses the payload the divergent
# port accepted.
expect wal "--test self_host_sim_session" "the port validated the payload"
expect net --lib "test model::net::tests::bounds_are_checked_before_anything_is_retained ... FAILED"
expect net "--test self_host_sim_session" "^net: "
echo "divergent sim-session port detected (link, WAL and net each caught)"
