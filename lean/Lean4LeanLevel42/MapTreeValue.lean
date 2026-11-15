import Lean4LeanLevel42.TreePathValue
import Lean4LeanLevel42.LexChainCompletion

/-! Actual chain certificates and the original-map fold for reconstruction.
All valuation bounds come from stored rows and independent completion. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level Lean.Level.Normalize

theorem lexChain_key_injective (s : NormLevel) (p q : List Name)
    (hp : KeySorted p) (hq : KeySorted q)
    (h : s.lexChain p.length p = s.lexChain q.length q) : p = q := by
  have hperm : p.Perm q := (lexChain_perm s p.length p).symm.trans
    (h ▸ lexChain_perm s q.length q)
  apply List.Perm.eq_of_sorted (le := fun a b : Name => Name.cmp a b = .lt)
    (fun a b _ _ hab hba => ?_) hp hq hperm
  have hg := Std.OrientedCmp.gt_of_lt (cmp := Name.cmp) hab
  simp [hba] at hg

theorem enabled_perm (ρ : Valuation) (p q : List Name) (h : p.Perm q) :
    enabled ρ p = enabled ρ q := by
  have he : enabled ρ p = true ↔ enabled ρ q = true := by
    constructor
    · intro hp
      exact enabled_eq_true_iff.mpr
        (fun v hv => enabled_eq_true_iff.mp hp v (h.symm.subset hv))
    · intro hq
      exact enabled_eq_true_iff.mpr
        (fun v hv => enabled_eq_true_iff.mp hq v (h.subset hv))
  cases hp : enabled ρ p <;> cases hq : enabled ρ q <;> simp_all

theorem edgeCovered_enabled_le_mapValue (ρ : Valuation) (s : NormLevel)
    (q : List Name) (a : Name) (h : EdgeCovered s q a) :
    (if enabled ρ q then ρ a else 0) ≤ mapValue ρ s := by
  by_cases hq : enabled ρ q = true
  · simp only [hq, ↓reduceIte]
    by_cases ha : 0 < ρ a
    · obtain ⟨p, n, k, hget, hvar, hsub⟩ := h
      have hp : enabled ρ p = true := by
        apply enabled_eq_true_iff.mpr
        intro v hv
        rcases List.mem_cons.mp (hsub hv) with rfl | hv
        · exact ha
        · exact enabled_eq_true_iff.mp hq v hv
      have hn := nodeValue_le_mapValue (ρ := ρ) hget
      rw [nodeValue_of_enabled hp] at hn
      have hv := varsValue_mem_bound (ρ := ρ) hvar
      change ρ a + k ≤ varsValue ρ n.var at hv
      omega
    · omega
  · simp [eq_false_of_ne_true hq]

theorem admissible_prefix_enabled_bound (ρ : Valuation) (s : NormLevel)
    (r : List Name) (h : Admissible s [] r) :
    ∀ front a rest, r = front ++ a :: rest →
      (if enabled ρ front then ρ a else 0) ≤ mapValue ρ s := by
  intro front a rest he
  subst r
  have hs := (admissible_append_iff s [] front (a :: rest)).mp h
  have hedge : EdgeCovered s front.reverse a := by
    simpa only [List.append_nil] using hs.2.1
  have hb := edgeCovered_enabled_le_mapValue ρ s front.reverse a hedge
  rw [enabled_perm ρ front.reverse front (List.reverse_perm front)] at hb
  exact hb

theorem enabled_subset_gate_le (ρ : Valuation) (p q : List Name) (z : Nat)
    (h : p ⊆ q) :
    (if enabled ρ q then z else 0) ≤ (if enabled ρ p then z else 0) := by
  by_cases hq : enabled ρ q = true
  · have hp := enabled_of_subset h hq
    simp [hq, hp]
  · simp [eq_false_of_ne_true hq]

