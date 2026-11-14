import Lean4LeanLevel42.Semantics
import Lean4LeanLevel42.ListProofs

namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level.Normalize

/-- If the constant test discards a constant using a variable dominator,
positivity of the dominator's variables supplies the required extra one. -/
theorem subsumeBy_removed_const_le_vars (ρ : Valuation) (same : Bool) (n1 n2 : Node)
    (hp : ∀ v ∈ n2.var, 0 < ρ v.var)
    (hc : same = true ∨ n2.const < n1.const)
    (hg : (n1.const = 0 ||
      (same || n1.const > n2.const) &&
      (n2.var.isEmpty || n1.const > n2.var.foldl (fun a v => max a v.offset) 0 + 1)) = false) :
    n1.const ≤ varsValue ρ n2.var := by
  have hcomp : (same || n1.const > n2.const) = true := by
    simpa only [Bool.or_eq_true, decide_eq_true_eq] using hc
  have hne : n2.var ≠ [] := by
    intro hn
    simp [hn, hcomp] at hg
  have hbound := offset_bound ρ n2.var hne hp
  have hconst : n1.const ≤ n2.var.foldl (fun a v => max a v.offset) 0 + 1 := by
    by_contra hn
    have hlt : n2.var.foldl (fun a v => max a v.offset) 0 + 1 < n1.const :=
      Nat.lt_of_not_ge hn
    simp [hcomp, hlt] at hg
  exact Nat.le_trans hconst hbound

/-- The constant test only discards constants covered by the other node. -/
theorem subsumeBy_removed_const_bound (ρ : Valuation) (same : Bool) (n1 n2 : Node)
    (hp : ∀ v ∈ n2.var, 0 < ρ v.var)
    (hg : (n1.const = 0 ||
      (same || n1.const > n2.const) &&
      (n2.var.isEmpty || n1.const > n2.var.foldl (fun a v => max a v.offset) 0 + 1)) = false) :
    n1.const ≤ max n2.const (varsValue ρ n2.var) := by
  by_cases hc : n1.const ≤ n2.const
  · exact Nat.le_trans hc (Nat.le_max_left _ _)
  · exact Nat.le_trans
      (subsumeBy_removed_const_le_vars ρ same n1 n2 hp
        (Or.inr (Nat.lt_of_not_ge hc)) hg) (Nat.le_max_right _ _)

/-- The actual helper preserves the constant's union with its dominator. -/
theorem node_subsumeBy_const_union (ρ : Valuation) (same : Bool) (n1 n2 : Node)
    (hp : ∀ v ∈ n2.var, 0 < ρ v.var) :
    max (n1.subsumeBy same n2).const (max n2.const (varsValue ρ n2.var)) =
      max n1.const (max n2.const (varsValue ρ n2.var)) := by
  unfold Node.subsumeBy
  split
  next hg => split <;> rfl
  next hg =>
    have hb := subsumeBy_removed_const_bound ρ same n1 n2 hp
      (Bool.eq_false_of_not_eq_true hg)
    split <;> simp only <;> omega

/-- Variable filtering uses the actual subsumeVars helper, with no sorting premise. -/
theorem node_subsumeBy_vars_union (ρ : Valuation) (same : Bool) (n1 n2 : Node) :
    max (varsValue ρ (n1.subsumeBy same n2).var) (varsValue ρ n2.var) =
      max (varsValue ρ n1.var) (varsValue ρ n2.var) := by
  unfold Node.subsumeBy
  split <;> split
  · rfl
  · exact subsumeVars_union ρ n1.var n2.var
  · rfl
  · exact subsumeVars_union ρ n1.var n2.var

/-- One actual subsumeBy call preserves max-union for either value of same.
Only the right node must obey its condition set. -/
theorem node_subsumeBy_union (ρ : Valuation) (same : Bool)
    (p1 p2 : List Name) (n1 n2 : Node)
    (hsub : p2 ⊆ p1) (hw : NodeWF p2 n2) :
    max (nodeValue ρ p1 (n1.subsumeBy same n2)) (nodeValue ρ p2 n2) =
      max (nodeValue ρ p1 n1) (nodeValue ρ p2 n2) := by
  by_cases he : enabled ρ p1 = true
  · have he2 := enabled_of_subset hsub he
    have hc := node_subsumeBy_const_union ρ same n1 n2 (NodeWF_positive hw he2)
    have hv := node_subsumeBy_vars_union ρ same n1 n2
    simp only [nodeValue, he, he2, ite_true]
    omega
  · have hf := Bool.eq_false_of_not_eq_true he
    simp [nodeValue, hf]

/-- The public wrapper's actual subset test supplies the semantic inclusion. -/
theorem node_subsume_union (ρ : Valuation) (p1 p2 : List Name) (n1 n2 : Node)
    (hw : NodeWF p2 n2) :
    max (nodeValue ρ p1 (Node.subsume p1 n1 p2 n2)) (nodeValue ρ p2 n2) =
      max (nodeValue ρ p1 n1) (nodeValue ρ p2 n2) := by
  unfold Node.subsume
  split
  next hs =>
    exact node_subsumeBy_union ρ (p1.length == p2.length) p1 p2 n1 n2
      (subset_sound hs) hw
  next => rfl

