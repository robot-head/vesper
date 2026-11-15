import Lean4LeanLevel42.NormalizeInvariants
import Lean4LeanLevel42.LevelSemantics

/-! Exact denotation of the actual recursive construction, before subsumption.
The domain excludes metavariables structurally; no cached flags are used. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level Lean.Level.Normalize

/-- The parameter branch includes both skipped updates and real map updates. -/
theorem normalizeAux_param_value (ρ : Valuation) (v : Name) (p : List Name)
    (k : Nat) (acc : NormLevel) (hp : PathPrimed p acc) :
    mapValue ρ (Normalize.normalizeAux (.param v) p k acc) =
      max (mapValue ρ acc) (if enabled ρ p then ρ v + k else 0) := by
  rw [Normalize.normalizeAux]
  cases hi : Normalize.orderedInsert Name.cmp v p with
  | some p' =>
    rw [map_addNode_value, map_addConst_value_of_primed ρ acc k p hp,
      orderedInsert_enabled ρ hi]
    cases he : enabled ρ p <;> by_cases hv : 0 < ρ v <;> simp [hv] <;> omega
  | none =>
    by_cases hk : k = 0
    · simp only [hk, ite_true, Nat.add_zero]
      cases he : enabled ρ p
      · simp
      · have hb := pathPrimed_variable_bound hp (orderedInsert_none_mem hi) he
        simp only [ite_true]
        omega
    · simp only [hk, ite_false]
      have hne : p ≠ [] := by
        intro h
        have hm := orderedInsert_none_mem hi
        simp [h] at hm
      rcases hp.1 hne with ⟨n, hn⟩
      exact map_addVar_value ρ v k p acc n hn

/-- Exact value on arbitrary maps and offsets with current-path coverage. -/
theorem normalizeAux_value
    (ρ : Valuation) (l : Level) (p : List Name)
    (k : Nat) (acc : NormLevel)
    (hl : NoMVars l) (hp : PathPrimed p acc) :
    mapValue ρ (Normalize.normalizeAux l p k acc) =
      max (mapValue ρ acc)
        (if enabled ρ p then levelValue ρ l + k else 0) := by
  cases hlevel : l with
  | zero =>
    simp only [hlevel, NoMVars] at hl
    rw [Normalize.normalizeAux, map_addConst_value_of_primed ρ acc k p hp]
    simp [levelValue]
  | succ u =>
    simp only [hlevel, NoMVars] at hl
    rw [Normalize.normalizeAux,
      normalizeAux_value ρ u p (k + 1) acc hl hp]
    simp [levelValue, Nat.add_assoc, Nat.add_comm]
  | max u v =>
    simp only [hlevel, NoMVars] at hl
    rw [Normalize.normalizeAux]
    rw [normalizeAux_value ρ v p k _ hl.2
      (normalizeAux_preserves_pathPrimed u p p k acc hp)]
    rw [normalizeAux_value ρ u p k acc hl.1 hp]
    cases enabled ρ p <;>
      simp only [levelValue, max_add_offset, Bool.false_eq_true, ↓reduceIte] <;> omega
  | param v =>
    simp only [hlevel, NoMVars] at hl
    exact normalizeAux_param_value ρ v p k acc hp
  | mvar m =>
    simp only [hlevel, NoMVars] at hl
  | imax u v =>
    simp only [hlevel, NoMVars] at hl
    cases hright : v with
    | zero =>
      simp only [hright, NoMVars] at hl
      rw [Normalize.normalizeAux, map_addConst_value_of_primed ρ acc k p hp]
      simp [levelValue, nat_imax_zero]
    | succ v =>
      simp only [hright, NoMVars] at hl
      rw [Normalize.normalizeAux]
      rw [normalizeAux_value ρ v p (k + 1) _ hl.2
        (normalizeAux_preserves_pathPrimed u p p k acc hp)]
      rw [normalizeAux_value ρ u p k acc hl.1 hp]
      cases enabled ρ p <;>
        simp only [levelValue, nat_imax_succ, max_add_offset, Bool.false_eq_true,
          ↓reduceIte] <;> omega
    | max v w =>
      simp only [hright, NoMVars] at hl
      rw [Normalize.normalizeAux]
      rw [normalizeAux_value ρ (.imax u w) p k _ ⟨hl.1, hl.2.2⟩
        (normalizeAux_preserves_pathPrimed (.imax u v) p p k acc hp)]
      rw [normalizeAux_value ρ (.imax u v) p k acc ⟨hl.1, hl.2.1⟩ hp]
      cases enabled ρ p <;>
        simp only [levelValue, nat_imax_max, max_add_offset, Bool.false_eq_true,
          ↓reduceIte] <;> omega
    | imax v w =>
      simp only [hright, NoMVars] at hl
      rw [Normalize.normalizeAux]
      rw [normalizeAux_value ρ (.imax v w) p k _ ⟨hl.2.1, hl.2.2⟩
        (normalizeAux_preserves_pathPrimed (.imax u w) p p k acc hp)]
      rw [normalizeAux_value ρ (.imax u w) p k acc ⟨hl.1, hl.2.2⟩ hp]
      cases enabled ρ p <;>
        simp only [levelValue, nat_imax_imax, max_add_offset, Bool.false_eq_true,
          ↓reduceIte] <;> omega
    | mvar m =>
      simp only [hright, NoMVars] at hl
      exact False.elim hl.2
    | param v =>
      simp only [hright, NoMVars] at hl
      rw [Normalize.normalizeAux]
      cases hi : Normalize.orderedInsert Name.cmp v p with
      | some p' =>
        have hp' := pathPrimed_extend (k := k) (pathPrimed_addConst hp k p) hi
        rw [normalizeAux_value ρ u p' k _ hl.1 hp']
        rw [map_addNode_value, map_addConst_value_of_primed ρ acc k p hp,
          orderedInsert_enabled ρ hi]
        cases he : enabled ρ p <;> by_cases hv : 0 < ρ v <;>
          simp [levelValue, nat_imax_add_offset, hv] <;> omega
      | none =>
        have hstep : PathPrimed p (if k = 0 then acc else acc.addVar v k p) := by
          split
          · exact hp
          · exact pathPrimed_addVar hp v k p
        rw [normalizeAux_value ρ u p k _ hl.1 hstep]
        have hm := normalizeAux_param_value ρ v p k acc hp
        rw [Normalize.normalizeAux] at hm
        simp only [hi] at hm
        rw [hm]
        cases he : enabled ρ p
        · simp
        · have hv := orderedInsert_none_positive hi he
          simp [levelValue, nat_imax_add_offset, hv]
          omega
