namespace VesperProofs.ShadowedValues
open Vesper

theorem pack_contract : «pack$contract» := by
  intro before choose
  cases choose <;> simp [ensures_pack, pack]

theorem local_contract : «local$contract» := by
  intro n
  by_cases zero : n = 0 <;> simp [ensures_local, «local», zero]

theorem global_value_still_callable : value 42 = 42 := by rfl

theorem global_function_value_still_callable : global_callable = 42 := by
  simp [global_callable, value]

theorem envelope_value (T : Type) (payload : T) (before : UInt64) :
    (envelope payload before).value = before := by
  simp [envelope]

end VesperProofs.ShadowedValues
