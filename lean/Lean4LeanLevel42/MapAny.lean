import Lean4LeanLevel42.MapValue

/-! The actual short-circuit any traversal, reflected through actual stored
lookups. No row well-formedness or inner-list order is assumed. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level.Normalize

theorem list_forIn_any_eq {α : Type} (xs : List α) (f : α → Bool) :
    (Id.run do
      for x in xs do
        if f x then return true
      return false) = xs.any f := by
  induction xs with
  | nil => rfl
  | cons x xs ih =>
    cases hf : f x with
    | false => simpa [List.forIn_cons, hf, List.any_cons] using ih
    | true => simp [List.forIn_cons, hf, List.any_cons]

theorem norm_any_eq_toList_any (s : NormLevel) (f : List Name → Node → Bool) :
    s.any f = s.toList.any (fun row => f row.1 row.2) := by
  change (Id.run do
    for row in s do
      if f row.1 row.2 then return true
    return false) = _
  rw [Std.TreeMap.forIn_eq_forIn_toList]
  exact list_forIn_any_eq s.toList (fun row => f row.1 row.2)

theorem norm_any_eq_true_iff (s : NormLevel) (f : List Name → Node → Bool) :
    s.any f = true ↔ ∃ p n, s.get? p = some n ∧ f p n = true := by
  rw [norm_any_eq_toList_any, List.any_eq_true]
  constructor
  · rintro ⟨⟨p, n⟩, hmem, hf⟩
    exact ⟨p, n, (Std.TreeMap.mem_toList_iff_getElem?_eq_some
      (cmp := List.compareLex Name.cmp)).mp hmem, hf⟩
  · rintro ⟨p, n, hget, hf⟩
    exact ⟨(p, n), (Std.TreeMap.mem_toList_iff_getElem?_eq_some
      (cmp := List.compareLex Name.cmp)).mpr hget, hf⟩

theorem norm_any_witness {s : NormLevel} {f : List Name → Node → Bool}
    (h : s.any f = true) : ∃ p n, s.get? p = some n ∧ f p n = true :=
  (norm_any_eq_true_iff s f).mp h

theorem norm_any_of_lookup {s : NormLevel} {f : List Name → Node → Bool}
    {p : List Name} {n : Node} (hget : s.get? p = some n) (hf : f p n = true) :
    s.any f = true :=
  (norm_any_eq_true_iff s f).mpr ⟨p, n, hget, hf⟩

theorem norm_any_congr_lookup (s t : NormLevel) (f : List Name → Node → Bool)
    (h : ∀ p, s.get? p = t.get? p) : s.any f = t.any f := by
  have he : s.any f = true ↔ t.any f = true := by
    rw [norm_any_eq_true_iff, norm_any_eq_true_iff]
    constructor
    · rintro ⟨p, n, hp, hf⟩
      exact ⟨p, n, (h p).symm.trans hp, hf⟩
    · rintro ⟨p, n, hp, hf⟩
      exact ⟨p, n, (h p).trans hp, hf⟩
  cases hs : s.any f <;> cases ht : t.any f <;> simp_all

end Vesper.Lean4LeanLevel42
