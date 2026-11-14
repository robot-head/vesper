import Lean4LeanLevel42.Completion
import Lean4LeanLevel42.MapHelpers
import Lean4LeanLevel42.LevelSemantics
import Lean4LeanLevel42.OrderedInsert

/-! Completion certificates for the actual recursive construction, before
subsumption. The current path has its own independent completion certificate. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level Lean.Level.Normalize

theorem map_addConst_varSupport (s : NormLevel) (k : Nat) (p : List Name) :
    VarSupport s (s.addConst k p) := by
  intro q n a j hq hv
  obtain ⟨m, ⟨b, i⟩, hm, hw, he⟩ := map_addConst_preserves_var k p hq hv
  have hb : b = a := he
  subst b
  exact ⟨q, m, i, hm, hw, fun _ h => h⟩

theorem map_addNode_varSupport (s : NormLevel) (v : Name) (k : Nat) (p : List Name) :
    VarSupport s (s.addNode v k p) := by
  intro q n a j hq hv
  obtain ⟨m, ⟨b, i⟩, hm, hw, he⟩ := map_addNode_preserves_var v k p hq hv
  have hb : b = a := he
  subst b
  exact ⟨q, m, i, hm, hw, fun _ h => h⟩

theorem map_addVar_varSupport (s : NormLevel) (v : Name) (k : Nat) (p : List Name) :
    VarSupport s (s.addVar v k p) := by
  intro q n a j hq hv
  obtain ⟨m, ⟨b, i⟩, hm, hw, he⟩ := map_addVar_preserves_var v k p hq hv
  have hb : b = a := he
  subst b
  exact ⟨q, m, i, hm, hw, fun _ h => h⟩

theorem map_addVar_key_origin (s : NormLevel) (v : Name) (k : Nat) (p : List Name) :
    ∀ q n, (s.addVar v k p).get? q = some n → ∃ m, s.get? q = some m := by
  intro q n hq
  rw [NormLevel.addVar, map_get_modify] at hq
  split at hq
  next he =>
    subst q
    cases hs : s.get? p with
    | none => rw [hs] at hq; contradiction
    | some m => exact ⟨m, rfl⟩
  next => exact ⟨n, hq⟩

theorem map_addVar_completion (s : NormLevel) (v : Name) (k : Nat) (p : List Name)
    (h : MapCompletes s) : MapCompletes (s.addVar v k p) := by
  exact mapCompletes_support s _ (map_addVar_varSupport s v k p)
    (map_addVar_key_origin s v k p) h

theorem map_addConst_completion (s : NormLevel) (k : Nat) (p : List Name)
    (h : MapCompletes s) (hp : Completes s [] p) : MapCompletes (s.addConst k p) := by
  have hs := map_addConst_varSupport s k p
  intro q n hq
  have ho : q = p ∨ ∃ m, s.get? q = some m := by
    unfold NormLevel.addConst at hq
    split at hq
    · exact Or.inr ⟨n, hq⟩
    · rw [map_get_alter] at hq
      split at hq
      next he => exact Or.inl he.symm
      next => exact Or.inr ⟨n, hq⟩
  rcases ho with rfl | ⟨m, hm⟩
  · exact completes_support s _ hs [] q hp
  · exact completes_support s _ hs [] q (h q m hm)

/-- Appending one certified edge executes after every edge of the old order. -/
theorem construction_admissible_snoc (s : NormLevel) (acc order : List Name) (a : Name)
    (h : Admissible s acc order) (ha : EdgeCovered s (order.reverse ++ acc) a) :
    Admissible s acc (order ++ [a]) := by
  induction order generalizing acc with
  | nil => exact ⟨ha, trivial⟩
  | cons b rest ih =>
    change EdgeCovered s acc b ∧ Admissible s (b :: acc) (rest ++ [a])
    refine ⟨h.1, ih (b :: acc) h.2 ?_⟩
    simpa only [List.reverse_cons, List.append_assoc, List.singleton_append] using ha

