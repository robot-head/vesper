import Lean4LeanLevel42.NameOrder
import Std.Data.TreeMap.Lemmas

/-! Denotation of the actual normalizer map, including overwrite semantics.
The comparator captured by `NormLevel` is `List.compareLex Name.cmp`. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level.Normalize

def mapValue (ρ : Valuation) (s : NormLevel) : Nat :=
  s.foldl (fun z p n => max z (nodeValue ρ p n)) 0

def MapWF (s : NormLevel) : Prop :=
  ∀ p n, s.get? p = some n → NodeWF p n

theorem map_get_empty (p : List Name) :
    ({} : NormLevel).get? p = none := by
  exact Std.TreeMap.getElem?_emptyc (cmp := List.compareLex Name.cmp)

theorem map_get_insert (s : NormLevel) (p q : List Name) (n : Node) :
    (s.insert p n).get? q = if p = q then some n else s.get? q := by
  calc
    (s.insert p n).get? q =
        if List.compareLex Name.cmp p q = .eq then some n else s.get? q :=
      Std.TreeMap.getElem?_insert (cmp := List.compareLex Name.cmp)
    _ = _ := by simp only [path_cmp_eq_iff]

theorem map_get_erase (s : NormLevel) (p q : List Name) :
    (s.erase p).get? q = if p = q then none else s.get? q := by
  calc
    (s.erase p).get? q =
        if List.compareLex Name.cmp p q = .eq then none else s.get? q :=
      Std.TreeMap.getElem?_erase (cmp := List.compareLex Name.cmp)
    _ = _ := by simp only [path_cmp_eq_iff]

theorem maxfold_le {α : Type} (f : α → Nat) (xs : List α) (z b : Nat)
    (hz : z ≤ b) (h : ∀ x ∈ xs, f x ≤ b) :
    xs.foldl (fun a x => max a (f x)) z ≤ b := by
  induction xs generalizing z with
  | nil => exact hz
  | cons x xs ih =>
    apply ih (max z (f x))
    · exact Nat.max_le.mpr ⟨hz, h x (by simp)⟩
    · intro y hy
      exact h y (by simp [hy])

theorem le_maxfold {α : Type} (f : α → Nat) (xs : List α) (z : Nat) :
    z ≤ xs.foldl (fun a x => max a (f x)) z := by
  induction xs generalizing z with
  | nil => exact Nat.le_refl _
  | cons x xs ih => exact Nat.le_trans (Nat.le_max_left _ _) (ih _)

theorem mem_le_maxfold {α : Type} (f : α → Nat) (xs : List α) (z : Nat)
    {x : α} (h : x ∈ xs) :
    f x ≤ xs.foldl (fun a x => max a (f x)) z := by
  induction xs generalizing z with
  | nil => simp at h
  | cons y ys ih =>
    simp only [List.mem_cons] at h
    rcases h with h | h
    · subst x
      exact Nat.le_trans (Nat.le_max_right _ _) (le_maxfold f ys _)
    · exact ih _ h

theorem mapValue_empty (ρ : Valuation) : mapValue ρ ({} : NormLevel) = 0 := by
  rfl

theorem nodeValue_le_mapValue {ρ : Valuation} {s : NormLevel}
    {p : List Name} {n : Node} (h : s.get? p = some n) :
    nodeValue ρ p n ≤ mapValue ρ s := by
  unfold mapValue
  rw [Std.TreeMap.foldl_eq_foldl_toList]
  exact mem_le_maxfold (fun a => nodeValue ρ a.1 a.2) s.toList 0
    ((Std.TreeMap.mem_toList_iff_getElem?_eq_some
      (cmp := List.compareLex Name.cmp)).mpr h)

theorem mapValue_le {ρ : Valuation} {s : NormLevel} {b : Nat}
    (h : ∀ p n, s.get? p = some n → nodeValue ρ p n ≤ b) :
    mapValue ρ s ≤ b := by
  unfold mapValue
  rw [Std.TreeMap.foldl_eq_foldl_toList]
  apply maxfold_le (fun a => nodeValue ρ a.1 a.2) s.toList 0 b (Nat.zero_le _)
  intro a ha
  exact h a.1 a.2 ((Std.TreeMap.mem_toList_iff_getElem?_eq_some
    (cmp := List.compareLex Name.cmp)).mp ha)

theorem mapValue_mono {ρ : Valuation} {s t : NormLevel}
    (h : ∀ p n, s.get? p = some n →
      ∃ m, t.get? p = some m ∧ nodeValue ρ p n ≤ nodeValue ρ p m) :
    mapValue ρ s ≤ mapValue ρ t := by
  apply mapValue_le
  intro p n hn
  obtain ⟨m, hm, hle⟩ := h p n hn
  exact Nat.le_trans hle (nodeValue_le_mapValue hm)

theorem mapValue_erase_le (ρ : Valuation) (s : NormLevel) (p : List Name) :
    mapValue ρ (s.erase p) ≤ mapValue ρ s := by
  apply mapValue_mono
  intro q n hn
  rw [map_get_erase] at hn
  split at hn
  · contradiction
  · exact ⟨n, hn, Nat.le_refl _⟩

theorem mapValue_insert (ρ : Valuation) (s : NormLevel) (p : List Name) (n : Node) :
    mapValue ρ (s.insert p n) =
      max (mapValue ρ (s.erase p)) (nodeValue ρ p n) := by
  apply Nat.le_antisymm
  · apply mapValue_le
    intro q m hm
    rw [map_get_insert] at hm
    split at hm
    next he =>
      subst q
      cases hm
      exact Nat.le_max_right _ _
    next he =>
      have hmem : (s.erase p).get? q = some m := by
        rw [map_get_erase, if_neg he]
        exact hm
      exact Nat.le_trans (nodeValue_le_mapValue hmem) (Nat.le_max_left _ _)
  · apply Nat.max_le.mpr
    constructor
    · apply mapValue_mono
      intro q m hm
      rw [map_get_erase] at hm
      split at hm
      · contradiction
      next he =>
        refine ⟨m, ?_, Nat.le_refl _⟩
        rw [map_get_insert, if_neg he]
        exact hm
    · apply nodeValue_le_mapValue
      rw [map_get_insert, if_pos rfl]

theorem mapValue_decompose (ρ : Valuation) (s : NormLevel) (p : List Name) :
    mapValue ρ s = max (mapValue ρ (s.erase p))
      (((s.get? p).map (nodeValue ρ p)).getD 0) := by
  apply Nat.le_antisymm
  · apply mapValue_le
    intro q n hn
    by_cases he : p = q
    · subst q
      rw [hn]
      exact Nat.le_max_right _ _
    · have hmem : (s.erase p).get? q = some n := by
        rw [map_get_erase, if_neg he]
        exact hn
      exact Nat.le_trans (nodeValue_le_mapValue hmem) (Nat.le_max_left _ _)
  · apply Nat.max_le.mpr
    constructor
    · exact mapValue_erase_le ρ s p
    · cases hn : s.get? p with
      | none => exact Nat.zero_le _
      | some n => exact nodeValue_le_mapValue hn

theorem mapWF_empty : MapWF ({} : NormLevel) := by
  intro p n h
  rw [map_get_empty] at h
  contradiction

end Vesper.Lean4LeanLevel42
