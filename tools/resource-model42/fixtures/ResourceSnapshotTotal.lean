import Std

/-! Ordinary total recursion deliberately generates compiler implementation
helpers. The kernel-facing definitions and theorems remain total. -/
namespace ResourceSnapshotTotal

def total : Nat → Nat
  | 0 => 0
  | n + 1 => total n + (n + 1)

theorem total_correct : total 4 = 10 := rfl

inductive Tree where
  | leaf
  | branch (left right : Tree)

def leaves : Tree → Nat
  | .leaf => 1
  | .branch left right => leaves left + leaves right

theorem tree_correct : leaves (.branch .leaf (.branch .leaf .leaf)) = 3 := rfl

end ResourceSnapshotTotal
