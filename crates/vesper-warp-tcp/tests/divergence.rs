//! The divergence check: a Vesper build with one deliberate error per ported
//! module (seq, rtt, cc, wire, conn; see `DIVERGENT_EDITS` in build.rs) must
//! fail the tests that cover each module, and must still pass a test that
//! touches none of them. Bazel passes the divergent test binaries; under
//! Cargo the check is scripts/test-port-harness.sh. Without the variables
//! these tests have nothing to run and pass.
use std::process::Command;

fn check(var: &str, failing: &[&str], passing: &str) {
    let Some(binary) = std::env::var_os(var) else {
        return;
    };
    let output = Command::new(&binary)
        .arg("--test-threads=1")
        .output()
        .expect("run the divergent port tests");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !output.status.success(),
        "the divergent port passed {var}:\n{stdout}"
    );
    for test in failing {
        assert!(
            stdout.contains(&format!("test {test} ... FAILED")),
            "the divergent port did not fail {test}:\n{stdout}"
        );
    }
    assert!(
        stdout.contains(&format!("test {passing} ... ok")),
        "the unchanged code must still agree ({passing}):\n{stdout}"
    );
}

/// seq, rtt and cc.
#[test]
fn divergent_units_fail() {
    check(
        "VESPER_TCP_DIVERGENT_UNITS",
        &[
            "wraparound_ordering",
            "rfc6298_formulas_and_backoff",
            "slow_start_then_one_reduction_per_episode",
        ],
        "rfc1071_numeric_example",
    );
}

/// wire.
#[test]
fn divergent_fixtures_fail() {
    check(
        "VESPER_TCP_DIVERGENT_FIXTURES",
        &[
            "fixture_checksums_verify_and_recompute",
            "fixtures_reserialize_byte_for_byte",
        ],
        "fixture_fields_and_options_decode",
    );
}

/// conn (and wire, through reset checksums).
#[test]
fn divergent_state_machine_fails() {
    check(
        "VESPER_TCP_DIVERGENT_STATE_MACHINE",
        &["third_duplicate_ack_triggers_fast_retransmit"],
        "listen_close_goes_to_closed",
    );
}
