import Lean4LeanLevel42.MapMinimize

/-! The actual outer subsumption fold preserves denotation and well-formedness.
Its source rows remain fixed while the accumulator changes. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level.Normalize

/-- Exactly the source expression used for one actual outer-fold entry. -/
def minimizeEntry (acc : NormLevel) (p : List Name) (original : Node) : NormLevel :=
  let n := acc.minimize p original
  if n.isEmpty then acc.erase p else acc.insert p n

theorem minimizeEntry_value (ρ : Valuation) (acc : NormLevel)
    (p : List Name) (original : Node)
    (hw : MapWF acc) (hget : acc.get? p = some original) :
    mapValue ρ (minimizeEntry acc p original) = mapValue ρ acc := by
  have hc := minimize_value_context ρ acc p original hw hget
  unfold minimizeEntry
  dsimp only
  split
  next he =>
    rw [node_isEmpty_value_zero ρ p (acc.minimize p original) he] at hc
    simpa using hc
  next =>
    rw [mapValue_insert, Nat.max_comm]
    exact hc

theorem minimizeEntry_WF (acc : NormLevel) (p : List Name) (original : Node)
    (hw : MapWF acc) (hget : acc.get? p = some original) :
    MapWF (minimizeEntry acc p original) := by
  have hn := minimize_WF acc p original (hw p original hget)
  unfold minimizeEntry
  dsimp only
  split
  · intro q n hq
    rw [map_get_erase] at hq
    split at hq
    · contradiction
    · exact hw q n hq
  · intro q n hq
    rw [map_get_insert] at hq
    split at hq
    next he =>
      subst q
      cases hq
      exact hn
    next => exact hw q n hq

theorem minimizeEntry_get_other (acc : NormLevel) (p q : List Name)
    (original : Node) (hne : q ≠ p) :
    (minimizeEntry acc p original).get? q = acc.get? q := by
  unfold minimizeEntry
  dsimp only
  split <;> simp only [map_get_erase, map_get_insert, if_neg (Ne.symm hne)]

/-- Every remaining source row still denotes its original node at its current key.
Distinct keys make the just-visited key irrelevant to this invariant. -/
theorem subsumption_fold_preserves (ρ : Valuation)
    (rows : List (List Name × Node)) (acc : NormLevel)
    (hd : rows.Pairwise (fun a b => a.1 ≠ b.1))
    (hw : MapWF acc)
    (hget : ∀ row ∈ rows, acc.get? row.1 = some row.2) :
    mapValue ρ (rows.foldl (fun s row => minimizeEntry s row.1 row.2) acc) =
        mapValue ρ acc ∧
      MapWF (rows.foldl (fun s row => minimizeEntry s row.1 row.2) acc) := by
  induction rows generalizing acc with
  | nil => exact ⟨rfl, hw⟩
  | cons row rows ih =>
    rcases List.pairwise_cons.mp hd with ⟨hne, htail⟩
    have hrow := hget row (by simp)
    have hstep := minimizeEntry_value ρ acc row.1 row.2 hw hrow
    have hwstep := minimizeEntry_WF acc row.1 row.2 hw hrow
    have hremaining : ∀ other ∈ rows,
        (minimizeEntry acc row.1 row.2).get? other.1 = some other.2 := by
      intro other ho
      rw [minimizeEntry_get_other acc row.1 other.1 row.2 (Ne.symm (hne other ho))]
      exact hget other (by simp [ho])
    obtain ⟨hvalue, hwfinal⟩ := ih _ htail hwstep hremaining
    exact ⟨hvalue.trans hstep, hwfinal⟩

theorem map_toList_distinct_keys (acc : NormLevel) :
    acc.toList.Pairwise (fun a b => a.1 ≠ b.1) := by
  apply (Std.TreeMap.distinct_keys_toList (t := acc)
    (cmp := List.compareLex Name.cmp)).imp
  intro a b hab he
  exact hab ((path_cmp_eq_iff a.1 b.1).mpr he)

theorem subsumption_value (ρ : Valuation) (acc : NormLevel) (hw : MapWF acc) :
    mapValue ρ acc.subsumption = mapValue ρ acc := by
  unfold NormLevel.subsumption
  rw [Std.TreeMap.foldl_eq_foldl_toList]
  exact (subsumption_fold_preserves ρ acc.toList acc (map_toList_distinct_keys acc) hw
    (fun row hrow => (Std.TreeMap.mem_toList_iff_getElem?_eq_some
      (cmp := List.compareLex Name.cmp)).mp hrow)).1

theorem subsumption_WF (acc : NormLevel) (hw : MapWF acc) :
    MapWF acc.subsumption := by
  unfold NormLevel.subsumption
  rw [Std.TreeMap.foldl_eq_foldl_toList]
  exact (subsumption_fold_preserves (fun _ => 0) acc.toList acc
    (map_toList_distinct_keys acc) hw
    (fun row hrow => (Std.TreeMap.mem_toList_iff_getElem?_eq_some
      (cmp := List.compareLex Name.cmp)).mp hrow)).2

/-- The actual singleton outer fold loses a positive constant without map WF. -/
theorem subsumption_needs_WF_counterexample :
    let u : Name := .str .anonymous "u"
    let n : Node := { const := 1, var := [⟨u, 0⟩] }
    let s : NormLevel := ({} : NormLevel).insert [] n
    let ρ : Valuation := fun _ => 0
    ¬MapWF s ∧ mapValue ρ s = 1 ∧ mapValue ρ s.subsumption = 0 := by
  dsimp only
  constructor
  · intro hw
    have hn := hw [] { const := 1, var := [⟨.str .anonymous "u", 0⟩] }
      (by rw [map_get_insert]; rfl)
    have hb := hn ⟨.str .anonymous "u", 0⟩ (by simp)
    simp at hb
  · exact ⟨rfl, rfl⟩

/-- An external, distinct empty key really deletes the dominated nonempty row. -/
theorem subsumption_erases_dominated_row :
    let u : Name := .str .anonymous "u"
    let s : NormLevel := (({} : NormLevel).insert [] { const := 2 }).insert [u]
      { const := 1 }
    MapWF s ∧ s.get? [u] = some { const := 1 } ∧
      s.subsumption.get? [u] = none ∧
      s.subsumption.get? [] = some { const := 2 } ∧
      mapValue (fun _ => 1) s.subsumption = 2 := by
  dsimp only
  constructor
  · intro p n hget
    rw [map_get_insert] at hget
    split at hget
    · cases hget; simp [NodeWF]
    · rw [map_get_insert] at hget
      split at hget
      · cases hget; simp [NodeWF]
      · rw [map_get_empty] at hget; contradiction
  · exact ⟨rfl, rfl, rfl, rfl⟩

end Vesper.Lean4LeanLevel42
