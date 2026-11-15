import Lean4LeanLevel42.Subsumption
import Lean4LeanLevel42.NameOrder

/-! Residual structure and the variable invariant needed by actual self steps. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level Lean.Level.Normalize

def nodeVarsValue (ρ : Valuation) (p : List Name) (n : Node) : Nat :=
  if enabled ρ p then varsValue ρ n.var else 0

theorem subsumeVars_sublist (xs ys : List VarNode) :
    (Normalize.subsumeVars xs ys).Sublist xs := by
  cases xs with
  | nil => simp [Normalize.subsumeVars]
  | cons x xs =>
    cases ys with
    | nil => simp [Normalize.subsumeVars]
    | cons y ys =>
      cases hc : Name.cmp x.var y.var with
      | lt =>
        simpa only [Normalize.subsumeVars, hc] using
          (subsumeVars_sublist xs (y :: ys)).cons₂ x
      | eq =>
        by_cases ho : x.offset ≤ y.offset
        · simpa only [Normalize.subsumeVars, hc, if_pos ho] using
            (subsumeVars_sublist xs ys).cons x
        · simpa only [Normalize.subsumeVars, hc, if_neg ho] using
            (subsumeVars_sublist xs ys).cons₂ x
      | gt =>
        simpa only [Normalize.subsumeVars, hc] using
          subsumeVars_sublist (x :: xs) ys
termination_by xs.length + ys.length
decreasing_by all_goals (simp_all <;> omega)

theorem node_subsumeBy_var_sublist (same : Bool) (n1 n2 : Node) :
    (n1.subsumeBy same n2).var.Sublist n1.var := by
  unfold Node.subsumeBy
  split <;> split
  · exact List.Sublist.refl _
  · exact subsumeVars_sublist _ _
  · exact List.Sublist.refl _
  · exact subsumeVars_sublist _ _

theorem node_subsume_var_sublist (p1 p2 : List Name) (n1 n2 : Node) :
    (Node.subsume p1 n1 p2 n2).var.Sublist n1.var := by
  unfold Node.subsume
  split
  · exact node_subsumeBy_var_sublist _ _ _
  · exact List.Sublist.refl _

theorem node_subsume_WF (p1 p2 : List Name) (n1 n2 : Node)
    (hw : NodeWF p1 n1) : NodeWF p1 (Node.subsume p1 n1 p2 n2) := by
  intro v hv
  exact hw v ((node_subsume_var_sublist p1 p2 n1 n2).subset hv)

theorem node_isEmpty_value_zero (ρ : Valuation) (p : List Name) (n : Node)
    (h : n.isEmpty = true) : nodeValue ρ p n = 0 := by
  have hh : n.const = 0 ∧ n.var = [] := by simpa [Node.isEmpty] using h
  simp [nodeValue, hh.1, hh.2, varsValue]

/-- Gated variables have a union law without a well-formedness premise. -/
theorem node_subsume_vars_union (ρ : Valuation) (p1 p2 : List Name)
    (n1 n2 : Node) :
    max (nodeVarsValue ρ p1 (Node.subsume p1 n1 p2 n2)) (nodeValue ρ p2 n2) =
      max (nodeVarsValue ρ p1 n1) (nodeValue ρ p2 n2) := by
  unfold Node.subsume
  split
  next hs =>
    change subset Name.cmp p2 p1 = true at hs
    by_cases he : enabled ρ p1 = true
    · have he2 := enabled_of_subset (subset_sound hs) he
      have hv := node_subsumeBy_vars_union ρ (p1.length == p2.length) n1 n2
      simp only [nodeVarsValue, nodeValue, he, he2, ite_true]
      omega
    · simp [nodeVarsValue, eq_false_of_ne_true he]
  next => rfl

/-- At a self row, the old variables may discharge a residual constant only
when those variables remain covered by residual variables or the context. -/
theorem node_subsumeBy_same_context (ρ : Valuation) (p : List Name)
    (original r : Node) (C : Nat) (hw : NodeWF p original)
    (hv : nodeVarsValue ρ p original ≤ max (nodeVarsValue ρ p r) C) :
    max (nodeValue ρ p (r.subsumeBy true original)) C =
      max (nodeValue ρ p r) C := by
  by_cases he : enabled ρ p = true
  · have hp := NodeWF_positive hw he
    simp only [nodeVarsValue, he, ite_true] at hv
    simp only [nodeValue, he, ite_true]
    unfold Node.subsumeBy
    split
    next => rfl
    next hg =>
      have hb := subsumeBy_removed_const_le_vars ρ true r original hp (Or.inl rfl)
        (eq_false_of_ne_true hg)
      simp only [Bool.true_or, ite_true]
      omega
  · simp [nodeValue, eq_false_of_ne_true he]

theorem node_subsume_same_context (ρ : Valuation) (p : List Name)
    (original r : Node) (C : Nat) (hw : NodeWF p original)
    (hv : nodeVarsValue ρ p original ≤ max (nodeVarsValue ρ p r) C) :
    max (nodeValue ρ p (Node.subsume p r p original)) C =
      max (nodeValue ρ p r) C := by
  unfold Node.subsume
  split
  · simpa using node_subsumeBy_same_context ρ p original r C hw hv
  · rfl

/-- Whole-value equality, residual WF and a variable sublist cannot replace
the gated-variable coverage premise of the self-context theorem. -/
theorem self_context_needs_vars_counterexample :
    let u : Name := .str .anonymous "u"
    let original : Node := { const := 1, var := [⟨u, 0⟩] }
    let r : Node := { const := 1 }
    let ρ : Valuation := fun _ => 1
    NodeWF [u] original ∧ NodeWF [u] r ∧ r.var.Sublist original.var ∧
      nodeValue ρ [u] r = nodeValue ρ [u] original ∧
      max (nodeValue ρ [u] r) 0 = 1 ∧
      max (nodeValue ρ [u] (Node.subsume [u] r [u] original)) 0 = 0 ∧
      ¬ nodeVarsValue ρ [u] original ≤ max (nodeVarsValue ρ [u] r) 0 := by
  dsimp only
  constructor
  · simp [NodeWF]
  constructor
  · simp [NodeWF]
  constructor
  · exact List.nil_sublist _
  · exact ⟨rfl, rfl, rfl, by decide⟩

end Vesper.Lean4LeanLevel42
