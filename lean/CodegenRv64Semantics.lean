import CodegenRv64Encoding

/-!
# RV64 semantics of the emitted sequences

`CodegenRv64Encoding.lean` proves that the RV64 encoder writes the words the
ISA's formats assign to each `Rv64.Inst`. This file gives those instructions a
meaning and proves that the sequence `Rv64.emit` lays out for every data
instruction of the abstract machine implements `Mach.step` of
`VerifiedCodegen.lean`, which until now rested on QEMU runs only.

The model is a small RV64IM core with the D-extension moves and operations
the emitter uses:

* `St`: 32 integer registers (`x0` reads as zero), 32 FP registers holding
  bit patterns, memory as a map from byte addresses to doublewords, and a
  pc relative to the code's load address `base`.
* `exec`: one instruction. Results are taken modulo `2^64`; `ld`/`sd` need an
  8-aligned address; `div`/`rem` follow the ISA's divide-by-zero results;
  `ecall` has no meaning here. FP arithmetic and comparisons use Lean's
  IEEE-754 `Float`, as `Mach.step` does.
* `Steps`: the reflexive-transitive closure of fetch and execute.

`Corr fr k m s` relates an RV64 state to an abstract machine: `a0`/`a1` hold
`r0`/`r1` (and `a2` holds `r2` when the flag `k` says so), `sp` and `s0`
frame the slots, every valid frame word holds the abstract memory, and every
register and memory word is a 64-bit word. `RSim` says that the code reaches
its end in a related state when `Mach.step` succeeds, and reaches the
failure's trap label (outside the code, below it) when it aborts. The
`.fault` abort of a zero divisor, which `trapzero` rules out before every
divide, carries no obligation.

* `emit_sim`: every data instruction (one lemma per instruction, `sim_*`).
* `run_sim`: straight-line sequences, composed with `steps_seq`.
* `row_sim`: every data row of the selection table, joined to
  `select_simulates`.
* `sim_bz`, `sim_jmp`: the branch and the jump reach their label.

Assumptions, stated as hypotheses: `FNegFlips` (`Float` negation flips the
sign bit, which `Float` cannot prove in the kernel), `Frame.Ok` (the frame
lies in the address space and is 8-aligned), `Fits` (loads and stores name
valid frame words, immediates are words). `call`, `enter` and `leave` are
not covered.
-/

namespace Vesper.VerifiedCodegen.Rv64.Sem

-- The `rv` tactics pass one lemma list to every step; not every step uses all of it.
set_option linter.unusedSimpArgs false
set_option linter.unnecessarySimpa false

/-! ## RV64 state -/

structure St where
  x : Nat → Int
  f : Nat → Int
  mem : Int → Int
  pc : Int

