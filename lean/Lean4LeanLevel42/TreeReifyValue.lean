import Lean4LeanLevel42.LevelSemantics
import Lean4LeanLevel42.MapValue

/-! Denotation of the actual one-layer tree reconstruction equation. Child
values refer to actual reification, without a replacement recursive evaluator. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level Lean.Level.Normalize

def reifyOptionValue (ρ : Valuation) (r : Option Level) : Nat :=
  r.elim 0 (levelValue ρ)

def reifyChildValue (ρ : Valuation) (e : Name × Tree) : Nat :=
  Nat.imax (levelValue ρ e.2.reify) (ρ e.1)

theorem levelValue_ofNat (ρ : Valuation) (n : Nat) :
    levelValue ρ (Level.ofNat n) = n := by
  induction n with
  | zero => rfl
  | succ n ih => simpa [Level.ofNat, mkLevelSucc, levelValue] using ih

theorem levelValue_addOffsetAux (ρ : Valuation) (n : Nat) (l : Level) :
    levelValue ρ (Level.addOffsetAux n l) = levelValue ρ l + n := by
  induction n generalizing l with
  | zero => rfl
  | succ n ih =>
    rw [Level.addOffsetAux, ih]
    simp only [mkLevelSucc, levelValue]
    omega

theorem levelValue_addOffset (ρ : Valuation) (l : Level) (n : Nat) :
    levelValue ρ (l.addOffset n) = levelValue ρ l + n :=
  levelValue_addOffsetAux ρ n l

theorem reify_mkMax_value (ρ : Valuation) (l : Level) (r : Option Level) :
    levelValue ρ (Tree.reify.mkMax l r) =
      max (levelValue ρ l) (reifyOptionValue ρ r) := by
  cases r <;> simp [Tree.reify.mkMax, reifyOptionValue, levelValue]

theorem reify_mkChild_value (ρ : Valuation) (a : Name) (t : Tree) (r : Option Level) :
    reifyOptionValue ρ (Tree.reify.mkChild (a, t) r) =
      max (reifyChildValue ρ (a, t)) (reifyOptionValue ρ r) := by
  cases he : t.reify <;>
    simp [Tree.reify.mkChild, he, reifyOptionValue, reifyChildValue,
      reify_mkMax_value, levelValue, Nat.imax]
  by_cases hz : ρ a = 0 <;> simp [hz]

theorem reify_children_fold_value (ρ : Valuation) (cs : List (Name × Tree))
    (r : Option Level) :
    reifyOptionValue ρ (cs.foldr Tree.reify.mkChild r) =
      max (cs.foldr (fun e z => max (reifyChildValue ρ e) z) 0)
        (reifyOptionValue ρ r) := by
  induction cs with
  | nil => simp
  | cons e cs ih =>
    cases e with
    | mk a t =>
      rw [List.foldr_cons, reify_mkChild_value, ih]
      simp only [List.foldr_cons, Nat.max_assoc]

theorem reify_vars_fold_value (ρ : Valuation) (vs : List VarNode) (r : Option Level) :
    reifyOptionValue ρ (vs.foldr (fun n r =>
      some (Tree.reify.mkMax ((.param n.var : Level).addOffset n.offset) r)) r) =
      max (varsValue ρ vs) (reifyOptionValue ρ r) := by
  induction vs with
  | nil => simp [varsValue]
  | cons v vs ih =>
    change levelValue ρ (Tree.reify.mkMax ((.param v.var : Level).addOffset v.offset)
      (vs.foldr _ r)) = _
    rw [reify_mkMax_value, levelValue_addOffset, ih]
    simp only [levelValue, varsValue_cons, Nat.max_assoc]

theorem tree_reify_value (ρ : Valuation) (t : Tree) :
    levelValue ρ t.reify = max t.const (max (varsValue ρ t.var)
      (t.child.foldr (fun e z => max (reifyChildValue ρ e) z) 0)) := by
  cases t with
  | mk c vs cs =>
    have hf := reify_vars_fold_value ρ vs (cs.foldr Tree.reify.mkChild none)
    rw [reify_children_fold_value] at hf
    simp only [reifyOptionValue, Option.elim, Nat.max_zero] at hf
    rw [Tree.reify]
    split
    next h =>
      rw [levelValue_ofNat]
      rw [h] at hf
      have hz : max (varsValue ρ vs)
          (cs.foldr (fun e z => max (reifyChildValue ρ e) z) 0) = 0 := by
        simpa [reifyOptionValue] using hf.symm
      simp [hz]
    next l h =>
      rw [h] at hf
      change levelValue ρ l = _ at hf
      split
      next hc => simp [hc, hf]
      next => simp only [levelValue, levelValue_ofNat, hf]

