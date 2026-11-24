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
expect() { # <test file, or lib> <tests that must fail> -- <tests that must pass>
    file=$1
    shift
    target="--test $file"
    if [ "$file" = lib ]; then
        target=--lib
    fi
    # shellcheck disable=SC2086 # $target is two words or one.
    if cargo test --locked -p vesper-aether --features vesper-port-divergent $target -- --test-threads=1 >"$log" 2>&1; then
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
# packed: a used descriptor counts as available.
expect packed_port capture_matches_the_rust_capture
# vq (Vesper-only; the Rust reference is a unit-test module): a chain may
# hold one buffer more than the limit.
expect lib devices::queue::reference::differential::capture_matches_the_rust_capture -- devices::queue::reference::abba::both_builds_compute_the_same_workload
# seccomp: unaligned seccomp_data load offset is rejected.
expect seccomp malformed_programs_are_rejected -- compiled_profile_allows_exactly_the_allowlist
expect seccomp_port validate_and_evaluate_match_rust -- compile_matches_rust
# guest_mem: touching regions are accepted as non-overlapping.
expect guest_mem_port layout_and_locate_match_rust -- out_of_bounds_and_overflow_match_rust
# block: unaligned discard sector/count is rejected.
expect block_port discard_and_write_zeroes_match_rust -- span_and_config_match_rust
