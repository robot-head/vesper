//! Bazel checks of the Vesper port (port wave P1 lane 7, #904): the
//! divergent builds (BUILD.bazel `PORT_DIVERGENT`, the edits of `DIVERGENT`
//! in build.rs, one build per ported Rust module) each fail their test, and the
//! committed binding is the one the compiler generates. Bazel passes the
//! paths; under Cargo, build.rs checks the binding and
//! scripts/test-port-harness.sh runs the divergence check, so without the
//! variables these tests have nothing to run and pass.
use std::process::Command;

fn run(var: &str, failing: &[&str], passing: &str) {
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
        "the divergent port passed:\n{stdout}"
    );
    for test in failing {
        assert!(
            stdout.contains(&format!("test {test} ... FAILED")),
            "the divergent port did not fail {test}:\n{stdout}"
        );
    }
    assert!(
        stdout.contains(&format!("test {passing} ... ok")),
        "the divergent port failed an unchanged test:\n{stdout}"
    );
}

#[test]
fn divergent_lexer_fails_the_front_end_transcript() {
    run(
        "VESPER_QUARRY_DIVERGENT_LEX",
        &["front_end_transcript_matches_golden"],
        "golden_results_equal_filtered_index_lookups",
    );
}

#[test]
fn divergent_ddl_fails_the_ddl_test() {
    run(
        "VESPER_QUARRY_DIVERGENT_DDL",
        &["ddl_atomic_deterministic_and_stale_revision_or_content_rejected"],
        "real_where_aliases_literals_and_typed_bindings",
    );
}

#[test]
fn divergent_checker_and_optimizer_fail_the_outer_join_tests() {
    run(
        "VESPER_QUARRY_DIVERGENT_PLAN",
        &[
            "outer_join_null_extension_and_filter_placement",
            "outer_join_pushdown_differential_for_many_predicates_and_null_inputs",
        ],
        "real_where_aliases_literals_and_typed_bindings",
    );
}

#[test]
fn divergent_executor_fails_three_valued_logic() {
    run(
        "VESPER_QUARRY_DIVERGENT_VALUES",
        &["sql_null_three_valued_logic_exhaustive"],
        "real_where_aliases_literals_and_typed_bindings",
    );
}

#[test]
fn divergent_access_planner_fails_the_golden_access_paths() {
    run(
        "VESPER_QUARRY_DIVERGENT_STATS",
        &["planner_chooses_the_golden_access_paths"],
        "golden_results_equal_filtered_index_lookups",
    );
}

#[test]
fn divergent_page_fails_the_corrupt_header_test() {
    run(
        "VESPER_QUARRY_DIVERGENT_PAGE",
        &["malicious_crc_valid_slot_layouts_fail_closed"],
        "exact_little_endian_header_and_actual_record_roundtrip",
    );
}

#[test]
fn divergent_wal_fails_reopen_recovery_test() {
    run(
        "VESPER_QUARRY_DIVERGENT_WAL",
        &["committed_changes_survive_reopen_and_aborts_roll_back"],
        "page_locks_conflict_without_waiting_and_handles_are_checked",
    );
}

#[test]
fn committed_binding_is_current() {
    let Some(generated) = std::env::var_os("VESPER_QUARRY_BINDING") else {
        return;
    };
    let generated = std::fs::read_to_string(generated).expect("read the generated binding");
    let committed = std::fs::read_to_string("crates/vesper-quarry/port/binding.rs")
        .expect("read port/binding.rs");
    assert!(
        committed.replace("\r\n", "\n") == generated.replace("\r\n", "\n"),
        "port/binding.rs is stale: rebuild with VESPER_PORT_BLESS=1"
    );
}
