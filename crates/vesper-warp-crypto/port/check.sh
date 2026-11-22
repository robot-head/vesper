#!/bin/sh
# Rust-exit SH6 lane 17 (#1412), sourced by scripts/test-port-harness.sh
# (which defines `divergent`): every vesper-warp-crypto test passes on the
# Vesper port (its only build), and each divergent area (DIVERGENT in
# crates/vesper-warp-crypto/build.rs, applied one at a time) fails its
# differential test against the Rust reference while another passes.
set -eu
cargo test --locked -p vesper-warp-crypto
while read -r area failing passing; do
    export VESPER_WARP_CRYPTO_DIVERGE="$area"
    divergent "warp-crypto port ($area)" "differential::$failing" "differential::$passing" \
        -p vesper-warp-crypto --features vesper-port-divergent --lib </dev/null
done <<'CHECKS'
schedule key_schedule_matches_the_rust_schedule retry_integrity_matches_the_rust_code
nonce packet_and_header_protection_match_the_rust_code retry_integrity_matches_the_rust_code
header packet_and_header_protection_match_the_rust_code retry_integrity_matches_the_rust_code
sealer sealers_and_openers_match_the_rust_ticket_rules retry_integrity_matches_the_rust_code
update one_rtt_key_updates_match_the_rust_state_machine retry_integrity_matches_the_rust_code
retry retry_integrity_matches_the_rust_code key_schedule_matches_the_rust_schedule
CHECKS
unset VESPER_WARP_CRYPTO_DIVERGE
