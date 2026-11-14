/-!
# Verified direct codegen: the instruction-selection table (§42.1 pillar 3b)

This file models three things and proves one theorem family about them.

* `Lir`: the slot IR of `src/verified_codegen/machine.rs`, the scalar subset
  of checked Place IR after every value is flattened into 64-bit frame words.
  Its semantics `lirStep` is mathematical: values are integers and checked
  arithmetic follows `vesper.arith.checked/1` (compute the exact result, abort
  `overflow` when it leaves the kind's range; `/` and `%` by zero abort
  `divZero` first).
* `M`: the abstract target instruction subset the Rust emitter encodes to
  x86-64 and AArch64. Its semantics `Mach.step` works on machine words (an
  integer in `[0, 2^64)`) with hardware-style flags: signed overflow is "the
  operands have the same sign and the result's sign differs", unsigned
  overflow is the carry or borrow, a checked multiply compares the full
  product with its low word, and division faults on a zero divisor (and, for
  the signed form, on `MIN / -1`).
* `select`: the instruction-selection table, row by row the same function as
  `select` in `machine.rs`. `listing` prints it in the format of
  `table_listing()`, and `tests/verified_codegen_lean.rs` checks that the two
  print the same text.

The theorem `select_simulates` is a forward simulation for every data row of
the table: if each frame word of the machine holds the two's-complement
encoding of the slot IR value (`Rel`) and the row's operands are in range of
their kind, then running the selected machine instructions either reaches a
state that is again related to the slot IR state after the row, or aborts with
the same failure as the slot IR. `select_branch` shows the branch row tests the
same condition.

What is *not* proved: anything about the Rust code (the emitter, the Place IR
lowering, the byte encodings of `M`, the ELF/Mach-O containers), the control
rows (labels, jumps, calls, prologue and epilogue), or that the Place IR
lowering produces well-typed slot IR. Those are covered by differential tests
against the evaluator and Cranelift, not by this file. The theorems are about
these models only.
-/

namespace Vesper.VerifiedCodegen

/-! ## Words -/

/-- `2^64`. -/
def W : Int := 18446744073709551616
/-- `2^63`. -/
def H : Int := 9223372036854775808

/-- The two's-complement encoding of an integer as a machine word. -/
def enc (v : Int) : Int := v % W
/-- A machine word read as a signed integer. -/
def toS (x : Int) : Int := if x < H then x else x - W

theorem enc_lt (v : Int) : 0 ≤ enc v ∧ enc v < W := by
  unfold enc W; omega

/-! ## Kinds and the slot IR -/

inductive Kind | u8 | u32 | u64 | i64 | bool
  deriving DecidableEq, Repr

def Kind.lo : Kind → Int
  | .i64 => -H
  | _ => 0
def Kind.hi : Kind → Int
  | .u8 => 255
  | .u32 => 4294967295
  | .u64 => 18446744073709551615
  | .i64 => 9223372036854775807
  | .bool => 1

/-- Whether a value lies in its kind's range. -/
def fits (k : Kind) (v : Int) : Bool := decide (k.lo ≤ v ∧ v ≤ k.hi)
def InRange (k : Kind) (v : Int) : Prop := k.lo ≤ v ∧ v ≤ k.hi

inductive ArithOp | add | sub | mul | div | rem
  deriving DecidableEq, Repr
inductive WrapOp | add | sub | mul
  deriving DecidableEq, Repr
inductive Cond | eq | ne | lt | le | gt | ge
  deriving DecidableEq, Repr
inductive Failure | overflow | divZero | unreachable | fault
  deriving DecidableEq, Repr

/-- A frame word: a local slot, the outgoing call area, the incoming area. -/
inductive Loc | slot (n : Nat) | out (n : Nat) | inn (n : Nat)
  deriving DecidableEq, Repr

inductive Lir
  | const (d : Nat) (v : Int)
  | move (d a : Nat)
  | arith (op : ArithOp) (k : Kind) (d a b : Nat)
  | wrapping (op : WrapOp) (k : Kind) (d a b : Nat)
  | neg (k : Kind) (d a : Nat)
  | not (d a : Nat)
  | compare (c : Cond) (k : Kind) (d a b : Nat)
  | storeOut (i src : Nat)
  | loadOut (d i : Nat)
  | loadIn (d i : Nat)
  | storeIn (i src : Nat)
  | branchZero (c target : Nat)
  | jump (l : Nat)
  | label (l : Nat)
  | call (l : Nat)
  | enter (words : Nat)
  | ret
  | trap (f : Failure)

inductive Res (α : Type) | ok (a : α) | abort (f : Failure)

def Res.bind {α β : Type} : Res α → (α → Res β) → Res β
  | .ok a, f => f a
  | .abort e, _ => .abort e

