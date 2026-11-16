namespace VesperProofs.LabeledFailContracts
open Vesper

theorem choose_contract : «choose$contract» := by
  intro n reject result run
  cases reject with
  | false =>
    simp [choose] at run
    cases run
    simp [ensures_choose]
  | true =>
    change Except.error n = Except.ok result at run
    contradiction

theorem forward_contract : «forward$contract» := by
  intro n reject result run
  exact choose_contract n reject result (by simpa [forward] using run)

theorem raised_error_retains_payload : choose (m := Except UInt64) 13 true = .error 13 := by
  rfl

theorem success_retains_result : choose (m := Except UInt64) 42 false = .ok 42 := by
  rfl

theorem descend_contract : «descend$contract» := by
  intro n reject fuel
  revert n reject
  induction fuel with
  | zero =>
    intro n reject result run
    change (none : Option (Except UInt64 UInt64)) = some (.ok result) at run
    contradiction
  | succ fuel ih =>
    intro n reject result run
    unfold «descend$fuel» at run
    by_cases zero : n = 0
    · cases reject with
      | false =>
        simp [zero, ExceptT.run] at run
        cases run
        rfl
      | true =>
        simp [zero, ExceptT.run] at run
        change some (Except.error (7 : UInt64)) = some (Except.ok result) at run
        simp at run
    · simp [zero, ExceptT.run] at run
      exact ih (n - 1) reject result run

theorem choose_input : «choose$admissible» := ⟨42, false, trivial⟩

end VesperProofs.LabeledFailContracts
