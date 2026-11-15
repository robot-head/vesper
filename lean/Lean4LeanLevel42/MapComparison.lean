import Lean4LeanLevel42.MapAll
import Lean4LeanLevel42.CompareFold
import Lean4LeanLevel42.NormalizeFinal

/-! Soundness of the unchanged map comparison and normalized level comparison.
Only the right map needs well-formed stored rows. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level Lean.Level.Normalize

theorem norm_le_sound (ρ : Valuation) (left right : NormLevel)
    (hw : MapWF right) (h : left.le right = true) :
    mapValue ρ left ≤ mapValue ρ right := by
  apply mapValue_le
  intro p n hn
  unfold NormLevel.le at h
  have hrow := norm_all_lookup h hn
  change (right.foldlM (fun r q m => compareStep p r q m) n).isNone = true at hrow
  have hnone := Option.isNone_iff_eq_none.mp hrow
  rw [Std.TreeMap.foldlM_eq_foldlM_toList] at hnone
  apply compareFold_none_bound ρ p right.toList n (mapValue ρ right) _ _ hnone
  · intro row hmem
    exact hw row.1 row.2 ((Std.TreeMap.mem_toList_iff_getElem?_eq_some
      (cmp := List.compareLex Name.cmp)).mp hmem)
  · intro row hmem
    exact nodeValue_le_mapValue ((Std.TreeMap.mem_toList_iff_getElem?_eq_some
      (cmp := List.compareLex Name.cmp)).mp hmem)

