import Lean4LeanLevel42.Completion
import Lean4LeanLevel42.MapSubsumption

/-! Actual variable pruning retains a same-name witness at a subset key.
Certificate transport additionally tracks the origin of every final key. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level Lean.Level.Normalize

theorem subsumeVars_var_support (xs ys : List VarNode) (v : VarNode) (hv : v ∈ xs) :
    v ∈ Normalize.subsumeVars xs ys ∨
      ∃ w, w ∈ ys ∧ w.var = v.var ∧ v.offset ≤ w.offset := by
  cases xs with
  | nil => simp at hv
  | cons x xs =>
    cases ys with
    | nil => exact Or.inl (by simpa [Normalize.subsumeVars] using hv)
    | cons y ys =>
      cases hc : Name.cmp x.var y.var with
      | lt =>
        simp only [Normalize.subsumeVars, hc]
        rcases List.mem_cons.mp hv with rfl | hv
        · exact Or.inl (by simp)
        · rcases subsumeVars_var_support xs (y :: ys) v hv with hk | hd
          · exact Or.inl (List.mem_cons_of_mem x hk)
          · exact Or.inr hd
      | eq =>
        have hn := name_cmp_eq x.var y.var hc
        by_cases ho : x.offset ≤ y.offset
        · simp only [Normalize.subsumeVars, hc, if_pos ho]
          rcases List.mem_cons.mp hv with rfl | hv
          · exact Or.inr ⟨y, List.mem_cons_self, hn.symm, ho⟩
          · rcases subsumeVars_var_support xs ys v hv with hk | ⟨w, hw, he, hb⟩
            · exact Or.inl hk
            · exact Or.inr ⟨w, List.mem_cons_of_mem y hw, he, hb⟩
        · simp only [Normalize.subsumeVars, hc, if_neg ho]
          rcases List.mem_cons.mp hv with rfl | hv
          · exact Or.inl (by simp)
          · rcases subsumeVars_var_support xs ys v hv with hk | ⟨w, hw, he, hb⟩
            · exact Or.inl (List.mem_cons_of_mem x hk)
            · exact Or.inr ⟨w, List.mem_cons_of_mem y hw, he, hb⟩
      | gt =>
        simp only [Normalize.subsumeVars, hc]
        rcases subsumeVars_var_support (x :: xs) ys v hv with hk | ⟨w, hw, he, hb⟩
        · exact Or.inl hk
        · exact Or.inr ⟨w, List.mem_cons_of_mem y hw, he, hb⟩
termination_by xs.length + ys.length
decreasing_by all_goals (simp_all <;> omega)

theorem node_subsumeBy_var_support (same : Bool) (n m : Node) (v : VarNode)
    (hv : v ∈ n.var) :
    v ∈ (n.subsumeBy same m).var ∨
      ∃ w, w ∈ m.var ∧ w.var = v.var ∧ v.offset ≤ w.offset := by
  unfold Node.subsumeBy
  split <;> split
  · exact Or.inl hv
  · exact subsumeVars_var_support n.var m.var v hv
  · exact Or.inl hv
  · exact subsumeVars_var_support n.var m.var v hv

theorem node_subsume_var_support (p q : List Name) (n m : Node) (v : VarNode)
    (hv : v ∈ n.var) :
    v ∈ (Node.subsume p n q m).var ∨
      (q ⊆ p ∧ ∃ w, w ∈ m.var ∧ w.var = v.var ∧ v.offset ≤ w.offset) := by
  unfold Node.subsume
  split
  next hs =>
    change Normalize.subset Name.cmp q p = true at hs
    rcases node_subsumeBy_var_support (p.length == q.length) n m v hv with hk | hd
    · exact Or.inl hk
    · exact Or.inr ⟨subset_sound hs, hd⟩
  next => exact Or.inl hv

/-- A self comparison retains variables; every other supporting row survives erase p. -/
theorem minimize_fold_var_support (acc : NormLevel) (p : List Name)
    (rows : List (List Name × Node))
    (hrows : ∀ row ∈ rows, acc.get? row.1 = some row.2)
    (residual : Node) (v : VarNode) (hv : v ∈ residual.var) :
    v ∈ (rows.foldl (fun n row => Node.subsume p n row.1 row.2) residual).var ∨
      ∃ q m w, (acc.erase p).get? q = some m ∧ w ∈ m.var ∧
        w.var = v.var ∧ v.offset ≤ w.offset ∧ q ⊆ p := by
  induction rows generalizing residual with
  | nil => exact Or.inl hv
  | cons row rows ih =>
    have hrest : ∀ other ∈ rows, acc.get? other.1 = some other.2 := by
      intro other ho
      exact hrows other (by simp [ho])
    by_cases he : p = row.1
    · have hk : v ∈ (Node.subsume p residual row.1 row.2).var := by
        rw [← he, minimize_self_var]
        exact hv
      exact ih hrest _ hk
    · rcases node_subsume_var_support p row.1 residual row.2 v hv with hk | hd
      · exact ih hrest _ hk
      · rcases hd with ⟨hsub, w, hw, hname, hbound⟩
        right
        refine ⟨row.1, row.2, w, ?_, hw, hname, hbound, hsub⟩
        rw [map_get_erase, if_neg he]
        exact hrows row (by simp)

