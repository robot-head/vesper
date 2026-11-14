namespace VesperProofs.Structs
open Vesper

/-- `swap` exchanges the coordinates of its inout point. -/
theorem swap_exchanges (p : Point) : (swap p).2 = { x := p.y, y := p.x } := by
  simp [swap]

/-- Writing `s.start.x` leaves every disjoint field as it was (§11.2 frames). -/
theorem move_start_frame (s : Segment) (v : UInt64) :
    (move_start s v).2.start.x = v ∧ (move_start s v).2.start.y = s.start.y ∧
      (move_start s v).2.end = s.end := by
  simp [move_start]

theorem main_value : main = 2 := by
  rfl

end VesperProofs.Structs
