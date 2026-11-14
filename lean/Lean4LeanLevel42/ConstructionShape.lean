import Lean4LeanLevel42.Shape

/-! Strict key and variable-name order for the actual map updates and recursive
construction. These structural facts hold even for levels with metavariables. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level Lean.Level.Normalize

theorem map_keysSorted_empty : MapKeysSorted ({} : NormLevel) := by
  intro p n h
  rw [map_get_empty] at h
  contradiction

theorem map_varsSorted_empty : MapVarsSorted ({} : NormLevel) := by
  intro p n h
  rw [map_get_empty] at h
  contradiction

theorem map_addConst_keysSorted (k : Nat) (p : List Name) (s : NormLevel)
    (hp : KeySorted p) (hs : MapKeysSorted s) : MapKeysSorted (s.addConst k p) := by
  unfold NormLevel.addConst
  split
  · exact hs
  · intro q n hn
    rw [map_get_alter] at hn
    split at hn
    · rename_i he
      subst q
      exact hp
    · exact hs q n hn

theorem map_addNode_keysSorted (v : Name) (k : Nat) (p : List Name) (s : NormLevel)
    (hp : KeySorted p) (hs : MapKeysSorted s) : MapKeysSorted (s.addNode v k p) := by
  intro q n hn
  rw [NormLevel.addNode, map_get_alter] at hn
  split at hn
  · rename_i he
    subst q
    exact hp
  · exact hs q n hn

/-- Modify creates no key, so its supplied path need not be sorted. -/
theorem map_addVar_keysSorted (v : Name) (k : Nat) (p : List Name) (s : NormLevel)
    (hs : MapKeysSorted s) : MapKeysSorted (s.addVar v k p) := by
  intro q n hn
  rw [NormLevel.addVar, map_get_modify] at hn
  split at hn
  · rename_i he
    subst q
    cases h : s.get? p with
    | none => simp only [h, Option.map_none, reduceCtorEq] at hn
    | some m => exact hs p m h
  · exact hs q n hn

theorem map_addConst_varsSorted (k : Nat) (p : List Name) (s : NormLevel)
    (hs : MapVarsSorted s) : MapVarsSorted (s.addConst k p) := by
  unfold NormLevel.addConst
  split
  · exact hs
  · intro q n hn
    rw [map_get_alter] at hn
    split at hn
    · rename_i he
      subst q
      cases h : s.get? p with
      | none =>
        simp only [h, Option.some.injEq] at hn
        subst n
        simp [VarsSorted]
      | some m =>
        simp only [h, Option.some.injEq] at hn
        subst n
        exact hs p m h
    · exact hs q n hn

theorem map_addNode_varsSorted (v : Name) (k : Nat) (p : List Name) (s : NormLevel)
    (hs : MapVarsSorted s) : MapVarsSorted (s.addNode v k p) := by
  intro q n hn
  rw [NormLevel.addNode, map_get_alter] at hn
  split at hn
  · rename_i he
    subst q
    cases h : s.get? p with
    | none =>
      simp only [h, Option.some.injEq] at hn
      subst n
      simp [VarsSorted]
    | some m =>
      simp only [h, Option.some.injEq] at hn
      subst n
      exact addVar_sorted v k (hs p m h)
  · exact hs q n hn

theorem map_addVar_varsSorted (v : Name) (k : Nat) (p : List Name) (s : NormLevel)
    (hs : MapVarsSorted s) : MapVarsSorted (s.addVar v k p) := by
  intro q n hn
  rw [NormLevel.addVar, map_get_modify] at hn
  split at hn
  · rename_i he
    subst q
    cases h : s.get? p with
    | none => simp only [h, Option.map_none, reduceCtorEq] at hn
    | some m =>
      simp only [h, Option.map_some, Option.some.injEq] at hn
      subst n
      exact addVar_sorted v k (hs p m h)
  · exact hs q n hn

/-- Variable order is independent of the current path's own order. -/
theorem normalizeAux_varsSorted (l : Level) (p : List Name) (k : Nat) (acc : NormLevel)
    (hs : MapVarsSorted acc) : MapVarsSorted (Normalize.normalizeAux l p k acc) := by
  apply normalizeAux_preserves_property MapVarsSorted
    (fun s k p h => map_addConst_varsSorted k p s h)
    (fun s v k p _ h => map_addNode_varsSorted v k p s h)
    (fun s v k p _ h => map_addVar_varsSorted v k p s h) l p k acc hs

