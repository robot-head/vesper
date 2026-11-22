#!/bin/sh
# vesper-aether under Cargo (SH6 lane 14, #1409). Run from the repository
# root by scripts/test-port-harness.sh. The crate's tests must pass against
# both builds, and a deliberately divergent port (one bug per module,
# DIVERGENT_EDITS in build.rs) must fail the tests that cover each module.
set -eu
cargo test --locked -p vesper-aether
cargo test --locked -p vesper-aether --features vesper-port
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
expect() { # <test file> <tests that must fail> -- <tests that must pass>
    file=$1
    shift
    if cargo test --locked -p vesper-aether --features vesper-port-divergent --test "$file" -- --test-threads=1 >"$log" 2>&1; then
        cat "$log"
        echo "the divergent aether port passed tests/$file.rs" >&2
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
            echo "divergent aether port: expected '$t ... $want' in tests/$file.rs" >&2
            exit 1
        fi
    done
}
# slot_plan: touching ranges count as overlapping.
expect slot_plan accepts_adjacent_aligned_slots overlap_check_matches_pairwise_definition -- rejects_each_malformed_region
expect slot_plan_port verify_matches_the_rust_verifier
# pvh: a segment may end one byte past the file.
expect pvh_port truncations_and_bit_flips_match
# hotset: equal indices count as increasing.
expect hotset_port decode_matches_on_valid_and_mutated_encodings -- runs_match
expect hotset encoded_form_is_validated -- runs_coalesce_and_split_at_the_batch_size
