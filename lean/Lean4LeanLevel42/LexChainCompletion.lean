import Lean4LeanLevel42.Feasible
import Lean4LeanLevel42.ReifyCongruence

/-! Complete sorted paths have actual lexChain candidates. The reconstructed
chain is inside-out: its reverse, rather than its forward order, is admissible. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level Lean.Level.Normalize

/-- Processing a front prepends its reverse to the prior accumulator. -/
theorem admissible_append_iff (s : NormLevel) (acc xs ys : List Name) :
    Admissible s acc (xs ++ ys) ↔
      Admissible s acc xs ∧ Admissible s (xs.reverse ++ acc) ys := by
  induction xs generalizing acc with
  | nil =>
    change Admissible s acc ys ↔ True ∧ Admissible s acc ys
    exact ⟨fun h => ⟨trivial, h⟩, fun h => h.2⟩
  | cons a xs ih =>
    change (EdgeCovered s acc a ∧ Admissible s (a :: acc) (xs ++ ys)) ↔
      (EdgeCovered s acc a ∧ Admissible s (a :: acc) xs) ∧
        Admissible s ((a :: xs).reverse ++ acc) ys
    rw [ih (a :: acc)]
    simp only [List.reverse_cons, List.append_assoc, List.singleton_append]
    exact ⟨fun ⟨ha, hx, hy⟩ => ⟨⟨ha, hx⟩, hy⟩,
      fun ⟨⟨ha, hx⟩, hy⟩ => ⟨ha, hx, hy⟩⟩

/-- The final occurrence of an independent witnessing order gives a candidate
whose preceding names still have their own independent completion. -/
theorem completes_last_edge (s : NormLevel) (p : List Name)
    (hc : Completes s [] p) (hne : p ≠ []) :
    ∃ a ∈ p, EdgeCovered s (p.erase a) a ∧ Completes s [] (p.erase a) := by
  obtain ⟨order, hperm, hadm⟩ := hc
  have ho : order ≠ [] := by
    intro he
    subst order
    exact hne (List.Perm.nil_eq hperm).symm
  let front := order.dropLast
  let a := order.getLast ho
  have heq : front ++ [a] = order := List.dropLast_concat_getLast ho
  have hp : (front ++ [a]).Perm p := by rw [heq]; exact hperm
  have hcons : (a :: front).Perm p := (List.perm_append_singleton a front).symm.trans hp
  have herase : front.Perm (p.erase a) := by
    simpa only [List.erase_cons_head] using hcons.erase a
  have hs : Admissible s [] front ∧ Admissible s (front.reverse ++ []) [a] :=
    (admissible_append_iff s [] front [a]).mp (by rw [heq]; exact hadm)
  have hedge : EdgeCovered s front.reverse a := by
    simpa only [List.append_nil] using hs.2.1
  refine ⟨a, hcons.subset (by simp), ?_, ⟨front, herase, hs.1⟩⟩
  exact edgeCovered_acc_mono s a front.reverse (p.erase a)
    ((List.reverse_perm front).trans herase).subset hedge

/-- The actual Boolean predicate has a member witness, not a supplied flag. -/
theorem lexChain_candidate_exists (s : NormLevel) (p : List Name)
    (hkeys : ∀ q n, s.get? q = some n → KeySorted q) (hp : KeySorted p)
    (hc : Completes s [] p) (hne : p ≠ []) :
    ∃ a ∈ p, (s.addable a (p.erase a) && s.feasible [] (p.erase a)) = true := by
  obtain ⟨a, ha, he, hr⟩ := completes_last_edge s p hc hne
  exact ⟨a, ha, Bool.and_eq_true_iff.mpr
    ⟨(addable_iff_edgeCovered s a (p.erase a) hkeys (keySorted_erase a p hp)).mpr he,
      feasible_complete s [] (p.erase a) hkeys (by simp [KeySorted]) hr⟩⟩

theorem lexChain_find_some (s : NormLevel) (p : List Name)
    (hkeys : ∀ q n, s.get? q = some n → KeySorted q) (hp : KeySorted p)
    (hc : Completes s [] p) (hne : p ≠ []) :
    ∃ a, p.find? (fun a => s.addable a (p.erase a) &&
      s.feasible [] (p.erase a)) = some a := by
  cases hf : p.find? (fun a => s.addable a (p.erase a) &&
      s.feasible [] (p.erase a)) with
  | none =>
    obtain ⟨a, ha, hpred⟩ := lexChain_candidate_exists s p hkeys hp hc hne
    exact False.elim ((List.find?_eq_none.mp hf a ha) hpred)
  | some a => exact ⟨a, rfl⟩

