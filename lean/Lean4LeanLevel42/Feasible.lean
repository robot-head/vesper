import Lean4LeanLevel42.Completion
import Lean4LeanLevel42.Addable

/-! Actual greedy feasibility reflects independent permutation certificates.
The exchange argument permits duplicates and names already in the accumulator. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level Lean.Level.Normalize

theorem addable_edgeCovered (s : NormLevel) (a : Name) (acc : List Name)
    (h : s.addable a acc = true) : EdgeCovered s acc a := by
  obtain ⟨p, n, k, hp, hv, hsub⟩ := (addable_iff_witness s a acc).mp h
  refine ⟨p, n, k, hp, hv, ?_⟩
  intro b hb
  by_cases he : b = a
  · exact List.mem_cons.mpr (Or.inl he)
  · exact List.mem_cons.mpr (Or.inr
      (subset_sound hsub ((List.mem_erase_of_ne he).mpr hb)))

theorem addable_iff_edgeCovered (s : NormLevel) (a : Name) (acc : List Name)
    (hkeys : ∀ p n, s.get? p = some n → KeySorted p) (hacc : KeySorted acc) :
    s.addable a acc = true ↔ EdgeCovered s acc a :=
  addable_iff_subset s a acc hkeys hacc

/-- Actual ordered insertion/getD has the membership of cons, even on none. -/
theorem feasible_insert_getD_mem (a : Name) (acc : List Name) (b : Name) :
    b ∈ (Normalize.orderedInsert Name.cmp a acc).getD acc ↔ b ∈ a :: acc := by
  cases hi : Normalize.orderedInsert Name.cmp a acc with
  | none =>
    simp only [Option.getD_none]
    constructor
    · exact List.mem_cons_of_mem a
    · intro h
      rcases List.mem_cons.mp h with he | h
      · subst b
        exact orderedInsert_none_mem hi
      · exact h
  | some acc' =>
    simpa only [Option.getD_some, List.mem_cons] using
      (orderedInsert_some_mem (x := b) hi)

theorem feasible_insert_getD_sorted (a : Name) (acc : List Name)
    (hacc : KeySorted acc) :
    KeySorted ((Normalize.orderedInsert Name.cmp a acc).getD acc) := by
  cases hi : Normalize.orderedInsert Name.cmp a acc with
  | none => exact hacc
  | some acc' => exact orderedInsert_sorted hacc hi

/-- Move one occurrence to the accumulator and erase only that occurrence.
This works even when that name was already accumulated or occurs repeatedly. -/
theorem admissible_erase (s : NormLevel) (acc order : List Name) (a : Name)
    (h : Admissible s acc order) : Admissible s (a :: acc) (order.erase a) := by
  induction order generalizing acc with
  | nil => trivial
  | cons b rest ih =>
    by_cases he : b = a
    · subst b
      simpa only [List.erase_cons_head] using h.2
    · rw [List.erase_cons_tail (by simpa only [beq_iff_eq] using he)]
      change EdgeCovered s (a :: acc) b ∧ Admissible s (b :: a :: acc) (rest.erase a)
      refine ⟨edgeCovered_acc_mono s b acc (a :: acc)
        (fun _ hb => List.mem_cons_of_mem a hb) h.1, ?_⟩
      apply admissible_acc_mono s (a :: b :: acc) (b :: a :: acc) (rest.erase a)
        ?_ (ih (b :: acc) h.2)
      intro x hx
      simp only [List.mem_cons] at hx ⊢
      rcases hx with hx | hx | hx
      · exact Or.inr (Or.inl hx)
      · exact Or.inl hx
      · exact Or.inr (Or.inr hx)

theorem completes_erase (s : NormLevel) (acc rem : List Name) (a : Name)
    (h : Completes s acc rem) : Completes s (a :: acc) (rem.erase a) := by
  obtain ⟨order, hp, ha⟩ := h
  exact ⟨order.erase a, hp.erase a, admissible_erase s acc order a ha⟩

/-- A nonempty independent certificate supplies an actually covered first name. -/
theorem completes_nonempty_edge (s : NormLevel) (acc rem : List Name)
    (h : Completes s acc rem) (hne : rem ≠ []) :
    ∃ a ∈ rem, EdgeCovered s acc a := by
  obtain ⟨order, hp, ha⟩ := h
  cases order with
  | nil =>
    have hr : rem = [] := (List.Perm.nil_eq hp).symm
    exact False.elim (hne hr)
  | cons a rest => exact ⟨a, hp.subset (by simp), ha.1⟩

