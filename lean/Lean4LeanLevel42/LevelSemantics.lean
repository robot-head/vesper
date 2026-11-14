import Lean4LeanLevel42.Semantics

/-! Structural denotation and proof measure for actual `Lean.Level` values.
Metavariables are totalized at zero; correctness uses structural `NoMVars`.
Neither the domain nor the measure reads cached level data. -/
namespace Vesper.Lean4LeanLevel42

open Lean

def levelValue (ρ : Valuation) : Level → Nat
  | .zero => 0
  | .succ u => levelValue ρ u + 1
  | .max u v => max (levelValue ρ u) (levelValue ρ v)
  | .imax u v => Nat.imax (levelValue ρ u) (levelValue ρ v)
  | .param v => ρ v
  | .mvar _ => 0

def NoMVars : Level → Prop
  | .zero | .param _ => True
  | .mvar _ => False
  | .succ u => NoMVars u
  | .max u v | .imax u v => NoMVars u ∧ NoMVars v

def levelSize : Level → Nat
  | .zero | .param _ | .mvar _ => 1
  | .succ u => levelSize u + 1
  | .max u v | .imax u v => levelSize u + levelSize v + 1

theorem levelValue_zero (ρ : Valuation) : levelValue ρ .zero = 0 := rfl

theorem levelValue_succ (ρ : Valuation) (u : Level) :
    levelValue ρ (.succ u) = levelValue ρ u + 1 := rfl

theorem levelValue_max (ρ : Valuation) (u v : Level) :
    levelValue ρ (.max u v) = max (levelValue ρ u) (levelValue ρ v) := rfl

theorem levelValue_imax (ρ : Valuation) (u v : Level) :
    levelValue ρ (.imax u v) = Nat.imax (levelValue ρ u) (levelValue ρ v) := rfl

theorem levelValue_param (ρ : Valuation) (v : Name) : levelValue ρ (.param v) = ρ v := rfl

theorem levelValue_mvar (ρ : Valuation) (m : LMVarId) : levelValue ρ (.mvar m) = 0 := rfl

theorem levelSize_pos (l : Level) : 0 < levelSize l := by
  cases l <;> simp [levelSize]

theorem levelSize_imax_max_left (u v w : Level) :
    levelSize (.imax u v) < levelSize (.imax u (.max v w)) := by
  simp only [levelSize]
  have := levelSize_pos w
  omega

theorem levelSize_imax_max_right (u v w : Level) :
    levelSize (.imax u w) < levelSize (.imax u (.max v w)) := by
  simp only [levelSize]
  have := levelSize_pos v
  omega

theorem levelSize_imax_imax_left (u v w : Level) :
    levelSize (.imax u w) < levelSize (.imax u (.imax v w)) := by
  simp only [levelSize]
  have := levelSize_pos v
  omega

theorem levelSize_imax_imax_right (u v w : Level) :
    levelSize (.imax v w) < levelSize (.imax u (.imax v w)) := by
  simp only [levelSize]
  have := levelSize_pos u
  omega

theorem nat_imax_zero (a : Nat) : Nat.imax a 0 = 0 := by
  simp [Nat.imax]

theorem nat_imax_succ (a b : Nat) : Nat.imax a (b + 1) = max a (b + 1) := by
  simp [Nat.imax]

theorem nat_imax_max (a b c : Nat) :
    Nat.imax a (max b c) = max (Nat.imax a b) (Nat.imax a c) := by
  cases b with
  | zero => simp [Nat.imax]
  | succ b =>
    cases c with
    | zero => simp [Nat.imax]
    | succ c =>
      simp only [Nat.imax, Nat.succ_ne_zero, ↓reduceIte, Nat.max_def]
      repeat' first | omega | split

theorem nat_imax_imax (a b c : Nat) :
    Nat.imax a (Nat.imax b c) = max (Nat.imax a c) (Nat.imax b c) := by
  cases c with
  | zero => simp [Nat.imax]
  | succ c =>
    have h : max b (c + 1) ≠ 0 := by omega
    simp only [Nat.imax, Nat.succ_ne_zero, h, ↓reduceIte]
    simp only [Nat.max_def]
    repeat' first | omega | split

theorem max_add_offset (a b k : Nat) : max a b + k = max (a + k) (b + k) := by
  exact (Nat.add_max_add_right a b k).symm

theorem nat_imax_add_offset (a b k : Nat) :
    Nat.imax a b + k = max k (if 0 < b then max (a + k) (b + k) else 0) := by
  cases b with
  | zero => simp [Nat.imax]
  | succ b =>
    simp only [Nat.imax, Nat.succ_ne_zero, ↓reduceIte, Nat.zero_lt_succ]
    rw [max_add_offset]
    omega

end Vesper.Lean4LeanLevel42