@[simp] theorem Res.bind_ok {α β : Type} (a : α) (f : α → Res β) : (Res.ok a).bind f = f a := rfl
@[simp] theorem Res.bind_abort {α β : Type} (e : Failure) (f : α → Res β) :
    (Res.abort e : Res α).bind f = .abort e := rfl
@[simp] theorem Res.bind_ite {α β : Type} (c : Prop) [Decidable c] (x y : Res α) (f : α → Res β) :
    (if c then x else y).bind f = if c then x.bind f else y.bind f := by
  split <;> rfl

abbrev Store := Loc → Int

def upd (s : Store) (l : Loc) (v : Int) : Store := fun l' => if l' = l then v else s l'

def cond (c : Cond) (x y : Int) : Bool :=
  match c with
  | .eq => decide (x = y)
  | .ne => decide (x ≠ y)
  | .lt => decide (x < y)
  | .le => decide (x ≤ y)
  | .gt => decide (x > y)
  | .ge => decide (x ≥ y)

/-- The number of bits of an integer kind. -/
def Kind.modulus : Kind → Int
  | .u8 => 256
  | .u32 => 4294967296
  | _ => 18446744073709551616

/-- `wrapping_*` (`IntegerKind::wrapping`): the exact result reduced into the
kind's range modulo `2^bits`. -/
def wrapInto (k : Kind) (r : Int) : Int :=
  match k with
  | .i64 => toS (r % W)
  | _ => r % k.modulus

def checkedArith (op : ArithOp) (k : Kind) (x y : Int) : Res Int :=
  match op with
  | .add => if fits k (x + y) then .ok (x + y) else .abort .overflow
  | .sub => if fits k (x - y) then .ok (x - y) else .abort .overflow
  | .mul => if fits k (x * y) then .ok (x * y) else .abort .overflow
  | .div => if y = 0 then .abort .divZero
            else if fits k (x.tdiv y) then .ok (x.tdiv y) else .abort .overflow
  -- `%` never overflows (`i64::MIN % -1` is 0): only the divisor is checked.
  | .rem => if y = 0 then .abort .divZero else .ok (x.tmod y)

def wrapOp (op : WrapOp) (x y : Int) : Int :=
  match op with
  | .add => x + y
  | .sub => x - y
  | .mul => x * y

/-- One data row of the slot IR. Control rows do not change the store. -/
def lirStep (r : Lir) (s : Store) : Res Store :=
  match r with
  | .const d v => .ok (upd s (.slot d) v)
  | .move d a => .ok (upd s (.slot d) (s (.slot a)))
  | .arith op k d a b =>
      (checkedArith op k (s (.slot a)) (s (.slot b))).bind fun v => .ok (upd s (.slot d) v)
  | .wrapping op k d a b =>
      .ok (upd s (.slot d) (wrapInto k (wrapOp op (s (.slot a)) (s (.slot b)))))
  | .neg k d a =>
      if fits k (-(s (.slot a))) then .ok (upd s (.slot d) (-(s (.slot a))))
      else .abort .overflow
  | .not d a => .ok (upd s (.slot d) (1 - s (.slot a)))
  | .compare c _ d a b =>
      .ok (upd s (.slot d) (if cond c (s (.slot a)) (s (.slot b)) then 1 else 0))
  | .storeOut i src => .ok (upd s (.out i) (s (.slot src)))
  | .loadOut d i => .ok (upd s (.slot d) (s (.out i)))
  | .loadIn d i => .ok (upd s (.slot d) (s (.inn i)))
  | .storeIn i src => .ok (upd s (.inn i) (s (.slot src)))
  | .trap f => .abort f
  | _ => .ok s

/-! ## The abstract target instruction subset -/

inductive Reg | r0 | r1 | r2
  deriving DecidableEq, Repr
inductive Sign | s | u
  deriving DecidableEq, Repr

inductive M
  | ld (r : Reg) (l : Loc)
  | st (l : Loc) (r : Reg)
  | imm (r : Reg) (w : Int)
  | addchk (sg : Sign)
  | subchk (sg : Sign)
  | mulchk (sg : Sign)
  | wrap (op : WrapOp)
  | andr2
  | trapgtu
  | trapzero
  | trapdivovf
  | div (sg : Sign)
  | rem (sg : Sign)
  | negchk (sg : Sign)
  | xorone
  | cmp (c : Cond) (sg : Sign)
  | label (l : Nat)
  | jmp (l : Nat)
  | bz (l : Nat)
  | call (l : Nat)
  | enter (words : Nat)
  | leave
  | trap (f : Failure)

structure Mach where
  mem : Loc → Int
  reg : Reg → Int

