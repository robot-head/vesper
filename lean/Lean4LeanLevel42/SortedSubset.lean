import Lean4LeanLevel42.SortedKeys

/-! Reflection of actual sorted subset tests and their erasure boundary.
Sound successful tests preserve multiplicity even on arbitrary raw lists. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level Lean.Level.Normalize

/-- Every successful actual test consumes each matching right entry once. -/
theorem subset_sublist {p q : List Name}
    (h : Normalize.subset Name.cmp p q = true) : p.Sublist q := by
  induction q generalizing p with
  | nil =>
    cases p with
    | nil => exact List.Sublist.refl _
    | cons a ps => simp [Normalize.subset] at h
  | cons b qs ih =>
    cases p with
    | nil => exact List.nil_sublist _
    | cons a ps =>
      cases hc : Name.cmp a b with
      | lt => simp [Normalize.subset, hc] at h
      | eq =>
        have hab : a = b := name_cmp_eq a b hc
        subst a
        exact (ih (by simpa [Normalize.subset, hc] using h)).cons₂ b
      | gt =>
        exact (ih (by simpa [Normalize.subset, hc] using h)).cons b

/-- Erasing one occurrence preserves strict key order. -/
theorem keySorted_erase (a : Name) (p : List Name) (hp : KeySorted p) :
    KeySorted (p.erase a) := by
  exact List.Pairwise.sublist List.erase_sublist hp

/-- Strict ordering on both sides makes the actual linear test complete. -/
theorem subset_complete (p q : List Name) (hp : KeySorted p) (hq : KeySorted q)
    (h : p ⊆ q) : Normalize.subset Name.cmp p q = true := by
  induction q generalizing p with
  | nil =>
    cases p with
    | nil => rfl
    | cons a ps => have ha := h (List.mem_cons_self (a := a) (l := ps)); simp at ha
  | cons b qs ih =>
    cases p with
    | nil => rfl
    | cons a ps =>
      obtain ⟨hpa, hps⟩ := List.pairwise_cons.mp hp
      obtain ⟨hqb, hqs⟩ := List.pairwise_cons.mp hq
      cases hc : Name.cmp a b with
      | lt =>
        have ha := h (List.mem_cons_self (a := a) (l := ps))
        rcases List.mem_cons.mp ha with hab | ha
        · subst a
          simp [name_cmp_refl] at hc
        · have hg := Std.OrientedCmp.gt_of_lt (cmp := Name.cmp) (hqb a ha)
          simp [hc] at hg
      | eq =>
        have hab : a = b := name_cmp_eq a b hc
        subst a
        have hs : ps ⊆ qs := by
          intro x hx
          rcases List.mem_cons.mp (h (List.mem_cons_of_mem b hx)) with he | hxq
          · subst x
            have hb := hpa b hx
            simp [name_cmp_refl] at hb
          · exact hxq
        simpa [Normalize.subset, hc] using ih ps hps hqs hs
      | gt =>
        have hs : (a :: ps) ⊆ qs := by
          intro x hx
          rcases List.mem_cons.mp (h hx) with he | hxq
          · subst x
            rcases List.mem_cons.mp hx with he | hb
            · subst b
              simp [name_cmp_refl] at hc
            · have hl := hpa b hb
              simp [hc] at hl
          · exact hxq
        simpa [Normalize.subset, hc] using ih (a :: ps) hp hqs hs

theorem subset_eq_true_iff (p q : List Name) (hp : KeySorted p) (hq : KeySorted q) :
    Normalize.subset Name.cmp p q = true ↔ p ⊆ q :=
  ⟨subset_sound, subset_complete p q hp hq⟩

/-- Strict left keys have no duplicate occurrence left behind by erasure. -/
theorem erase_subset_iff_subset_cons (p acc : List Name) (a : Name)
    (hp : KeySorted p) : p.erase a ⊆ acc ↔ p ⊆ a :: acc := by
  constructor
  · intro h x hx
    by_cases he : x = a
    · exact List.mem_cons.mpr (Or.inl he)
    · exact List.mem_cons.mpr (Or.inr (h ((List.mem_erase_of_ne he).mpr hx)))
  · intro h x hx
    have hh := (List.Nodup.mem_erase_iff (keySorted_nodup hp)).mp hx
    rcases List.mem_cons.mp (h hh.2) with he | hm
    · exact False.elim (hh.1 he)
    · exact hm

/-- An unsorted left list can have all its names present and still fail. -/
theorem subset_needs_left_sorted :
    let z : Name := .num .anonymous 0
    let o : Name := .num .anonymous 1
    ¬ KeySorted [o, z] ∧ KeySorted [z, o] ∧
      [o, z] ⊆ [z, o] ∧ Normalize.subset Name.cmp [o, z] [z, o] = false := by
  exact ⟨orderedInsert_unsorted_key_witness.1,
    by simp [KeySorted, Name.cmp]; decide, by simp, rfl⟩

/-- The actual early less-than branch can miss a later unsorted right name. -/
theorem subset_needs_right_sorted :
    let z : Name := .num .anonymous 0
    let o : Name := .num .anonymous 1
    KeySorted [z] ∧ ¬ KeySorted [o, z] ∧
      [z] ⊆ [o, z] ∧ Normalize.subset Name.cmp [z] [o, z] = false := by
  exact ⟨by simp [KeySorted], orderedInsert_unsorted_key_witness.1, by simp, rfl⟩

/-- Set inclusion loses left multiplicity, but the actual test does not. -/
theorem subset_duplicate_left_counterexample :
    let z : Name := .num .anonymous 0
    ¬ KeySorted [z, z] ∧ KeySorted [z] ∧
      [z, z] ⊆ [z] ∧ Normalize.subset Name.cmp [z, z] [z] = false := by
  exact ⟨by simp [KeySorted, name_cmp_refl], by simp [KeySorted], by simp, rfl⟩

/-- Removing only one occurrence cannot justify the erasure equivalence. -/
theorem erase_subset_needs_nodup_counterexample :
    let z : Name := .num .anonymous 0
    [z, z] ⊆ z :: [] ∧ ¬ ([z, z].erase z ⊆ ([] : List Name)) := by
  exact ⟨by simp, by simp⟩

end Vesper.Lean4LeanLevel42
