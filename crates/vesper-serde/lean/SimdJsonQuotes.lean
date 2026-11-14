/-
Stage 1 quote parity and byte classification of design §20.3, for the
kernels in `crates/vesper-serde/src/simd/` (`mod.rs` and `kernels.rs`).

1. Quote parity. `clmulLow x y` is the low 64 bits of the carry-less
   product of `x` and `y` (what PCLMULQDQ, PMULL and, per 128-bit lane,
   VPCLMULQDQ compute); with `y` all ones it equals the shift cascade
   `prefixXorShifts`, and both satisfy the defining recurrence of the
   per-bit reference (`reference_block` in `mod.rs`): bit 0 is `x[0]` and
   bit i+1 is bit i xor `x[i+1]`. With the block carry `c` (all ones when
   the previous block ended inside a string, else zero) the string-interior
   mask `s = prefix(q) ^ c` satisfies `s[0] = q[0] xor c`,
   `s[i+1] = s[i] xor q[i+1]`, and the next carry `sshiftRight s 63` is all
   ones exactly when `s[63]` is set.
2. Classification tables. For every byte, the nibble-indexed whitespace and
   operator tables (`WS_TABLE`, `OP_TABLE`) with x86 `pshufb` semantics
   (index bit 7 set gives 0) and with AArch64 `tbl` on the low nibble, plus
   the control-byte test, classify exactly space, tab, line feed and
   carriage return; exactly `{ } [ ] : ,` once control bytes are masked out;
   and exactly bytes 0x00..=0x1f.

These are theorems about the Lean definitions only. They are not theorems
about the compiled Rust code, whose agreement with the scalar reference is
tested separately (tests/simd_stage1.rs); the tables below are transcribed
from `kernels.rs` and checked against it by tests/lean_models.rs.
-/
import Std.Tactic.BVDecide

namespace SimdJsonQuotes

def allOnes : BitVec 64 := 0xffffffffffffffff#64