def setReg (m : Mach) (r : Reg) (v : Int) : Mach :=
  { m with reg := fun r' => if r' = r then v else m.reg r' }

/-- Is the word's sign bit set? -/
def neg? (x : Int) : Bool := decide (H ≤ x)

/-- Bitwise `x & (2^n - 1)` on words, i.e. `x mod 2^n`
(`Nat.and_two_pow_sub_one_eq_mod`); the only masks the table uses. -/
def andMask (x mask : Int) : Int :=
  if mask = 255 then x % 256
  else if mask = 4294967295 then x % 4294967296
  else Int.ofNat (x.toNat &&& mask.toNat)

/-- One machine instruction on the data state. Control instructions are
outside this straight-line model and leave it unchanged. -/
def Mach.step (i : M) (m : Mach) : Res Mach :=
  let a := m.reg .r0
  let b := m.reg .r1
  let c := m.reg .r2
  match i with
  | .ld r l => .ok (setReg m r (m.mem l))
  | .st l r => .ok { m with mem := upd m.mem l (m.reg r) }
  | .imm r w => .ok (setReg m r w)
  | .addchk .s =>
      let r := (a + b) % W
      if neg? a = neg? b ∧ neg? r ≠ neg? a then .abort .overflow else .ok (setReg m .r0 r)
  | .addchk .u => if a + b ≥ W then .abort .overflow else .ok (setReg m .r0 ((a + b) % W))
  | .subchk .s =>
      let r := (a - b) % W
      if neg? a ≠ neg? b ∧ neg? r ≠ neg? a then .abort .overflow else .ok (setReg m .r0 r)
  | .subchk .u => if a < b then .abort .overflow else .ok (setReg m .r0 ((a - b) % W))
  | .mulchk .s =>
      let p := toS a * toS b
      if toS (p % W) ≠ p then .abort .overflow
      else .ok (setReg (setReg m .r0 (p % W)) .r2 ((p / W) % W))
  | .mulchk .u =>
      let p := a * b
      if p / W ≠ 0 then .abort .overflow else .ok (setReg (setReg m .r0 (p % W)) .r2 0)
  | .wrap op => .ok (setReg m .r0 (wrapOp op a b % W))
  | .andr2 => .ok (setReg m .r0 (andMask a c))
  | .trapgtu => if a > c then .abort .overflow else .ok m
  | .trapzero => if b = 0 then .abort .divZero else .ok m
  | .trapdivovf =>
      if a = H ∧ b = W - 1 then .abort .overflow else .ok (setReg m .r2 H)
  | .div .s =>
      if b = 0 ∨ (a = H ∧ b = W - 1) then .abort .fault
      else .ok (setReg (setReg m .r0 (enc ((toS a).tdiv (toS b)))) .r2
        (enc ((toS a).tmod (toS b))))
  | .div .u =>
      if b = 0 then .abort .fault else .ok (setReg (setReg m .r0 (a / b)) .r2 (a % b))
  | .rem .s =>
      if b = 0 then .abort .fault
      else if toS b = -1 then .ok (setReg m .r0 0)
      else .ok (setReg (setReg m .r0 (enc ((toS a).tmod (toS b)))) .r2
        (enc ((toS a).tdiv (toS b))))
  | .rem .u =>
      if b = 0 then .abort .fault else .ok (setReg (setReg m .r0 (a % b)) .r2 (a / b))
  | .negchk .s => if a = H then .abort .overflow else .ok (setReg m .r0 ((W - a) % W))
  | .negchk .u => if a ≠ 0 then .abort .overflow else .ok (setReg m .r0 0)
  | .xorone => .ok (setReg m .r0 (if a % 2 = 0 then a + 1 else a - 1))
  | .cmp c .s => .ok (setReg m .r0 (if cond c (toS a) (toS b) then 1 else 0))
  | .cmp c .u => .ok (setReg m .r0 (if cond c a b then 1 else 0))
  | .trap f => .abort f
  | _ => .ok m

def run : List M → Mach → Res Mach
  | [], m => .ok m
  | i :: is, m => (Mach.step i m).bind (run is)

/-! ## The instruction-selection table -/

def Kind.sign : Kind → Sign
  | .i64 => .s
  | _ => .u
def Kind.mask : Kind → Option Int
  | .u8 => some 255
  | .u32 => some 4294967295
  | _ => none

def binary (a b : Nat) (body : List M) (d : Nat) : List M :=
  [.ld .r0 (.slot a), .ld .r1 (.slot b)] ++ body ++ [.st (.slot d) .r0]

def rangeCheck (k : Kind) (i : M) : List M :=
  match k.mask with
  | some mask => [i, .imm .r2 mask, .trapgtu]
  | none => [i]

/-- `select` in `src/verified_codegen/machine.rs`, row by row. -/
def select : Lir → List M
  | .const d v => [.imm .r0 (enc v), .st (.slot d) .r0]
  | .move d a => [.ld .r0 (.slot a), .st (.slot d) .r0]
  | .arith op k d a b =>
      let body := match op with
        | .add => rangeCheck k (.addchk k.sign)
        | .sub => [.subchk k.sign]
        | .mul => rangeCheck k (.mulchk k.sign)
        | .div => if k = .i64 then [.trapzero, .trapdivovf, .div k.sign]
                  else [.trapzero, .div k.sign]
        | .rem => [.trapzero, .rem k.sign]
      binary a b body d
  | .wrapping op k d a b =>
      let body := match k.mask with
        | some mask => [.wrap op, .imm .r2 mask, .andr2]
        | none => [.wrap op]
      binary a b body d
  | .neg k d a => [.ld .r0 (.slot a), .negchk k.sign, .st (.slot d) .r0]
  | .not d a => [.ld .r0 (.slot a), .xorone, .st (.slot d) .r0]
  | .compare c k d a b => binary a b [.cmp c k.sign] d
  | .storeOut i src => [.ld .r0 (.slot src), .st (.out i) .r0]
  | .loadOut d i => [.ld .r0 (.out i), .st (.slot d) .r0]
  | .loadIn d i => [.ld .r0 (.inn i), .st (.slot d) .r0]
  | .storeIn i src => [.ld .r0 (.slot src), .st (.inn i) .r0]
  | .branchZero c t => [.ld .r0 (.slot c), .bz t]
  | .jump l => [.jmp l]
  | .label l => [.label l]
  | .call l => [.call l]
  | .enter n => [.enter n]
  | .ret => [.leave]
  | .trap f => [.trap f]

/-! ## The simulation relation and the theorem -/

/-- Every frame word holds the encoding of the slot IR value. Registers are
scratch: no value lives in a register between rows. -/
def Rel (s : Store) (m : Mach) : Prop := ∀ l, m.mem l = enc (s l)

def Sim : Res Store → Res Mach → Prop
  | .ok s, .ok m => Rel s m
  | .abort f, .abort g => f = g
  | _, _ => False

/-- The operand typing a row relies on (the checker's guarantee, not proved
here): integer operands lie in their kind's range, booleans are 0 or 1. -/
def Typed : Lir → Store → Prop
  | .arith _ k _ a b, s => k ≠ .bool ∧ InRange k (s (.slot a)) ∧ InRange k (s (.slot b))
  | .wrapping _ k _ a b, s => k ≠ .bool ∧ InRange k (s (.slot a)) ∧ InRange k (s (.slot b))
  | .neg k _ a, s => k ≠ .bool ∧ InRange k (s (.slot a))
  | .not _ a, s => InRange .bool (s (.slot a))
  | .compare _ k _ a b, s => InRange k (s (.slot a)) ∧ InRange k (s (.slot b))
  | _, _ => True

/-- Rows whose effect is on data (everything except labels, jumps, calls,
prologue, epilogue and the branch, which `select_branch` covers). -/
def DataRow : Lir → Prop
  | .branchZero .. | .jump _ | .label _ | .call _ | .enter _ | .ret => False
  | _ => True

/-! ### Word lemmas -/

theorem toS_enc (k : Kind) (v : Int) (h : InRange k v) (hk : k ≠ .bool) :
    (k.sign = .s → toS (enc v) = v) ∧ (k.sign = .u → enc v = v) := by
  cases k <;> simp only [InRange, Kind.lo, Kind.hi] at h <;>
    simp_all [Kind.sign, toS, enc, W, H] <;> first | omega | (split <;> omega)

theorem enc_upd (s : Store) (m : Mach) (h : Rel s m) (l : Loc) (v w : Int)
    (hw : w = enc v) : Rel (upd s l v) { m with mem := upd m.mem l w } := by
  intro l'
  by_cases hl : l' = l <;> simp [upd, hl, hw, h l']

