//! Actual addability witnesses and the raw cyclic reification boundary.
#[path = "common/lean4lean_proof_packet.rs"]
pub mod proof_packet;

#[test]
fn actual_addability_proofs_exact_artifacts_and_complete_axiom_policy() {
    proof_packet::run(proof_packet::Packet::Addability);
}
