import VerifiedCodegen

/-!
# Verified direct codegen: x86-64 instruction encoding (§42.1 pillar 3b)

`VerifiedCodegen.lean` proves the instruction-selection table correct down to
the abstract machine `M`. This file covers the next step for the x86-64
target: the bytes `src/verified_codegen/x86_64.rs` writes for each `M`
instruction. It follows `CodegenRv64Encoding.lean` and
`CodegenA64Encoding.lean`, but x86-64 instructions have variable length, so
the encoding is a byte list and the specification is a prefix decoder.

* `Inst` is the x86-64 subset the emitter uses, named as in the Intel SDM
  (`MOV r64, r/m64`, `MOV r64, imm64`, `ADD r/m64, r64`, `IMUL r64, r/m64`,
  the `F7` group (`MUL`, `NEG`, `DIV`, `IDIV`), the `81`/`83` immediate
  groups, `Jcc`, `SETcc`, `MOVZX`, `MOVQ`, `ADDSD`/..., `UCOMISD`, `BTC`,
  `PUSH`, `POP`, `RET`, ...). Only the low eight registers occur, so no REX
  prefix other than `REX.W` (`48`) is used.
* `decode` is the specification: it reads the legacy prefix (`66`, `F2`),
  `REX.W`, the opcode (one byte or `0F` and a second byte), the ModRM byte
  (`mod` in bits 7:6, `reg` in 5:3, `r/m` in 2:0), the SIB byte where
  `r/m = 100`, and the little-endian displacement and immediate fields. It
  accepts only the operand forms the emitter writes (register operands with
  `mod = 11`; memory operands `[rsp + disp32]` with SIB `24`, and
  `[rbp + disp32]`), and rejects the others rather than misreading them.
  Displacements, immediates and branch offsets are the raw fields; the
  processor reads them sign-extended (except `imm64` and the `B8+r` forms).
* `encode` is the encoder, byte by byte like `x86_64.rs`.
* `decode_encode` proves that every well-formed instruction (registers below
  8, every field in its range) encodes to bytes below 256 that the
  specification decodes back to the same instruction, whatever bytes follow.
  `decodeAll_encodeAll` extends it to instruction sequences.
* `emit` is `x86_64::emit` for one `M` instruction (Linux or Windows), as a
  list of items; `emit_decodes` proves that the laid-out bytes of `emit`
  decode back to exactly its instruction sequence.
* `listing` prints the bytes of `emit` for sample `M` instructions;
  `tests/verified_codegen_lean.rs` checks it line for line against
  `x86_64::encoding_listing()`, which runs the Rust emitter on the same
  instructions. The theorems are about the Lean encoder; the listing ties the
  Rust encoder to it on those samples, not in general.

Branch targets: `emit` resolves every `rel32` against a layout where the
abort exits (labels below `firstFree`) sit at offset 0, before the instruction,
and every other label at the end, which is how the listing lays them out. A `rel32` is the target
minus the end of the instruction, modulo `2^32`.
-/

namespace Vesper.VerifiedCodegen.X64

/-! ## The instruction subset -/

/-- `op r/m64, r64` with a register `r/m` (`MR` encoding). -/
inductive Alu | add | sub | and | cmp | test | mov
  deriving DecidableEq, Repr
/-- The `F7 /digit` group on a register. -/
inductive G3 | mul | neg | div | idiv
  deriving DecidableEq, Repr
/-- The `81 /digit id` and `83 /digit ib` groups on a register. -/
inductive G1 | add | sub | xor | cmp
  deriving DecidableEq, Repr
/-- Scalar double-precision `F2 0F` operations. -/
inductive Sse | add | sub | mul | div
  deriving DecidableEq, Repr
/-- The memory operands the emitter uses. -/
inductive Base | rsp | rbp
  deriving DecidableEq, Repr

inductive Inst
  /-- `MOV r64, [base + disp32]` (`REX.W 8B /r`). -/
  | load (r : Nat) (b : Base) (disp : Nat)
  /-- `MOV [base + disp32], r64` (`REX.W 89 /r`). -/
  | store (r : Nat) (b : Base) (disp : Nat)
  /-- `MOV r64, imm64` (`REX.W B8+r io`). -/
  | movabs (r imm : Nat)
  /-- `op dst, src` on 64-bit registers. -/
  | alu (op : Alu) (dst src : Nat)
  /-- `IMUL dst, src` (`REX.W 0F AF /r`). -/
  | imul (dst src : Nat)
  | grp3 (op : G3) (r : Nat)
  /-- `CQO` (`REX.W 99`). -/
  | cqo
  /-- `XOR dst32, src32` (`31 /r`). -/
  | xor32 (dst src : Nat)
  /-- `op r64, imm8` (`REX.W 83 /digit ib`). -/
  | grpI8 (op : G1) (r imm : Nat)
  /-- `op r64, imm32` (`REX.W 81 /digit id`). -/
  | grpI32 (op : G1) (r imm : Nat)
  /-- `Jcc rel32` (`0F 80+cc cd`). -/
  | jcc (cc rel : Nat)
  /-- `Jcc rel8` (`70+cc cb`). -/
  | jcc8 (cc rel : Nat)
  /-- `JMP rel8` (`EB cb`). -/
  | jmp8 (rel : Nat)
  /-- `JMP rel32` (`E9 cd`). -/
  | jmp (rel : Nat)
  /-- `CALL rel32` (`E8 cd`). -/
  | call (rel : Nat)
  /-- `SETcc r8` (`0F 90+cc /0`). -/
  | setcc (cc r : Nat)
  /-- `MOVZX dst32, src8` (`0F B6 /r`). -/
  | movzxb (dst src : Nat)
  /-- `MOVQ xmm, r64` (`66 REX.W 0F 6E /r`). -/
  | movqToX (x r : Nat)
  /-- `MOVQ r64, xmm` (`66 REX.W 0F 7E /r`). -/
  | movqFromX (r x : Nat)
  /-- `op xmm_dst, xmm_src` (`F2 0F op /r`). -/
  | sse (op : Sse) (dst src : Nat)
  /-- `UCOMISD xmm_a, xmm_b` (`66 0F 2E /r`). -/
  | ucomisd (a b : Nat)
  /-- `BTC r64, imm8` (`REX.W 0F BA /7 ib`). -/
  | btc (r imm : Nat)
  /-- `MOV r32, imm32` (`B8+r id`). -/
  | movImm32 (r imm : Nat)
  /-- `TEST [rsp + disp32], r32` (`85 /r`). -/
  | testM (r disp : Nat)
  /-- `PUSH r64` (`50+r`). -/
  | push (r : Nat)
  /-- `POP r64` (`58+r`). -/
  | pop (r : Nat)
  | ret
  deriving DecidableEq, Repr

