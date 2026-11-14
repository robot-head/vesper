import Lean4Lean.Level

/-! Denotations for the actual pinned normalizer's node helpers. These definitions
do not assume that arbitrary raw nodes satisfy the condition invariant. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level.Normalize

abbrev Valuation := Name → Nat

def enabled (ρ : Valuation) (p : List Name) : Bool :=
  p.all (fun v => decide (0 < ρ v))

def varsValue (ρ : Valuation) (vs : List VarNode) : Nat :=
  vs.foldr (fun v acc => max (ρ v.var + v.offset) acc) 0

def nodeValue (ρ : Valuation) (p : List Name) (n : Node) : Nat :=
  if enabled ρ p then max n.const (varsValue ρ n.var) else 0

def NodeWF (p : List Name) (n : Node) : Prop :=
  ∀ v ∈ n.var, v.var ∈ p

theorem enabled_eq_true_iff {ρ : Valuation} {p : List Name} :
    enabled ρ p = true ↔ ∀ v ∈ p, 0 < ρ v := by
  simp [enabled, List.all_eq_true]

theorem enabled_of_subset {ρ : Valuation} {p₁ p₂ : List Name}
    (h : p₂ ⊆ p₁) (hp : enabled ρ p₁ = true) : enabled ρ p₂ = true := by
  apply enabled_eq_true_iff.mpr
  intro v hv
  exact enabled_eq_true_iff.mp hp v (h hv)

theorem varsValue_nil (ρ : Valuation) : varsValue ρ [] = 0 := rfl

theorem varsValue_cons (ρ : Valuation) (v : VarNode) (vs : List VarNode) :
    varsValue ρ (v :: vs) = max (ρ v.var + v.offset) (varsValue ρ vs) := rfl

theorem varsValue_mem_bound {ρ : Valuation} {vs : List VarNode} {v : VarNode}
    (hv : v ∈ vs) : ρ v.var + v.offset ≤ varsValue ρ vs := by
  induction vs with
  | nil => simp at hv
  | cons w ws ih =>
    simp only [List.mem_cons] at hv
    rw [varsValue_cons]
    rcases hv with h | h
    · subst v
      exact Nat.le_max_left _ _
    · exact Nat.le_trans (ih h) (Nat.le_max_right _ _)

theorem varsValue_append (ρ : Valuation) (as bs : List VarNode) :
    varsValue ρ (as ++ bs) = max (varsValue ρ as) (varsValue ρ bs) := by
  induction as with
  | nil => simp [varsValue_nil]
  | cons a as ih => simp only [List.cons_append, varsValue_cons, ih, Nat.max_assoc]

theorem nodeValue_of_enabled {ρ : Valuation} {p : List Name} {n : Node}
    (hp : enabled ρ p = true) :
    nodeValue ρ p n = max n.const (varsValue ρ n.var) := by
  simp [nodeValue, hp]

theorem nodeValue_of_disabled {ρ : Valuation} {p : List Name} {n : Node}
    (hp : enabled ρ p = false) : nodeValue ρ p n = 0 := by
  simp [nodeValue, hp]

theorem NodeWF_positive {ρ : Valuation} {p : List Name} {n : Node}
    (hw : NodeWF p n) (hp : enabled ρ p = true) :
    ∀ v ∈ n.var, 0 < ρ v.var := by
  intro v hv
  exact enabled_eq_true_iff.mp hp v.var (hw v hv)

end Vesper.Lean4LeanLevel42