/-- Nonempty complete sorted paths cannot take the actual failed-find branch.
This does not assert that the chain differs from the original path as a list. -/
theorem lexChain_no_nonempty_fallback (s : NormLevel) (p : List Name)
    (hkeys : ∀ q n, s.get? q = some n → KeySorted q) (hp : KeySorted p)
    (hc : Completes s [] p) (hne : p ≠ []) :
    p.find? (fun a => s.addable a (p.erase a) &&
      s.feasible [] (p.erase a)) ≠ none := by
  obtain ⟨a, ha⟩ := lexChain_find_some s p hkeys hp hc hne
  rw [ha]
  intro h
  contradiction

/-- With positive fuel, the actual computation takes a selected-name step. -/
theorem lexChain_nonempty_step (s : NormLevel) (fuel : Nat) (p : List Name)
    (hkeys : ∀ q n, s.get? q = some n → KeySorted q) (hp : KeySorted p)
    (hc : Completes s [] p) (hne : p ≠ []) :
    ∃ a, p.find? (fun a => s.addable a (p.erase a) &&
      s.feasible [] (p.erase a)) = some a ∧
      s.lexChain (fuel + 1) p = a :: s.lexChain fuel (p.erase a) := by
  obtain ⟨a, ha⟩ := lexChain_find_some s p hkeys hp hc hne
  refine ⟨a, ha, ?_⟩
  rw [NormLevel.lexChain, ha]

/-- Actual selected remainders complete independently. Reverse-chain prefixes
have their membership by the actual unconditional chain permutation theorem. -/
theorem lexChain_reverse_admissible (s : NormLevel) (fuel : Nat) (p : List Name)
    (hkeys : ∀ q n, s.get? q = some n → KeySorted q) (hp : KeySorted p)
    (hc : Completes s [] p) (hfuel : p.length ≤ fuel) :
    Admissible s [] (s.lexChain fuel p).reverse := by
  induction fuel generalizing p with
  | zero =>
    have he : p = [] := List.length_eq_zero_iff.mp (by omega)
    subst p
    trivial
  | succ fuel ih =>
    by_cases hnil : p = []
    · subst p
      simp [NormLevel.lexChain, Admissible]
    · obtain ⟨a, hfind, hout⟩ := lexChain_nonempty_step s fuel p hkeys hp hc hnil
      have hpred := List.find?_some
        (p := fun a => s.addable a (p.erase a) && s.feasible [] (p.erase a)) hfind
      obtain ⟨hadd, hfeas⟩ := Bool.and_eq_true_iff.mp hpred
      have hr := feasible_sound s [] (p.erase a) hfeas
      have hm := List.mem_of_find?_eq_some hfind
      have hlen : (p.erase a).length ≤ fuel := by
        rw [List.length_erase_of_mem hm]
        omega
      have hrec := ih (p.erase a) (keySorted_erase a p hp) hr hlen
      rw [hout, List.reverse_cons]
      apply (admissible_append_iff s [] (s.lexChain fuel (p.erase a)).reverse [a]).mpr
      refine ⟨hrec, ?_⟩
      simp only [List.reverse_reverse, List.append_nil]
      change EdgeCovered s (s.lexChain fuel (p.erase a)) a ∧ True
      refine ⟨?_, trivial⟩
      exact edgeCovered_acc_mono s a (p.erase a) (s.lexChain fuel (p.erase a))
        (lexChain_perm s fuel (p.erase a)).symm.subset
        (addable_edgeCovered s a (p.erase a) hadd)

theorem lexChain_length_reverse_admissible (s : NormLevel) (p : List Name)
    (hkeys : ∀ q n, s.get? q = some n → KeySorted q) (hp : KeySorted p)
    (hc : Completes s [] p) : Admissible s [] (s.lexChain p.length p).reverse :=
  lexChain_reverse_admissible s p.length p hkeys hp hc (Nat.le_refl _)

/-- The actual chain is inside-out even on a simple independent completion. -/
theorem lexChain_reverse_order_witness :
    let a : Name := .num .anonymous 0
    let b : Name := .num .anonymous 1
    let s : NormLevel := (({} : NormLevel).insert [a] { var := [⟨a, 7⟩] }).insert
      [a, b] { var := [⟨b, 0⟩] }
    s.feasible [] [a, b] = true ∧ s.lexChain 2 [a, b] = [b, a] ∧
      (s.lexChain 2 [a, b]).reverse = [a, b] ∧ Completes s [] [a, b] := by
  dsimp only
  exact ⟨rfl, rfl, rfl, feasible_sound _ _ _ rfl⟩

end Vesper.Lean4LeanLevel42
