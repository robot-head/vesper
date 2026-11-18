#!/bin/sh
# vesper-tensor port checks under Cargo (port wave P1 lane 8, #905), run by
# scripts/test-port-harness.sh in the Linux Platforms job; Bazel runs the same
# checks as Bazel targets (crates/vesper-tensor/BUILD.bazel).
#
# - The port-backed package (vesper-tensor) and the compiler's Rust seed
#   copy (vesper-tensor-seed) both pass every test of the crate.
# - The divergent Vesper build (one edit per ported module, DIVERGENT in
#   build.rs) fails the test that guards each module, and an unchanged test
#   of the same file still passes.
set -eu
cd "$(dirname "$0")/../../.."
cargo test --locked -p vesper-tensor -p vesper-tensor-seed
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
# expect <test file> <passing test> <failing test>...
expect() {
    file=$1 passing=$2
    shift 2
    if cargo test --locked -p vesper-tensor --features vesper-port-divergent --test "$file" -- --test-threads=1 >"$log" 2>&1; then
        cat "$log"
        echo "the divergent vesper-tensor port passed tests/$file.rs" >&2
        exit 1
    fi
    for t in "$@"; do
        grep -q "test $t ... FAILED" "$log" || { cat "$log"; echo "the divergent vesper-tensor port did not fail $t" >&2; exit 1; }
    done
    grep -q "test $passing ... ok" "$log" || { cat "$log"; echo "the divergent vesper-tensor port failed an unchanged test ($passing)" >&2; exit 1; }
}
expect reference_fixtures gpt2_case_file_is_well_formed \
    q4_k_bytes_equal_llama_cpp fp8_channel_block_and_mxfp8_codes_equal_ml_dtypes q8_0_and_q4_0_weights_equal_gguf_py
expect tokenizer_sampling rng_matches_run_c_xorshift \
    sampling_follows_the_distribution_and_the_nucleus encoder_matches_run_c_and_round_trips
expect inference_ops rope_matches_f64_rotation_and_rejects_odd_widths silu_swiglu_softmax_argmax
expect abba both_builds_encode_the_same_prompt \
    both_builds_read_the_same_safetensors_headers both_builds_read_the_same_gguf_files
echo "divergent vesper-tensor port detected (one failing test per ported module)"