theorem enc_idem (v : Int) : enc (enc v) = enc v := by
  unfold enc W; omega

/-- A product overflows the signed range iff its low word, read signed,
differs from it. -/
theorem toS_mod_eq (p : Int) : toS (p % W) = p ↔ InRange .i64 p := by
  simp only [toS, InRange, Kind.lo, Kind.hi, W, H]; split <;> omega

theorem tdiv_fits_i64 (x y : Int) (hx : InRange .i64 x) (hy : y ≠ 0) :
    InRange .i64 (x.tdiv y) ↔ ¬ (x = -H ∧ y = -1) := by
  simp only [InRange, Kind.lo, Kind.hi, H] at *
  constructor
  · intro h ⟨h1, h2⟩
    subst h1; subst h2
    have : Int.tdiv (-9223372036854775808) (-1) = 9223372036854775808 := by decide
    omega
  · intro hn
    have hle := Int.natAbs_tdiv_le_natAbs x y
    by_cases hxa : x.natAbs < 9223372036854775808
    · omega
    · have hxm : x = -9223372036854775808 := by omega
      by_cases hy1 : y = 1
      · subst hy1; simp [Int.tdiv_one]; omega
      · have hyn : y ≠ -1 := fun h => hn ⟨hxm, h⟩
        have hq := Int.natAbs_tdiv x y
        have hb : 1 < y.natAbs := by omega
        have hlt := @Nat.div_lt_self x.natAbs y.natAbs (by omega) hb
        have : (x.tdiv y).natAbs < 9223372036854775808 := by
          rw [hq]; show x.natAbs / y.natAbs < _; omega
        omega

