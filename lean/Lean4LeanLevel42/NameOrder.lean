import Lean4LeanLevel42.ListProofs

/-! Laws for the actual prefix-first `Name.cmp`, and pinned core list lifting.
The global `Ord Name` compares with `quickCmp` and is not used here. -/
namespace Vesper.Lean4LeanLevel42

open Lean

theorem name_cmp_refl (a : Name) : Name.cmp a a = .eq := by
  induction a with
  | anonymous => rfl
  | str p s ih => simp [Name.cmp, ih, Std.ReflCmp.compare_self]
  | num p n ih => simp [Name.cmp, ih, Std.ReflCmp.compare_self]

theorem name_cmp_swap (a b : Name) : Name.cmp a b = (Name.cmp b a).swap := by
  induction a generalizing b with
  | anonymous => cases b <;> rfl
  | str p s ih =>
    cases b with
    | anonymous => rfl
    | num q n => rfl
    | str q t =>
      have hp := ih q
      cases hq : Name.cmp q p <;>
        simp_all [Name.cmp]
      exact Std.OrientedCmp.eq_swap
  | num p n ih =>
    cases b with
    | anonymous => rfl
    | str q t => rfl
    | num q m =>
      have hp := ih q
      cases hq : Name.cmp q p <;>
        simp_all [Name.cmp]
      exact Std.OrientedCmp.eq_swap

theorem name_cmp_eq_iff (a b : Name) : Name.cmp a b = .eq ↔ a = b :=
  ⟨name_cmp_eq a b, fun h => h ▸ name_cmp_refl a⟩

/-- Lexicographic constructor step: the prefix proof is supplied recursively. -/
theorem name_cmp_suffix_lt_trans {α : Type} [Ord α] [Std.TransOrd α]
    {a b c : Name} {x y z : α}
    (hp : Name.cmp a b = .lt → Name.cmp b c = .lt → Name.cmp a c = .lt)
    (hab : (Name.cmp a b).then (compare x y) = .lt)
    (hbc : (Name.cmp b c).then (compare y z) = .lt) :
    (Name.cmp a c).then (compare x z) = .lt := by
  rcases Ordering.then_eq_lt.mp hab with hab | ⟨hab, hxy⟩
  · rcases Ordering.then_eq_lt.mp hbc with hbc | ⟨hbc, _⟩
    · simp [hp hab hbc]
    · have he := name_cmp_eq b c hbc
      subst c
      simp [hab]
  · have he := name_cmp_eq a b hab
    subst b
    rcases Ordering.then_eq_lt.mp hbc with hbc | ⟨hbc, hyz⟩
    · simp [hbc]
    · have he := name_cmp_eq a c hbc
      subst c
      simp [name_cmp_refl, Std.TransCmp.lt_trans hxy hyz]

theorem name_cmp_lt_trans {a b c : Name}
    (hab : Name.cmp a b = .lt) (hbc : Name.cmp b c = .lt) :
    Name.cmp a c = .lt := by
  induction a generalizing b c with
  | anonymous => cases b <;> cases c <;> simp_all [Name.cmp]
  | num a x ih =>
    cases b <;> cases c <;> try { simp_all [Name.cmp] }
    rename_i b y c z
    exact name_cmp_suffix_lt_trans (fun h₁ h₂ => ih h₁ h₂) hab hbc
  | str a x ih =>
    cases b <;> cases c <;> try { simp_all [Name.cmp] }
    rename_i b y c z
    exact name_cmp_suffix_lt_trans (fun h₁ h₂ => ih h₁ h₂) hab hbc

theorem name_cmp_lt_irrefl (a : Name) : Name.cmp a a ≠ .lt := by
  rw [name_cmp_refl]
  decide

theorem name_cmp_isLE_trans {a b c : Name}
    (hab : (Name.cmp a b).isLE) (hbc : (Name.cmp b c).isLE) :
    (Name.cmp a c).isLE := by
  cases ha : Name.cmp a b with
  | gt => simp [ha] at hab
  | eq =>
    have he := name_cmp_eq a b ha
    subst b
    exact hbc
  | lt =>
    cases hb : Name.cmp b c with
    | gt => simp [hb] at hbc
    | eq =>
      have he := name_cmp_eq b c hb
      subst c
      simp [ha]
    | lt => simp [name_cmp_lt_trans ha hb]

/-- Explicit comparator instances; these do not replace `Ord Name`. -/
theorem nameCmpTrans : Std.TransCmp Name.cmp where
  eq_swap := name_cmp_swap _ _
  isLE_trans := name_cmp_isLE_trans

theorem nameCmpLawfulEq : Std.LawfulEqCmp Name.cmp where
  compare_self := name_cmp_refl _
  eq_of_compare := name_cmp_eq _ _

attribute [instance] nameCmpTrans nameCmpLawfulEq

theorem path_cmp_trans : Std.TransCmp (List.compareLex Name.cmp) := inferInstance

theorem path_cmp_lawfulEq : Std.LawfulEqCmp (List.compareLex Name.cmp) := inferInstance

theorem path_cmp_eq_iff (a b : List Name) :
    List.compareLex Name.cmp a b = .eq ↔ a = b :=
  Std.LawfulEqCmp.compare_eq_iff_eq

theorem name_cmp_numeric_witness :
    Name.cmp (.num .anonymous 2) (.num .anonymous 10) = .lt := by decide

theorem name_cmp_string_witness :
    Name.cmp (.str .anonymous "alpha") (.str .anonymous "beta") = .lt := by decide

theorem name_cmp_prefix_witness :
    Name.cmp (.str (.num .anonymous 2) "z")
      (.str (.num .anonymous 10) "a") = .lt := by decide

theorem path_cmp_prefix_witness :
    List.compareLex Name.cmp [.num .anonymous 2]
      [.num .anonymous 2, .str .anonymous "a"] = .lt := by decide

end Vesper.Lean4LeanLevel42