termination_by levelSize l
decreasing_by
  all_goals try simp_wf
  all_goals simp_all only [levelSize]
  all_goals omega

/-- Empty construction has no retained contribution and its path is enabled. -/
theorem normalizeAux_empty_value (ρ : Valuation) (l : Level)
    (hl : NoMVars l) :
    mapValue ρ (Normalize.normalizeAux l [] 0 {}) = levelValue ρ l := by
  rw [normalizeAux_value ρ l [] 0 {} hl (pathPrimed_empty {})]
  simp [mapValue_empty, enabled]

/-- A genuine fresh imax-over-max rewrite, with the two activations enabled. -/
theorem normalizeAux_imax_max_witness :
    let a : Level := .param (.num .anonymous 0)
    let b : Level := .param (.num .anonymous 1)
    let c : Level := .param (.num .anonymous 2)
    mapValue (fun _ => 2) (Normalize.normalizeAux (.imax a (.max b c)) [] 3 {}) = 5 := by
  dsimp only
  rw [normalizeAux_value _ _ _ _ _ (by trivial) (pathPrimed_empty {})]
  rfl

/-- A genuine fresh nested-imax rewrite, with a nonzero offset. -/
theorem normalizeAux_imax_imax_witness :
    let a : Level := .param (.num .anonymous 0)
    let b : Level := .param (.num .anonymous 1)
    let c : Level := .param (.num .anonymous 2)
    mapValue (fun _ => 2) (Normalize.normalizeAux (.imax a (.imax b c)) [] 4 {}) = 6 := by
  dsimp only
  rw [normalizeAux_value _ _ _ _ _ (by trivial) (pathPrimed_empty {})]
  rfl

/-- A repeated current-path parameter at zero offset uses existing coverage. -/
theorem normalizeAux_member_zero_witness :
    let v : Name := .num .anonymous 0
    let s : NormLevel := ({} : NormLevel).addNode v 0 [v]
    Normalize.orderedInsert Name.cmp v [v] = none ∧
      PathPrimed [v] s ∧
      mapValue (fun _ => 2) (Normalize.normalizeAux (.param v) [v] 0 s) = 2 := by
  dsimp only
  have hp := pathPrimed_extend (k := 0) (pathPrimed_empty ({} : NormLevel))
    (show Normalize.orderedInsert Name.cmp (.num .anonymous 0) [] =
      some [.num .anonymous 0] from rfl)
  refine ⟨rfl, hp, ?_⟩
  rw [normalizeAux_value _ _ _ _ _ (by trivial) hp]
  rfl

