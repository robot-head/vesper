namespace VesperProofs.Effects
open Vesper

/-- The stateful handler threads its counter: `next` returns 5, then 6. -/
theorem twice_five : twice 5 = 11 := by
  rfl

theorem halve_even : checked_halve (m := Except ArithOverflow) 10 = .ok 5 := by
  rfl

theorem halve_odd : checked_halve (m := Except ArithOverflow) 7 = .error ArithOverflow.mk := by
  rfl

/-- The `Fail` handler turns the raise into its clause's answer. -/
theorem handled_odd : halve_or_zero 7 = 0 ∧ halve_or_zero 10 = 5 := by
  constructor <;> rfl

end VesperProofs.Effects
