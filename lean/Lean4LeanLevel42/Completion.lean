import Lean4LeanLevel42.MapValue

/-! Combinatorial reconstruction certificates, independent of search success
and denotation correctness. Stored offsets are deliberately unrestricted. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level.Normalize

def EdgeCovered (s : NormLevel) (acc : List Name) (a : Name) : Prop :=
  ∃ p n k, s.get? p = some n ∧ (⟨a, k⟩ : VarNode) ∈ n.var ∧ p ⊆ a :: acc

def Admissible (s : NormLevel) (acc : List Name) : List Name → Prop
  | [] => True
  | a :: rest => EdgeCovered s acc a ∧ Admissible s (a :: acc) rest

def Completes (s : NormLevel) (acc rem : List Name) : Prop :=
  ∃ order, order.Perm rem ∧ Admissible s acc order

def MapCompletes (s : NormLevel) : Prop :=
  ∀ p n, s.get? p = some n → Completes s [] p

def VarSupport (s t : NormLevel) : Prop :=
  ∀ p n a k, s.get? p = some n → (⟨a, k⟩ : VarNode) ∈ n.var →
    ∃ q m j, t.get? q = some m ∧ (⟨a, j⟩ : VarNode) ∈ m.var ∧ q ⊆ p

end Vesper.Lean4LeanLevel42
