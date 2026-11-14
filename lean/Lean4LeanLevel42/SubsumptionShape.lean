import Lean4LeanLevel42.Shape

/-! Shape of actual pruning and the fixed-source outer subsumption fold.
The no-empty-row result applies to every raw map, independently of WF. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level Lean.Level.Normalize

/-- Actual variable pruning retains a sublist, even when either input is raw. -/
theorem subsumeVars_shape_sublist (xs ys : List VarNode) :
    (Normalize.subsumeVars xs ys).Sublist xs :=
  subsumeVars_sublist xs ys

theorem subsumeVars_sorted (xs ys : List VarNode) (hx : VarsSorted xs) :
    VarsSorted (Normalize.subsumeVars xs ys) := by
  exact List.Pairwise.sublist (subsumeVars_shape_sublist xs ys) hx

theorem node_subsumeBy_varsSorted (same : Bool) (n1 n2 : Node)
    (h : VarsSorted n1.var) : VarsSorted (n1.subsumeBy same n2).var := by
  exact List.Pairwise.sublist (node_subsumeBy_var_sublist same n1 n2) h

theorem node_subsume_varsSorted (p1 p2 : List Name) (n1 n2 : Node)
    (h : VarsSorted n1.var) : VarsSorted (Node.subsume p1 n1 p2 n2).var := by
  exact List.Pairwise.sublist (node_subsume_var_sublist p1 p2 n1 n2) h

theorem minimize_varsSorted (acc : NormLevel) (p : List Name) (original : Node)
    (h : VarsSorted original.var) : VarsSorted (acc.minimize p original).var := by
  exact List.Pairwise.sublist (minimize_var_sublist acc p original) h

theorem minimizeEntry_keysSorted (acc : NormLevel) (p : List Name) (original : Node)
    (ha : MapKeysSorted acc) (hp : KeySorted p) :
    MapKeysSorted (minimizeEntry acc p original) := by
  unfold minimizeEntry
  dsimp only
  split
  · intro q n hget
    rw [map_get_erase] at hget
    split at hget
    · contradiction
    · exact ha q n hget
  · intro q n hget
    rw [map_get_insert] at hget
    split at hget
    next he => subst q; exact hp
    next => exact ha q n hget

theorem minimizeEntry_varsSorted (acc : NormLevel) (p : List Name) (original : Node)
    (ha : MapVarsSorted acc) (hn : VarsSorted original.var) :
    MapVarsSorted (minimizeEntry acc p original) := by
  unfold minimizeEntry
  dsimp only
  split
  · intro q n hget
    rw [map_get_erase] at hget
    split at hget
    · contradiction
    · exact ha q n hget
  · intro q n hget
    rw [map_get_insert] at hget
    split at hget
    next he =>
      subst q
      cases hget
      exact minimize_varsSorted acc p original hn
    next => exact ha q n hget

theorem subsumption_fold_keysSorted (rows : List (List Name × Node)) (acc : NormLevel)
    (ha : MapKeysSorted acc) (hr : ∀ row ∈ rows, KeySorted row.1) :
    MapKeysSorted (rows.foldl (fun s row => minimizeEntry s row.1 row.2) acc) := by
  induction rows generalizing acc with
  | nil => exact ha
  | cons row rows ih =>
    apply ih _ (minimizeEntry_keysSorted acc row.1 row.2 ha (hr row (by simp)))
    intro other ho
    exact hr other (by simp [ho])

theorem subsumption_fold_varsSorted (rows : List (List Name × Node)) (acc : NormLevel)
    (ha : MapVarsSorted acc) (hr : ∀ row ∈ rows, VarsSorted row.2.var) :
    MapVarsSorted (rows.foldl (fun s row => minimizeEntry s row.1 row.2) acc) := by
  induction rows generalizing acc with
  | nil => exact ha
  | cons row rows ih =>
    apply ih _ (minimizeEntry_varsSorted acc row.1 row.2 ha (hr row (by simp)))
    intro other ho
    exact hr other (by simp [ho])

theorem subsumption_keysSorted (s : NormLevel) (h : MapKeysSorted s) :
    MapKeysSorted s.subsumption := by
  unfold NormLevel.subsumption
  rw [Std.TreeMap.foldl_eq_foldl_toList]
  apply subsumption_fold_keysSorted s.toList s h
  intro row hr
  exact h row.1 row.2 ((Std.TreeMap.mem_toList_iff_getElem?_eq_some
    (cmp := List.compareLex Name.cmp)).mp hr)

theorem subsumption_varsSorted (s : NormLevel) (h : MapVarsSorted s) :
    MapVarsSorted s.subsumption := by
  unfold NormLevel.subsumption
  rw [Std.TreeMap.foldl_eq_foldl_toList]
  apply subsumption_fold_varsSorted s.toList s h
  intro row hr
  exact h row.1 row.2 ((Std.TreeMap.mem_toList_iff_getElem?_eq_some
    (cmp := List.compareLex Name.cmp)).mp hr)

/-- If the visited key remains stored, the actual source test found it nonempty. -/
theorem minimizeEntry_current_noEmpty (acc : NormLevel) (p : List Name)
    (original n : Node) (hget : (minimizeEntry acc p original).get? p = some n) :
    n.isEmpty = false := by
  unfold minimizeEntry at hget
  dsimp only at hget
  split at hget
  · rw [map_get_erase, if_pos rfl] at hget
    contradiction
  next he =>
    rw [map_get_insert, if_pos rfl] at hget
    cases hget
    exact eq_false_of_ne_true he

/-- Every possibly empty current row has a key still awaiting a source visit.
No distinctness, original-row lookup, sorting, WF, or nonempty input is needed. -/
theorem subsumption_fold_noEmpty (rows : List (List Name × Node)) (acc : NormLevel)
    (hcovered : ∀ p n, acc.get? p = some n →
      n.isEmpty = false ∨ p ∈ rows.map Prod.fst) :
    MapNoEmpty (rows.foldl (fun s row => minimizeEntry s row.1 row.2) acc) := by
  induction rows generalizing acc with
  | nil =>
    intro p n hget
    rcases hcovered p n hget with hn | hk
    · exact hn
    · simp at hk
  | cons row rows ih =>
    apply ih (minimizeEntry acc row.1 row.2)
    intro p n hget
    by_cases he : p = row.1
    · subst p
      exact Or.inl (minimizeEntry_current_noEmpty acc row.1 row.2 n hget)
    · rw [minimizeEntry_get_other acc row.1 p row.2 he] at hget
      rcases hcovered p n hget with hn | hk
      · exact Or.inl hn
      · right
        simp only [List.map_cons, List.mem_cons] at hk
        exact hk.resolve_left he

theorem subsumption_noEmpty (s : NormLevel) : MapNoEmpty s.subsumption := by
  unfold NormLevel.subsumption
  rw [Std.TreeMap.foldl_eq_foldl_toList]
  apply subsumption_fold_noEmpty s.toList s
  intro p n hget
  right
  exact List.mem_map.mpr ⟨(p, n), (Std.TreeMap.mem_toList_iff_getElem?_eq_some
    (cmp := List.compareLex Name.cmp)).mpr hget, rfl⟩

end Vesper.Lean4LeanLevel42
