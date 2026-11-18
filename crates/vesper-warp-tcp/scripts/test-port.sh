#!/bin/sh
# The vesper-warp-tcp port checks for Cargo (the Linux Platforms job runs this
# from scripts/test-port-harness.sh; BuildBuddy runs the Bazel targets in
# crates/vesper-warp-tcp/BUILD.bazel). Every test must pass against the
# Vesper build, and a build with one wrong line per ported module (see
# DIVERGENT_EDITS in build.rs) must fail the tests that cover each module
# while still passing one that touches none of them.
set -eu
cd "$(dirname "$0")/../../.."
cargo test --locked -p vesper-warp-tcp --features vesper-port
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
if cargo test --locked -p vesper-warp-tcp --features vesper-port-divergent --no-fail-fast \
    --test units --test fixtures --test state_machine -- --test-threads=1 >"$log" 2>&1; then
    cat "$log"
    echo "the divergent vesper-warp-tcp port passed its tests" >&2
    exit 1
fi
for t in wraparound_ordering rfc6298_formulas_and_backoff slow_start_then_one_reduction_per_episode \
    fixture_checksums_verify_and_recompute fixtures_reserialize_byte_for_byte \
    third_duplicate_ack_triggers_fast_retransmit; do
    grep -q "test $t ... FAILED" "$log" || { cat "$log"; echo "the divergent port did not fail $t" >&2; exit 1; }
done
for t in rfc1071_numeric_example fixture_fields_and_options_decode listen_close_goes_to_closed; do
    grep -q "test $t ... ok" "$log" || { cat "$log"; echo "the divergent port failed the unchanged $t" >&2; exit 1; }
done
echo "divergent vesper-warp-tcp port detected (one module at a time: seq, rtt, cc, wire, conn)"