/-- Low 64 bits of the carry-less product: xor of `x <<< j` for set bits `j`
of `y`. -/
def clmulLow (x y : BitVec 64) : BitVec 64 :=
    (if y.getLsbD 0 then x <<< 0 else 0#64) ^^^
    ((if y.getLsbD 1 then x <<< 1 else 0#64) ^^^
    ((if y.getLsbD 2 then x <<< 2 else 0#64) ^^^
    ((if y.getLsbD 3 then x <<< 3 else 0#64) ^^^
    ((if y.getLsbD 4 then x <<< 4 else 0#64) ^^^
    ((if y.getLsbD 5 then x <<< 5 else 0#64) ^^^
    ((if y.getLsbD 6 then x <<< 6 else 0#64) ^^^
    ((if y.getLsbD 7 then x <<< 7 else 0#64) ^^^
    ((if y.getLsbD 8 then x <<< 8 else 0#64) ^^^
    ((if y.getLsbD 9 then x <<< 9 else 0#64) ^^^
    ((if y.getLsbD 10 then x <<< 10 else 0#64) ^^^
    ((if y.getLsbD 11 then x <<< 11 else 0#64) ^^^
    ((if y.getLsbD 12 then x <<< 12 else 0#64) ^^^
    ((if y.getLsbD 13 then x <<< 13 else 0#64) ^^^
    ((if y.getLsbD 14 then x <<< 14 else 0#64) ^^^
    ((if y.getLsbD 15 then x <<< 15 else 0#64) ^^^
    ((if y.getLsbD 16 then x <<< 16 else 0#64) ^^^
    ((if y.getLsbD 17 then x <<< 17 else 0#64) ^^^
    ((if y.getLsbD 18 then x <<< 18 else 0#64) ^^^
    ((if y.getLsbD 19 then x <<< 19 else 0#64) ^^^
    ((if y.getLsbD 20 then x <<< 20 else 0#64) ^^^
    ((if y.getLsbD 21 then x <<< 21 else 0#64) ^^^
    ((if y.getLsbD 22 then x <<< 22 else 0#64) ^^^
    ((if y.getLsbD 23 then x <<< 23 else 0#64) ^^^
    ((if y.getLsbD 24 then x <<< 24 else 0#64) ^^^
    ((if y.getLsbD 25 then x <<< 25 else 0#64) ^^^
    ((if y.getLsbD 26 then x <<< 26 else 0#64) ^^^
    ((if y.getLsbD 27 then x <<< 27 else 0#64) ^^^
    ((if y.getLsbD 28 then x <<< 28 else 0#64) ^^^
    ((if y.getLsbD 29 then x <<< 29 else 0#64) ^^^
    ((if y.getLsbD 30 then x <<< 30 else 0#64) ^^^
    ((if y.getLsbD 31 then x <<< 31 else 0#64) ^^^
    ((if y.getLsbD 32 then x <<< 32 else 0#64) ^^^
    ((if y.getLsbD 33 then x <<< 33 else 0#64) ^^^
    ((if y.getLsbD 34 then x <<< 34 else 0#64) ^^^
    ((if y.getLsbD 35 then x <<< 35 else 0#64) ^^^
    ((if y.getLsbD 36 then x <<< 36 else 0#64) ^^^
    ((if y.getLsbD 37 then x <<< 37 else 0#64) ^^^
    ((if y.getLsbD 38 then x <<< 38 else 0#64) ^^^
    ((if y.getLsbD 39 then x <<< 39 else 0#64) ^^^
    ((if y.getLsbD 40 then x <<< 40 else 0#64) ^^^
    ((if y.getLsbD 41 then x <<< 41 else 0#64) ^^^
    ((if y.getLsbD 42 then x <<< 42 else 0#64) ^^^
    ((if y.getLsbD 43 then x <<< 43 else 0#64) ^^^
    ((if y.getLsbD 44 then x <<< 44 else 0#64) ^^^
    ((if y.getLsbD 45 then x <<< 45 else 0#64) ^^^
    ((if y.getLsbD 46 then x <<< 46 else 0#64) ^^^
    ((if y.getLsbD 47 then x <<< 47 else 0#64) ^^^
    ((if y.getLsbD 48 then x <<< 48 else 0#64) ^^^
    ((if y.getLsbD 49 then x <<< 49 else 0#64) ^^^
    ((if y.getLsbD 50 then x <<< 50 else 0#64) ^^^
    ((if y.getLsbD 51 then x <<< 51 else 0#64) ^^^
    ((if y.getLsbD 52 then x <<< 52 else 0#64) ^^^
    ((if y.getLsbD 53 then x <<< 53 else 0#64) ^^^
    ((if y.getLsbD 54 then x <<< 54 else 0#64) ^^^
    ((if y.getLsbD 55 then x <<< 55 else 0#64) ^^^
    ((if y.getLsbD 56 then x <<< 56 else 0#64) ^^^
    ((if y.getLsbD 57 then x <<< 57 else 0#64) ^^^
    ((if y.getLsbD 58 then x <<< 58 else 0#64) ^^^
    ((if y.getLsbD 59 then x <<< 59 else 0#64) ^^^
    ((if y.getLsbD 60 then x <<< 60 else 0#64) ^^^
    ((if y.getLsbD 61 then x <<< 61 else 0#64) ^^^
    ((if y.getLsbD 62 then x <<< 62 else 0#64) ^^^
    ((if y.getLsbD 63 then x <<< 63 else 0#64))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))

/-- The carry-less product by all ones, written out: xor of every shift. -/
def xorShifts (x : BitVec 64) : BitVec 64 :=
    x <<< 0 ^^^
    (x <<< 1 ^^^
    (x <<< 2 ^^^
    (x <<< 3 ^^^
    (x <<< 4 ^^^
    (x <<< 5 ^^^
    (x <<< 6 ^^^
    (x <<< 7 ^^^
    (x <<< 8 ^^^
    (x <<< 9 ^^^
    (x <<< 10 ^^^
    (x <<< 11 ^^^
    (x <<< 12 ^^^
    (x <<< 13 ^^^
    (x <<< 14 ^^^
    (x <<< 15 ^^^
    (x <<< 16 ^^^
    (x <<< 17 ^^^
    (x <<< 18 ^^^
    (x <<< 19 ^^^
    (x <<< 20 ^^^
    (x <<< 21 ^^^
    (x <<< 22 ^^^
    (x <<< 23 ^^^
    (x <<< 24 ^^^
    (x <<< 25 ^^^
    (x <<< 26 ^^^
    (x <<< 27 ^^^
    (x <<< 28 ^^^
    (x <<< 29 ^^^
    (x <<< 30 ^^^
    (x <<< 31 ^^^
    (x <<< 32 ^^^
    (x <<< 33 ^^^
    (x <<< 34 ^^^
    (x <<< 35 ^^^
    (x <<< 36 ^^^
    (x <<< 37 ^^^
    (x <<< 38 ^^^
    (x <<< 39 ^^^
    (x <<< 40 ^^^
    (x <<< 41 ^^^
    (x <<< 42 ^^^
    (x <<< 43 ^^^
    (x <<< 44 ^^^
    (x <<< 45 ^^^
    (x <<< 46 ^^^
    (x <<< 47 ^^^
    (x <<< 48 ^^^
    (x <<< 49 ^^^
    (x <<< 50 ^^^
    (x <<< 51 ^^^
    (x <<< 52 ^^^
    (x <<< 53 ^^^
    (x <<< 54 ^^^
    (x <<< 55 ^^^
    (x <<< 56 ^^^
    (x <<< 57 ^^^
    (x <<< 58 ^^^
    (x <<< 59 ^^^
    (x <<< 60 ^^^
    (x <<< 61 ^^^
    (x <<< 62 ^^^
    (x <<< 63)))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))

/-- `prefix_xor_shifts` in `mod.rs`. -/
def prefixXorShifts (x : BitVec 64) : BitVec 64 :=
  let x := x ^^^ (x <<< 1)
  let x := x ^^^ (x <<< 2)
  let x := x ^^^ (x <<< 4)
  let x := x ^^^ (x <<< 8)
  let x := x ^^^ (x <<< 16)
  x ^^^ (x <<< 32)

theorem clmul_ones_eq_xor_shifts (x : BitVec 64) : clmulLow x allOnes = xorShifts x := by
  simp [clmulLow, xorShifts, allOnes]

/-- Shifting the xor of all shifts by one drops `x <<< 0` and adds
`x <<< 64 = 0`. -/
theorem xor_shifts_recurrence (x : BitVec 64) : xorShifts x = x ^^^ (xorShifts x <<< 1) := by
  simp only [xorShifts, BitVec.shiftLeft_xor_distrib, ← BitVec.shiftLeft_add, Nat.reduceAdd]
  simp only [BitVec.shiftLeft_zero]
  have h : x <<< 64 = 0#64 := by simp
  rw [h, BitVec.xor_zero]

/-- The per-bit recurrence in vector form: `p = x ^ (p << 1)` says
`p[0] = x[0]` and `p[i+1] = x[i+1] xor p[i]`. -/
theorem prefix_xor_recurrence (x : BitVec 64) :
    prefixXorShifts x = x ^^^ (prefixXorShifts x <<< 1) := by
  simp only [prefixXorShifts]
  bv_decide

/-- The recurrence has exactly one solution, so it defines the reference. -/
theorem prefix_xor_unique (x p : BitVec 64) (h : p = x ^^^ (p <<< 1)) :
    p = prefixXorShifts x := by
  simp only [prefixXorShifts] at *
  bv_decide

/-- The carry-less multiply used by PCLMULQDQ, PMULL and VPCLMULQDQ equals
the shift cascade and the per-bit parity reference. -/
theorem clmul_ones_eq_shifts (x : BitVec 64) : clmulLow x allOnes = prefixXorShifts x := by
  rw [clmul_ones_eq_xor_shifts]
  exact prefix_xor_unique x _ (xor_shifts_recurrence x)

/-- String interiors of one block given quote mask `q` and carry `c`
(`block_finish` in `mod.rs`). -/
def inString (q c : BitVec 64) : BitVec 64 := clmulLow q allOnes ^^^ c

/-- `s[0] = q[0] xor c[0]` and `s[i+1] = s[i] xor q[i+1]`. -/
theorem in_string_recurrence (q c : BitVec 64) (hc : c = 0#64 ∨ c = allOnes) :
    inString q c = q ^^^ ((inString q c <<< 1) ||| (c &&& 1#64)) := by
  simp only [inString, clmul_ones_eq_shifts]
  simp only [prefixXorShifts, allOnes] at *
  bv_decide

/-- The next block's carry is all ones exactly when the block ends inside
a string. -/
theorem in_string_carry_out (q c : BitVec 64) (hc : c = 0#64 ∨ c = allOnes) :
    (inString q c).sshiftRight 63 =
      if (inString q c).getLsbD 63 then allOnes else 0#64 := by
  simp only [inString, clmul_ones_eq_shifts]
  simp only [prefixXorShifts, allOnes] at *
  bv_decide

/-- `WS_TABLE` in `kernels.rs`. -/
def wsTable (n : BitVec 8) : BitVec 8 :=
  if n = 0#8 then 32#8
  else if n = 1#8 then 100#8
  else if n = 2#8 then 100#8
  else if n = 3#8 then 100#8
  else if n = 4#8 then 17#8
  else if n = 5#8 then 100#8
  else if n = 6#8 then 113#8
  else if n = 7#8 then 2#8
  else if n = 8#8 then 100#8
  else if n = 9#8 then 9#8
  else if n = 10#8 then 10#8
  else if n = 11#8 then 112#8
  else if n = 12#8 then 100#8
  else if n = 13#8 then 13#8
  else if n = 14#8 then 100#8
  else 100#8

/-- `OP_TABLE` in `kernels.rs`. -/
def opTable (n : BitVec 8) : BitVec 8 :=
  if n = 0#8 then 0#8
  else if n = 1#8 then 0#8
  else if n = 2#8 then 0#8
  else if n = 3#8 then 0#8
  else if n = 4#8 then 0#8
  else if n = 5#8 then 0#8
  else if n = 6#8 then 0#8
  else if n = 7#8 then 0#8
  else if n = 8#8 then 0#8
  else if n = 9#8 then 0#8
  else if n = 10#8 then 58#8
  else if n = 11#8 then 123#8
  else if n = 12#8 then 44#8
  else if n = 13#8 then 125#8
  else if n = 14#8 then 0#8
  else 0#8

/-- x86 `pshufb` on one byte: index bit 7 set selects zero. -/
def pshufb (t : BitVec 8 → BitVec 8) (idx : BitVec 8) : BitVec 8 :=
  if idx.getLsbD 7 then 0#8 else t (idx &&& 0x0f#8)

/-- AArch64 `tbl` as the NEON path uses it, on the low nibble. -/
def tblNibble (t : BitVec 8 → BitVec 8) (b : BitVec 8) : BitVec 8 :=
  t (b &&& 0x0f#8)

def isWhitespace (b : BitVec 8) : Prop :=
  b = 0x20#8 ∨ b = 0x09#8 ∨ b = 0x0a#8 ∨ b = 0x0d#8

def isOperator (b : BitVec 8) : Prop :=
  b = 0x7b#8 ∨ b = 0x7d#8 ∨ b = 0x5b#8 ∨ b = 0x5d#8 ∨ b = 0x3a#8 ∨ b = 0x2c#8

/-- `_mm_max_epu8(b, 0x1f) == 0x1f` (x86) and `vcleq_u8(b, 0x1f)` (NEON). -/
def isControl (b : BitVec 8) : Prop := BitVec.ule b 0x1f#8

theorem control_x86 (b : BitVec 8) :
    (if BitVec.ule b 0x1f#8 then 0x1f#8 else b) = 0x1f#8 ↔ isControl b := by
  simp only [isControl]
  bv_decide

theorem whitespace_x86 (b : BitVec 8) : pshufb wsTable b = b ↔ isWhitespace b := by
  simp only [pshufb, wsTable, isWhitespace]
  bv_decide

theorem whitespace_neon (b : BitVec 8) : tblNibble wsTable b = b ↔ isWhitespace b := by
  simp only [tblNibble, wsTable, isWhitespace]
  bv_decide

theorem operator_x86 (b : BitVec 8) :
    (pshufb opTable b = (b ||| 0x20#8) ∧ ¬ isControl b) ↔ isOperator b := by
  simp only [pshufb, opTable, isOperator, isControl]
  bv_decide

theorem operator_neon (b : BitVec 8) :
    (tblNibble opTable b = (b ||| 0x20#8) ∧ ¬ isControl b) ↔ isOperator b := by
  simp only [tblNibble, opTable, isOperator, isControl]
  bv_decide

/-- Why control bytes are masked out of the operator class: the table alone
also matches 0x0c and 0x1a. -/
theorem operator_table_alone_matches_two_controls :
    pshufb opTable 0x0c#8 = (0x0c#8 ||| 0x20#8) ∧
      pshufb opTable 0x1a#8 = (0x1a#8 ||| 0x20#8) := by
  simp only [pshufb, opTable]
  decide

end SimdJsonQuotes
