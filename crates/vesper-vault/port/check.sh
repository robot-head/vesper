#!/bin/sh
# Rust-exit SH6 lane 17 (#1412), sourced by scripts/test-port-harness.sh
# (which defines `divergent`) or runnable standalone from the repo root:
# every vesper-vault and vesper-vault-seed test passes, and each divergent
# area (DIVERGENT in crates/vesper-vault/build.rs, applied one at a time)
# fails its differential test against the Rust reference while another passes.
set -eu

if ! command -v divergent >/dev/null 2>&1; then
    divergent() {
        what=$1 failing=$2 passing=$3
        shift 3
        log=$(mktemp)
        if cargo test --locked "$@" -- --test-threads=1 >"$log" 2>&1; then
            cat "$log"
            rm -f "$log"
            echo "the divergent $what passed its tests" >&2
            exit 1
        fi
        if ! grep -q "test $failing ... FAILED" "$log" || ! grep -q "test $passing ... ok" "$log"; then
            cat "$log"
            rm -f "$log"
            echo "the divergent $what failed for an unexpected reason" >&2
            exit 1
        fi
        rm -f "$log"
        echo "divergent $what detected ($failing failed, $passing agreed)"
    }
fi

cargo test --locked -p vesper-vault -p vesper-vault-seed
while read -r area failing passing; do
    export VESPER_VAULT_DIVERGE="$area"
    divergent "vault port ($area)" "differential::$failing" "differential::$passing" \
        -p vesper-vault --features vesper-port-divergent --lib </dev/null
done <<'CHECKS'
verify_tag verify_tag_matches_the_rust_code aead_limits_and_tickets_match_the_rust_code
aead_limits aead_limits_and_tickets_match_the_rust_code verify_tag_matches_the_rust_code
aead_seal aead_sealing_and_frames_match_the_rust_code verify_tag_matches_the_rust_code
aead_open aead_opening_and_doom_match_the_rust_code aead_sealing_and_frames_match_the_rust_code
kem_client kem_client_shares_match_the_rust_code verify_tag_matches_the_rust_code
kem_server kem_server_encaps_and_validation_match_the_rust_code kem_client_shares_match_the_rust_code
CHECKS
unset VESPER_VAULT_DIVERGE
