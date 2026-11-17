namespace VesperProofs.InoutContracts
open Vesper

theorem set_cell_contract : «set_cell$contract» := by
  intro before value
  cases before
  simp [ensures_set_cell, set_cell]

theorem replace_contract : «replace$contract» := by
  intro before value
  simp [ensures_replace, replace]

theorem exchange_contract : «exchange$contract» := by
  intro left keep right
  simp [ensures_exchange, exchange]

theorem reset_contract : «reset$contract» := by
  intro before
  simp [ensures_reset, reset]

theorem increment_contract : «increment$contract» := by
  intro before pre result run
  simp [increment] at run
  cases run
  simp [ensures_increment]

theorem drain_contract : «drain$contract» := by
  intro before fuel
  revert before
  induction fuel with
  | zero => intro before result run; simp [«drain$fuel», VesperDiverge.diverge] at run
  | succ fuel ih =>
    intro before result run
    unfold «drain$fuel» at run
    by_cases zero : before = 0
    · simp [zero] at run; cases run; rfl
    · simp [zero] at run
      exact ih (before - 1) result (by simpa only [Prod.eta, Option.bind_fun_some] using run)

theorem replace_input : «replace$admissible» := ⟨5, 42, trivial⟩
theorem increment_input : «increment$admissible» := ⟨0, rfl⟩

end VesperProofs.InoutContracts
