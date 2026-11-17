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