theorem tdiv_fits_u (k : Kind) (x y : Int) (hk : k.sign = .u) (hb : k ≠ .bool)
    (hx : InRange k x) (hy : InRange k y) : InRange k (x.tdiv y) := by
  have hle := Int.natAbs_tdiv_le_natAbs x y
  cases k <;> simp only [InRange, Kind.lo, Kind.hi, Kind.sign] at * <;>
    first
    | contradiction
    | (have hn := Int.tdiv_nonneg (a := x) (b := y) (by omega) (by omega); omega)

theorem tdiv_nonneg_eq {x : Int} (y : Int) (h : 0 ≤ x) : x.tdiv y = x / y := by
  simp [Int.tdiv_eq_ediv, h]
theorem tmod_nonneg_eq {x : Int} (y : Int) (h : 0 ≤ x) : x.tmod y = x % y := by
  simp [Int.tmod_eq_emod, h]

theorem tmod_neg_one (x : Int) : x.tmod (-1) = 0 := by simp [Int.tmod_eq_emod]

theorem toS_mod_i64 (x : Int) (h : -9223372036854775808 ≤ x ∧ x ≤ 9223372036854775807) :
    toS (x % 18446744073709551616) = x := by
  simp only [toS, H, W]; split <;> omega

/-! ### Row theorems -/

section Rows
variable (s : Store) (m : Mach) (hR : Rel s m)
include hR

theorem sim_const (d : Nat) (v : Int) :
    Sim (lirStep (.const d v) s) (run (select (.const d v)) m) := by
  simp only [lirStep, select, run, Mach.step, setReg, Sim]
  exact enc_upd s _ hR _ _ _ (by simp)

theorem sim_move (d a : Nat) :
    Sim (lirStep (.move d a) s) (run (select (.move d a)) m) := by
  simp only [lirStep, select, run, Mach.step, setReg, Sim]
  exact enc_upd s _ hR _ _ _ (by simp [hR (.slot a)])

theorem sim_storeOut (i a : Nat) :
    Sim (lirStep (.storeOut i a) s) (run (select (.storeOut i a)) m) := by
  simp only [lirStep, select, run, Mach.step, setReg, Sim]
  exact enc_upd s _ hR _ _ _ (by simp [hR (.slot a)])

theorem sim_loadOut (d i : Nat) :
    Sim (lirStep (.loadOut d i) s) (run (select (.loadOut d i)) m) := by
  simp only [lirStep, select, run, Mach.step, setReg, Sim]
  exact enc_upd s _ hR _ _ _ (by simp [hR (.out i)])

theorem sim_loadIn (d i : Nat) :
    Sim (lirStep (.loadIn d i) s) (run (select (.loadIn d i)) m) := by
  simp only [lirStep, select, run, Mach.step, setReg, Sim]
  exact enc_upd s _ hR _ _ _ (by simp [hR (.inn i)])

theorem sim_storeIn (i a : Nat) :
    Sim (lirStep (.storeIn i a) s) (run (select (.storeIn i a)) m) := by
  simp only [lirStep, select, run, Mach.step, setReg, Sim]
  exact enc_upd s _ hR _ _ _ (by simp [hR (.slot a)])

omit hR in
theorem sim_trap (f : Failure) :
    Sim (lirStep (.trap f) s) (run (select (.trap f)) m) := by
  simp [lirStep, select, run, Mach.step, Sim]

theorem rel_st (l : Loc) (v w : Int) (r : Reg → Int) (hw : w = enc v) :
    Rel (upd s l v) { mem := upd m.mem l w, reg := r } := by
  intro l'
  by_cases hl : l' = l <;> simp [upd, hl, hw, hR l']

