#!/bin/sh
# The Vesper port of vesper-lean-syntax (self-hosting SH0 lane 6, #1128)
# under Cargo, run by scripts/test-port-harness.sh; Bazel builds the same
# port (crates/vesper-lean-syntax/BUILD.bazel). The crate's tests pass
# against the port-backed package and the Rust seed copy, the port prints
# the same text as the seed on generated trees (tests/differential.rs), and
# the divergent port, one area at a time (VESPER_LEAN_DIVERGE, EDITS in
# build.rs), fails a test that guards that area while a test it does not
# reach still passes.
set -eu
cd "$(dirname "$0")/../../.."
cargo test --locked -p vesper-lean-syntax -p vesper-lean-syntax-seed
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
while read -r area failing passing; do
    if VESPER_LEAN_DIVERGE=$area cargo test --locked -p vesper-lean-syntax \
        --features vesper-port-divergent --no-fail-fast --test lean --test lists \
        -- --test-threads=1 >"$log" 2>&1; then
        cat "$log"
        echo "the divergent lean-syntax port ($area) passed its tests" >&2
        exit 1
    fi
    if ! grep -q "test $failing ... FAILED" "$log" ||
        ! grep -q "test $passing ... ok" "$log"; then
        cat "$log"
        echo "the divergent lean-syntax port ($area) failed for an unexpected reason" >&2
        exit 1
    fi
    echo "divergent lean-syntax port ($area) detected ($failing failed)"
done <<'CHECKS'
printer ordinary_lists_include_empty_nested_and_escaped_literal_elements free_references_respect_binders
checks identifiers_are_injective_and_cannot_break_quotations free_references_respect_binders
walks free_references_respect_binders identifiers_are_injective_and_cannot_break_quotations
CHECKS
