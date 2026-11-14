import Lean4LeanLevel42.NormalizeValue
import Lean4LeanLevel42.MapSubsumption

/-! Exact denotation and map well-formedness of the unchanged actual normalizer. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level Lean.Level.Normalize

theorem normalize_value (ρ : Valuation) (l : Level) (h : NoMVars l) :
    mapValue ρ (Normalize.normalize l) = levelValue ρ l := by
  unfold Normalize.normalize
  rw [subsumption_value ρ _ (normalizeAux_empty_WF l)]
  exact normalizeAux_empty_value ρ l h

theorem normalize_WF (l : Level) : MapWF (Normalize.normalize l) := by
  exact subsumption_WF _ (normalizeAux_empty_WF l)

/-- A final, actual normalization of imax over max, including subsumption. -/
theorem normalize_value_witness :
    let a : Level := .param (.num .anonymous 0)
    let b : Level := .param (.num .anonymous 1)
    let c : Level := .param (.num .anonymous 2)
    mapValue (fun _ => 2) (Normalize.normalize (.imax a (.max b c))) = 2 ∧
      MapWF (Normalize.normalize (.imax a (.max b c))) := by
  dsimp only
  constructor
  · rw [normalize_value _ _ (by trivial)]
    rfl
  · exact normalize_WF _

end Vesper.Lean4LeanLevel42
