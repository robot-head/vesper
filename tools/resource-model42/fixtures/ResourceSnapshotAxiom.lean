import Std

/-! Projection is not axiom-policy approval. The final unchanged strict
artifact auditor must reject this project's explicit axiom. -/
namespace ResourceSnapshotAxiom

axiom projectFalse : False

theorem exposed : False := projectFalse

end ResourceSnapshotAxiom
