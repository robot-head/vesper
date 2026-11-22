#!/bin/sh
# vesper-loom-cas under Cargo (Rust-exit wave SH6 lane 16, #1411), run from
# the repository root by scripts/test-port-harness.sh. The crate's tests
# (the store and transfer suites run against the ported store), including
# the codec differential (tests/codec_differential.rs, whose
# digest is pinned from the Rust build), must pass against the port build,
# and each deliberately divergent module (DIVERGENT_EDITS in build.rs,
# picked by VESPER_CAS_DIVERGE) must fail.
set -eu
cargo test --locked -p vesper-loom-cas-port
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
expect() { # <module> <test target args> <text that must appear>
    module=$1
    args=$2
    text=$3
    # shellcheck disable=SC2086
    if VESPER_CAS_DIVERGE=$module cargo test --locked -p vesper-loom-cas-port --features divergent $args >"$log" 2>&1; then
        cat "$log"
        echo "the divergent loom-cas port ($module) passed $args" >&2
        exit 1
    fi
    if ! grep -q "$text" "$log"; then
        cat "$log"
        echo "divergent loom-cas port ($module): expected '$text' in $args" >&2
        exit 1
    fi
}
expect name --lib "test tree::tests::rejects_adversarial_names ... FAILED"
expect name "--test codec_differential" "codecs_agree_with_the_rust_build ... FAILED"
expect id "--test codec_differential" "codecs_agree_with_the_rust_build ... FAILED"
expect tree --lib "test tree::tests::directory_entries_must_have_zero_size ... FAILED"
expect tree "--test codec_differential" "codecs_agree_with_the_rust_build ... FAILED"
expect drv "--test codec_differential" "codecs_agree_with_the_rust_build ... FAILED"
expect store "--test store" "corrupted_objects_are_rejected_reported_and_repaired ... FAILED"
expect fold --lib "test port::tests::name_fold_is_to_lowercase ... FAILED"
expect fold "--test store" "ingest_rejects_nonportable_names ... FAILED"
expect walk "--test store" "ingest_rejects_nonportable_names ... FAILED"
expect pack "--test transfer_acceptance" "complete_valid_hashes_do_not_authorize_duplicate_extra_missing_or_reordered_records ... FAILED"
echo "divergent loom-cas port detected (name, id, tree, drv, store, fold, walk and pack each caught)"