/-- A same-key self step preserves the node itself, not just its union. -/
theorem node_subsumeBy_self (ρ : Valuation) (p : List Name) (n : Node)
    (hw : NodeWF p n) :
    nodeValue ρ p (n.subsumeBy true n) = nodeValue ρ p n := by
  by_cases he : enabled ρ p = true
  · have hp := NodeWF_positive hw he
    simp only [nodeValue, he, ite_true]
    unfold Node.subsumeBy
    split
    next hg => rfl
    next hg =>
      have hb := subsumeBy_removed_const_le_vars ρ true n n hp (Or.inl rfl)
        (Bool.eq_false_of_not_eq_true hg)
      simp only [Bool.true_or, ite_true]
      omega
  · simp [nodeValue, Bool.eq_false_of_not_eq_true he]

/-- The public self wrapper also preserves a well-formed node exactly. -/
theorem node_subsume_self (ρ : Valuation) (p : List Name) (n : Node)
    (hw : NodeWF p n) :
    nodeValue ρ p (Node.subsume p n p n) = nodeValue ρ p n := by
  unfold Node.subsume
  split
  next => simpa using node_subsumeBy_self ρ p n hw
  next => rfl

/-- Dropping well-formedness is genuinely unsound: the empty condition set
allows a zero-valued variable to erase the positive constant one. -/
theorem invalid_wf_counterexample :
    let u : Name := .str .anonymous "u"
    let n1 : Node := { const := 1 }
    let n2 : Node := { var := [⟨u, 0⟩] }
    let ρ : Valuation := fun _ => 0
    ¬ NodeWF [] n2 ∧
      max (nodeValue ρ [] n1) (nodeValue ρ [] n2) = 1 ∧
      max (nodeValue ρ [] (n1.subsumeBy false n2)) (nodeValue ρ [] n2) = 0 ∧
      max (nodeValue ρ [] (Node.subsume [] n1 [] n2)) (nodeValue ρ [] n2) = 0 := by
  dsimp only
  constructor
  · intro h
    have hb := h ⟨.str .anonymous "u", 0⟩ (by simp)
    simpa using hb
  · decide

/-- An enabled, well-formed variable dominator really erases both the
constant and the variable from the larger-key node. -/
theorem enabled_variable_removal :
    let u : Name := .str .anonymous "u"
    let v : Name := .str .anonymous "v"
    let n1 : Node := { const := 2, var := [⟨u, 1⟩] }
    let n2 : Node := { var := [⟨u, 1⟩] }
    let ρ : Valuation := fun _ => 1
    NodeWF [u] n2 ∧
      (Node.subsume [u, v] n1 [u] n2).const = 0 ∧
      (Node.subsume [u, v] n1 [u] n2).var = [] ∧
      nodeValue ρ [u, v] n1 = 2 ∧
      nodeValue ρ [u, v] (Node.subsume [u, v] n1 [u] n2) = 0 ∧
      max (nodeValue ρ [u, v] (Node.subsume [u, v] n1 [u] n2))
        (nodeValue ρ [u] n2) = 2 := by
  dsimp only
  constructor
  · simp [NodeWF]
  · decide

/-- A larger constant at the smaller key actually removes the old constant. -/
theorem larger_constant_removal :
    let u : Name := .str .anonymous "u"
    let n1 : Node := { const := 3 }
    let n2 : Node := { const := 4 }
    let ρ : Valuation := fun _ => 1
    NodeWF [] n2 ∧
      (Node.subsume [u] n1 [] n2).const = 0 ∧
      nodeValue ρ [u] n1 = 3 ∧
      nodeValue ρ [u] (Node.subsume [u] n1 [] n2) = 0 ∧
      max (nodeValue ρ [u] (Node.subsume [u] n1 [] n2))
        (nodeValue ρ [] n2) = 4 := by
  dsimp only
  constructor
  · simp [NodeWF]
  · decide

/-- The larger condition set may be disabled while the smaller one remains
active; the real removal step still preserves the union. -/
theorem disabled_larger_key :
    let u : Name := .str .anonymous "u"
    let n1 : Node := { const := 3 }
    let n2 : Node := { const := 4 }
    let ρ : Valuation := fun _ => 0
    NodeWF [] n2 ∧
      enabled ρ [u] = false ∧ enabled ρ [] = true ∧
      (Node.subsume [u] n1 [] n2).const = 0 ∧
      nodeValue ρ [u] n1 = 0 ∧
      max (nodeValue ρ [u] (Node.subsume [u] n1 [] n2))
        (nodeValue ρ [] n2) = 4 := by
  dsimp only
  constructor
  · simp [NodeWF]
  · decide

/-- At the same key the constant really disappears, while the variable is
retained and the node's own value is preserved. -/
theorem valid_self_same_removal :
    let u : Name := .str .anonymous "u"
    let n : Node := { const := 1, var := [⟨u, 0⟩] }
    let ρ : Valuation := fun _ => 1
    NodeWF [u] n ∧
      (n.subsumeBy true n).const = 0 ∧
      (n.subsumeBy true n).var = n.var ∧
      (Node.subsume [u] n [u] n).const = 0 ∧
      (Node.subsume [u] n [u] n).var = n.var ∧
      nodeValue ρ [u] (n.subsumeBy true n) = 1 ∧
      nodeValue ρ [u] (Node.subsume [u] n [u] n) = 1 := by
  dsimp only
  constructor
  · simp [NodeWF]
  · decide

end Vesper.Lean4LeanLevel42
