//! Equality and sorted/nonempty shape of the actual normalizer maps.
#[path = "common/lean4lean_proof_packet.rs"]
pub mod proof_packet;

#[test]
fn actual_normalizer_shape_proofs_exact_artifacts_and_complete_axiom_policy() {
    proof_packet::run(proof_packet::Packet::Shape);
}