/-- The actual recursively extended path remains sorted. The measure also
covers the fresh imax terms produced by the max-right and imax-right rewrites. -/
theorem normalizeAux_keysSorted (l : Level) (p : List Name) (k : Nat) (acc : NormLevel)
    (hp : KeySorted p) (hs : MapKeysSorted acc) :
    MapKeysSorted (Normalize.normalizeAux l p k acc) := by
  cases l with
  | zero =>
    rw [Normalize.normalizeAux]
    exact map_addConst_keysSorted k p acc hp hs
  | succ u =>
    rw [Normalize.normalizeAux]
    exact normalizeAux_keysSorted u p (k + 1) acc hp hs
  | max u v =>
    rw [Normalize.normalizeAux]
    exact normalizeAux_keysSorted v p k _ hp
      (normalizeAux_keysSorted u p k acc hp hs)
  | imax u r =>
    cases r with
    | zero =>
      rw [Normalize.normalizeAux]
      exact map_addConst_keysSorted k p acc hp hs
    | succ v =>
      rw [Normalize.normalizeAux]
      exact normalizeAux_keysSorted v p (k + 1) _ hp
        (normalizeAux_keysSorted u p k acc hp hs)
    | max v w =>
      rw [Normalize.normalizeAux]
      exact normalizeAux_keysSorted (.imax u w) p k _ hp
        (normalizeAux_keysSorted (.imax u v) p k acc hp hs)
    | imax v w =>
      rw [Normalize.normalizeAux]
      exact normalizeAux_keysSorted (.imax v w) p k _ hp
        (normalizeAux_keysSorted (.imax u w) p k acc hp hs)
    | param v =>
      rw [Normalize.normalizeAux]
      cases hi : Normalize.orderedInsert Name.cmp v p with
      | some p' =>
        have hp' : KeySorted p' := orderedInsert_sorted hp hi
        exact normalizeAux_keysSorted u p' k _ hp'
          (map_addNode_keysSorted v k p' _ hp'
            (map_addConst_keysSorted k p acc hp hs))
      | none =>
        apply normalizeAux_keysSorted u p k _ hp
        split
        · exact hs
        · exact map_addVar_keysSorted v k p acc hs
    | mvar _ =>
      rw [Normalize.normalizeAux]
      exact hs
  | param v =>
    rw [Normalize.normalizeAux]
    cases hi : Normalize.orderedInsert Name.cmp v p with
    | some p' =>
      exact map_addNode_keysSorted v k p' _ (orderedInsert_sorted hp hi)
        (map_addConst_keysSorted k p acc hp hs)
    | none =>
      change MapKeysSorted (if k = 0 then acc else acc.addVar v k p)
      split
      · exact hs
      · exact map_addVar_keysSorted v k p acc hs
  | mvar _ =>
    rw [Normalize.normalizeAux]
    exact hs
termination_by levelSize l
decreasing_by all_goals simp_wf; simp only [levelSize]; omega

theorem normalizeAux_empty_varsSorted (l : Level) :
    MapVarsSorted (Normalize.normalizeAux l [] 0 {}) :=
  normalizeAux_varsSorted l [] 0 {} map_varsSorted_empty

theorem normalizeAux_empty_keysSorted (l : Level) :
    MapKeysSorted (Normalize.normalizeAux l [] 0 {}) :=
  normalizeAux_keysSorted l [] 0 {} (by simp [KeySorted]) map_keysSorted_empty

/-- Actual construction at an arbitrary unsorted path can store that path. -/
theorem normalizeAux_needs_sorted_path :
    let z : Name := .num .anonymous 0
    let o : Name := .num .anonymous 1
    let p : List Name := [o, z]
    ¬ KeySorted p ∧ MapKeysSorted ({} : NormLevel) ∧
      ¬ MapKeysSorted (Normalize.normalizeAux .zero p 2 {}) := by
  dsimp only
  have hb : ¬ KeySorted [.num .anonymous 1, .num .anonymous 0] :=
    orderedInsert_unsorted_key_witness.1
  refine ⟨hb, map_keysSorted_empty, ?_⟩
  intro hs
  have hg : (Normalize.normalizeAux .zero [.num .anonymous 1, .num .anonymous 0]
      2 {}).get? [.num .anonymous 1, .num .anonymous 0] = some { const := 2 } := by
    rw [Normalize.normalizeAux]
    change (({} : NormLevel).alter [.num .anonymous 1, .num .anonymous 0] (fun
      | none => some { const := 2 }
      | some n => some { n with const := max 2 n.const })).get?
        [.num .anonymous 1, .num .anonymous 0] = _
    rw [map_get_alter, if_pos rfl, map_get_empty]
  exact hb (hs [.num .anonymous 1, .num .anonymous 0] { const := 2 } hg)

/-- The actual map modify merges an offset without disturbing strict names. -/
theorem map_addVar_varsSorted_merge_witness :
    let z : Name := .num .anonymous 0
    let o : Name := .num .anonymous 1
    let p : List Name := [z, o]
    let s : NormLevel := ({} : NormLevel).insert p { var := [⟨z, 2⟩, ⟨o, 7⟩] }
    MapVarsSorted s ∧
      (s.addVar z 5 p).get? p = some { var := [⟨z, 5⟩, ⟨o, 7⟩] } ∧
      MapVarsSorted (s.addVar z 5 p) := by
  dsimp only
  have hs : MapVarsSorted (({} : NormLevel).insert
      [.num .anonymous 0, .num .anonymous 1]
      { var := [⟨.num .anonymous 0, 2⟩, ⟨.num .anonymous 1, 7⟩] }) := by
    intro p n hn
    rw [map_get_insert, map_get_empty] at hn
    split at hn
    · cases hn
      exact addVar_numeric_sorted_merge.1
    · contradiction
  refine ⟨hs, ?_, map_addVar_varsSorted _ _ _ _ hs⟩
  rw [NormLevel.addVar, map_get_modify, if_pos rfl, map_get_insert, if_pos rfl]
  rfl

end Vesper.Lean4LeanLevel42
