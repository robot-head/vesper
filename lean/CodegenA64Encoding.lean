import VerifiedCodegen

/-!
# Verified direct codegen: AArch64 instruction encoding (§42.1 pillar 3b)

`VerifiedCodegen.lean` proves the instruction-selection table correct down to
the abstract machine `M`. This file covers the next step for the AArch64
target: the 32-bit words `src/verified_codegen/aarch64.rs` writes for each
`M` instruction. It follows `CodegenRv64Encoding.lean`.

* `Inst` is the A64 subset the emitter uses, named as in the Arm
  Architecture Reference Manual (`LDR (immediate)`, `MOVZ`, `B.cond`,
  `ADDS (shifted register)`, `MADD`, `UDIV`, `CSINC`, `FMOV (general)`,
  `FADD (scalar)`, `STP` pre-index, `RET`, ...).
* `decode` is the specification: for each instruction, the fixed bits of its
  encoding diagram (`sf`, `op`, `S`, the class bits, `opc`, `ftype`, `op2`,
  ...) and its operand fields (`Rd` in bits 4:0, `Rn` in 9:5, `Rm` in 20:16,
  `imm12` in 21:10, `imm19` in 23:5, `imm26` in 25:0, `imm7` in 21:15,
  sign extension of the branch and pair offsets). Bitmask immediates are
  decoded only for `N = 1` (a single 64-bit element), the only form the
  emitter writes; the decoder rejects the others rather than misreading them.
