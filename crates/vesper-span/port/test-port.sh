#!/bin/sh
# The Vesper port of vesper-span (self-hosting SH0 lane 6, #1128) under
# Cargo, run by scripts/test-port-harness.sh; Bazel builds the same port
# (crates/vesper-span/BUILD.bazel). The crate's tests pass against the
# port-backed package and the Rust seed copy, the port matches the seed on
# generated inputs (tests/differential.rs), and the divergent port, one
# area at a time (VESPER_SPAN_DIVERGE, EDITS in build.rs), fails a test that
# guards that area while a test it does not reach still passes.
set -eu
cd "$(dirname "$0")/../../.."
cargo test --locked -p vesper-span -p vesper-span-seed
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
while read -r area failing passing; do
    if VESPER_SPAN_DIVERGE=$area cargo test --locked -p vesper-span \
        --features vesper-port-divergent --no-fail-fast --test span \
        -- --test-threads=1 >"$log" 2>&1; then
        cat "$log"
        echo "the divergent span port ($area) passed its tests" >&2
        exit 1
    fi
    if ! grep -q "test $failing ... FAILED" "$log" ||
        ! grep -q "test $passing ... ok" "$log"; then
        cat "$log"
        echo "the divergent span port ($area) failed for an unexpected reason" >&2
        exit 1
    fi
    echo "divergent span port ($area) detected ($failing failed)"
done <<'CHECKS'
index utf8_utf16_and_strict_interiors unordered_edits_are_sorted_and_snapshot_bound
edits collisions_and_invalid_edits_leave_source_unchanged utf8_utf16_and_strict_interiors
diagnostics diagnostic_invalid_labels_fixes_and_aggregate_budgets utf8_utf16_and_strict_interiors
CHECKS
