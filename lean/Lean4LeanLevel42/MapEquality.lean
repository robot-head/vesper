import Lean4LeanLevel42.MapAll

/-! Equality reflection for the actual transparent normalizer records and map
comparison. Lookup equality is extensional; no physical tree equality is used. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level.Normalize

/-- The actual derived VarNode comparison reflects both structural fields. -/
theorem varNode_beq_iff_eq (a b : VarNode) : (a == b) = true ↔ a = b := by
  cases a with
  | mk av ak =>
    cases b with
    | mk bv bk =>
      change (av == bv && ak == bk) = true ↔ _
      simp only [Bool.and_eq_true, beq_iff_eq, VarNode.mk.injEq]

theorem varNode_lawfulBEq : LawfulBEq VarNode where
  rfl {a} := (varNode_beq_iff_eq a a).mpr rfl
  eq_of_beq {a b} h := (varNode_beq_iff_eq a b).mp h

/-- The actual Node comparison reflects the constant and full variable list. -/
theorem node_beq_iff_eq (a b : Node) : (a == b) = true ↔ a = b := by
  letI := varNode_lawfulBEq
  cases a with
  | mk ac av =>
    cases b with
    | mk bc bv =>
      change (ac == bc && av == bv) = true ↔ _
      simp only [Bool.and_eq_true, beq_iff_eq, Node.mk.injEq]

theorem node_lawfulBEq : LawfulBEq Node where
  rfl {a} := (node_beq_iff_eq a a).mpr rfl
  eq_of_beq {a b} h := (node_beq_iff_eq a b).mp h

theorem nodeOption_beq_iff_eq (a b : Option Node) : (a == b) = true ↔ a = b := by
  letI := node_lawfulBEq
  exact beq_iff_eq

/-- Both actual all/get? traversals together express pointwise lookup equality. -/
theorem norm_beq_iff_lookup_eq (s t : NormLevel) :
    (s == t) = true ↔ ∀ p, s.get? p = t.get? p := by
  change ((s.all fun p n => t.get? p == some n) &&
    (t.all fun p n => s.get? p == some n)) = true ↔ _
  rw [Bool.and_eq_true, norm_all_eq_true_iff, norm_all_eq_true_iff]
  simp only [nodeOption_beq_iff_eq]
  constructor
  · intro ⟨hst, hts⟩ p
    cases hs : s.get? p with
    | none =>
      cases ht : t.get? p with
      | none => rfl
      | some n =>
        have h := hts p n ht
        rw [hs] at h
        contradiction
    | some n => exact (hst p n hs).symm
  · intro h
    constructor
    · intro p n hn
      exact (h p).symm.trans hn
    · intro p n hn
      exact (h p).trans hn

/-- The denotation depends only on actual lookups, without any shape premise. -/
theorem norm_lookup_eq_mapValue (ρ : Valuation) (s t : NormLevel)
    (h : ∀ p, s.get? p = t.get? p) : mapValue ρ s = mapValue ρ t := by
  apply Nat.le_antisymm
  · apply mapValue_mono
    intro p n hn
    exact ⟨n, (h p).symm.trans hn, Nat.le_refl _⟩
  · apply mapValue_mono
    intro p n hn
    exact ⟨n, (h p).trans hn, Nat.le_refl _⟩

/-- Actual normalized-map Boolean equality implies exact denotation equality. -/
theorem norm_beq_mapValue (ρ : Valuation) (s t : NormLevel)
    (h : (s == t) = true) : mapValue ρ s = mapValue ρ t := by
  exact norm_lookup_eq_mapValue ρ s t ((norm_beq_iff_lookup_eq s t).mp h)

end Vesper.Lean4LeanLevel42
