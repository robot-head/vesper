#!/bin/sh
# Rust-exit SH6 lane 17 (#1412), sourced by scripts/test-port-harness.sh
# (which defines `divergent`): every vesper-loom-trust test passes on the
# Vesper port (its only build), and each divergent area (DIVERGENT in
# crates/vesper-loom-trust/build.rs, applied one at a time) fails its
# differential test against the Rust reference while another passes.
set -eu
cargo test --locked -p vesper-loom-trust
while read -r area failing passing; do
    export VESPER_LOOM_TRUST_DIVERGE="$area"
    divergent "loom-trust port ($area)" "differential::$failing" "differential::$passing" \
        -p vesper-loom-trust --features vesper-port-divergent --lib </dev/null
done <<'CHECKS'
semver semver_versions_match_the_semver_crate distances_keys_and_splits_match_the_rust_code
object signing_messages_match_the_rust_encoder distances_keys_and_splits_match_the_rust_code
message signing_messages_match_the_rust_encoder admission_screens_match_the_rust_screen
quorum verification_matches_the_rust_verifier admission_screens_match_the_rust_screen
interval verification_matches_the_rust_verifier admission_screens_match_the_rust_screen
conflation distances_keys_and_splits_match_the_rust_code verification_matches_the_rust_verifier
distance distances_keys_and_splits_match_the_rust_code verification_matches_the_rust_verifier
screen admission_screens_match_the_rust_screen verification_matches_the_rust_verifier
CHECKS
unset VESPER_LOOM_TRUST_DIVERGE
