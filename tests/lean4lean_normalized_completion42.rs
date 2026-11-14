//! Actual normalized-map completion and well-formedness composition.
#[path = "common/lean4lean_proof_packet.rs"]
pub mod proof_packet;

#[test]
fn actual_normalized_completion_exact_artifacts_and_complete_axiom_policy() {
    proof_packet::run(proof_packet::Packet::NormalizedCompletion);
}