* `encode` is the encoder, field by field like `aarch64.rs`.
* `decode_encode` proves that every well-formed instruction (registers below
  32, every immediate in its field's range) encodes to a word below `2^32`
  that the specification decodes back to the same instruction.
  `bytes_encode` gives the four little-endian bytes.
* `emit` is `aarch64::emit` for one `M` instruction, as a list of items;
  `emit_decodes` proves every word of its layout decodes back.
* `listing` prints the encoded words of `emit` for sample `M` instructions;
  `tests/verified_codegen_lean.rs` checks it line for line against
  `aarch64::encoding_listing()`, which runs the Rust emitter on the same
  instructions. The theorems are about the Lean encoder; the listing ties the
  Rust encoder to it on those samples, not in general.

Branch targets: `emit` resolves every branch against a layout where the abort
exits (labels below `firstFree`) sit just before the instruction and every
other label just after it,
which is how the listing lays them out. Offsets are in instructions (words),
as in the `imm19` and `imm26` fields.
-/

namespace Vesper.VerifiedCodegen.A64

/-! ## The instruction subset -/

/-- Shifted-register data processing: `ADD`, `ADDS`, `SUB`, `SUBS` and the
logical `AND`, 64-bit (`sf = 1`). -/
inductive SOp | add | adds | sub | subs | and
  deriving DecidableEq, Repr
/-- The shift applied to `Rm` (`shift` field: `LSL = 00`, `ASR = 10`). -/
inductive Shift | lsl | asr
  deriving DecidableEq, Repr
/-- Three-source data processing, 64-bit. -/
inductive D3 | madd | msub | smulh | umulh
  deriving DecidableEq, Repr
/-- Two-source data processing, 64-bit. -/
inductive D2 | udiv | sdiv
  deriving DecidableEq, Repr
/-- Add/subtract (immediate), 64-bit. -/
inductive IOp | add | adds | sub
  deriving DecidableEq, Repr
/-- Floating-point data processing (2 source), double precision. -/
inductive FOp | fmul | fdiv | fadd | fsub
  deriving DecidableEq, Repr

inductive Inst
  /-- `LDR Xt, [Xn|SP, #imm*8]` (unsigned offset). -/
  | ldr (rt rn imm : Nat)
  /-- `STR Xt, [Xn|SP, #imm*8]` (unsigned offset). -/
  | str (rt rn imm : Nat)
  | movz (rd hw imm : Nat)
  | movk (rd hw imm : Nat)
  /-- `B.cond` by `off` instructions. -/
  | bcond (cond : Nat) (off : Int)
  | b (off : Int)
  | bl (off : Int)
  | cbz (rt : Nat) (off : Int)
  | cbnz (rt : Nat) (off : Int)
  /-- `op Xd, Xn, Xm, shift #amt`. -/
  | sreg (op : SOp) (sh : Shift) (rd rn rm amt : Nat)
  /-- `op Xd, Xn, Xm, Xa` (`Ra = 31` for `SMULH`/`UMULH`). -/
  | dp3 (op : D3) (rd rn rm ra : Nat)
  | dp2 (op : D2) (rd rn rm : Nat)
  /-- `op Xd|SP, Xn|SP, #imm, LSL #(12*sh)`. -/
  | aimm (op : IOp) (rd rn imm sh : Nat)
  /-- `EOR Xd, Xn, #bitmask(N, immr, imms)`. -/
  | eori (rd rn n immr imms : Nat)
  | csinc (rd rn rm cond : Nat)
  /-- `FMOV Dd, Xn`. -/
  | fmovDX (rd rn : Nat)
  /-- `FMOV Xd, Dn`. -/
  | fmovXD (rd rn : Nat)
  | fp (op : FOp) (rd rn rm : Nat)
  | fneg (rd rn : Nat)
  | fcmp (rn rm : Nat)
  /-- `STP Xt, Xt2, [Xn|SP, #imm*8]!`. -/
  | stpPre (rt rt2 rn : Nat) (imm : Int)
  /-- `LDP Xt, Xt2, [Xn|SP], #imm*8`. -/
  | ldpPost (rt rt2 rn : Nat) (imm : Int)
  | ret (rn : Nat)
  deriving DecidableEq, Repr

/-! ## The specification: the encoding diagrams of the Arm ARM -/

/-- Bits `lo + len - 1 : lo` of a word. -/
def fld (w lo len : Nat) : Nat := w / 2 ^ lo % 2 ^ len

/-- Two's-complement reading of an `n + 1`-bit field. -/
def sext (n : Nat) (x : Nat) : Int :=
  if x < 2 ^ n then (x : Int) else (x : Int) - 2 ^ (n + 1)

/-- Bits 31:24 (`sf op S 01011` or `sf opc 01010`) of the shifted-register
forms. -/
def SOp.code : SOp → Nat
  | .add => 0x8b | .adds => 0xab | .sub => 0xcb | .subs => 0xeb | .and => 0x8a
def Shift.code : Shift → Nat
  | .lsl => 0 | .asr => 2
/-- `op31` (bits 23:21) and `o0` (bit 15). -/
def D3.code : D3 → Nat × Nat
  | .madd => (0, 0) | .msub => (0, 1) | .smulh => (2, 0) | .umulh => (6, 0)
/-- `opcode` (bits 15:10). -/
def D2.code : D2 → Nat
  | .udiv => 2 | .sdiv => 3
/-- Bits 31:23 (`sf op S 100010`). -/
def IOp.code : IOp → Nat
  | .add => 0x122 | .adds => 0x162 | .sub => 0x1a2
/-- `opcode` (bits 15:12). -/
def FOp.code : FOp → Nat
  | .fmul => 0 | .fdiv => 1 | .fadd => 2 | .fsub => 3

def rd (w : Nat) : Nat := fld w 0 5
def rn (w : Nat) : Nat := fld w 5 5
def rm (w : Nat) : Nat := fld w 16 5

/-- A shifted-register instruction once `sf op S` and the shift are known:
bit 21 (`0` for add/subtract, `N = 0` for `AND`) must be clear. -/
def sregD (op : SOp) (sh : Shift) (w : Nat) : Option Inst :=
  match fld w 21 1 with
  | 0 => some (.sreg op sh (rd w) (rn w) (rm w) (fld w 10 6))
  | _ => none

/-- The specification decoder for the subset, on a 32-bit word; `none` for
anything else. It dispatches on bits 31:26 and then on the fixed bits that
distinguish the encodings sharing them; bits 25:22 of the shifted-register
forms are `01 shift` with `shift` `00` (`LSL`) or `10` (`ASR`). -/
def decode (w : Nat) : Option Inst :=
  match fld w 26 6 with
  -- Load/store register (unsigned immediate), size = 11, V = 0, opc 01/00.
  | 0x3e => match fld w 22 4 with
    | 5 => some (.ldr (rd w) (rn w) (fld w 10 12))
    | 4 => some (.str (rd w) (rn w) (fld w 10 12))
    | _ => none
  -- sf = 1, op = 1, S = 0: MOVZ (100101), EOR immediate (100100, opc = 10),
  -- SUB immediate (100010).
  | 0x34 => match fld w 23 3 with
    | 5 => some (.movz (rd w) (fld w 21 2) (fld w 5 16))
    | 4 => match fld w 22 1 with
      | 1 => if fld w 10 6 < 63 then some (.eori (rd w) (rn w) 1 (fld w 16 6) (fld w 10 6))
        else none
      | _ => none
    | 2 => some (.aimm .sub (rd w) (rn w) (fld w 10 12) (fld w 22 1))
    | _ => none
  -- MOVK.
  | 0x3c => match fld w 23 3 with
    | 5 => some (.movk (rd w) (fld w 21 2) (fld w 5 16))
    | _ => none
  -- Conditional branch (immediate): bits 31:24 = 01010100, o0 = 0.
  | 0x15 => match fld w 24 2, fld w 4 1 with
    | 0, 0 => some (.bcond (fld w 0 4) (sext 18 (fld w 5 19)))
    | _, _ => none
  -- Unconditional branch (immediate): B (op = 0), BL (op = 1).
  | 0x05 => some (.b (sext 25 (fld w 0 26)))
  | 0x25 => some (.bl (sext 25 (fld w 0 26)))
  -- Compare and branch (immediate), sf = 1: CBZ (op = 0), CBNZ (op = 1).
  | 0x2d => match fld w 24 2 with
    | 0 => some (.cbz (rd w) (sext 18 (fld w 5 19)))
    | 1 => some (.cbnz (rd w) (sext 18 (fld w 5 19)))
    | _ => none
  -- ADD and AND (shifted register); ADD (immediate).
  | 0x22 => match fld w 22 4 with
    | 12 => sregD .add .lsl w
    | 14 => sregD .add .asr w
    | 8 => sregD .and .lsl w
    | 10 => sregD .and .asr w
    | _ => none
  | 0x24 => match fld w 23 3 with
    | 2 => some (.aimm .add (rd w) (rn w) (fld w 10 12) (fld w 22 1))
    | _ => none
  -- ADDS (shifted register); STP (pre-index) and LDP (post-index), opc = 10.
  | 0x2a => match fld w 22 4 with
    | 12 => sregD .adds .lsl w
    | 14 => sregD .adds .asr w
    | 6 => some (.stpPre (rd w) (fld w 10 5) (rn w) (sext 6 (fld w 15 7)))
    | 3 => some (.ldpPost (rd w) (fld w 10 5) (rn w) (sext 6 (fld w 15 7)))
    | _ => none
  -- ADDS (immediate).
  | 0x2c => match fld w 23 3 with
    | 2 => some (.aimm .adds (rd w) (rn w) (fld w 10 12) (fld w 22 1))
    | _ => none
  -- SUB and SUBS (shifted register).
  | 0x32 => match fld w 22 4 with
    | 12 => sregD .sub .lsl w
    | 14 => sregD .sub .asr w
    | _ => none
  | 0x3a => match fld w 22 4 with
    | 12 => sregD .subs .lsl w
    | 14 => sregD .subs .asr w
    | _ => none
  -- sf = 1, op = 0, 11010: data-processing (3 source) 11 op31, (2 source)
  -- 10110, conditional select 10100.
  | 0x26 => match fld w 21 5 with
    | 24 => match fld w 15 1 with
      | 0 => some (.dp3 .madd (rd w) (rn w) (rm w) (fld w 10 5))
      | _ => some (.dp3 .msub (rd w) (rn w) (rm w) (fld w 10 5))
    | 26 => match fld w 15 1, fld w 10 5 with
      | 0, 31 => some (.dp3 .smulh (rd w) (rn w) (rm w) 31)
      | _, _ => none
    | 30 => match fld w 15 1, fld w 10 5 with
      | 0, 31 => some (.dp3 .umulh (rd w) (rn w) (rm w) 31)
      | _, _ => none
    | 22 => match fld w 10 6 with
      | 2 => some (.dp2 .udiv (rd w) (rn w) (rm w))
      | 3 => some (.dp2 .sdiv (rd w) (rn w) (rm w))
      | _ => none
    | 20 => match fld w 10 2 with
      | 1 => some (.csinc (rd w) (rn w) (rm w) (fld w 12 4))
      | _ => none
    | _ => none
  -- Conversion between floating-point and integer, sf = 1, ftype = 01,
  -- rmode = 00: FMOV Dd, Xn (opcode 111) and FMOV Xd, Dn (opcode 110).
  | 0x27 => match fld w 16 10, fld w 10 6 with
    | 0x267, 0 => some (.fmovDX (rd w) (rn w))
    | 0x266, 0 => some (.fmovXD (rd w) (rn w))
    | _, _ => none
  -- Floating-point data processing, M = 0, S = 0, ftype = 01: 2 source
  -- (bits 11:10 = 10), FNEG (1 source, opcode 000010), FCMP (opcode2 0).
  | 0x07 => match fld w 21 5 with
    | 19 => match fld w 10 2 with
      | 2 => match fld w 12 4 with
        | 0 => some (.fp .fmul (rd w) (rn w) (rm w))
        | 1 => some (.fp .fdiv (rd w) (rn w) (rm w))
        | 2 => some (.fp .fadd (rd w) (rn w) (rm w))
        | 3 => some (.fp .fsub (rd w) (rn w) (rm w))
        | _ => none
      | 0 => match fld w 10 6, fld w 16 5, fld w 0 5 with
        | 16, 1, _ => some (.fneg (rd w) (rn w))
        | 8, _, 0 => some (.fcmp (rn w) (rm w))
        | _, _, _ => none
      | _ => none
    | _ => none
  -- Unconditional branch (register): RET (opc = 0010, op2 = 11111).
  | 0x35 => match fld w 10 16, fld w 0 5 with
    | 0x97c0, 0 => some (.ret (rn w))
    | _, _ => none
  | _ => none

/-! ## The encoder -/

/-- The low `n` bits of an integer's two's-complement form. -/
def bits (n : Nat) (v : Int) : Nat := (v % (2 ^ n : Int)).toNat

def encode : Inst → Nat
  | .ldr rt rn imm => 0x3e5 * 2 ^ 22 + imm * 2 ^ 10 + rn * 2 ^ 5 + rt
  | .str rt rn imm => 0x3e4 * 2 ^ 22 + imm * 2 ^ 10 + rn * 2 ^ 5 + rt
  | .movz rd hw imm => 0x1a5 * 2 ^ 23 + hw * 2 ^ 21 + imm * 2 ^ 5 + rd
  | .movk rd hw imm => 0x1e5 * 2 ^ 23 + hw * 2 ^ 21 + imm * 2 ^ 5 + rd
  | .bcond cond off => 0x54 * 2 ^ 24 + bits 19 off * 2 ^ 5 + cond
  | .b off => 0x05 * 2 ^ 26 + bits 26 off
  | .bl off => 0x25 * 2 ^ 26 + bits 26 off
  | .cbz rt off => 0xb4 * 2 ^ 24 + bits 19 off * 2 ^ 5 + rt
  | .cbnz rt off => 0xb5 * 2 ^ 24 + bits 19 off * 2 ^ 5 + rt
  | .sreg op sh rd rn rm amt =>
    op.code * 2 ^ 24 + sh.code * 2 ^ 22 + rm * 2 ^ 16 + amt * 2 ^ 10 + rn * 2 ^ 5 + rd
  | .dp3 op rd rn rm ra =>
    0x9b * 2 ^ 24 + op.code.1 * 2 ^ 21 + rm * 2 ^ 16 + op.code.2 * 2 ^ 15 + ra * 2 ^ 10 +
      rn * 2 ^ 5 + rd
  | .dp2 op rd rn rm => 0x4d6 * 2 ^ 21 + rm * 2 ^ 16 + op.code * 2 ^ 10 + rn * 2 ^ 5 + rd
  | .aimm op rd rn imm sh => op.code * 2 ^ 23 + sh * 2 ^ 22 + imm * 2 ^ 10 + rn * 2 ^ 5 + rd
  | .eori rd rn n immr imms =>
    0x1a4 * 2 ^ 23 + n * 2 ^ 22 + immr * 2 ^ 16 + imms * 2 ^ 10 + rn * 2 ^ 5 + rd
  | .csinc rd rn rm cond =>
    0x4d4 * 2 ^ 21 + rm * 2 ^ 16 + cond * 2 ^ 12 + 1 * 2 ^ 10 + rn * 2 ^ 5 + rd
  | .fmovDX rd rn => 0x9e67 * 2 ^ 16 + rn * 2 ^ 5 + rd
  | .fmovXD rd rn => 0x9e66 * 2 ^ 16 + rn * 2 ^ 5 + rd
  | .fp op rd rn rm => 0x0f3 * 2 ^ 21 + rm * 2 ^ 16 + op.code * 2 ^ 12 + 2 * 2 ^ 10 +
      rn * 2 ^ 5 + rd
  | .fneg rd rn => 0x0f3 * 2 ^ 21 + 0x50 * 2 ^ 10 + rn * 2 ^ 5 + rd
  | .fcmp rn rm => 0x0f3 * 2 ^ 21 + rm * 2 ^ 16 + 8 * 2 ^ 10 + rn * 2 ^ 5
  | .stpPre rt rt2 rn imm => 0x2a6 * 2 ^ 22 + bits 7 imm * 2 ^ 15 + rt2 * 2 ^ 10 + rn * 2 ^ 5 + rt
  | .ldpPost rt rt2 rn imm =>
    0x2a3 * 2 ^ 22 + bits 7 imm * 2 ^ 15 + rt2 * 2 ^ 10 + rn * 2 ^ 5 + rt
  | .ret rn => 0x3597c0 * 2 ^ 10 + rn * 2 ^ 5

/-- Little-endian bytes of a 32-bit word. -/
def bytes (w : Nat) : List Nat := [w % 256, w / 2 ^ 8 % 256, w / 2 ^ 16 % 256, w / 2 ^ 24]

/-! ## Well-formedness -/

/-- A signed offset that fits an `n + 1`-bit field. -/
def Fits (n : Nat) (v : Int) : Prop := -2 ^ n ≤ v ∧ v < 2 ^ n

def Inst.Wf : Inst → Prop
  | .ldr rt rn imm | .str rt rn imm => rt < 32 ∧ rn < 32 ∧ imm < 4096
  | .movz rd hw imm | .movk rd hw imm => rd < 32 ∧ hw < 4 ∧ imm < 2 ^ 16
  | .bcond cond off => cond < 16 ∧ Fits 18 off
  | .b off | .bl off => Fits 25 off
  | .cbz rt off | .cbnz rt off => rt < 32 ∧ Fits 18 off
  | .sreg _ _ rd rn rm amt => rd < 32 ∧ rn < 32 ∧ rm < 32 ∧ amt < 64
  | .dp3 op rd rn rm ra => rd < 32 ∧ rn < 32 ∧ rm < 32 ∧ ra < 32 ∧
      (op = .smulh ∨ op = .umulh → ra = 31)
  | .dp2 _ rd rn rm | .fp _ rd rn rm => rd < 32 ∧ rn < 32 ∧ rm < 32
  | .aimm _ rd rn imm sh => rd < 32 ∧ rn < 32 ∧ imm < 4096 ∧ sh < 2
  | .eori rd rn n immr imms => rd < 32 ∧ rn < 32 ∧ n = 1 ∧ immr < 64 ∧ imms < 63
  | .csinc rd rn rm cond => rd < 32 ∧ rn < 32 ∧ rm < 32 ∧ cond < 16
  | .fmovDX rd rn | .fmovXD rd rn | .fneg rd rn => rd < 32 ∧ rn < 32
  | .fcmp rn rm => rn < 32 ∧ rm < 32
  | .stpPre rt rt2 rn imm | .ldpPost rt rt2 rn imm =>
    rt < 32 ∧ rt2 < 32 ∧ rn < 32 ∧ Fits 6 imm
  | .ret rn => rn < 32

/-! ## The encoder meets the specification -/

theorem bits19 (v : Int) : bits 19 v < 2 ^ 19 ∧ ((bits 19 v : Nat) : Int) = v % 2 ^ 19 := by
  unfold bits; simp only [Nat.reducePow, Int.reducePow]; omega
theorem bits26 (v : Int) : bits 26 v < 2 ^ 26 ∧ ((bits 26 v : Nat) : Int) = v % 2 ^ 26 := by
  unfold bits; simp only [Nat.reducePow, Int.reducePow]; omega
theorem bits7 (v : Int) : bits 7 v < 2 ^ 7 ∧ ((bits 7 v : Nat) : Int) = v % 2 ^ 7 := by
  unfold bits; simp only [Nat.reducePow, Int.reducePow]; omega

/-- A field holding the low `n + 1` bits of an in-range offset reads back as
the offset (for the three field widths the subset uses). -/
theorem sext18 {e : Nat} {v : Int} (he : e < 2 ^ 19) (hv : (e : Int) = v % 2 ^ 19)
    (hr : Fits 18 v) : sext 18 e = v := by
  unfold sext Fits at *; simp only [Nat.reducePow, Int.reducePow, Nat.reduceAdd] at *
  split <;> omega
theorem sext25 {e : Nat} {v : Int} (he : e < 2 ^ 26) (hv : (e : Int) = v % 2 ^ 26)
    (hr : Fits 25 v) : sext 25 e = v := by
  unfold sext Fits at *; simp only [Nat.reducePow, Int.reducePow, Nat.reduceAdd] at *
  split <;> omega
theorem sext6 {e : Nat} {v : Int} (he : e < 2 ^ 7) (hv : (e : Int) = v % 2 ^ 7)
    (hr : Fits 6 v) : sext 6 e = v := by
  unfold sext Fits at *; simp only [Nat.reducePow, Int.reducePow, Nat.reduceAdd] at *
  split <;> omega

/-- Unfold an encoding's fields to arithmetic for `omega`. -/
macro "fields" : tactic => `(tactic| (
  simp only [encode, fld, rd, rn, rm, SOp.code, Shift.code, D3.code, D2.code, IOp.code,
    FOp.code, Nat.reducePow]
  omega))

/-- Every well-formed instruction encodes to a 32-bit word that the
specification decodes back to the same instruction. Each case computes the
fields the decoder reads on the path to that instruction, rewrites them,
and lets the dispatch reduce. -/
theorem decode_encode (i : Inst) (h : i.Wf) :
    encode i < 2 ^ 32 ∧ decode (encode i) = some i := by
  cases i with
  | ldr t n imm =>
    obtain ⟨h1, h2, h3⟩ := h
    refine ⟨by fields, ?_⟩
    have e0 : fld (encode (.ldr t n imm)) 26 6 = 0x3e := by fields
    have e1 : fld (encode (.ldr t n imm)) 22 4 = 5 := by fields
    have e2 : rd (encode (.ldr t n imm)) = t := by fields
    have e3 : rn (encode (.ldr t n imm)) = n := by fields
    have e4 : fld (encode (.ldr t n imm)) 10 12 = imm := by fields
    unfold decode; rw [e0, e1, e2, e3, e4]; rfl
  | str t n imm =>
    obtain ⟨h1, h2, h3⟩ := h
    refine ⟨by fields, ?_⟩
    have e0 : fld (encode (.str t n imm)) 26 6 = 0x3e := by fields
    have e1 : fld (encode (.str t n imm)) 22 4 = 4 := by fields
    have e2 : rd (encode (.str t n imm)) = t := by fields
    have e3 : rn (encode (.str t n imm)) = n := by fields
    have e4 : fld (encode (.str t n imm)) 10 12 = imm := by fields
    unfold decode; rw [e0, e1, e2, e3, e4]; rfl
  | movz d hw imm =>
    obtain ⟨h1, h2, h3⟩ := h
    refine ⟨by fields, ?_⟩
    have e0 : fld (encode (.movz d hw imm)) 26 6 = 0x34 := by fields
    have e1 : fld (encode (.movz d hw imm)) 23 3 = 5 := by fields
    have e2 : rd (encode (.movz d hw imm)) = d := by fields
    have e3 : fld (encode (.movz d hw imm)) 21 2 = hw := by fields
    have e4 : fld (encode (.movz d hw imm)) 5 16 = imm := by fields
    unfold decode; rw [e0, e1, e2, e3, e4]; rfl
  | movk d hw imm =>
    obtain ⟨h1, h2, h3⟩ := h
    refine ⟨by fields, ?_⟩
    have e0 : fld (encode (.movk d hw imm)) 26 6 = 0x3c := by fields
    have e1 : fld (encode (.movk d hw imm)) 23 3 = 5 := by fields
    have e2 : rd (encode (.movk d hw imm)) = d := by fields
    have e3 : fld (encode (.movk d hw imm)) 21 2 = hw := by fields
    have e4 : fld (encode (.movk d hw imm)) 5 16 = imm := by fields
    unfold decode; rw [e0, e1, e2, e3, e4]; rfl
  | bcond c off =>
    obtain ⟨h1, hoff⟩ := h
    have := bits19 off
    refine ⟨by fields, ?_⟩
    have e0 : fld (encode (.bcond c off)) 26 6 = 0x15 := by fields
    have e1 : fld (encode (.bcond c off)) 24 2 = 0 := by fields
    have e2 : fld (encode (.bcond c off)) 4 1 = 0 := by fields
    have e3 : fld (encode (.bcond c off)) 0 4 = c := by fields
    have e4 : sext 18 (fld (encode (.bcond c off)) 5 19) = off := sext18 (by fields) (by have := bits19 off; fields) hoff
    unfold decode; rw [e0, e1, e2, e3, e4]; rfl
  | b off =>
    have hoff : Fits 25 off := h
    have := bits26 off
    refine ⟨by fields, ?_⟩
    have e0 : fld (encode (.b off)) 26 6 = 0x05 := by fields
    have e1 : sext 25 (fld (encode (.b off)) 0 26) = off := sext25 (by fields) (by have := bits26 off; fields) hoff
    unfold decode; rw [e0, e1]; rfl
  | bl off =>
    have hoff : Fits 25 off := h
    have := bits26 off
    refine ⟨by fields, ?_⟩
    have e0 : fld (encode (.bl off)) 26 6 = 0x25 := by fields
    have e1 : sext 25 (fld (encode (.bl off)) 0 26) = off := sext25 (by fields) (by have := bits26 off; fields) hoff
    unfold decode; rw [e0, e1]; rfl
  | cbz t off =>
    obtain ⟨h1, hoff⟩ := h
    have := bits19 off
    refine ⟨by fields, ?_⟩
    have e0 : fld (encode (.cbz t off)) 26 6 = 0x2d := by fields
    have e1 : fld (encode (.cbz t off)) 24 2 = 0 := by fields
    have e2 : rd (encode (.cbz t off)) = t := by fields
    have e3 : sext 18 (fld (encode (.cbz t off)) 5 19) = off := sext18 (by fields) (by have := bits19 off; fields) hoff
    unfold decode; rw [e0, e1, e2, e3]; rfl
  | cbnz t off =>
    obtain ⟨h1, hoff⟩ := h
    have := bits19 off
    refine ⟨by fields, ?_⟩
    have e0 : fld (encode (.cbnz t off)) 26 6 = 0x2d := by fields
    have e1 : fld (encode (.cbnz t off)) 24 2 = 1 := by fields
    have e2 : rd (encode (.cbnz t off)) = t := by fields
    have e3 : sext 18 (fld (encode (.cbnz t off)) 5 19) = off := sext18 (by fields) (by have := bits19 off; fields) hoff
    unfold decode; rw [e0, e1, e2, e3]; rfl
  | sreg op sh d n m amt =>
    obtain ⟨h1, h2, h3, h4⟩ := h
    cases op <;> cases sh
    · refine ⟨by fields, ?_⟩
      have e0 : fld (encode (.sreg .add .lsl d n m amt)) 26 6 = 0x22 := by fields
      have e1 : fld (encode (.sreg .add .lsl d n m amt)) 22 4 = 12 := by fields
      have e2 : fld (encode (.sreg .add .lsl d n m amt)) 21 1 = 0 := by fields
      have e3 : rd (encode (.sreg .add .lsl d n m amt)) = d := by fields
      have e4 : rn (encode (.sreg .add .lsl d n m amt)) = n := by fields
      have e5 : rm (encode (.sreg .add .lsl d n m amt)) = m := by fields
      have e6 : fld (encode (.sreg .add .lsl d n m amt)) 10 6 = amt := by fields
      unfold decode; rw [e0, e1]; unfold sregD; rw [e2, e3, e4, e5, e6]; rfl
    · refine ⟨by fields, ?_⟩
      have e0 : fld (encode (.sreg .add .asr d n m amt)) 26 6 = 0x22 := by fields
      have e1 : fld (encode (.sreg .add .asr d n m amt)) 22 4 = 14 := by fields
      have e2 : fld (encode (.sreg .add .asr d n m amt)) 21 1 = 0 := by fields
      have e3 : rd (encode (.sreg .add .asr d n m amt)) = d := by fields
      have e4 : rn (encode (.sreg .add .asr d n m amt)) = n := by fields
      have e5 : rm (encode (.sreg .add .asr d n m amt)) = m := by fields
      have e6 : fld (encode (.sreg .add .asr d n m amt)) 10 6 = amt := by fields
      unfold decode; rw [e0, e1]; unfold sregD; rw [e2, e3, e4, e5, e6]; rfl
    · refine ⟨by fields, ?_⟩
      have e0 : fld (encode (.sreg .adds .lsl d n m amt)) 26 6 = 0x2a := by fields
      have e1 : fld (encode (.sreg .adds .lsl d n m amt)) 22 4 = 12 := by fields
      have e2 : fld (encode (.sreg .adds .lsl d n m amt)) 21 1 = 0 := by fields
      have e3 : rd (encode (.sreg .adds .lsl d n m amt)) = d := by fields
      have e4 : rn (encode (.sreg .adds .lsl d n m amt)) = n := by fields
      have e5 : rm (encode (.sreg .adds .lsl d n m amt)) = m := by fields
      have e6 : fld (encode (.sreg .adds .lsl d n m amt)) 10 6 = amt := by fields
      unfold decode; rw [e0, e1]; unfold sregD; rw [e2, e3, e4, e5, e6]; rfl
    · refine ⟨by fields, ?_⟩
      have e0 : fld (encode (.sreg .adds .asr d n m amt)) 26 6 = 0x2a := by fields
      have e1 : fld (encode (.sreg .adds .asr d n m amt)) 22 4 = 14 := by fields
      have e2 : fld (encode (.sreg .adds .asr d n m amt)) 21 1 = 0 := by fields
      have e3 : rd (encode (.sreg .adds .asr d n m amt)) = d := by fields
      have e4 : rn (encode (.sreg .adds .asr d n m amt)) = n := by fields
      have e5 : rm (encode (.sreg .adds .asr d n m amt)) = m := by fields
      have e6 : fld (encode (.sreg .adds .asr d n m amt)) 10 6 = amt := by fields
      unfold decode; rw [e0, e1]; unfold sregD; rw [e2, e3, e4, e5, e6]; rfl
    · refine ⟨by fields, ?_⟩
      have e0 : fld (encode (.sreg .sub .lsl d n m amt)) 26 6 = 0x32 := by fields
      have e1 : fld (encode (.sreg .sub .lsl d n m amt)) 22 4 = 12 := by fields
      have e2 : fld (encode (.sreg .sub .lsl d n m amt)) 21 1 = 0 := by fields
      have e3 : rd (encode (.sreg .sub .lsl d n m amt)) = d := by fields
      have e4 : rn (encode (.sreg .sub .lsl d n m amt)) = n := by fields
      have e5 : rm (encode (.sreg .sub .lsl d n m amt)) = m := by fields
      have e6 : fld (encode (.sreg .sub .lsl d n m amt)) 10 6 = amt := by fields
      unfold decode; rw [e0, e1]; unfold sregD; rw [e2, e3, e4, e5, e6]; rfl
    · refine ⟨by fields, ?_⟩
      have e0 : fld (encode (.sreg .sub .asr d n m amt)) 26 6 = 0x32 := by fields
      have e1 : fld (encode (.sreg .sub .asr d n m amt)) 22 4 = 14 := by fields
      have e2 : fld (encode (.sreg .sub .asr d n m amt)) 21 1 = 0 := by fields
      have e3 : rd (encode (.sreg .sub .asr d n m amt)) = d := by fields
      have e4 : rn (encode (.sreg .sub .asr d n m amt)) = n := by fields
      have e5 : rm (encode (.sreg .sub .asr d n m amt)) = m := by fields
      have e6 : fld (encode (.sreg .sub .asr d n m amt)) 10 6 = amt := by fields
      unfold decode; rw [e0, e1]; unfold sregD; rw [e2, e3, e4, e5, e6]; rfl
    · refine ⟨by fields, ?_⟩
      have e0 : fld (encode (.sreg .subs .lsl d n m amt)) 26 6 = 0x3a := by fields
      have e1 : fld (encode (.sreg .subs .lsl d n m amt)) 22 4 = 12 := by fields
      have e2 : fld (encode (.sreg .subs .lsl d n m amt)) 21 1 = 0 := by fields
      have e3 : rd (encode (.sreg .subs .lsl d n m amt)) = d := by fields
      have e4 : rn (encode (.sreg .subs .lsl d n m amt)) = n := by fields
      have e5 : rm (encode (.sreg .subs .lsl d n m amt)) = m := by fields
      have e6 : fld (encode (.sreg .subs .lsl d n m amt)) 10 6 = amt := by fields
      unfold decode; rw [e0, e1]; unfold sregD; rw [e2, e3, e4, e5, e6]; rfl
    · refine ⟨by fields, ?_⟩
      have e0 : fld (encode (.sreg .subs .asr d n m amt)) 26 6 = 0x3a := by fields
      have e1 : fld (encode (.sreg .subs .asr d n m amt)) 22 4 = 14 := by fields
      have e2 : fld (encode (.sreg .subs .asr d n m amt)) 21 1 = 0 := by fields
      have e3 : rd (encode (.sreg .subs .asr d n m amt)) = d := by fields
      have e4 : rn (encode (.sreg .subs .asr d n m amt)) = n := by fields
      have e5 : rm (encode (.sreg .subs .asr d n m amt)) = m := by fields
      have e6 : fld (encode (.sreg .subs .asr d n m amt)) 10 6 = amt := by fields
      unfold decode; rw [e0, e1]; unfold sregD; rw [e2, e3, e4, e5, e6]; rfl
    · refine ⟨by fields, ?_⟩
      have e0 : fld (encode (.sreg .and .lsl d n m amt)) 26 6 = 0x22 := by fields
      have e1 : fld (encode (.sreg .and .lsl d n m amt)) 22 4 = 8 := by fields
      have e2 : fld (encode (.sreg .and .lsl d n m amt)) 21 1 = 0 := by fields
      have e3 : rd (encode (.sreg .and .lsl d n m amt)) = d := by fields
      have e4 : rn (encode (.sreg .and .lsl d n m amt)) = n := by fields
      have e5 : rm (encode (.sreg .and .lsl d n m amt)) = m := by fields
      have e6 : fld (encode (.sreg .and .lsl d n m amt)) 10 6 = amt := by fields
      unfold decode; rw [e0, e1]; unfold sregD; rw [e2, e3, e4, e5, e6]; rfl
    · refine ⟨by fields, ?_⟩
      have e0 : fld (encode (.sreg .and .asr d n m amt)) 26 6 = 0x22 := by fields
      have e1 : fld (encode (.sreg .and .asr d n m amt)) 22 4 = 10 := by fields
      have e2 : fld (encode (.sreg .and .asr d n m amt)) 21 1 = 0 := by fields
      have e3 : rd (encode (.sreg .and .asr d n m amt)) = d := by fields
      have e4 : rn (encode (.sreg .and .asr d n m amt)) = n := by fields
      have e5 : rm (encode (.sreg .and .asr d n m amt)) = m := by fields
      have e6 : fld (encode (.sreg .and .asr d n m amt)) 10 6 = amt := by fields
      unfold decode; rw [e0, e1]; unfold sregD; rw [e2, e3, e4, e5, e6]; rfl
  | dp3 op d n m a =>
    obtain ⟨h1, h2, h3, h4, h5⟩ := h
    cases op
    · refine ⟨by fields, ?_⟩
      have e0 : fld (encode (.dp3 .madd d n m a)) 26 6 = 0x26 := by fields
      have e1 : fld (encode (.dp3 .madd d n m a)) 21 5 = 24 := by fields
      have e2 : fld (encode (.dp3 .madd d n m a)) 15 1 = 0 := by fields
      have e3 : rd (encode (.dp3 .madd d n m a)) = d := by fields
      have e4 : rn (encode (.dp3 .madd d n m a)) = n := by fields
      have e5 : rm (encode (.dp3 .madd d n m a)) = m := by fields
      have e6 : fld (encode (.dp3 .madd d n m a)) 10 5 = a := by fields
      unfold decode; rw [e0, e1, e2, e3, e4, e5, e6]; rfl
    · refine ⟨by fields, ?_⟩
      have e0 : fld (encode (.dp3 .msub d n m a)) 26 6 = 0x26 := by fields
      have e1 : fld (encode (.dp3 .msub d n m a)) 21 5 = 24 := by fields
      have e2 : fld (encode (.dp3 .msub d n m a)) 15 1 = 1 := by fields
      have e3 : rd (encode (.dp3 .msub d n m a)) = d := by fields
      have e4 : rn (encode (.dp3 .msub d n m a)) = n := by fields
      have e5 : rm (encode (.dp3 .msub d n m a)) = m := by fields
      have e6 : fld (encode (.dp3 .msub d n m a)) 10 5 = a := by fields
      unfold decode; rw [e0, e1, e2, e3, e4, e5, e6]; rfl
    · obtain rfl := h5 (Or.inl rfl)
      refine ⟨by fields, ?_⟩
      have e0 : fld (encode (.dp3 .smulh d n m 31)) 26 6 = 0x26 := by fields
      have e1 : fld (encode (.dp3 .smulh d n m 31)) 21 5 = 26 := by fields
      have e2 : fld (encode (.dp3 .smulh d n m 31)) 15 1 = 0 := by fields
      have e3 : rd (encode (.dp3 .smulh d n m 31)) = d := by fields
      have e4 : rn (encode (.dp3 .smulh d n m 31)) = n := by fields
      have e5 : rm (encode (.dp3 .smulh d n m 31)) = m := by fields
      have e6 : fld (encode (.dp3 .smulh d n m 31)) 10 5 = 31 := by fields
      unfold decode; rw [e0, e1, e2, e3, e4, e5, e6]; rfl
    · obtain rfl := h5 (Or.inr rfl)
      refine ⟨by fields, ?_⟩
      have e0 : fld (encode (.dp3 .umulh d n m 31)) 26 6 = 0x26 := by fields
      have e1 : fld (encode (.dp3 .umulh d n m 31)) 21 5 = 30 := by fields
      have e2 : fld (encode (.dp3 .umulh d n m 31)) 15 1 = 0 := by fields
      have e3 : rd (encode (.dp3 .umulh d n m 31)) = d := by fields
      have e4 : rn (encode (.dp3 .umulh d n m 31)) = n := by fields
      have e5 : rm (encode (.dp3 .umulh d n m 31)) = m := by fields
      have e6 : fld (encode (.dp3 .umulh d n m 31)) 10 5 = 31 := by fields
      unfold decode; rw [e0, e1, e2, e3, e4, e5, e6]; rfl
  | dp2 op d n m =>
    obtain ⟨h1, h2, h3⟩ := h
    cases op
    · refine ⟨by fields, ?_⟩
      have e0 : fld (encode (.dp2 .udiv d n m)) 26 6 = 0x26 := by fields
      have e1 : fld (encode (.dp2 .udiv d n m)) 21 5 = 22 := by fields
      have e2 : fld (encode (.dp2 .udiv d n m)) 10 6 = 2 := by fields
      have e3 : rd (encode (.dp2 .udiv d n m)) = d := by fields
      have e4 : rn (encode (.dp2 .udiv d n m)) = n := by fields
      have e5 : rm (encode (.dp2 .udiv d n m)) = m := by fields
      unfold decode; rw [e0, e1, e2, e3, e4, e5]; rfl
    · refine ⟨by fields, ?_⟩
      have e0 : fld (encode (.dp2 .sdiv d n m)) 26 6 = 0x26 := by fields
      have e1 : fld (encode (.dp2 .sdiv d n m)) 21 5 = 22 := by fields
      have e2 : fld (encode (.dp2 .sdiv d n m)) 10 6 = 3 := by fields
      have e3 : rd (encode (.dp2 .sdiv d n m)) = d := by fields
      have e4 : rn (encode (.dp2 .sdiv d n m)) = n := by fields
      have e5 : rm (encode (.dp2 .sdiv d n m)) = m := by fields
      unfold decode; rw [e0, e1, e2, e3, e4, e5]; rfl
  | aimm op d n imm sh =>
    obtain ⟨h1, h2, h3, h4⟩ := h
    cases op
    · refine ⟨by fields, ?_⟩
      have e0 : fld (encode (.aimm .add d n imm sh)) 26 6 = 0x24 := by fields
      have e1 : fld (encode (.aimm .add d n imm sh)) 23 3 = 2 := by fields
      have e2 : rd (encode (.aimm .add d n imm sh)) = d := by fields
      have e3 : rn (encode (.aimm .add d n imm sh)) = n := by fields
      have e4 : fld (encode (.aimm .add d n imm sh)) 10 12 = imm := by fields
      have e5 : fld (encode (.aimm .add d n imm sh)) 22 1 = sh := by fields
      unfold decode; rw [e0, e1, e2, e3, e4, e5]; rfl
    · refine ⟨by fields, ?_⟩
      have e0 : fld (encode (.aimm .adds d n imm sh)) 26 6 = 0x2c := by fields
      have e1 : fld (encode (.aimm .adds d n imm sh)) 23 3 = 2 := by fields
      have e2 : rd (encode (.aimm .adds d n imm sh)) = d := by fields
      have e3 : rn (encode (.aimm .adds d n imm sh)) = n := by fields
      have e4 : fld (encode (.aimm .adds d n imm sh)) 10 12 = imm := by fields
      have e5 : fld (encode (.aimm .adds d n imm sh)) 22 1 = sh := by fields
      unfold decode; rw [e0, e1, e2, e3, e4, e5]; rfl
    · refine ⟨by fields, ?_⟩
      have e0 : fld (encode (.aimm .sub d n imm sh)) 26 6 = 0x34 := by fields
      have e1 : fld (encode (.aimm .sub d n imm sh)) 23 3 = 2 := by fields
      have e2 : rd (encode (.aimm .sub d n imm sh)) = d := by fields
      have e3 : rn (encode (.aimm .sub d n imm sh)) = n := by fields
      have e4 : fld (encode (.aimm .sub d n imm sh)) 10 12 = imm := by fields
      have e5 : fld (encode (.aimm .sub d n imm sh)) 22 1 = sh := by fields
      unfold decode; rw [e0, e1, e2, e3, e4, e5]; rfl
  | fp op d n m =>
    obtain ⟨h1, h2, h3⟩ := h
    cases op
    · refine ⟨by fields, ?_⟩
      have e0 : fld (encode (.fp .fmul d n m)) 26 6 = 0x07 := by fields
      have e1 : fld (encode (.fp .fmul d n m)) 21 5 = 19 := by fields
      have e2 : fld (encode (.fp .fmul d n m)) 10 2 = 2 := by fields
      have e3 : fld (encode (.fp .fmul d n m)) 12 4 = 0 := by fields
      have e4 : rd (encode (.fp .fmul d n m)) = d := by fields
      have e5 : rn (encode (.fp .fmul d n m)) = n := by fields
      have e6 : rm (encode (.fp .fmul d n m)) = m := by fields
      unfold decode; rw [e0, e1, e2, e3, e4, e5, e6]; rfl
    · refine ⟨by fields, ?_⟩
      have e0 : fld (encode (.fp .fdiv d n m)) 26 6 = 0x07 := by fields
      have e1 : fld (encode (.fp .fdiv d n m)) 21 5 = 19 := by fields
      have e2 : fld (encode (.fp .fdiv d n m)) 10 2 = 2 := by fields
      have e3 : fld (encode (.fp .fdiv d n m)) 12 4 = 1 := by fields
      have e4 : rd (encode (.fp .fdiv d n m)) = d := by fields
      have e5 : rn (encode (.fp .fdiv d n m)) = n := by fields
      have e6 : rm (encode (.fp .fdiv d n m)) = m := by fields
      unfold decode; rw [e0, e1, e2, e3, e4, e5, e6]; rfl
    · refine ⟨by fields, ?_⟩
      have e0 : fld (encode (.fp .fadd d n m)) 26 6 = 0x07 := by fields
      have e1 : fld (encode (.fp .fadd d n m)) 21 5 = 19 := by fields
      have e2 : fld (encode (.fp .fadd d n m)) 10 2 = 2 := by fields
      have e3 : fld (encode (.fp .fadd d n m)) 12 4 = 2 := by fields
      have e4 : rd (encode (.fp .fadd d n m)) = d := by fields
      have e5 : rn (encode (.fp .fadd d n m)) = n := by fields
      have e6 : rm (encode (.fp .fadd d n m)) = m := by fields
      unfold decode; rw [e0, e1, e2, e3, e4, e5, e6]; rfl
    · refine ⟨by fields, ?_⟩
      have e0 : fld (encode (.fp .fsub d n m)) 26 6 = 0x07 := by fields
      have e1 : fld (encode (.fp .fsub d n m)) 21 5 = 19 := by fields
      have e2 : fld (encode (.fp .fsub d n m)) 10 2 = 2 := by fields
      have e3 : fld (encode (.fp .fsub d n m)) 12 4 = 3 := by fields
      have e4 : rd (encode (.fp .fsub d n m)) = d := by fields
      have e5 : rn (encode (.fp .fsub d n m)) = n := by fields
      have e6 : rm (encode (.fp .fsub d n m)) = m := by fields
      unfold decode; rw [e0, e1, e2, e3, e4, e5, e6]; rfl
  | eori d n k immr imms =>
    obtain ⟨h1, h2, rfl, h4, h5⟩ := h
    refine ⟨by fields, ?_⟩
    have e0 : fld (encode (.eori d n 1 immr imms)) 26 6 = 0x34 := by fields
    have e1 : fld (encode (.eori d n 1 immr imms)) 23 3 = 4 := by fields
    have e2 : fld (encode (.eori d n 1 immr imms)) 22 1 = 1 := by fields
    have e3 : rd (encode (.eori d n 1 immr imms)) = d := by fields
    have e4 : rn (encode (.eori d n 1 immr imms)) = n := by fields
    have e5 : fld (encode (.eori d n 1 immr imms)) 16 6 = immr := by fields
    have e6 : fld (encode (.eori d n 1 immr imms)) 10 6 = imms := by fields
    unfold decode; rw [e0, e1, e2, e3, e4, e5, e6]; rw [ite_eq_left_of_eq_true _ _ (eq_true h5)]; rfl
  | csinc d n m c =>
    obtain ⟨h1, h2, h3, h4⟩ := h
    refine ⟨by fields, ?_⟩
    have e0 : fld (encode (.csinc d n m c)) 26 6 = 0x26 := by fields
    have e1 : fld (encode (.csinc d n m c)) 21 5 = 20 := by fields
    have e2 : fld (encode (.csinc d n m c)) 10 2 = 1 := by fields
    have e3 : rd (encode (.csinc d n m c)) = d := by fields
    have e4 : rn (encode (.csinc d n m c)) = n := by fields
    have e5 : rm (encode (.csinc d n m c)) = m := by fields
    have e6 : fld (encode (.csinc d n m c)) 12 4 = c := by fields
    unfold decode; rw [e0, e1, e2, e3, e4, e5, e6]; rfl
  | fmovDX d n =>
    obtain ⟨h1, h2⟩ := h
    refine ⟨by fields, ?_⟩
    have e0 : fld (encode (.fmovDX d n)) 26 6 = 0x27 := by fields
    have e1 : fld (encode (.fmovDX d n)) 16 10 = 0x267 := by fields
    have e2 : fld (encode (.fmovDX d n)) 10 6 = 0 := by fields
    have e3 : rd (encode (.fmovDX d n)) = d := by fields
    have e4 : rn (encode (.fmovDX d n)) = n := by fields
    unfold decode; rw [e0, e1, e2, e3, e4]; rfl
  | fmovXD d n =>
    obtain ⟨h1, h2⟩ := h
    refine ⟨by fields, ?_⟩
    have e0 : fld (encode (.fmovXD d n)) 26 6 = 0x27 := by fields
    have e1 : fld (encode (.fmovXD d n)) 16 10 = 0x266 := by fields
    have e2 : fld (encode (.fmovXD d n)) 10 6 = 0 := by fields
    have e3 : rd (encode (.fmovXD d n)) = d := by fields
    have e4 : rn (encode (.fmovXD d n)) = n := by fields
    unfold decode; rw [e0, e1, e2, e3, e4]; rfl
  | fneg d n =>
    obtain ⟨h1, h2⟩ := h
    refine ⟨by fields, ?_⟩
    have e0 : fld (encode (.fneg d n)) 26 6 = 0x07 := by fields
    have e1 : fld (encode (.fneg d n)) 21 5 = 19 := by fields
    have e2 : fld (encode (.fneg d n)) 10 2 = 0 := by fields
    have e3 : fld (encode (.fneg d n)) 10 6 = 16 := by fields
    have e4 : fld (encode (.fneg d n)) 16 5 = 1 := by fields
    have e5 : fld (encode (.fneg d n)) 0 5 = d := by fields
    have e6 : rd (encode (.fneg d n)) = d := by fields
    have e7 : rn (encode (.fneg d n)) = n := by fields
    unfold decode; rw [e0, e1, e2, e3, e4, e5, e6, e7]; rfl
  | fcmp n m =>
    obtain ⟨h1, h2⟩ := h
    refine ⟨by fields, ?_⟩
    have e0 : fld (encode (.fcmp n m)) 26 6 = 0x07 := by fields
    have e1 : fld (encode (.fcmp n m)) 21 5 = 19 := by fields
    have e2 : fld (encode (.fcmp n m)) 10 2 = 0 := by fields
    have e3 : fld (encode (.fcmp n m)) 10 6 = 8 := by fields
    have e4 : fld (encode (.fcmp n m)) 16 5 = m := by fields
    have e5 : fld (encode (.fcmp n m)) 0 5 = 0 := by fields
    have e6 : rn (encode (.fcmp n m)) = n := by fields
    have e7 : rm (encode (.fcmp n m)) = m := by fields
    unfold decode; rw [e0, e1, e2, e3, e4, e5, e6, e7]; rfl
  | stpPre t t2 n imm =>
    obtain ⟨h1, h2, h3, hoff⟩ := h
    have := bits7 imm
    refine ⟨by fields, ?_⟩
    have e0 : fld (encode (.stpPre t t2 n imm)) 26 6 = 0x2a := by fields
    have e1 : fld (encode (.stpPre t t2 n imm)) 22 4 = 6 := by fields
    have e2 : rd (encode (.stpPre t t2 n imm)) = t := by fields
    have e3 : fld (encode (.stpPre t t2 n imm)) 10 5 = t2 := by fields
    have e4 : rn (encode (.stpPre t t2 n imm)) = n := by fields
    have e5 : sext 6 (fld (encode (.stpPre t t2 n imm)) 15 7) = imm := sext6 (by fields) (by have := bits7 imm; fields) hoff
    unfold decode; rw [e0, e1, e2, e3, e4, e5]; rfl
  | ldpPost t t2 n imm =>
    obtain ⟨h1, h2, h3, hoff⟩ := h
    have := bits7 imm
    refine ⟨by fields, ?_⟩
    have e0 : fld (encode (.ldpPost t t2 n imm)) 26 6 = 0x2a := by fields
    have e1 : fld (encode (.ldpPost t t2 n imm)) 22 4 = 3 := by fields
    have e2 : rd (encode (.ldpPost t t2 n imm)) = t := by fields
    have e3 : fld (encode (.ldpPost t t2 n imm)) 10 5 = t2 := by fields
    have e4 : rn (encode (.ldpPost t t2 n imm)) = n := by fields
    have e5 : sext 6 (fld (encode (.ldpPost t t2 n imm)) 15 7) = imm := sext6 (by fields) (by have := bits7 imm; fields) hoff
    unfold decode; rw [e0, e1, e2, e3, e4, e5]; rfl
  | ret n =>
    have h1 : n < 32 := h
    refine ⟨by fields, ?_⟩
    have e0 : fld (encode (.ret n)) 26 6 = 0x35 := by fields
    have e1 : fld (encode (.ret n)) 10 16 = 0x97c0 := by fields
    have e2 : fld (encode (.ret n)) 0 5 = 0 := by fields
    have e3 : rn (encode (.ret n)) = n := by fields
    unfold decode; rw [e0, e1, e2, e3]; rfl

theorem encode_lt (i : Inst) (h : i.Wf) : encode i < 2 ^ 32 := (decode_encode i h).1

/-- The four bytes `aarch64.rs` writes reassemble to the encoded word. -/
theorem bytes_encode (i : Inst) (h : i.Wf) :
    match bytes (encode i) with
    | [b0, b1, b2, b3] => b0 < 256 ∧ b1 < 256 ∧ b2 < 256 ∧ b3 < 256 ∧
        b0 + 2 ^ 8 * b1 + 2 ^ 16 * b2 + 2 ^ 24 * b3 = encode i
    | _ => False := by
  have := encode_lt i h
  simp only [bytes]; omega

/-! ## `aarch64::emit` -/

/-- `x0`, `x1`, `x2`; the frame pointer `x29`, the link register `x30`, and
register 31 (`sp` or `xzr` depending on the instruction). -/
def regX : Reg → Nat
  | .r0 => 0 | .r1 => 1 | .r2 => 2
def fp : Nat := 29
def lr : Nat := 30
def sp : Nat := 31
def xzr : Nat := 31

/-- Condition codes. -/
def EQ : Nat := 0
def NE : Nat := 1
def HS : Nat := 2
def LO : Nat := 3
def MI : Nat := 4
def VS : Nat := 6
def HI : Nat := 8
def LS : Nat := 9
def GE : Nat := 10
def LT : Nat := 11
def GT : Nat := 12
def LE : Nat := 13

/-- `FIRST_FREE_LABEL`: labels below it are the abort exits of
`TrapKind::ALL` (`tests/verified_codegen_lean.rs` checks the value against the
Rust constant); every label from it on is a function or block label. -/
def firstFree : Nat := 3

/-- The program-wide abort exits (`TrapKind::label`). -/
def trapLabel : Failure → Nat
  | .overflow => 0 | .divZero => 1 | .unreachable => 2 | .fault => 3
def ovf : Nat := trapLabel .overflow

/-- The branch instructions that take a label. -/
inductive Br | b | bl | bcond (c : Nat) | cbz (rt : Nat) | cbnz (rt : Nat)

/-- An emitted instruction, or a branch to a label resolved by layout. -/
inductive Item
  | ins (i : Inst)
  | br (k : Br) (l : Nat)

def Br.inst : Br → Int → Inst
  | .b, off => .b off
  | .bl, off => .bl off
  | .bcond c, off => .bcond c off
  | .cbz r, off => .cbz r off
  | .cbnz r, off => .cbnz r off

/-- `movz`/`movk` the 64-bit immediate (always four instructions). -/
def imm (d w : Nat) : List Item :=
  [.ins (.movz d 0 (w % 2 ^ 16)), .ins (.movk d 1 (w / 2 ^ 16 % 2 ^ 16)),
   .ins (.movk d 2 (w / 2 ^ 32 % 2 ^ 16)), .ins (.movk d 3 (w / 2 ^ 48 % 2 ^ 16))]

/-- The base register and scaled offset of a frame word. -/
def frame : Loc → Nat × Nat
  | .slot n | .out n => (sp, n)
  | .inn n => (fp, n + 2)

def ldst (load : Bool) (r : Nat) (l : Loc) : List Item :=
  [.ins (if load then .ldr r (frame l).1 (frame l).2 else .str r (frame l).1 (frame l).2)]

/-- `sub sp, sp, #bytes` (bytes < 2^24). -/
def subSp (bytes : Nat) : List Item :=
  (if bytes / 4096 ≠ 0 then [.ins (.aimm .sub sp sp (bytes / 4096) 1)] else []) ++
  (if bytes % 4096 ≠ 0 ∨ bytes = 0 then [.ins (.aimm .sub sp sp (bytes % 4096) 0)] else [])

/-- `cmp x_n, x_m` (`subs xzr, x_n, x_m`). -/
def cmpR (n m : Nat) : Item := .ins (.sreg .subs .lsl xzr n m 0)
/-- `cset x0, cond` (`csinc x0, xzr, xzr, !cond`). -/
def cset (c : Nat) : Item := .ins (.csinc 0 xzr xzr (c ^^^ 1))

def intCond : Cond → Sign → Nat
  | .eq, _ => EQ | .ne, _ => NE
  | .lt, .s => LT | .le, .s => LE | .gt, .s => GT | .ge, .s => GE
  | .lt, .u => LO | .le, .u => LS | .gt, .u => HI | .ge, .u => HS
/-- An unordered pair sets C and V, so MI, LS, GT, GE and EQ are false and
NE true. -/
def floatCond : Cond → Nat
  | .eq => EQ | .ne => NE | .lt => MI | .le => LS | .gt => GT | .ge => GE

def fopOf : FloatOp → FOp
  | .add => .fadd | .sub => .fsub | .mul => .fmul | .div => .fdiv

def emit : M → List Item
  | .ld r l => ldst true (regX r) l
  | .st l r => ldst false (regX r) l
  | .imm r w => imm (regX r) (w % 2 ^ 64).toNat
  | .addchk sg => [.ins (.sreg .adds .lsl 0 0 1 0), .br (.bcond (if sg = .s then VS else HS)) ovf]
  | .subchk sg => [.ins (.sreg .subs .lsl 0 0 1 0), .br (.bcond (if sg = .s then VS else LO)) ovf]
  | .mulchk .s => [.ins (.dp3 .smulh 2 0 1 31), .ins (.dp3 .madd 0 0 1 31),
      .ins (.sreg .subs .asr xzr 2 0 63), .br (.bcond NE) ovf]
  | .mulchk .u => [.ins (.dp3 .umulh 2 0 1 31), .ins (.dp3 .madd 0 0 1 31), .br (.cbnz 2) ovf]
  | .wrap .add => [.ins (.sreg .add .lsl 0 0 1 0)]
  | .wrap .sub => [.ins (.sreg .sub .lsl 0 0 1 0)]
  | .wrap .mul => [.ins (.dp3 .madd 0 0 1 31)]
  | .andr2 => [.ins (.sreg .and .lsl 0 0 2 0)]
  | .trapgtu => [cmpR 0 2, .br (.bcond HI) ovf]
  | .trapzero => [.br (.cbz 1) (trapLabel .divZero)]
  | .trapdivovf => [.ins (.movz 2 3 0x8000), cmpR 0 2, .ins (.bcond NE 3),
      .ins (.aimm .adds xzr 1 1 0), .br (.bcond EQ) ovf]
  | .div .s => [.ins (.dp2 .sdiv 0 0 1)]
  | .div .u => [.ins (.dp2 .udiv 0 0 1)]
  | .rem sg => [.ins (.dp2 (if sg = .s then .sdiv else .udiv) 2 0 1), .ins (.dp3 .msub 0 2 1 0)]
  | .negchk .s => [.ins (.sreg .subs .lsl 0 xzr 0 0), .br (.bcond VS) ovf]
  | .negchk .u => [.br (.cbnz 0) ovf]
  | .xorone => [.ins (.eori 0 0 1 0 0)]
  | .cmp c sg => [cmpR 0 1, cset (intCond c sg)]
  | .fop op => [.ins (.fmovDX 0 0), .ins (.fmovDX 1 1), .ins (.fp (fopOf op) 0 0 1),
      .ins (.fmovXD 0 0)]
  | .fneg => [.ins (.fmovDX 0 0), .ins (.fneg 0 0), .ins (.fmovXD 0 0)]
  | .fcmp c => [.ins (.fmovDX 0 0), .ins (.fmovDX 1 1), .ins (.fcmp 0 1), cset (floatCond c)]
  | .label _ => []
  | .jmp l => [.br .b l]
  | .bz l => [.br (.cbz 0) l]
  | .call l => [.br .bl l]
  | .enter words => [.ins (.stpPre fp lr sp (-2)), .ins (.aimm .add fp sp 0 0)] ++
      subSp (words * 8)
  | .leave => [.ins (.aimm .add sp fp 0 0), .ins (.ldpPost fp lr sp 2), .ins (.ret lr)]
  | .trap f => [.br .b (trapLabel f)]

/-! ## Layout: abort exits before the instruction, other labels after it -/

def Item.at (k n : Nat) : Item → Inst
  | .ins i => i
  | .br bk l => bk.inst (if l < firstFree then -(k : Int) else ((n - k : Nat) : Int))

def place (n : Nat) : Nat → List Item → List Inst
  | _, [] => []
  | k, it :: rest => it.at k n :: place n (k + 1) rest

def layout (items : List Item) : List Inst := place items.length 0 items

def Br.Ok : Br → Prop
  | .b | .bl => True
  | .bcond c => c < 16
  | .cbz r | .cbnz r => r < 32

def Item.Ok : Item → Prop
  | .ins i => i.Wf
  | .br k _ => k.Ok

instance (i : Inst) : Decidable i.Wf := by
  cases i <;> simp only [Inst.Wf, Fits] <;> infer_instance
instance (k : Br) : Decidable k.Ok := by
  cases k <;> simp only [Br.Ok] <;> infer_instance
instance (it : Item) : Decidable it.Ok := by
  cases it <;> simp only [Item.Ok] <;> infer_instance

theorem place_wf (n : Nat) (hn : n < 2 ^ 18) :
    ∀ (items : List Item) (k : Nat), k + items.length ≤ n → (∀ it ∈ items, it.Ok) →
      ∀ i ∈ place n k items, i.Wf := by
  intro items
  induction items with
  | nil => intro _ _ _ i hi; cases hi
  | cons it rest ih =>
    intro k hk hok i hi
    simp only [place, List.mem_cons] at hi
    rcases hi with rfl | hi
    · have := hok it (by simp)
      cases it with
      | ins j => exact this
      | br bk l =>
        simp only [List.length_cons] at hk
        cases bk <;> simp only [Item.at, Br.inst, Inst.Wf, Fits, Item.Ok, Br.Ok] at this ⊢ <;>
          (try refine ⟨this, ?_, ?_⟩) <;> (try refine ⟨?_, ?_⟩) <;> split <;> omega
    · exact ih (k + 1) (by simp at hk; omega) (fun it h => hok it (by simp [h])) i hi

/-- `M` operands the Rust emitter can be given: frames hold at most
`MAX_FRAME_WORDS` (4000) words, so slot, outgoing and incoming indices fit
the scaled 12-bit offset, and frame sizes fit `sub sp` (below 2^24 bytes). -/
def mBounded : M → Prop
  | .ld _ l | .st l _ => match l with | .slot n | .out n => n < 4096 | .inn n => n + 2 < 4096
  | .enter words => words < 2 ^ 21
  | _ => True

theorem imm_ok (d w : Nat) (hd : d < 32) : ∀ it ∈ imm d w, it.Ok := by
  simp only [imm, List.mem_cons, List.not_mem_nil, or_false]
  intro it hit
  rcases hit with rfl | rfl | rfl | rfl <;> simp only [Item.Ok, Inst.Wf] <;> omega

theorem ldst_ok (load : Bool) (r : Nat) (l : Loc) (hr : r < 32)
    (hl : match l with | .slot n | .out n => n < 4096 | .inn n => n + 2 < 4096) :
    ∀ it ∈ ldst load r l, it.Ok := by
  have hf : (frame l).1 < 32 ∧ (frame l).2 < 4096 := by
    cases l <;> simp only [frame, sp, fp] at hl ⊢ <;> omega
  intro it hit
  simp only [ldst, List.mem_cons, List.not_mem_nil, or_false] at hit
  subst hit
  cases load <;> simp only [Item.Ok, Inst.Wf, Bool.false_eq_true, ite_false, ite_true] <;> omega

theorem subSp_ok (bytes : Nat) (hb : bytes < 2 ^ 24) : ∀ it ∈ subSp bytes, it.Ok := by
  intro it hit
  simp only [subSp, List.mem_append] at hit
  rcases hit with hit | hit <;> split at hit <;>
    simp only [List.mem_cons, List.not_mem_nil, or_false] at hit <;>
    (try subst hit) <;> (try simp only [Item.Ok, Inst.Wf, sp]) <;> omega

/-- Every item `emit` produces is well formed. -/
theorem emit_ok (m : M) (hm : mBounded m) : ∀ it ∈ emit m, it.Ok := by
  cases m with
  | ld r l => exact ldst_ok true (regX r) l (by cases r <;> decide) hm
  | st l r => exact ldst_ok false (regX r) l (by cases r <;> decide) hm
  | imm r w => exact imm_ok (regX r) _ (by cases r <;> decide)
  | enter words =>
    intro it hit
    simp only [emit, List.mem_append, List.mem_cons, List.not_mem_nil, or_false] at hit
    rcases hit with (rfl | rfl) | hit
    · decide
    · decide
    · exact subSp_ok _ (by simp only [mBounded] at hm; omega) it hit
  | addchk sg | subchk sg | mulchk sg | div sg | rem sg | negchk sg =>
    cases sg <;> decide
  | wrap op | fop op => cases op <;> decide
  | cmp c sg => cases c <;> cases sg <;> decide
  | fcmp c => cases c <;> decide
  | trap f => cases f <;> decide
  | label l | jmp l | bz l | call l => simp [emit, Item.Ok, Br.Ok]
  | _ => decide

theorem emit_length (m : M) : (emit m).length ≤ 8 := by
  cases m with
  | ld _ l | st l _ => simp [emit, ldst]
  | enter words => simp only [emit, subSp]; split <;> split <;> simp
  | imm _ _ => simp [emit, imm]
  | addchk sg | subchk sg | mulchk sg | div sg | rem sg | negchk sg =>
    cases sg <;> decide
  | wrap op | fop op => cases op <;> decide
  | cmp c sg => cases c <;> cases sg <;> decide
  | fcmp c => cases c <;> decide
  | trap f => cases f <;> decide
  | _ => simp [emit]

/-- The theorem the listing relies on: every word of the laid-out `emit m`
is the encoding of a well-formed instruction, a 32-bit word the encoding
specification decodes back to that instruction. -/
theorem emit_decodes (m : M) (hm : mBounded m) :
    ∀ i ∈ layout (emit m), encode i < 2 ^ 32 ∧ decode (encode i) = some i := by
  intro i hi
  exact decode_encode i
    (place_wf _ (by have := emit_length m; omega) (emit m) 0 (by omega) (emit_ok m hm) i hi)

/-! ## The printed listing (compared with `aarch64::encoding_listing()`) -/

def hex8 (w : Nat) : String :=
  let d := Nat.toDigits 16 w
  String.ofList (List.replicate (8 - d.length) '0' ++ d)

def samples : List M :=
  [.ld .r0 (.slot 1), .ld .r1 (.slot 300), .st (.out 2) .r0, .ld .r2 (.inn 3),
   .st (.inn 400) .r1, .imm .r0 0x0123456789abcdef, .imm .r2 18446744073709551615] ++
  ([Sign.s, .u].flatMap fun sg => [.addchk sg, .subchk sg, .mulchk sg]) ++
  [.wrap .add, .wrap .sub, .wrap .mul, .andr2, .trapgtu, .trapzero, .trapdivovf,
   .div .s, .div .u, .rem .s, .rem .u, .negchk .s, .negchk .u, .xorone] ++
  ([Cond.eq, .ne, .lt, .le, .gt, .ge].flatMap fun c => [.cmp c .s, .cmp c .u]) ++
  [.fop .add, .fop .sub, .fop .mul, .fop .div, .fneg] ++
  ([Cond.eq, .ne, .lt, .le, .gt, .ge].map .fcmp) ++
  [.jmp 9, .jmp firstFree, .bz 9, .call 9, .enter 4, .enter 300, .enter 1000, .enter 512, .leave,
   .trap .overflow, .trap .divZero, .trap .unreachable]

def listing : String :=
  String.join (samples.map fun m =>
    m.render ++ ":" ++ String.join ((layout (emit m)).map fun i => " " ++ hex8 (encode i)) ++
      "\n")

#eval IO.print ("BEGIN A64\n" ++ listing ++ "END A64\n")

#print axioms Vesper.VerifiedCodegen.A64.bits19
#print axioms Vesper.VerifiedCodegen.A64.bits26
#print axioms Vesper.VerifiedCodegen.A64.bits7
#print axioms Vesper.VerifiedCodegen.A64.sext18
#print axioms Vesper.VerifiedCodegen.A64.sext25
#print axioms Vesper.VerifiedCodegen.A64.sext6
#print axioms Vesper.VerifiedCodegen.A64.decode_encode
#print axioms Vesper.VerifiedCodegen.A64.encode_lt
#print axioms Vesper.VerifiedCodegen.A64.bytes_encode
#print axioms Vesper.VerifiedCodegen.A64.place_wf
#print axioms Vesper.VerifiedCodegen.A64.imm_ok
#print axioms Vesper.VerifiedCodegen.A64.ldst_ok
#print axioms Vesper.VerifiedCodegen.A64.subSp_ok
#print axioms Vesper.VerifiedCodegen.A64.emit_ok
#print axioms Vesper.VerifiedCodegen.A64.emit_length
#print axioms Vesper.VerifiedCodegen.A64.emit_decodes

end Vesper.VerifiedCodegen.A64
