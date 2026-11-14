//! Actual completion prerequisites and deterministic reconstruction congruence.
#[path = "common/lean4lean_proof_packet.rs"]
pub mod proof_packet;

#[test]
fn actual_completion_prerequisites_exact_artifacts_and_complete_axiom_policy() {
    proof_packet::run(proof_packet::Packet::Completion);
}
