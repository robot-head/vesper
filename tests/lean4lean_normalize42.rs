//! Actual recursive normalizeAux construction before subsumption.
#[path = "common/lean4lean_proof_packet.rs"]
pub mod proof_packet;

#[test]
fn actual_normalize_proofs_exact_artifacts_and_complete_axiom_policy() {
    proof_packet::run(proof_packet::Packet::Normalize);
}
