import sys
WS=[0x20,100,100,100,17,100,113,2,100,9,10,112,100,13,100,100]
OP=[0,0,0,0,0,0,0,0,0,0,ord(':'),ord('{'),ord(','),ord('}'),0,0]
def table(name, vals, doc):
    lines=[f"/-- {doc} -/", f"def {name} (n : BitVec 8) : BitVec 8 :="]
    for i,v in enumerate(vals[:-1]):
        kw = "if" if i==0 else "else if"
        lines.append(f"  {kw} n = {i}#8 then {v}#8")
    lines.append(f"  else {vals[-1]}#8")
    return "\n".join(lines)
def nest(ts):
    out = ts[-1]
    for t in reversed(ts[:-1]):
        out = f"{t} ^^^\n    ({out})"
    return out
clmul_terms = nest([f"(if y.getLsbD {j} then x <<< {j} else 0#64)" for j in range(64)])
xor_terms = nest([f"x <<< {j}" for j in range(64)])
out = f'''/-
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
   carriage return; exactly `{{ }} [ ] : ,` once control bytes are masked out;
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
    {clmul_terms}

/-- The carry-less product by all ones, written out: xor of every shift. -/
def xorShifts (x : BitVec 64) : BitVec 64 :=
    {xor_terms}

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

{table("wsTable", WS, "`WS_TABLE` in `kernels.rs`.")}

{table("opTable", OP, "`OP_TABLE` in `kernels.rs`.")}

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
'''
sys.stdout.write(out)