/-! ## The specification: the Intel SDM encodings -/

def Alu.code : Alu → Nat
  | .add => 0x01 | .sub => 0x29 | .and => 0x21 | .cmp => 0x39 | .test => 0x85 | .mov => 0x89
def G3.digit : G3 → Nat
  | .mul => 4 | .neg => 3 | .div => 6 | .idiv => 7
def G1.digit : G1 → Nat
  | .add => 0 | .sub => 5 | .xor => 6 | .cmp => 7
def Sse.code : Sse → Nat
  | .add => 0x58 | .sub => 0x5c | .mul => 0x59 | .div => 0x5e

/-- The operations of an `MR` opcode byte with a register operand (`89` is
`MOV`; with a memory operand it is the store). -/
def aluOf (o : Nat) : Option Alu :=
  if o = 0x01 then some .add else if o = 0x29 then some .sub
  else if o = 0x21 then some .and else if o = 0x39 then some .cmp
  else if o = 0x85 then some .test else if o = 0x89 then some .mov else none
def g3Of (d : Nat) : Option G3 :=
  if d = 4 then some .mul else if d = 3 then some .neg
  else if d = 6 then some .div else if d = 7 then some .idiv else none
def g1Of (d : Nat) : Option G1 :=
  if d = 0 then some .add else if d = 5 then some .sub
  else if d = 6 then some .xor else if d = 7 then some .cmp else none
def sseOf (o : Nat) : Option Sse :=
  if o = 0x58 then some .add else if o = 0x5c then some .sub
  else if o = 0x59 then some .mul else if o = 0x5e then some .div else none

/-- ModRM fields. -/
def modOf (m : Nat) : Nat := m / 64
def regOf (m : Nat) : Nat := m / 8 % 8
def rmOf (m : Nat) : Nat := m % 8

/-- `k` little-endian bytes of `n`. -/
def le : Nat → Nat → List Nat
  | 0, _ => []
  | k + 1, n => n % 256 :: le k (n / 256)

/-- Read a `k`-byte little-endian field. -/
def rdLE : Nat → List Nat → Option (Nat × List Nat)
  | 0, bs => some (0, bs)
  | _ + 1, [] => none
  | k + 1, b :: bs => (rdLE k bs).map fun (n, r) => (b + 256 * n, r)

/-- A register-direct ModRM byte (`mod = 11`): `(reg, r/m)`. -/
def regModRM : List Nat → Option ((Nat × Nat) × List Nat)
  | m :: r => if modOf m = 3 then some ((regOf m, rmOf m), r) else none
  | [] => none

/-- A memory ModRM operand: `[rsp + disp32]` (`mod = 10`, `r/m = 100`, SIB
`24`: base `rsp`, no index) or `[rbp + disp32]` (`mod = 10`, `r/m = 101`).
Returns `reg`, the base and the displacement. -/
def memModRM : List Nat → Option ((Nat × Base × Nat) × List Nat)
  | m :: r =>
    if modOf m = 2 ∧ rmOf m = 4 then
      match r with
      | 0x24 :: r => (rdLE 4 r).map fun (d, r) => ((regOf m, .rsp, d), r)
      | _ => none
    else if modOf m = 2 ∧ rmOf m = 5 then
      (rdLE 4 r).map fun (d, r) => ((regOf m, .rbp, d), r)
    else none
  | [] => none

/-- After `REX.W`. -/
def decodeW : List Nat → Option (Inst × List Nat)
  | o :: r =>
    if o / 8 = 0xb8 / 8 then (rdLE 8 r).map fun (n, r) => (.movabs (o % 8) n, r)
    else if o = 0x99 then some (.cqo, r)
    else if o = 0x8b then (memModRM r).map fun ((g, b, d), r) => (.load g b d, r)
    else if o = 0x0f then
      match r with
      | 0xaf :: r => (regModRM r).map fun ((g, m), r) => (.imul g m, r)
      | 0xba :: r =>
        match regModRM r with
        | some ((7, m), b :: r) => some (.btc m b, r)
        | _ => none
      | _ => none
    else if o = 0xf7 then
      match regModRM r with
      | some ((d, m), r) => (g3Of d).map fun op => (.grp3 op m, r)
      | none => none
    else if o = 0x83 then
      match regModRM r with
      | some ((d, m), b :: r) => (g1Of d).map fun op => (.grpI8 op m b, r)
      | _ => none
    else if o = 0x81 then
      match regModRM r with
      | some ((d, m), r) =>
        match g1Of d, rdLE 4 r with
        | some op, some (n, r) => some (.grpI32 op m n, r)
        | _, _ => none
      | none => none
    else
      match r with
      | m :: t =>
        if o = 0x89 ∧ modOf m = 2 then
          (memModRM (m :: t)).map fun ((g, b, d), r) => (.store g b d, r)
        else
          match aluOf o, regModRM (m :: t) with
          | some op, some ((g, m), r) => some (.alu op m g, r)
          | _, _ => none
      | [] => none
  | [] => none

/-- After `0F` (no prefix). -/
def decode0F : List Nat → Option (Inst × List Nat)
  | o :: r =>
    if o / 16 = 8 then (rdLE 4 r).map fun (n, r) => (.jcc (o % 16) n, r)
    else if o / 16 = 9 then
      match regModRM r with
      | some ((0, m), r) => some (.setcc (o % 16) m, r)
      | _ => none
    else if o = 0xb6 then (regModRM r).map fun ((g, m), r) => (.movzxb g m, r)
    else none
  | [] => none

