//! The divergence checks of the Vesper port (port wave P1 lane 14, #911): a
//! deliberately wrong build of each Vesper library must make the tests of its
//! module fail, while an unrelated test still passes. Bazel passes the
//! divergent test binaries (BUILD.bazel); under Cargo the same check is
//! aspect crate prism test-port. Without the variables this test has
//! nothing to run and passes.
use std::process::Command;

/// Variable naming the divergent binary, the test it must fail, and a test
/// it must still pass.
const CHECKS: [(&str, &str, &str); 5] = [
    (
        "VESPER_PRISM_DIVERGENT_DECODER",
        "exact_limits_accept_and_overruns_poison_without_retaining_excess",
        "host_must_be_unique_and_unambiguous_with_valid_port_and_address",
    ),
    (
        "VESPER_PRISM_DIVERGENT_RESPONSE",
        "body_forbidden_statuses_omit_length_and_never_accept_a_payload",
        "exact_binary_response_and_repeated_fields_are_not_combined",
    ),
    (
        "VESPER_PRISM_DIVERGENT_TARGET",
        "query_entries_plus_duplicates_and_empty_forms_are_defined",
        "static_segments_beat_parameters_independent_of_insertion_order",
    ),
    (
        "VESPER_PRISM_DIVERGENT_ROUTER",
        "static_segments_beat_parameters_independent_of_insertion_order",
        "query_entries_plus_duplicates_and_empty_forms_are_defined",
    ),
    (
        "VESPER_PRISM_DIVERGENT_CONNECTION",
        "out_of_order_completions_cannot_starve_the_earliest_response",
        "single_byte_feed_issues_each_request_once",
    ),
];

#[test]
fn each_divergent_library_fails_its_module_tests() {
    for (variable, failing, passing) in CHECKS {
        let Some(binary) = std::env::var_os(variable) else {
            continue;
        };
        let output = Command::new(&binary)
            .arg("--test-threads=1")
            .output()
            .expect("run the divergent port tests");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !output.status.success(),
            "{variable}: the divergent port passed:\n{stdout}"
        );
        assert!(
            stdout.contains(&format!("test {failing} ... FAILED")),
            "{variable}: the divergent port did not fail {failing}:\n{stdout}"
        );
        assert!(
            stdout.contains(&format!("test {passing} ... ok")),
            "{variable}: the unchanged code must still agree ({passing}):\n{stdout}"
        );
    }
}
