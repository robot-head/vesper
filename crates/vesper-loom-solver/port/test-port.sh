#!/bin/sh
# vesper-loom-solver port checks under Cargo (port wave P1 lane 17, #914),
# run by scripts/test-port-harness.sh.
#
# - The port-backed package (vesper-loom-solver) and the compiler's Rust seed
#   copy (vesper-loom-solver-seed) both pass every test of the crate,
#   including the differential corpus digests.
# - The divergent Vesper build (one edit per ported module, DIVERGENT in
#   build.rs) fails the test that guards each module, and an unchanged test
#   still passes.
set -eu
cd "$(dirname "$0")/../../.."
cargo test --locked -p vesper-loom-solver -p vesper-loom-solver-seed
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
if cargo test --locked -p vesper-loom-solver --features vesper-port-divergent --no-fail-fast --tests -- --test-threads=1 >"$log" 2>&1; then
    cat "$log"
    echo "the divergent loom-solver port passed its tests" >&2
    exit 1
fi
for t in schema_unknown_fields_and_root_collisions_are_errors late_feature_requests_revisit_nodes_through_cycles valid_older_selection_is_preserved_without_a_search_budget native_constraints_unify_and_pick_latest_satisfying_version manifest_metadata_and_build_metadata_must_match distinguishes_missing_candidate_metadata_from_zero_cost resolve_corpus_matches_the_rust_build distinguishes_exhaustive_failure_from_search_budget; do
    grep -q "test $t ... FAILED" "$log" || { cat "$log"; echo "the divergent loom-solver port did not fail $t" >&2; exit 1; }
done
grep -q 'test backtracks_latest_parent_after_transitive_conflict ... ok' "$log" || { cat "$log"; echo "the divergent loom-solver port failed an unchanged test" >&2; exit 1; }
echo "divergent loom-solver port detected (one failure per ported module)"
