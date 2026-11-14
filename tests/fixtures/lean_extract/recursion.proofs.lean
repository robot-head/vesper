namespace VesperProofs.Recursion
open Vesper

theorem length_two :
    «length$fuel» (m := Option) 8 (.Cons 7 (.Cons 8 .Nil)) = some 2 := by
  rfl

/-- Partial correctness by induction on fuel: whenever `drain` returns, it
returns zero. -/
theorem drain_returns_zero : ∀ (fuel : Nat) (n r : UInt64),
    «drain$fuel» (m := Option) fuel n = some r → r = 0 := by
  intro fuel
  induction fuel with
  | zero => intro n r h; simp [«drain$fuel», VesperDiverge.diverge] at h
  | succ fuel ih =>
    intro n r h
    unfold «drain$fuel» at h
    by_cases z : n = 0
    · simp [z] at h; exact h.symm
    · simp [z] at h; exact ih _ _ h

/-- Mutual recursion: the companions decide parity on small inputs. -/
theorem parity : «is_even$fuel» (m := Option) 20 10 = some true ∧
    «is_odd$fuel» (m := Option) 20 7 = some true := by
  constructor <;> rfl

/-- Running out of fuel is divergence, never a fabricated result. -/
theorem no_fuel (n : UInt64) : «drain$fuel» (m := Option) 0 n = none := by
  rfl

theorem main_value : «main$fuel» (m := Option) 8 = some 2 := by
  rfl

end VesperProofs.Recursion