theorem subsumeVars_zero_node_union (ρ : Valuation) (p : List Name)
    (n : Node) (a : Name) :
    max (nodeValue ρ p { n with var := Normalize.subsumeVars n.var [⟨a, 0⟩] })
        (if enabled ρ p then ρ a else 0) =
      max (nodeValue ρ p n) (if enabled ρ p then ρ a else 0) := by
  by_cases hp : enabled ρ p = true
  · have hv := subsumeVars_union ρ n.var [⟨a, 0⟩]
    simp only [varsValue_cons, varsValue_nil, Nat.add_zero, Nat.max_zero] at hv
    simp only [nodeValue, hp, ↓reduceIte]
    omega
  · simp [nodeValue, eq_false_of_ne_true hp]

theorem map_tree_row_update_bounds (ρ : Valuation) (s : NormLevel)
    (p : List Name) (n : Node) (t : Tree)
    (hKeys : ∀ q m, s.get? q = some m → KeySorted q)
    (hCompletes : MapCompletes s) (hRow : s.get? p = some n)
    (hSorted : TreeChildrenSorted t)
    (hZero : treePayloadAt (s.lexChain p.length p).reverse t = ({} : Node))
    (hOld : levelValue ρ t.reify ≤ mapValue ρ s) :
    let path := s.lexChain p.length p
    let residual : Node := { n with var := if let a :: _ := path then Normalize.subsumeVars n.var [⟨a, 0⟩] else n.var }
    let out := t.modify path (treeWithPayload residual)
    TreeChildrenSorted out ∧
      levelValue ρ t.reify ≤ levelValue ρ out.reify ∧
      nodeValue ρ p n ≤ levelValue ρ out.reify ∧
      levelValue ρ out.reify ≤ mapValue ρ s := by
  let path := s.lexChain p.length p
  let residual : Node := { n with var := if let a :: _ := path then Normalize.subsumeVars n.var [⟨a, 0⟩] else n.var }
  let out := t.modify path (treeWithPayload residual)
  change TreeChildrenSorted out ∧
    levelValue ρ t.reify ≤ levelValue ρ out.reify ∧
    nodeValue ρ p n ≤ levelValue ρ out.reify ∧
    levelValue ρ out.reify ≤ mapValue ρ s
  have hs := tree_modify_payload_sorted t path residual hSorted
  have hm := tree_modify_payload_mono ρ t path residual hSorted hZero
  have hl := tree_modify_payload_lower ρ t path residual hSorted
  have hadm : Admissible s [] path.reverse :=
    lexChain_length_reverse_admissible s p hKeys (hKeys p n hRow) (hCompletes p n hRow)
  have hPerm : path.reverse.Perm p :=
    (List.reverse_perm path).trans (lexChain_perm s p.length p)
  have hgate := enabled_perm ρ path.reverse p hPerm
  have hnode : nodeValue ρ path.reverse n ≤ mapValue ρ s := by
    simpa only [nodeValue, hgate] using nodeValue_le_mapValue (ρ := ρ) hRow
  have hEdges : ∀ front a rest, path.reverse = front ++ a :: rest →
      treePathGate ρ front (ρ a) ≤ mapValue ρ s := by
    exact admissible_prefix_enabled_bound ρ s path.reverse hadm
  have hr : nodeValue ρ p n ≤ levelValue ρ out.reify ∧
      treePathGate ρ path.reverse (max residual.const (varsValue ρ residual.var)) ≤
        mapValue ρ s := by
    cases hpath : path with
    | nil =>
      have hn : residual = n := by simp [residual, hpath]
      constructor
      · have hb : nodeValue ρ path.reverse n ≤ levelValue ρ out.reify := by
          simpa only [out, hn, treePathGate, nodeValue] using hl.1
        simpa only [nodeValue, hgate] using hb
      · simpa only [hn, treePathGate, nodeValue, hpath] using hnode
    | cons a tail =>
      have hn : residual = { n with var := Normalize.subsumeVars n.var [⟨a, 0⟩] } := by
        simp [residual, hpath]
      have hu := subsumeVars_zero_node_union ρ path.reverse n a
      rw [← hn] at hu
      change max (treePathGate ρ path.reverse
          (max residual.const (varsValue ρ residual.var)))
          (treePathGate ρ path.reverse (ρ a)) =
        max (nodeValue ρ path.reverse n) (treePathGate ρ path.reverse (ρ a)) at hu
      have hpref : treePathGate ρ tail.reverse (ρ a) ≤ mapValue ρ s :=
        hEdges tail.reverse a [] (by simp [hpath, List.reverse_cons])
      have hsub : tail.reverse ⊆ path.reverse := by
        rw [hpath, List.reverse_cons]
        intro x hx
        exact List.mem_append.mpr (Or.inl hx)
      have hedge : treePathGate ρ path.reverse (ρ a) ≤ mapValue ρ s :=
        Nat.le_trans (enabled_subset_gate_le ρ tail.reverse path.reverse (ρ a) hsub) hpref
      constructor
      · have hb : max (nodeValue ρ path.reverse n) (treePathGate ρ path.reverse (ρ a)) ≤
            levelValue ρ out.reify := by
          rw [← hu]
          exact Nat.max_le.mpr ⟨hl.1, hl.2 a tail hpath⟩
        have hv := Nat.le_trans (Nat.le_max_left _ _) hb
        simpa only [nodeValue, hgate] using hv
      · have hb := Nat.max_le.mpr ⟨hnode, hedge⟩
        rw [← hu] at hb
        have hh : treePathGate ρ path.reverse
            (max residual.const (varsValue ρ residual.var)) ≤ mapValue ρ s :=
          Nat.le_trans (Nat.le_max_left _ _) hb
        simpa only [hpath] using hh
  exact ⟨hs, hm, hr.1,
    tree_modify_payload_upper ρ t path residual (mapValue ρ s) hSorted hOld hr.2 hEdges⟩

