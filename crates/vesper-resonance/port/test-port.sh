#!/bin/sh
# The Vesper port of vesper-resonance (port wave P3b lane 9, #1071) under
# Cargo, for the port harness; Bazel runs the same tests as its own targets
# (crates/vesper-resonance/BUILD.bazel). Three checks:
#   1. the crate's tests pass against the Rust build, the Rust seed copy and
#      the port-backed build (tests/port_differential.rs compares the port's
#      samples, schedules, findings and leases with the Rust build bit for bit);
#   2. the divergent port (one edit in each ported module, see build.rs) fails
#      one test per module while a test in the same file still passes;
#   3. the native realtime verifier proves the port's audio path (the
#      `rt_audio` root in vesper/src/lib.ves) allocation-free, lock-free and
#      non-blocking on x86_64 and aarch64. The Rust alloc tests count only the
#      Rust allocator, so this is the zero-allocation proof for the port.
set -eu
cd "$(dirname "$0")/../../.."
cargo test --locked -p vesper-resonance -p vesper-resonance-seed
cargo test --locked -p vesper-resonance --features vesper-port
log=$(mktemp)
obj=$(mktemp)
trap 'rm -f "$log" "$obj"' EXIT HUP INT TERM
# module, test file, the test the divergent build must fail, a test it must pass
for check in \
    "kernels simd bank_render_matches_reference_with_events host_reports_its_paths" \
    "events realtime malformed_event_streams_are_typed_errors emergency_silence_zeroes_the_rest_and_drops_are_counted" \
    "lease realtime lease_must_be_fully_written_to_commit emergency_silence_zeroes_the_rest_and_drops_are_counted" \
    "pdc pdc parallel_branches_get_compensation_delays buffer_liveness_reuses_buffers" \
    "graph port_differential graph_outputs_match_the_rust_build events_and_ramps_match_the_rust_build" \
    "slab slab delay_slab_read_write_wraps_and_clones_cleanly clones_are_realigned_and_hold_the_same_samples" \
    "confine confinement allocation_two_calls_deep_is_rejected_with_its_path loops_follow_the_policy"; do
    set -- $check
    if cargo test --locked -p vesper-resonance --features vesper-port-divergent --test "$2" -- --test-threads=1 >"$log" 2>&1; then
        cat "$log"
        echo "the divergent Vesper port ($1) passed tests/$2.rs" >&2
        exit 1
    fi
    if ! grep -q "test $3 \.\.\. FAILED" "$log" || ! grep -q "test $4 \.\.\. ok" "$log"; then
        cat "$log"
        echo "the divergent Vesper port ($1) failed tests/$2.rs for an unexpected reason" >&2
        exit 1
    fi
done
echo "divergent Vesper resonance detected (one failing test per ported module, the rest agreed)"
# The policy lets only the compiler's own unbounded-loop finding through: the
# audio loops run to block and voice counts the caller passes, which the
# verifier cannot bound (see the report). Any other finding fails.
pkg=crates/vesper-resonance/vesper
for target in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu; do
    "$VESPER_PORT_COMPILER" emit "$pkg" --emit object -o "$obj" --entry rt_audio \
        --target "$target" --verify-realtime --verify-policy "$pkg/../port/realtime-policy.json" >"$log" 2>&1 \
        || grep -q "native verifier finding(s)$" "$log" || { cat "$log"; exit 1; }
    if grep -q "panicked\|was not emitted\|^error:" "$log" \
        || grep "error\[" "$log" | grep -vq "error\[realtime/rt-unbounded-loop\]"; then
        cat "$log"
        echo "the realtime verifier rejected the port's audio path on $target" >&2
        exit 1
    fi
done
# Positive control: the schedule compiler grows sequences, so the same policy
# with it as the root must report its allocator calls.
"$VESPER_PORT_COMPILER" emit "$pkg" --emit object -o "$obj" --entry compile_schedule \
    --verify-realtime --verify-policy "$pkg/../port/realtime-control.json" >"$log" 2>&1 || true
if ! grep -q "error\[realtime/rt-unknown-callee\]" "$log"; then
    cat "$log"
    echo "the realtime verifier's positive control was not rejected" >&2
    exit 1
fi
echo "realtime verifier: the port's audio path is allocation-free on x86_64 and aarch64 (control rejected)"
