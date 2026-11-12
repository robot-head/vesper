import Std

/-!
# `vesper.arith.checked/1`

The source integer kinds are `u8`, `u32`, `u64` and `i64`. Under this
contract the checked operators `+`, `-`, `*`, their compound assignments, and
signed negation denote the exact mathematical result when it lies in the
kind's range, and otherwise raise `Fail[ArithOverflow]`. Signed `/` raises
`ArithOverflow` only for `MIN / -1`; `MIN % -1` is `0`. Division by zero and
out-of-bounds indexing raise `Fail[RuntimeError]` instead. `wrapping_add`,
`wrapping_sub` and `wrapping_mul` are total and compute modulo `2^bits`.

The executable extractor (`src/lean.rs`) represents values with Lean's
`UInt8`/`UInt32`/`UInt64`/`Int64`, whose operators wrap. Before each checked
operation it tests a guard written over exact `Nat`/`Int` arithmetic
(`decide (a.toNat + b.toNat ≤ 255)`, `inI64 (a.toInt + b.toInt)`, ...) and
raises when the guard fails. This file proves the property that encoding
relies on: whenever the guard holds, the native wrapping operation equals the
exact result, so the checked denotation and the extracted code agree. The
division and remainder corners are checked by evaluation. No `sorry` or
project axioms are used; the proofs depend only on Lean's standard axioms.
-/

namespace Vesper.CheckedArithmetic

/-- The contract's exact denotation of a checked result in `[lo, hi]`.
`none` is `Fail[ArithOverflow]`. -/
def checked (lo hi : Int) (r : Int) : Option Int :=
  if lo ≤ r ∧ r ≤ hi then some r else none

def inI64 (x : Int) : Bool := decide (-9223372036854775808 ≤ x ∧ x ≤ 9223372036854775807)

/-! ## Unsigned kinds: a passing guard makes the wrapping operation exact -/

theorem u8_add (a b : UInt8) (h : a.toNat + b.toNat ≤ 255) :
    (a + b).toNat = a.toNat + b.toNat := by
  rw [UInt8.toNat_add]; exact Nat.mod_eq_of_lt (by omega)

theorem u8_mul (a b : UInt8) (h : a.toNat * b.toNat ≤ 255) :
    (a * b).toNat = a.toNat * b.toNat := by
  rw [UInt8.toNat_mul]; exact Nat.mod_eq_of_lt (by omega)

theorem u8_sub (a b : UInt8) (h : b ≤ a) : (a - b).toNat = a.toNat - b.toNat :=
  UInt8.toNat_sub_of_le a b h

theorem u32_add (a b : UInt32) (h : a.toNat + b.toNat ≤ 4294967295) :
    (a + b).toNat = a.toNat + b.toNat := by
  rw [UInt32.toNat_add]; exact Nat.mod_eq_of_lt (by omega)

theorem u32_mul (a b : UInt32) (h : a.toNat * b.toNat ≤ 4294967295) :
    (a * b).toNat = a.toNat * b.toNat := by
  rw [UInt32.toNat_mul]; exact Nat.mod_eq_of_lt (by omega)

theorem u32_sub (a b : UInt32) (h : b ≤ a) : (a - b).toNat = a.toNat - b.toNat :=
  UInt32.toNat_sub_of_le a b h

theorem u64_add (a b : UInt64) (h : a.toNat + b.toNat ≤ 18446744073709551615) :
    (a + b).toNat = a.toNat + b.toNat := by
  rw [UInt64.toNat_add]; exact Nat.mod_eq_of_lt (by omega)

theorem u64_mul (a b : UInt64) (h : a.toNat * b.toNat ≤ 18446744073709551615) :
    (a * b).toNat = a.toNat * b.toNat := by
  rw [UInt64.toNat_mul]; exact Nat.mod_eq_of_lt (by omega)

theorem u64_sub (a b : UInt64) (h : b ≤ a) : (a - b).toNat = a.toNat - b.toNat :=
  UInt64.toNat_sub_of_le a b h

/-! ## `i64`: a passing `inI64` guard makes the wrapping operation exact -/

theorem bmod_in_range (x : Int) (h : inI64 x = true) : x.bmod (2 ^ 64) = x := by
  simp [inI64] at h
  exact Int.bmod_eq_of_le (by omega) (by omega)

theorem i64_add (a b : Int64) (h : inI64 (a.toInt + b.toInt) = true) :
    (a + b).toInt = a.toInt + b.toInt := by
  rw [Int64.toInt_add]; exact bmod_in_range _ h

theorem i64_sub (a b : Int64) (h : inI64 (a.toInt - b.toInt) = true) :
    (a - b).toInt = a.toInt - b.toInt := by
  rw [Int64.toInt_sub]; exact bmod_in_range _ h

theorem i64_mul (a b : Int64) (h : inI64 (a.toInt * b.toInt) = true) :
    (a * b).toInt = a.toInt * b.toInt := by
  rw [Int64.toInt_mul]; exact bmod_in_range _ h

theorem i64_neg (a : Int64) (h : inI64 (-a.toInt) = true) : (-a).toInt = -a.toInt := by
  rw [Int64.toInt_neg]; exact bmod_in_range _ h

/-- The guards agree with the contract: a guard holds exactly when the
checked denotation is defined. -/
theorem guard_iff_checked (x : Int) :
    inI64 x = true ↔ checked (-9223372036854775808) 9223372036854775807 x = some x := by
  simp [inI64, checked]

/-! ## Boundaries -/

-- Wrapping intrinsics are total and modular.
example : (255 : UInt8) + 1 = 0 := by decide
example : (0 : UInt32) - 1 = 4294967295 := by decide
example : (9223372036854775807 : Int64) + 1 = -9223372036854775808 := by decide
example : (-9223372036854775808 : Int64) * -1 = -9223372036854775808 := by decide

-- Checked operations at the edge of each range.
example : checked 0 255 (250 + 5) = some 255 := by decide
example : checked 0 255 (250 + 6) = none := by decide
example : checked 0 18446744073709551615 (0 - 1) = none := by decide
example : checked (-9223372036854775808) 9223372036854775807 (-(-9223372036854775808)) = none := by
  decide
example : inI64 ((9223372036854775807 : Int64).toInt + (1 : Int64).toInt) = false := by decide
example : inI64 ((-9223372036854775807 : Int64).toInt - (1 : Int64).toInt) = true := by decide

/-! ## Signed division corners

`/` is guarded by `!(a == MIN && b == -1)`. Lean's `Int64` remainder traps on
`MIN % -1` at run time, so the extractor emits `if b == -1 then 0 else a % b`,
which agrees with the contract. -/

def divGuard (a b : Int64) : Bool := !(a == -9223372036854775808 && b == -1)
def rem (a b : Int64) : Int64 := if b == -1 then 0 else a % b

example : divGuard (-9223372036854775808) (-1) = false := by decide
example : divGuard (-9223372036854775808) 1 = true := by decide
example : divGuard 9223372036854775807 (-1) = true := by decide
example : rem (-9223372036854775808) (-1) = 0 := by decide
example : rem (-7) 2 = -1 := by decide
example : (-7 : Int64) / 2 = -3 := by decide

theorem rem_neg_one (a : Int64) : rem a (-1) = 0 := by simp [rem]

end Vesper.CheckedArithmetic
