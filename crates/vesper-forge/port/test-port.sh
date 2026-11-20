#!/bin/sh
# The Vesper port of the vesper-forge hygiene core (self-hosting SH1 lane
# 14, #1157) under Cargo, run by scripts/test-port-harness.sh; Bazel builds
# the same port (crates/vesper-forge/BUILD.bazel). The crate's tests pass
# against the Rust build, the Rust seed copy and the port-backed build
# (`vesper-port`), the port matches the seed on the random corpus
# (tests/port_differential.rs) and frees what it allocates
# (tests/port_memory.rs), and the divergent port, one area at a time
# (VESPER_FORGE_DIVERGE, EDITS in build.rs), fails a test that guards that
# area while a test it does not reach still passes.
set -eu
cd "$(dirname "$0")/../../.."
cargo test --locked -p vesper-forge -p vesper-forge-seed
cargo test --locked -p vesper-forge --features vesper-port
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
while read -r area test failing passing; do
    if VESPER_FORGE_DIVERGE=$area cargo test --locked -p vesper-forge \
        --features vesper-port-divergent --no-fail-fast --test "$test" \
        -- --test-threads=1 >"$log" 2>&1; then
        cat "$log"
        echo "the divergent forge port ($area) passed its tests" >&2
        exit 1
    fi
    if ! grep -q "test $failing ... FAILED" "$log" ||
        ! grep -q "test $passing ... ok" "$log"; then
        cat "$log"
        echo "the divergent forge port ($area) failed for an unexpected reason" >&2
        exit 1
    fi
    echo "divergent forge port ($area) detected ($failing failed)"
done <<'CHECKS'
delimiters fragment delimiters_must_match_and_nest reserved_words_are_not_identifiers
parse fragment reserved_words_are_not_identifiers delimiters_must_match_and_nest
resolve hygiene same_package_caller_locals_cannot_capture_macro_globals introduced_binder_cannot_capture_caller_identifier
registry hygiene forged_def_id_claims_are_rejected introduced_binder_cannot_capture_caller_identifier
CHECKS
