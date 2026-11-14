import Lean4LeanLevel42.MapValue
import Lean4LeanLevel42.VarInsert

namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level.Normalize

attribute [-simp] Std.TreeMap.get?_eq_getElem?

/-- Lookup for the actual comparator captured by the vendor map. -/
theorem map_get_alter (s : NormLevel) (p q : List Name) (f : Option Node → Option Node) :
    (s.alter p f).get? q = if p = q then f (s.get? p) else s.get? q := by
  have h := @Std.TreeMap.getElem?_alter (List Name) Node
    (List.compareLex Name.cmp) s path_cmp_trans p q f
  simpa only [Std.TreeMap.get?_eq_getElem?, path_cmp_eq_iff] using h

theorem map_get_modify (s : NormLevel) (p q : List Name) (f : Node → Node) :
    (s.modify p f).get? q = if p = q then (s.get? p).map f else s.get? q := by
  have h := @Std.TreeMap.getElem?_modify (List Name) Node
    (List.compareLex Name.cmp) s path_cmp_trans p q f
  simpa only [Std.TreeMap.get?_eq_getElem?, path_cmp_eq_iff] using h

theorem mapValue_congr {ρ : Valuation} {s t : NormLevel}
    (h : ∀ p, s.get? p = t.get? p) : mapValue ρ s = mapValue ρ t := by
  apply Nat.le_antisymm
  · apply mapValue_mono
    intro p n hn
    exact ⟨n, (h p) ▸ hn, Nat.le_refl _⟩
  · apply mapValue_mono
    intro p n hn
    exact ⟨n, (h p).symm ▸ hn, Nat.le_refl _⟩

/-- A local update's denotation, with the old row read before overwriting. -/
theorem mapValue_update (ρ : Valuation) (s t : NormLevel) (p : List Name)
    (n : Node) (a : Nat)
    (ht : ∀ q, t.get? q = if p = q then some n else s.get? q)
    (hn : nodeValue ρ p n = max (((s.get? p).map (nodeValue ρ p)).getD 0) a) :
    mapValue ρ t = max (mapValue ρ s) a := by
  have he : mapValue ρ t = mapValue ρ (s.insert p n) := by
    apply mapValue_congr
    intro q
    rw [ht, map_get_insert]
  rw [he, mapValue_insert, hn]
  have hd := mapValue_decompose ρ s p
  omega

theorem map_addNode_value (ρ : Valuation) (v : Name) (k : Nat)
    (p : List Name) (s : NormLevel) :
    mapValue ρ (s.addNode v k p) =
      max (mapValue ρ s) (if enabled ρ p then ρ v + k else 0) := by
  cases hs : s.get? p with
  | none =>
    apply mapValue_update ρ s _ p { var := [⟨v, k⟩] } _
    · intro q
      simp only [NormLevel.addNode, map_get_alter, hs]
    · simp [hs, nodeValue, varsValue_cons, varsValue_nil]
  | some n =>
    apply mapValue_update ρ s _ p { n with var := VarNode.addVar v k n.var } _
    · intro q
      simp only [NormLevel.addNode, map_get_alter, hs]
    · simpa [hs] using node_addVar_value ρ p n v k

theorem map_addVar_value (ρ : Valuation) (v : Name) (k : Nat)
    (p : List Name) (s : NormLevel) (n : Node) (hs : s.get? p = some n) :
    mapValue ρ (s.addVar v k p) =
      max (mapValue ρ s) (if enabled ρ p then ρ v + k else 0) := by
  apply mapValue_update ρ s _ p { n with var := VarNode.addVar v k n.var } _
  · intro q
    simp only [NormLevel.addVar, map_get_modify, hs, Option.map_some]
  · simpa [hs] using node_addVar_value ρ p n v k

theorem node_addConst_value (ρ : Valuation) (p : List Name) (n : Node) (k : Nat) :
    nodeValue ρ p { n with const := max k n.const } =
      max (nodeValue ρ p n) (if enabled ρ p then k else 0) := by
  cases he : enabled ρ p <;> simp [nodeValue, he] <;> omega

theorem map_addConst_value (ρ : Valuation) (k : Nat) (p : List Name) (s : NormLevel) :
    mapValue ρ (s.addConst k p) =
      if k = 0 || k = 1 && !p.isEmpty then mapValue ρ s
      else max (mapValue ρ s) (if enabled ρ p then k else 0) := by
  unfold NormLevel.addConst
  split
  · rfl
  · cases hs : s.get? p with
    | none =>
      apply mapValue_update ρ s _ p { const := k } _
      · intro q
        simp only [map_get_alter, hs]
      · simp [hs, nodeValue, varsValue_nil]
    | some n =>
      apply mapValue_update ρ s _ p { n with const := max k n.const } _
      · intro q
        simp only [map_get_alter, hs]
      · simpa [hs] using node_addConst_value ρ p n k