set_option maxHeartbeats 4000000 in
theorem sim_arith (op : ArithOp) (k : Kind) (d a b : Nat) (ht : Typed (.arith op k d a b) s) :
    Sim (lirStep (.arith op k d a b) s) (run (select (.arith op k d a b)) m) := by
  obtain ⟨hk, ha, hb⟩ := ht
  have hma := hR (.slot a)
  have hmb := hR (.slot b)
  simp only [lirStep, checkedArith, select, binary, rangeCheck, Kind.mask, Kind.sign, run,
    List.cons_append, List.nil_append]
  generalize s (.slot a) = x at *
  generalize s (.slot b) = y at *
  have hrem : y ≠ -1 ∨ x.tmod y = 0 := by
    by_cases h : y = -1
    · exact .inr (h ▸ tmod_neg_one x)
    · exact .inl h
  cases op <;> cases k
  all_goals try contradiction
  all_goals try have hdiv := tdiv_fits_i64 x y ha
  all_goals try have hdu := tdiv_fits_u _ x y rfl (by decide) ha hb
  all_goals simp only [InRange, Kind.lo, Kind.hi, H] at ha hb
  all_goals try simp only [InRange, Kind.lo, Kind.hi, H] at hdiv
  all_goals simp [Mach.step, run, setReg, hma, hmb, fits, neg?, enc, W, H, Kind.lo, Kind.hi]
  all_goals try simp only [toS_mod_i64 x ha, toS_mod_i64 y hb]
  all_goals try (
    have hx0 : 0 ≤ x := by omega
    have hxW : x % 18446744073709551616 = x := by omega
    have hyW : y % 18446744073709551616 = y := by omega
    have hr : y = 0 ∨ (0 ≤ x % y ∧ x % y < y) := by
      by_cases h : y = 0
      · exact .inl h
      · exact .inr ⟨Int.emod_nonneg _ h, Int.emod_lt_of_pos _ (by omega)⟩
    simp (config := { failIfUnchanged := false }) only
      [hxW, hyW, tdiv_nonneg_eq _ hx0, tmod_nonneg_eq _ hx0, InRange, Kind.lo, Kind.hi] at hdu ⊢)
  all_goals try generalize x * y = p at *
  all_goals try simp only [toS, H, W]
  all_goals repeat' split
  all_goals first
    | (exfalso; omega)
    | (simp only [Sim]; exact rel_st s m hR _ _ _ _ (by simp only [enc, W] <;> omega))
    | (simp only [Sim]; done)

theorem sim_wrapping (op : WrapOp) (k : Kind) (d a b : Nat)
    (ht : Typed (.wrapping op k d a b) s) :
    Sim (lirStep (.wrapping op k d a b) s) (run (select (.wrapping op k d a b)) m) := by
  obtain ⟨hk, ha, hb⟩ := ht
  have hma := hR (.slot a)
  have hmb := hR (.slot b)
  simp only [lirStep, select, binary, Kind.mask, run, List.cons_append, List.nil_append]
  generalize s (.slot a) = x at *
  generalize s (.slot b) = y at *
  have hm : (x % 18446744073709551616) * (y % 18446744073709551616) % 18446744073709551616
      = (x * y) % 18446744073709551616 := (Int.mul_emod x y _).symm
  cases op <;> cases k
  all_goals try contradiction
  all_goals simp only [InRange, Kind.lo, Kind.hi, H] at ha hb
  all_goals simp [Mach.step, run, setReg, hma, hmb, enc, W, wrapInto, wrapOp, andMask,
    Kind.modulus]
  all_goals try (
    have hxW : x % 18446744073709551616 = x := by omega
    have hyW : y % 18446744073709551616 = y := by omega
    simp (config := { failIfUnchanged := false }) only [hxW, hyW] at hm ⊢)
  all_goals try generalize x % 18446744073709551616 * (y % 18446744073709551616) = q at *
  all_goals try generalize x * y = p at *
  all_goals try simp only [toS, H, W]
  all_goals repeat' split
  all_goals first
    | (exfalso; omega)
    | (simp only [Sim]; exact rel_st s m hR _ _ _ _ (by simp only [enc, W] <;> omega))
    | (simp only [Sim]; done)

theorem sim_neg (k : Kind) (d a : Nat) (ht : Typed (.neg k d a) s) :
    Sim (lirStep (.neg k d a) s) (run (select (.neg k d a)) m) := by
  obtain ⟨hk, ha⟩ := ht
  have hma := hR (.slot a)
  simp only [lirStep, select, run]
  generalize s (.slot a) = x at *
  cases k
  all_goals try contradiction
  all_goals simp only [InRange, Kind.lo, Kind.hi, H] at ha
  all_goals simp [Mach.step, setReg, hma, fits, enc, W, H, Kind.lo, Kind.hi, Kind.sign]
  all_goals repeat' split
  all_goals first
    | (exfalso; omega)
    | (simp only [Sim]; exact rel_st s m hR _ _ _ _ (by simp only [enc, W] <;> omega))
    | (simp only [Sim]; done)

theorem sim_not (d a : Nat) (ht : Typed (.not d a) s) :
    Sim (lirStep (.not d a) s) (run (select (.not d a)) m) := by
  have hma := hR (.slot a)
  simp only [Typed, InRange, Kind.lo, Kind.hi] at ht
  simp only [lirStep, select, run]
  generalize s (.slot a) = x at *
  simp [Mach.step, setReg, hma, enc, W]
  repeat' split
  all_goals first
    | (exfalso; omega)
    | (simp only [Sim]; exact rel_st s m hR _ _ _ _ (by simp only [enc, W] <;> omega))