def rx (s : St) (r : Nat) : Int := if r = 0 then 0 else s.x r
def wx (s : St) (r : Nat) (v : Int) : St := { s with x := fun r' => if r' = r then v % W else s.x r' }
def wf (s : St) (r : Nat) (v : Int) : St := { s with f := fun r' => if r' = r then v % W else s.f r' }
def setPc (s : St) (p : Int) : St := { s with pc := p }
def next (s : St) : St := setPc s (s.pc + 4)
def wmem (s : St) (a v : Int) : St := { s with mem := fun a' => if a' = a then v else s.mem a' }

@[simp] theorem wx_pc (s : St) (r : Nat) (v : Int) : (wx s r v).pc = s.pc := rfl
@[simp] theorem wx_mem (s : St) (r : Nat) (v : Int) : (wx s r v).mem = s.mem := rfl
@[simp] theorem wx_f (s : St) (r : Nat) (v : Int) : (wx s r v).f = s.f := rfl
@[simp] theorem wf_pc (s : St) (r : Nat) (v : Int) : (wf s r v).pc = s.pc := rfl
@[simp] theorem wf_mem (s : St) (r : Nat) (v : Int) : (wf s r v).mem = s.mem := rfl
@[simp] theorem wf_f (s : St) (r r' : Nat) (v : Int) :
    (wf s r v).f r' = if r' = r then v % W else s.f r' := rfl
@[simp] theorem rx_wf (s : St) (r r' : Nat) (v : Int) : rx (wf s r v) r' = rx s r' := rfl
@[simp] theorem setPc_pc (s : St) (p : Int) : (setPc s p).pc = p := rfl
@[simp] theorem setPc_mem (s : St) (p : Int) : (setPc s p).mem = s.mem := rfl
@[simp] theorem setPc_f (s : St) (p : Int) : (setPc s p).f = s.f := rfl
@[simp] theorem rx_setPc (s : St) (p : Int) (r : Nat) : rx (setPc s p) r = rx s r := rfl
@[simp] theorem wmem_pc (s : St) (a v : Int) : (wmem s a v).pc = s.pc := rfl
@[simp] theorem wmem_f (s : St) (a v : Int) : (wmem s a v).f = s.f := rfl
@[simp] theorem wmem_mem (s : St) (a v a' : Int) :
    (wmem s a v).mem a' = if a' = a then v else s.mem a' := rfl
@[simp] theorem rx_wmem (s : St) (a v : Int) (r : Nat) : rx (wmem s a v) r = rx s r := rfl
@[simp] theorem rx_wx (s : St) (r r' : Nat) (v : Int) :
    rx (wx s r v) r' = if r' = 0 then 0 else if r' = r then v % W else rx s r' := by
  simp only [rx, wx]; split <;> rfl
@[simp] theorem next_pc (s : St) : (next s).pc = s.pc + 4 := rfl
@[simp] theorem next_mem (s : St) : (next s).mem = s.mem := rfl
@[simp] theorem next_f (s : St) : (next s).f = s.f := rfl
@[simp] theorem rx_next (s : St) (r : Nat) : rx (next s) r = rx s r := rfl
@[simp] theorem pc_add_sub (a b : Int) : a + (b - a) = b := by omega
@[simp] theorem rx_zero (s : St) : rx s 0 = 0 := rfl

def band (a b : Int) : Int := Int.ofNat (a.toNat &&& b.toNat)
def bor (a b : Int) : Int := Int.ofNat (a.toNat ||| b.toNat)
def bxor (a b : Int) : Int := Int.ofNat (a.toNat ^^^ b.toNat)

def rop (op : ROp) (a b : Int) : Int :=
  match op with
  | .add => a + b
  | .sub => a - b
  | .slt => if toS a < toS b then 1 else 0
  | .sltu => if a < b then 1 else 0
  | .and => band a b
  | .mul => a * b
  | .mulh => toS a * toS b / W
  | .mulhu => a * b / W
  | .div => if b = 0 then -1 else (toS a).tdiv (toS b)
  | .divu => if b = 0 then -1 else a / b
  | .rem => if b = 0 then a else (toS a).tmod (toS b)
  | .remu => if b = 0 then a else a % b

def iop (op : IOp) (a imm : Int) : Int :=
  match op with
  | .addi => a + imm
  | .sltiu => if a < enc imm then 1 else 0
  | .xori => bxor a (enc imm)
  | .ori => bor a (enc imm)

def bcond (op : BOp) (a b : Int) : Bool :=
  match op with
  | .beq => decide (a = b)
  | .bne => decide (a ≠ b)
  | .bltu => decide (a < b)
  | .bgeu => decide (b ≤ a)

def signFlip (x : Int) : Int := if x < H then x + H else x - H

def exec (base : Int) : Inst → St → Option St
  | .r op rd r1 r2, s => some (next (wx s rd (rop op (rx s r1) (rx s r2))))
  | .i op rd r1 imm, s => some (next (wx s rd (iop op (rx s r1) imm)))
  | .slli rd r1 sh, s => some (next (wx s rd (rx s r1 * 2 ^ sh)))
  | .srai rd r1 sh, s => some (next (wx s rd (toS (rx s r1) / 2 ^ sh)))
  | .ld rd r1 imm, s =>
    if enc (rx s r1 + imm) % 8 = 0 then some (next (wx s rd (s.mem (enc (rx s r1 + imm)))))
    else none
  | .sd r2 r1 imm, s =>
    if enc (rx s r1 + imm) % 8 = 0 then some (next (wmem s (enc (rx s r1 + imm)) (rx s r2)))
    else none
  | .branch op r1 r2 off, s =>
    some (setPc s (if bcond op (rx s r1) (rx s r2) then s.pc + off else s.pc + 4))
  | .jal rd off, s => some (setPc (wx s rd (base + s.pc + 4)) (s.pc + off))
  | .jalr rd r1 imm, s =>
    some (setPc (wx s rd (base + s.pc + 4))
      (enc (rx s r1 + imm) - enc (rx s r1 + imm) % 2 - base))
  | .ecall, _ => none
  | .fp .faddD rd r1 r2, s => some (next (wf s rd (fword (fbits (s.f r1) + fbits (s.f r2)))))
  | .fp .fsubD rd r1 r2, s => some (next (wf s rd (fword (fbits (s.f r1) - fbits (s.f r2)))))
  | .fp .fmulD rd r1 r2, s => some (next (wf s rd (fword (fbits (s.f r1) * fbits (s.f r2)))))
  | .fp .fdivD rd r1 r2, s => some (next (wf s rd (fword (fbits (s.f r1) / fbits (s.f r2)))))
  | .fp .fsgnjnD rd r1 r2, s =>
    some (next (wf s rd (s.f r1 % W % H + (if s.f r2 % W < H then H else 0))))
  | .fp .feqD rd r1 r2, s =>
    some (next (wx s rd (if fbits (s.f r1) == fbits (s.f r2) then 1 else 0)))
  | .fp .fltD rd r1 r2, s =>
    some (next (wx s rd (if decide (fbits (s.f r1) < fbits (s.f r2)) then 1 else 0)))
  | .fp .fleD rd r1 r2, s =>
    some (next (wx s rd (if decide (fbits (s.f r1) ≤ fbits (s.f r2)) then 1 else 0)))
  | .fmvDX rd r1, s => some (next (wf s rd (rx s r1)))
  | .fmvXD rd r1, s => some (next (wx s rd (s.f r1)))

def fetch (code : List Inst) (pc : Int) : Option Inst :=
  if 0 ≤ pc ∧ pc % 4 = 0 then code[(pc / 4).toNat]? else none

inductive Steps (base : Int) (code : List Inst) : St → St → Prop
  | refl (s : St) : Steps base code s s
  | step (s s' s'' : St) (i : Inst) : fetch code s.pc = some i → exec base i s = some s' →
      Steps base code s' s'' → Steps base code s s''

/-! ## Layout with label addresses -/

def atL (lab : Nat → Int) (k : Nat) : Item → Inst
  | .ins i => i
  | .jump rd l => .jal rd (lab l - 4 * k)

def placeL (lab : Nat → Int) : Nat → List Item → List Inst
  | _, [] => []
  | k, it :: rest => atL lab k it :: placeL lab (k + 1) rest

def code (lab : Nat → Int) (items : List Item) : List Inst := placeL lab 0 items

@[simp] theorem wx_x (s : St) (r r' : Nat) (v : Int) :
    (wx s r v).x r' = if r' = r then v % W else s.x r' := rfl
@[simp] theorem wf_x (s : St) (r : Nat) (v : Int) : (wf s r v).x = s.x := rfl
@[simp] theorem setPc_x (s : St) (p : Int) : (setPc s p).x = s.x := rfl
@[simp] theorem next_x (s : St) : (next s).x = s.x := rfl
@[simp] theorem wmem_x (s : St) (a v : Int) : (wmem s a v).x = s.x := rfl
@[simp] theorem setReg_reg (m : Mach) (r r' : Reg) (v : Int) :
    (setReg m r v).reg r' = if r' = r then v else m.reg r' := rfl
@[simp] theorem setReg_mem (m : Mach) (r : Reg) (v : Int) : (setReg m r v).mem = m.mem := rfl

theorem mod_word (v : Int) (h : 0 ≤ v ∧ v < 18446744073709551616) : v % W = v := by
  unfold W; omega

theorem bor_step (v c : Int) (hv : 0 ≤ v ∧ v < 9007199254740992) (hc : 0 ≤ c ∧ c < 2048) :
    bor (v * 2048) (enc c) % W = v * 2048 + c := by
  have h2 : enc c = c := by unfold enc W; omega
  rw [h2, bor]
  have e1 : (v * 2048).toNat = 2 ^ 11 * v.toNat := by omega
  have e2 := Nat.two_pow_add_eq_or_of_lt (i := 11) (b := c.toNat) (by omega) v.toNat
  rw [e1, ← e2, Int.ofNat_eq_natCast]
  unfold W; omega

/-! ## Steps lemmas -/

theorem Steps.trans {base : Int} {c : List Inst} {s1 s2 s3 : St} (h : Steps base c s1 s2)
    (h' : Steps base c s2 s3) : Steps base c s1 s3 := by
  induction h with
  | refl => exact h'
  | step _ _ _ i hf he _ ih => exact .step _ _ _ i hf he (ih h')

theorem fetch_append {c : List Inst} (d : List Inst) {pc : Int} {i : Inst} (h : fetch c pc = some i) :
    fetch (c ++ d) pc = some i := by
  unfold fetch at *
  split at h
  · rw [ite_eq_left (by assumption)]
    exact List.getElem?_append_left (List.getElem?_eq_some_iff.mp h).1 ▸ h
  · simp at h

theorem Steps.append {base : Int} {c : List Inst} (d : List Inst) {s s' : St}
    (h : Steps base c s s') : Steps base (c ++ d) s s' := by
  induction h with
  | refl => exact .refl _
  | step _ _ _ i hf he _ ih => exact .step _ _ _ i (fetch_append d hf) he ih

theorem fetch_shift (p : List Inst) {c : List Inst} {pc : Int} {i : Inst} (h : fetch c pc = some i) :
    fetch (p ++ c) (pc + 4 * p.length) = some i := by
  unfold fetch at *
  split at h
  · rename_i hc
    rw [ite_eq_left (by omega)]
    have : ((pc + 4 * p.length) / 4).toNat = p.length + (pc / 4).toNat := by omega
    rw [this, List.getElem?_append_right (by omega)]
    simpa using h
  · simp at h

theorem exec_shift (base o : Int) (i : Inst) (s s' : St) (h : exec (base + o) i s = some s') :
    exec base i (setPc s (s.pc + o)) = some (setPc s' (s'.pc + o)) := by
  obtain ⟨x, f, mem, pc⟩ := s
  cases i with
  | ld rd r1 imm | sd rd r1 imm =>
    simp only [exec, rx_setPc] at h ⊢
    split at h
    · rename_i ha; simp only [ha, ite_true, Option.some.injEq] at h ⊢; subst h
      simp only [next, setPc, wx, wmem, St.mk.injEq, true_and, and_true]; omega
    · simp at h
  | ecall => simp [exec] at h
  | fp op rd r1 r2 =>
    cases op <;> (simp only [exec, Option.some.injEq] at h ⊢; subst h; simp [next, setPc, wf, wx, rx]; first | omega | exact ⟨rfl, by omega⟩)
  | branch op r1 r2 off =>
    simp only [exec, Option.some.injEq] at h ⊢; subst h
    simp only [setPc, St.mk.injEq, true_and]
    have e : ∀ p, rx ⟨x, f, mem, p⟩ = rx ⟨x, f, mem, pc⟩ := fun _ => rfl
    simp only [e (pc + o)]; split <;> omega
  | jal rd off =>
    simp only [exec, Option.some.injEq] at h ⊢; subst h
    simp only [setPc, wx, St.mk.injEq]
    refine ⟨?_, trivial, trivial, by omega⟩
    funext r'; split <;> (try rfl); congr 1; omega
  | jalr rd r1 imm =>
    simp only [exec, Option.some.injEq] at h ⊢; subst h
    simp only [rx_setPc]; simp only [setPc, wx, St.mk.injEq]
    refine ⟨?_, trivial, trivial, by omega⟩
    funext r'; split <;> (try rfl); congr 1; omega
  | _ =>
    simp only [exec, Option.some.injEq] at h ⊢; subst h
    simp [next, setPc, wx, wf, rx]; omega

theorem Steps.shift {base : Int} (p : List Inst) {c : List Inst} {s s' : St}
    (h : Steps (base + 4 * p.length) c s s') :
    Steps base (p ++ c) (setPc s (s.pc + 4 * p.length)) (setPc s' (s'.pc + 4 * p.length)) := by
  induction h with
  | refl => exact .refl _
  | step s1 s2 _ i hf he _ ih =>
    exact .step _ _ _ i (by simpa using fetch_shift p hf) (exec_shift _ _ i s1 s2 he) ih

theorem setPc_self (s : St) : setPc s s.pc = s := rfl
theorem setPc_setPc (s : St) (p q : Int) : setPc (setPc s p) q = setPc s q := rfl

/-! ## Layout lemmas -/

theorem placeL_length (lab : Nat → Int) : ∀ (k : Nat) (xs : List Item),
    (placeL lab k xs).length = xs.length
  | _, [] => rfl
  | k, _ :: rest => by simp [placeL, placeL_length lab (k + 1) rest]

theorem placeL_append (lab : Nat → Int) : ∀ (k : Nat) (xs ys : List Item),
    placeL lab k (xs ++ ys) = placeL lab k xs ++ placeL lab (k + xs.length) ys
  | _, [], _ => rfl
  | k, x :: rest, ys => by
    simp only [List.cons_append, placeL, placeL_append lab (k + 1) rest ys, List.length_cons,
      List.cons_append, show k + 1 + rest.length = k + (rest.length + 1) by omega]

/-- Label addresses seen from `n` instructions further on. -/
def shiftLab (lab : Nat → Int) (n : Nat) : Nat → Int := fun l => lab l - 4 * n

theorem placeL_shift (lab : Nat → Int) (n : Nat) : ∀ (j : Nat) (ys : List Item),
    placeL lab (n + j) ys = placeL (shiftLab lab n) j ys
  | _, [] => rfl
  | j, y :: rest => by
    simp only [placeL]
    rw [show n + j + 1 = n + (j + 1) by omega, placeL_shift lab n (j + 1) rest]
    congr 1
    cases y <;> simp only [atL, shiftLab]
    congr 1; omega

theorem code_append (lab : Nat → Int) (xs ys : List Item) :
    code lab (xs ++ ys) = code lab xs ++ code (shiftLab lab xs.length) ys := by
  simp only [code, placeL_append, Nat.zero_add]
  have := placeL_shift lab xs.length 0 ys
  rw [Nat.add_zero] at this
  rw [this]

theorem code_length (lab : Nat → Int) (xs : List Item) : (code lab xs).length = xs.length :=
  placeL_length lab 0 xs

/-- Run the first part to its end, then the second part from its start. -/
theorem steps_seq {base : Int} {lab : Nat → Int} {xs ys : List Item} {s s1 s2 : St}
    (h1 : Steps base (code lab xs) s s1) (hp : s1.pc = 4 * xs.length)
    (h2 : Steps (base + 4 * xs.length) (code (shiftLab lab xs.length) ys) (setPc s1 0) s2) :
    Steps base (code lab (xs ++ ys)) s (setPc s2 (s2.pc + 4 * xs.length)) := by
  rw [code_append]
  have h2' := Steps.shift (code lab xs) (by rw [code_length]; exact h2)
  rw [code_length] at h2'
  have e : setPc s1 ((setPc s1 0).pc + 4 * (xs.length : Int)) = s1 := by
    rw [setPc_pc, Int.zero_add, ← hp]; rfl
  rw [setPc_setPc, e] at h2'
  exact (h1.append _).trans h2'

/-! ## The frame and the simulation relation -/

def Word (v : Int) : Prop := 0 ≤ v ∧ v < W

theorem word_mod (v : Int) : Word (v % W) := by unfold Word W; omega

structure Frame where
  sp : Int
  out : Nat
  words : Nat
  inWords : Nat

structure Frame.Ok (fr : Frame) : Prop where
  nonneg : 0 ≤ fr.sp
  aligned : fr.sp % 8 = 0
  out_le : fr.out ≤ fr.words
  fits : fr.sp + 8 * fr.words + 16 + 8 * fr.inWords ≤ W

def Frame.addr (fr : Frame) : Loc → Int
  | .slot n | .out n => fr.sp + 8 * n
  | .inn n => fr.sp + 8 * fr.words + 16 + 8 * n

def Frame.Valid (fr : Frame) : Loc → Prop
  | .slot n => fr.out ≤ n ∧ n < fr.words
  | .out n => n < fr.out
  | .inn n => n < fr.inWords

structure Corr (fr : Frame) (k : Bool) (m : Mach) (s : St) : Prop where
  words : ∀ r, Word (s.x r)
  mwords : ∀ a, Word (s.mem a)
  sp : rx s 2 = fr.sp
  s0 : rx s 8 = fr.sp + 8 * fr.words
  frame : ∀ l, fr.Valid l → s.mem (fr.addr l) = m.mem l
  r0 : rx s 10 = m.reg .r0
  r1 : rx s 11 = m.reg .r1
  r2 : k = true → rx s 12 = m.reg .r2

theorem Corr.setPc {fr : Frame} {k : Bool} {m : Mach} {s : St} (h : Corr fr k m s) (p : Int) :
    Corr fr k m (setPc s p) := ⟨h.words, h.mwords, h.sp, h.s0, h.frame, h.r0, h.r1, h.r2⟩

theorem Corr.word {fr : Frame} {k : Bool} {m : Mach} {s : St} (h : Corr fr k m s) (r : Nat) :
    Word (rx s r) := by
  unfold rx; split
  · unfold Word W; omega
  · exact h.words r

def Lands (base : Int) (c : List Inst) (s : St) (P : St → Prop) : Prop :=
  ∃ s', Steps base c s s' ∧ P s'

theorem lands_done {base : Int} {c : List Inst} {s : St} {P : St → Prop} (h : P s) :
    Lands base c s P := ⟨s, .refl _, h⟩

/-- Execute the instruction at index `k` (where the pc is). -/
theorem lands_step {base : Int} {c : List Inst} {s : St} {P : St → Prop} (k : Nat) {i : Inst}
    {s' : St} (hpc : s.pc = 4 * k) (hk : c[k]? = some i) (he : exec base i s = some s')
    (h : Lands base c s' P) : Lands base c s P := by
  obtain ⟨s'', hs, hp⟩ := h
  refine ⟨s'', .step _ _ _ i ?_ he hs, hp⟩
  unfold fetch; rw [ite_eq_left (by omega)]
  rw [show (s.pc / 4).toNat = k by omega]; exact hk

/-- The outcome of the RV64 code `c` for an outcome of the abstract machine. -/
def RSim (fr : Frame) (base : Int) (lab : Nat → Int) (k : Bool) (c : List Inst) (s : St) :
    Res Mach → Prop
  | .ok m' => Lands base c s fun s' => s'.pc = 4 * c.length ∧ Corr fr k m' s'
  | .abort f => f ≠ .fault → Lands base c s fun s' => s'.pc = lab (trapLabel f)

def flag : M → Bool → Bool
  | .imm .r2 _, _ | .ld .r2 _, _ | .mulchk _, _ | .trapdivovf, _ => true
  | .div _, _ | .rem _, _ => false
  | _, k => k

def needs2 : M → Bool
  | .andr2 | .trapgtu | .st _ .r2 => true
  | _ => false

def Fits (fr : Frame) : M → Prop
  | .ld _ l | .st l _ => fr.Valid l
  | .imm _ w => Word w
  | _ => True

def DataM : M → Prop
  | .label _ | .jmp _ | .bz _ | .call _ | .enter _ | .leave => False
  | _ => True

def FNegFlips : Prop := ∀ x, Word x → fword (-(fbits x)) = signFlip x

/-! ## Instruction proofs -/

theorem ite01_mod (p : Prop) [Decidable p] :
    (if p then (1 : Int) else 0) % 18446744073709551616 = if p then 1 else 0 := by
  split <;> decide

theorem ite01_eq (p q : Prop) [Decidable p] [Decidable q] :
    ((if p then (1 : Int) else 0) = if q then 1 else 0) ↔ (p ↔ q) := by
  by_cases hp : p <;> by_cases hq : q <;> simp [hp, hq]

theorem ite01_eq_one (p : Prop) [Decidable p] : ((if p then (1 : Int) else 0) = 1) ↔ p := by
  by_cases hp : p <;> simp [hp]

theorem ite01_eq_zero (p : Prop) [Decidable p] : ((if p then (1 : Int) else 0) = 0) ↔ ¬p := by
  by_cases hp : p <;> simp [hp]

theorem neg_eq_iff (a b : Int) :
    neg? a = neg? b ↔ (9223372036854775808 ≤ a ↔ 9223372036854775808 ≤ b) := by
  unfold neg? H
  by_cases h1 : 9223372036854775808 ≤ a <;> by_cases h2 : 9223372036854775808 ≤ b <;> simp [h1, h2]

theorem toS_word (x : Int) (h : 0 ≤ x ∧ x < 18446744073709551616) :
    toS x = x - 18446744073709551616 * (x / 9223372036854775808) := by
  unfold toS H W; split <;> omega

theorem toS_emod (a : Int) (_h : 0 ≤ a ∧ a < W) : toS a % W = a % W := by
  unfold toS; split
  · rfl
  · unfold W at *; omega

theorem mul_mod_toS (a b : Int) (ha : 0 ≤ a ∧ a < W) (hb : 0 ≤ b ∧ b < W) :
    a * b % W = toS a * toS b % W := by
  rw [Int.mul_emod a, Int.mul_emod (toS a), toS_emod a ha, toS_emod b hb]

theorem toS_bound (a : Int) (h : 0 ≤ a ∧ a < W) : (toS a).natAbs ≤ 9223372036854775808 := by
  unfold toS W H at *; split <;> omega

theorem mulS_bound (a b : Int) (ha : 0 ≤ a ∧ a < W) (hb : 0 ≤ b ∧ b < W) :
    -85070591730234615865843651857942052864 ≤ toS a * toS b ∧
      toS a * toS b ≤ 85070591730234615865843651857942052864 := by
  have h1 := toS_bound a ha
  have h2 := toS_bound b hb
  have h3 : (toS a * toS b).natAbs ≤ 85070591730234615865843651857942052864 := by
    rw [Int.natAbs_mul]
    exact Nat.le_trans (Nat.mul_le_mul h1 h2) (by decide)
  omega

theorem mulU_bound (a b : Int) (ha : 0 ≤ a ∧ a < W) (hb : 0 ≤ b ∧ b < W) :
    0 ≤ a * b ∧ a * b ≤ 340282366920938463426481119284349108225 := by
  unfold W at *
  refine ⟨Int.mul_nonneg ha.1 hb.1, ?_⟩
  have := Int.mul_le_mul (a := a) (b := b) (c := 18446744073709551615) (d := 18446744073709551615)
    (by omega) (by omega) hb.1 (by omega)
  omega

theorem xor_one (a : Nat) : a ^^^ 1 = if a % 2 = 0 then a + 1 else a - 1 := by
  apply Nat.eq_of_testBit_eq; intro i
  rw [Nat.testBit_xor]
  cases i with
  | zero =>
    simp only [Nat.testBit_zero]; split
    · have : (a + 1) % 2 = 1 := by omega
      simp [*]
    · have : (a - 1) % 2 = 0 := by omega
      have : a % 2 = 1 := by omega
      simp [*]
  | succ j =>
    simp only [Nat.testBit_succ]
    have h1 : (1 / 2 : Nat).testBit j = false := by simp
    rw [h1, Bool.xor_false]; congr 1; split <;> omega

theorem bxor_one (a : Int) (h : 0 ≤ a ∧ a < 18446744073709551616) :
    bxor a (enc 1) % 18446744073709551616 = if a % 2 = 0 then a + 1 else a - 1 := by
  have e : enc 1 = 1 := by decide
  have ht : (a.toNat : Int) = a := Int.toNat_of_nonneg h.1
  unfold bxor; rw [e]
  simp only [Int.toNat_one, xor_one, Int.ofNat_eq_natCast]
  split <;> split <;> omega

theorem band_mod (a c : Int) (h : 0 ≤ a ∧ a < 18446744073709551616) :
    band a c % 18446744073709551616 = andMask a c := by
  have ht : (a.toNat : Int) = a := Int.toNat_of_nonneg h.1
  have hle : a.toNat &&& c.toNat ≤ a.toNat := Nat.and_le_left
  have hm : band a c % 18446744073709551616 = band a c := by
    unfold band; simp only [Int.ofNat_eq_natCast]; omega
  rw [hm]; unfold band andMask
  simp only [Int.ofNat_eq_natCast]
  split
  · subst c
    have := Nat.and_two_pow_sub_one_eq_mod a.toNat 8
    rw [show (255 : Int).toNat = 2 ^ 8 - 1 by decide, this]; omega
  · split
    · subst c
      have := Nat.and_two_pow_sub_one_eq_mod a.toNat 32
      rw [show (4294967295 : Int).toNat = 2 ^ 32 - 1 by decide, this]; omega
    · rfl

theorem ite_bool_not (b : Bool) :
    (if b = false then (1 : Int) else 0) = if b = true then 0 else 1 := by
  cases b <;> rfl

theorem fword_mod (f : Float) : fword f % 18446744073709551616 = fword f := by
  have := f.toBits.toNat_lt
  unfold fword; simp only [Int.ofNat_eq_natCast]; omega

theorem fbits_mod (a : Int) : fbits (a % 18446744073709551616) = fbits a := by
  unfold fbits enc W; rw [Int.emod_emod_of_dvd _ (by decide)]

theorem bxor_ite (p : Prop) [Decidable p] :
    bxor (if p then 1 else 0) (enc 1) = if p then 0 else 1 := by
  split <;> decide

/-! ## Immediates: `li` in six chunks -/

theorem wx_eq (s : St) (r : Nat) (a b : Int) (h : a % W = b % W) : wx s r a = wx s r b := by
  simp only [wx, h]

theorem lands_congr {base : Int} {c : List Inst} {s s' : St} {P : St → Prop} (e : s = s')
    (h : Lands base c s' P) : Lands base c s P := e ▸ h

theorem chunk_lands {base : Int} {c : List Inst} {s : St} {P : St → Prop} (k rd : Nat)
    (hrd : rd ≠ 0) (v cc : Int) (hv : 0 ≤ v ∧ v < 9007199254740992) (hcc : 0 ≤ cc ∧ cc < 2048)
    (hpc : s.pc = 4 * k) (e1 : c[k]? = some (.slli rd rd 11))
    (e2 : c[k + 1]? = some (.i .ori rd rd cc)) (hr : rx s rd = v)
    (h : Lands base c (setPc (wx s rd (v * 2048 + cc)) (s.pc + 8)) P) : Lands base c s P := by
  refine lands_step k hpc e1 rfl ?_
  refine lands_step (k + 1) (by simp only [next_pc, wx_pc]; omega) e2 rfl ?_
  have hv1 : rx (next (wx s rd (rx s rd * 2 ^ 11))) rd = v * 2048 := by
    simp only [rx_next, rx_wx, hrd, hr, reduceIte, W]; omega
  have hb := bor_step v cc hv hcc
  simp only [iop, hv1]
  have e : next (wx (next (wx s rd (rx s rd * 2 ^ 11))) rd (bor (v * 2048) (enc cc))) =
      setPc (wx s rd (v * 2048 + cc)) (s.pc + 8) := by
    simp only [next, setPc, wx, St.mk.injEq, and_true, true_and]
    refine ⟨?_, by omega⟩
    funext r'; split
    · rw [hb]; unfold W; omega
    · rfl
  rw [e]; exact h

theorem imm_lands {base : Int} {lab : Nat → Int} {rest : List Item} {s : St} {P : St → Prop}
    (rd n : Nat) (hrd : rd ≠ 0) (hn : n < 18446744073709551616) (hp : s.pc = 0)
    (h : Lands base (code lab (imm rd n ++ rest)) (setPc (wx s rd n) 44) P) :
    Lands base (code lab (imm rd n ++ rest)) s P := by
  refine lands_step 0 (by omega) rfl rfl ?_
  refine chunk_lands 1 rd hrd (n / 2 ^ 55 : Nat) (n / 2 ^ 44 % 2048 : Nat) (by omega) (by omega)
    (by simp only [next_pc, wx_pc, hp]; omega) rfl rfl
    (by simp only [rx_next, rx_wx, hrd, reduceIte, iop, zero, rx_zero, W]; omega) ?_
  refine chunk_lands 3 rd hrd (n / 2 ^ 44 : Nat) (n / 2 ^ 33 % 2048 : Nat) (by omega) (by omega)
    (by simp only [setPc_pc, next_pc, wx_pc, hp]; omega) rfl rfl
    (by simp only [rx_setPc, rx_wx, hrd, reduceIte, W]; omega) ?_
  refine chunk_lands 5 rd hrd (n / 2 ^ 33 : Nat) (n / 2 ^ 22 % 2048 : Nat) (by omega) (by omega)
    (by simp only [setPc_pc, next_pc, wx_pc, hp]; omega) rfl rfl
    (by simp only [rx_setPc, rx_wx, hrd, reduceIte, W]; omega) ?_
  refine chunk_lands 7 rd hrd (n / 2 ^ 22 : Nat) (n / 2 ^ 11 % 2048 : Nat) (by omega) (by omega)
    (by simp only [setPc_pc, next_pc, wx_pc, hp]; omega) rfl rfl
    (by simp only [rx_setPc, rx_wx, hrd, reduceIte, W]; omega) ?_
  refine chunk_lands 9 rd hrd (n / 2 ^ 11 : Nat) (n % 2048 : Nat) (by omega) (by omega)
    (by simp only [setPc_pc, next_pc, wx_pc, hp]; omega) rfl rfl
    (by simp only [rx_setPc, rx_wx, hrd, reduceIte, W]; omega) ?_
  refine lands_congr ?_ h
  simp only [next, setPc, wx, St.mk.injEq, and_true, true_and]
  refine ⟨?_, by omega⟩
  funext r'
  repeat' split
  all_goals first | rfl | (unfold W; omega)

theorem exec_ld (base : Int) (s : St) (rd r1 : Nat) (imm A : Int) (hA : enc (rx s r1 + imm) = A)
    (h8 : A % 8 = 0) : exec base (.ld rd r1 imm) s = some (next (wx s rd (s.mem A))) := by
  simp only [exec, hA, h8, reduceIte]

theorem exec_sd (base : Int) (s : St) (r2 r1 : Nat) (imm A : Int) (hA : enc (rx s r1 + imm) = A)
    (h8 : A % 8 = 0) : exec base (.sd r2 r1 imm) s = some (next (wmem s A (rx s r2))) := by
  simp only [exec, hA, h8, reduceIte]

theorem addr_inj {fr : Frame} {l l' : Loc} (hle : fr.out ≤ fr.words) (hv : fr.Valid l)
    (hv' : fr.Valid l') (h : fr.addr l' = fr.addr l) : l' = l := by
  cases l <;> cases l' <;> simp only [Frame.Valid, Frame.addr] at hv hv' h <;>
    first | (congr 1; omega) | omega

theorem upd_frame {fr : Frame} {l : Loc} {A : Int} (hle : fr.out ≤ fr.words) (hv : fr.Valid l)
    (hA : A = fr.addr l)
    (v : Int) (mm : Store) (l' : Loc) (hl' : fr.Valid l') :
    (if fr.addr l' = A then v else mm l') = upd mm l v l' := by
  unfold upd; subst hA
  by_cases e : l' = l
  · subst e; simp
  · rw [ite_eq_right (fun h => e (addr_inj hle hv hl' h)), ite_eq_right e]

section Instr
set_option linter.unusedSectionVars false
variable (fr : Frame) (base : Int) (lab : Nat → Int) (k : Bool) (m : Mach) (s : St)
  (hc : Corr fr k m s) (hp : s.pc = 0) (hl : ∀ f, lab (trapLabel f) < 0)
include hc hp hl

/-- The simplifier set for pc equations and register reads of a symbolic run. -/
macro "rv_simp" "[" hs:Lean.Parser.Tactic.simpLemma,* "]" : tactic => `(tactic|
  simp only [next_pc, setPc_pc, wx_pc, wf_pc, wmem_pc, bcond, rx_wx, rx_next, rx_setPc, rx_wf,
    rx_wmem, rx_zero, rop, iop, Nat.reduceEqDiff, reduceIte, ite_true, ite_false, pc_add_sub,
    decide_eq_true_eq, decide_not, Bool.not_eq_true', Nat.reduceAdd, Int.natCast_add,
    Int.cast_ofNat_Int, Int.natCast_one, Int.reduceMul, Int.reduceAdd, List.length_cons,
    List.length_nil, setReg_reg, setReg_mem, wx_mem, next_mem, setPc_mem, wf_mem, wmem_mem,
    reduceCtorEq, decide_false, decide_true, Bool.false_eq_true, Bool.true_eq_false, eq_self_iff_true,
    Int.reducePow, Nat.reducePow, decide_eq_false_iff_not, ite01_mod, W, H, $hs,*])

/-- Rewrite word readings into linear arithmetic over literals (at the
given locations): `neg?` and `toS` of words become divisions by `2^63`. -/
macro "rv_norm" loc:(Lean.Parser.Tactic.location)? : tactic => `(tactic| (
  try simp only [neg_eq_iff, ne_eq, ite01_mod, ite01_eq, ite01_eq_one, ite01_eq_zero] $[$loc]?
  try simp only [neg?] $[$loc]?
  try simp only [W, H, enc] $[$loc]?
  try simp (disch := omega) only [toS_word] $[$loc]?
  try simp only [decide_eq_decide, decide_eq_true_eq, ne_eq] $[$loc]?))

/-- Close an arithmetic goal over words. -/
macro "rv_arith" : tactic => `(tactic| (
  rv_norm at *
  repeat' split
  all_goals rv_norm at *
  all_goals omega))

/-- Advance over the instruction at index `k`. -/
macro "rv" k:num "[" hs:Lean.Parser.Tactic.simpLemma,* "]" : tactic => `(tactic|
  refine lands_step $k (by rv_simp [$hs,*] <;> first | omega | rv_arith) rfl rfl ?_)

macro "rv_unfold" : tactic => `(tactic|
  simp only [emit, branchTo, code, placeL, atL, ovf, intMin, Rv64.lt, Rv64.addi, fopOf,
    List.cons_append, List.nil_append, a0, a1, a2, t0, t1, zero, Sign.s, reduceIte])


/-- The relation at the end of a fall-through run. -/
macro "rv_corr" hc:ident "[" hs:Lean.Parser.Tactic.simpLemma,* "]" : tactic => `(tactic| (
  refine ⟨fun r => ?_, fun a => ?_, ?_, ?_, fun l hl => ?_, ?_, ?_, ?_⟩
  · simp only [wx_x, next_x, setPc_x, wf_x, wmem_x]
    repeat' split
    all_goals first | exact word_mod _ | exact ($hc).words r
  · simp only [wx_mem, next_mem, setPc_mem, wf_mem, wmem_mem]
    repeat' split
    all_goals try simp only [rx_wx, rx_next, rx_setPc, rx_wf, rx_wmem, Nat.reduceEqDiff, reduceIte,
      a0, a1, a2, t0, regX]
    all_goals first | exact ($hc).mwords a | exact ($hc).word _
  · rv_simp [($hc).sp, $hs,*]
  · rv_simp [($hc).s0, $hs,*]
  · rv_simp [($hc).frame l hl, hl, $hs,*]
  all_goals try (rv_simp [($hc).r0, ($hc).r1, $hs,*] <;> first | omega | rfl | rv_arith)
  all_goals try (intro hk; rv_simp [$hs,*]; first | exact ($hc).r2 (by simpa [flag] using hk) | omega | rfl | rv_arith)
  all_goals intro hk; simp [flag] at hk))

macro "rv_fin" hc:ident "[" hs:Lean.Parser.Tactic.simpLemma,* "]" : tactic => `(tactic| (
  refine lands_done ⟨by rv_simp [$hs,*] <;> first | omega | rv_arith, ?_⟩
  rv_corr $hc [$hs,*]))

/-- Finish at a state whose pc is the given target. -/
macro "rv_end" "[" hs:Lean.Parser.Tactic.simpLemma,* "]" : tactic => `(tactic|
  exact lands_done (by rv_simp [$hs,*] <;> first | omega | rv_arith))

/-- Advance over a load or store at index `k` whose address is `A`. -/
macro "rvm" k:num ex:ident A:term:max "[" hs:Lean.Parser.Tactic.simpLemma,* "]" : tactic => `(tactic|
  refine lands_step $k (by rv_simp [$hs,*] <;> first | omega | rv_arith) rfl
    ($ex _ _ _ _ _ $A (by rv_simp [$hs,*] <;> rv_norm <;> omega) (by omega)) ?_)

theorem sim_addchk_u :
    RSim fr base lab (flag (.addchk .u) k) (code lab (emit (.addchk .u))) s
      (Mach.step (.addchk .u) m) := by
  have ha := hc.word 10; have hb := hc.word 11; have hc2 := hc.word 12
  have h0 := hc.r0; have h1 := hc.r1
  rw [h0] at ha; rw [h1] at hb
  unfold Word W at ha hb hc2
  simp only [Mach.step]
  have hl0 := hl .overflow
  by_cases h : m.reg .r0 + m.reg .r1 ≥ W
  · rw [ite_eq_left h]; intro _
    rv_unfold
    rv 0 [hp, h0, h1, h]
    rv 1 [hp, h0, h1, h]
    rv 2 [hp, h0, h1, h]
    rv_end [hp, h0, h1, h]
  · rw [ite_eq_right h]
    rv_unfold
    rv 0 [hp, h0, h1, h]
    rv 1 [hp, h0, h1, h]
    rv_fin hc [hp, h0, h1, h]

theorem sim_addchk_s :
    RSim fr base lab (flag (.addchk .s) k) (code lab (emit (.addchk .s))) s
      (Mach.step (.addchk .s) m) := by
  have ha := hc.word 10; have hb := hc.word 11; have hc2 := hc.word 12
  have h0 := hc.r0; have h1 := hc.r1
  rw [h0] at ha; rw [h1] at hb
  unfold Word W at ha hb hc2
  simp only [Mach.step]
  have hl0 := hl .overflow
  by_cases h : neg? (m.reg .r0) = neg? (m.reg .r1) ∧
      neg? ((m.reg .r0 + m.reg .r1) % W) ≠ neg? (m.reg .r0)
  · rw [ite_eq_left h]; intro _
    rv_norm at h
    rv_unfold
    rv 0 [hp, h0, h1, h]
    rv 1 [hp, h0, h1, h]
    rv 2 [hp, h0, h1, h]
    rv 3 [hp, h0, h1, h]
    rv 4 [hp, h0, h1, h]
    rv 5 [hp, h0, h1, h]
    rv_end [hp, h0, h1, h]
  · rw [ite_eq_right h]
    rv_norm at h
    rv_unfold
    rv 0 [hp, h0, h1, h]
    rv 1 [hp, h0, h1, h]
    rv 2 [hp, h0, h1, h]
    rv 3 [hp, h0, h1, h]
    rv 4 [hp, h0, h1, h]
    rv_fin hc [hp, h0, h1, h]

theorem sim_subchk_u :
    RSim fr base lab (flag (.subchk .u) k) (code lab (emit (.subchk .u))) s
      (Mach.step (.subchk .u) m) := by
  have ha := hc.word 10; have hb := hc.word 11; have hc2 := hc.word 12
  have h0 := hc.r0; have h1 := hc.r1
  rw [h0] at ha; rw [h1] at hb
  unfold Word W at ha hb hc2
  simp only [Mach.step]
  have hl0 := hl .overflow
  by_cases h : m.reg .r0 < m.reg .r1
  · rw [ite_eq_left h]; intro _
    rv_unfold
    rv 0 [hp, h0, h1, h]
    rv 1 [hp, h0, h1, h]
    rv_end [hp, h0, h1, h]
  · rw [ite_eq_right h]
    rv_unfold
    rv 0 [hp, h0, h1, h]
    rv 2 [hp, h0, h1, h]
    rv_fin hc [hp, h0, h1, h]

theorem sim_subchk_s :
    RSim fr base lab (flag (.subchk .s) k) (code lab (emit (.subchk .s))) s
      (Mach.step (.subchk .s) m) := by
  have ha := hc.word 10; have hb := hc.word 11; have hc2 := hc.word 12
  have h0 := hc.r0; have h1 := hc.r1
  rw [h0] at ha; rw [h1] at hb
  unfold Word W at ha hb hc2
  simp only [Mach.step]
  have hl0 := hl .overflow
  by_cases h : neg? (m.reg .r0) ≠ neg? (m.reg .r1) ∧
      neg? ((m.reg .r0 - m.reg .r1) % W) ≠ neg? (m.reg .r0)
  · rw [ite_eq_left h]; intro _
    rv_norm at h
    rv_unfold
    rv 0 [hp, h0, h1, h]
    rv 1 [hp, h0, h1, h]
    rv 2 [hp, h0, h1, h]
    rv 3 [hp, h0, h1, h]
    rv 4 [hp, h0, h1, h]
    rv 5 [hp, h0, h1, h]
    rv_end [hp, h0, h1, h]
  · rw [ite_eq_right h]
    rv_norm at h
    rv_unfold
    rv 0 [hp, h0, h1, h]
    rv 1 [hp, h0, h1, h]
    rv 2 [hp, h0, h1, h]
    rv 3 [hp, h0, h1, h]
    rv 4 [hp, h0, h1, h]
    rv_fin hc [hp, h0, h1, h]

theorem sim_wrap (op : WrapOp) :
    RSim fr base lab (flag (.wrap op) k) (code lab (emit (.wrap op))) s
      (Mach.step (.wrap op) m) := by
  have ha := hc.word 10; have hb := hc.word 11; have hc2 := hc.word 12
  have h0 := hc.r0; have h1 := hc.r1
  rw [h0] at ha; rw [h1] at hb
  unfold Word W at ha hb hc2
  simp only [Mach.step]
  cases op <;>
  · rv_unfold
    rv 0 [hp, h0, h1]
    rv_fin hc [hp, h0, h1, wrapOp]

theorem sim_trapgtu (h2 : k = true) :
    RSim fr base lab (flag .trapgtu k) (code lab (emit .trapgtu)) s (Mach.step .trapgtu m) := by
  have ha := hc.word 10; have hb := hc.word 11; have hc2 := hc.word 12
  have h0 := hc.r0; have h1 := hc.r1
  rw [h0] at ha; rw [h1] at hb
  unfold Word W at ha hb hc2
  simp only [Mach.step]
  have hl0 := hl .overflow
  have e2 := hc.r2 h2
  rw [e2] at hc2
  by_cases h : m.reg .r0 > m.reg .r2
  · rw [ite_eq_left h]; intro _
    rv_unfold
    rv 0 [hp, h0, h1, e2]
    rv 1 [hp, h0, h1, h]
    rv_end [hp, h0, h1, h]
  · rw [ite_eq_right h]
    rv_unfold
    rv 0 [hp, h0, h1, e2]
    rv_fin hc [hp, h0, h1, e2]

theorem sim_trapzero :
    RSim fr base lab (flag .trapzero k) (code lab (emit .trapzero)) s (Mach.step .trapzero m) := by
  have ha := hc.word 10; have hb := hc.word 11; have hc2 := hc.word 12
  have h0 := hc.r0; have h1 := hc.r1
  rw [h0] at ha; rw [h1] at hb
  unfold Word W at ha hb hc2
  simp only [Mach.step]
  have hl1 := hl .divZero
  by_cases h : m.reg .r1 = 0
  · rw [ite_eq_left h]; intro _
    rv_unfold
    rv 0 [hp, h0, h1, h]
    rv 1 [hp, h0, h1, h]
    rv_end [hp, h0, h1, h]
  · rw [ite_eq_right h]
    rv_unfold
    rv 0 [hp, h0, h1, h]
    rv_fin hc [hp, h0, h1, h]

theorem sim_trapdivovf :
    RSim fr base lab (flag .trapdivovf k) (code lab (emit .trapdivovf)) s
      (Mach.step .trapdivovf m) := by
  have ha := hc.word 10; have hb := hc.word 11; have hc2 := hc.word 12
  have h0 := hc.r0; have h1 := hc.r1
  rw [h0] at ha; rw [h1] at hb
  unfold Word W at ha hb hc2
  simp only [Mach.step]
  have hl0 := hl .overflow
  by_cases h : m.reg .r0 = H ∧ m.reg .r1 = W - 1
  · rw [ite_eq_left h]; intro _
    rv_unfold
    rv 0 [hp, h0, h1, h]
    rv 1 [hp, h0, h1, h]
    rv 2 [hp, h0, h1, h]
    rv 3 [hp, h0, h1, h]
    rv 4 [hp, h0, h1, h]
    rv 5 [hp, h0, h1, h]
    rv_end [hp, h0, h1, h]
  · rw [ite_eq_right h]
    rv_unfold
    by_cases ha' : m.reg .r0 = H
    · rv 0 [hp, h0, h1, h]
      rv 1 [hp, h0, h1, h]
      rv 2 [hp, h0, h1, h]
      rv 3 [hp, h0, h1, h]
      rv 4 [hp, h0, h1, h]
      rv_fin hc [hp, h0, h1, h]
    · rv 0 [hp, h0, h1, h]
      rv 1 [hp, h0, h1, h]
      rv 2 [hp, h0, h1, h]
      rv_fin hc [hp, h0, h1, h]

theorem sim_negchk_s :
    RSim fr base lab (flag (.negchk .s) k) (code lab (emit (.negchk .s))) s
      (Mach.step (.negchk .s) m) := by
  have ha := hc.word 10; have hb := hc.word 11; have hc2 := hc.word 12
  have h0 := hc.r0; have h1 := hc.r1
  rw [h0] at ha; rw [h1] at hb
  unfold Word W at ha hb hc2
  simp only [Mach.step]
  have hl0 := hl .overflow
  by_cases h : m.reg .r0 = H
  · rw [ite_eq_left h]; intro _
    rv_unfold
    rv 0 [hp, h0, h1, h]
    rv 1 [hp, h0, h1, h]
    rv 2 [hp, h0, h1, h]
    rv 3 [hp, h0, h1, h]
    rv_end [hp, h0, h1, h]
  · rw [ite_eq_right h]
    rv_unfold
    rv 0 [hp, h0, h1, h]
    rv 1 [hp, h0, h1, h]
    rv 2 [hp, h0, h1, h]
    rv 4 [hp, h0, h1, h]
    rv_fin hc [hp, h0, h1, h]

theorem sim_negchk_u :
    RSim fr base lab (flag (.negchk .u) k) (code lab (emit (.negchk .u))) s
      (Mach.step (.negchk .u) m) := by
  have ha := hc.word 10; have hb := hc.word 11; have hc2 := hc.word 12
  have h0 := hc.r0; have h1 := hc.r1
  rw [h0] at ha; rw [h1] at hb
  unfold Word W at ha hb hc2
  simp only [Mach.step]
  have hl0 := hl .overflow
  by_cases h : m.reg .r0 ≠ 0
  · rw [ite_eq_left h]; intro _
    rv_unfold
    rv 0 [hp, h0, h1, h]
    rv 1 [hp, h0, h1, h]
    rv_end [hp, h0, h1, h]
  · rw [ite_eq_right h]
    have h' : m.reg .r0 = 0 := by omega
    rv_unfold
    rv 0 [hp, h0, h1, h]
    rv_fin hc [hp, h0, h']

theorem sim_trap (f : Failure) :
    RSim fr base lab (flag (.trap f) k) (code lab (emit (.trap f))) s (Mach.step (.trap f) m) := by
  have hlf := hl f
  simp only [Mach.step]
  intro _
  rv_unfold
  rv 0 [hp]
  rv_end [hp]
theorem sim_mulchk_s :
    RSim fr base lab (flag (.mulchk .s) k) (code lab (emit (.mulchk .s))) s
      (Mach.step (.mulchk .s) m) := by
  have ha := hc.word 10; have hb := hc.word 11; have hc2 := hc.word 12
  have h0 := hc.r0; have h1 := hc.r1
  rw [h0] at ha; rw [h1] at hb
  unfold Word at ha hb hc2
  have hm := mul_mod_toS _ _ ha hb
  have hB := mulS_bound _ _ ha hb
  obtain ⟨P, hP⟩ : ∃ P, toS (m.reg .r0) * toS (m.reg .r1) = P := ⟨_, rfl⟩
  rw [hP] at hm hB
  unfold W at ha hb hc2 hm
  simp only [Mach.step]
  rw [hP]
  have hl0 := hl .overflow
  by_cases h : toS (P % W) ≠ P
  · rw [ite_eq_left h]; intro _
    rv_unfold
    rv 0 [hp, h0, h1, hP, hm]
    rv 1 [hp, h0, h1, hP, hm]
    rv 2 [hp, h0, h1, hP, hm]
    rv 3 [hp, h0, h1, hP, hm]
    rv 4 [hp, h0, h1, hP, hm]
    rv_end [hp, h0, h1, hP, hm]
  · rw [ite_eq_right h]
    rv_unfold
    rv 0 [hp, h0, h1, hP, hm]
    rv 1 [hp, h0, h1, hP, hm]
    rv 2 [hp, h0, h1, hP, hm]
    rv 3 [hp, h0, h1, hP, hm]
    rv_fin hc [hp, h0, h1, hP, hm]

theorem sim_mulchk_u :
    RSim fr base lab (flag (.mulchk .u) k) (code lab (emit (.mulchk .u))) s
      (Mach.step (.mulchk .u) m) := by
  have ha := hc.word 10; have hb := hc.word 11; have hc2 := hc.word 12
  have h0 := hc.r0; have h1 := hc.r1
  rw [h0] at ha; rw [h1] at hb
  unfold Word at ha hb hc2
  have hB := mulU_bound _ _ ha hb
  obtain ⟨P, hP⟩ : ∃ P, m.reg .r0 * m.reg .r1 = P := ⟨_, rfl⟩
  rw [hP] at hB
  unfold W at ha hb hc2
  simp only [Mach.step]
  rw [hP]
  have hl0 := hl .overflow
  by_cases h : P / W ≠ 0
  · rw [ite_eq_left h]; intro _
    rv_unfold
    rv 0 [hp, h0, h1, hP]
    rv 1 [hp, h0, h1, hP]
    rv 2 [hp, h0, h1, hP]
    rv 3 [hp, h0, h1, hP]
    rv_end [hp, h0, h1, hP]
  · rw [ite_eq_right h]
    rv_unfold
    rv 0 [hp, h0, h1, hP]
    rv 1 [hp, h0, h1, hP]
    rv 2 [hp, h0, h1, hP]
    rv_fin hc [hp, h0, h1, hP]
theorem sim_div_s :
    RSim fr base lab (flag (.div .s) k) (code lab (emit (.div .s))) s (Mach.step (.div .s) m) := by
  have ha := hc.word 10; have hb := hc.word 11; have hc2 := hc.word 12
  have h0 := hc.r0; have h1 := hc.r1
  rw [h0] at ha; rw [h1] at hb
  unfold Word W at ha hb hc2
  simp only [Mach.step]
  by_cases h : m.reg .r1 = 0 ∨ (m.reg .r0 = H ∧ m.reg .r1 = W - 1)
  · rw [ite_eq_left h]; intro hf; exact absurd rfl hf
  · rw [ite_eq_right h]
    have hb0 : m.reg .r1 ≠ 0 := fun e => h (Or.inl e)
    rv_unfold
    rv 0 [hp, h0, h1, hb0]
    rv_fin hc [hp, h0, h1, hb0, enc]

theorem sim_div_u :
    RSim fr base lab (flag (.div .u) k) (code lab (emit (.div .u))) s (Mach.step (.div .u) m) := by
  have ha := hc.word 10; have hb := hc.word 11; have hc2 := hc.word 12
  have h0 := hc.r0; have h1 := hc.r1
  rw [h0] at ha; rw [h1] at hb
  unfold Word W at ha hb hc2
  simp only [Mach.step]
  by_cases h : m.reg .r1 = 0
  · rw [ite_eq_left h]; intro hf; exact absurd rfl hf
  · rw [ite_eq_right h]
    have hq : m.reg .r0 / m.reg .r1 % 18446744073709551616 = m.reg .r0 / m.reg .r1 := by
      have := Int.ediv_nonneg ha.1 hb.1
      have := Int.ediv_le_self (m.reg .r1) ha.1
      omega
    rv_unfold
    rv 0 [hp, h0, h1, h]
    rv_fin hc [hp, h0, h1, h, hq]

theorem sim_rem_s :
    RSim fr base lab (flag (.rem .s) k) (code lab (emit (.rem .s))) s (Mach.step (.rem .s) m) := by
  have ha := hc.word 10; have hb := hc.word 11; have hc2 := hc.word 12
  have h0 := hc.r0; have h1 := hc.r1
  rw [h0] at ha; rw [h1] at hb
  unfold Word W at ha hb hc2
  simp only [Mach.step]
  by_cases h : m.reg .r1 = 0
  · rw [ite_eq_left h]; intro hf; exact absurd rfl hf
  · rw [ite_eq_right h]
    by_cases hm : toS (m.reg .r1) = -1
    · rw [ite_eq_left hm]
      have ht : (toS (m.reg .r0)).tmod (toS (m.reg .r1)) = 0 := by rw [hm, Int.tmod_neg]; exact Int.tmod_one _
      rv_unfold
      rv 0 [hp, h0, h1, h]
      rv_fin hc [hp, h0, h1, h, ht]
    · rw [ite_eq_right hm]
      rv_unfold
      rv 0 [hp, h0, h1, h]
      rv_fin hc [hp, h0, h1, h, enc]

theorem sim_rem_u :
    RSim fr base lab (flag (.rem .u) k) (code lab (emit (.rem .u))) s (Mach.step (.rem .u) m) := by
  have ha := hc.word 10; have hb := hc.word 11; have hc2 := hc.word 12
  have h0 := hc.r0; have h1 := hc.r1
  rw [h0] at ha; rw [h1] at hb
  unfold Word W at ha hb hc2
  simp only [Mach.step]
  by_cases h : m.reg .r1 = 0
  · rw [ite_eq_left h]; intro hf; exact absurd rfl hf
  · rw [ite_eq_right h]
    have hq : m.reg .r0 % m.reg .r1 % 18446744073709551616 = m.reg .r0 % m.reg .r1 := by
      have := Int.emod_nonneg (m.reg .r0) h
      have := Int.emod_lt_of_pos (m.reg .r0) (show 0 < m.reg .r1 by omega)
      omega
    rv_unfold
    rv 0 [hp, h0, h1, h]
    rv_fin hc [hp, h0, h1, h, hq]
theorem sim_xorone :
    RSim fr base lab (flag .xorone k) (code lab (emit .xorone)) s (Mach.step .xorone m) := by
  have ha := hc.word 10; have hb := hc.word 11; have hc2 := hc.word 12
  have h0 := hc.r0; have h1 := hc.r1
  rw [h0] at ha; rw [h1] at hb
  unfold Word W at ha hb hc2
  simp only [Mach.step]
  have hx := bxor_one _ ha
  rv_unfold
  rv 0 [hp, h0, h1]
  rv_fin hc [hp, h0, h1, hx]

theorem sim_andr2 (h2 : k = true) :
    RSim fr base lab (flag .andr2 k) (code lab (emit .andr2)) s (Mach.step .andr2 m) := by
  have ha := hc.word 10; have hb := hc.word 11; have hc2 := hc.word 12
  have h0 := hc.r0; have h1 := hc.r1
  rw [h0] at ha; rw [h1] at hb
  unfold Word W at ha hb hc2
  simp only [Mach.step]
  have e2 := hc.r2 h2
  have hx := band_mod (m.reg .r0) (m.reg .r2) ha
  rv_unfold
  rv 0 [hp, h0, h1, e2]
  rv_fin hc [hp, h0, h1, e2, hx]

theorem sim_cmp (c : Cond) (sg : Sign) :
    RSim fr base lab (flag (.cmp c sg) k) (code lab (emit (.cmp c sg))) s
      (Mach.step (.cmp c sg) m) := by
  have ha := hc.word 10; have hb := hc.word 11; have hc2 := hc.word 12
  have h0 := hc.r0; have h1 := hc.r1
  rw [h0] at ha; rw [h1] at hb
  unfold Word W at ha hb hc2
  simp only [Mach.step]
  cases c <;> cases sg <;> rv_unfold <;>
    first
    | (rv 0 [hp, h0, h1]; rv 1 [hp, h0, h1]; rv_fin hc [hp, h0, h1, cond, bxor_ite])
    | (rv 0 [hp, h0, h1]; rv_fin hc [hp, h0, h1, cond])

theorem sim_fop (op : FloatOp) :
    RSim fr base lab (flag (.fop op) k) (code lab (emit (.fop op))) s (Mach.step (.fop op) m) := by
  have ha := hc.word 10; have hb := hc.word 11; have hc2 := hc.word 12
  have h0 := hc.r0; have h1 := hc.r1
  rw [h0] at ha; rw [h1] at hb
  unfold Word W at ha hb hc2
  simp only [Mach.step]
  have e0 : m.reg .r0 % 18446744073709551616 = m.reg .r0 := by omega
  have e1 : m.reg .r1 % 18446744073709551616 = m.reg .r1 := by omega
  cases op <;> rv_unfold <;>
  · rv 0 [hp, h0, h1, wf_f, next_f, wx_f, setPc_f, fbits_mod, fword_mod]
    rv 1 [hp, h0, h1, wf_f, next_f, wx_f, setPc_f, fbits_mod, fword_mod]
    rv 2 [hp, h0, h1, wf_f, next_f, wx_f, setPc_f, fbits_mod, fword_mod]
    rv 3 [hp, h0, h1, wf_f, next_f, wx_f, setPc_f, fbits_mod, fword_mod]
    rv_fin hc [hp, h0, h1, wf_f, next_f, wx_f, setPc_f, fbits_mod, fword_mod, e0, e1, Vesper.VerifiedCodegen.fop]

theorem sim_fneg (hn : FNegFlips) :
    RSim fr base lab (flag .fneg k) (code lab (emit .fneg)) s (Mach.step .fneg m) := by
  have ha := hc.word 10; have hb := hc.word 11; have hc2 := hc.word 12
  have h0 := hc.r0; have h1 := hc.r1
  rw [h0] at ha; rw [h1] at hb
  unfold Word W at ha hb hc2
  simp only [Mach.step]
  have e0 : m.reg .r0 % 18446744073709551616 = m.reg .r0 := by omega
  have hf := hn _ ha
  rw [hf]
  rv_unfold
  rv 0 [hp, h0, h1, wf_f, next_f, wx_f, setPc_f, fbits_mod, fword_mod]
  rv 1 [hp, h0, h1, wf_f, next_f, wx_f, setPc_f, fbits_mod, fword_mod]
  rv 2 [hp, h0, h1, wf_f, next_f, wx_f, setPc_f, fbits_mod, fword_mod]
  rv_fin hc [hp, h0, h1, wf_f, next_f, wx_f, setPc_f, fbits_mod, fword_mod, e0, signFlip]

theorem sim_fcmp (c : Cond) :
    RSim fr base lab (flag (.fcmp c) k) (code lab (emit (.fcmp c))) s (Mach.step (.fcmp c) m) := by
  have ha := hc.word 10; have hb := hc.word 11; have hc2 := hc.word 12
  have h0 := hc.r0; have h1 := hc.r1
  rw [h0] at ha; rw [h1] at hb
  unfold Word W at ha hb hc2
  simp only [Mach.step]
  have e0 : m.reg .r0 % 18446744073709551616 = m.reg .r0 := by omega
  have e1 : m.reg .r1 % 18446744073709551616 = m.reg .r1 := by omega
  cases c <;> rv_unfold <;>
    first
    | (rv 0 [hp, h0, h1, wf_f, next_f, wx_f, setPc_f, fbits_mod, fword_mod]; rv 1 [hp, h0, h1, wf_f, next_f, wx_f, setPc_f, fbits_mod, fword_mod]; rv 2 [hp, h0, h1, wf_f, next_f, wx_f, setPc_f, fbits_mod, fword_mod]; rv 3 [hp, h0, h1, wf_f, next_f, wx_f, setPc_f, fbits_mod, fword_mod]; rv_fin hc [hp, h0, h1, wf_f, next_f, wx_f, setPc_f, fbits_mod, fword_mod, e0, e1, fcond, bxor_ite, ite_bool_not])
    | (rv 0 [hp, h0, h1, wf_f, next_f, wx_f, setPc_f, fbits_mod, fword_mod]; rv 1 [hp, h0, h1, wf_f, next_f, wx_f, setPc_f, fbits_mod, fword_mod]; rv 2 [hp, h0, h1, wf_f, next_f, wx_f, setPc_f, fbits_mod, fword_mod]; rv_fin hc [hp, h0, h1, wf_f, next_f, wx_f, setPc_f, fbits_mod, fword_mod, e0, e1, fcond])
theorem sim_imm (r : Reg) (w : Int) (hw : Word w) :
    RSim fr base lab (flag (.imm r w) k) (code lab (emit (.imm r w))) s (Mach.step (.imm r w) m) := by
  have hn : (((w % 2 ^ 64).toNat : Nat) : Int) = w := by unfold Word W at hw; omega
  simp only [Mach.step, RSim]
  rw [show emit (.imm r w) = imm (regX r) (w % 2 ^ 64).toNat ++ [] from (List.append_nil _).symm]
  refine imm_lands (regX r) _ (by cases r <;> decide) (by omega) hp ?_
  rw [hn]
  have hw' : w % 18446744073709551616 = w := by unfold Word W at hw; omega
  cases r <;>
  · refine lands_done ⟨by simp only [setPc_pc, code, imm, placeL, List.length_cons, List.length_nil, List.append_nil]; rfl, ?_⟩
    rv_corr hc [hp, hw', regX, a0, a1, a2]
theorem sim_ld (r : Reg) (l : Loc) (hok : fr.Ok) (hv : fr.Valid l) :
    RSim fr base lab (flag (.ld r l) k) (code lab (emit (.ld r l))) s (Mach.step (.ld r l) m) := by
  have hfr := hc.frame l hv
  have hmw := hc.mwords (fr.addr l)
  have hsp := hc.sp; have hs0 := hc.s0
  have ⟨hn, ha8, hle, hfit⟩ := hok
  simp only [Mach.step, RSim]
  unfold Word W at hmw; unfold W at hfit
  by_cases hb : 2048 ≤ (frame l).2 <;>
    rcases l with n | n | n <;> simp only [Frame.Valid, Frame.addr] at hv hfr hmw <;>
    simp only [frame] at hb <;> simp only [emit, ldst, frame, hb, reduceIte, ite_true, ite_false]
  · refine imm_lands t0 _ (by decide) (by omega) hp ?_
    cases r <;>
    · rv 11 [hp, hsp, hs0, regX, a0, a1, a2, t0, sp, s0]
      rvm 12 exec_ld (fr.sp + 8 * n) [hp, hsp, hs0, regX, a0, a1, a2, t0, sp, s0]
      refine lands_done ⟨by rv_simp [hp, hsp, hs0, regX, a0, a1, a2, t0, sp, s0]; rfl, ?_⟩
      rv_corr hc [hp, hsp, hs0, regX, a0, a1, a2, t0, sp, s0, hfr]
  · refine imm_lands t0 _ (by decide) (by omega) hp ?_
    cases r <;>
    · rv 11 [hp, hsp, hs0, regX, a0, a1, a2, t0, sp, s0]
      rvm 12 exec_ld (fr.sp + 8 * n) [hp, hsp, hs0, regX, a0, a1, a2, t0, sp, s0]
      refine lands_done ⟨by rv_simp [hp, hsp, hs0, regX, a0, a1, a2, t0, sp, s0]; rfl, ?_⟩
      rv_corr hc [hp, hsp, hs0, regX, a0, a1, a2, t0, sp, s0, hfr]
  · refine imm_lands t0 _ (by decide) (by omega) hp ?_
    cases r <;>
    · rv 11 [hp, hsp, hs0, regX, a0, a1, a2, t0, sp, s0]
      rvm 12 exec_ld (fr.sp + 8 * fr.words + 16 + 8 * n) [hp, hsp, hs0, regX, a0, a1, a2, t0, sp, s0]
      refine lands_done ⟨by rv_simp [hp, hsp, hs0, regX, a0, a1, a2, t0, sp, s0]; rfl, ?_⟩
      rv_corr hc [hp, hsp, hs0, regX, a0, a1, a2, t0, sp, s0, hfr]
  · cases r <;>
    · rvm 0 exec_ld (fr.sp + 8 * n) [hp, hsp, hs0, regX, a0, a1, a2, t0, sp, s0]
      refine lands_done ⟨by rv_simp [hp, hsp, hs0, regX, a0, a1, a2, t0, sp, s0]; rfl, ?_⟩
      rv_corr hc [hp, hsp, hs0, regX, a0, a1, a2, t0, sp, s0, hfr]
  · cases r <;>
    · rvm 0 exec_ld (fr.sp + 8 * n) [hp, hsp, hs0, regX, a0, a1, a2, t0, sp, s0]
      refine lands_done ⟨by rv_simp [hp, hsp, hs0, regX, a0, a1, a2, t0, sp, s0]; rfl, ?_⟩
      rv_corr hc [hp, hsp, hs0, regX, a0, a1, a2, t0, sp, s0, hfr]
  · cases r <;>
    · rvm 0 exec_ld (fr.sp + 8 * fr.words + 16 + 8 * n) [hp, hsp, hs0, regX, a0, a1, a2, t0, sp, s0]
      refine lands_done ⟨by rv_simp [hp, hsp, hs0, regX, a0, a1, a2, t0, sp, s0]; rfl, ?_⟩
      rv_corr hc [hp, hsp, hs0, regX, a0, a1, a2, t0, sp, s0, hfr]

theorem sim_st (l : Loc) (r : Reg) (hok : fr.Ok) (hv : fr.Valid l) (hrv : rx s (regX r) = m.reg r) :
    RSim fr base lab (flag (.st l r) k) (code lab (emit (.st l r))) s (Mach.step (.st l r) m) := by
  have hsp := hc.sp; have hs0 := hc.s0
  have h0 := hc.r0; have h1 := hc.r1
  have ⟨hn, ha8, hle, hfit⟩ := hok
  simp only [Mach.step, RSim]
  unfold W at hfit
  by_cases hb : 2048 ≤ (frame l).2 <;>
    rcases l with n | n | n <;> simp only [Frame.Valid] at hv <;>
    simp only [frame] at hb <;> simp only [emit, ldst, frame, hb, reduceIte, ite_true, ite_false]
  · refine imm_lands t0 _ (by decide) (by omega) hp ?_
    cases r <;> simp only [regX, a0, a1, a2] at hrv <;>
    · rv 11 [hp, hsp, hs0, h0, h1, regX, a0, a1, a2, t0, sp, s0]
      rvm 12 exec_sd (fr.sp + 8 * n) [hp, hsp, hs0, h0, h1, regX, a0, a1, a2, t0, sp, s0]
      refine lands_done ⟨by rv_simp [hp, hsp, hs0, h0, h1, regX, a0, a1, a2, t0, sp, s0]; rfl, ?_⟩
      rv_corr hc [hp, hsp, hs0, h0, h1, regX, a0, a1, a2, t0, sp, s0, hrv, upd_frame (l := .slot n) hle hv (A := fr.sp + 8 * n) rfl]
  · refine imm_lands t0 _ (by decide) (by omega) hp ?_
    cases r <;> simp only [regX, a0, a1, a2] at hrv <;>
    · rv 11 [hp, hsp, hs0, h0, h1, regX, a0, a1, a2, t0, sp, s0]
      rvm 12 exec_sd (fr.sp + 8 * n) [hp, hsp, hs0, h0, h1, regX, a0, a1, a2, t0, sp, s0]
      refine lands_done ⟨by rv_simp [hp, hsp, hs0, h0, h1, regX, a0, a1, a2, t0, sp, s0]; rfl, ?_⟩
      rv_corr hc [hp, hsp, hs0, h0, h1, regX, a0, a1, a2, t0, sp, s0, hrv, upd_frame (l := .out n) hle hv (A := fr.sp + 8 * n) rfl]
  · refine imm_lands t0 _ (by decide) (by omega) hp ?_
    cases r <;> simp only [regX, a0, a1, a2] at hrv <;>
    · rv 11 [hp, hsp, hs0, h0, h1, regX, a0, a1, a2, t0, sp, s0]
      rvm 12 exec_sd (fr.sp + 8 * fr.words + 16 + 8 * n) [hp, hsp, hs0, h0, h1, regX, a0, a1, a2, t0, sp, s0]
      refine lands_done ⟨by rv_simp [hp, hsp, hs0, h0, h1, regX, a0, a1, a2, t0, sp, s0]; rfl, ?_⟩
      rv_corr hc [hp, hsp, hs0, h0, h1, regX, a0, a1, a2, t0, sp, s0, hrv, upd_frame (l := .inn n) hle hv (A := fr.sp + 8 * fr.words + 16 + 8 * n) rfl]
  · cases r <;> simp only [regX, a0, a1, a2] at hrv <;>
    · rvm 0 exec_sd (fr.sp + 8 * n) [hp, hsp, hs0, h0, h1, regX, a0, a1, a2, t0, sp, s0]
      refine lands_done ⟨by rv_simp [hp, hsp, hs0, h0, h1, regX, a0, a1, a2, t0, sp, s0]; rfl, ?_⟩
      rv_corr hc [hp, hsp, hs0, h0, h1, regX, a0, a1, a2, t0, sp, s0, hrv, upd_frame (l := .slot n) hle hv (A := fr.sp + 8 * n) rfl]
  · cases r <;> simp only [regX, a0, a1, a2] at hrv <;>
    · rvm 0 exec_sd (fr.sp + 8 * n) [hp, hsp, hs0, h0, h1, regX, a0, a1, a2, t0, sp, s0]
      refine lands_done ⟨by rv_simp [hp, hsp, hs0, h0, h1, regX, a0, a1, a2, t0, sp, s0]; rfl, ?_⟩
      rv_corr hc [hp, hsp, hs0, h0, h1, regX, a0, a1, a2, t0, sp, s0, hrv, upd_frame (l := .out n) hle hv (A := fr.sp + 8 * n) rfl]
  · cases r <;> simp only [regX, a0, a1, a2] at hrv <;>
    · rvm 0 exec_sd (fr.sp + 8 * fr.words + 16 + 8 * n) [hp, hsp, hs0, h0, h1, regX, a0, a1, a2, t0, sp, s0]
      refine lands_done ⟨by rv_simp [hp, hsp, hs0, h0, h1, regX, a0, a1, a2, t0, sp, s0]; rfl, ?_⟩
      rv_corr hc [hp, hsp, hs0, h0, h1, regX, a0, a1, a2, t0, sp, s0, hrv, upd_frame (l := .inn n) hle hv (A := fr.sp + 8 * fr.words + 16 + 8 * n) rfl]

/-- A trap scope emits no code and leaves the machine unchanged. -/
theorem sim_scope (o d : Nat) :
    RSim fr base lab (flag (.scope o d) k) (code lab (emit (.scope o d))) s
      (Mach.step (.scope o d) m) := by
  simp only [Mach.step, RSim, emit, flag]
  exact lands_done ⟨by simp only [code, placeL, List.length_nil, hp]; rfl, hc⟩

/-- Every data instruction's emitted RV64 sequence implements `Mach.step`. -/
theorem emit_sim (i : M) (hok : fr.Ok) (hn : FNegFlips) (hd : DataM i) (hf : Fits fr i)
    (h2 : needs2 i = true → k = true) :
    RSim fr base lab (flag i k) (code lab (emit i)) s (Mach.step i m) := by
  cases i with
  | ld r l => exact sim_ld _ _ _ _ _ _ hc hp hl r l hok hf
  | st l r =>
    refine sim_st _ _ _ _ _ _ hc hp hl l r hok hf ?_
    cases r
    · exact hc.r0
    · exact hc.r1
    · exact hc.r2 (h2 rfl)
  | imm r w => exact sim_imm _ _ _ _ _ _ hc hp hl r w hf
  | addchk sg => cases sg; exact sim_addchk_s _ _ _ _ _ _ hc hp hl; exact sim_addchk_u _ _ _ _ _ _ hc hp hl
  | subchk sg => cases sg; exact sim_subchk_s _ _ _ _ _ _ hc hp hl; exact sim_subchk_u _ _ _ _ _ _ hc hp hl
  | mulchk sg => cases sg; exact sim_mulchk_s _ _ _ _ _ _ hc hp hl; exact sim_mulchk_u _ _ _ _ _ _ hc hp hl
  | wrap op => exact sim_wrap _ _ _ _ _ _ hc hp hl op
  | andr2 => exact sim_andr2 _ _ _ _ _ _ hc hp hl (h2 rfl)
  | trapgtu => exact sim_trapgtu _ _ _ _ _ _ hc hp hl (h2 rfl)
  | trapzero => exact sim_trapzero _ _ _ _ _ _ hc hp hl
  | trapdivovf => exact sim_trapdivovf _ _ _ _ _ _ hc hp hl
  | div sg => cases sg; exact sim_div_s _ _ _ _ _ _ hc hp hl; exact sim_div_u _ _ _ _ _ _ hc hp hl
  | rem sg => cases sg; exact sim_rem_s _ _ _ _ _ _ hc hp hl; exact sim_rem_u _ _ _ _ _ _ hc hp hl
  | negchk sg => cases sg; exact sim_negchk_s _ _ _ _ _ _ hc hp hl; exact sim_negchk_u _ _ _ _ _ _ hc hp hl
  | xorone => exact sim_xorone _ _ _ _ _ _ hc hp hl
  | cmp c sg => exact sim_cmp _ _ _ _ _ _ hc hp hl c sg
  | fop op => exact sim_fop _ _ _ _ _ _ hc hp hl op
  | fneg => exact sim_fneg _ _ _ _ _ _ hc hp hl hn
  | fcmp c => exact sim_fcmp _ _ _ _ _ _ hc hp hl c
  | trap f => exact sim_trap _ _ _ _ _ _ hc hp hl f
  | scope o d => exact sim_scope _ _ _ _ _ _ hc hp hl o d
  | label | jmp | bz | call | enter | leave => exact hd.elim

/-- `bz l` jumps to label `l` exactly when `r0` is zero, and otherwise falls
through with the state unchanged. -/
theorem sim_bz (l : Nat) :
    Lands base (code lab (emit (.bz l))) s fun s' =>
      (if m.reg .r0 = 0 then s'.pc = lab l else s'.pc = 8) ∧ Corr fr k m s' := by
  have h0 := hc.r0
  rv_unfold
  by_cases h : m.reg .r0 = 0
  · rv 0 [hp, h0, h]
    rv 1 [hp, h0, h]
    refine lands_done ⟨by rv_simp [hp, h0, h], ?_⟩
    rv_corr hc [hp, h0, h]
  · rv 0 [hp, h0, h]
    refine lands_done ⟨by rv_simp [hp, h0, h], ?_⟩
    rv_corr hc [hp, h0, h]

/-- `jmp l` jumps to label `l` with the state unchanged. -/
theorem sim_jmp (l : Nat) :
    Lands base (code lab (emit (.jmp l))) s fun s' => s'.pc = lab l ∧ Corr fr k m s' := by
  rv_unfold
  rv 0 [hp]
  refine lands_done ⟨by rv_simp [hp], ?_⟩
  rv_corr hc [hp]

end Instr

/-! ## Instruction sequences and rows -/

/-- Whether `a2` holds `m.reg .r2` after a sequence. -/
def flagL : List M → Bool → Bool
  | [], k => k
  | i :: is, k => flagL is (flag i k)

/-- Every instruction that reads `r2` finds it related. -/
def R2Ok : Bool → List M → Prop
  | _, [] => True
  | k, i :: is => (needs2 i = true → k = true) ∧ R2Ok (flag i k) is

/-- A straight-line run of data instructions: the concatenated RV64 code
reaches its end in a related state, or a trap label when the machine aborts. -/
theorem run_sim (fr : Frame) (hok : fr.Ok) (hn : FNegFlips) :
    ∀ (is : List M) (base : Int) (lab : Nat → Int) (k : Bool) (m : Mach) (s : St),
      (∀ i ∈ is, DataM i ∧ Fits fr i) → R2Ok k is → Corr fr k m s → s.pc = 0 →
      (∀ f, lab (trapLabel f) < 0) →
      RSim fr base lab (flagL is k) (code lab (is.flatMap emit)) s (run is m)
  | [], base, lab, k, m, s, _, _, hc, hp, _ => by
    simp only [run, RSim, List.flatMap_nil, flagL]
    exact lands_done ⟨by simp only [code, placeL, List.length_nil, hp]; rfl, hc⟩
  | i :: is, base, lab, k, m, s, hd, hr, hc, hp, hl => by
    have hi := emit_sim fr base lab k m s hc hp hl i hok hn (hd i (List.mem_cons_self ..)).1
      (hd i (List.mem_cons_self ..)).2 hr.1
    have hd' : ∀ j ∈ is, DataM j ∧ Fits fr j := fun j hj => hd j (List.mem_cons_of_mem _ hj)
    simp only [run, List.flatMap_cons, flagL]
    revert hi
    cases Mach.step i m with
    | abort f =>
      intro hi
      simp only [Res.bind, RSim] at hi ⊢
      intro hf
      obtain ⟨s', hs, hpc⟩ := hi hf
      refine ⟨s', ?_, hpc⟩
      rw [code_append]; exact hs.append _
    | ok m' =>
      intro hi
      obtain ⟨s1, hs1, hpc1, hc1⟩ := hi
      rw [code_length] at hpc1
      simp only [Res.bind_ok]
      have ih := run_sim fr hok hn is (base + 4 * (emit i).length) (shiftLab lab (emit i).length)
        (flag i k) m' (setPc s1 0) hd' hr.2 (hc1.setPc 0) rfl
        (by intro f; unfold shiftLab; have := hl f; omega)
      revert ih
      cases run is m' with
      | ok m'' =>
        intro ih
        obtain ⟨s2, hs2, hpc2, hc2⟩ := ih
        refine ⟨_, steps_seq hs1 hpc1 hs2, ?_, hc2.setPc _⟩
        rw [code_length] at hpc2 ⊢
        simp only [setPc_pc, hpc2, List.length_append]; omega
      | abort f =>
        intro ih hf
        obtain ⟨s2, hs2, hpc2⟩ := ih hf
        refine ⟨_, steps_seq hs1 hpc1 hs2, ?_⟩
        simp only [setPc_pc, hpc2, shiftLab]; omega

theorem select_data (r : Lir) (hd : DataRow r) : ∀ i ∈ select r, DataM i := by
  cases r <;> simp only [DataRow] at hd <;>
    simp only [select, binary, rangeCheck, List.cons_append, List.nil_append, List.mem_cons,
      List.mem_nil_iff, or_false, forall_eq_or_imp, forall_eq, DataM, true_and, and_true,
      imp_self, implies_true] <;>
    first
    | trivial
    | (rename_i op k _ _ _; cases op <;> cases k <;>
        simp [Kind.mask, Kind.sign, DataM, List.mem_cons, rangeCheck])
    | (rename_i op k _ _ _; cases k <;> simp [Kind.mask, DataM])

theorem select_r2 (r : Lir) : R2Ok false (select r) := by
  cases r <;>
    simp only [select, binary, rangeCheck, List.cons_append, List.nil_append, R2Ok, needs2, flag,
      true_and, imp_self, and_true, Bool.false_eq_true, false_implies, and_self] <;>
    first
    | trivial
    | (rename_i op k _ _ _; cases op <;> cases k <;>
        simp [Kind.mask, Kind.sign, R2Ok, needs2, flag, rangeCheck])
    | (rename_i op k _ _ _; cases k <;> simp [Kind.mask, R2Ok, needs2, flag])
    | (rename_i c k _ _ _; cases k <;> simp [R2Ok, needs2, flag])
    | simp [R2Ok, needs2, flag]

/-- A data row of the slot IR: the abstract machine simulates the row
(`select_simulates`), and the RV64 code emitted for the row's instructions
simulates the abstract machine. -/
theorem row_sim (r : Lir) (st : Store) (m : Mach) (s : St) (fr : Frame) (base : Int)
    (lab : Nat → Int) (hok : fr.Ok) (hn : FNegFlips) (hR : Rel st m) (hd : DataRow r)
    (ht : Typed r st) (hfit : ∀ i ∈ select r, Fits fr i) (hc : Corr fr false m s) (hp : s.pc = 0)
    (hl : ∀ f, lab (trapLabel f) < 0) :
    Sim (lirStep r st) (run (select r) m) ∧
      RSim fr base lab (flagL (select r) false) (code lab ((select r).flatMap emit)) s
        (run (select r) m) :=
  ⟨select_simulates r st m hR hd ht,
   run_sim fr hok hn (select r) base lab false m s
     (fun i hi => ⟨select_data r hd i hi, hfit i hi⟩) (select_r2 r) hc hp hl⟩

end Vesper.VerifiedCodegen.Rv64.Sem

#print axioms Vesper.VerifiedCodegen.Rv64.Sem.wx_pc
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.wx_mem
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.wx_f
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.wf_pc
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.wf_mem
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.wf_f
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.rx_wf
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.setPc_pc
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.setPc_mem
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.setPc_f
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.rx_setPc
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.wmem_pc
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.wmem_f
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.wmem_mem
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.rx_wmem
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.rx_wx
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.next_pc
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.next_mem
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.next_f
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.rx_next
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.pc_add_sub
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.rx_zero
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.wx_x
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.wf_x
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.setPc_x
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.next_x
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.wmem_x
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.setReg_reg
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.setReg_mem
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.mod_word
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.bor_step
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.Steps.trans
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.fetch_append
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.Steps.append
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.fetch_shift
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.exec_shift
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.Steps.shift
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.setPc_self
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.setPc_setPc
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.placeL_length
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.placeL_append
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.placeL_shift
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.code_append
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.code_length
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.steps_seq
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.word_mod
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.Corr.setPc
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.Corr.word
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.lands_done
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.lands_step
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.ite01_mod
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.ite01_eq
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.ite01_eq_one
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.ite01_eq_zero
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.neg_eq_iff
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.toS_word
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.toS_emod
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.mul_mod_toS
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.toS_bound
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.mulS_bound
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.mulU_bound
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.xor_one
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.bxor_one
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.band_mod
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.ite_bool_not
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.fword_mod
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.fbits_mod
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.bxor_ite
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.wx_eq
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.lands_congr
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.chunk_lands
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.imm_lands
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.exec_ld
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.exec_sd
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.addr_inj
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.upd_frame
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_addchk_u
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_addchk_s
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_subchk_u
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_subchk_s
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_wrap
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_trapgtu
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_trapzero
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_trapdivovf
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_negchk_s
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_negchk_u
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_trap
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_mulchk_s
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_mulchk_u
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_div_s
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_div_u
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_rem_s
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_rem_u
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_xorone
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_andr2
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_cmp
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_fop
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_fneg
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_fcmp
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_imm
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_ld
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_st
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_scope
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.emit_sim
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_bz
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.sim_jmp
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.run_sim
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.select_data
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.select_r2
#print axioms Vesper.VerifiedCodegen.Rv64.Sem.row_sim
