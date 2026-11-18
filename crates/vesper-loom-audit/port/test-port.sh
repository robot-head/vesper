#!/bin/sh
# vesper-loom-audit port checks under Cargo (port wave P3a lane 4, #1039),
# run by scripts/test-port-harness.sh.
#
# - The port-backed package (vesper-loom-audit) and the compiler's Rust seed
#   copy (vesper-loom-audit-seed) both pass every test of the crate,
#   including the differential corpus digests.
# - The divergent Vesper build (one edit per ported module, DIVERGENT in
#   build.rs) fails the test that guards each module, and an unchanged test
#   still passes.
set -eu
cd "$(dirname "$0")/../../.."
cargo test --locked -p vesper-loom-audit -p vesper-loom-audit-seed
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
if cargo test --locked -p vesper-loom-audit --features vesper-port-divergent --no-fail-fast --tests -- --test-threads=1 >"$log" 2>&1; then
    cat "$log"
    echo "the divergent loom-audit port passed its tests" >&2
    exit 1
fi
for t in graph_catalog_path_and_work_budgets_fail_closed spdx_boolean_precedence_and_explicit_exception_grants malformed_or_ambiguous_catalogs_never_produce_clean_results native_shared_dependency_chooses_shortest_path_per_root severity_threshold_and_mixed_unknown_violations_are_explicit semver_intervals_include_lower_exclude_fixed_and_ignore_build_metadata; do
    grep -q "test $t ... FAILED" "$log" || { cat "$log"; echo "the divergent loom-audit port did not fail $t" >&2; exit 1; }
done
grep -q 'test empty_root_scope_cannot_certify_an_empty_or_unreachable_graph_clean ... ok' "$log" || { cat "$log"; echo "the divergent loom-audit port failed an unchanged test" >&2; exit 1; }
echo "divergent loom-audit port detected (one failure per ported module)"