theorem map_addConst_value_of_covered (ρ : Valuation) (k : Nat)
    (p : List Name) (s : NormLevel)
    (hc : k = 1 → p ≠ [] → enabled ρ p = true → 1 ≤ mapValue ρ s) :
    mapValue ρ (s.addConst k p) =
      max (mapValue ρ s) (if enabled ρ p then k else 0) := by
  rw [map_addConst_value]
  split
  · rename_i h
    have h' : k = 0 ∨ k = 1 ∧ p ≠ [] := by simpa using h
    rcases h' with hz | ⟨hk, hp⟩
    · simp [hz]
    · cases he : enabled ρ p with
      | false => simp
      | true => have hb := hc hk hp he; simp only [ite_true]; omega
  · rfl

theorem map_addNode_WF {s : NormLevel} (v : Name) (k : Nat) (p : List Name)
    (hs : MapWF s) (hv : v ∈ p) : MapWF (s.addNode v k p) := by
  intro q n hn
  rw [NormLevel.addNode, map_get_alter] at hn
  split at hn
  · rename_i hp
    subst q
    cases h : s.get? p with
    | none =>
      simp only [h, Option.some.injEq] at hn
      subst n
      intro w hw
      simpa using (List.mem_singleton.mp hw ▸ hv)
    | some m =>
      simp only [h, Option.some.injEq] at hn
      subst n
      exact node_addVar_WF p m v k (hs p m h) hv
  · exact hs q n hn

theorem map_addVar_WF {s : NormLevel} (v : Name) (k : Nat) (p : List Name)
    (hs : MapWF s) (hv : v ∈ p) : MapWF (s.addVar v k p) := by
  intro q n hn
  rw [NormLevel.addVar, map_get_modify] at hn
  split at hn
  · rename_i hp
    subst q
    cases h : s.get? p with
    | none => simp [h] at hn
    | some m =>
      simp only [h, Option.map_some, Option.some.injEq] at hn
      subst n
      exact node_addVar_WF p m v k (hs p m h) hv
  · exact hs q n hn

theorem map_addConst_WF {s : NormLevel} (k : Nat) (p : List Name)
    (hs : MapWF s) : MapWF (s.addConst k p) := by
  unfold NormLevel.addConst
  split
  · exact hs
  · intro q n hn
    rw [map_get_alter] at hn
    split at hn
    · rename_i hp
      subst q
      cases h : s.get? p with
      | none =>
        simp only [h, Option.some.injEq] at hn
        subst n
        intro w hw
        simp at hw
      | some m =>
        simp only [h, Option.some.injEq] at hn
        subst n
        exact hs p m h
    · exact hs q n hn

theorem map_addNode_preserves_lookup {s : NormLevel} {q : List Name} {n : Node}
    (v : Name) (k : Nat) (p : List Name) (h : s.get? q = some n) :
    ∃ n', (s.addNode v k p).get? q = some n' := by
  by_cases hp : p = q
  · subst p
    exact ⟨{ n with var := VarNode.addVar v k n.var }, by
      simp [NormLevel.addNode, map_get_alter, h]⟩
  · exact ⟨n, by simp [NormLevel.addNode, map_get_alter, hp, h]⟩

theorem map_addVar_preserves_lookup {s : NormLevel} {q : List Name} {n : Node}
    (v : Name) (k : Nat) (p : List Name) (h : s.get? q = some n) :
    ∃ n', (s.addVar v k p).get? q = some n' := by
  by_cases hp : p = q
  · subst p
    exact ⟨{ n with var := VarNode.addVar v k n.var }, by
      simp [NormLevel.addVar, map_get_modify, h]⟩
  · exact ⟨n, by simp [NormLevel.addVar, map_get_modify, hp, h]⟩

theorem map_addConst_preserves_lookup {s : NormLevel} {q : List Name} {n : Node}
    (k : Nat) (p : List Name) (h : s.get? q = some n) :
    ∃ n', (s.addConst k p).get? q = some n' := by
  unfold NormLevel.addConst
  split
  · exact ⟨n, h⟩
  · by_cases hp : p = q
    · subst p
      exact ⟨{ n with const := max k n.const }, by simp [map_get_alter, h]⟩
    · exact ⟨n, by simp [map_get_alter, hp, h]⟩

theorem map_addNode_preserves_var {s : NormLevel} {q : List Name} {n : Node}
    {w : VarNode} (v : Name) (k : Nat) (p : List Name)
    (h : s.get? q = some n) (hw : w ∈ n.var) :
    ∃ n' w', (s.addNode v k p).get? q = some n' ∧ w' ∈ n'.var ∧ w'.var = w.var := by
  by_cases hp : p = q
  · subst p
    rcases (addVar_names v k n.var w.var).mpr (Or.inr ⟨w, hw, rfl⟩) with ⟨w', hw', he⟩
    exact ⟨{ n with var := VarNode.addVar v k n.var }, w', by
      simp [NormLevel.addNode, map_get_alter, h], hw', he⟩
  · exact ⟨n, w, by simp [NormLevel.addNode, map_get_alter, hp, h], hw, rfl⟩

