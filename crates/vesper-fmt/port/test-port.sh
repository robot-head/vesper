#!/bin/sh
# The Vesper port of vesper-fmt (self-hosting SH1 lane 11, #1154) under
# Cargo, run by scripts/test-port-harness.sh; Bazel builds the same port
# (crates/vesper-fmt/BUILD.bazel). The crate's tests pass against the Rust
# build, the port build and the Rust seed copy; the port formats every
# repository source, scrambled and fuzzed copies, and limit sweeps exactly
# as the seed does (tests/differential.rs); and the divergent port, one area
# at a time (VESPER_FMT_DIVERGE, EDITS in build.rs), fails a test that
# guards that area while a test it does not reach still passes.
set -eu
cd "$(dirname "$0")/../../.."
cargo test --locked -p vesper-fmt -p vesper-fmt-seed
cargo test --locked -p vesper-fmt --features vesper-port
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
while read -r area failing passing; do
    if VESPER_FMT_DIVERGE=$area cargo test --locked -p vesper-fmt \
        --features vesper-port,vesper-port-divergent --no-fail-fast \
        --test differential --test format >"$log" 2>&1; then
        cat "$log"
        echo "the divergent fmt port ($area) passed its tests" >&2
        exit 1
    fi
    if ! grep -q "test $failing ... FAILED" "$log" ||
        ! grep -q "test $passing ... ok" "$log"; then
        cat "$log"
        echo "the divergent fmt port ($area) failed for an unexpected reason" >&2
        exit 1
    fi
    echo "divergent fmt port ($area) detected ($failing failed)"
done <<'CHECKS'
doc repository_sources_format_byte_identically limit_sweeps_fail_at_the_same_point
gate repository_sources_format_byte_identically limit_sweeps_fail_at_the_same_point
layout repository_sources_format_byte_identically line_comment_never_absorbs_next_token
CHECKS
