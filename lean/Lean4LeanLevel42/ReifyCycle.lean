import Lean4LeanLevel42.SortedKeys
import Lean4LeanLevel42.MapValue
import Lean4LeanLevel42.PathPriming
import Lean4LeanLevel42.LevelSemantics

/-! A concrete raw-map boundary for reconstruction. Strictly sorted, nonempty,
well-formed rows and PathPrimed coverage can still be cyclic. No claim is made
that actual normalization produces this map. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level Lean.Level.Normalize

def reifyCycleA : Name := .num .anonymous 0

def reifyCycleB : Name := .num .anonymous 1

def reifyCyclePath : List Name := [reifyCycleA, reifyCycleB]

def reifyCycleNode : Node :=
  { const := 0, var := [⟨reifyCycleA, 0⟩, ⟨reifyCycleB, 0⟩] }

def reifyCycleMap : NormLevel :=
  ({} : NormLevel).insert reifyCyclePath reifyCycleNode

def reifyCycleValuation : Valuation := fun v => if v = reifyCycleA then 0 else 1

theorem reifyCycle_lookup {p : List Name} {n : Node}
    (h : reifyCycleMap.get? p = some n) :
    p = reifyCyclePath ∧ n = reifyCycleNode := by
  change (({} : NormLevel).insert reifyCyclePath reifyCycleNode).get? p = some n at h
  rw [map_get_insert] at h
  split at h
  · rename_i hp
    cases h
    exact ⟨hp.symm, rfl⟩
  · rw [map_get_empty] at h
    contradiction

theorem reifyCycle_WF : MapWF reifyCycleMap := by
  intro p n hn
  obtain ⟨rfl, rfl⟩ := reifyCycle_lookup hn
  dsimp [NodeWF, reifyCycleNode, reifyCyclePath]
  intro v hv
  simp only [List.mem_cons, List.not_mem_nil, or_false] at hv
  rcases hv with rfl | rfl <;> simp

theorem reifyCycle_keysSorted :
    ∀ p n, reifyCycleMap.get? p = some n → KeySorted p := by
  intro p n hn
  obtain ⟨rfl, rfl⟩ := reifyCycle_lookup hn
  simp [KeySorted, reifyCyclePath, reifyCycleA, reifyCycleB, Name.cmp] <;> decide

theorem reifyCycle_varsSorted :
    ∀ p n, reifyCycleMap.get? p = some n → VarsSorted n.var := by
  intro p n hn
  obtain ⟨rfl, rfl⟩ := reifyCycle_lookup hn
  simp [VarsSorted, reifyCycleNode, reifyCycleA, reifyCycleB, Name.cmp] <;> decide

theorem reifyCycle_noEmpty :
    ∀ p n, reifyCycleMap.get? p = some n → n.isEmpty = false := by
  intro p n hn
  obtain ⟨rfl, rfl⟩ := reifyCycle_lookup hn
  rfl

theorem reifyCycle_pathPrimed : PathPrimed reifyCyclePath reifyCycleMap := by
  constructor
  · intro _
    exact ⟨reifyCycleNode, rfl⟩
  · intro v hv
    refine ⟨reifyCyclePath, reifyCycleNode, ⟨v, 0⟩, rfl, ?_, ?_, rfl⟩
    · intro x hx
      exact hx
    · simp only [reifyCyclePath, List.mem_cons, List.not_mem_nil, or_false] at hv
      rcases hv with rfl | rfl <;> simp [reifyCycleNode]

theorem reifyCycle_addable_a :
    reifyCycleMap.addable reifyCycleA [] = false := by
  rfl

theorem reifyCycle_addable_b :
    reifyCycleMap.addable reifyCycleB [] = false := by
  rfl

theorem reifyCycle_feasible :
    reifyCycleMap.feasible [] reifyCyclePath = false := by
  rfl

theorem reifyCycle_lexChain :
    reifyCycleMap.lexChain reifyCyclePath.length reifyCyclePath = reifyCyclePath := by
  rfl

/-- This is the actual reconstruction, including variable pruning and the
reverse nesting performed by Tree.modify, rather than a replacement tree. -/
theorem reifyCycle_toTree_reify :
    reifyCycleMap.toTree.reify =
      .imax (.imax (.param reifyCycleB) (.param reifyCycleA)) (.param reifyCycleB) := by
  have hs : Normalize.subsumeVars reifyCycleNode.var [⟨reifyCycleA, 0⟩] =
      [⟨reifyCycleB, 0⟩] := by
    simp [Normalize.subsumeVars, reifyCycleNode, reifyCycleA, Name.cmp]
  change (⟨0, [], [(reifyCycleB, ⟨0, [],
    [(reifyCycleA, ⟨0, Normalize.subsumeVars reifyCycleNode.var [⟨reifyCycleA, 0⟩],
      []⟩)]⟩)]⟩ : Tree).reify = _
  rw [hs]
  simp [Tree.reify, Tree.reify.mkChild, Tree.reify.mkMax] <;> rfl

theorem reifyCycle_mapValue : mapValue reifyCycleValuation reifyCycleMap = 0 := by
  rfl

theorem reifyCycle_reifiedValue :
    levelValue reifyCycleValuation reifyCycleMap.toTree.reify = 1 := by
  rw [reifyCycle_toTree_reify]
  rfl

/-- Existing shape and coverage predicates do not suffice to prove denotation
preservation for reconstruction on arbitrary raw normalizer maps. -/
theorem reifyCycle_counterexample :
    MapWF reifyCycleMap ∧
      (∀ p n, reifyCycleMap.get? p = some n → KeySorted p) ∧
      (∀ p n, reifyCycleMap.get? p = some n → VarsSorted n.var) ∧
      (∀ p n, reifyCycleMap.get? p = some n → n.isEmpty = false) ∧
      PathPrimed reifyCyclePath reifyCycleMap ∧
      reifyCycleMap.feasible [] reifyCyclePath = false ∧
      reifyCycleMap.lexChain reifyCyclePath.length reifyCyclePath = reifyCyclePath ∧
      mapValue reifyCycleValuation reifyCycleMap ≠
        levelValue reifyCycleValuation reifyCycleMap.toTree.reify := by
  refine ⟨reifyCycle_WF, reifyCycle_keysSorted, reifyCycle_varsSorted,
    reifyCycle_noEmpty, reifyCycle_pathPrimed, reifyCycle_feasible,
    reifyCycle_lexChain, ?_⟩
  rw [reifyCycle_mapValue, reifyCycle_reifiedValue]
  decide

end Vesper.Lean4LeanLevel42
