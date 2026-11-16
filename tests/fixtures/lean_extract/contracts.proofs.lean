namespace VesperProofs.Contracts
open Vesper

/-- Pure contract: `clamp x hi ≤ hi` and `≤ x` for every input. -/
theorem clamp_contract : «clamp$contract» := by
  intro x hi
  by_cases h : x > hi <;> simp [ensures_clamp, clamp, h]
  · simp [«$arith_simps»] at *; omega
  · split <;> simp_all [«$arith_simps»]

/-- `Fail[ArithOverflow]` contract under its precondition, in `Except`. -/
theorem add_small_contract : «add_small$contract» := by
  intro a b pre r run
  simp [requires_add_small] at pre
  split at pre <;> simp_all [«$arith_simps»]
  have fits : a.toNat + b.toNat ≤ 18446744073709551615 := by omega
  simp [add_small, fits] at run
  cases run
  simp [ensures_add_small]
  split <;> simp_all [«$arith_simps»] <;> omega

/-- Recursive contract: partial correctness of the fuel companion. -/
theorem countdown_contract : «countdown$contract» := by
  intro n fuel
  revert n
  induction fuel with
  | zero => intro n r h; simp [«countdown$fuel», VesperDiverge.diverge] at h
  | succ fuel ih =>
    intro n r h
    unfold «countdown$fuel» at h
    by_cases z : n = 0
    · simp [z] at h; subst h; rfl
    · simp [z] at h; exact ih (n - 1) r h

end VesperProofs.Contracts
