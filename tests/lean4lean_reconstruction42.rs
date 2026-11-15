//! Actual construction completion, admissible chains and tree primitives.
#[path = "common/lean4lean_proof_packet.rs"]
pub mod proof_packet;

#[test]
fn actual_reconstruction_prerequisites_exact_artifacts_and_complete_axiom_policy() {
    proof_packet::run(proof_packet::Packet::Reconstruction);
}