theorem foldr_max_mem_bound {α : Type} (f : α → Nat) (xs : List α)
    {x : α} (hx : x ∈ xs) : f x ≤ xs.foldr (fun e z => max (f e) z) 0 := by
  induction xs with
  | nil => simp at hx
  | cons e xs ih =>
    rcases List.mem_cons.mp hx with rfl | hx
    · exact Nat.le_max_left _ _
    · exact Nat.le_trans (ih hx) (Nat.le_max_right _ _)

theorem foldr_max_le {α : Type} (f : α → Nat) (xs : List α) (b : Nat)
    (h : ∀ x ∈ xs, f x ≤ b) : xs.foldr (fun e z => max (f e) z) 0 ≤ b := by
  induction xs with
  | nil => exact Nat.zero_le _
  | cons e xs ih =>
    exact Nat.max_le.mpr ⟨h e (by simp), ih (fun x hx => h x (by simp [hx]))⟩

theorem tree_reify_const_bound (ρ : Valuation) (t : Tree) :
    t.const ≤ levelValue ρ t.reify := by
  rw [tree_reify_value]
  exact Nat.le_max_left _ _

theorem tree_reify_vars_bound (ρ : Valuation) (t : Tree) :
    varsValue ρ t.var ≤ levelValue ρ t.reify := by
  rw [tree_reify_value]
  exact Nat.le_trans (Nat.le_max_left _ _) (Nat.le_max_right _ _)

theorem tree_reify_payload_bound (ρ : Valuation) (t : Tree) :
    max t.const (varsValue ρ t.var) ≤ levelValue ρ t.reify :=
  Nat.max_le.mpr ⟨tree_reify_const_bound ρ t, tree_reify_vars_bound ρ t⟩

theorem tree_reify_var_bound (ρ : Valuation) (t : Tree) {v : VarNode}
    (hv : v ∈ t.var) : ρ v.var + v.offset ≤ levelValue ρ t.reify :=
  Nat.le_trans (varsValue_mem_bound hv) (tree_reify_vars_bound ρ t)

theorem tree_reify_child_bound (ρ : Valuation) (t : Tree) {a : Name} {u : Tree}
    (hu : (a, u) ∈ t.child) :
    Nat.imax (levelValue ρ u.reify) (ρ a) ≤ levelValue ρ t.reify := by
  rw [tree_reify_value ρ t]
  exact Nat.le_trans (foldr_max_mem_bound (reifyChildValue ρ) t.child hu)
    (Nat.le_trans (Nat.le_max_right _ _) (Nat.le_max_right _ _))

theorem tree_reify_edge_bound (ρ : Valuation) (t : Tree) {a : Name} {u : Tree}
    (hu : (a, u) ∈ t.child) : ρ a ≤ levelValue ρ t.reify := by
  apply Nat.le_trans ?_ (tree_reify_child_bound ρ t hu)
  cases ha : ρ a with
  | zero => exact Nat.zero_le _
  | succ n => simpa [Nat.imax] using Nat.le_max_right (levelValue ρ u.reify) (n + 1)

theorem tree_reify_leaf_value (ρ : Valuation) (c : Nat) (vs : List VarNode) :
    levelValue ρ (⟨c, vs, []⟩ : Tree).reify = max c (varsValue ρ vs) := by
  rw [tree_reify_value]
  simp

theorem tree_reify_le (ρ : Valuation) (t : Tree) (b : Nat)
    (hc : t.const ≤ b) (hv : varsValue ρ t.var ≤ b)
    (he : ∀ a u, (a, u) ∈ t.child → Nat.imax (levelValue ρ u.reify) (ρ a) ≤ b) :
    levelValue ρ t.reify ≤ b := by
  rw [tree_reify_value]
  exact Nat.max_le.mpr ⟨hc, Nat.max_le.mpr ⟨hv,
    foldr_max_le (reifyChildValue ρ) t.child b (fun ⟨a, u⟩ hu => he a u hu)⟩⟩

end Vesper.Lean4LeanLevel42
