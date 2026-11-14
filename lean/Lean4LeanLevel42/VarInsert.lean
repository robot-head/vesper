import Lean4LeanLevel42.Semantics
import Lean4LeanLevel42.ListProofs

namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level.Normalize

/-- The actual variable insertion preserves the maximum, even on unsorted
lists containing repeated variable names. -/
theorem addVar_value (ρ : Valuation) (v : Name) (k : Nat) (xs : List VarNode) :
    varsValue ρ (VarNode.addVar v k xs) = max (ρ v + k) (varsValue ρ xs) := by
  induction xs with
  | nil => simp [VarNode.addVar, varsValue_nil, varsValue_cons]
  | cons w ws ih =>
    cases hc : Name.cmp v w.var with
    | lt => simp only [VarNode.addVar, hc, varsValue_cons]
    | eq =>
      have hn : v = w.var := name_cmp_eq v w.var hc
      simp only [VarNode.addVar, hc, varsValue_cons]
      rw [← hn]
      rw [← Nat.add_max_add_left]
      omega
    | gt =>
      simp only [VarNode.addVar, hc, varsValue_cons, ih]
      omega

/-- Exactly the inserted name is added to the set of names represented by
the actual output. Existing names remain represented. -/
theorem addVar_names (v : Name) (k : Nat) (xs : List VarNode) (x : Name) :
    (∃ w ∈ VarNode.addVar v k xs, w.var = x) ↔
      x = v ∨ ∃ w ∈ xs, w.var = x := by
  induction xs with
  | nil => simp [VarNode.addVar, eq_comm]
  | cons w ws ih =>
    cases hc : Name.cmp v w.var with
    | lt =>
      simp only [VarNode.addVar, hc, List.mem_cons, or_and_right, exists_or, exists_eq_left]
      simp [eq_comm]
    | eq =>
      have hn : v = w.var := name_cmp_eq v w.var hc
      simp only [VarNode.addVar, hc, List.mem_cons, or_and_right, exists_or, exists_eq_left]
      rw [← hn]
      simp [eq_comm]
    | gt =>
      simp only [VarNode.addVar, hc, List.mem_cons, or_and_right, exists_or, exists_eq_left, ih]
      simp [or_left_comm]

/-- Key membership is preserved without assumptions about list order. -/
theorem addVar_preserves_key (v : Name) (k : Nat) (xs : List VarNode) (p : List Name)
    (hx : ∀ w ∈ xs, w.var ∈ p) (hv : v ∈ p) :
    ∀ w ∈ VarNode.addVar v k xs, w.var ∈ p := by
  intro w hw
  rcases (addVar_names v k xs w.var).mp ⟨w, hw, rfl⟩ with he | he
  · simpa [he] using hv
  · rcases he with ⟨w', hw', he⟩
    rw [← he]
    exact hx w' hw'

/-- Value of the exact inner record update used by the actual map helpers.
This does not assert a whole-map property. -/
theorem node_addVar_value (ρ : Valuation) (p : List Name) (n : Node) (v : Name) (k : Nat) :
    nodeValue ρ p { n with var := VarNode.addVar v k n.var } =
      max (nodeValue ρ p n) (if enabled ρ p then ρ v + k else 0) := by
  by_cases he : enabled ρ p = true
  · simp only [nodeValue, he, ite_true]
    rw [addVar_value]
    omega
  · simp [nodeValue, eq_false_of_ne_true he]

/-- The exact inner record update preserves the node's condition invariant. -/
theorem node_addVar_WF (p : List Name) (n : Node) (v : Name) (k : Nat)
    (hw : NodeWF p n) (hv : v ∈ p) :
    NodeWF p { n with var := VarNode.addVar v k n.var } := by
  exact addVar_preserves_key v k n.var p hw hv

/-- Numeric name zero is inserted before name one, retaining the old entry. -/
theorem numeric_name_insert_before :
    VarNode.addVar (.num .anonymous 0) 3 [⟨.num .anonymous 1, 2⟩] =
      [⟨.num .anonymous 0, 3⟩, ⟨.num .anonymous 1, 2⟩] := by
  rfl

/-- Numeric name one is inserted after name zero by the actual recursive call. -/
theorem numeric_name_insert_after :
    VarNode.addVar (.num .anonymous 1) 3 [⟨.num .anonymous 0, 2⟩] =
      [⟨.num .anonymous 0, 2⟩, ⟨.num .anonymous 1, 3⟩] := by
  rfl

/-- Equal numeric names merge with the maximum offset, in both directions. -/
theorem numeric_name_merge :
    VarNode.addVar (.num .anonymous 0) 5 [⟨.num .anonymous 0, 2⟩] =
      [⟨.num .anonymous 0, 5⟩] ∧
    VarNode.addVar (.num .anonymous 0) 1 [⟨.num .anonymous 0, 2⟩] =
      [⟨.num .anonymous 0, 2⟩] := by
  exact ⟨rfl, rfl⟩

/-- An unsorted input can retain a duplicate. Repeating insertion merges the
new front entry, while leaving the old trailing duplicate intact. -/
theorem numeric_name_repeated_unsorted :
    let z : Name := .num .anonymous 0
    let o : Name := .num .anonymous 1
    let xs : List VarNode := [⟨o, 2⟩, ⟨z, 7⟩]
    VarNode.addVar z 3 xs = [⟨z, 3⟩, ⟨o, 2⟩, ⟨z, 7⟩] ∧
      VarNode.addVar z 5 (VarNode.addVar z 3 xs) = [⟨z, 5⟩, ⟨o, 2⟩, ⟨z, 7⟩] ∧
      varsValue (fun _ => 1) (VarNode.addVar z 5 (VarNode.addVar z 3 xs)) = 8 := by
  exact ⟨rfl, rfl, rfl⟩

end Vesper.Lean4LeanLevel42