def decode : List Nat → Option (Inst × List Nat)
  | b :: r =>
    if b = 0x48 then decodeW r
    else if b = 0x66 then
      match r with
      | 0x48 :: 0x0f :: 0x6e :: r => (regModRM r).map fun ((x, g), r) => (.movqToX x g, r)
      | 0x48 :: 0x0f :: 0x7e :: r => (regModRM r).map fun ((x, g), r) => (.movqFromX g x, r)
      | 0x0f :: 0x2e :: r => (regModRM r).map fun ((a, c), r) => (.ucomisd a c, r)
      | _ => none
    else if b = 0xf2 then
      match r with
      | 0x0f :: o :: r =>
        match sseOf o, regModRM r with
        | some op, some ((d, s), r) => some (.sse op d s, r)
        | _, _ => none
      | _ => none
    else if b = 0x0f then decode0F r
    else if b = 0x31 then (regModRM r).map fun ((s, d), r) => (.xor32 d s, r)
    else if b = 0x85 then
      match memModRM r with
      | some ((g, .rsp, d), r) => some (.testM g d, r)
      | _ => none
    else if b / 8 = 0xb8 / 8 then (rdLE 4 r).map fun (n, r) => (.movImm32 (b % 8) n, r)
    else if b / 8 = 0x50 / 8 then some (.push (b % 8), r)
    else if b / 8 = 0x58 / 8 then some (.pop (b % 8), r)
    else if b / 16 = 7 then
      match r with
      | d :: r => some (.jcc8 (b % 16) d, r)
      | [] => none
    else if b = 0xeb then
      match r with
      | d :: r => some (.jmp8 d, r)
      | [] => none
    else if b = 0xe9 then (rdLE 4 r).map fun (n, r) => (.jmp n, r)
    else if b = 0xe8 then (rdLE 4 r).map fun (n, r) => (.call n, r)
    else if b = 0xc3 then some (.ret, r)
    else none
  | [] => none

/-! ## The encoder (as `x86_64.rs` writes it) -/

/-- A register-direct ModRM byte. -/
def rr (reg rm : Nat) : Nat := 0xc0 + reg * 8 + rm

/-- A memory operand: ModRM, SIB for `rsp`, then `disp32`. -/
def memOp (reg : Nat) : Base → Nat → List Nat
  | .rsp, d => [0x84 + reg * 8, 0x24] ++ le 4 d
  | .rbp, d => [0x85 + reg * 8] ++ le 4 d

def encode : Inst → List Nat
  | .load r b d => [0x48, 0x8b] ++ memOp r b d
  | .store r b d => [0x48, 0x89] ++ memOp r b d
  | .movabs r n => [0x48, 0xb8 + r] ++ le 8 n
  | .alu op d s => [0x48, op.code, rr s d]
  | .imul d s => [0x48, 0x0f, 0xaf, rr d s]
  | .grp3 op r => [0x48, 0xf7, rr op.digit r]
  | .cqo => [0x48, 0x99]
  | .xor32 d s => [0x31, rr s d]
  | .grpI8 op r n => [0x48, 0x83, rr op.digit r, n]
  | .grpI32 op r n => [0x48, 0x81, rr op.digit r] ++ le 4 n
  | .jcc cc n => [0x0f, 0x80 + cc] ++ le 4 n
  | .jcc8 cc n => [0x70 + cc, n]
  | .jmp8 n => [0xeb, n]
  | .jmp n => [0xe9] ++ le 4 n
  | .call n => [0xe8] ++ le 4 n
  | .setcc cc r => [0x0f, 0x90 + cc, rr 0 r]
  | .movzxb d s => [0x0f, 0xb6, rr d s]
  | .movqToX x r => [0x66, 0x48, 0x0f, 0x6e, rr x r]
  | .movqFromX r x => [0x66, 0x48, 0x0f, 0x7e, rr x r]
  | .sse op d s => [0xf2, 0x0f, op.code, rr d s]
  | .ucomisd a b => [0x66, 0x0f, 0x2e, rr a b]
  | .btc r n => [0x48, 0x0f, 0xba, rr 7 r, n]
  | .movImm32 r n => [0xb8 + r] ++ le 4 n
  | .testM r d => [0x85] ++ memOp r .rsp d
  | .push r => [0x50 + r]
  | .pop r => [0x58 + r]
  | .ret => [0xc3]

/-- Every register below 8 and every field in its range. -/
def Inst.Wf : Inst → Prop
  | .load r _ d | .store r _ d | .testM r d => r < 8 ∧ d < 2 ^ 32
  | .movabs r n => r < 8 ∧ n < 2 ^ 64
  | .alu _ d s | .imul d s | .xor32 d s | .movzxb d s | .sse _ d s | .ucomisd d s
  | .movqToX d s | .movqFromX d s => d < 8 ∧ s < 8
  | .grp3 _ r | .push r | .pop r => r < 8
  | .grpI8 _ r n | .btc r n => r < 8 ∧ n < 256
  | .grpI32 _ r n | .movImm32 r n => r < 8 ∧ n < 2 ^ 32
  | .jcc cc n => cc < 16 ∧ n < 2 ^ 32
  | .jcc8 cc n => cc < 16 ∧ n < 256
  | .setcc cc r => cc < 16 ∧ r < 8
  | .jmp8 n => n < 256
  | .jmp n | .call n => n < 2 ^ 32
  | .cqo | .ret => True

instance (i : Inst) : Decidable i.Wf := by
  cases i <;> simp only [Inst.Wf] <;> infer_instance

/-! ## Round trip -/

theorem le_lt (k n : Nat) : ∀ b ∈ le k n, b < 256 := by
  induction k generalizing n with
  | zero => simp [le]
  | succ k ih =>
    intro b hb
    simp only [le, List.mem_cons] at hb
    rcases hb with rfl | hb
    · omega
    · exact ih _ b hb

theorem rdLE_le (k n : Nat) (rest : List Nat) (h : n < 2 ^ (8 * k)) :
    rdLE k (le k n ++ rest) = some (n, rest) := by
  induction k generalizing n with
  | zero => simp [le, rdLE]; omega
  | succ k ih =>
    have : n / 256 < 2 ^ (8 * k) := by
      rw [Nat.div_lt_iff_lt_mul (by decide)]
      rw [show 8 * (k + 1) = 8 * k + 8 by omega, Nat.pow_add] at h; omega
    simp only [le, List.cons_append, rdLE, ih _ this, Option.map_some]
    congr; omega

theorem rr_fields (g m : Nat) (hg : g < 8) (hm : m < 8) :
    modOf (rr g m) = 3 ∧ regOf (rr g m) = g ∧ rmOf (rr g m) = m := by
  simp only [modOf, regOf, rmOf, rr]; omega

