import Lean4LeanLevel42.SortedSubset
import Lean4LeanLevel42.MapAny
import Lean4LeanLevel42.MapEquality

/-! Exact witnesses and monotonicity for the unchanged addability test.
Offsets remain arbitrary; semantic or raw-map shape assumptions are unnecessary
for the operational witness and extensional congruence results. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level Lean.Level.Normalize

theorem addable_iff_witness (s : NormLevel) (a : Name) (acc : List Name) :
    s.addable a acc = true ↔
      ∃ p n k, s.get? p = some n ∧ ⟨a, k⟩ ∈ n.var ∧
        Normalize.subset Name.cmp (p.erase a) acc = true := by
  change (s.any (fun p n => n.var.any (fun v => v.var == a) &&
    Normalize.subset Name.cmp (p.erase a) acc)) = true ↔ _
  rw [norm_any_eq_true_iff]
  constructor
  · rintro ⟨p, n, hget, hrow⟩
    rcases Bool.and_eq_true_iff.mp hrow with ⟨hvar, hsub⟩
    rcases List.any_eq_true.mp hvar with ⟨⟨v, k⟩, hmem, he⟩
    have hname : v = a := beq_iff_eq.mp he
    subst v
    exact ⟨p, n, k, hget, hmem, hsub⟩
  · rintro ⟨p, n, k, hget, hvar, hsub⟩
    exact ⟨p, n, hget, Bool.and_eq_true_iff.mpr
      ⟨List.any_eq_true.mpr ⟨⟨a, k⟩, hvar, by simp⟩, hsub⟩⟩

theorem addable_iff_subset (s : NormLevel) (a : Name) (acc : List Name)
    (hkeys : ∀ p n, s.get? p = some n → KeySorted p) (hacc : KeySorted acc) :
    s.addable a acc = true ↔
      ∃ p n k, s.get? p = some n ∧ ⟨a, k⟩ ∈ n.var ∧ p ⊆ a :: acc := by
  rw [addable_iff_witness]
  constructor
  · rintro ⟨p, n, k, hget, hvar, hsub⟩
    exact ⟨p, n, k, hget, hvar,
      (erase_subset_iff_subset_cons p acc a (hkeys p n hget)).mp (subset_sound hsub)⟩
  · rintro ⟨p, n, k, hget, hvar, hsub⟩
    have hp := hkeys p n hget
    exact ⟨p, n, k, hget, hvar, subset_complete (p.erase a) acc
      (keySorted_erase a p hp) hacc
      ((erase_subset_iff_subset_cons p acc a hp).mpr hsub)⟩