/-- Positive repeated-parameter offset executes modify on a present current row. -/
theorem normalizeAux_member_positive_witness :
    let v : Name := .num .anonymous 0
    let s : NormLevel := ({} : NormLevel).addNode v 0 [v]
    Normalize.orderedInsert Name.cmp v [v] = none ∧
      PathPrimed [v] s ∧
      mapValue (fun _ => 2) (Normalize.normalizeAux (.param v) [v] 3 s) = 5 := by
  dsimp only
  have hp := pathPrimed_extend (k := 0) (pathPrimed_empty ({} : NormLevel))
    (show Normalize.orderedInsert Name.cmp (.num .anonymous 0) [] =
      some [.num .anonymous 0] from rfl)
  refine ⟨rfl, hp, ?_⟩
  rw [normalizeAux_value _ _ _ _ _ (by trivial) hp]
  rfl

/-- Zero-valued activation suppresses even a positive offset under imax. -/
theorem normalizeAux_disabled_witness :
    let v : Name := .num .anonymous 0
    mapValue (fun _ => 0)
      (Normalize.normalizeAux (.imax (.succ .zero) (.param v)) [] 0 {}) = 0 := by
  dsimp only
  rw [normalizeAux_empty_value _ _ (by trivial)]
  rfl

/-- Every arbitrary old map contributes to the actual constructed maximum. -/
theorem normalizeAux_retains_old_witness (ρ : Valuation) (acc : NormLevel) :
    mapValue ρ acc ≤ mapValue ρ
      (Normalize.normalizeAux (.imax (.param (.num .anonymous 0))
        (.param (.num .anonymous 1))) [] 2 acc) := by
  rw [normalizeAux_value _ _ _ _ _ (by trivial) (pathPrimed_empty acc)]
  exact Nat.le_max_left _ _

/-- Coverage does not require a sorted or duplicate-free current key. -/
theorem normalizeAux_primed_duplicate_witness :
    let lo : Name := .num .anonymous 0
    let hi : Name := .num .anonymous 1
    let p : List Name := [hi, lo, lo]
    let s : NormLevel := ({} : NormLevel).insert p { var := [⟨hi, 0⟩, ⟨lo, 0⟩] }
    PathPrimed p s ∧ ¬p.Nodup ∧
      Normalize.orderedInsert Name.cmp lo p = some [lo, hi, lo, lo] ∧
      mapValue (fun _ => 2) (Normalize.normalizeAux (.param lo) p 3 s) = 5 := by
  dsimp only
  have hp : PathPrimed [.num .anonymous 1, .num .anonymous 0, .num .anonymous 0]
      (({} : NormLevel).insert [.num .anonymous 1, .num .anonymous 0, .num .anonymous 0]
        { var := [⟨.num .anonymous 1, 0⟩, ⟨.num .anonymous 0, 0⟩] }) := by
    constructor
    · intro _
      exact ⟨_, rfl⟩
    · intro v hv
      have hv' : v = .num .anonymous 1 ∨ v = .num .anonymous 0 := by
        simpa using hv
      rcases hv' with rfl | rfl
      · exact ⟨[.num .anonymous 1, .num .anonymous 0, .num .anonymous 0],
          { var := [⟨.num .anonymous 1, 0⟩, ⟨.num .anonymous 0, 0⟩] },
          ⟨.num .anonymous 1, 0⟩, rfl, fun _ ha => ha, by simp, rfl⟩
      · exact ⟨[.num .anonymous 1, .num .anonymous 0, .num .anonymous 0],
          { var := [⟨.num .anonymous 1, 0⟩, ⟨.num .anonymous 0, 0⟩] },
          ⟨.num .anonymous 0, 0⟩, rfl, fun _ ha => ha, by simp, rfl⟩
  refine ⟨hp, by simp, rfl, ?_⟩
  rw [normalizeAux_value _ _ _ _ _ (by trivial) hp]
  rfl

/-- Metavariables must be excluded structurally: succ mvar is erased by construction. -/
theorem normalizeAux_succ_mvar_counterexample (m : LMVarId) (ρ : Valuation) :
    mapValue ρ (Normalize.normalizeAux (.succ (.mvar m)) [] 0 {}) = 0 ∧
      levelValue ρ (.succ (.mvar m)) = 1 ∧ ¬NoMVars (.succ (.mvar m)) := by
  constructor
  · rw [Normalize.normalizeAux, Normalize.normalizeAux]
    exact mapValue_empty ρ
  · exact ⟨rfl, fun h => h⟩

end Vesper.Lean4LeanLevel42
