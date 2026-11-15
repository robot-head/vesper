import Lean4LeanLevel42.ConstructionCompletion
import Lean4LeanLevel42.VariableSupport
import Lean4LeanLevel42.NormalizeInvariants

/-! Completion and row well-formedness of the actual complete normalizer.
Both properties hold for every level, independently of denotation assumptions. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level Lean.Level.Normalize

theorem normalize_mapCompletes (l : Level) :
    MapCompletes (Normalize.normalize l) := by
  exact subsumption_mapCompletes _ (normalizeAux_empty_completion l)

theorem normalize_mapCompletes_WF (l : Level) :
    MapCompletes (Normalize.normalize l) ∧ MapWF (Normalize.normalize l) := by
  exact ⟨normalize_mapCompletes l, subsumption_WF _ (normalizeAux_empty_WF l)⟩

end Vesper.Lean4LeanLevel42
