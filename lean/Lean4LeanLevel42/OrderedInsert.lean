import Lean4LeanLevel42.ListProofs

/-! Soundness of the actual ordered insertion helper on arbitrary input lists.
Sortedness can affect whether a duplicate is detected, but is not a premise
of the result-preservation facts below. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level Lean.Level.Normalize

universe u

theorem orderedInsert_some_perm {α : Type u} (cmp : α → α → Ordering) (a : α)
    {p p' : List α} (h : Normalize.orderedInsert cmp a p = some p') :
    p'.Perm (a :: p) := by
  induction p generalizing p' with
  | nil =>
    simp only [Normalize.orderedInsert, Option.some.injEq] at h
    subst p'
    exact List.Perm.refl _
  | cons b p ih =>
    cases hc : cmp a b with
    | lt =>
      simp only [Normalize.orderedInsert, hc, Option.some.injEq] at h
      subst p'
      exact List.Perm.refl _
    | eq => simp [Normalize.orderedInsert, hc] at h
    | gt =>
      cases hr : Normalize.orderedInsert cmp a p with
      | none => simp [Normalize.orderedInsert, hc, hr] at h
      | some q =>
        simp only [Normalize.orderedInsert, hc, hr, Option.map_some, Option.some.injEq] at h
        subst p'
        exact (List.Perm.cons b (ih hr)).trans (List.Perm.swap _ _ _)

theorem orderedInsert_some_length {α : Type u} (cmp : α → α → Ordering) (a : α)
    {p p' : List α} (h : Normalize.orderedInsert cmp a p = some p') :
    p'.length = p.length + 1 := by
  simpa only [List.length_cons] using (orderedInsert_some_perm cmp a h).length_eq

theorem orderedInsert_some_mem {v x : Name} {p p' : List Name}
    (h : Normalize.orderedInsert Name.cmp v p = some p') :
    x ∈ p' ↔ x = v ∨ x ∈ p := by
  simpa only [List.mem_cons] using (orderedInsert_some_perm Name.cmp v h).mem_iff (a := x)

theorem orderedInsert_none_mem {v : Name} {p : List Name}
    (h : Normalize.orderedInsert Name.cmp v p = none) : v ∈ p := by
  induction p with
  | nil => simp [Normalize.orderedInsert] at h
  | cons b p ih =>
    cases hc : Name.cmp v b with
    | lt => simp [Normalize.orderedInsert, hc] at h
    | eq =>
      have hvb := name_cmp_eq v b hc
      exact List.mem_cons.mpr (Or.inl hvb)
    | gt =>
      have hr : Normalize.orderedInsert Name.cmp v p = none := by
        cases he : Normalize.orderedInsert Name.cmp v p with
        | none => rfl
        | some q => simp [Normalize.orderedInsert, hc, he] at h
      exact List.mem_cons_of_mem b (ih hr)

theorem orderedInsert_some_of_not_mem {v : Name} {p : List Name} (hv : v ∉ p) :
    ∃ p', Normalize.orderedInsert Name.cmp v p = some p' := by
  cases h : Normalize.orderedInsert Name.cmp v p with
  | none => exact False.elim (hv (orderedInsert_none_mem h))
  | some p' => exact ⟨p', rfl⟩

theorem orderedInsert_input_subset {v : Name} {p p' : List Name}
    (h : Normalize.orderedInsert Name.cmp v p = some p') : p ⊆ p' := by
  intro x hx
  exact (orderedInsert_some_mem h).mpr (Or.inr hx)

theorem orderedInsert_inserted_mem {v : Name} {p p' : List Name}
    (h : Normalize.orderedInsert Name.cmp v p = some p') : v ∈ p' :=
  (orderedInsert_some_mem h).mpr (Or.inl rfl)

theorem orderedInsert_enabled (ρ : Valuation) {v : Name} {p p' : List Name}
    (h : Normalize.orderedInsert Name.cmp v p = some p') :
    enabled ρ p' = (decide (0 < ρ v) && enabled ρ p) := by
  have ht : enabled ρ p' = true ↔ (decide (0 < ρ v) && enabled ρ p) = true := by
    rw [enabled_eq_true_iff, Bool.and_eq_true, decide_eq_true_eq, enabled_eq_true_iff]
    constructor
    · intro hp
      exact ⟨hp v (orderedInsert_inserted_mem h),
        fun x hx => hp x (orderedInsert_input_subset h hx)⟩
    · rintro ⟨hv, hp⟩ x hx
      rcases (orderedInsert_some_mem h).mp hx with heq | hx
      · exact heq ▸ hv
      · exact hp x hx
  cases he : enabled ρ p' <;> cases hr : (decide (0 < ρ v) && enabled ρ p) <;>
    simp_all

theorem orderedInsert_none_positive {ρ : Valuation} {v : Name} {p : List Name}
    (h : Normalize.orderedInsert Name.cmp v p = none)
    (hp : enabled ρ p = true) : 0 < ρ v :=
  enabled_eq_true_iff.mp hp v (orderedInsert_none_mem h)

theorem orderedInsert_preserves_WF {v : Name} {p p' : List Name} {n : Node}
    (h : Normalize.orderedInsert Name.cmp v p = some p')
    (hw : NodeWF p n) : NodeWF p' n := by
  intro w hwmem
  exact orderedInsert_input_subset h (hw w hwmem)

theorem orderedInsert_some_nodup {α : Type u} (cmp : α → α → Ordering) (a : α)
    {p p' : List α} (h : Normalize.orderedInsert cmp a p = some p')
    (hp : p.Nodup) (ha : a ∉ p) : p'.Nodup := by
  have hi : (a :: p).Nodup := List.nodup_cons.mpr ⟨ha, hp⟩
  exact (orderedInsert_some_perm cmp a h).symm.nodup hi

/-- Being a member does not imply none on unsorted input: insertion stops at
the first less-than comparison and actually introduces a duplicate. -/
theorem orderedInsert_unsorted_duplicate_counterexample :
    let lo : Name := .num .anonymous 0
    let hi : Name := .num .anonymous 1
    Normalize.orderedInsert Name.cmp lo [hi, lo] = some [lo, hi, lo] ∧
      lo ∈ [hi, lo] ∧ ¬([lo, hi, lo] : List Name).Nodup := by
  dsimp only
  exact ⟨rfl, by simp, by simp⟩

/-- The inserted zero-valued name really disables a previously enabled key. -/
theorem orderedInsert_enabled_example :
    let lo : Name := .num .anonymous 0
    let hi : Name := .num .anonymous 1
    let ρ : Valuation := fun x => match x with | .num .anonymous 0 => 0 | _ => 1
    Normalize.orderedInsert Name.cmp lo [hi] = some [lo, hi] ∧
      enabled ρ [hi] = true ∧ enabled ρ [lo, hi] = false := by
  exact ⟨rfl, rfl, rfl⟩

/-- Actual equality returns none, and the enabled input supplies positivity. -/
theorem orderedInsert_none_example :
    let v : Name := .num .anonymous 1
    let ρ : Valuation := fun _ => 2
    Normalize.orderedInsert Name.cmp v [v] = none ∧
      enabled ρ [v] = true ∧ 0 < ρ v := by
  exact ⟨rfl, rfl, by decide⟩

end Vesper.Lean4LeanLevel42
