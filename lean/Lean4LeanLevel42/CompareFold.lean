import Lean4LeanLevel42.NodeResidual

/-! Soundness of the actual comparison step and its short-circuiting Option
fold. Only dominator rows need their condition invariant. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level.Normalize

/-- The exact step used by the actual `NormLevel.le` fold. -/
abbrev compareStep (p : List Name) (n : Node)
    (q : List Name) (m : Node) : Option Node :=
  if Lean.Level.Normalize.subset Name.cmp q p then
    let r := n.subsumeBy false m
    if r.isEmpty then none else some r
  else some n

/-- A real early exit leaves no outstanding sublevel. -/
theorem compareStep_none_bound (ρ : Valuation) (p q : List Name) (n m : Node)
    (hw : NodeWF q m) (h : compareStep p n q m = none) :
    nodeValue ρ p n ≤ nodeValue ρ q m := by
  unfold compareStep at h
  split at h
  next hs =>
    dsimp only at h
    split at h
    next he =>
      have hv := node_subsumeBy_union ρ false p q n m (subset_sound hs) hw
      have hz := node_isEmpty_value_zero ρ p (n.subsumeBy false m) he
      rw [hz] at hv
      omega
    next => contradiction
  next => contradiction

/-- A nonterminal actual step preserves union with its dominator. -/
theorem compareStep_some_union (ρ : Valuation) (p q : List Name) (n m r : Node)
    (hw : NodeWF q m) (h : compareStep p n q m = some r) :
    max (nodeValue ρ p r) (nodeValue ρ q m) =
      max (nodeValue ρ p n) (nodeValue ρ q m) := by
  unfold compareStep at h
  split at h
  next hs =>
    dsimp only at h
    split at h
    next => contradiction
    next =>
      cases h
      exact node_subsumeBy_union ρ false p q n m (subset_sound hs) hw
  next =>
    cases h
    rfl

/-- Success of the actual Option fold bounds the original outstanding node.
An empty row list cannot return `none`; each successful exit comes from an
actual empty residual, and surviving residuals continue through the tail. -/
theorem compareFold_none_bound (ρ : Valuation) (p : List Name)
    (rows : List (List Name × Node)) (n : Node) (B : Nat)
    (hw : ∀ row ∈ rows, NodeWF row.1 row.2)
    (hb : ∀ row ∈ rows, nodeValue ρ row.1 row.2 ≤ B)
    (h : rows.foldlM (fun r row => compareStep p r row.1 row.2) n = none) :
    nodeValue ρ p n ≤ B := by
  induction rows generalizing n with
  | nil => simp at h
  | cons row rows ih =>
    have hwr : NodeWF row.1 row.2 := hw row (by simp)
    have hbr : nodeValue ρ row.1 row.2 ≤ B := hb row (by simp)
    have hwt : ∀ row ∈ rows, NodeWF row.1 row.2 := by
      intro row hr
      exact hw row (by simp [hr])
    have hbt : ∀ row ∈ rows, nodeValue ρ row.1 row.2 ≤ B := by
      intro row hr
      exact hb row (by simp [hr])
    cases hs : compareStep p n row.1 row.2 with
    | none =>
      exact Nat.le_trans (compareStep_none_bound ρ p row.1 n row.2 hwr hs) hbr
    | some r =>
      have ht : rows.foldlM (fun r row => compareStep p r row.1 row.2) r = none := by
        simpa only [List.foldlM_cons, hs, Option.bind_some] using h
      have hres := ih r hwt hbt ht
      have hu := compareStep_some_union ρ p row.1 n row.2 r hwr hs
      omega

end Vesper.Lean4LeanLevel42
