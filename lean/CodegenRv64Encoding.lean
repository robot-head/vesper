import VerifiedCodegen

/-!
# Verified direct codegen: RV64 instruction encoding (§42.1 pillar 3b)

`VerifiedCodegen.lean` proves the instruction-selection table correct down to
the abstract machine `M`. This file covers the next step for the RISC-V
target: the 32-bit words `src/verified_codegen/riscv64.rs` writes for each
`M` instruction.

* `Inst` is the integer and D-extension subset of RV64 the emitter uses, as
  the unprivileged ISA manual names it (`add`, `ld`, `bne`, `fadd.d`, ...).
* `decode` is the specification: the base instruction formats (R, I, S, B,
  J) of the ISA manual, field by field (`opcode` in bits 6:0, `rd` in 11:7,
  `funct3` in 14:12, `rs1` in 19:15, `rs2` in 24:20, `funct7` in 31:25, the
  scattered B and J immediates, sign extension of every immediate), with the
  opcode and function codes of the manual's instruction listing.
* `encode` is the encoder, written field by field like `riscv64.rs`.
* `decode_encode` proves that every well-formed instruction (registers below
  32, every immediate in its field's range, branch and jump offsets even)
  encodes to a word below `2^32` that the specification decodes back to the
  same instruction. `bytes_encode` gives the four little-endian bytes.
* `emit` is `riscv64::emit` for one `M` instruction, as a list of `Inst`.
  `emit_wf` proves every instruction it produces is well formed, so
  `decode_encode` applies to all of them.
* `listing` prints the encoded words of `emit` for sample `M` instructions;
  `tests/verified_codegen_lean.rs` checks it line for line against
  `riscv64::encoding_listing()`, which runs the Rust emitter on the same
  instructions. The theorems are about the Lean encoder; the listing ties the
  Rust encoder to it on those samples, not in general.

Branch targets: `emit` resolves every jump against a layout where the abort
exits (labels below `firstFree`) sit just before the instruction and every
other label just after it,
which is how the listing lays them out.
-/

namespace Vesper.VerifiedCodegen.Rv64

/-! ## The instruction subset -/

/-- Register-register operations (major opcode `OP`). -/
inductive ROp | add | sub | slt | sltu | and | mul | mulh | mulhu | div | divu | rem | remu
  deriving DecidableEq, Repr
/-- Register-immediate operations (major opcode `OP-IMM`) with a 12-bit
signed immediate. -/
inductive IOp | addi | sltiu | xori | ori
  deriving DecidableEq, Repr
/-- Conditional branches. -/
inductive BOp | beq | bne | bltu | bgeu
  deriving DecidableEq, Repr
/-- D-extension register operations (major opcode `OP-FP`); the arithmetic
ones use the static rounding mode RNE (`rm = 000`). -/
inductive FOp | faddD | fsubD | fmulD | fdivD | fsgnjnD | feqD | fltD | fleD
  deriving DecidableEq, Repr

inductive Inst
  | r (op : ROp) (rd rs1 rs2 : Nat)
  | i (op : IOp) (rd rs1 : Nat) (imm : Int)
  | slli (rd rs1 sh : Nat)
  | srai (rd rs1 sh : Nat)
  | ld (rd rs1 : Nat) (imm : Int)
  | sd (rs2 rs1 : Nat) (imm : Int)
  | branch (op : BOp) (rs1 rs2 : Nat) (off : Int)
  | jal (rd : Nat) (off : Int)
  | jalr (rd rs1 : Nat) (imm : Int)
  | ecall
  | fp (op : FOp) (rd rs1 rs2 : Nat)
  | fmvDX (rd rs1 : Nat)
  | fmvXD (rd rs1 : Nat)
  deriving DecidableEq, Repr

/-! ## The specification: the ISA manual's opcode tables and formats -/

/-- `funct7`, `funct3` of each `OP` instruction (opcode `0110011`). -/
def ROp.code : ROp → Nat × Nat
  | .add => (0, 0) | .sub => (32, 0) | .slt => (0, 2) | .sltu => (0, 3) | .and => (0, 7)
  | .mul => (1, 0) | .mulh => (1, 1) | .mulhu => (1, 3) | .div => (1, 4) | .divu => (1, 5)
  | .rem => (1, 6) | .remu => (1, 7)
/-- `funct3` of each `OP-IMM` instruction (opcode `0010011`). -/
def IOp.code : IOp → Nat
  | .addi => 0 | .sltiu => 3 | .xori => 4 | .ori => 6
/-- `funct3` of each branch (opcode `1100011`). -/
def BOp.code : BOp → Nat
  | .beq => 0 | .bne => 1 | .bltu => 6 | .bgeu => 7
/-- `funct7`, `funct3` (or `rm`) of each `OP-FP` instruction (opcode
`1010011`). -/
def FOp.code : FOp → Nat × Nat
  | .faddD => (1, 0) | .fsubD => (5, 0) | .fmulD => (9, 0) | .fdivD => (13, 0)
  | .fsgnjnD => (17, 1) | .feqD => (81, 2) | .fltD => (81, 1) | .fleD => (81, 0)

def ROp.all : List ROp :=
  [.add, .sub, .slt, .sltu, .and, .mul, .mulh, .mulhu, .div, .divu, .rem, .remu]
def IOp.all : List IOp := [.addi, .sltiu, .xori, .ori]
def BOp.all : List BOp := [.beq, .bne, .bltu, .bgeu]
def FOp.all : List FOp := [.faddD, .fsubD, .fmulD, .fdivD, .fsgnjnD, .feqD, .fltD, .fleD]

/-- Two's-complement reading of an `n + 1`-bit field. -/
def sext (n : Nat) (x : Nat) : Int :=
  if x < 2 ^ n then (x : Int) else (x : Int) - 2 ^ (n + 1)

/-! Fields at their positions in a 32-bit word. -/
def opcode (w : Nat) : Nat := w % 128
def rdF (w : Nat) : Nat := w / 2 ^ 7 % 32
def funct3 (w : Nat) : Nat := w / 2 ^ 12 % 8
def rs1F (w : Nat) : Nat := w / 2 ^ 15 % 32
def rs2F (w : Nat) : Nat := w / 2 ^ 20 % 32
def funct7 (w : Nat) : Nat := w / 2 ^ 25 % 128
/-- RV64 shifts: `imm[11:6]` selects the shift, `imm[5:0]` is the amount. -/
def funct6 (w : Nat) : Nat := w / 2 ^ 26 % 64
def shamt (w : Nat) : Nat := w / 2 ^ 20 % 64
/-- I-type: `imm[11:0]` in bits 31:20. -/
def immI (w : Nat) : Int := sext 11 (w / 2 ^ 20 % 4096)
/-- S-type: `imm[11:5]` in bits 31:25, `imm[4:0]` in bits 11:7. -/
def immS (w : Nat) : Int := sext 11 (funct7 w * 32 + rdF w)
/-- B-type: `imm[12|10:5]` in bits 31:25, `imm[4:1|11]` in bits 11:7. -/
def immB (w : Nat) : Int :=
  sext 12 (w / 2 ^ 31 % 2 * 4096 + w / 2 ^ 7 % 2 * 2048 + w / 2 ^ 25 % 64 * 32 +
    w / 2 ^ 8 % 16 * 2)
/-- J-type: `imm[20|10:1|11|19:12]` in bits 31:12. -/
def immJ (w : Nat) : Int :=
  sext 20 (w / 2 ^ 31 % 2 * 2 ^ 20 + w / 2 ^ 12 % 256 * 2 ^ 12 + w / 2 ^ 20 % 2 * 2048 +
    w / 2 ^ 21 % 1024 * 2)

/-- The specification decoder for the subset, on a 32-bit word; `none` for
anything else. -/
def decode (w : Nat) : Option Inst :=
  match opcode w with
  | 0x33 => (ROp.all.find? fun op => op.code = (funct7 w, funct3 w)).map
      fun op => .r op (rdF w) (rs1F w) (rs2F w)
  | 0x13 =>
    if funct3 w = 1 then
      if funct6 w = 0 then some (.slli (rdF w) (rs1F w) (shamt w)) else none
    else if funct3 w = 5 then
      if funct6 w = 16 then some (.srai (rdF w) (rs1F w) (shamt w)) else none
    else (IOp.all.find? fun op => op.code = funct3 w).map
      fun op => .i op (rdF w) (rs1F w) (immI w)
  | 0x03 => if funct3 w = 3 then some (.ld (rdF w) (rs1F w) (immI w)) else none
  | 0x23 => if funct3 w = 3 then some (.sd (rs2F w) (rs1F w) (immS w)) else none
  | 0x63 => (BOp.all.find? fun op => op.code = funct3 w).map
      fun op => .branch op (rs1F w) (rs2F w) (immB w)
  | 0x6f => some (.jal (rdF w) (immJ w))
  | 0x67 => if funct3 w = 0 then some (.jalr (rdF w) (rs1F w) (immI w)) else none
  | 0x73 => if w = 0x73 then some .ecall else none
  | 0x53 =>
    if funct7 w = 0x79 ∧ funct3 w = 0 ∧ rs2F w = 0 then some (.fmvDX (rdF w) (rs1F w))
    else if funct7 w = 0x71 ∧ funct3 w = 0 ∧ rs2F w = 0 then some (.fmvXD (rdF w) (rs1F w))
    else (FOp.all.find? fun op => op.code = (funct7 w, funct3 w)).map
      fun op => .fp op (rdF w) (rs1F w) (rs2F w)
  | _ => none

/-! ## The encoder -/

/-- The R-type layout; the other formats reuse it with immediate bits in the
register and function fields. -/
def encR (f7 rs2 rs1 f3 rd opc : Nat) : Nat :=
  f7 * 2 ^ 25 + rs2 * 2 ^ 20 + rs1 * 2 ^ 15 + f3 * 2 ^ 12 + rd * 2 ^ 7 + opc

/-- The low `n` bits of an integer's two's-complement form. -/
def bits (n : Nat) (v : Int) : Nat := (v % (2 ^ n : Int)).toNat

def encI (imm : Int) (rs1 f3 rd opc : Nat) : Nat :=
  encR (bits 12 imm / 32) (bits 12 imm % 32) rs1 f3 rd opc
def encS (imm : Int) (rs2 rs1 f3 opc : Nat) : Nat :=
  encR (bits 12 imm / 32) rs2 rs1 f3 (bits 12 imm % 32) opc
def encB (off : Int) (rs2 rs1 f3 opc : Nat) : Nat :=
  encR (bits 13 off / 4096 % 2 * 64 + bits 13 off / 32 % 64) rs2 rs1 f3
    (bits 13 off / 2 % 16 * 2 + bits 13 off / 2048 % 2) opc
def encJ (off : Int) (rd opc : Nat) : Nat :=
  encR (bits 21 off / 2 ^ 20 % 2 * 64 + bits 21 off / 32 % 64)
    (bits 21 off / 2 % 16 * 2 + bits 21 off / 2048 % 2) (bits 21 off / 2 ^ 15 % 32)
    (bits 21 off / 4096 % 8) rd opc

def encode : Inst → Nat
  | .r op rd rs1 rs2 => encR op.code.1 rs2 rs1 op.code.2 rd 0x33
  | .i op rd rs1 imm => encI imm rs1 op.code rd 0x13
  | .slli rd rs1 sh => encI sh rs1 1 rd 0x13
  | .srai rd rs1 sh => encI (0x400 + sh) rs1 5 rd 0x13
  | .ld rd rs1 imm => encI imm rs1 3 rd 0x03
  | .sd rs2 rs1 imm => encS imm rs2 rs1 3 0x23
  | .branch op rs1 rs2 off => encB off rs2 rs1 op.code 0x63
  | .jal rd off => encJ off rd 0x6f
  | .jalr rd rs1 imm => encI imm rs1 0 rd 0x67
  | .ecall => 0x73
  | .fp op rd rs1 rs2 => encR op.code.1 rs2 rs1 op.code.2 rd 0x53
  | .fmvDX rd rs1 => encR 0x79 0 rs1 0 rd 0x53
  | .fmvXD rd rs1 => encR 0x71 0 rs1 0 rd 0x53

/-- Little-endian bytes of a 32-bit word. -/
def bytes (w : Nat) : List Nat := [w % 256, w / 2 ^ 8 % 256, w / 2 ^ 16 % 256, w / 2 ^ 24]

/-! ## Well-formedness -/

def Imm12 (v : Int) : Prop := -2048 ≤ v ∧ v < 2048

def Inst.Wf : Inst → Prop
  | .r _ rd rs1 rs2 | .fp _ rd rs1 rs2 => rd < 32 ∧ rs1 < 32 ∧ rs2 < 32
  | .i _ rd rs1 imm | .ld rd rs1 imm | .jalr rd rs1 imm => rd < 32 ∧ rs1 < 32 ∧ Imm12 imm
  | .slli rd rs1 sh | .srai rd rs1 sh => rd < 32 ∧ rs1 < 32 ∧ sh < 64
  | .sd rs2 rs1 imm => rs2 < 32 ∧ rs1 < 32 ∧ Imm12 imm
  | .branch _ rs1 rs2 off => rs1 < 32 ∧ rs2 < 32 ∧ -4096 ≤ off ∧ off < 4096 ∧ off % 2 = 0
  | .jal rd off => rd < 32 ∧ -2 ^ 20 ≤ off ∧ off < 2 ^ 20 ∧ off % 2 = 0
  | .ecall => True
  | .fmvDX rd rs1 | .fmvXD rd rs1 => rd < 32 ∧ rs1 < 32

/-! ## Field lemmas -/

theorem bits12_lt (v : Int) : bits 12 v < 4096 := by unfold bits; omega
theorem bits13_lt (v : Int) : bits 13 v < 8192 := by unfold bits; omega
theorem bits21_lt (v : Int) : bits 21 v < 2 ^ 21 := by unfold bits; omega

theorem sext_bits12 (v : Int) (h : Imm12 v) : sext 11 (bits 12 v) = v := by
  unfold sext bits Imm12 at *; split <;> omega


/-! Each format's fields read back from its encoding. -/

theorem fields_R {f7 rs2 rs1 f3 rd opc : Nat} (h7 : f7 < 128) (h2 : rs2 < 32)
    (h1 : rs1 < 32) (h3 : f3 < 8) (hd : rd < 32) (ho : opc < 128) :
    encR f7 rs2 rs1 f3 rd opc < 2 ^ 32 ∧ opcode (encR f7 rs2 rs1 f3 rd opc) = opc ∧
      rdF (encR f7 rs2 rs1 f3 rd opc) = rd ∧ funct3 (encR f7 rs2 rs1 f3 rd opc) = f3 ∧
      rs1F (encR f7 rs2 rs1 f3 rd opc) = rs1 ∧ rs2F (encR f7 rs2 rs1 f3 rd opc) = rs2 ∧
      funct7 (encR f7 rs2 rs1 f3 rd opc) = f7 := by
  simp only [encR, opcode, rdF, funct3, rs1F, rs2F, funct7]; omega

/-- The B and J immediates' bit groups, read through the R-type fields. -/
theorem immB_fields (w : Nat) :
    immB w = sext 12 (funct7 w / 64 * 4096 + rdF w % 2 * 2048 + funct7 w % 64 * 32 +
      rdF w / 2 * 2) := by
  unfold immB; exact congrArg (sext 12) (by simp only [funct7, rdF, Nat.reducePow]; omega)

theorem immJ_fields (w : Nat) :
    immJ w = sext 20 (funct7 w / 64 * 2 ^ 20 + (rs1F w * 8 + funct3 w) * 2 ^ 12 +
      rs2F w % 2 * 2048 + (funct7 w % 64 * 16 + rs2F w / 2) * 2) := by
  unfold immJ
  exact congrArg (sext 20) (by simp only [funct7, rs1F, funct3, rs2F, Nat.reducePow]; omega)

theorem shift_fields (w : Nat) :
    funct6 w = funct7 w / 2 ∧ shamt w = funct7 w % 2 * 32 + rs2F w := by
  simp only [funct6, shamt, funct7, rs2F, Nat.reducePow]; omega

theorem immI_fields (w : Nat) : immI w = sext 11 (funct7 w * 32 + rs2F w) := by
  unfold immI; exact congrArg (sext 11) (by simp only [funct7, rs2F, Nat.reducePow]; omega)

/-- A value packed from a high and a low part unpacks to them. -/
theorem pack (p q n : Nat) (hn : 0 < n) (hq : q < n) :
    (p * n + q) / n = p ∧ (p * n + q) % n = q := by
  constructor
  · rw [Nat.add_comm, Nat.add_mul_div_right _ _ hn, Nat.div_eq_of_lt hq, Nat.zero_add]
  · rw [Nat.add_comm, Nat.add_mul_mod_self_right, Nat.mod_eq_of_lt hq]

theorem sext_bits13 (v : Int) (h : -4096 ≤ v ∧ v < 4096 ∧ v % 2 = 0) :
    sext 12 ((bits 13 v / 4096 % 2 * 64 + bits 13 v / 32 % 64) / 64 * 4096 +
      (bits 13 v / 2 % 16 * 2 + bits 13 v / 2048 % 2) % 2 * 2048 +
      (bits 13 v / 4096 % 2 * 64 + bits 13 v / 32 % 64) % 64 * 32 +
      (bits 13 v / 2 % 16 * 2 + bits 13 v / 2048 % 2) / 2 * 2) = v := by
  have hb := bits13_lt v
  have hv : ((bits 13 v : Nat) : Int) = v % 8192 := by unfold bits; omega
  generalize bits 13 v = b at *
  rw [(pack (b / 4096 % 2) (b / 32 % 64) 64 (by omega) (by omega)).1,
    (pack (b / 4096 % 2) (b / 32 % 64) 64 (by omega) (by omega)).2,
    (pack (b / 2 % 16) (b / 2048 % 2) 2 (by omega) (by omega)).1,
    (pack (b / 2 % 16) (b / 2048 % 2) 2 (by omega) (by omega)).2,
    show b / 4096 % 2 * 4096 + b / 2048 % 2 * 2048 + b / 32 % 64 * 32 + b / 2 % 16 * 2 = b by
      omega]; unfold sext; simp only [Nat.reducePow] at *; split <;> omega

theorem sext_bits21 (v : Int) (h : -2 ^ 20 ≤ v ∧ v < 2 ^ 20 ∧ v % 2 = 0) :
    sext 20 ((bits 21 v / 2 ^ 20 % 2 * 64 + bits 21 v / 32 % 64) / 64 * 2 ^ 20 +
      (bits 21 v / 2 ^ 15 % 32 * 8 + bits 21 v / 4096 % 8) * 2 ^ 12 +
      (bits 21 v / 2 % 16 * 2 + bits 21 v / 2048 % 2) % 2 * 2048 +
      ((bits 21 v / 2 ^ 20 % 2 * 64 + bits 21 v / 32 % 64) % 64 * 16 +
        (bits 21 v / 2 % 16 * 2 + bits 21 v / 2048 % 2) / 2) * 2) = v := by
  have hb := bits21_lt v
  have hv : ((bits 21 v : Nat) : Int) = v % 2097152 := by unfold bits; omega
  generalize bits 21 v = b at *
  rw [(pack (b / 2 ^ 20 % 2) (b / 32 % 64) 64 (by omega) (by omega)).1,
    (pack (b / 2 ^ 20 % 2) (b / 32 % 64) 64 (by omega) (by omega)).2,
    (pack (b / 2 % 16) (b / 2048 % 2) 2 (by omega) (by omega)).1,
    (pack (b / 2 % 16) (b / 2048 % 2) 2 (by omega) (by omega)).2,
    show b / 2 ^ 20 % 2 * 2 ^ 20 + (b / 2 ^ 15 % 32 * 8 + b / 4096 % 8) * 2 ^ 12 +
      b / 2048 % 2 * 2048 + (b / 32 % 64 * 16 + b / 2 % 16) * 2 = b by omega]
  unfold sext; simp only [Nat.reducePow] at *; split <;> omega

/-! ## The encoder meets the specification -/

theorem ROp.code_lt (op : ROp) : op.code.1 < 128 ∧ op.code.2 < 8 := by cases op <;> decide
theorem FOp.code_lt (op : FOp) : op.code.1 < 128 ∧ op.code.2 < 8 := by cases op <;> decide
theorem IOp.code_lt (op : IOp) : op.code < 8 := by cases op <;> decide
theorem BOp.code_lt (op : BOp) : op.code < 8 := by cases op <;> decide

/-- Every well-formed instruction encodes to a 32-bit word that the
specification decodes back to the same instruction. -/
theorem decode_encode (i : Inst) (h : i.Wf) :
    encode i < 2 ^ 32 ∧ decode (encode i) = some i := by
  cases i with
  | r op rd rs1 rs2 =>
    obtain ⟨hd, h1, h2⟩ := h
    obtain ⟨lt, ho, hr, h3, hs1, hs2, h7⟩ :=
      fields_R op.code_lt.1 h2 h1 op.code_lt.2 hd (by decide : 0x33 < 128)
    refine ⟨lt, ?_⟩
    rw [encode, decode, ho, hr, h3, hs1, hs2, h7]
    cases op <;> rfl
  | fp op rd rs1 rs2 =>
    obtain ⟨hd, h1, h2⟩ := h
    obtain ⟨lt, ho, hr, h3, hs1, hs2, h7⟩ :=
      fields_R op.code_lt.1 h2 h1 op.code_lt.2 hd (by decide : 0x53 < 128)
    refine ⟨lt, ?_⟩
    rw [encode, decode, ho, hr, h3, hs1, hs2, h7]
    cases op <;> rfl
  | fmvDX rd rs1 =>
    obtain ⟨hd, h1⟩ := h
    obtain ⟨lt, ho, hr, h3, hs1, hs2, h7⟩ :=
      fields_R (by decide : 0x79 < 128) (by decide : 0 < 32) h1 (by decide : 0 < 8) hd
        (by decide : 0x53 < 128)
    refine ⟨lt, ?_⟩
    rw [encode, decode, ho, hr, h3, hs1, hs2, h7]
    rfl
  | fmvXD rd rs1 =>
    obtain ⟨hd, h1⟩ := h
    obtain ⟨lt, ho, hr, h3, hs1, hs2, h7⟩ :=
      fields_R (by decide : 0x71 < 128) (by decide : 0 < 32) h1 (by decide : 0 < 8) hd
        (by decide : 0x53 < 128)
    refine ⟨lt, ?_⟩
    rw [encode, decode, ho, hr, h3, hs1, hs2, h7]
    rfl
  | i op rd rs1 imm =>
    obtain ⟨hd, h1, hi⟩ := h
    have hb := bits12_lt imm
    obtain ⟨lt, ho, hr, h3, hs1, hs2, h7⟩ :=
      fields_R (f7 := bits 12 imm / 32) (rs2 := bits 12 imm % 32) (by omega) (by omega) h1
        op.code_lt hd (by decide : 0x13 < 128)
    have hx : bits 12 imm / 32 * 32 + bits 12 imm % 32 = bits 12 imm := by omega
    refine ⟨lt, ?_⟩
    rw [encode, encI, decode, ho, hr, h3, hs1, immI_fields, h7, hs2, hx, sext_bits12 imm hi]
    cases op <;> rfl
  | slli rd rs1 sh =>
    obtain ⟨hd, h1, hs⟩ := h
    have hb : bits 12 (sh : Int) = sh := by unfold bits; omega
    obtain ⟨lt, ho, hr, h3, hs1, hs2, h7⟩ :=
      fields_R (f7 := sh / 32) (rs2 := sh % 32) (by omega) (by omega) h1
        (by decide : 1 < 8) hd (by decide : 0x13 < 128)
    have h6 := shift_fields (encR (sh / 32) (sh % 32) rs1 1 rd 0x13)
    rw [h7, hs2] at h6
    simp only [encode, encI, hb]
    refine ⟨lt, ?_⟩
    rw [decode, ho, hr, h3, hs1, h6.1, h6.2, show sh / 32 / 2 = 0 by omega,
      show sh / 32 % 2 * 32 + sh % 32 = sh by omega]
    rfl
  | srai rd rs1 sh =>
    obtain ⟨hd, h1, hs⟩ := h
    have hb : bits 12 (0x400 + (sh : Int)) = 1024 + sh := by unfold bits; omega
    obtain ⟨lt, ho, hr, h3, hs1, hs2, h7⟩ :=
      fields_R (f7 := (1024 + sh) / 32) (rs2 := (1024 + sh) % 32) (by omega) (by omega) h1
        (by decide : 5 < 8) hd (by decide : 0x13 < 128)
    have h6 := shift_fields (encR ((1024 + sh) / 32) ((1024 + sh) % 32) rs1 5 rd 0x13)
    rw [h7, hs2] at h6
    simp only [encode, encI, hb]
    refine ⟨lt, ?_⟩
    rw [decode, ho, hr, h3, hs1, h6.1, h6.2, show (1024 + sh) / 32 / 2 = 16 by omega,
      show (1024 + sh) / 32 % 2 * 32 + (1024 + sh) % 32 = sh by omega]
    rfl
  | ld rd rs1 imm =>
    obtain ⟨hd, h1, hi⟩ := h
    have hb := bits12_lt imm
    obtain ⟨lt, ho, hr, h3, hs1, hs2, h7⟩ :=
      fields_R (f7 := bits 12 imm / 32) (rs2 := bits 12 imm % 32) (by omega) (by omega) h1
        (by decide : 3 < 8) hd (by decide : 0x03 < 128)
    have hx : bits 12 imm / 32 * 32 + bits 12 imm % 32 = bits 12 imm := by omega
    refine ⟨lt, ?_⟩
    rw [encode, encI, decode, ho, hr, h3, hs1, immI_fields, h7, hs2, hx, sext_bits12 imm hi]
    rfl
  | jalr rd rs1 imm =>
    obtain ⟨hd, h1, hi⟩ := h
    have hb := bits12_lt imm
    obtain ⟨lt, ho, hr, h3, hs1, hs2, h7⟩ :=
      fields_R (f7 := bits 12 imm / 32) (rs2 := bits 12 imm % 32) (by omega) (by omega) h1
        (by decide : 0 < 8) hd (by decide : 0x67 < 128)
    have hx : bits 12 imm / 32 * 32 + bits 12 imm % 32 = bits 12 imm := by omega
    refine ⟨lt, ?_⟩
    rw [encode, encI, decode, ho, hr, h3, hs1, immI_fields, h7, hs2, hx, sext_bits12 imm hi]
    rfl
  | sd rs2 rs1 imm =>
    obtain ⟨h2, h1, hi⟩ := h
    have hb := bits12_lt imm
    obtain ⟨lt, ho, hr, h3, hs1, hs2, h7⟩ :=
      fields_R (f7 := bits 12 imm / 32) (rd := bits 12 imm % 32) (by omega) h2 h1
        (by decide : 3 < 8) (by omega) (by decide : 0x23 < 128)
    have hx : bits 12 imm / 32 * 32 + bits 12 imm % 32 = bits 12 imm := by omega
    refine ⟨lt, ?_⟩
    rw [encode, encS, decode, ho, immS, hr, h3, hs1, hs2, h7, hx, sext_bits12 imm hi]
    rfl
  | branch op rs1 rs2 off =>
    obtain ⟨h1, h2, hoff⟩ := h
    have hb := bits13_lt off
    obtain ⟨lt, ho, hr, h3, hs1, hs2, h7⟩ :=
      fields_R (f7 := bits 13 off / 4096 % 2 * 64 + bits 13 off / 32 % 64)
        (rd := bits 13 off / 2 % 16 * 2 + bits 13 off / 2048 % 2) (by omega) h2 h1
        op.code_lt (by omega) (by decide : 0x63 < 128)
    have hx := sext_bits13 off hoff
    refine ⟨lt, ?_⟩
    rw [encode, encB, decode, ho, immB_fields, h3, hs1, hs2, h7, hr, hx]
    cases op <;> rfl
  | jal rd off =>
    obtain ⟨hd, hoff⟩ := h
    have hb := bits21_lt off
    obtain ⟨lt, ho, hr, h3, hs1, hs2, h7⟩ :=
      fields_R (f7 := bits 21 off / 2 ^ 20 % 2 * 64 + bits 21 off / 32 % 64)
        (rs2 := bits 21 off / 2 % 16 * 2 + bits 21 off / 2048 % 2)
        (rs1 := bits 21 off / 2 ^ 15 % 32) (f3 := bits 21 off / 4096 % 8)
        (by omega) (by omega) (by omega) (by omega) hd (by decide : 0x6f < 128)
    have hx := sext_bits21 off hoff
    refine ⟨lt, ?_⟩
    rw [encode, encJ, decode, ho, immJ_fields, hr, h7, hs1, h3, hs2, hx]
    rfl
  | ecall => exact ⟨by decide, rfl⟩

theorem encode_lt (i : Inst) (h : i.Wf) : encode i < 2 ^ 32 := (decode_encode i h).1

/-- The four bytes `riscv64.rs` writes reassemble to the encoded word. -/
theorem bytes_encode (i : Inst) (h : i.Wf) :
    match bytes (encode i) with
    | [b0, b1, b2, b3] => b0 < 256 ∧ b1 < 256 ∧ b2 < 256 ∧ b3 < 256 ∧
        b0 + 2 ^ 8 * b1 + 2 ^ 16 * b2 + 2 ^ 24 * b3 = encode i
    | _ => False := by
  have := encode_lt i h
  simp only [bytes]; omega


/-! ## `riscv64::emit` -/

/-- Integer registers by ABI name. -/
def zero : Nat := 0
def ra : Nat := 1
def sp : Nat := 2
def t0 : Nat := 5
def t1 : Nat := 6
def s0 : Nat := 8
def a0 : Nat := 10
def a1 : Nat := 11
def a2 : Nat := 12

def regX : Reg → Nat
  | .r0 => a0 | .r1 => a1 | .r2 => a2

/-- `FIRST_FREE_LABEL`: labels below it are the abort exits of
`TrapKind::ALL` (`tests/verified_codegen_lean.rs` checks the value against the
Rust constant); every label from it on is a function or block label. -/
def firstFree : Nat := 3

/-- The program-wide abort exits (`TrapKind::label`); `fault` is not a
trap kind of the Rust emitter, which never selects it. -/
def trapLabel : Failure → Nat
  | .overflow => 0 | .divZero => 1 | .unreachable => 2 | .fault => 3

/-- An emitted instruction, or a `jal rd` to a label resolved by layout. -/
inductive Item
  | ins (i : Inst)
  | jump (rd : Nat) (l : Nat)

def addi (rd rs1 : Nat) (imm : Int) : Item := .ins (.i .addi rd rs1 imm)

/-- `li rd, w`: the top nine bits, then five 11-bit chunks shifted in. -/
def imm (rd w : Nat) : List Item :=
  [addi rd zero (w / 2 ^ 55),
   .ins (.slli rd rd 11), .ins (.i .ori rd rd (w / 2 ^ 44 % 2048)),
   .ins (.slli rd rd 11), .ins (.i .ori rd rd (w / 2 ^ 33 % 2048)),
   .ins (.slli rd rd 11), .ins (.i .ori rd rd (w / 2 ^ 22 % 2048)),
   .ins (.slli rd rd 11), .ins (.i .ori rd rd (w / 2 ^ 11 % 2048)),
   .ins (.slli rd rd 11), .ins (.i .ori rd rd (w % 2048))]

/-- The base register and byte offset of a frame word. -/
def frame : Loc → Nat × Nat
  | .slot n | .out n => (sp, n * 8)
  | .inn n => (s0, 16 + n * 8)

def memOp : Bool → Nat → Nat → Nat → Item
  | true, r, base, off => .ins (.ld r base off)
  | false, r, base, off => .ins (.sd r base off)

def ldst (load : Bool) (r : Nat) (l : Loc) : List Item :=
  if 2048 ≤ (frame l).2 then
    imm t0 (frame l).2 ++ [.ins (.r .add t0 t0 (frame l).1), memOp load r t0 0]
  else [memOp load r (frame l).1 (frame l).2]

def subSp (bytes : Nat) : List Item :=
  if bytes < 2048 then [addi sp sp (-(bytes : Int))]
  else imm t0 bytes ++ [.ins (.r .sub sp sp t0)]

def intMin (rd : Nat) : List Item := [addi rd zero 1, .ins (.slli rd rd 63)]

/-- Branch to `l` when the condition holds: the inverted branch skips a
`jal`. -/
def branchTo (op : BOp) (rs1 rs2 l : Nat) : List Item :=
  [.ins (.branch op rs1 rs2 8), .jump zero l]

def ovf : Nat := trapLabel .overflow

def lt (sg : Sign) (rd x y : Nat) : Item :=
  .ins (.r (if sg = .s then .slt else .sltu) rd x y)

def fopOf : FloatOp → FOp
  | .add => .faddD | .sub => .fsubD | .mul => .fmulD | .div => .fdivD

def emit : M → List Item
  | .ld r l => ldst true (regX r) l
  | .st l r => ldst false (regX r) l
  | .imm r w => imm (regX r) (w % 2 ^ 64).toNat
  | .addchk .u => [.ins (.r .add a0 a0 a1)] ++ branchTo .bgeu a0 a1 ovf
  | .addchk .s => [addi t0 a0 0, .ins (.r .add a0 a0 a1), .ins (.r .slt t1 a1 zero),
      .ins (.r .slt t0 a0 t0)] ++ branchTo .beq t1 t0 ovf
  | .subchk .u => branchTo .bgeu a0 a1 ovf ++ [.ins (.r .sub a0 a0 a1)]
  | .subchk .s => [addi t0 a0 0, .ins (.r .sub a0 a0 a1), .ins (.r .slt t1 zero a1),
      .ins (.r .slt t0 a0 t0)] ++ branchTo .beq t1 t0 ovf
  | .mulchk .s => [.ins (.r .mulh a2 a0 a1), .ins (.r .mul a0 a0 a1),
      .ins (.srai t0 a0 63)] ++ branchTo .beq a2 t0 ovf
  | .mulchk .u => [.ins (.r .mulhu a2 a0 a1), .ins (.r .mul a0 a0 a1)] ++
      branchTo .beq a2 zero ovf
  | .wrap .add => [.ins (.r .add a0 a0 a1)]
  | .wrap .sub => [.ins (.r .sub a0 a0 a1)]
  | .wrap .mul => [.ins (.r .mul a0 a0 a1)]
  | .andr2 => [.ins (.r .and a0 a0 a2)]
  | .trapgtu => branchTo .bgeu a2 a0 ovf
  | .trapzero => branchTo .bne a1 zero (trapLabel .divZero)
  | .trapdivovf => intMin a2 ++ [.ins (.branch .bne a0 a2 16), addi t0 zero (-1),
      .ins (.branch .bne a1 t0 8), .jump zero ovf]
  | .div .s => [.ins (.r .div a0 a0 a1)]
  | .div .u => [.ins (.r .divu a0 a0 a1)]
  | .rem .s => [.ins (.r .rem a0 a0 a1)]
  | .rem .u => [.ins (.r .remu a0 a0 a1)]
  | .negchk .s => intMin t0 ++ branchTo .bne a0 t0 ovf ++ [.ins (.r .sub a0 zero a0)]
  | .negchk .u => branchTo .beq a0 zero ovf
  | .xorone => [.ins (.i .xori a0 a0 1)]
  | .cmp .eq _ => [.ins (.r .sub t0 a0 a1), .ins (.i .sltiu a0 t0 1)]
  | .cmp .ne _ => [.ins (.r .sub t0 a0 a1), .ins (.r .sltu a0 zero t0)]
  | .cmp .lt sg => [lt sg a0 a0 a1]
  | .cmp .gt sg => [lt sg a0 a1 a0]
  | .cmp .le sg => [lt sg a0 a1 a0, .ins (.i .xori a0 a0 1)]
  | .cmp .ge sg => [lt sg a0 a0 a1, .ins (.i .xori a0 a0 1)]
  | .fop op => [.ins (.fmvDX 0 a0), .ins (.fmvDX 1 a1), .ins (.fp (fopOf op) 0 0 1),
      .ins (.fmvXD a0 0)]
  | .fneg => [.ins (.fmvDX 0 a0), .ins (.fp .fsgnjnD 0 0 0), .ins (.fmvXD a0 0)]
  | .fcmp c =>
    [.ins (.fmvDX 0 a0), .ins (.fmvDX 1 a1)] ++
    match c with
    | .eq => [.ins (.fp .feqD a0 0 1)]
    | .ne => [.ins (.fp .feqD a0 0 1), .ins (.i .xori a0 a0 1)]
    | .lt => [.ins (.fp .fltD a0 0 1)]
    | .le => [.ins (.fp .fleD a0 0 1)]
    | .gt => [.ins (.fp .fltD a0 1 0)]
    | .ge => [.ins (.fp .fleD a0 1 0)]
  | .label _ => []
  | .jmp l => [.jump zero l]
  | .bz l => branchTo .bne a0 zero l
  | .call l => [.jump ra l]
  | .enter words => [addi sp sp (-16), .ins (.sd ra sp 8), .ins (.sd s0 sp 0), addi s0 sp 0] ++
      subSp (words * 8)
  | .leave => [addi sp s0 0, .ins (.ld s0 sp 0), .ins (.ld ra sp 8), addi sp sp 16,
      .ins (.jalr zero ra 0)]
  | .trap f => [.jump zero (trapLabel f)]
  -- Milestone 7: a trap scope emits no code (it changes which labels the
  -- checked instructions' branches name).
  | .scope _ _ => []

/-! ## Layout: abort exits before the instruction, other labels after it -/

def Item.at (k n : Nat) : Item → Inst
  | .ins i => i
  | .jump rd l => .jal rd (if l < firstFree then -(4 * k : Int) else 4 * (n - k : Nat))

def place (n : Nat) : Nat → List Item → List Inst
  | _, [] => []
  | k, it :: rest => it.at k n :: place n (k + 1) rest

def layout (items : List Item) : List Inst := place items.length 0 items

def Item.Ok : Item → Prop
  | .ins i => i.Wf
  | .jump rd _ => rd < 32

instance (i : Inst) : Decidable i.Wf := by
  cases i <;> simp only [Inst.Wf, Imm12] <;> infer_instance
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
      | jump rd l =>
        simp only [Item.at, Inst.Wf]
        simp only [List.length_cons] at hk
        refine ⟨this, ?_, ?_, ?_⟩ <;> split <;> omega
    · exact ih (k + 1) (by simp at hk; omega) (fun it h => hok it (by simp [h])) i hi

/-- `M` operands the Rust emitter can be given: frame indices and frame
sizes are `u32`. -/
def mBounded : M → Prop
  | .ld _ l | .st l _ => match l with | .slot n | .out n | .inn n => n < 2 ^ 32
  | .enter words => words < 2 ^ 32
  | _ => True

theorem imm_ok (rd w : Nat) (hd : rd < 32) (hw : w < 2 ^ 64) : ∀ it ∈ imm rd w, it.Ok := by
  simp only [imm, addi, List.mem_cons, List.not_mem_nil, or_false]
  intro it hit
  rcases hit with rfl | rfl | rfl | rfl | rfl | rfl | rfl | rfl | rfl | rfl | rfl <;>
    simp only [Item.Ok, Inst.Wf, Imm12, zero] <;> omega

theorem ldst_ok (load : Bool) (r : Nat) (l : Loc) (hr : r < 32)
    (hl : match l with | .slot n | .out n | .inn n => n < 2 ^ 32) :
    ∀ it ∈ ldst load r l, it.Ok := by
  have hf : (frame l).1 < 32 ∧ (frame l).2 < 2 ^ 64 := by
    cases l <;> simp only [frame, sp, s0] at hl ⊢ <;> omega
  unfold ldst
  split
  · intro it hit
    simp only [List.mem_append, List.mem_cons, List.not_mem_nil, or_false] at hit
    rcases hit with hit | rfl | rfl
    · exact imm_ok t0 _ (by decide) hf.2 it hit
    · simp only [Item.Ok, Inst.Wf, t0]; omega
    · cases load <;> simp only [memOp, Item.Ok, Inst.Wf, Imm12, t0] <;> omega
  · intro it hit
    simp only [List.mem_cons, List.not_mem_nil, or_false] at hit
    subst hit
    cases load <;> simp only [memOp, Item.Ok, Inst.Wf, Imm12] <;> omega

theorem subSp_ok (bytes : Nat) (hb : bytes < 2 ^ 64) : ∀ it ∈ subSp bytes, it.Ok := by
  unfold subSp
  split
  · intro it hit
    simp only [List.mem_cons, List.not_mem_nil, or_false] at hit
    subst hit; simp only [addi, Item.Ok, Inst.Wf, Imm12, sp]; omega
  · intro it hit
    simp only [List.mem_append, List.mem_cons, List.not_mem_nil, or_false] at hit
    rcases hit with hit | rfl
    · exact imm_ok t0 _ (by decide) hb it hit
    · simp only [Item.Ok, Inst.Wf, sp, t0]; omega

/-- Every item `emit` produces is well formed. -/
theorem emit_ok (m : M) (hm : mBounded m) : ∀ it ∈ emit m, it.Ok := by
  cases m with
  | ld r l => exact ldst_ok true (regX r) l (by cases r <;> decide) hm
  | st l r => exact ldst_ok false (regX r) l (by cases r <;> decide) hm
  | imm r w =>
    exact imm_ok (regX r) _ (by cases r <;> decide) (show (w % 2 ^ 64).toNat < 2 ^ 64 by omega)
  | enter words =>
    intro it hit
    simp only [emit, List.mem_append, List.mem_cons, List.not_mem_nil, or_false] at hit
    rcases hit with (rfl | rfl | rfl | rfl) | hit
    all_goals first
      | exact subSp_ok _ (by simp only [mBounded] at hm; omega) it hit
      | (simp only [addi, Item.Ok, Inst.Wf, Imm12, sp, s0, ra]; omega)
  | addchk sg | subchk sg | mulchk sg | div sg | rem sg | negchk sg =>
    cases sg <;> decide
  | wrap op | fop op => cases op <;> decide
  | cmp c sg => cases c <;> cases sg <;> decide
  | fcmp c => cases c <;> decide
  | trap f => cases f <;> decide
  | label l | jmp l | bz l | call l =>
    simp [emit, branchTo, Item.Ok, Inst.Wf, zero, a0, ra]
  | scope _ _ => simp [emit]
  | _ => decide

theorem emit_length (m : M) : (emit m).length ≤ 24 := by
  cases m with
  | ld _ l | st l _ => simp only [emit, ldst]; split <;> simp [imm]
  | enter words => simp only [emit, subSp]; split <;> simp [imm]
  | imm _ _ => simp [emit, imm]
  | addchk sg | subchk sg | mulchk sg | div sg | rem sg | negchk sg =>
    cases sg <;> decide
  | wrap op | fop op => cases op <;> decide
  | cmp c sg => cases c <;> cases sg <;> decide
  | fcmp c => cases c <;> decide
  | trap f => cases f <;> decide
  | _ => simp [emit, branchTo, intMin]

/-- The theorem the listing relies on: every word of the laid-out `emit m`
is the encoding of a well-formed instruction, a 32-bit word the ISA
specification decodes back to that instruction. -/
theorem emit_decodes (m : M) (hm : mBounded m) :
    ∀ i ∈ layout (emit m), encode i < 2 ^ 32 ∧ decode (encode i) = some i := by
  intro i hi
  exact decode_encode i
    (place_wf _ (by have := emit_length m; omega) (emit m) 0 (by omega) (emit_ok m hm) i hi)

/-! ## The printed listing (compared with `riscv64::encoding_listing()`) -/

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
  [.jmp 9, .jmp firstFree, .bz 9, .call 9, .enter 4, .enter 300, .leave, .trap .overflow, .trap .divZero,
   .trap .unreachable]

def listing : String :=
  String.join (samples.map fun m =>
    m.render ++ ":" ++ String.join ((layout (emit m)).map fun i => " " ++ hex8 (encode i)) ++
      "\n")

#eval IO.print ("BEGIN RV64\n" ++ listing ++ "END RV64\n")

#print axioms Vesper.VerifiedCodegen.Rv64.bits12_lt
#print axioms Vesper.VerifiedCodegen.Rv64.bits13_lt
#print axioms Vesper.VerifiedCodegen.Rv64.bits21_lt
#print axioms Vesper.VerifiedCodegen.Rv64.sext_bits12
#print axioms Vesper.VerifiedCodegen.Rv64.fields_R
#print axioms Vesper.VerifiedCodegen.Rv64.immB_fields
#print axioms Vesper.VerifiedCodegen.Rv64.immJ_fields
#print axioms Vesper.VerifiedCodegen.Rv64.shift_fields
#print axioms Vesper.VerifiedCodegen.Rv64.immI_fields
#print axioms Vesper.VerifiedCodegen.Rv64.pack
#print axioms Vesper.VerifiedCodegen.Rv64.sext_bits13
#print axioms Vesper.VerifiedCodegen.Rv64.sext_bits21
#print axioms Vesper.VerifiedCodegen.Rv64.ROp.code_lt
#print axioms Vesper.VerifiedCodegen.Rv64.FOp.code_lt
#print axioms Vesper.VerifiedCodegen.Rv64.IOp.code_lt
#print axioms Vesper.VerifiedCodegen.Rv64.BOp.code_lt
#print axioms Vesper.VerifiedCodegen.Rv64.decode_encode
#print axioms Vesper.VerifiedCodegen.Rv64.encode_lt
#print axioms Vesper.VerifiedCodegen.Rv64.bytes_encode
#print axioms Vesper.VerifiedCodegen.Rv64.place_wf
#print axioms Vesper.VerifiedCodegen.Rv64.imm_ok
#print axioms Vesper.VerifiedCodegen.Rv64.ldst_ok
#print axioms Vesper.VerifiedCodegen.Rv64.subSp_ok
#print axioms Vesper.VerifiedCodegen.Rv64.emit_ok
#print axioms Vesper.VerifiedCodegen.Rv64.emit_length
#print axioms Vesper.VerifiedCodegen.Rv64.emit_decodes

end Vesper.VerifiedCodegen.Rv64
