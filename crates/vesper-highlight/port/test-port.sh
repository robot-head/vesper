#!/bin/sh
# The Vesper port of vesper-highlight tier 0 (self-hosting SH1 lane 11,
# #1154) under Cargo, run by scripts/test-port-harness.sh; Bazel builds the
# same port (crates/vesper-highlight/BUILD.bazel). The crate's tests pass
# against the Rust build, the port build and the Rust seed copy; the port
# gives the seed's class spans on every repository source, every prefix of
# the snippets and fuzzed sources (tests/differential.rs); and the divergent
# port, one area at a time (VESPER_HIGHLIGHT_DIVERGE, EDITS in build.rs),
# fails a test that guards that area while a test it does not reach still
# passes.
set -eu
cd "$(dirname "$0")/../../.."
cargo test --locked -p vesper-highlight -p vesper-highlight-seed
cargo test --locked -p vesper-highlight --features vesper-port
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
while read -r area failing passing; do
    if VESPER_HIGHLIGHT_DIVERGE=$area cargo test --locked -p vesper-highlight \
        --features vesper-port,vesper-port-divergent --no-fail-fast \
        --test differential >"$log" 2>&1; then
        cat "$log"
        echo "the divergent highlight port ($area) passed its tests" >&2
        exit 1
    fi
    if ! grep -q "test $failing ... FAILED" "$log" ||
        ! grep -q "test $passing ... ok" "$log"; then
        cat "$log"
        echo "the divergent highlight port ($area) failed for an unexpected reason" >&2
        exit 1
    fi
    echo "divergent highlight port ($area) detected ($failing failed)"
done <<'CHECKS'
effects repository_sources_classify_identically sources_past_the_front_end_ceilings_are_classified_by_rust
words repository_sources_classify_identically sources_past_the_front_end_ceilings_are_classified_by_rust
types repository_sources_classify_identically sources_past_the_front_end_ceilings_are_classified_by_rust
CHECKS
