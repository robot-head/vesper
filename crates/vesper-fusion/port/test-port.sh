#!/bin/sh
# The vesper-fusion port checks for Cargo (the Linux "Vesper ports" job runs
# this from scripts/test-port-harness.sh; BuildBuddy runs the Bazel targets
# in crates/vesper-fusion/BUILD.bazel). Every crate test must pass (the
# Vesper engine is the only build), and a build whose explanations record
# the wrong direction (EDITS in build.rs) must fail the witness test while a
# test that does not run the engine still passes.
set -eu
cd "$(dirname "$0")/../../.."
cargo test --locked -p vesper-fusion
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
if cargo test --locked -p vesper-fusion --features vesper-port-divergent --no-fail-fast \
    --test witness -- --test-threads=1 >"$log" 2>&1; then
    cat "$log"
    echo "the divergent vesper-fusion port passed its tests" >&2
    exit 1
fi
for want in "every_shipped_rule_appears_in_a_checked_witness ... FAILED" \
    "rule_sets_name_every_rewrite_with_a_lemma ... ok"; do
    grep -q "test $want" "$log" || {
        cat "$log"
        echo "the divergent vesper-fusion port: expected \"$want\"" >&2
        exit 1
    }
done
echo "divergent vesper-fusion port detected (engine.ves)"
