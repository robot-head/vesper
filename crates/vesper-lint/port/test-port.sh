#!/bin/sh
# The Vesper port of vesper-lint (self-hosting SH1 lane 13, #1156) under
# Cargo, run by scripts/test-port-harness.sh; Bazel builds the same port
# (crates/vesper-lint/BUILD.bazel). The crate's tests pass against the Rust
# build, the port build and the Rust seed copy; the port lints every
# repository source, fuzzed fact sets, TOML policy documents and edit lists
# exactly as the seed does (tests/differential.rs); and the divergent port,
# one area at a time (VESPER_LINT_DIVERGE, EDITS in build.rs), fails a test
# that guards that area while a test it does not reach still passes.
set -eu
cd "$(dirname "$0")/../../.."
cargo test --locked -p vesper-lint -p vesper-lint-seed
# The allocation balance reads one process-wide counter, so it runs alone.
cargo test --locked -p vesper-lint --features vesper-port -- --skip port_frees_its_allocations
cargo test --locked -p vesper-lint --features vesper-port --test differential -- --exact port_frees_its_allocations
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
while read -r area failing passing; do
    if VESPER_LINT_DIVERGE=$area cargo test --locked -p vesper-lint \
        --features vesper-port,vesper-port-divergent --no-fail-fast \
        --test differential --test typed --test policy \
        -- --skip port_frees_its_allocations >"$log" 2>&1; then
        cat "$log"
        echo "the divergent lint port ($area) passed its tests" >&2
        exit 1
    fi
    if ! grep -q "test $failing ... FAILED" "$log" ||
        ! grep -q "test $passing ... ok" "$log"; then
        cat "$log"
        echo "the divergent lint port ($area) failed for an unexpected reason" >&2
        exit 1
    fi
    echo "divergent lint port ($area) detected ($failing failed)"
done <<'CHECKS'
effects fuzzed_facts_lint_identically fuzzed_documents_parse_identically
projections fuzzed_facts_lint_identically fuzzed_documents_parse_identically
toml fuzzed_documents_parse_identically fuzzed_facts_lint_identically
edits fuzzed_edits_apply_identically fuzzed_facts_lint_identically
CHECKS
