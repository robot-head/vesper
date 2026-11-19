#!/bin/sh
# The vesper-chorus port checks for Cargo (the Linux "Vesper ports" job runs
# this from scripts/test-port-harness.sh; BuildBuddy runs the Bazel targets
# in crates/vesper-chorus/BUILD.bazel). Every crate test must pass (the
# Vesper library is the only build), and a build with one wrong line in a
# ported module (see DIVERGENT in build.rs) must fail the test that guards
# that module while an unrelated test still passes, one module at a time.
set -eu
cd "$(dirname "$0")/../../.."
cargo test --locked -p vesper-chorus
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
while read -r module failing; do
    if VESPER_CHORUS_DIVERGE=$module cargo test --locked -p vesper-chorus \
        --features vesper-port-divergent --no-fail-fast --test projection \
        -- --test-threads=1 >"$log" 2>&1; then
        cat "$log"
        echo "the divergent vesper-chorus port ($module) passed its tests" >&2
        exit 1
    fi
    grep -q "test $failing ... FAILED" "$log" || {
        cat "$log"
        echo "the divergent port ($module) did not fail $failing" >&2
        exit 1
    }
    grep -q "test binary_projections_are_dual ... ok" "$log" || {
        cat "$log"
        echo "the divergent port ($module) failed the unchanged binary_projections_are_dual" >&2
        exit 1
    }
done <<'CHECKS'
util informed_third_party_merges_into_an_offer
tree well_formedness_errors_are_typed
local two_buyer_projects_to_the_textbook_local_types
project role_outside_a_loop_projects_through_it
explore recursion_projects_and_runs
parse hostile_inputs_are_bounded
CHECKS
echo "divergent vesper-chorus port detected (one module at a time: util, tree, local, project, explore, parse)"
