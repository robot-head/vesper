namespace VesperProofs.Enums
open Vesper

/-- The rectangle arm multiplies in checked arithmetic and succeeds when the
exact product fits in `u64`. -/
theorem area_rect (w h : UInt64) (fits : w.toNat * h.toNat ≤ 18446744073709551615) :
    area (m := Except ArithOverflow) (.Rect w h) = .ok (w * h) := by
  simp [area, fits]
  rfl

/-- An overflowing product raises `Fail[ArithOverflow]` instead of wrapping. -/
theorem area_rect_overflow (w h : UInt64) (big : ¬ w.toNat * h.toNat ≤ 18446744073709551615) :
    area (m := Except ArithOverflow) (.Rect w h) = .error ArithOverflow.mk := by
  simp [area, big]
  rfl

theorem empty_area : area (m := Except ArithOverflow) .Empty = .ok 0 := by
  rfl

theorem round_iff (s : Shape) : is_round s = true ↔ ∃ r, s = .Circle r := by
  cases s <;> simp [is_round]

theorem main_value : main = 42 := by
  rfl

end VesperProofs.Enums
