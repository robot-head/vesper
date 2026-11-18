#!/bin/sh
# vesper-serde port checks under Cargo (port wave P1 lane 19, #916), run by
# scripts/test-port-harness.sh on macOS and Windows; Linux runs the same
# builds as Bazel targets (crates/vesper-serde/BUILD.bazel). Every test
# must pass against the Rust seed (vesper-serde-seed) and the port-backed
# package (vesper-serde), and each divergent build (one edit per ported
# module, DIVERGENT_EDITS in build.rs, picked by VESPER_SERDE_DIVERGE) must
# fail the test that guards its module while an unrelated one passes.
set -eu
cd "$(dirname "$0")/../../.."
cargo test --locked -p vesper-serde-seed -p vesper-serde
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
# check <module> <test file> <expected failing test> <expected passing test>
check() {
    if VESPER_SERDE_DIVERGE=$1 cargo test --locked -p vesper-serde --features vesper-port-divergent --test "$2" -- --test-threads=1 >"$log" 2>&1; then
        cat "$log"
        echo "the divergent serde port ($1) passed tests/$2.rs" >&2
        exit 1
    fi
    if ! grep -q "test $3 ... FAILED" "$log" || ! grep -q "test $4 ... ok" "$log"; then
        cat "$log"
        echo "the divergent serde port ($1) failed for an unexpected reason" >&2
        exit 1
    fi
    echo "divergent serde port ($1) detected ($3 failed, $4 agreed)"
}
check lemire float_lemire every_path_is_bit_identical_to_std_on_a_differential_corpus power_of_five_table_matches_an_independent_big_integer
check index json_test_suite json_test_suite_corpus random_documents_equal_serde_json
check stage2 simd_stage2 random_valid_documents_build_identical_tapes deep_and_wide_inputs
check encode float_encode every_finite_exponent_class_and_arbitrary_finite_bits_roundtrip_exactly all_nonfinite_sign_and_payload_classes_refuse_before_any_nested_output_write
check helpers simd_stage1 branchless_escape_mask_matches_scalar_reference prefix_xor_is_parity
check binary binary streamvbyte_fixed_reference_bytes_and_output_suffixes archive_fixed_reference_bytes_signed_pointers_and_borrowed_views
check records tiles ingests_into_hot_and_cold_tiles_on_every_path swar_keys_match_exactly_the_equal_byte_strings
