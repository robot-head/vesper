#!/bin/sh
# The Vesper port of vesper-prism (port wave P1 lane 14, #911; the only build
# since SH6 lane 5, #1400) under Cargo, for the Linux Platforms job; Bazel runs
# the same checks as its own targets (BUILD.bazel). The crate's tests and
# vesper-prism-h3's must pass, and each deliberately divergent Vesper library
# must fail its module's tests.
set -eu
cd "$(dirname "$0")/../.."
cargo test --locked -p vesper-prism
cargo test --locked -p vesper-prism-h3
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
# test file, the test the divergent build must fail, a test it must pass
for check in \
    "framing exact_limits_accept_and_overruns_poison_without_retaining_excess host_must_be_unique_and_unambiguous_with_valid_port_and_address" \
    "responses body_forbidden_statuses_omit_length_and_never_accept_a_payload exact_binary_response_and_repeated_fields_are_not_combined" \
    "routing query_entries_plus_duplicates_and_empty_forms_are_defined utf8_policy_is_explicit_for_paths_and_queries" \
    "routing static_segments_beat_parameters_independent_of_insertion_order utf8_policy_is_explicit_for_paths_and_queries" \
    "connection out_of_order_completions_cannot_starve_the_earliest_response single_byte_feed_issues_each_request_once"; do
    set -- $check
    if cargo test --locked -p vesper-prism --features vesper-port-divergent --test "$1" -- --test-threads=1 >"$log" 2>&1; then
        cat "$log"
        echo "the divergent Vesper port passed tests/$1.rs" >&2
        exit 1
    fi
    if ! grep -q "test $2 \.\.\. FAILED" "$log" || ! grep -q "test $3 \.\.\. ok" "$log"; then
        cat "$log"
        echo "the divergent Vesper port failed tests/$1.rs for an unexpected reason" >&2
        exit 1
    fi
done
echo "divergent Vesper prism detected (one failing test per library, the rest agreed)"
