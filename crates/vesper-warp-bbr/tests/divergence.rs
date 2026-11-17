//! The divergence check of the Vesper port (#907): a deliberately wrong
//! Vesper build, one change per ported module (`DIVERGENT_EDITS` in
//! build.rs), must make exactly those modules' unit tests fail. Bazel passes
//! the divergent unit-test binary; under Cargo the check is
//! scripts/test-port-harness.sh. Without the variable this test has nothing
//! to run and passes.
use std::process::Command;

/// The unit test each divergent edit breaks, one per ported module.
const FAILING: &[&str] = &[
    "pacing::tests::pacing_delay_is_ceiling_and_defined_at_zero",
    "filter::tests::cycle_max_keeps_two_cycles",
    "bbr::tests::persistent_congestion_collapses_cwnd_then_floors_at_min_pipe",
];

/// Tests of functions the edits do not touch, which must still agree.
const PASSING: &[&str] = &[
    "bbr::tests::inflight_at_loss_solves_for_the_threshold_crossing",
    "filter::tests::prng_is_deterministic_and_bounded",
    "pacing::tests::clamp_bounds",
];

#[test]
fn divergent_port_fails_one_test_per_module() {
    let Some(binary) = std::env::var_os("VESPER_PORT_DIVERGENT_TEST") else {
        return;
    };
    let output = Command::new(&binary)
        .arg("--test-threads=1")
        .output()
        .expect("run the divergent port's unit tests");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !output.status.success(),
        "the divergent port passed the unit tests:\n{stdout}"
    );
    for test in FAILING {
        assert!(
            stdout.contains(&format!("test {test} ... FAILED")),
            "the divergent port did not fail {test}:\n{stdout}"
        );
    }
    for test in PASSING {
        assert!(
            stdout.contains(&format!("test {test} ... ok")),
            "the unchanged functions must still agree ({test}):\n{stdout}"
        );
    }
}