theorem map_tree_fold_value_bounds (ρ : Valuation) (s : NormLevel)
    (rows : List (List Name × Node)) (t : Tree) (z : Nat)
    (hKeys : ∀ p n, s.get? p = some n → KeySorted p)
    (hCompletes : MapCompletes s)
    (hDistinct : rows.Pairwise (fun a b => a.1 ≠ b.1))
    (hRows : ∀ row ∈ rows, s.get? row.1 = some row.2)
    (hSorted : TreeChildrenSorted t)
    (hZero : ∀ row ∈ rows,
      treePayloadAt (s.lexChain row.1.length row.1).reverse t = ({} : Node))
    (hLower : z ≤ levelValue ρ t.reify)
    (hUpper : levelValue ρ t.reify ≤ mapValue ρ s) :
    let out := rows.foldl (fun (t : Tree) row =>
      let path := s.lexChain row.1.length row.1
      let residual : Node := { row.2 with var := if let a :: _ := path then Normalize.subsumeVars row.2.var [⟨a, 0⟩] else row.2.var }
      t.modify path (treeWithPayload residual)) t
    rows.foldl (fun z row => max z (nodeValue ρ row.1 row.2)) z ≤
        levelValue ρ out.reify ∧
      levelValue ρ out.reify ≤ mapValue ρ s ∧ TreeChildrenSorted out := by
  induction rows generalizing t z with
  | nil => exact ⟨hLower, hUpper, hSorted⟩
  | cons row rows ih =>
    let path := s.lexChain row.1.length row.1
    let residual : Node := { row.2 with var := if let a :: _ := path then Normalize.subsumeVars row.2.var [⟨a, 0⟩] else row.2.var }
    let next := t.modify path (treeWithPayload residual)
    have hRow := hRows row (by simp)
    have hb := map_tree_row_update_bounds ρ s row.1 row.2 t hKeys hCompletes hRow
      hSorted (hZero row (by simp)) hUpper
    change TreeChildrenSorted next ∧
      levelValue ρ t.reify ≤ levelValue ρ next.reify ∧
      nodeValue ρ row.1 row.2 ≤ levelValue ρ next.reify ∧
      levelValue ρ next.reify ≤ mapValue ρ s at hb
    obtain ⟨hne, htail⟩ := List.pairwise_cons.mp hDistinct
    have hRowsNext : ∀ other ∈ rows, s.get? other.1 = some other.2 := by
      intro other ho
      exact hRows other (by simp [ho])
    have hZeroNext : ∀ other ∈ rows,
        treePayloadAt (s.lexChain other.1.length other.1).reverse next = ({} : Node) := by
      intro other ho
      have hpaths : (s.lexChain other.1.length other.1).reverse ≠ path.reverse := by
        intro he
        have hc : s.lexChain other.1.length other.1 = path := List.reverse_inj.mp he
        have hk := lexChain_key_injective s other.1 row.1
          (hKeys other.1 other.2 (hRowsNext other ho)) (hKeys row.1 row.2 hRow) hc
        exact hne other ho hk.symm
      have hf := (tree_modify_payload_frame t path residual hSorted).2.2 _ hpaths
      rw [hf]
      exact hZero other (by simp [ho])
    have hz : max z (nodeValue ρ row.1 row.2) ≤ levelValue ρ next.reify :=
      Nat.max_le.mpr ⟨Nat.le_trans hLower hb.2.1, hb.2.2.1⟩
    exact ih next (max z (nodeValue ρ row.1 row.2)) htail hRowsNext hb.1 hZeroNext hz hb.2.2.2