/-- Successful matching into the old sorted accumulator sorts the erased key. -/
theorem addable_mono_sorted_acc (s : NormLevel) (a : Name) (acc acc' : List Name)
    (hacc : KeySorted acc) (hacc' : KeySorted acc') (hinc : acc ⊆ acc')
    (h : s.addable a acc = true) : s.addable a acc' = true := by
  obtain ⟨p, n, k, hget, hvar, hsub⟩ := (addable_iff_witness s a acc).mp h
  apply (addable_iff_witness s a acc').mpr
  refine ⟨p, n, k, hget, hvar, ?_⟩
  have hp : KeySorted (p.erase a) := List.Pairwise.sublist (subset_sublist hsub) hacc
  exact subset_complete (p.erase a) acc' hp hacc'
    (fun v hv => hinc (subset_sound hsub hv))

/-- Sorted map keys allow the old accumulator itself to be completely raw. -/
theorem addable_mono_keys (s : NormLevel) (a : Name) (acc acc' : List Name)
    (hkeys : ∀ p n, s.get? p = some n → KeySorted p)
    (hacc' : KeySorted acc') (hinc : acc ⊆ acc')
    (h : s.addable a acc = true) : s.addable a acc' = true := by
  obtain ⟨p, n, k, hget, hvar, hsub⟩ := (addable_iff_witness s a acc).mp h
  apply (addable_iff_witness s a acc').mpr
  exact ⟨p, n, k, hget, hvar, subset_complete (p.erase a) acc'
    (keySorted_erase a p (hkeys p n hget)) hacc'
    (fun v hv => hinc (subset_sound hsub hv))⟩

theorem addable_orderedInsert (s : NormLevel) (a b : Name) (acc : List Name)
    (hacc : KeySorted acc) (h : s.addable b acc = true) :
    s.addable b ((Normalize.orderedInsert Name.cmp a acc).getD acc) = true := by
  cases hi : Normalize.orderedInsert Name.cmp a acc with
  | none => simpa only [hi, Option.getD_none] using h
  | some acc' =>
    simp only [Option.getD_some]
    exact addable_mono_sorted_acc s b acc acc' hacc (orderedInsert_sorted hacc hi)
      (orderedInsert_input_subset hi) h

theorem addable_lookup_congr (s t : NormLevel) (a : Name) (acc : List Name)
    (h : ∀ p, s.get? p = t.get? p) : s.addable a acc = t.addable a acc := by
  unfold NormLevel.addable
  exact norm_any_congr_lookup s t _ h

theorem addable_beq_congr (s t : NormLevel) (a : Name) (acc : List Name)
    (h : (s == t) = true) : s.addable a acc = t.addable a acc := by
  exact addable_lookup_congr s t a acc ((norm_beq_iff_lookup_eq s t).mp h)

/-- Exact traversal identity, retaining the actual captured Name comparator. -/
theorem addable_eq_toList_any (s : NormLevel) (a : Name) (acc : List Name) :
    s.addable a acc = s.toList.any (fun row =>
      row.2.var.any (fun v => v.var == a) &&
        Normalize.subset Name.cmp (row.1.erase a) acc) := by
  change s.any (fun p n => n.var.any (fun v => v.var == a) &&
    Normalize.subset Name.cmp (p.erase a) acc) = _
  exact norm_any_eq_toList_any s _

/-- Even a sorted stored key cannot compensate for an unsorted target. -/
theorem addable_needs_sorted_target :
    let z : Name := .num .anonymous 0
    let o : Name := .num .anonymous 1
    let a : Name := .num .anonymous 2
    let s : NormLevel := ({} : NormLevel).insert [z, a] { var := [⟨a, 0⟩] }
    (∀ p n, s.get? p = some n → KeySorted p) ∧ KeySorted [z] ∧
      [z] ⊆ [o, z] ∧ [z, a] ⊆ a :: [o, z] ∧ ¬KeySorted [o, z] ∧
      s.addable a [z] = true ∧ s.addable a [o, z] = false := by
  dsimp only
  have ht : (({} : NormLevel).insert [.num .anonymous 0, .num .anonymous 2]
      { var := [⟨.num .anonymous 2, 0⟩] }).toList =
      [([.num .anonymous 0, .num .anonymous 2],
        { var := [⟨.num .anonymous 2, 0⟩] })] := by rfl
  refine ⟨?_, by simp [KeySorted], by intro v hv; simp_all,
    ?_, ?_, ?_, ?_⟩
  · intro p n hget
    rw [map_get_insert] at hget
    split at hget
    next he => subst p; simp [KeySorted, Name.cmp]; decide
    next => rw [map_get_empty] at hget; contradiction
  · intro v hv
    simp only [List.mem_cons, List.not_mem_nil, or_false] at hv
    rcases hv with rfl | rfl <;> simp
  · simp [KeySorted, Name.cmp]
    decide
  · rw [addable_eq_toList_any, ht]
    simp [Normalize.subset, Name.cmp]
  · rw [addable_eq_toList_any, ht]
    simp [Normalize.subset, Name.cmp]
    decide

/-- A sorted target cannot repair an unsorted stored key after a raw success. -/
theorem addable_needs_sorted_keys :
    let z : Name := .num .anonymous 0
    let o : Name := .num .anonymous 1
    let a : Name := .num .anonymous 2
    let s : NormLevel := ({} : NormLevel).insert [o, z, a] { var := [⟨a, 0⟩] }
    KeySorted [z, o] ∧ ¬KeySorted [o, z, a] ∧ [o, z] ⊆ [z, o] ∧
      [o, z, a] ⊆ a :: [z, o] ∧
      s.addable a [o, z] = true ∧ s.addable a [z, o] = false := by
  dsimp only
  have ht : (({} : NormLevel).insert [.num .anonymous 1, .num .anonymous 0,
      .num .anonymous 2] { var := [⟨.num .anonymous 2, 0⟩] }).toList =
      [([.num .anonymous 1, .num .anonymous 0, .num .anonymous 2],
        { var := [⟨.num .anonymous 2, 0⟩] })] := by rfl
  refine ⟨?_, ?_, ?_, ?_, ?_, ?_⟩
  · simp [KeySorted, Name.cmp]
    decide
  · simp [KeySorted, Name.cmp]
    decide
  · intro v hv
    simp only [List.mem_cons, List.not_mem_nil, or_false] at hv
    rcases hv with rfl | rfl <;> simp
  · intro v hv
    simp only [List.mem_cons, List.not_mem_nil, or_false] at hv
    rcases hv with rfl | rfl | rfl <;> simp
  · rw [addable_eq_toList_any, ht]
    simp [Normalize.subset, Name.cmp]
  · rw [addable_eq_toList_any, ht]
    simp [Normalize.subset, Name.cmp]
    decide

/-- A stored positive-offset variable really suffices; no zero-offset row exists. -/
theorem addable_nonzero_offset_witness :
    let a : Name := .num .anonymous 2
    let s : NormLevel := ({} : NormLevel).insert [a] { var := [⟨a, 7⟩] }
    s.get? [a] = some { var := [⟨a, 7⟩] } ∧ s.addable a [] = true ∧
      ¬∃ p n, s.get? p = some n ∧ (⟨a, 0⟩ : VarNode) ∈ n.var := by
  dsimp only
  have ht : (({} : NormLevel).insert [.num .anonymous 2]
      { var := [⟨.num .anonymous 2, 7⟩] }).toList =
      [([.num .anonymous 2], { var := [⟨.num .anonymous 2, 7⟩] })] := by rfl
  refine ⟨?_, ?_, ?_⟩
  · rw [map_get_insert]
    rfl
  · rw [addable_eq_toList_any, ht]
    simp [Normalize.subset]
  · rintro ⟨p, n, hget, hvar⟩
    rw [map_get_insert] at hget
    split at hget
    · cases hget
      simp at hvar
    · rw [map_get_empty] at hget
      contradiction

end Vesper.Lean4LeanLevel42
