import Lean4LeanLevel42.SortedKeys
import Lean4LeanLevel42.NormalizeValue
import Lean4LeanLevel42.MapSubsumption

/-! Shape predicates for the actual finite normalizer maps. These are separate
from row well-formedness and impose no semantic valuation assumptions. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level.Normalize

def MapKeysSorted (s : NormLevel) : Prop :=
  ∀ p n, s.get? p = some n → KeySorted p

def MapVarsSorted (s : NormLevel) : Prop :=
  ∀ p n, s.get? p = some n → VarsSorted n.var

def MapNoEmpty (s : NormLevel) : Prop :=
  ∀ p n, s.get? p = some n → n.isEmpty = false

end Vesper.Lean4LeanLevel42