theorem map_addVar_preserves_var {s : NormLevel} {q : List Name} {n : Node}
    {w : VarNode} (v : Name) (k : Nat) (p : List Name)
    (h : s.get? q = some n) (hw : w ∈ n.var) :
    ∃ n' w', (s.addVar v k p).get? q = some n' ∧ w' ∈ n'.var ∧ w'.var = w.var := by
  by_cases hp : p = q
  · subst p
    rcases (addVar_names v k n.var w.var).mpr (Or.inr ⟨w, hw, rfl⟩) with ⟨w', hw', he⟩
    exact ⟨{ n with var := VarNode.addVar v k n.var }, w', by
      simp [NormLevel.addVar, map_get_modify, h], hw', he⟩
  · exact ⟨n, w, by simp [NormLevel.addVar, map_get_modify, hp, h], hw, rfl⟩

theorem map_addConst_preserves_var {s : NormLevel} {q : List Name} {n : Node}
    {w : VarNode} (k : Nat) (p : List Name)
    (h : s.get? q = some n) (hw : w ∈ n.var) :
    ∃ n' w', (s.addConst k p).get? q = some n' ∧ w' ∈ n'.var ∧ w'.var = w.var := by
  unfold NormLevel.addConst
  split
  · exact ⟨n, w, h, hw, rfl⟩
  · by_cases hp : p = q
    · subst p
      exact ⟨{ n with const := max k n.const }, w, by simp [map_get_alter, h], hw, rfl⟩
    · exact ⟨n, w, by simp [map_get_alter, hp, h], hw, rfl⟩

theorem map_addNode_var (s : NormLevel) (v : Name) (k : Nat) (p : List Name) :
    ∃ n w, (s.addNode v k p).get? p = some n ∧ w ∈ n.var ∧ w.var = v := by
  cases h : s.get? p with
  | none =>
    exact ⟨{ var := [⟨v, k⟩] }, ⟨v, k⟩, by
      simp [NormLevel.addNode, map_get_alter, h], List.mem_cons_self, rfl⟩
  | some n =>
    rcases (addVar_names v k n.var v).mpr (Or.inl rfl) with ⟨w, hw, he⟩
    exact ⟨{ n with var := VarNode.addVar v k n.var }, w, by
      simp [NormLevel.addNode, map_get_alter, h], hw, he⟩

/-- Missing-key modify does not create the positive variable contribution. -/
theorem missing_key_addVar_counterexample :
    let v : Name := .num .anonymous 0
    let ρ : Valuation := fun _ => 2
    mapValue ρ (({} : NormLevel).addVar v 0 [v]) = 0 ∧
      (if enabled ρ [v] then ρ v + 0 else 0) = 2 := by
  constructor
  · rw [show mapValue (fun _ => 2) (({} : NormLevel).addVar (.num .anonymous 0) 0
        [.num .anonymous 0]) = mapValue (fun _ => 2) ({} : NormLevel) from
      mapValue_congr (fun q => by simp [NormLevel.addVar, map_get_modify, map_get_empty])]
    exact mapValue_empty _
  · rfl

/-- NodeWF and an empty map do not justify the actual skipped-one branch. -/
theorem skipped_one_empty_counterexample :
    let v : Name := .num .anonymous 0
    let ρ : Valuation := fun _ => 2
    MapWF ({} : NormLevel) ∧ mapValue ρ (({} : NormLevel).addConst 1 [v]) = 0 ∧
      (if enabled ρ [v] then 1 else 0) = 1 := by
  exact ⟨mapWF_empty, mapValue_empty _, rfl⟩

/-- Even key presence and NodeWF leave a zero row insufficient for skipping one. -/
theorem skipped_one_zero_node_counterexample :
    let v : Name := .num .anonymous 0
    let ρ : Valuation := fun _ => 2
    let s : NormLevel := ({} : NormLevel).insert [v] {}
    MapWF s ∧ s.get? [v] = some {} ∧ mapValue ρ (s.addConst 1 [v]) = 0 ∧
      (if enabled ρ [v] then 1 else 0) = 1 := by
  dsimp
  refine ⟨?_, ?_, ?_, rfl⟩
  · intro p n hn
    rw [map_get_insert, map_get_empty] at hn
    split at hn
    · cases Option.some.inj hn
      intro w hw
      simp at hw
    · contradiction
  · simp [map_get_insert]
  · change mapValue (fun _ => 2) (({} : NormLevel).insert [.num .anonymous 0] {}) = 0
    apply Nat.le_antisymm
    · apply mapValue_le
      intro p n hn
      rw [map_get_insert, map_get_empty] at hn
      split at hn
      · cases Option.some.inj hn
        simp [nodeValue, varsValue_nil]
      · contradiction
    · exact Nat.zero_le _

attribute [simp] Std.TreeMap.get?_eq_getElem?

end Vesper.Lean4LeanLevel42