theorem regModRM_rr (g m : Nat) (rest : List Nat) (hg : g < 8) (hm : m < 8) :
    regModRM (rr g m :: rest) = some ((g, m), rest) := by
  obtain ⟨e1, e2, e3⟩ := rr_fields g m hg hm
  simp [regModRM, e1, e2, e3]

theorem memModRM_memOp (g : Nat) (b : Base) (d : Nat) (rest : List Nat) (hg : g < 8)
    (hd : d < 2 ^ 32) : memModRM (memOp g b d ++ rest) = some ((g, b, d), rest) := by
  have h4 : d < 2 ^ (8 * 4) := hd
  cases b
  · have e1 : modOf (0x84 + g * 8) = 2 := by simp only [modOf]; omega
    have e2 : rmOf (0x84 + g * 8) = 4 := by simp only [rmOf]; omega
    have e3 : regOf (0x84 + g * 8) = g := by simp only [regOf]; omega
    simp [memOp, memModRM, e1, e2, e3, rdLE_le 4 d rest h4]
  · have e1 : modOf (0x85 + g * 8) = 2 := by simp only [modOf]; omega
    have e2 : rmOf (0x85 + g * 8) = 5 := by simp only [rmOf]; omega
    have e3 : regOf (0x85 + g * 8) = g := by simp only [regOf]; omega
    simp [memOp, memModRM, e1, e2, e3, rdLE_le 4 d rest h4]

/-- The specification reads back every well-formed instruction, whatever
bytes follow it. -/
theorem decode_encode (i : Inst) (h : i.Wf) (rest : List Nat) :
    decode (encode i ++ rest) = some (i, rest) := by
  cases i with
  | load r b d =>
    obtain ⟨h1, h2⟩ := h
    simp [encode, decode, decodeW, memModRM_memOp r b d rest h1 h2]
  | store r b d =>
    obtain ⟨h1, h2⟩ := h
    have hm := memModRM_memOp r b d rest h1 h2
    cases b
    · have e1 : modOf (0x84 + r * 8) = 2 := by simp only [modOf]; omega
      simp only [memOp, List.cons_append, List.nil_append] at hm
      simp [encode, memOp, decode, decodeW, e1, hm]
    · have e1 : modOf (0x85 + r * 8) = 2 := by simp only [modOf]; omega
      simp only [memOp, List.cons_append, List.nil_append] at hm
      simp [encode, memOp, decode, decodeW, e1, hm]
  | movabs r n =>
    obtain ⟨h1, h2⟩ := h
    have e1 : (0xb8 + r) / 8 = 0xb8 / 8 := by omega
    have e2 : (0xb8 + r) % 8 = r := by omega
    simp [encode, decode, decodeW, e1, e2, rdLE_le 8 n rest h2]
  | alu op d s =>
    obtain ⟨h1, h2⟩ := h
    obtain ⟨e1, -, -⟩ := rr_fields s d h2 h1
    cases op <;> simp (disch := omega) [encode, decode, decodeW, Alu.code, aluOf, regModRM_rr s d rest h2 h1, e1]
  | imul d s =>
    obtain ⟨h1, h2⟩ := h
    simp [encode, decode, decodeW, regModRM_rr d s rest h1 h2]
  | grp3 op r =>
    have h1 : r < 8 := h
    cases op <;> simp (disch := omega) [encode, decode, decodeW, G3.digit, g3Of, regModRM_rr]
  | cqo => simp [encode, decode, decodeW]
  | xor32 d s =>
    obtain ⟨h1, h2⟩ := h
    simp [encode, decode, regModRM_rr s d rest h2 h1]
  | grpI8 op r n =>
    obtain ⟨h1, h2⟩ := h
    cases op <;> simp (disch := omega) [encode, decode, decodeW, G1.digit, g1Of,
      regModRM_rr]
  | grpI32 op r n =>
    obtain ⟨h1, h2⟩ := h
    have h4 : n < 2 ^ (8 * 4) := h2
    cases op <;> simp (disch := omega) [encode, decode, decodeW, G1.digit, g1Of,
      regModRM_rr, rdLE_le 4 n rest h4]
  | jcc cc n =>
    obtain ⟨h1, h2⟩ := h
    have h4 : n < 2 ^ (8 * 4) := h2
    have e1 : (0x80 + cc) / 16 = 8 := by omega
    have e2 : (0x80 + cc) % 16 = cc := by omega
    simp [encode, decode, decode0F, e1, e2, rdLE_le 4 n rest h4]
  | jcc8 cc n =>
    obtain ⟨h1, h2⟩ := h
    have e1 : (0x70 + cc) / 16 = 7 := by omega
    have e2 : (0x70 + cc) % 16 = cc := by omega
    have n1 : 0x70 + cc ≠ 0x48 := by omega
    have n2 : 0x70 + cc ≠ 0x66 := by omega
    have n3 : 0x70 + cc ≠ 0xf2 := by omega
    have n4 : 0x70 + cc ≠ 0x0f := by omega
    have n5 : 0x70 + cc ≠ 0x31 := by omega
    have n6 : 0x70 + cc ≠ 0x85 := by omega
    have n7 : (0x70 + cc) / 8 ≠ 0xb8 / 8 := by omega
    have n8 : (0x70 + cc) / 8 ≠ 0x50 / 8 := by omega
    have n9 : (0x70 + cc) / 8 ≠ 0x58 / 8 := by omega
    simp [encode, decode, e1, e2, n1, n2, n3, n4, n5, n6, n7, n8, n9]
  | jmp8 n => simp [encode, decode]
  | jmp n =>
    have h4 : n < 2 ^ (8 * 4) := h
    simp [encode, decode, rdLE_le 4 n rest h4]
  | call n =>
    have h4 : n < 2 ^ (8 * 4) := h
    simp [encode, decode, rdLE_le 4 n rest h4]
  | setcc cc r =>
    obtain ⟨h1, h2⟩ := h
    have e1 : (0x90 + cc) / 16 = 9 := by omega
    have e2 : (0x90 + cc) % 16 = cc := by omega
    simp [encode, decode, decode0F, e1, e2, regModRM_rr 0 r rest (by decide) h2]
  | movzxb d s =>
    obtain ⟨h1, h2⟩ := h
    simp [encode, decode, decode0F, regModRM_rr d s rest h1 h2]
  | movqToX x r =>
    obtain ⟨h1, h2⟩ := h
    simp [encode, decode, regModRM_rr x r rest h1 h2]
  | movqFromX r x =>
    obtain ⟨h1, h2⟩ := h
    simp [encode, decode, regModRM_rr x r rest h2 h1]
  | sse op d s =>
    obtain ⟨h1, h2⟩ := h
    cases op <;> simp (disch := omega) [encode, decode, Sse.code, sseOf, regModRM_rr d s rest h1 h2]
  | ucomisd a b =>
    obtain ⟨h1, h2⟩ := h
    simp [encode, decode, regModRM_rr a b rest h1 h2]
  | btc r n =>
    obtain ⟨h1, h2⟩ := h
    simp [encode, decode, decodeW, regModRM_rr 7 r (n :: rest) (by decide) h1]
  | movImm32 r n =>
    obtain ⟨h1, h2⟩ := h
    have h4 : n < 2 ^ (8 * 4) := h2
    have e1 : (0xb8 + r) / 8 = 0xb8 / 8 := by omega
    have e2 : (0xb8 + r) % 8 = r := by omega
    have n1 : 0xb8 + r ≠ 0x48 := by omega
    have n2 : 0xb8 + r ≠ 0x66 := by omega
    have n3 : 0xb8 + r ≠ 0xf2 := by omega
    have n4 : 0xb8 + r ≠ 0x0f := by omega
    have n5 : 0xb8 + r ≠ 0x31 := by omega
    have n6 : 0xb8 + r ≠ 0x85 := by omega
    simp [encode, decode, e1, e2, n1, n2, n3, n4, n5, n6, rdLE_le 4 n rest h4]
  | testM r d =>
    obtain ⟨h1, h2⟩ := h
    have hm := memModRM_memOp r .rsp d rest h1 h2
    simp only [memOp, List.cons_append, List.nil_append] at hm
    simp [encode, memOp, decode, hm]
  | push r =>
    have h1 : r < 8 := h
    have e1 : (0x50 + r) / 8 = 0x50 / 8 := by omega
    have e2 : (0x50 + r) % 8 = r := by omega
    have n1 : 0x50 + r ≠ 0x48 := by omega
    have n2 : 0x50 + r ≠ 0x66 := by omega
    have n3 : 0x50 + r ≠ 0xf2 := by omega
    have n4 : 0x50 + r ≠ 0x0f := by omega
    have n5 : 0x50 + r ≠ 0x31 := by omega
    have n6 : 0x50 + r ≠ 0x85 := by omega
    simp [encode, decode, e1, e2, n1, n2, n3, n4, n5, n6]
  | pop r =>
    have h1 : r < 8 := h
    have e1 : (0x58 + r) / 8 = 0x58 / 8 := by omega
    have e2 : (0x58 + r) % 8 = r := by omega
    have n1 : 0x58 + r ≠ 0x48 := by omega
    have n2 : 0x58 + r ≠ 0x66 := by omega
    have n3 : 0x58 + r ≠ 0xf2 := by omega
    have n4 : 0x58 + r ≠ 0x0f := by omega
    have n5 : 0x58 + r ≠ 0x31 := by omega
    have n6 : 0x58 + r ≠ 0x85 := by omega
    simp [encode, decode, e1, e2, n1, n2, n3, n4, n5, n6]
  | ret => simp [encode, decode]

