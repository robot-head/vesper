import Lean4LeanLevel42.SortedKeys

/-! Flat child-entry facts for the actual tree updater. This module adds only
an observation through List.find?, not an updater or recursive tree predicate. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level Lean.Level.Normalize

universe u

def treeEntriesLookup {α : Type u} (n : Name) (xs : List (Name × α)) : Option α :=
  (xs.find? (fun row => row.1 == n)).map Prod.snd

theorem treeEntriesLookup_cons {α : Type u} (n m : Name) (v : α)
    (xs : List (Name × α)) :
    treeEntriesLookup n ((m, v) :: xs) =
      if m = n then some v else treeEntriesLookup n xs := by
  by_cases he : m = n <;> simp [treeEntriesLookup, he]

theorem treeEntriesLookup_none_of_not_mem {α : Type u} (n : Name)
    (xs : List (Name × α)) (hn : n ∉ xs.map Prod.fst) :
    treeEntriesLookup n xs = none := by
  induction xs with
  | nil => rfl
  | cons row xs ih =>
    rcases row with ⟨m, v⟩
    have hne : m ≠ n := by
      intro he
      subst m
      exact hn (by simp)
    have ht : n ∉ xs.map Prod.fst := by
      intro hm
      exact hn (by simp [hm])
    rw [treeEntriesLookup_cons, if_neg hne]
    exact ih ht

/-- Key projection agrees with the actual ordered insertion even on raw lists. -/
theorem modifyAt_keys {α : Type u} [Inhabited α] (f : α → α) (n : Name)
    (xs : List (Name × α)) :
    (Normalize.modifyAt f n xs).map Prod.fst =
      (Normalize.orderedInsert Name.cmp n (xs.map Prod.fst)).getD (xs.map Prod.fst) := by
  induction xs with
  | nil => rfl
  | cons row xs ih =>
    rcases row with ⟨m, v⟩
    cases hc : Name.cmp n m with
    | lt => simp [Normalize.modifyAt, Normalize.orderedInsert, hc]
    | eq => simp [Normalize.modifyAt, Normalize.orderedInsert, hc]
    | gt =>
      simp only [Normalize.modifyAt, hc, List.map_cons, Normalize.orderedInsert]
      rw [ih]
      cases Normalize.orderedInsert Name.cmp n (xs.map Prod.fst) <;> rfl

/-- Updating or inserting n cannot change the first lookup of a distinct name. -/
theorem modifyAt_lookup_other {α : Type u} [Inhabited α] (f : α → α)
    (n m : Name) (xs : List (Name × α)) (hne : m ≠ n) :
    treeEntriesLookup m (Normalize.modifyAt f n xs) = treeEntriesLookup m xs := by
  induction xs with
  | nil => simp [Normalize.modifyAt, Ne.symm hne, treeEntriesLookup]
  | cons row xs ih =>
    rcases row with ⟨a, v⟩
    cases hc : Name.cmp n a with
    | lt => simp [Normalize.modifyAt, hc, treeEntriesLookup_cons, Ne.symm hne]
    | eq =>
      have he : n = a := name_cmp_eq n a hc
      subst a
      simp [Normalize.modifyAt, hc, treeEntriesLookup_cons, Ne.symm hne]
    | gt =>
      simp only [Normalize.modifyAt, hc, treeEntriesLookup_cons]
      rw [ih]

/-- The actual updater preserves strict prefix-first order of child names. -/
theorem modifyAt_keysSorted {α : Type u} [Inhabited α] (f : α → α) (n : Name)
    (xs : List (Name × α)) (hxs : KeySorted (xs.map Prod.fst)) :
    KeySorted ((Normalize.modifyAt f n xs).map Prod.fst) := by
  rw [modifyAt_keys]
  cases hi : Normalize.orderedInsert Name.cmp n (xs.map Prod.fst) with
  | none => exact hxs
  | some keys => exact orderedInsert_sorted hxs hi

/-- Sortedness rules out an earlier insertion before an existing target. -/
theorem modifyAt_lookup_target {α : Type u} [Inhabited α] (f : α → α) (n : Name)
    (xs : List (Name × α)) (hxs : KeySorted (xs.map Prod.fst)) :
    treeEntriesLookup n (Normalize.modifyAt f n xs) =
      some (f ((treeEntriesLookup n xs).getD default)) := by
  induction xs with
  | nil => simp [Normalize.modifyAt, treeEntriesLookup]
  | cons row xs ih =>
    rcases row with ⟨a, v⟩
    change KeySorted (a :: xs.map Prod.fst) at hxs
    rcases List.pairwise_cons.mp hxs with ⟨ha, ht⟩
    cases hc : Name.cmp n a with
    | lt =>
      have hnot : n ∉ ((a, v) :: xs).map Prod.fst := by
        intro hn
        simp only [List.map_cons, List.mem_cons] at hn
        rcases hn with he | hn
        · subst n
          simp [name_cmp_refl] at hc
        · have hgt := Std.OrientedCmp.gt_of_lt (cmp := Name.cmp) hc
          have hlt := ha n hn
          simp [hlt] at hgt
      rw [treeEntriesLookup_none_of_not_mem n ((a, v) :: xs) hnot]
      simp [Normalize.modifyAt, hc, treeEntriesLookup_cons]
    | eq =>
      have he : n = a := name_cmp_eq n a hc
      subst n
      simp [Normalize.modifyAt, hc, treeEntriesLookup_cons]
    | gt =>
      have hne : a ≠ n := by
        intro he
        subst n
        simp [name_cmp_refl] at hc
      simp only [Normalize.modifyAt, hc, treeEntriesLookup_cons, if_neg hne]
      exact ih ht

/-- The real unsorted updater inserts a fresh default-derived entry before a
later existing target: the observed new value is one, not successor two. -/
theorem modifyAt_unsorted_target_witness :
    let z : Name := .num .anonymous 0
    let o : Name := .num .anonymous 1
    let xs : List (Name × Nat) := [(o, 1), (z, 2)]
    ¬KeySorted (xs.map Prod.fst) ∧
      treeEntriesLookup z xs = some 2 ∧
      Normalize.modifyAt Nat.succ z xs = [(z, 1), (o, 1), (z, 2)] ∧
      treeEntriesLookup z (Normalize.modifyAt Nat.succ z xs) = some 1 ∧
      treeEntriesLookup z (Normalize.modifyAt Nat.succ z xs) ≠
        some (Nat.succ ((treeEntriesLookup z xs).getD default)) := by
  dsimp only
  refine ⟨?_, rfl, rfl, rfl, ?_⟩
  · simp [KeySorted, Name.cmp] <;> decide
  · decide

end Vesper.Lean4LeanLevel42
