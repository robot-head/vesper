/-
Stage 1 escape detection of design §20.3: the branchless odd-backslash-run
kernel satisfies the bit-serial recurrence of the scalar reference, for every
64-bit backslash mask and both carry-in values.

`computeEscapedMask` transliterates `escaped_mask_branchless` in
`crates/vesper-serde/src/simd/mod.rs` (the simdjson kernel). The reference
`compute_escaped_mask_64` in `src/helpers.rs` is the unique mask `e` with
  e[0] = carry_in,  e[i+1] = bs[i] && !e[i],  carry_out = bs[63] && !e[63]
(byte i+1 is escaped iff byte i is an unescaped backslash), so proving that
recurrence for the kernel's output proves the two equal.

These are theorems about the Lean definitions only, not about the compiled
Rust code, whose agreement is tested separately (tests/simd_stage1.rs).
-/
import Std.Tactic.BVDecide

namespace SimdJsonEscape

def evenBits : BitVec 64 := 0x5555555555555555#64

/-- Branchless kernel: `(escaped, carry_out)` for backslash mask `bs` and a
carry-in `prev` that is `0` or `1` (byte 0 escaped). -/
def computeEscapedMask (bs prev : BitVec 64) : BitVec 64 × Bool :=
  let b := bs &&& ~~~prev
  let follows := (b <<< 1) ||| prev
  let oddStarts := b &&& ~~~evenBits &&& ~~~follows
  let sum := oddStarts + b
  ((evenBits ^^^ (sum <<< 1)) &&& follows, BitVec.ult sum oddStarts)

