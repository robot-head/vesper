/-
Vesper Crucible (design §29.4): termination of choice-tape shrinking.

Model. A choice tape is a `List Nat`. Its shrink key is `(length, sum)` and
the shrink order `Smaller a b` is the lexicographic order on keys, which is the
"shortlex" measure the design states in `choice_tape_shrink_terminates`.

What is proved, about this model only:
* `smaller_wf`: the order is well-founded.
* `choice_tape_shrink_terminates`: the design's statement.
* `delete_span_smaller`, `lower_pointwise_smaller`, `set_lower_smaller`: the
  three reduction kinds the Rust shrinker proposes (span deletion, span
  zeroing / pointwise lowering, single-value lowering) each decrease the order.
* `shrink`: a shrink loop that accepts a proposed candidate only if it is
  `Smaller`; it is a total function (accepted by Lean's termination checker),
  so shrinking terminates for every proposal oracle.
* `shrink_preserves`: if the start tape satisfies an invariant (for Crucible,
  "still fails the property and is generator-valid") and the oracle only
  proposes tapes satisfying it, the result satisfies it.
* `shrink_le`: the result is the start tape or `Smaller` than it.

This is not a proof about the Rust code in crates/vesper-crucible, and it says
nothing about global minimality. The Rust shrinker enforces the same
acceptance rule at run time (`ShrinkKey` comparison before accepting).
-/

namespace CrucibleShrink

def modelId : String := "vesper.crucible.shrink-model/1"
def modelClaim : String :=
  "Lean model of tape shrinking; proofs cover the model, not Rust"

def tapeSum : List Nat → Nat
  | [] => 0
  | x :: xs => x + tapeSum xs

def key (t : List Nat) : Nat × Nat := (t.length, tapeSum t)

def Smaller (a b : List Nat) : Prop :=
  Prod.Lex (· < ·) (· < ·) (key a) (key b)

/-- Executable decision procedure, compared with Rust's `ShrinkKey` order. -/
def smallerB (a b : List Nat) : Bool :=
  decide (a.length < b.length) || (a.length == b.length && decide (tapeSum a < tapeSum b))

theorem smaller_wf : WellFounded Smaller :=
  InvImage.wf key (Prod.lex Nat.lt_wfRel Nat.lt_wfRel).wf

instance : WellFoundedRelation (List Nat) := ⟨Smaller, smaller_wf⟩

theorem choice_tape_shrink_terminates
    (len1 len2 sum1 sum2 : Nat)
    (h_step : len2 < len1 ∨ (len2 = len1 ∧ sum2 < sum1)) :
    Prod.Lex (· < ·) (· < ·) (len2, sum2) (len1, sum1) := by
  rcases h_step with h_lt | ⟨h_eq, h_sum⟩
  · exact Prod.Lex.left _ _ h_lt
  · subst h_eq; exact Prod.Lex.right _ h_sum

theorem smaller_iff (a b : List Nat) :
    Smaller a b ↔ a.length < b.length ∨ (a.length = b.length ∧ tapeSum a < tapeSum b) := by
  unfold Smaller
  rw [Prod.lex_def]
  simp [key]

theorem smallerB_correct (a b : List Nat) : smallerB a b = true ↔ Smaller a b := by
  rw [smaller_iff]
  unfold smallerB
  simp [Bool.or_eq_true, Bool.and_eq_true, beq_iff_eq, decide_eq_true_eq]

theorem length_take_append_drop_lt (t : List Nat) (i k : Nat) (hk : 0 < k)
    (h : i + k ≤ t.length) : (t.take i ++ t.drop (i + k)).length < t.length := by
  simp [List.length_append, List.length_take, List.length_drop]
  omega

/-- Deleting a non-empty span `[i, i + k)` decreases the order. -/
theorem delete_span_smaller (t : List Nat) (i k : Nat) (hk : 0 < k)
    (h : i + k ≤ t.length) : Smaller (t.take i ++ t.drop (i + k)) t :=
  (smaller_iff _ _).2 (Or.inl (length_take_append_drop_lt t i k hk h))

/-- Pointwise `≤`, as produced by zeroing a span or lowering one value. -/
inductive PointwiseLe : List Nat → List Nat → Prop
  | nil : PointwiseLe [] []
  | cons {x y : Nat} {xs ys : List Nat} : x ≤ y → PointwiseLe xs ys →
      PointwiseLe (x :: xs) (y :: ys)

theorem PointwiseLe.length_eq : ∀ {a b : List Nat}, PointwiseLe a b → a.length = b.length
  | _, _, .nil => rfl
  | _, _, .cons _ h => by simp [h.length_eq]

theorem PointwiseLe.sum_le : ∀ {a b : List Nat}, PointwiseLe a b → tapeSum a ≤ tapeSum b
  | _, _, .nil => Nat.le_refl _
  | _, _, .cons hxy h => by
      have := h.sum_le
      simp only [tapeSum]
      omega

theorem PointwiseLe.sum_lt : ∀ {a b : List Nat}, PointwiseLe a b → a ≠ b →
    tapeSum a < tapeSum b
  | _, _, .nil, hne => absurd rfl hne
  | x :: xs, y :: ys, .cons hxy h, hne => by
      simp only [tapeSum]
      by_cases hx : x = y
      · subst hx
        have hne' : xs ≠ ys := fun e => hne (by rw [e])
        have := h.sum_lt hne'
        omega
      · have := h.sum_le
        omega

/-- Lowering any values (pointwise `≤`, at least one strictly) decreases the order. -/
theorem lower_pointwise_smaller (a b : List Nat) (h : PointwiseLe a b) (hne : a ≠ b) :
    Smaller a b :=
  (smaller_iff _ _).2 (Or.inr ⟨h.length_eq, h.sum_lt hne⟩)

theorem PointwiseLe.refl : ∀ (a : List Nat), PointwiseLe a a
  | [] => .nil
  | _ :: xs => .cons (Nat.le_refl _) (PointwiseLe.refl xs)

theorem set_pointwise : ∀ (t : List Nat) (i v : Nat), (h : i < t.length) → v ≤ t[i] →
    PointwiseLe (t.set i v) t
  | [], _, _, h, _ => absurd h (Nat.not_lt_zero _)
  | x :: xs, 0, v, _, hv => by
      simp only [List.set_cons_zero]
      exact .cons hv (PointwiseLe.refl xs)
  | x :: xs, i + 1, v, h, hv => by
      simp only [List.set_cons_succ]
      exact .cons (Nat.le_refl x)
        (set_pointwise xs i v (by simp at h; omega) (by simpa using hv))

theorem set_ne : ∀ (t : List Nat) (i v : Nat), (h : i < t.length) → v < t[i] →
    t.set i v ≠ t
  | [], _, _, h, _ => absurd h (Nat.not_lt_zero _)
  | x :: xs, 0, v, _, hv => by
      simp only [List.set_cons_zero]
      intro e
      injection e with e1
      simp at hv
      omega
  | x :: xs, i + 1, v, h, hv => by
      simp only [List.set_cons_succ]
      intro e
      injection e with _ e2
      exact set_ne xs i v (by simp at h; omega) (by simpa using hv) e2

/-- Replacing one value by a strictly smaller one decreases the order. -/
theorem set_lower_smaller (t : List Nat) (i v : Nat) (h : i < t.length) (hv : v < t[i]) :
    Smaller (t.set i v) t :=
  lower_pointwise_smaller _ _ (set_pointwise t i v h (Nat.le_of_lt hv)) (set_ne t i v h hv)

/-- The shrink loop: an oracle proposes a candidate (the Rust shrinker runs
the property on it and proposes it only if it still fails and is valid); the
loop accepts it only if it is `Smaller`. Lean accepts this definition by
well-founded recursion on `Smaller`, so it terminates for every oracle. -/
def shrink (propose : List Nat → Option (List Nat)) (t : List Nat) : List Nat :=
  match propose t with
  | some c =>
      if _hs : smallerB c t = true then shrink propose c else t
  | none => t
termination_by t
decreasing_by exact (smallerB_correct c t).1 _hs

theorem shrink_preserves (propose : List Nat → Option (List Nat))
    (P : List Nat → Prop) (hprop : ∀ t c, propose t = some c → P c) :
    ∀ t, P t → P (shrink propose t) := by
  intro t
  induction t using smaller_wf.induction with
  | _ t ih =>
    intro ht
    unfold shrink
    split
    · rename_i c hc
      split
      · rename_i hs
        exact ih c ((smallerB_correct c t).1 hs) (hprop t c hc)
      · exact ht
    · exact ht

theorem shrink_le (propose : List Nat → Option (List Nat)) :
    ∀ t, shrink propose t = t ∨ Smaller (shrink propose t) t := by
  intro t
  induction t using smaller_wf.induction with
  | _ t ih =>
    unfold shrink
    split
    · rename_i c _
      split
      · rename_i hb
        have hs := (smallerB_correct c t).1 hb
        rcases ih c hs with h | h
        · rw [h]; exact Or.inr hs
        · right
          -- transitivity of the lexicographic key order
          rw [smaller_iff] at h hs ⊢
          omega
      · exact Or.inl rfl
    · exact Or.inl rfl

end CrucibleShrink

#print axioms CrucibleShrink.smaller_wf
#print axioms CrucibleShrink.choice_tape_shrink_terminates
#print axioms CrucibleShrink.smallerB_correct
#print axioms CrucibleShrink.delete_span_smaller
#print axioms CrucibleShrink.lower_pointwise_smaller
#print axioms CrucibleShrink.set_lower_smaller
#print axioms CrucibleShrink.shrink_preserves
#print axioms CrucibleShrink.shrink_le
