import Lean4LeanLevel42.LevelSemantics
import Lean4LeanLevel42.PathPriming

/-! Preservation for the actual recursive construction, before subsumption.
The induction measure covers the fresh imax expressions produced by the
max-right and imax-right rewrites as well as structural recursive calls. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level Lean.Level.Normalize

/-- Any property preserved by the actual map updates is preserved by the
actual construction. Variable updates need membership in their actual key;
no ordering or current-path coverage is assumed. -/
theorem normalizeAux_preserves_property
    (I : NormLevel → Prop)
    (hc : ∀ (s : NormLevel) (k : Nat) (p : List Name), I s → I (s.addConst k p))
    (hn : ∀ (s : NormLevel) (v : Name) (k : Nat) (p : List Name),
      v ∈ p → I s → I (s.addNode v k p))
    (hv : ∀ (s : NormLevel) (v : Name) (k : Nat) (p : List Name),
      v ∈ p → I s → I (s.addVar v k p))
    (l : Level) (p : List Name) (k : Nat) (acc : NormLevel) (h : I acc) :
    I (Normalize.normalizeAux l p k acc) := by
  cases l with
  | zero =>
    rw [Normalize.normalizeAux]
    exact hc acc k p h
  | succ u =>
    rw [Normalize.normalizeAux]
    exact normalizeAux_preserves_property I hc hn hv u p (k + 1) acc h
  | max u v =>
    rw [Normalize.normalizeAux]
    exact normalizeAux_preserves_property I hc hn hv v p k _
        (normalizeAux_preserves_property I hc hn hv u p k acc h)
  | imax u r =>
    cases r with
    | zero =>
      rw [Normalize.normalizeAux]
      exact hc acc k p h
    | succ v =>
      rw [Normalize.normalizeAux]
      exact normalizeAux_preserves_property I hc hn hv v p (k + 1) _
          (normalizeAux_preserves_property I hc hn hv u p k acc h)
    | max v w =>
      rw [Normalize.normalizeAux]
      exact normalizeAux_preserves_property I hc hn hv (.imax u w) p k _
          (normalizeAux_preserves_property I hc hn hv (.imax u v) p k acc h)
    | imax v w =>
      rw [Normalize.normalizeAux]
      exact normalizeAux_preserves_property I hc hn hv (.imax v w) p k _
          (normalizeAux_preserves_property I hc hn hv (.imax u w) p k acc h)
    | param v =>
      rw [Normalize.normalizeAux]
      cases hi : Normalize.orderedInsert Name.cmp v p with
      | some p' =>
        exact normalizeAux_preserves_property I hc hn hv u p' k _
          (hn _ v k p' (orderedInsert_inserted_mem hi) (hc acc k p h))
      | none =>
        apply normalizeAux_preserves_property I hc hn hv u p k
        split
        · exact h
        · exact hv acc v k p (orderedInsert_none_mem hi) h
    | mvar _ =>
      rw [Normalize.normalizeAux]
      exact h
  | param v =>
    rw [Normalize.normalizeAux]
    cases hi : Normalize.orderedInsert Name.cmp v p with
    | some p' =>
      exact hn _ v k p' (orderedInsert_inserted_mem hi) (hc acc k p h)
    | none =>
      change I (if k = 0 then acc else acc.addVar v k p)
      split
      · exact h
      · exact hv acc v k p (orderedInsert_none_mem hi) h
  | mvar _ =>
    rw [Normalize.normalizeAux]
    exact h
termination_by levelSize l
decreasing_by all_goals simp_wf; simp only [levelSize]; omega

/-- Priming at an arbitrary independent path survives construction at p. -/
theorem normalizeAux_preserves_pathPrimed
    (l : Level) (p q : List Name) (k : Nat) (acc : NormLevel)
    (h : PathPrimed q acc) :
    PathPrimed q (Normalize.normalizeAux l p k acc) := by
  apply normalizeAux_preserves_property (PathPrimed q)
    (fun _ k p h => pathPrimed_addConst h k p)
    (fun _ v k p _ h => pathPrimed_addNode h v k p)
    (fun _ v k p _ h => pathPrimed_addVar h v k p) l p k acc h

/-- All constructed variable names belong to their actual stored row keys. -/
theorem normalizeAux_preserves_WF
    (l : Level) (p : List Name) (k : Nat) (acc : NormLevel)
    (h : MapWF acc) : MapWF (Normalize.normalizeAux l p k acc) := by
  apply normalizeAux_preserves_property MapWF
    (fun _ k p h => map_addConst_WF k p h)
    (fun _ v k p hv h => map_addNode_WF v k p h hv)
    (fun _ v k p hv h => map_addVar_WF v k p h hv) l p k acc h

theorem normalizeAux_empty_WF (l : Level) :
    MapWF (Normalize.normalizeAux l [] 0 {}) := by
  exact normalizeAux_preserves_WF l [] 0 {} mapWF_empty

end Vesper.Lean4LeanLevel42
