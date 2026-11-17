//! The divergence check: a deliberately wrong Vesper build must make the
//! API tests fail. Bazel passes the divergent test binary; under Cargo the
//! check is scripts/test-port-harness.sh. Without the variable this test
//! has nothing to run and passes.
use std::process::Command;

#[test]
fn divergent_port_fails_the_api_tests() {
    let Some(binary) = std::env::var_os("VESPER_PORT_DIVERGENT_TEST") else {
        return;
    };
    let output = Command::new(&binary)
        .arg("--test-threads=1")
        .output()
        .expect("run the divergent port tests");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !output.status.success(),
        "the divergent port passed the API tests:\n{stdout}"
    );
    assert!(
        stdout.contains("test bytes_and_booleans ... FAILED"),
        "the divergent port failed for another reason:\n{stdout}"
    );
    assert!(
        stdout.contains("test gcd_matches_euclid ... ok"),
        "the unchanged functions must still agree:\n{stdout}"
    );
}

/// The kit's form tests (`tests/kit.rs`) that its divergent build must fail,
/// one per exported form, and a pattern test that must still pass.
const KIT_FAILING: &[&str] = &[
    "sequence_results",
    "slice_parameters",
    "owned_handles",
    "linear_handles",
    "recursive_enum_handles",
    "generic_instances",
    "vsys_imports",
];

#[test]
fn divergent_kit_fails_one_test_per_form() {
    let Some(binary) = std::env::var_os("VESPER_PORT_KIT_DIVERGENT_TEST") else {
        return;
    };
    let output = Command::new(&binary)
        .arg("--test-threads=1")
        .output()
        .expect("run the divergent kit tests");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !output.status.success(),
        "the divergent kit passed:\n{stdout}"
    );
    for test in KIT_FAILING {
        assert!(
            stdout.contains(&format!("test {test} ... FAILED")),
            "the divergent kit did not fail {test}:\n{stdout}"
        );
    }
    assert!(
        stdout.contains("test pattern_sink_closure ... ok"),
        "the unchanged functions must still agree:\n{stdout}"
    );
}

#[test]
fn divergent_kit_leaks_are_detected() {
    let Some(binary) = std::env::var_os("VESPER_PORT_KIT_MEMORY_DIVERGENT_TEST") else {
        return;
    };
    let output = Command::new(&binary)
        .output()
        .expect("run the divergent kit memory test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "the leak went unnoticed:\n{stdout}"
    );
    assert!(
        stdout.contains("leaked over 10^5 cycles") || stderr.contains("leaked over 10^5 cycles"),
        "the divergent kit memory test failed for another reason:\n{stdout}\n{stderr}"
    );
}