/-- Denotation of actual reconstruction from raw maps with independently
complete, strictly sorted stored keys. No normalization or Level-BEq premise. -/
theorem norm_toTree_reify_value (ρ : Valuation) (s : NormLevel)
    (hKeys : ∀ p n, s.get? p = some n → KeySorted p)
    (hCompletes : MapCompletes s) :
    levelValue ρ s.toTree.reify = mapValue ρ s := by
  have hDistinct : s.toList.Pairwise (fun a b => a.1 ≠ b.1) := by
    apply (Std.TreeMap.distinct_keys_toList (t := s)
      (cmp := List.compareLex Name.cmp)).imp
    intro a b hab he
    exact hab ((path_cmp_eq_iff a.1 b.1).mpr he)
  have hRows : ∀ row ∈ s.toList, s.get? row.1 = some row.2 := by
    intro row hr
    exact (Std.TreeMap.mem_toList_iff_getElem?_eq_some
      (cmp := List.compareLex Name.cmp)).mp hr
  have hempty := tree_default_value ρ
  have hf := map_tree_fold_value_bounds ρ s s.toList (default : Tree) 0 hKeys hCompletes
    hDistinct hRows tree_empty_payload_sorted.1
    (fun row _ => tree_empty_payload_sorted.2 _)
    (Nat.zero_le _) (by rw [hempty]; exact Nat.zero_le _)
  have hout : s.toTree = s.toList.foldl (fun (t : Tree) row =>
      let path := s.lexChain row.1.length row.1
      let residual : Node := { row.2 with var := if let a :: _ := path then Normalize.subsumeVars row.2.var [⟨a, 0⟩] else row.2.var }
      t.modify path (treeWithPayload residual)) (default : Tree) := by
    unfold NormLevel.toTree
    rw [Std.TreeMap.foldl_eq_foldl_toList]
    rfl
  have hmap : mapValue ρ s =
      s.toList.foldl (fun z row => max z (nodeValue ρ row.1 row.2)) 0 := by
    exact Std.TreeMap.foldl_eq_foldl_toList
  rw [hmap] at hf
  rw [hout, hmap]
  exact Nat.le_antisymm hf.2.1 hf.1

end Vesper.Lean4LeanLevel42