theorem sim_compare (c : Cond) (k : Kind) (d a b : Nat) (ht : Typed (.compare c k d a b) s) :
    Sim (lirStep (.compare c k d a b) s) (run (select (.compare c k d a b)) m) := by
  obtain ⟨ha, hb⟩ := ht
  have hma := hR (.slot a)
  have hmb := hR (.slot b)
  simp only [lirStep, select, binary, run, List.cons_append, List.nil_append]
  generalize s (.slot a) = x at *
  generalize s (.slot b) = y at *
  cases k
  all_goals simp only [InRange, Kind.lo, Kind.hi, H] at ha hb
  all_goals simp [Mach.step, setReg, hma, hmb, enc, W, Kind.sign]
  all_goals try simp only [toS_mod_i64 x ha, toS_mod_i64 y hb]
  all_goals try (
    have hxW : x % 18446744073709551616 = x := by omega
    have hyW : y % 18446744073709551616 = y := by omega
    simp (config := { failIfUnchanged := false }) only [hxW, hyW])
  all_goals simp only [Sim]
  all_goals exact rel_st s m hR _ _ _ _ (by simp only [enc, W]; split <;> omega)

end Rows

/-! ### The table -/

/-- Forward simulation for every data row of the instruction-selection
table: related states stay related, and failures agree. -/
theorem select_simulates (r : Lir) (s : Store) (m : Mach) (hR : Rel s m) (hd : DataRow r)
    (ht : Typed r s) : Sim (lirStep r s) (run (select r) m) := by
  cases r with
  | const d v => exact sim_const s m hR d v
  | move d a => exact sim_move s m hR d a
  | arith op k d a b => exact sim_arith s m hR op k d a b ht
  | wrapping op k d a b => exact sim_wrapping s m hR op k d a b ht
  | neg k d a => exact sim_neg s m hR k d a ht
  | not d a => exact sim_not s m hR d a ht
  | compare c k d a b => exact sim_compare s m hR c k d a b ht
  | storeOut i a => exact sim_storeOut s m hR i a
  | loadOut d i => exact sim_loadOut s m hR d i
  | loadIn d i => exact sim_loadIn s m hR d i
  | storeIn i a => exact sim_storeIn s m hR i a
  | trap f => exact sim_trap s m f
  | branchZero => simp [DataRow] at hd
  | jump => simp [DataRow] at hd
  | label => simp [DataRow] at hd
  | call => simp [DataRow] at hd
  | enter => simp [DataRow] at hd
  | ret => simp [DataRow] at hd

