#!/bin/sh
# vesper-pact port checks under Cargo (port wave P3a lane 3, #1038), run by
# scripts/test-port-harness.sh.
#
# - The Rust build (the default), the Vesper port (feature vesper-port) and
#   the compiler's Rust seed copy (vesper-pact-seed) pass every test of the
#   crate.
# - The divergent Vesper build (one edit per ported module, DIVERGENT in
#   build.rs) fails the test that guards each module, and an unchanged test
#   still passes.
set -eu
cd "$(dirname "$0")/../../.."
cargo test --locked -p vesper-pact -p vesper-pact-seed
cargo test --locked -p vesper-pact --features vesper-port
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
if cargo test --locked -p vesper-pact --features vesper-port-divergent --no-fail-fast --tests -- --test-threads=1 >"$log" 2>&1; then
    cat "$log"
    echo "the divergent pact port passed its tests" >&2
    exit 1
fi
for t in missing_impl_is_no_solution_with_deterministic_explanation merging_duplicate_effect_labels_fails_closed open_row_impl_extends_rows implicit_brand_uses_lexical_model_scopes mode_bounds_follow_the_lattice associated_value_must_satisfy_alias_bounds malformed_declarations_are_rejected_with_sorted_diagnostics schema_version_is_enforced step_budget_overflow_is_indeterminate query_dto_from_fixture_solves coherence_violations_are_reported_deterministically tampered_derivations_are_rejected coinductive_cycle_is_proven_and_marked; do
    grep -q "test $t ... FAILED" "$log" || { cat "$log"; echo "the divergent pact port did not fail $t" >&2; exit 1; }
done
grep -q 'test capability_labels_canonicalize_by_first_occurrence ... ok' "$log" || { cat "$log"; echo "the divergent pact port failed an unchanged test" >&2; exit 1; }
echo "divergent pact port detected (one failure per ported module)"
