import Lean4LeanLevel42.MapEquality
import Lean4LeanLevel42.ConstructionShape
import Lean4LeanLevel42.SubsumptionShape

/-! Shape of the actual normalizer and soundness of its transparent map-equality
branch. This does not reflect opaque native Level.beq or prove canonicality. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level Lean.Level.Normalize

theorem normalize_keysSorted (l : Level) :
    MapKeysSorted (Normalize.normalize l) := by
  exact subsumption_keysSorted _
    (normalizeAux_keysSorted l [] 0 {} (by simp [KeySorted]) map_keysSorted_empty)

theorem normalize_varsSorted (l : Level) :
    MapVarsSorted (Normalize.normalize l) := by
  exact subsumption_varsSorted _
    (normalizeAux_varsSorted l [] 0 {} map_varsSorted_empty)

theorem normalize_noEmpty (l : Level) :
    MapNoEmpty (Normalize.normalize l) := by
  exact subsumption_noEmpty _

theorem normalize_shape (l : Level) :
    MapKeysSorted (Normalize.normalize l) ∧
      MapVarsSorted (Normalize.normalize l) ∧
      MapNoEmpty (Normalize.normalize l) :=
  ⟨normalize_keysSorted l, normalize_varsSorted l, normalize_noEmpty l⟩

theorem normalized_beq_value (ρ : Valuation) (u v : Level)
    (hu : NoMVars u) (hv : NoMVars v)
    (h : (Normalize.normalize u == Normalize.normalize v) = true) :
    levelValue ρ u = levelValue ρ v := by
  have hm := norm_beq_mapValue ρ (Normalize.normalize u) (Normalize.normalize v) h
  unfold Normalize.normalize at hm
  rw [subsumption_value ρ _ (normalizeAux_empty_WF u),
    subsumption_value ρ _ (normalizeAux_empty_WF v),
    normalizeAux_empty_value ρ u hu, normalizeAux_empty_value ρ v hv] at hm
  exact hm

end Vesper.Lean4LeanLevel42