theorem addNode_inserted_path_completes (s : NormLevel) (v : Name) (k : Nat)
    (p p' : List Name) (hi : Normalize.orderedInsert Name.cmp v p = some p')
    (hp : Completes s [] p) : Completes (s.addNode v k p') [] p' := by
  have hs := map_addNode_varSupport s v k p'
  obtain ⟨order, hperm, hadm⟩ := completes_support s _ hs [] p hp
  obtain ⟨n, ⟨b, j⟩, hn, hv, he⟩ := map_addNode_var s v k p'
  have hb : b = v := he
  subst b
  have hedge : EdgeCovered (s.addNode v k p') (order.reverse ++ []) v := by
    refine ⟨p', n, j, hn, hv, ?_⟩
    intro a ha
    rcases (orderedInsert_some_mem hi).mp ha with he | ha
    · exact List.mem_cons.mpr (Or.inl he)
    · apply List.mem_cons.mpr
      right
      simp only [List.append_nil, List.mem_reverse]
      exact hperm.mem_iff.mpr ha
  refine ⟨order ++ [v], ?_, construction_admissible_snoc _ [] order v hadm hedge⟩
  exact ((List.perm_append_singleton v order).trans (hperm.cons v)).trans
    (orderedInsert_some_perm Name.cmp v hi).symm

theorem map_addNode_completion (s : NormLevel) (v : Name) (k : Nat) (p p' : List Name)
    (hi : Normalize.orderedInsert Name.cmp v p = some p')
    (h : MapCompletes s) (hp : Completes s [] p) :
    MapCompletes (s.addNode v k p') ∧ Completes (s.addNode v k p') [] p' := by
  have hs := map_addNode_varSupport s v k p'
  have hnew := addNode_inserted_path_completes s v k p p' hi hp
  refine ⟨?_, hnew⟩
  intro q n hq
  rw [NormLevel.addNode, map_get_alter] at hq
  split at hq
  next he => subst q; exact hnew
  next => exact completes_support s _ hs [] q (h q n hq)

theorem normalizeAux_completion (l : Level) (p : List Name) (k : Nat) (acc : NormLevel)
    (h : MapCompletes acc) (hp : Completes acc [] p) :
    MapCompletes (Normalize.normalizeAux l p k acc) ∧
      VarSupport acc (Normalize.normalizeAux l p k acc) := by
  cases l with
  | zero =>
    rw [Normalize.normalizeAux]
    exact ⟨map_addConst_completion acc k p h hp, map_addConst_varSupport acc k p⟩
  | succ u =>
    rw [Normalize.normalizeAux]
    exact normalizeAux_completion u p (k + 1) acc h hp
  | max u v =>
    rw [Normalize.normalizeAux]
    have hu := normalizeAux_completion u p k acc h hp
    have hv := normalizeAux_completion v p k _ hu.1 (completes_support acc _ hu.2 [] p hp)
    exact ⟨hv.1, varSupport_trans acc _ _ hu.2 hv.2⟩
  | imax u r =>
    cases r with
    | zero =>
      rw [Normalize.normalizeAux]
      exact ⟨map_addConst_completion acc k p h hp, map_addConst_varSupport acc k p⟩
    | succ v =>
      rw [Normalize.normalizeAux]
      have hu := normalizeAux_completion u p k acc h hp
      have hv := normalizeAux_completion v p (k + 1) _ hu.1
        (completes_support acc _ hu.2 [] p hp)
      exact ⟨hv.1, varSupport_trans acc _ _ hu.2 hv.2⟩
    | max v w =>
      rw [Normalize.normalizeAux]
      have hv := normalizeAux_completion (.imax u v) p k acc h hp
      have hw := normalizeAux_completion (.imax u w) p k _ hv.1
        (completes_support acc _ hv.2 [] p hp)
      exact ⟨hw.1, varSupport_trans acc _ _ hv.2 hw.2⟩
    | imax v w =>
      rw [Normalize.normalizeAux]
      have hu := normalizeAux_completion (.imax u w) p k acc h hp
      have hv := normalizeAux_completion (.imax v w) p k _ hu.1
        (completes_support acc _ hu.2 [] p hp)
      exact ⟨hv.1, varSupport_trans acc _ _ hu.2 hv.2⟩
    | param v =>
      rw [Normalize.normalizeAux]
      cases hi : Normalize.orderedInsert Name.cmp v p with
      | some p' =>
        have hc := map_addConst_varSupport acc k p
        have hn := map_addNode_completion (acc.addConst k p) v k p p' hi
          (map_addConst_completion acc k p h hp) (completes_support acc _ hc [] p hp)
        have hs := varSupport_trans acc _ _ hc (map_addNode_varSupport _ v k p')
        have hu := normalizeAux_completion u p' k _ hn.1 hn.2
        exact ⟨hu.1, varSupport_trans acc _ _ hs hu.2⟩
      | none =>
        dsimp only
        by_cases hk : k = 0
        · simp only [if_pos hk]
          exact normalizeAux_completion u p k acc h hp
        · simp only [if_neg hk]
          have hs := map_addVar_varSupport acc v k p
          have hu := normalizeAux_completion u p k _ (map_addVar_completion acc v k p h)
            (completes_support acc _ hs [] p hp)
          exact ⟨hu.1, varSupport_trans acc _ _ hs hu.2⟩
    | mvar _ =>
      rw [Normalize.normalizeAux]
      exact ⟨h, varSupport_refl acc⟩
  | param v =>
    rw [Normalize.normalizeAux]
    cases hi : Normalize.orderedInsert Name.cmp v p with
    | some p' =>
      have hc := map_addConst_varSupport acc k p
      have hn := map_addNode_completion (acc.addConst k p) v k p p' hi
        (map_addConst_completion acc k p h hp) (completes_support acc _ hc [] p hp)
      exact ⟨hn.1, varSupport_trans acc _ _ hc (map_addNode_varSupport _ v k p')⟩
    | none =>
      by_cases hk : k = 0
      · simp only [if_pos hk]
        exact ⟨h, varSupport_refl acc⟩
      · simp only [if_neg hk]
        exact ⟨map_addVar_completion acc v k p h, map_addVar_varSupport acc v k p⟩
  | mvar _ =>
    rw [Normalize.normalizeAux]
    exact ⟨h, varSupport_refl acc⟩
termination_by levelSize l
decreasing_by all_goals simp_wf; simp only [levelSize]; omega

theorem normalizeAux_preserves_completes (l : Level) (p : List Name) (k : Nat)
    (acc : NormLevel) (h : MapCompletes acc) (hp : Completes acc [] p)
    (prior rem : List Name) (hseed : Completes acc prior rem) :
    Completes (Normalize.normalizeAux l p k acc) prior rem := by
  exact completes_support acc _ (normalizeAux_completion l p k acc h hp).2 prior rem hseed

theorem normalizeAux_empty_completion (l : Level) :
    MapCompletes (Normalize.normalizeAux l [] 0 {}) := by
  exact (normalizeAux_completion l [] 0 {} mapCompletes_empty (completes_nil {} [])).1

end Vesper.Lean4LeanLevel42
