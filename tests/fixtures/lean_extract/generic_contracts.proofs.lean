namespace VesperProofs.GenericContracts
open Vesper

theorem tag_contract : «tag$contract» := by
  intro T before pre
  simp [ensures_tag, tag]

theorem package_contract : «package$contract» := by
  intro T payload tag
  simp [ensures_package, package]

theorem copy_flag_contract : «copy_flag$contract» := by
  intro T value flag
  simp [ensures_copy_flag, copy_flag]

theorem second_contract : «second$contract» := by
  intro T U first value flag
  simp [ensures_second, second]

theorem phantom_contract : «phantom$contract» := by
  intro T flag
  simp [ensures_phantom, phantom]

theorem checked_tag_contract : «checked_tag$contract» := by
  intro T before pre result run
  have fits : before.tag.toNat + 1 ≤ 18446744073709551615 := by
    simp [requires_checked_tag, UInt64.lt_iff_toNat_lt] at pre
    omega
  simp [checked_tag, fits] at run
  cases run
  simp [ensures_checked_tag]

theorem countdown_contract : «countdown$contract» := by
  intro T token n fuel
  revert token n
  induction fuel with
  | zero => intro token n result run; simp [«countdown$fuel», VesperDiverge.diverge] at run
  | succ fuel ih =>
    intro token n result run
    unfold «countdown$fuel» at run
    by_cases zero : n = 0
    · simp [zero] at run; cases run; rfl
    · simp [zero] at run
      exact ih token (n - 1) result run

theorem tag_input : «tag$admissible» := ⟨Bool, ⟨true, 42⟩, rfl⟩
theorem phantom_input : «phantom$admissible» := ⟨Empty, true, trivial⟩

end VesperProofs.GenericContracts
