#!/bin/sh
# vesper-warp-shard under Cargo (Rust exit SH6 lane 4, #1399), for the macOS
# and Windows checks; Linux runs the same checks as Bazel targets
# (crates/vesper-warp-shard/BUILD.bazel). Run from the repository root by
# scripts/test-port-harness.sh. The crate's tests must pass against both
# builds, vesper-warp must pass against the port, and a deliberately
# divergent port (one bug per module, DIVERGENT_EDITS in build.rs) must fail
# exactly the tests that cover each module.
set -eu
cargo test --locked -p vesper-warp-shard
cargo test --locked -p vesper-warp-shard --features vesper-port
cargo test --locked -p vesper-warp --features vesper-warp-shard/vesper-port
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
expect() { # <test file> <tests that must fail> -- <tests that must pass>
    file=$1
    shift
    if cargo test --locked -p vesper-warp-shard --features vesper-port-divergent --test "$file" -- --test-threads=1 >"$log" 2>&1; then
        cat "$log"
        echo "the divergent warp-shard port passed tests/$file.rs" >&2
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
            echo "divergent warp-shard port: expected '$t ... $want' in tests/$file.rs" >&2
            exit 1
        fi
    done
}
expect equivalence kernels_match_scalar rejects_bad_configs_and_inputs batch_semantics_examples -- reports_isa_paths generation_reuse_invalidates_old_handles
expect rss table_hash_matches_bitwise_reference microsoft_verification_vectors -- rejects_bad_inputs
echo "divergent warp-shard port detected (kernels, table, batch and rss each caught)"