/-- Success in the actual fuelled search always supplies a certificate. -/
theorem feasible_go_sound (s : NormLevel) (fuel : Nat) (acc rem : List Name)
    (h : NormLevel.feasible.go s fuel acc rem = true) : Completes s acc rem := by
  induction fuel generalizing acc rem with
  | zero =>
    have hr : rem = [] := by simpa only [NormLevel.feasible.go, List.isEmpty_iff] using h
    subst rem
    exact completes_nil s acc
  | succ fuel ih =>
    cases hf : rem.find? (s.addable · acc) with
    | none =>
      have hr : rem = [] := by simpa only [NormLevel.feasible.go, hf, List.isEmpty_iff] using h
      subst rem
      exact completes_nil s acc
    | some a =>
      have hnext : NormLevel.feasible.go s fuel
          ((Normalize.orderedInsert Name.cmp a acc).getD acc) (rem.erase a) = true := by
        simpa only [NormLevel.feasible.go, hf] using h
      obtain ⟨order, hp, ha⟩ := ih _ _ hnext
      refine ⟨a :: order, (hp.cons a).trans (List.perm_cons_erase
        (List.mem_of_find?_eq_some hf)).symm, ?_⟩
      refine ⟨addable_edgeCovered s a acc (List.find?_some (p := fun x => s.addable x acc) hf), ?_⟩
      apply admissible_acc_mono s _ (a :: acc) order ?_ ha
      intro b hb
      exact (feasible_insert_getD_mem a acc b).mp hb

/-- Enough fuel makes the actual greedy choice complete on sorted keys/acc.
The remaining list is arbitrary: no nodup or freshness premise is needed. -/
theorem feasible_go_complete (s : NormLevel) (fuel : Nat) (acc rem : List Name)
    (hkeys : ∀ p n, s.get? p = some n → KeySorted p) (hacc : KeySorted acc)
    (hfuel : rem.length ≤ fuel) (h : Completes s acc rem) :
    NormLevel.feasible.go s fuel acc rem = true := by
  induction fuel generalizing acc rem with
  | zero =>
    have hr : rem = [] := List.length_eq_zero_iff.mp (by omega)
    subst rem
    rfl
  | succ fuel ih =>
    cases hf : rem.find? (s.addable · acc) with
    | none =>
      have hr : rem = [] := by
        by_cases hnil : rem = []
        · exact hnil
        · obtain ⟨a, hm, he⟩ := completes_nonempty_edge s acc rem h hnil
          have ha := (addable_iff_edgeCovered s a acc hkeys hacc).mpr he
          exact False.elim ((List.find?_eq_none.mp hf a hm) ha)
      subst rem
      rfl
    | some a =>
      have hm := List.mem_of_find?_eq_some hf
      have hl : (rem.erase a).length ≤ fuel := by
        rw [List.length_erase_of_mem hm]
        omega
      have hc : Completes s ((Normalize.orderedInsert Name.cmp a acc).getD acc)
          (rem.erase a) := by
        apply completes_acc_mono s (a :: acc) _ (rem.erase a)
          (fun b hb => (feasible_insert_getD_mem a acc b).mpr hb)
          (completes_erase s acc rem a h)
      have hn := ih _ _ (feasible_insert_getD_sorted a acc hacc) hl hc
      simpa only [NormLevel.feasible.go, hf] using hn

theorem feasible_sound (s : NormLevel) (acc rem : List Name)
    (h : s.feasible acc rem = true) : Completes s acc rem :=
  feasible_go_sound s rem.length acc rem h

theorem feasible_complete (s : NormLevel) (acc rem : List Name)
    (hkeys : ∀ p n, s.get? p = some n → KeySorted p) (hacc : KeySorted acc)
    (h : Completes s acc rem) : s.feasible acc rem = true :=
  feasible_go_complete s rem.length acc rem hkeys hacc (Nat.le_refl _) h

theorem feasible_iff_completes (s : NormLevel) (acc rem : List Name)
    (hkeys : ∀ p n, s.get? p = some n → KeySorted p) (hacc : KeySorted acc) :
    s.feasible acc rem = true ↔ Completes s acc rem :=
  ⟨feasible_sound s acc rem, feasible_complete s acc rem hkeys hacc⟩

/-- A positive-offset row supports repeated remaining names already in acc;
actual insertion returns none in both steps, while erase removes one occurrence. -/
theorem feasible_duplicate_present_witness :
    let a : Name := .num .anonymous 0
    let s : NormLevel := ({} : NormLevel).insert [a] { var := [⟨a, 7⟩] }
    Normalize.orderedInsert Name.cmp a [a] = none ∧
      NormLevel.feasible.go s 1 [a] [a, a] = false ∧
      s.feasible [a] [a, a] = true ∧ Completes s [a] [a, a] := by
  dsimp only
  refine ⟨rfl, rfl, rfl, ?_⟩
  exact feasible_sound _ _ _ rfl

end Vesper.Lean4LeanLevel42
