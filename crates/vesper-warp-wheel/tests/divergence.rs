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

/// The clock edit (moving back by 1 ns is accepted).
#[test]
fn divergent_clock_is_detected() {
    expect(
        "VESPER_WHEEL_DIVERGENT_WHEEL_TEST",
        &["clock_never_moves_backwards"],
        &["invalid_configs_are_rejected"],
    );
}

/// The wheel edit (an entry fires 1 ns early).
#[test]
fn divergent_wheel_is_detected() {
    expect(
        "VESPER_WHEEL_DIVERGENT_MODEL_TEST",
        &["nothing_fires_early_within_a_tick"],
        &["equal_deadlines_fire_fifo_across_cascades"],
    );
}

/// The lease edit (packet number 2^62 is accepted) and the EDT edit (an EDT
/// exactly at the horizon is refused).
#[test]
fn divergent_leases_and_edt_are_detected() {
    expect(
        "VESPER_WHEEL_DIVERGENT_LAYERS_TEST",
        &[
            "park_refusals_return_the_lease",
            "edt_horizon_and_order_rejections_return_packet",
        ],
        &["discard_space_and_index_stay_consistent_under_churn"],
    );
}