theorem minimize_vars_erased_support (acc : NormLevel) (p : List Name)
    (original : Node) (v : VarNode) (hv : v ∈ original.var) :
    v ∈ (acc.minimize p original).var ∨
      ∃ q m w, (acc.erase p).get? q = some m ∧ w ∈ m.var ∧
        w.var = v.var ∧ v.offset ≤ w.offset ∧ q ⊆ p := by
  unfold NormLevel.minimize
  rw [Std.TreeMap.foldl_eq_foldl_toList]
  exact minimize_fold_var_support acc p acc.toList
    (fun row hr => (Std.TreeMap.mem_toList_iff_getElem?_eq_some
      (cmp := List.compareLex Name.cmp)).mp hr) original v hv

theorem node_var_mem_noEmpty (n : Node) (v : VarNode) (hv : v ∈ n.var) :
    n.isEmpty = false := by
  cases n with
  | mk c vs =>
    cases vs with
    | nil => simp at hv
    | cons w ws => simp [Node.isEmpty]

/-- The actual original-entry lookup is essential for support of the whole map. -/
theorem minimizeEntry_varSupport (acc : NormLevel) (p : List Name) (original : Node)
    (hget : acc.get? p = some original) : VarSupport acc (minimizeEntry acc p original) := by
  intro q n a k hq hv
  by_cases he : q = p
  · subst q
    have hn : n = original := Option.some.inj (hq.symm.trans hget)
    subst n
    rcases minimize_vars_erased_support acc p original ⟨a, k⟩ hv with hk | hd
    · have hne := node_var_mem_noEmpty (acc.minimize p original) ⟨a, k⟩ hk
      refine ⟨p, acc.minimize p original, k, ?_, hk, fun _ h => h⟩
      unfold minimizeEntry
      dsimp only
      rw [hne]
      simp only [Bool.false_eq_true, ↓reduceIte]
      rw [map_get_insert, if_pos rfl]
    · rcases hd with ⟨q, m, ⟨b, j⟩, hq, hw, hname, _, hsub⟩
      have hb : b = a := hname
      subst b
      rw [map_get_erase] at hq
      split at hq
      · contradiction
      next hne =>
        refine ⟨q, m, j, ?_, hw, hsub⟩
        rw [minimizeEntry_get_other acc p q original (Ne.symm hne)]
        exact hq
  · refine ⟨q, n, k, ?_, hv, fun _ h => h⟩
    rw [minimizeEntry_get_other acc p q original he]
    exact hq

theorem minimizeEntry_key_origin (acc : NormLevel) (p : List Name) (original : Node)
    (hget : acc.get? p = some original) :
    ∀ q n, (minimizeEntry acc p original).get? q = some n →
      ∃ m, acc.get? q = some m := by
  intro q n hq
  by_cases he : q = p
  · subst q
    exact ⟨original, hget⟩
  · rw [minimizeEntry_get_other acc p q original he] at hq
    exact ⟨n, hq⟩

/-- Remaining original rows retain their actual current lookups after each visit. -/
theorem subsumption_fold_varSupport (rows : List (List Name × Node)) (acc : NormLevel)
    (hd : rows.Pairwise (fun a b => a.1 ≠ b.1))
    (hget : ∀ row ∈ rows, acc.get? row.1 = some row.2) :
    VarSupport acc (rows.foldl (fun s row => minimizeEntry s row.1 row.2) acc) ∧
      (∀ p n, (rows.foldl (fun s row => minimizeEntry s row.1 row.2) acc).get? p = some n →
        ∃ m, acc.get? p = some m) := by
  induction rows generalizing acc with
  | nil => exact ⟨varSupport_refl acc, fun _ n hn => ⟨n, hn⟩⟩
  | cons row rows ih =>
    rcases List.pairwise_cons.mp hd with ⟨hne, htail⟩
    have hrow := hget row (by simp)
    have hremaining : ∀ other ∈ rows,
        (minimizeEntry acc row.1 row.2).get? other.1 = some other.2 := by
      intro other ho
      rw [minimizeEntry_get_other acc row.1 other.1 row.2 (Ne.symm (hne other ho))]
      exact hget other (by simp [ho])
    obtain ⟨hs, ho⟩ := ih _ htail hremaining
    refine ⟨varSupport_trans acc _ _ (minimizeEntry_varSupport acc row.1 row.2 hrow) hs, ?_⟩
    intro p n hp
    obtain ⟨m, hm⟩ := ho p n hp
    exact minimizeEntry_key_origin acc row.1 row.2 hrow p m hm

theorem subsumption_varSupport (s : NormLevel) : VarSupport s s.subsumption := by
  unfold NormLevel.subsumption
  rw [Std.TreeMap.foldl_eq_foldl_toList]
  exact (subsumption_fold_varSupport s.toList s (map_toList_distinct_keys s)
    (fun row hr => (Std.TreeMap.mem_toList_iff_getElem?_eq_some
      (cmp := List.compareLex Name.cmp)).mp hr)).1

theorem subsumption_key_origin (s : NormLevel) :
    ∀ p n, s.subsumption.get? p = some n → ∃ m, s.get? p = some m := by
  unfold NormLevel.subsumption
  rw [Std.TreeMap.foldl_eq_foldl_toList]
  exact (subsumption_fold_varSupport s.toList s (map_toList_distinct_keys s)
    (fun row hr => (Std.TreeMap.mem_toList_iff_getElem?_eq_some
      (cmp := List.compareLex Name.cmp)).mp hr)).2

theorem subsumption_mapCompletes (s : NormLevel) (h : MapCompletes s) :
    MapCompletes s.subsumption := by
  exact mapCompletes_support s s.subsumption (subsumption_varSupport s)
    (subsumption_key_origin s) h

end Vesper.Lean4LeanLevel42
