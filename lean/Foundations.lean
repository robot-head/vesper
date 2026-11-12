import Std

namespace Vesper.Foundations

inductive Mode where
  | copy | affine | linear
  deriving DecidableEq, Repr

def rank : Mode → Nat
  | .copy => 0 | .affine => 1 | .linear => 2

def join : Mode → Mode → Mode
  | .linear, _ | _, .linear => .linear
  | .affine, _ | _, .affine => .affine
  | .copy, .copy => .copy

def meet : Mode → Mode → Mode
  | .copy, _ | _, .copy => .copy
  | .affine, _ | _, .affine => .affine
  | .linear, .linear => .linear

def allowed : Mode → Nat → Prop
  | .copy, _ => True
  | .affine, n => n ≤ 1
  | .linear, n => n = 1

theorem join_comm (a b : Mode) : join a b = join b a := by
  cases a <;> cases b <;> rfl

theorem join_assoc (a b c : Mode) : join (join a b) c = join a (join b c) := by
  cases a <;> cases b <;> cases c <;> rfl

theorem join_allowed (a b : Mode) (n : Nat) :
    allowed (join a b) n ↔ allowed a n ∧ allowed b n := by
  cases a <;> cases b <;> simp [join, allowed] <;> omega

theorem submode (a b : Mode) (n : Nat)
    (h : rank a ≤ rank b) (use : allowed b n) : allowed a n := by
  cases a <;> cases b <;> simp_all [rank, allowed] <;> omega

theorem linear_exactly_once (n : Nat) (h : allowed .linear n) : n = 1 := h

theorem linear_no_weakening : ¬ allowed .linear 0 := by
  simp [allowed]

theorem linear_no_contraction : ¬ allowed .linear 2 := by
  simp [allowed]

theorem copy_crossing (m : Mode) : meet .copy m = .copy := by
  cases m <;> rfl

inductive ContinuationState where
  | available | consumed
  deriving DecidableEq

def resume : ContinuationState → Option ContinuationState
  | .available => some .consumed
  | .consumed => none

theorem no_second_resume (s t : ContinuationState) (h : resume s = some t) :
    resume t = none := by
  cases s <;> simp [resume] at h
  subst t
  rfl

/- These are foundations for the mode lattice and one-shot transition protocol.
   They do not assert preservation or progress for the compiler implementation. -/
end Vesper.Foundations
