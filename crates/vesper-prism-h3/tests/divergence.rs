//! The divergence check: a Vesper build with one deliberate error per ported
//! module (huffman, prefix, table, encoder, decoder, varint, frame,
//! priority, capsule, message, control, session, core, server, client;
//! `DIVERGENT_EDITS` in build.rs) must fail the test that guards that module,
//! and must still pass a test that the change does not reach. Bazel passes the
//! divergent test binaries; under Cargo the check is port/check.sh. Without the
//! variables these tests have nothing to run and pass.
use std::path::PathBuf;
use std::process::Command;

fn check(module: &str, failing: &str, passing: &str) {
    let var = format!("VESPER_H3_DIVERGENT_{}", module.to_uppercase());
    let Some(binary) = std::env::var_os(&var) else {
        return;
    };
    let binary = std::path::absolute(PathBuf::from(binary)).expect("absolute binary path");
    let cwd = std::env::var_os("VESPER_H3_DIVERGENT_CWD").unwrap_or_else(|| ".".into());
    let output = Command::new(&binary)
        .arg("--test-threads=1")
        .current_dir(cwd)
        .output()
        .expect("run the divergent port tests");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !output.status.success(),
        "the divergent port ({module}) passed:\n{stdout}"
    );
    assert!(
        stdout.contains(&format!("test {failing} ... FAILED")),
        "the divergent port ({module}) did not fail {failing}:\n{stdout}"
    );
    assert!(
        stdout.contains(&format!("test {passing} ... ok")),
        "the unchanged code must still agree ({module}, {passing}):\n{stdout}"
    );
}

#[test]
fn divergent_decoder_fails() {
    check(
        "decoder",
        "committed_qifs_subset_decodes",
        "full_qifs_corpus_when_available",
    );
}

#[test]
fn divergent_prefix_fails() {
    check(
        "prefix",
        "committed_qifs_subset_decodes",
        "full_qifs_corpus_when_available",
    );
}

#[test]
fn divergent_encoder_fails() {
    check(
        "encoder",
        "appendix_b_encoder_side",
        "appendix_b_decoder_side",
    );
}

#[test]
fn divergent_huffman_fails() {
    check(
        "huffman",
        "appendix_b_encoder_side",
        "appendix_b_decoder_side",
    );
}

#[test]
fn divergent_table_fails() {
    check(
        "table",
        "appendix_b_decoder_side",
        "appendix_b_encoder_side",
    );
}

#[test]
fn divergent_varint_fails() {
    check(
        "varint",
        "frames_round_trip_split_anywhere",
        "rfc9218_priorities",
    );
}

#[test]
fn divergent_frame_fails() {
    check(
        "frame",
        "malformed_frames_are_typed_errors",
        "capsules_round_trip_split_anywhere",
    );
}

#[test]
fn divergent_priority_fails() {
    check(
        "priority",
        "rfc9218_priorities",
        "malformed_frames_are_typed_errors",
    );
}

#[test]
fn divergent_capsule_fails() {
    check(
        "capsule",
        "capsules_round_trip_split_anywhere",
        "frames_round_trip_split_anywhere",
    );
}

#[test]
fn divergent_message_fails() {
    check(
        "message",
        "content_length_and_host_rules",
        "response_statuses",
    );
}

#[test]
fn divergent_control_fails() {
    check(
        "control",
        "bad_priority_updates_are_connection_errors",
        "priority_update_overrides_the_header",
    );
}

#[test]
fn divergent_session_fails() {
    check(
        "session",
        "held_stream_bound_and_gone_sessions_are_refused",
        "without_quic_datagrams_http_datagrams_use_capsules",
    );
}

#[test]
fn divergent_core_fails() {
    check(
        "core",
        "incremental_responses_of_one_urgency_share_capacity",
        "streaming_bodies_are_pulled_under_a_window",
    );
}

#[test]
fn divergent_server_fails() {
    check(
        "server",
        "goaway_refuses_later_requests",
        "request_and_response_round_trip",
    );
}

#[test]
fn divergent_client_fails() {
    check(
        "client",
        "each_lower_goaway_rejects_newly_excluded_requests",
        "request_and_response_round_trip",
    );
}