/-- Every encoded byte is a byte. -/
theorem encode_bytes (i : Inst) (h : i.Wf) : ∀ b ∈ encode i, b < 256 := by
  have hl : ∀ k n, ∀ b ∈ le k n, b < 256 := le_lt
  cases i <;> simp only [Inst.Wf] at h <;> (try cases ‹Base›) <;> (try cases ‹Alu›) <;>
    (try cases ‹G3›) <;> (try cases ‹G1›) <;> (try cases ‹Sse›) <;>
    simp only [encode, memOp, rr, Alu.code, G3.digit, G1.digit, Sse.code, List.cons_append,
      List.nil_append, List.forall_mem_cons, List.not_mem_nil,
      false_implies, and_true, implies_true] <;>
    (repeat' apply And.intro) <;> (first | omega | exact hl _ _)

theorem encode_ne_nil (i : Inst) : encode i ≠ [] := by
  cases i <;> simp [encode, memOp] <;> (try cases ‹Base›) <;> simp [memOp]

/-- Decode a byte stream into instructions (at most `fuel` of them), with
nothing left over. -/
def decodeAll : Nat → List Nat → Option (List Inst)
  | _, [] => some []
  | 0, _ :: _ => none
  | f + 1, bs@(_ :: _) =>
    match decode bs with
    | some (i, r) => (decodeAll f r).map (i :: ·)
    | none => none

theorem decodeAll_encodeAll (is : List Inst) (h : ∀ i ∈ is, i.Wf) (f : Nat)
    (hf : is.length ≤ f) : decodeAll f (is.flatMap encode) = some is := by
  induction is generalizing f with
  | nil => cases f <;> simp [decodeAll]
  | cons i is ih =>
    obtain ⟨f, rfl⟩ : ∃ g, f = g + 1 := ⟨f - 1, by simp at hf; omega⟩
    have hi := decode_encode i (h i (by simp)) (is.flatMap encode)
    have tail := ih (fun j hj => h j (by simp [hj])) f (by simp at hf; omega)
    obtain ⟨b, bs, hb⟩ : ∃ b bs, encode i = b :: bs := by
      cases e : encode i with
      | nil => exact absurd e (encode_ne_nil i)
      | cons b bs => exact ⟨b, bs, rfl⟩
    rw [hb] at hi
    simp only [List.flatMap_cons, hb, List.cons_append, decodeAll]
    simp only [List.cons_append] at hi
    rw [hi]; simp [tail]

/-! ## `x86_64::emit` -/

/-- `rax`, `rcx`, `rdx`; `rsp`, `rbp`. -/
def regX : Reg → Nat
  | .r0 => 0 | .r1 => 1 | .r2 => 2
def rsp : Nat := 4
def rbp : Nat := 5

/-- Condition codes (the low nibble of `Jcc`/`SETcc`). -/
def JO : Nat := 0x0
def JB : Nat := 0x2
def JAE : Nat := 0x3
def JE : Nat := 0x4
def JNE : Nat := 0x5
def JBE : Nat := 0x6
def JA : Nat := 0x7
def JP : Nat := 0xa
def JL : Nat := 0xc
def JGE : Nat := 0xd
def JLE : Nat := 0xe
def JG : Nat := 0xf

/-- `FIRST_FREE_LABEL`: labels below it are the abort exits of
`TrapKind::ALL` (`tests/verified_codegen_lean.rs` checks the value against the
Rust constant); every label from it on is a function or block label. -/
def firstFree : Nat := 3

/-- The program-wide abort exits (`TrapKind::label`). -/
def trapLabel : Failure → Nat
  | .overflow => 0 | .divZero => 1 | .unreachable => 2 | .fault => 3
def ovf : Nat := trapLabel .overflow

/-- The branch instructions that take a label (all `rel32`). -/
inductive Br | jcc (cc : Nat) | jmp | call

def Br.size : Br → Nat
  | .jcc _ => 6 | .jmp | .call => 5
def Br.inst : Br → Nat → Inst
  | .jcc cc, n => .jcc cc n
  | .jmp, n => .jmp n
  | .call, n => .call n

/-- An emitted instruction, or a branch to a label resolved by layout. -/
inductive Item
  | ins (i : Inst)
  | br (k : Br) (l : Nat)

def mem (load : Bool) (r : Nat) : Loc → Item
  | .slot n | .out n => .ins (if load then .load r .rsp (n * 8) else .store r .rsp (n * 8))
  | .inn n => .ins (if load then .load r .rbp (16 + n * 8) else .store r .rbp (16 + n * 8))

/-- `movq xmm0, rax; movq xmm1, rcx`. -/
def toXmm : List Item := [.ins (.movqToX 0 0), .ins (.movqToX 1 1)]

def sseOfOp : FloatOp → Sse
  | .add => .add | .sub => .sub | .mul => .mul | .div => .div

def intCC : Cond → Sign → Nat
  | .eq, _ => 0x4 | .ne, _ => 0x5
  | .lt, .s => 0xc | .le, .s => 0xe | .gt, .s => 0xf | .ge, .s => 0xd
  | .lt, .u => 0x2 | .le, .u => 0x6 | .gt, .u => 0x7 | .ge, .u => 0x3

/-- `test [rsp - 4096k], esp` for each page of a Windows frame. -/
def probes (bytes : Nat) : List Item :=
  (List.range (bytes / 4096)).map fun k => .ins (.testM rsp (2 ^ 32 - (k + 1) * 4096))

/-- `push rbp; mov rbp, rsp; [probes;] sub rsp, bytes`. -/
def prologue (win : Bool) (bytes : Nat) : List Item :=
  [.ins (.push rbp), .ins (.alu .mov rbp rsp)] ++ (if win then probes bytes else []) ++
    [.ins (.grpI32 .sub rsp bytes)]

/-- `x86_64::emit`: `win` is `Asm::windows`, `frame` the frame bytes of the
function (`Asm::frame`, set by its `enter`). -/
def emit (win : Bool) (frame : Nat) : M → List Item
  | .ld r l => [mem true (regX r) l]
  | .st l r => [mem false (regX r) l]
  | .imm r w => [.ins (.movabs (regX r) (w % 2 ^ 64).toNat)]
  | .addchk sg => [.ins (.alu .add 0 1), .br (.jcc (if sg = .s then JO else JB)) ovf]
  | .subchk sg => [.ins (.alu .sub 0 1), .br (.jcc (if sg = .s then JO else JB)) ovf]
  | .mulchk .s => [.ins (.imul 0 1), .br (.jcc JO) ovf]
  | .mulchk .u => [.ins (.grp3 .mul 1), .br (.jcc JO) ovf]
  | .wrap .add => [.ins (.alu .add 0 1)]
  | .wrap .sub => [.ins (.alu .sub 0 1)]
  | .wrap .mul => [.ins (.imul 0 1)]
  | .andr2 => [.ins (.alu .and 0 2)]
  | .trapgtu => [.ins (.alu .cmp 0 2), .br (.jcc JA) ovf]
  | .trapzero => [.ins (.alu .test 1 1), .br (.jcc JE) (trapLabel .divZero)]
  | .trapdivovf => [.ins (.movabs 2 (2 ^ 63)), .ins (.alu .cmp 0 2), .ins (.jcc8 JNE 10),
      .ins (.grpI8 .cmp 1 0xff), .br (.jcc JE) ovf]
  | .div .s => [.ins .cqo, .ins (.grp3 .idiv 1)]
  | .div .u => [.ins (.xor32 2 2), .ins (.grp3 .div 1)]
  | .rem .s => [.ins (.grpI8 .cmp 1 0xff), .ins (.jcc8 JNE 4), .ins (.xor32 0 0),
      .ins (.jmp8 8), .ins .cqo, .ins (.grp3 .idiv 1), .ins (.alu .mov 0 2)]
  | .rem .u => [.ins (.xor32 2 2), .ins (.grp3 .div 1), .ins (.alu .mov 0 2)]
  | .negchk .s => [.ins (.grp3 .neg 0), .br (.jcc JO) ovf]
  | .negchk .u => [.ins (.alu .test 0 0), .br (.jcc JNE) ovf]
  | .xorone => [.ins (.grpI8 .xor 0 1)]
  | .cmp c sg => [.ins (.alu .cmp 0 1), .ins (.setcc (intCC c sg) 0), .ins (.movzxb 0 0)]
  | .fop op => toXmm ++ [.ins (.sse (sseOfOp op) 0 1), .ins (.movqFromX 0 0)]
  | .fneg => [.ins (.btc 0 63)]
  | .fcmp .eq => toXmm ++ [.ins (.ucomisd 0 1), .ins (.movImm32 0 0), .ins (.jcc8 JP 3),
      .ins (.setcc JE 0)]
  | .fcmp .ne => toXmm ++ [.ins (.ucomisd 0 1), .ins (.movImm32 0 1), .ins (.jcc8 JP 3),
      .ins (.setcc JNE 0)]
  | .fcmp .gt => toXmm ++ [.ins (.ucomisd 0 1), .ins (.setcc JA 0), .ins (.movzxb 0 0)]
  | .fcmp .ge => toXmm ++ [.ins (.ucomisd 0 1), .ins (.setcc JAE 0), .ins (.movzxb 0 0)]
  | .fcmp .lt => toXmm ++ [.ins (.ucomisd 1 0), .ins (.setcc JA 0), .ins (.movzxb 0 0)]
  | .fcmp .le => toXmm ++ [.ins (.ucomisd 1 0), .ins (.setcc JAE 0), .ins (.movzxb 0 0)]
  | .label _ => []
  | .jmp l => [.br .jmp l]
  | .bz l => [.ins (.alu .test 0 0), .br (.jcc JE) l]
  | .call l => [.br .call l]
  | .enter words => prologue win (words * 8)
  | .leave => if win then [.ins (.grpI32 .add rsp frame), .ins (.pop rbp), .ins .ret]
      else [.ins (.alu .mov rsp rbp), .ins (.pop rbp), .ins .ret]
  | .trap f => [.br .jmp (trapLabel f)]

/-! ## Layout: abort exits at offset 0, other labels at the end -/

def Item.size : Item → Nat
  | .ins i => (encode i).length
  | .br k _ => k.size

/-- A `rel32` field: the displacement modulo `2^32`. -/
def rel32 (x : Int) : Nat := (x % 2 ^ 32).toNat

/-- The instruction an item lays out to at byte offset `p` in a block of `n`
bytes. -/
def Item.at (p n : Nat) : Item → Inst
  | .ins i => i
  | .br k l => k.inst (rel32 ((if l < firstFree then 0 else (n : Int)) - ((p + k.size : Nat) : Int)))

def place (n : Nat) : Nat → List Item → List Inst
  | _, [] => []
  | p, it :: rest => it.at p n :: place n (p + it.size) rest

def size (items : List Item) : Nat := (items.map Item.size).sum

def layout (items : List Item) : List Inst := place (size items) 0 items

def Br.Ok : Br → Prop
  | .jcc cc => cc < 16
  | .jmp | .call => True

def Item.Ok : Item → Prop
  | .ins i => i.Wf
  | .br k _ => k.Ok

instance (k : Br) : Decidable k.Ok := by
  cases k <;> simp only [Br.Ok] <;> infer_instance
instance (it : Item) : Decidable it.Ok := by
  cases it <;> simp only [Item.Ok] <;> infer_instance

theorem rel32_lt (x : Int) : rel32 x < 2 ^ 32 := by
  simp only [rel32]; omega

/-- Layout keeps every instruction well formed (a `rel32` always fits its
field; the Rust assembler refuses displacements outside `i32`). -/
theorem place_wf (n : Nat) :
    ∀ (items : List Item) (p : Nat), (∀ it ∈ items, it.Ok) → ∀ i ∈ place n p items, i.Wf := by
  intro items
  induction items with
  | nil => intro _ _ i hi; cases hi
  | cons it rest ih =>
    intro p hok i hi
    simp only [place, List.mem_cons] at hi
    rcases hi with rfl | hi
    · have := hok it (by simp)
      cases it with
      | ins j => exact this
      | br bk l =>
        cases bk <;> simp only [Item.at, Br.inst, Inst.Wf, Item.Ok, Br.Ok] at this ⊢ <;>
          (try exact ⟨this, rel32_lt _⟩) <;> exact rel32_lt _
    · exact ih _ (fun it h => hok it (by simp [h])) i hi

/-- `M` operands the Rust emitter can be given: frames hold at most
`MAX_FRAME_WORDS` (4000) words, so every frame displacement and frame size
fits its 32-bit field. -/
def mBounded (frame : Nat) : M → Prop
  | .ld _ l | .st l _ => match l with | .slot n | .out n => n < 2 ^ 28 | .inn n => n < 2 ^ 28
  | .enter words => words < 2 ^ 28
  | .leave => frame < 2 ^ 32
  | _ => True

theorem mem_ok (load : Bool) (r : Nat) (l : Loc) (hr : r < 8)
    (hl : match l with | .slot n | .out n => n < 2 ^ 28 | .inn n => n < 2 ^ 28) :
    (mem load r l).Ok := by
  cases l <;> cases load <;> simp only [mem, Item.Ok, Inst.Wf, Bool.false_eq_true,
    ite_false, ite_true] at hl ⊢ <;> omega

theorem prologue_ok (win : Bool) (bytes : Nat) (hb : bytes < 2 ^ 32) :
    ∀ it ∈ prologue win bytes, it.Ok := by
  intro it hit
  simp only [prologue, List.mem_append, List.mem_cons, List.not_mem_nil, or_false] at hit
  rcases hit with ((rfl | rfl) | hit) | rfl
  · decide
  · decide
  · cases win
    · simp at hit
    · simp only [ite_true, probes, List.mem_map, List.mem_range] at hit
      obtain ⟨k, hk, rfl⟩ := hit
      simp only [Item.Ok, Inst.Wf, rsp]
      omega
  · simp only [Item.Ok, Inst.Wf, rsp]; omega

set_option linter.unusedSimpArgs false in
/-- Every item `emit` produces is well formed. -/
theorem emit_ok (win : Bool) (frame : Nat) (m : M) (hm : mBounded frame m) :
    ∀ it ∈ emit win frame m, it.Ok := by
  cases m with
  | ld r l =>
    intro it hit; simp only [emit, List.mem_cons, List.not_mem_nil, or_false] at hit
    subst hit; exact mem_ok true (regX r) l (by cases r <;> decide) hm
  | st l r =>
    intro it hit; simp only [emit, List.mem_cons, List.not_mem_nil, or_false] at hit
    subst hit; exact mem_ok false (regX r) l (by cases r <;> decide) hm
  | imm r w =>
    intro it hit; simp only [emit, List.mem_cons, List.not_mem_nil, or_false] at hit
    subst hit; simp only [Item.Ok, Inst.Wf]
    exact ⟨by cases r <;> decide, by omega⟩
  | enter words =>
    exact prologue_ok win _ (by simp only [mBounded] at hm; omega)
  | leave =>
    simp only [mBounded] at hm
    cases win <;> simp only [emit, Bool.false_eq_true, ite_false, ite_true] <;>
      intro it hit <;> simp only [List.mem_cons, List.not_mem_nil, or_false] at hit <;>
      rcases hit with rfl | rfl | rfl <;> simp only [Item.Ok, Inst.Wf, rsp, rbp] <;> omega
  | addchk sg | subchk sg | mulchk sg | div sg | rem sg | negchk sg =>
    cases sg <;> simp [emit, Item.Ok, Br.Ok, Inst.Wf, toXmm, JO, JB, JAE, JE, JNE, JBE, JA, JP, JL, JGE, JLE, JG, ovf, trapLabel, intCC, sseOfOp]
  | wrap op | fop op => cases op <;> simp [emit, Item.Ok, Br.Ok, Inst.Wf, toXmm, JO, JB, JAE, JE, JNE, JBE, JA, JP, JL, JGE, JLE, JG, ovf, trapLabel, intCC, sseOfOp]
  | cmp c sg => cases c <;> cases sg <;> simp [emit, Item.Ok, Br.Ok, Inst.Wf, toXmm, JO, JB, JAE, JE, JNE, JBE, JA, JP, JL, JGE, JLE, JG, ovf, trapLabel, intCC, sseOfOp]
  | fcmp c => cases c <;> simp [emit, Item.Ok, Br.Ok, Inst.Wf, toXmm, JO, JB, JAE, JE, JNE, JBE, JA, JP, JL, JGE, JLE, JG, ovf, trapLabel, intCC, sseOfOp]
  | trap f => cases f <;> simp [emit, Item.Ok, Br.Ok, Inst.Wf, toXmm, JO, JB, JAE, JE, JNE, JBE, JA, JP, JL, JGE, JLE, JG, ovf, trapLabel, intCC, sseOfOp]
  | _ => simp [emit, Item.Ok, Br.Ok, Inst.Wf, toXmm, JO, JB, JAE, JE, JNE, JBE, JA, JP, JL, JGE, JLE, JG, ovf, trapLabel, intCC, sseOfOp]

/-- The theorem the listing relies on: the laid-out bytes of `emit m` are
bytes, and the specification decodes them back to exactly the laid-out
instruction sequence (so instruction boundaries are recovered as well). -/
theorem emit_decodes (win : Bool) (frame : Nat) (m : M) (hm : mBounded frame m) :
    let is := layout (emit win frame m)
    (∀ b ∈ is.flatMap encode, b < 256) ∧
      decodeAll (is.flatMap encode).length (is.flatMap encode) = some is := by
  intro is
  have wf : ∀ i ∈ is, i.Wf :=
    place_wf _ (emit win frame m) 0 (emit_ok win frame m hm)
  refine ⟨?_, decodeAll_encodeAll is wf _ ?_⟩
  · intro b hb
    simp only [List.mem_flatMap] at hb
    obtain ⟨i, hi, hb⟩ := hb
    exact encode_bytes i (wf i hi) b hb
  · clear wf
    induction is with
    | nil => simp
    | cons i is ih =>
      have := encode_ne_nil i
      have : 0 < (encode i).length := List.length_pos_iff.mpr this
      simp only [List.length_cons, List.flatMap_cons, List.length_append]
      omega

/-! ## The printed listing (compared with `x86_64::encoding_listing()`) -/

def hex2 (b : Nat) : String :=
  let d := Nat.toDigits 16 b
  String.ofList (List.replicate (2 - d.length) '0' ++ d)

def samples : List M :=
  [.ld .r0 (.slot 1), .ld .r1 (.slot 300), .st (.out 2) .r0, .ld .r2 (.inn 3),
   .st (.inn 400) .r1, .imm .r0 0x0123456789abcdef, .imm .r2 18446744073709551615] ++
  ([Sign.s, .u].flatMap fun sg => [.addchk sg, .subchk sg, .mulchk sg]) ++
  [.wrap .add, .wrap .sub, .wrap .mul, .andr2, .trapgtu, .trapzero, .trapdivovf,
   .div .s, .div .u, .rem .s, .rem .u, .negchk .s, .negchk .u, .xorone] ++
  ([Cond.eq, .ne, .lt, .le, .gt, .ge].flatMap fun c => [.cmp c .s, .cmp c .u]) ++
  [.fop .add, .fop .sub, .fop .mul, .fop .div, .fneg] ++
  ([Cond.eq, .ne, .lt, .le, .gt, .ge].map .fcmp) ++
  [.jmp 9, .jmp firstFree, .bz 9, .call 9, .enter 4, .enter 300, .leave,
   .trap .overflow, .trap .divZero, .trap .unreachable]

/-- Windows samples: `(instruction, frame bytes, suffix)`. -/
def windowsSamples : List (M × Nat) :=
  [(.enter 4, 0), (.enter 1000, 0), (.enter 2048, 0), (.leave, 8000)]

def line (win : Bool) (frame : Nat) (m : M) (tag : String) : String :=
  m.render ++ tag ++ ":" ++
    String.join (((layout (emit win frame m)).flatMap encode).map fun b => " " ++ hex2 b) ++ "\n"

def listing : String :=
  String.join (samples.map fun m => line false 0 m "") ++
  String.join (windowsSamples.map fun (m, frame) =>
    line true frame m (if frame = 0 then " (windows)" else s!" (windows, frame {frame})"))

#eval IO.print ("BEGIN X64\n" ++ listing ++ "END X64\n")

#print axioms Vesper.VerifiedCodegen.X64.le_lt
#print axioms Vesper.VerifiedCodegen.X64.rdLE_le
#print axioms Vesper.VerifiedCodegen.X64.rr_fields
#print axioms Vesper.VerifiedCodegen.X64.regModRM_rr
#print axioms Vesper.VerifiedCodegen.X64.memModRM_memOp
#print axioms Vesper.VerifiedCodegen.X64.decode_encode
#print axioms Vesper.VerifiedCodegen.X64.encode_bytes
#print axioms Vesper.VerifiedCodegen.X64.encode_ne_nil
#print axioms Vesper.VerifiedCodegen.X64.decodeAll_encodeAll
#print axioms Vesper.VerifiedCodegen.X64.rel32_lt
#print axioms Vesper.VerifiedCodegen.X64.place_wf
#print axioms Vesper.VerifiedCodegen.X64.mem_ok
#print axioms Vesper.VerifiedCodegen.X64.prologue_ok
#print axioms Vesper.VerifiedCodegen.X64.emit_ok
#print axioms Vesper.VerifiedCodegen.X64.emit_decodes

end Vesper.VerifiedCodegen.X64