theorem geq_sound (ρ : Valuation) (u v : Level) (hu : NoMVars u) (hv : NoMVars v)
    (h : Lean.Level.geq' u v = true) : levelValue ρ v ≤ levelValue ρ u := by
  have hm := norm_le_sound ρ (Normalize.normalize v) (Normalize.normalize u)
    (normalize_WF u) h
  rw [normalize_value ρ v hv, normalize_value ρ u hu] at hm
  exact hm

/-- Insertion preserves row WF when the replacement itself is well formed. -/
theorem comparison_insert_WF (s : NormLevel) (p : List Name) (n : Node)
    (hs : MapWF s) (hn : NodeWF p n) : MapWF (s.insert p n) := by
  intro q m hq
  rw [map_get_insert] at hq
  split at hq
  next he =>
    subst q
    cases hq
    exact hn
  next => exact hs q m hq

theorem comparison_singleton_toList (p : List Name) (n : Node) :
    (({} : NormLevel).insert p n).toList = [(p, n)] := by
  rfl

theorem comparison_pair_toList (u : Name) (c v : Node) :
    ((({} : NormLevel).insert [] c).insert [u] v).toList = [([], c), ([u], v)] := by
  rfl

theorem comparison_insert_existing_toList (s : NormLevel) (p : List Name) (n : Node)
    (hget : s.get? p = some n) : (s.insert p n).toList = s.toList := by
  apply Std.TreeMap.Equiv.toList_eq (cmp := List.compareLex Name.cmp)
  apply Std.TreeMap.Equiv.of_forall_constGet?_eq (cmp := List.compareLex Name.cmp)
  intro q
  change (s.insert p n).get? q = s.get? q
  rw [map_get_insert]
  by_cases he : p = q
  · subst q
    simp only [ite_true, hget]
  · rw [if_neg he]

theorem norm_le_eq_toList_compare (left right : NormLevel) :
    left.le right = left.toList.all (fun row =>
      (right.toList.foldlM (fun n other => compareStep row.1 n other.1 other.2)
        row.2).isNone) := by
  unfold NormLevel.le
  rw [norm_all_eq_toList_all]
  simp only [Std.TreeMap.foldlM_eq_foldlM_toList]
  rfl

/-- A zero-valued variable under the empty key unsoundly discharges one. -/
theorem norm_le_needs_right_WF_counterexample :
    let u : Name := .num .anonymous 0
    let left : NormLevel := ({} : NormLevel).insert [] { const := 1 }
    let right : NormLevel := ({} : NormLevel).insert [] { var := [⟨u, 0⟩] }
    let ρ : Valuation := fun _ => 0
    MapWF left ∧ ¬MapWF right ∧ left.le right = true ∧
      mapValue ρ left = 1 ∧ mapValue ρ right = 0 := by
  dsimp only
  refine ⟨comparison_insert_WF _ _ _ mapWF_empty (by simp [NodeWF]), ?_,
    ?_, rfl, rfl⟩
  · intro hw
    have hn := hw [] { var := [⟨.num .anonymous 0, 0⟩] }
      (by rw [map_get_insert]; rfl)
    have hb := hn ⟨.num .anonymous 0, 0⟩ (by simp)
    simp at hb
  · rw [norm_le_eq_toList_compare, comparison_singleton_toList,
      comparison_singleton_toList]
    simp [compareStep, subset, Node.subsumeBy, Node.isEmpty, subsumeVars]

/-- Neither right row alone empties the source node; together they do. -/
theorem norm_le_split_dominators :
    let u : Name := .num .anonymous 0
    let n : Node := { const := 2, var := [⟨u, 0⟩] }
    let c : Node := { const := 2 }
    let v : Node := { var := [⟨u, 0⟩] }
    let left : NormLevel := ({} : NormLevel).insert [u] n
    let right : NormLevel := (({} : NormLevel).insert [] c).insert [u] v
    MapWF left ∧ MapWF right ∧ left.le right = true ∧
      compareStep [u] n [] c = some v ∧
      compareStep [u] n [u] v = some c ∧
      compareStep [u] v [u] v = none := by
  dsimp only
  refine ⟨comparison_insert_WF _ _ _ mapWF_empty (by simp [NodeWF]),
    comparison_insert_WF _ _ _
      (comparison_insert_WF _ _ _ mapWF_empty (by simp [NodeWF]))
      (by simp [NodeWF]), ?_, rfl, ?_, ?_⟩
  · rw [norm_le_eq_toList_compare, comparison_singleton_toList, comparison_pair_toList]
    simp [compareStep, subset, name_cmp_refl, Node.subsumeBy, Node.isEmpty, subsumeVars]
  · simp [compareStep, subset, name_cmp_refl, Node.subsumeBy, subsumeVars, Node.isEmpty]
  · simp [compareStep, subset, name_cmp_refl, Node.subsumeBy, subsumeVars, Node.isEmpty]

/-- Explicit empty rows expose incompleteness on arbitrary raw maps. -/
theorem norm_le_raw_empty_row_incomplete :
    let left : NormLevel := ({} : NormLevel).insert [] {}
    let right : NormLevel := {}
    MapWF left ∧ MapWF right ∧ (∀ ρ : Valuation, mapValue ρ left ≤ mapValue ρ right) ∧
      left.le right = false := by
  dsimp only
  exact ⟨comparison_insert_WF _ _ _ mapWF_empty (by simp [NodeWF]),
    mapWF_empty, fun _ => Nat.le_refl 0, rfl⟩

/-- Actual normalized comparison splits imax's constant and variable obligations. -/
theorem geq_split_dominators :
    let u : Level := .param (.num .anonymous 0)
    let two : Level := .succ (.succ .zero)
    let upper : Level := .max two u
    let lower : Level := .imax two u
    NoMVars upper ∧ NoMVars lower ∧ Lean.Level.geq' upper lower = true ∧
      ∀ ρ : Valuation, levelValue ρ lower ≤ levelValue ρ upper := by
  dsimp only
  have hgeq : Lean.Level.geq'
      (.max (.succ (.succ .zero)) (.param (.num .anonymous 0)))
      (.imax (.succ (.succ .zero)) (.param (.num .anonymous 0))) = true := by
    have hlower :
        (Normalize.normalize (.imax (.succ (.succ .zero))
          (.param (.num .anonymous 0)))).toList =
        [([.num .anonymous 0], { const := 2, var := [⟨.num .anonymous 0, 0⟩] })] := by
      simp only [Normalize.normalize, Normalize.normalizeAux, orderedInsert]
      change (NormLevel.subsumption (({} : NormLevel).insert [.num .anonymous 0]
        { const := 2, var := [⟨.num .anonymous 0, 0⟩] })).toList = _
      simp [NormLevel.subsumption, NormLevel.minimize,
        Std.TreeMap.foldl_eq_foldl_toList, comparison_singleton_toList,
        Node.subsume, subset, Node.subsumeBy, Node.isEmpty]
      rw [comparison_insert_existing_toList _ _ _ (by rw [map_get_insert]; rfl),
        comparison_singleton_toList]
    have hupper :
        (Normalize.normalize (.max (.succ (.succ .zero))
          (.param (.num .anonymous 0)))).toList =
        [([], { const := 2 }), ([.num .anonymous 0], { var := [⟨.num .anonymous 0, 0⟩] })] := by
      have hfirst :
          (((( {} : NormLevel).insert [] { const := 2 }).insert [.num .anonymous 0]
            { var := [⟨.num .anonymous 0, 0⟩] }).insert [] { const := 2 }).toList =
          [([], { const := 2 }), ([.num .anonymous 0], { var := [⟨.num .anonymous 0, 0⟩] })] := by
        rw [comparison_insert_existing_toList _ _ _ (by
            rw [map_get_insert, if_neg (by simp), map_get_insert]; rfl),
          comparison_pair_toList]
      have hsecond :
          ((((({} : NormLevel).insert [] { const := 2 }).insert [.num .anonymous 0]
            { var := [⟨.num .anonymous 0, 0⟩] }).insert [] { const := 2 }).insert
              [.num .anonymous 0] { var := [⟨.num .anonymous 0, 0⟩] }).toList =
          [([], { const := 2 }), ([.num .anonymous 0], { var := [⟨.num .anonymous 0, 0⟩] })] := by
        rw [comparison_insert_existing_toList _ _ _ (by
            rw [map_get_insert, if_neg (by simp), map_get_insert]; rfl), hfirst]
      simp only [Normalize.normalize, Normalize.normalizeAux, orderedInsert]
      change (NormLevel.subsumption ((({} : NormLevel).insert [] { const := 2 }).insert
        [.num .anonymous 0] { var := [⟨.num .anonymous 0, 0⟩] })).toList = _
      simp [NormLevel.subsumption, NormLevel.minimize,
        Std.TreeMap.foldl_eq_foldl_toList, comparison_pair_toList,
        hfirst, hsecond, Node.subsume, subset, Node.subsumeBy, Node.isEmpty]
    rw [Lean.Level.geq', norm_le_eq_toList_compare, hlower, hupper]
    simp [compareStep, subset, name_cmp_refl, Node.subsumeBy, Node.isEmpty, subsumeVars]
  refine ⟨by trivial, by trivial, hgeq, ?_⟩
  intro ρ
  exact geq_sound ρ _ _ (by trivial) (by trivial) hgeq

end Vesper.Lean4LeanLevel42
