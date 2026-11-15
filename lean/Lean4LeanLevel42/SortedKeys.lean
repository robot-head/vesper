import Lean4LeanLevel42.NameOrder
import Lean4LeanLevel42.OrderedInsert
import Lean4LeanLevel42.VarInsert

/-! Strict name order for the actual insertion helpers. Variable offsets do
not participate in this invariant. No claim about every map key is made. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level Lean.Level.Normalize

def KeySorted (p : List Name) : Prop :=
  p.Pairwise (fun a b => Name.cmp a b = .lt)

def VarsSorted (xs : List VarNode) : Prop :=
  xs.Pairwise (fun a b => Name.cmp a.var b.var = .lt)

theorem keySorted_nodup {p : List Name} (hp : KeySorted p) : p.Nodup := by
  apply List.Pairwise.imp (R := fun a b => Name.cmp a b = .lt) _ hp
  intro a b hab he
  subst b
  have hr := Std.ReflCmp.compare_self (cmp := Name.cmp) (a := a)
  simp [hr] at hab

theorem orderedInsert_sorted {v : Name} {p p' : List Name}
    (hp : KeySorted p) (h : Normalize.orderedInsert Name.cmp v p = some p') :
    KeySorted p' := by
  induction p generalizing p' with
  | nil =>
    simp only [Normalize.orderedInsert, Option.some.injEq] at h
    subst p'
    simp [KeySorted]
  | cons b bs ih =>
    rcases List.pairwise_cons.mp hp with ⟨hb, hbs⟩
    cases hc : Name.cmp v b with
    | lt =>
      simp only [Normalize.orderedInsert, hc, Option.some.injEq] at h
      subst p'
      apply List.pairwise_cons.mpr
      refine ⟨?_, hp⟩
      intro x hx
      rcases List.mem_cons.mp hx with he | hx
      · simpa [he] using hc
      · exact Std.TransCmp.lt_trans (cmp := Name.cmp) hc (hb x hx)
    | eq => simp [Normalize.orderedInsert, hc] at h
    | gt =>
      cases hr : Normalize.orderedInsert Name.cmp v bs with
      | none => simp [Normalize.orderedInsert, hc, hr] at h
      | some q =>
        simp only [Normalize.orderedInsert, hc, hr, Option.map_some, Option.some.injEq] at h
        subst p'
        apply List.pairwise_cons.mpr
        refine ⟨?_, ih hbs hr⟩
        intro x hx
        rcases (orderedInsert_some_mem hr).mp hx with he | hx
        · subst x
          exact Std.OrientedCmp.lt_of_gt (cmp := Name.cmp) hc
        · exact hb x hx

theorem orderedInsert_none_iff_mem {v : Name} {p : List Name} (hp : KeySorted p) :
    Normalize.orderedInsert Name.cmp v p = none ↔ v ∈ p := by
  constructor
  · exact orderedInsert_none_mem
  · intro hv
    induction p with
    | nil => simp at hv
    | cons b bs ih =>
      rcases List.pairwise_cons.mp hp with ⟨hb, hbs⟩
      cases hc : Name.cmp v b with
      | lt =>
        rcases List.mem_cons.mp hv with he | hv
        · subst v
          have hr := Std.ReflCmp.compare_self (cmp := Name.cmp) (a := b)
          simp [hr] at hc
        · have hrev := Std.OrientedCmp.gt_of_lt (cmp := Name.cmp) (hb v hv)
          simp [hc] at hrev
      | eq => simp [Normalize.orderedInsert, hc]
      | gt =>
        have hvbs : v ∈ bs := by
          rcases List.mem_cons.mp hv with he | hv
          · subst v
            have hr := Std.ReflCmp.compare_self (cmp := Name.cmp) (a := b)
            simp [hr] at hc
          · exact hv
        simp [Normalize.orderedInsert, hc, ih hbs hvbs]

theorem orderedInsert_sorted_nodup {v : Name} {p p' : List Name}
    (hp : KeySorted p) (h : Normalize.orderedInsert Name.cmp v p = some p') :
    p'.Nodup :=
  keySorted_nodup (orderedInsert_sorted hp h)

theorem addVar_sorted (v : Name) (k : Nat) {xs : List VarNode} (hx : VarsSorted xs) :
    VarsSorted (VarNode.addVar v k xs) := by
  induction xs with
  | nil => simp [VarNode.addVar, VarsSorted]
  | cons w ws ih =>
    rcases List.pairwise_cons.mp hx with ⟨hw, hws⟩
    cases hc : Name.cmp v w.var with
    | lt =>
      simp only [VarNode.addVar, hc]
      apply List.pairwise_cons.mpr
      refine ⟨?_, hx⟩
      intro x hxm
      rcases List.mem_cons.mp hxm with he | hxm
      · subst x
        exact hc
      · exact Std.TransCmp.lt_trans (cmp := Name.cmp) hc (hw x hxm)
    | eq =>
      simp only [VarNode.addVar, hc]
      apply List.pairwise_cons.mpr
      refine ⟨?_, hws⟩
      intro x hxm
      have he := name_cmp_eq v w.var hc
      simpa only [he] using hw x hxm
    | gt =>
      simp only [VarNode.addVar, hc]
      apply List.pairwise_cons.mpr
      refine ⟨?_, ih hws⟩
      intro x hxm
      rcases (addVar_names v k ws x.var).mp ⟨x, hxm, rfl⟩ with he | he
      · rw [he]
        exact Std.OrientedCmp.lt_of_gt (cmp := Name.cmp) hc
      · rcases he with ⟨y, hym, hy⟩
        rw [← hy]
        exact hw y hym

theorem varsSorted_names_nodup {xs : List VarNode} (hx : VarsSorted xs) :
    (xs.map VarNode.var).Nodup := by
  apply keySorted_nodup
  exact List.pairwise_map.mpr hx

theorem addVar_sorted_names_nodup (v : Name) (k : Nat) {xs : List VarNode}
    (hx : VarsSorted xs) : (VarNode.addVar v k xs |>.map VarNode.var).Nodup :=
  varsSorted_names_nodup (addVar_sorted v k hx)

/-- Insertion really traverses a strict numeric key and preserves its order. -/
theorem orderedInsert_numeric_sorted :
    let z : Name := .num .anonymous 0
    let o : Name := .num .anonymous 1
    let t : Name := .num .anonymous 2
    KeySorted [z, t] ∧
      Normalize.orderedInsert Name.cmp o [z, t] = some [z, o, t] ∧
      KeySorted [z, o, t] := by
  exact ⟨by simp [KeySorted, Name.cmp]; decide, rfl,
    by simp [KeySorted, Name.cmp]; decide⟩

/-- A duplicate beyond the first entry is detected on a sorted numeric key. -/
theorem orderedInsert_numeric_duplicate :
    let z : Name := .num .anonymous 0
    let t : Name := .num .anonymous 2
    KeySorted [z, t] ∧ Normalize.orderedInsert Name.cmp t [z, t] = none := by
  exact ⟨by simp [KeySorted, Name.cmp]; decide, rfl⟩

/-- Numeric name equality merges offsets while retaining strict name order. -/
theorem addVar_numeric_sorted_merge :
    let z : Name := .num .anonymous 0
    let o : Name := .num .anonymous 1
    let xs : List VarNode := [⟨z, 2⟩, ⟨o, 7⟩]
    VarsSorted xs ∧
      VarNode.addVar z 5 xs = [⟨z, 5⟩, ⟨o, 7⟩] ∧
      VarsSorted (VarNode.addVar z 5 xs) ∧
      (VarNode.addVar z 5 xs |>.map VarNode.var).Nodup := by
  dsimp only
  have hs : VarsSorted [⟨.num .anonymous 0, 2⟩, ⟨.num .anonymous 1, 7⟩] := by
    simp [VarsSorted, Name.cmp]
    decide
  exact ⟨hs, rfl, addVar_sorted _ _ hs, addVar_sorted_names_nodup _ _ hs⟩

/-- Sorting is a real precondition: the actual helper can insert a duplicate
before an unsorted key's existing copy. -/
theorem orderedInsert_unsorted_key_witness :
    let z : Name := .num .anonymous 0
    let o : Name := .num .anonymous 1
    ¬ KeySorted [o, z] ∧
      Normalize.orderedInsert Name.cmp z [o, z] = some [z, o, z] ∧
      ¬ KeySorted [z, o, z] := by
  exact ⟨by simp [KeySorted, Name.cmp]; decide, rfl,
    by simp [KeySorted, Name.cmp]⟩

end Vesper.Lean4LeanLevel42
