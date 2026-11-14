import Lean4LeanLevel42.Addable
import Lean4LeanLevel42.MapEquality

/-! Deterministic reconstruction depends on actual stored lookups. The chain
permutation includes every fuel value and the actual fallback branch. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level Lean.Level.Normalize

theorem feasible_go_lookup_congr (s t : NormLevel) (fuel : Nat)
    (acc rem : List Name) (h : ∀ p, s.get? p = t.get? p) :
    NormLevel.feasible.go s fuel acc rem = NormLevel.feasible.go t fuel acc rem := by
  induction fuel generalizing acc rem with
  | zero => rfl
  | succ fuel ih =>
    simp only [NormLevel.feasible.go]
    have hf : (fun a => s.addable a acc) = (fun a => t.addable a acc) := by
      funext a
      exact addable_lookup_congr s t a acc h
    rw [hf]
    split
    · rfl
    · exact ih _ _

theorem feasible_lookup_congr (s t : NormLevel) (acc rem : List Name)
    (h : ∀ p, s.get? p = t.get? p) : s.feasible acc rem = t.feasible acc rem := by
  exact feasible_go_lookup_congr s t rem.length acc rem h

theorem lexChain_lookup_congr (s t : NormLevel) (fuel : Nat) (p : List Name)
    (h : ∀ q, s.get? q = t.get? q) : s.lexChain fuel p = t.lexChain fuel p := by
  induction fuel generalizing p with
  | zero => rfl
  | succ fuel ih =>
    simp only [NormLevel.lexChain]
    have hf : (fun a => s.addable a (p.erase a) && s.feasible [] (p.erase a)) =
        (fun a => t.addable a (p.erase a) && t.feasible [] (p.erase a)) := by
      funext a
      rw [addable_lookup_congr s t a (p.erase a) h,
        feasible_lookup_congr s t [] (p.erase a) h]
    rw [hf]
    split
    · exact congrArg (List.cons _) (ih _)
    · rfl

/-- No sorting, coverage, distinctness or successful search is needed. -/
theorem lexChain_perm (s : NormLevel) (fuel : Nat) (p : List Name) :
    (s.lexChain fuel p).Perm p := by
  induction fuel generalizing p with
  | zero => exact List.Perm.refl _
  | succ fuel ih =>
    simp only [NormLevel.lexChain]
    split
    next a hf =>
      exact ((ih (p.erase a)).cons a).trans
        (List.perm_cons_erase (List.mem_of_find?_eq_some hf)).symm
    next => exact List.Perm.refl _

/-- Ordered enumeration equality follows from actual extensional map equality,
without identifying the physical balancing trees. -/
theorem norm_toList_lookup_congr (s t : NormLevel)
    (h : ∀ p, s.get? p = t.get? p) : s.toList = t.toList := by
  exact Std.TreeMap.Equiv.toList_eq (cmp := List.compareLex Name.cmp)
    (Std.TreeMap.Equiv.of_forall_constGet?_eq (cmp := List.compareLex Name.cmp) h)

theorem norm_toTree_lookup_congr (s t : NormLevel)
    (h : ∀ p, s.get? p = t.get? p) : s.toTree = t.toTree := by
  unfold NormLevel.toTree
  rw [Std.TreeMap.foldl_eq_foldl_toList, Std.TreeMap.foldl_eq_foldl_toList,
    norm_toList_lookup_congr s t h]
  have hf : (fun (z : Tree) (row : List Name × Node) =>
      let path := s.lexChain row.1.length row.1
      let vs := if let v :: _ := path then
        Normalize.subsumeVars row.2.var [⟨v, 0⟩] else row.2.var
      z.modify path (fun z => { z with const := row.2.const, var := vs })) =
    (fun (z : Tree) (row : List Name × Node) =>
      let path := t.lexChain row.1.length row.1
      let vs := if let v :: _ := path then
        Normalize.subsumeVars row.2.var [⟨v, 0⟩] else row.2.var
      z.modify path (fun z => { z with const := row.2.const, var := vs })) := by
    funext z row
    rw [lexChain_lookup_congr s t row.1.length row.1 h]
  exact congrArg (fun f => List.foldl f (⟨0, [], []⟩ : Tree) t.toList) hf

theorem norm_reify_lookup_congr (s t : NormLevel)
    (h : ∀ p, s.get? p = t.get? p) : s.toTree.reify = t.toTree.reify :=
  congrArg Tree.reify (norm_toTree_lookup_congr s t h)

theorem feasible_beq_congr (s t : NormLevel) (acc rem : List Name)
    (h : (s == t) = true) : s.feasible acc rem = t.feasible acc rem :=
  feasible_lookup_congr s t acc rem ((norm_beq_iff_lookup_eq s t).mp h)

theorem lexChain_beq_congr (s t : NormLevel) (fuel : Nat) (p : List Name)
    (h : (s == t) = true) : s.lexChain fuel p = t.lexChain fuel p :=
  lexChain_lookup_congr s t fuel p ((norm_beq_iff_lookup_eq s t).mp h)

theorem norm_toTree_beq_congr (s t : NormLevel) (h : (s == t) = true) :
    s.toTree = t.toTree :=
  norm_toTree_lookup_congr s t ((norm_beq_iff_lookup_eq s t).mp h)

theorem norm_reify_beq_congr (s t : NormLevel) (h : (s == t) = true) :
    s.toTree.reify = t.toTree.reify :=
  norm_reify_lookup_congr s t ((norm_beq_iff_lookup_eq s t).mp h)

end Vesper.Lean4LeanLevel42
