#!/bin/sh
# Port P3b lane 4 (#1066), sourced by scripts/test-port-harness.sh (which
# defines `divergent`): vesper-warp's tests pass against its Rust build (the
# default) and against the Vesper port of varint, packet_number, header,
# frame, ack, recovery, the stream buffers and the lifecycle tokens, and each divergent edit
# (DIVERGENT_EDITS in crates/vesper-warp/build.rs, applied one at a time)
# fails a test of its module while another test of the same file still
# passes.
set -eu
cargo test --locked -p vesper-warp
cargo test --locked -p vesper-warp --features vesper-port
while read -r module target failing passing; do
    export VESPER_WARP_DIVERGE="$module"
    # shellcheck disable=SC2086 # $target is two words for --lib/--test.
    divergent "Warp port ($module)" "$failing" "$passing" \
        -p vesper-warp --features vesper-port-divergent $target </dev/null
done <<'CHECKS'
varint --lib varint::tests::rfc_examples packet_number::tests::first_packets_decode
pn --lib packet_number::tests::rfc_a3_example varint::tests::rfc_examples
header --test=headers configured_dcid_lengths long_headers_roundtrip_and_coalesce
frame --test=frames stream_without_length_runs_to_end_and_all_type_bits ack_wire_ranges_decode
ack --test=ack_tracking delayed_ack_policy merges_ranges_and_detects_duplicates
recovery --test=recovery packet_threshold_then_time_threshold rtt_estimator_and_ack_delay_rules
stream --lib stream::tests::recv_buf_reassembles_with_wrap stream::tests::send_buf_retransmits_only_unacked_lost_bytes
lifecycle --lib lifecycle::tests::tokens_round_trip_and_refuse_what_they_must lifecycle::tests::reset_tokens_are_keyed_and_compared_whole
CHECKS
unset VESPER_WARP_DIVERGE
