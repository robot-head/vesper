namespace VesperProofs.Loops
open Vesper

theorem triangle_ten :
    («triangle$fuel» (m := ExceptT ArithOverflow Option) 20 10).run = some (.ok 55) := by
  rfl

theorem main_value : «main$fuel» (m := Option) 20 = some 55 := by
  rfl

/-- With too little fuel the loop diverges rather than returning a value. -/
theorem short_fuel : («triangle$fuel» (m := ExceptT ArithOverflow Option) 3 10).run = none := by
  rfl

end VesperProofs.Loops
