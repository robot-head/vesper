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
while read -r module suite failing passing; do
    if VESPER_CHORUS_DIVERGE=$module cargo test --locked -p vesper-chorus \
        --features vesper-port-divergent --no-fail-fast --test "$suite" \
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
    grep -q "test $passing ... ok" "$log" || {
        cat "$log"
        echo "the divergent port ($module) failed the unchanged $passing" >&2
        exit 1
    }
done <<'CHECKS'
util projection informed_third_party_merges_into_an_offer binary_projections_are_dual
tree projection well_formedness_errors_are_typed binary_projections_are_dual
local projection two_buyer_projects_to_the_textbook_local_types binary_projections_are_dual
project projection role_outside_a_loop_projects_through_it binary_projections_are_dual
explore projection recursion_projects_and_runs binary_projections_are_dual
parse projection hostile_inputs_are_bounded binary_projections_are_dual
simnet session two_buyer_runs_under_simpulse_with_reordering_and_duplication two_buyer_runs_on_threads_over_memory_net
session session dropping_a_session_before_end_is_detected_and_unblocks_the_peer two_buyer_runs_on_threads_over_memory_net
wire raft_sim quorum_sizes wire_formats_have_exact_lengths_and_reject_malformed_input
raft raft_sim five_node_cluster_keeps_election_safety_and_log_matching wire_formats_have_exact_lengths_and_reject_malformed_input
replog replog_warp replicated_log_converges_over_lossy_partitioned_warp_links replicated_log_rejects_bad_configuration
CHECKS
echo "divergent vesper-chorus port detected (one module at a time: util, tree, local, project, explore, parse, simnet, session, wire, raft, replog)"
