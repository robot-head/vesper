#!/bin/sh
# vesper-prism-h3 port checks under Cargo (port wave P3b lane 5, #1067), run
# by scripts/test-port-harness.sh.
#
# - The Rust build (default) and the Vesper build (`vesper-port`) both pass
#   every test of the crate, the interop tests included (the connection
#   driver stays Rust and calls the port's codec).
# - The divergent Vesper build, one module at a time (VESPER_H3_DIVERGE,
#   DIVERGENT_EDITS in build.rs), fails a test that guards that module, and a
#   test that the edit does not reach still passes.
set -eu
cd "$(dirname "$0")/../../.."
cargo test --locked -p vesper-prism-h3
cargo test --locked -p vesper-prism-h3 --features vesper-port
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
while read -r module failing passing; do
    if VESPER_H3_DIVERGE=$module cargo test --locked -p vesper-prism-h3 \
        --features vesper-port-divergent --no-fail-fast \
        --test qpack_rfc --test qpack_qifs --test qpack_encode_golden --test frame_codec --test message_rules \
        -- --test-threads=1 >"$log" 2>&1; then
        cat "$log"
        echo "the divergent prism-h3 port ($module) passed its tests" >&2
        exit 1
    fi
    if ! grep -q "test $failing ... FAILED" "$log" ||
        ! grep -q "test $passing ... ok" "$log"; then
        cat "$log"
        echo "the divergent prism-h3 port ($module) failed for an unexpected reason" >&2
        exit 1
    fi
    echo "divergent prism-h3 port ($module) detected ($failing failed)"
done <<'CHECKS'
huffman appendix_b_encoder_side capsules_round_trip_split_anywhere
prefix committed_qifs_subset_decodes frames_round_trip_split_anywhere
table appendix_b_decoder_side frames_round_trip_split_anywhere
encoder appendix_b_encoder_side frames_round_trip_split_anywhere
decoder committed_qifs_subset_decodes frames_round_trip_split_anywhere
varint frames_round_trip_split_anywhere rfc9218_priorities
frame malformed_frames_are_typed_errors capsules_round_trip_split_anywhere
priority rfc9218_priorities malformed_frames_are_typed_errors
capsule capsules_round_trip_split_anywhere appendix_b_decoder_side
message content_length_and_host_rules response_statuses
CHECKS
