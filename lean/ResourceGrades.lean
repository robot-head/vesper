/-
Operational resource accounting for a finite call-tree fragment.
Sequential calls reuse stack space; nested frames add to live stack space.
Exact timing admission requires the same cost for both branch outcomes.
This does not model machine instruction latency, recursion, or handler costs.
-/
namespace Vesper.ResourceGrades

inductive Program where
  | tick (cost : Nat)
  | seq (left right : Program)
  | branch (condition : Nat) (left right : Program)
  | frame (bytes : Nat) (body : Program)
  deriving DecidableEq, Repr

abbrev Input := Nat → Bool

def time : Program → Input → Nat
  | .tick n, _ => n
  | .seq a b, input => time a input + time b input
  | .branch c a b, input => if input c then time a input else time b input
  | .frame _ body, input => time body input

def stack : Program → Input → Nat
  | .tick _, _ => 0
  | .seq a b, input => max (stack a input) (stack b input)
  | .branch c a b, input => if input c then stack a input else stack b input
  | .frame bytes body, input => bytes + stack body input

def stackBound : Program → Nat
  | .tick _ => 0
  | .seq a b | .branch _ a b => max (stackBound a) (stackBound b)
  | .frame bytes body => bytes + stackBound body

def exactTime : Program → Option Nat
  | .tick n => some n
  | .seq a b => do
      let x ← exactTime a
      let y ← exactTime b
      pure (x + y)
  | .branch _ a b => do
      let x ← exactTime a
      let y ← exactTime b
      if x = y then some x else none
  | .frame _ body => exactTime body

theorem stack_bound_sound (p : Program) (input : Input) :
    stack p input ≤ stackBound p := by
  induction p with
  | tick n => exact Nat.le_refl _
  | seq a b ha hb => exact Nat.max_le.mpr ⟨Nat.le_trans ha (Nat.le_max_left _ _),
      Nat.le_trans hb (Nat.le_max_right _ _)⟩
  | branch c a b ha hb =>
      simp only [stack, stackBound]
      split
      · exact Nat.le_trans ha (Nat.le_max_left _ _)
      · exact Nat.le_trans hb (Nat.le_max_right _ _)
  | frame bytes body h => exact Nat.add_le_add_left h bytes

theorem exact_time_sound (p : Program) (cost : Nat) (h : exactTime p = some cost)
    (input : Input) : time p input = cost := by
  induction p generalizing cost with
  | tick n => simpa [exactTime, time] using h
  | seq a b ha hb =>
      cases hx : exactTime a with
      | none => simp [exactTime, hx] at h
      | some x =>
          cases hy : exactTime b with
          | none => simp [exactTime, hx, hy] at h
          | some y =>
              simp [exactTime, hx, hy] at h
              simpa [time, ha x hx, hb y hy] using h
  | branch c a b ha hb =>
      cases hx : exactTime a with
      | none => simp [exactTime, hx] at h
      | some x =>
          cases hy : exactTime b with
          | none => simp [exactTime, hx, hy] at h
          | some y =>
              by_cases eq : x = y
              · subst y
                simp [exactTime, hx, hy] at h
                simp [time, ha x hx, hb x hy, h]
              · simp [exactTime, hx, hy, eq] at h
  | frame bytes body ih => exact ih cost h

theorem exact_time_noninterference (p : Program) (cost : Nat)
    (h : exactTime p = some cost) (first second : Input) :
    time p first = time p second := by
  rw [exact_time_sound p cost h first, exact_time_sound p cost h second]

theorem sequential_frames_reuse_space (a b : Nat) (input : Input) :
    stack (.seq (.frame a (.tick 0)) (.frame b (.tick 0))) input = max a b := by
  simp [stack]

theorem nested_frames_add_space (a b : Nat) (input : Input) :
    stack (.frame a (.frame b (.tick 0))) input = a + b := by simp [stack]

theorem sequential_bound_is_not_nested_bound :
    stackBound (.frame 64 (.frame 64 (.tick 0))) > max 64 64 := by decide

theorem unequal_timing_branch_rejected :
    exactTime (.branch 0 (.tick 99) (.tick 100)) = none := by decide

theorem unequal_timing_branch_leaks :
    time (.branch 0 (.tick 99) (.tick 100)) (fun _ => true) ≠
      time (.branch 0 (.tick 99) (.tick 100)) (fun _ => false) := by decide

-- Compatibility of published cost contracts: upper bounds are covariant;
-- exact observations preserve equality, even if the new cost is smaller.
inductive Contract where
  | atMost (bound : Nat)
  | exact (cost : Nat)

def Satisfies (cost : Nat) : Contract → Prop
  | .atMost bound => cost ≤ bound
  | .exact expected => cost = expected

def Compatible : Contract → Contract → Prop
  | .atMost new, .atMost old => new ≤ old
  | .exact new, .exact old => new = old
  | _, _ => False

theorem compatible_contract_preserves_callers (new old : Contract) (cost : Nat)
    (h : Compatible new old) (actual : Satisfies cost new) : Satisfies cost old := by
  cases new <;> cases old <;> simp_all [Compatible, Satisfies]
  exact Nat.le_trans actual h

theorem smaller_exact_cost_is_not_compatible :
    ¬ Compatible (.exact 99) (.exact 100) := by simp [Compatible]

theorem equality_cannot_weaken_to_bound :
    ¬ Compatible (.atMost 100) (.exact 100) := by simp [Compatible]

end Vesper.ResourceGrades

#print axioms Vesper.ResourceGrades.stack_bound_sound
#print axioms Vesper.ResourceGrades.exact_time_sound
#print axioms Vesper.ResourceGrades.exact_time_noninterference
#print axioms Vesper.ResourceGrades.sequential_frames_reuse_space
#print axioms Vesper.ResourceGrades.nested_frames_add_space
#print axioms Vesper.ResourceGrades.sequential_bound_is_not_nested_bound
#print axioms Vesper.ResourceGrades.unequal_timing_branch_rejected
#print axioms Vesper.ResourceGrades.unequal_timing_branch_leaks
#print axioms Vesper.ResourceGrades.compatible_contract_preserves_callers
#print axioms Vesper.ResourceGrades.smaller_exact_cost_is_not_compatible
#print axioms Vesper.ResourceGrades.equality_cannot_weaken_to_bound
