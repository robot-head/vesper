//! Actual raw-map toTree reification denotation.
#[path = "common/lean4lean_proof_packet.rs"]
pub mod proof_packet;

#[test]
fn actual_map_tree_denotation_exact_artifacts_and_complete_axiom_policy() {
    proof_packet::run(proof_packet::Packet::MapTree);
}
