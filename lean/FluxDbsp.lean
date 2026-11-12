import Std

/-!
Handwritten exact-integer model, checked by Lean 4.24. This is not an extractor
or a proof of Rust implementation, bounded arithmetic, compiler or persistence.
The bilinear identity applies pointwise to join contributions; finite support,
row semantics and transactional machine execution are validated by Rust tests.
-/
namespace Vesper.FluxDbsp

theorem bilinear_join_exact (a b da db : Int) :
    a * b + (da * b + a * db + da * db) = (a + da) * (b + db) := by
  simp [Int.add_mul, Int.mul_add, Int.add_assoc, Int.add_left_comm, Int.add_comm]

theorem retraction_cancels (w : Int) : w + (-w) = 0 := by omega

theorem integrate_delta (previous current : Int) :
    previous + (current - previous) = current := by omega

theorem frontier_chain_monotonic (t0 t1 t2 : Int)
    (h01 : t0 ≤ t1) (h12 : t1 ≤ t2) : t0 ≤ t2 := by omega

/-- A published bound respects every live hold, not only a chosen ticket. -/
def respects (watermark : Int) (holds : List Int) : Prop :=
  ∀ t ∈ holds, watermark ≤ t

theorem advance_preserves_live_bound (watermark old next : Int) (rest : List Int)
    (h : respects watermark (old :: rest)) (advance : old ≤ next) :
    respects watermark (next :: rest) := by
  intro t ht
  simp only [List.mem_cons] at ht
  rcases ht with rfl | ht
  · have h_old := h old (by simp)
    omega
  · exact h t (by simp [ht])

theorem retirement_preserves_live_bound (watermark old : Int) (rest : List Int)
    (h : respects watermark (old :: rest)) : respects watermark rest := by
  intro t ht
  exact h t (by simp [ht])

#print axioms bilinear_join_exact
#print axioms retraction_cancels
#print axioms integrate_delta
#print axioms frontier_chain_monotonic
#print axioms advance_preserves_live_bound
#print axioms retirement_preserves_live_bound
end Vesper.FluxDbsp
