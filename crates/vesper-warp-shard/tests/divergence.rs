//! The divergence check: a Vesper build with one deliberate bug per ported
//! module (`DIVERGENT_EDITS` in build.rs, `edits` in BUILD.bazel) must fail
//! exactly the tests that cover each module. Bazel passes the divergent test
//! binaries; under Cargo the check is port/test-port.sh. Without the
//! variables these tests have nothing to run and pass.
use std::process::Command;

/// Run a divergent test binary and check which tests failed and passed.
fn expect(variable: &str, failing: &[&str], passing: &[&str]) {
    let Some(binary) = std::env::var_os(variable) else {
        return;
    };
    let output = Command::new(&binary)
        .arg("--test-threads=1")
        .output()
        .expect("run the divergent port tests");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !output.status.success(),
        "the divergent port passed:\n{stdout}"
    );
    for test in failing {
        assert!(
            stdout.contains(&format!("test {test} ... FAILED")),
            "the divergent port did not fail {test}:\n{stdout}"
        );
    }
    for test in passing {
        assert!(
            stdout.contains(&format!("test {test} ... ok")),
            "the unchanged code must still agree on {test}:\n{stdout}"
        );
    }
}

/// The kernel edit (the scalar equality mask drops lane 15), the table edit
/// (packet number 2^62 + 1 is accepted) and the batch edit (a cwnd update is
/// ignored).
#[test]
fn divergent_kernels_table_and_batch_are_detected() {
    expect(
        "VESPER_SHARD_DIVERGENT_EQUIVALENCE_TEST",
        &[
            "kernels_match_scalar",
            "rejects_bad_configs_and_inputs",
            "batch_semantics_examples",
        ],
        &[
            "reports_isa_paths",
            "generation_reuse_invalidates_old_handles",
        ],
    );
}

/// The RSS edit (the Toeplitz hash skips the first input byte).
#[test]
fn divergent_rss_is_detected() {
    expect(
        "VESPER_SHARD_DIVERGENT_RSS_TEST",
        &[
            "table_hash_matches_bitwise_reference",
            "microsoft_verification_vectors",
        ],
        &["rejects_bad_inputs"],
    );
}
