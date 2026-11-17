namespace VesperProofs.RecursiveValues
open Vesper

theorem down_contract : «down$contract» := by
  intro n fuel
  revert n
  induction fuel with
  | zero =>
    intro n result run
    change (none : Option (Except UInt64 UInt64)) = some (.ok result) at run
    contradiction
  | succ fuel ih =>
    intro n result run
    unfold «down$fuel» at run
    by_cases zero : n = 0
    · simp [zero, ExceptT.run] at run
      cases run
      rfl
    · simp [zero, «apply», ExceptT.run] at run
      exact ih (n - 1) result run

theorem down_four_fuel : («down$fuel» (m := ExceptT UInt64 Option) 4 3).run = some (.ok 42) := by
  rfl

theorem down_no_fuel : («down$fuel» (m := ExceptT UInt64 Option) 0 3).run = none := by
  rfl

theorem down_insufficient_fuel : («down$fuel» (m := ExceptT UInt64 Option) 3 3).run = none := by
  rfl

theorem stored_callback_four_fuel : («stored$fuel» (m := ExceptT UInt64 Option) 4 3).run = some (.ok 42) := by
  rfl

theorem spin_exhausts : ∀ fuel, («spin$fuel» (m := ExceptT UInt64 Option) fuel).run = none := by
  intro fuel
  induction fuel with
  | zero => rfl
  | succ fuel ih => simpa [«spin$fuel», run] using ih

theorem down_input : «down$admissible» := ⟨3, trivial⟩

end VesperProofs.RecursiveValues