/-- The branch row loads the condition and branches when it is zero, which
for a related state is exactly when the slot IR boolean is `false`. -/
theorem select_branch (c t : Nat) (s : Store) (m : Mach) (hR : Rel s m)
    (hb : InRange .bool (s (.slot c))) :
    select (.branchZero c t) = [.ld .r0 (.slot c), .bz t] ∧
      ∃ m', Mach.step (.ld .r0 (.slot c)) m = .ok m' ∧ Rel s m' ∧
        (m'.reg .r0 = 0 ↔ s (.slot c) = 0) := by
  refine ⟨rfl, setReg m .r0 (m.mem (.slot c)), rfl, fun l => hR l, ?_⟩
  simp only [InRange, Kind.lo, Kind.hi] at hb
  simp [setReg, hR (.slot c), enc, W]
  omega

/-- Control rows select exactly one control instruction. -/
theorem select_control (l n : Nat) (f : Failure) :
    select (.jump l) = [.jmp l] ∧ select (.label l) = [.label l] ∧
      select (.call l) = [.call l] ∧ select (.enter n) = [.enter n] ∧
      select .ret = [.leave] ∧ select (.trap f) = [.trap f] :=
  ⟨rfl, rfl, rfl, rfl, rfl, rfl⟩

/-! ## The printed table (compared with `table_listing()` by the tests) -/

def Kind.render : Kind → String
  | .u8 => "u8" | .u32 => "u32" | .u64 => "u64" | .i64 => "i64" | .bool => "bool"
def ArithOp.render : ArithOp → String
  | .add => "add" | .sub => "sub" | .mul => "mul" | .div => "div" | .rem => "rem"
def WrapOp.render : WrapOp → String
  | .add => "add" | .sub => "sub" | .mul => "mul"
def Cond.render : Cond → String
  | .eq => "eq" | .ne => "ne" | .lt => "lt" | .le => "le" | .gt => "gt" | .ge => "ge"
def Sign.render : Sign → String
  | .s => "s" | .u => "u"
def Reg.render : Reg → String
  | .r0 => "r0" | .r1 => "r1" | .r2 => "r2"
def Loc.render : Loc → String
  | .slot n => s!"s{n}" | .out n => s!"o{n}" | .inn n => s!"i{n}"
def Failure.render : Failure → String
  | .overflow => "ovf" | .divZero => "div0" | .unreachable => "unreachable" | .fault => "fault"

def M.render : M → String
  | .ld r l => s!"ld {r.render} {l.render}"
  | .st l r => s!"st {l.render} {r.render}"
  | .imm r w => s!"li {r.render} {w}"
  | .addchk sg => s!"addchk {sg.render}"
  | .subchk sg => s!"subchk {sg.render}"
  | .mulchk sg => s!"mulchk {sg.render}"
  | .wrap op => s!"wrap {op.render}"
  | .andr2 => "andr2"
  | .trapgtu => "trapgtu"
  | .trapzero => "trapzero"
  | .trapdivovf => "trapdivovf"
  | .div sg => s!"div {sg.render}"
  | .rem sg => s!"rem {sg.render}"
  | .negchk sg => s!"negchk {sg.render}"
  | .xorone => "xorone"
  | .cmp c sg => s!"cmp {c.render} {sg.render}"
  | .label l => s!"label L{l}"
  | .jmp l => s!"jmp L{l}"
  | .bz l => s!"bz L{l}"
  | .call l => s!"call L{l}"
  | .enter n => s!"enter {n}"
  | .leave => "leave"
  | .trap f => s!"trap {f.render}"

def integerKinds : List Kind := [.u8, .u32, .u64, .i64]

/-- Every row kind at fixed slots (`d = 0`, `a = 1`, `b = 2`), in the order of
`table_rows()` in `machine.rs`. -/
def tableRows : List (String × Lir) :=
  [("const", .const 0 7), ("move", .move 0 1)] ++
  ([ArithOp.add, .sub, .mul, .div, .rem].flatMap fun op => integerKinds.map fun k =>
    (s!"arith {op.render} {k.render}", .arith op k 0 1 2)) ++
  ([WrapOp.add, .sub, .mul].flatMap fun op => integerKinds.map fun k =>
    (s!"wrapping {op.render} {k.render}", .wrapping op k 0 1 2)) ++
  (integerKinds.map fun k => (s!"neg {k.render}", .neg k 0 1)) ++
  [("not", .not 0 1)] ++
  ([Cond.eq, .ne, .lt, .le, .gt, .ge].flatMap fun c =>
    [Kind.u8, .u32, .u64, .i64, .bool].map fun k =>
      (s!"compare {c.render} {k.render}", .compare c k 0 1 2)) ++
  [("storeout", .storeOut 3 1), ("loadout", .loadOut 0 3), ("loadin", .loadIn 0 3),
   ("storein", .storeIn 3 1), ("branchzero", .branchZero 1 9), ("jump", .jump 9),
   ("label", .label 9), ("call", .call 9), ("enter", .enter 4), ("return", .ret),
   ("trap ovf", .trap .overflow), ("trap div0", .trap .divZero),
   ("trap unreachable", .trap .unreachable)]

def listing : String :=
  String.join (tableRows.map fun (name, r) =>
    name ++ ": " ++ "; ".intercalate ((select r).map M.render) ++ "\n")

#eval IO.print ("BEGIN TABLE\n" ++ listing ++ "END TABLE\n")

#print axioms Vesper.VerifiedCodegen.enc_lt
#print axioms Vesper.VerifiedCodegen.toS_enc
#print axioms Vesper.VerifiedCodegen.enc_upd
#print axioms Vesper.VerifiedCodegen.enc_idem
#print axioms Vesper.VerifiedCodegen.toS_mod_eq
#print axioms Vesper.VerifiedCodegen.tdiv_fits_i64
#print axioms Vesper.VerifiedCodegen.tdiv_fits_u
#print axioms Vesper.VerifiedCodegen.tdiv_nonneg_eq
#print axioms Vesper.VerifiedCodegen.tmod_nonneg_eq
#print axioms Vesper.VerifiedCodegen.tmod_neg_one
#print axioms Vesper.VerifiedCodegen.toS_mod_i64
#print axioms Vesper.VerifiedCodegen.sim_const
#print axioms Vesper.VerifiedCodegen.sim_move
#print axioms Vesper.VerifiedCodegen.sim_storeOut
#print axioms Vesper.VerifiedCodegen.sim_loadOut
#print axioms Vesper.VerifiedCodegen.sim_loadIn
#print axioms Vesper.VerifiedCodegen.sim_storeIn
#print axioms Vesper.VerifiedCodegen.sim_trap
#print axioms Vesper.VerifiedCodegen.rel_st
#print axioms Vesper.VerifiedCodegen.sim_arith
#print axioms Vesper.VerifiedCodegen.sim_wrapping
#print axioms Vesper.VerifiedCodegen.sim_neg
#print axioms Vesper.VerifiedCodegen.sim_not
#print axioms Vesper.VerifiedCodegen.sim_compare
#print axioms Vesper.VerifiedCodegen.select_simulates
#print axioms Vesper.VerifiedCodegen.select_branch
#print axioms Vesper.VerifiedCodegen.select_control
#print axioms Vesper.VerifiedCodegen.Res.bind_ok
#print axioms Vesper.VerifiedCodegen.Res.bind_abort
#print axioms Vesper.VerifiedCodegen.Res.bind_ite

end Vesper.VerifiedCodegen