theorem escaped_first (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 0 = prev.getLsbD 0 := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_0 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 1 =
      (bs.getLsbD 0 && !(computeEscapedMask bs prev).1.getLsbD 0) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_1 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 2 =
      (bs.getLsbD 1 && !(computeEscapedMask bs prev).1.getLsbD 1) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_2 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 3 =
      (bs.getLsbD 2 && !(computeEscapedMask bs prev).1.getLsbD 2) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_3 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 4 =
      (bs.getLsbD 3 && !(computeEscapedMask bs prev).1.getLsbD 3) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_4 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 5 =
      (bs.getLsbD 4 && !(computeEscapedMask bs prev).1.getLsbD 4) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_5 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 6 =
      (bs.getLsbD 5 && !(computeEscapedMask bs prev).1.getLsbD 5) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_6 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 7 =
      (bs.getLsbD 6 && !(computeEscapedMask bs prev).1.getLsbD 6) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_7 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 8 =
      (bs.getLsbD 7 && !(computeEscapedMask bs prev).1.getLsbD 7) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_8 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 9 =
      (bs.getLsbD 8 && !(computeEscapedMask bs prev).1.getLsbD 8) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_9 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 10 =
      (bs.getLsbD 9 && !(computeEscapedMask bs prev).1.getLsbD 9) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_10 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 11 =
      (bs.getLsbD 10 && !(computeEscapedMask bs prev).1.getLsbD 10) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_11 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 12 =
      (bs.getLsbD 11 && !(computeEscapedMask bs prev).1.getLsbD 11) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_12 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 13 =
      (bs.getLsbD 12 && !(computeEscapedMask bs prev).1.getLsbD 12) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_13 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 14 =
      (bs.getLsbD 13 && !(computeEscapedMask bs prev).1.getLsbD 13) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_14 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 15 =
      (bs.getLsbD 14 && !(computeEscapedMask bs prev).1.getLsbD 14) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_15 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 16 =
      (bs.getLsbD 15 && !(computeEscapedMask bs prev).1.getLsbD 15) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_16 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 17 =
      (bs.getLsbD 16 && !(computeEscapedMask bs prev).1.getLsbD 16) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_17 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 18 =
      (bs.getLsbD 17 && !(computeEscapedMask bs prev).1.getLsbD 17) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_18 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 19 =
      (bs.getLsbD 18 && !(computeEscapedMask bs prev).1.getLsbD 18) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_19 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 20 =
      (bs.getLsbD 19 && !(computeEscapedMask bs prev).1.getLsbD 19) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_20 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 21 =
      (bs.getLsbD 20 && !(computeEscapedMask bs prev).1.getLsbD 20) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_21 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 22 =
      (bs.getLsbD 21 && !(computeEscapedMask bs prev).1.getLsbD 21) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_22 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 23 =
      (bs.getLsbD 22 && !(computeEscapedMask bs prev).1.getLsbD 22) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_23 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 24 =
      (bs.getLsbD 23 && !(computeEscapedMask bs prev).1.getLsbD 23) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_24 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 25 =
      (bs.getLsbD 24 && !(computeEscapedMask bs prev).1.getLsbD 24) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_25 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 26 =
      (bs.getLsbD 25 && !(computeEscapedMask bs prev).1.getLsbD 25) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_26 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 27 =
      (bs.getLsbD 26 && !(computeEscapedMask bs prev).1.getLsbD 26) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_27 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 28 =
      (bs.getLsbD 27 && !(computeEscapedMask bs prev).1.getLsbD 27) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_28 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 29 =
      (bs.getLsbD 28 && !(computeEscapedMask bs prev).1.getLsbD 28) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_29 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 30 =
      (bs.getLsbD 29 && !(computeEscapedMask bs prev).1.getLsbD 29) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_30 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 31 =
      (bs.getLsbD 30 && !(computeEscapedMask bs prev).1.getLsbD 30) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_31 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 32 =
      (bs.getLsbD 31 && !(computeEscapedMask bs prev).1.getLsbD 31) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_32 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 33 =
      (bs.getLsbD 32 && !(computeEscapedMask bs prev).1.getLsbD 32) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_33 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 34 =
      (bs.getLsbD 33 && !(computeEscapedMask bs prev).1.getLsbD 33) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_34 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 35 =
      (bs.getLsbD 34 && !(computeEscapedMask bs prev).1.getLsbD 34) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_35 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 36 =
      (bs.getLsbD 35 && !(computeEscapedMask bs prev).1.getLsbD 35) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_36 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 37 =
      (bs.getLsbD 36 && !(computeEscapedMask bs prev).1.getLsbD 36) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_37 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 38 =
      (bs.getLsbD 37 && !(computeEscapedMask bs prev).1.getLsbD 37) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_38 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 39 =
      (bs.getLsbD 38 && !(computeEscapedMask bs prev).1.getLsbD 38) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_39 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 40 =
      (bs.getLsbD 39 && !(computeEscapedMask bs prev).1.getLsbD 39) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_40 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 41 =
      (bs.getLsbD 40 && !(computeEscapedMask bs prev).1.getLsbD 40) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_41 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 42 =
      (bs.getLsbD 41 && !(computeEscapedMask bs prev).1.getLsbD 41) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_42 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 43 =
      (bs.getLsbD 42 && !(computeEscapedMask bs prev).1.getLsbD 42) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_43 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 44 =
      (bs.getLsbD 43 && !(computeEscapedMask bs prev).1.getLsbD 43) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_44 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 45 =
      (bs.getLsbD 44 && !(computeEscapedMask bs prev).1.getLsbD 44) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_45 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 46 =
      (bs.getLsbD 45 && !(computeEscapedMask bs prev).1.getLsbD 45) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_46 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 47 =
      (bs.getLsbD 46 && !(computeEscapedMask bs prev).1.getLsbD 46) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_47 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 48 =
      (bs.getLsbD 47 && !(computeEscapedMask bs prev).1.getLsbD 47) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_48 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 49 =
      (bs.getLsbD 48 && !(computeEscapedMask bs prev).1.getLsbD 48) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_49 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 50 =
      (bs.getLsbD 49 && !(computeEscapedMask bs prev).1.getLsbD 49) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_50 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 51 =
      (bs.getLsbD 50 && !(computeEscapedMask bs prev).1.getLsbD 50) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_51 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 52 =
      (bs.getLsbD 51 && !(computeEscapedMask bs prev).1.getLsbD 51) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_52 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 53 =
      (bs.getLsbD 52 && !(computeEscapedMask bs prev).1.getLsbD 52) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_53 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 54 =
      (bs.getLsbD 53 && !(computeEscapedMask bs prev).1.getLsbD 53) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_54 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 55 =
      (bs.getLsbD 54 && !(computeEscapedMask bs prev).1.getLsbD 54) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_55 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 56 =
      (bs.getLsbD 55 && !(computeEscapedMask bs prev).1.getLsbD 55) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_56 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 57 =
      (bs.getLsbD 56 && !(computeEscapedMask bs prev).1.getLsbD 56) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_57 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 58 =
      (bs.getLsbD 57 && !(computeEscapedMask bs prev).1.getLsbD 57) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_58 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 59 =
      (bs.getLsbD 58 && !(computeEscapedMask bs prev).1.getLsbD 58) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_59 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 60 =
      (bs.getLsbD 59 && !(computeEscapedMask bs prev).1.getLsbD 59) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_60 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 61 =
      (bs.getLsbD 60 && !(computeEscapedMask bs prev).1.getLsbD 60) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_61 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 62 =
      (bs.getLsbD 61 && !(computeEscapedMask bs prev).1.getLsbD 61) := by
  simp only [computeEscapedMask]
  bv_decide

theorem escaped_succ_62 (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 63 =
      (bs.getLsbD 62 && !(computeEscapedMask bs prev).1.getLsbD 62) := by
  simp only [computeEscapedMask]
  bv_decide

theorem carry_out (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).2 =
      (bs.getLsbD 63 && !(computeEscapedMask bs prev).1.getLsbD 63) := by
  simp only [computeEscapedMask]
  bv_decide

/-- The kernel's output satisfies the scalar reference's defining recurrence. -/
theorem odd_backslash_simd_eq_scalar (bs prev : BitVec 64) (hp : prev = 0#64 ∨ prev = 1#64) :
    (computeEscapedMask bs prev).1.getLsbD 0 = prev.getLsbD 0 ∧
      (∀ i, i < 63 →
        (computeEscapedMask bs prev).1.getLsbD (i + 1) =
          (bs.getLsbD i && !(computeEscapedMask bs prev).1.getLsbD i)) ∧
      (computeEscapedMask bs prev).2 =
        (bs.getLsbD 63 && !(computeEscapedMask bs prev).1.getLsbD 63) := by
  refine ⟨escaped_first bs prev hp, fun i h => ?_, carry_out bs prev hp⟩
  match i, h with
  | 0, _ => escaped_succ_0 bs prev hp
  | 1, _ => escaped_succ_1 bs prev hp
  | 2, _ => escaped_succ_2 bs prev hp
  | 3, _ => escaped_succ_3 bs prev hp
  | 4, _ => escaped_succ_4 bs prev hp
  | 5, _ => escaped_succ_5 bs prev hp
  | 6, _ => escaped_succ_6 bs prev hp
  | 7, _ => escaped_succ_7 bs prev hp
  | 8, _ => escaped_succ_8 bs prev hp
  | 9, _ => escaped_succ_9 bs prev hp
  | 10, _ => escaped_succ_10 bs prev hp
  | 11, _ => escaped_succ_11 bs prev hp
  | 12, _ => escaped_succ_12 bs prev hp
  | 13, _ => escaped_succ_13 bs prev hp
  | 14, _ => escaped_succ_14 bs prev hp
  | 15, _ => escaped_succ_15 bs prev hp
  | 16, _ => escaped_succ_16 bs prev hp
  | 17, _ => escaped_succ_17 bs prev hp
  | 18, _ => escaped_succ_18 bs prev hp
  | 19, _ => escaped_succ_19 bs prev hp
  | 20, _ => escaped_succ_20 bs prev hp
  | 21, _ => escaped_succ_21 bs prev hp
  | 22, _ => escaped_succ_22 bs prev hp
  | 23, _ => escaped_succ_23 bs prev hp
  | 24, _ => escaped_succ_24 bs prev hp
  | 25, _ => escaped_succ_25 bs prev hp
  | 26, _ => escaped_succ_26 bs prev hp
  | 27, _ => escaped_succ_27 bs prev hp
  | 28, _ => escaped_succ_28 bs prev hp
  | 29, _ => escaped_succ_29 bs prev hp
  | 30, _ => escaped_succ_30 bs prev hp
  | 31, _ => escaped_succ_31 bs prev hp
  | 32, _ => escaped_succ_32 bs prev hp
  | 33, _ => escaped_succ_33 bs prev hp
  | 34, _ => escaped_succ_34 bs prev hp
  | 35, _ => escaped_succ_35 bs prev hp
  | 36, _ => escaped_succ_36 bs prev hp
  | 37, _ => escaped_succ_37 bs prev hp
  | 38, _ => escaped_succ_38 bs prev hp
  | 39, _ => escaped_succ_39 bs prev hp
  | 40, _ => escaped_succ_40 bs prev hp
  | 41, _ => escaped_succ_41 bs prev hp
  | 42, _ => escaped_succ_42 bs prev hp
  | 43, _ => escaped_succ_43 bs prev hp
  | 44, _ => escaped_succ_44 bs prev hp
  | 45, _ => escaped_succ_45 bs prev hp
  | 46, _ => escaped_succ_46 bs prev hp
  | 47, _ => escaped_succ_47 bs prev hp
  | 48, _ => escaped_succ_48 bs prev hp
  | 49, _ => escaped_succ_49 bs prev hp
  | 50, _ => escaped_succ_50 bs prev hp
  | 51, _ => escaped_succ_51 bs prev hp
  | 52, _ => escaped_succ_52 bs prev hp
  | 53, _ => escaped_succ_53 bs prev hp
  | 54, _ => escaped_succ_54 bs prev hp
  | 55, _ => escaped_succ_55 bs prev hp
  | 56, _ => escaped_succ_56 bs prev hp
  | 57, _ => escaped_succ_57 bs prev hp
  | 58, _ => escaped_succ_58 bs prev hp
  | 59, _ => escaped_succ_59 bs prev hp
  | 60, _ => escaped_succ_60 bs prev hp
  | 61, _ => escaped_succ_61 bs prev hp
  | 62, _ => escaped_succ_62 bs prev hp
  | n + 63, h => absurd h (by omega)

end SimdJsonEscape
