import Lean4LeanLevel42.MapValue

/-! The real short-circuit traversal behind the actual normalizer map's all.
The lookup bridge uses the actual map's list traversal and comparator laws. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level.Normalize

theorem list_forIn_all_eq {α : Type} (xs : List α) (f : α → Bool) :
    (Id.run do
      for x in xs do
        if f x = false then return false
      return true) = xs.all f := by
  induction xs with
  | nil => rfl
  | cons x xs ih =>
    cases hf : f x with
    | false => simp [List.forIn_cons, hf, List.all_cons]
    | true => simpa [List.forIn_cons, hf, List.all_cons] using ih

theorem norm_all_eq_toList_all (s : NormLevel) (f : List Name → Node → Bool) :
    s.all f = s.toList.all (fun row => f row.1 row.2) := by
  change (Id.run do
    for row in s do
      if f row.1 row.2 = false then return false
    return true) = _
  rw [Std.TreeMap.forIn_eq_forIn_toList]
  exact list_forIn_all_eq s.toList (fun row => f row.1 row.2)

theorem norm_all_eq_true_iff (s : NormLevel) (f : List Name → Node → Bool) :
    s.all f = true ↔ ∀ p n, s.get? p = some n → f p n = true := by
  rw [norm_all_eq_toList_all, List.all_eq_true]
  constructor
  · intro h p n hn
    exact h (p, n) ((Std.TreeMap.mem_toList_iff_getElem?_eq_some
      (cmp := List.compareLex Name.cmp)).mpr hn)
  · intro h row hr
    exact h row.1 row.2 ((Std.TreeMap.mem_toList_iff_getElem?_eq_some
      (cmp := List.compareLex Name.cmp)).mp hr)

theorem norm_all_lookup {s : NormLevel} {f : List Name → Node → Bool}
    {p : List Name} {n : Node}
    (hall : s.all f = true) (h : s.get? p = some n) : f p n = true :=
  (norm_all_eq_true_iff s f).mp hall p n h

end Vesper.Lean4LeanLevel42
