/-
  Vesper Vault (design §23.4): two machine-checked statements about MODELS.

  1. `verify_tag_16_ct_sound`: the branchless 128-bit tag comparison
     `VaultSpec.verifyTag16Ct` (xor the two 64-bit halves, or them, then the
     branchless zero test `((x ||| -x) >>> 63) == 0`) equals equality on all
     2^256 input pairs. Proved by `bv_decide` (SAT bit-blasting). It mirrors
     `vesper_vault::verify_tag_16_ct` and `SecU64::ct_is_zero`, but it is a
     theorem about this Lean definition, not about the Rust code or the
     machine code compiled from it.

  2. `trace_independent`: non-interference for a small register machine
     with an explicit `LeakageTrace` semantics. Every step emits the program
     counter, any memory address, the operands of a variable-latency divide
     and the condition of a branch. A program accepted by `WellTyped` (the
     same rules the Cranelift verifier in crates/vesper-vault enforces: no
     secret branch condition, no secret address, no secret divide operand,
     taint propagated through ALU and select) produces identical traces from
     any two states that agree on public registers and public memory. The
     theorem is about this modelled machine, not about Cranelift IR or the
     Rust verifier, and it excludes declassification.

  `tagKernel` is the tag comparison written for the machine; `tagKernel_ct`
  instantiates the theorem for it.
-/
import Std.Tactic.BVDecide

namespace VaultSpec

/-- Branchless 128-bit tag comparison, as in `verify_tag_16_ct`. -/
def verifyTag16Ct (expected computed : BitVec 128) : Bool :=
  let d := expected ^^^ computed
  let lo : BitVec 64 := d.extractLsb' 0 64
  let hi : BitVec 64 := d.extractLsb' 64 64
  let c := lo ||| hi
  ((c ||| (0 - c)) >>> 63) == 0

theorem verify_tag_16_ct_sound (expected computed : BitVec 128) :
    verifyTag16Ct expected computed = decide (expected = computed) := by
  unfold verifyTag16Ct
  bv_decide

end VaultSpec

namespace VaultCt

abbrev Reg := Nat
abbrev Word := BitVec 64

inductive AluOp where
  | xor | or | and | add | sub | mul | shl | lshr
  deriving DecidableEq

def AluOp.eval : AluOp → Word → Word → Word
  | .xor, a, b => a ^^^ b
  | .or, a, b => a ||| b
  | .and, a, b => a &&& b
  | .add, a, b => a + b
  | .sub, a, b => a - b
  | .mul, a, b => a * b
  | .shl, a, b => a <<< b
  | .lshr, a, b => a >>> b

inductive Instr where
  /-- `dst := op a b` -/
  | alu (op : AluOp) (dst a b : Reg)
  /-- `dst := const` -/
  | const (dst : Reg) (v : Word)
  /-- `dst := mask & a | ~mask & b` (branchless select) -/
  | sel (dst m a b : Reg)
  /-- `dst := mem[addr]`; `secretMem` picks the secret or public memory. -/
  | load (dst addr : Reg) (secretMem : Bool)
  /-- `mem[addr] := src` -/
  | store (addr src : Reg) (secretMem : Bool)
  /-- variable-latency division `dst := a / b` -/
  | div (dst a b : Reg)
  /-- skip the next `n` instructions when `c ≠ 0` -/
  | brif (c : Reg) (n : Nat)

structure State where
  regs : Reg → Word
  pubMem : Nat → Word
  secMem : Nat → Word

/-- What an attacker observing timing and addresses sees for one step. -/
inductive Event where
  | pc (pc : Nat)
  | addr (pc : Nat) (a : Nat)
  | divOperands (pc : Nat) (a b : Word)
  | branch (pc : Nat) (taken : Bool)
  deriving DecidableEq

def update (f : Reg → Word) (r : Reg) (v : Word) : Reg → Word :=
  fun x => if x = r then v else f x

def updMem (f : Nat → Word) (a : Nat) (v : Word) : Nat → Word :=
  fun x => if x = a then v else f x

/-- One step: new state, leaked event, next pc. -/
def step (i : Instr) (pc : Nat) (s : State) : State × Event × Nat :=
  match i with
  | .alu op d a b => ({ s with regs := update s.regs d (op.eval (s.regs a) (s.regs b)) }, .pc pc, pc + 1)
  | .const d v => ({ s with regs := update s.regs d v }, .pc pc, pc + 1)
  | .sel d m a b =>
      let mv := s.regs m
      ({ s with regs := update s.regs d ((mv &&& s.regs a) ||| (~~~mv &&& s.regs b)) }, .pc pc, pc + 1)
  | .load d a sm =>
      let ad := (s.regs a).toNat
      let v := if sm then s.secMem ad else s.pubMem ad
      ({ s with regs := update s.regs d v }, .addr pc ad, pc + 1)
  | .store a src sm =>
      let ad := (s.regs a).toNat
      let v := s.regs src
      (if sm then { s with secMem := updMem s.secMem ad v }
       else { s with pubMem := updMem s.pubMem ad v }, .addr pc ad, pc + 1)
  | .div d a b =>
      ({ s with regs := update s.regs d (s.regs a / s.regs b) }, .divOperands pc (s.regs a) (s.regs b), pc + 1)
  | .brif c n =>
      let taken := s.regs c != 0
      (s, .branch pc taken, if taken then pc + 1 + n else pc + 1)

/-- `LeakageTrace`: the events of running `p` from `pc` for `fuel` steps. -/
def LeakageTrace (p : List Instr) : Nat → Nat → State → List Event
  | 0, _, _ => []
  | fuel + 1, pc, s =>
      match p[pc]? with
      | none => []
      | some i =>
          let (s', ev, pc') := step i pc s
          ev :: LeakageTrace p fuel pc' s'

/-- A flow-insensitive secrecy labelling `Γ r = true` (secret) accepted by
the same rules the Cranelift verifier enforces. -/
def InstrOk (Γ : Reg → Bool) : Instr → Prop
  | .alu _ d a b => (Γ a || Γ b) = true → Γ d = true
  | .const _ _ => True
  | .sel d m a b => (Γ m || Γ a || Γ b) = true → Γ d = true
  | .load d a sm => Γ a = false ∧ (sm = true → Γ d = true)
  | .store a src sm => Γ a = false ∧ (sm = false → Γ src = false)
  | .div _ a b => Γ a = false ∧ Γ b = false
  | .brif c _ => Γ c = false

def WellTyped (Γ : Reg → Bool) (p : List Instr) : Prop :=
  ∀ i ∈ p, InstrOk Γ i

/-- Two states agree on everything public. -/
def LowEq (Γ : Reg → Bool) (s₁ s₂ : State) : Prop :=
  (∀ r, Γ r = false → s₁.regs r = s₂.regs r) ∧ s₁.pubMem = s₂.pubMem

theorem lowEq_update (Γ : Reg → Bool) {f g : Reg → Word} {d : Reg} {v w : Word}
    (h : ∀ r, Γ r = false → f r = g r) (hv : Γ d = false → v = w) :
    ∀ r, Γ r = false → update f d v r = update g d w r := by
  intro r hr
  unfold update
  by_cases hrd : r = d
  · subst hrd; simp [hv hr]
  · simp [hrd, h r hr]

theorem step_low (Γ : Reg → Bool) (i : Instr) (pc : Nat) (s₁ s₂ : State)
    (hi : InstrOk Γ i) (h : LowEq Γ s₁ s₂) :
    (step i pc s₁).2 = (step i pc s₂).2 ∧
    LowEq Γ (step i pc s₁).1 (step i pc s₂).1 := by
  obtain ⟨hr, hm⟩ := h
  cases i with
  | alu op d a b =>
    simp only [InstrOk] at hi
    refine ⟨rfl, ?_, hm⟩
    apply lowEq_update Γ hr
    intro hd
    have ha : Γ a = false := by
      cases h' : Γ a <;> simp_all
    have hb : Γ b = false := by
      cases h' : Γ b <;> simp_all
    simp [hr a ha, hr b hb]
  | const d v =>
    exact ⟨rfl, lowEq_update Γ hr (fun _ => rfl), hm⟩
  | sel d m a b =>
    simp only [InstrOk] at hi
    refine ⟨rfl, ?_, hm⟩
    apply lowEq_update Γ hr
    intro hd
    have hm' : Γ m = false := by cases h' : Γ m <;> simp_all
    have ha : Γ a = false := by cases h' : Γ a <;> simp_all
    have hb : Γ b = false := by cases h' : Γ b <;> simp_all
    simp [hr m hm', hr a ha, hr b hb]
  | load d a sm =>
    obtain ⟨ha, hd⟩ := hi
    simp only [step, hr a ha]
    refine ⟨trivial, ?_, hm⟩
    apply lowEq_update Γ hr
    intro hdf
    cases sm with
    | true => simp_all
    | false => simp [hm]
  | store a src sm =>
    obtain ⟨ha, hs⟩ := hi
    cases sm with
    | true =>
      simp only [step, hr a ha]
      exact ⟨trivial, hr, hm⟩
    | false =>
      simp only [step, hr a ha, hr src (hs rfl)]
      refine ⟨trivial, hr, ?_⟩
      simp [hm]
  | div d a b =>
    obtain ⟨ha, hb⟩ := hi
    simp only [step, hr a ha, hr b hb]
    exact ⟨trivial, lowEq_update Γ hr (fun _ => rfl), hm⟩
  | brif c n =>
    simp only [InstrOk] at hi
    simp only [step, hr c hi]
    exact ⟨trivial, hr, hm⟩

/-- Non-interference: a well-typed program leaks the same trace from any two
low-equivalent states. -/
theorem trace_independent (Γ : Reg → Bool) (p : List Instr) (hp : WellTyped Γ p) :
    ∀ (fuel pc : Nat) (s₁ s₂ : State), LowEq Γ s₁ s₂ →
      LeakageTrace p fuel pc s₁ = LeakageTrace p fuel pc s₂ := by
  intro fuel
  induction fuel with
  | zero => intro _ _ _ _; rfl
  | succ n ih =>
    intro pc s₁ s₂ h
    simp only [LeakageTrace]
    cases hget : p[pc]? with
    | none => rfl
    | some i =>
      have hi : InstrOk Γ i := hp i (List.mem_of_getElem? hget)
      obtain ⟨hev, hlow⟩ := step_low Γ i pc s₁ s₂ hi h
      have hpc : (step i pc s₁).2.2 = (step i pc s₂).2.2 := by
        rw [hev]
      have hev1 : (step i pc s₁).2.1 = (step i pc s₂).2.1 := by
        rw [hev]
      simp only
      rw [hev1, hpc, ih _ _ _ hlow]

/-- The tag comparison on the modelled machine: registers 0 and 1 hold the
public buffer addresses; the tags live in secret memory. -/
def tagKernel : List Instr :=
  [ .const 10 8,
    .alu .add 11 0 10,
    .alu .add 12 1 10,
    .load 2 0 true,    -- expected lo
    .load 3 11 true,   -- expected hi
    .load 4 1 true,    -- computed lo
    .load 5 12 true,   -- computed hi
    .alu .xor 6 2 4,
    .alu .xor 7 3 5,
    .alu .or 8 6 7,
    .const 13 0,
    .alu .sub 9 13 8,
    .alu .or 9 9 8,
    .const 14 63,
    .alu .lshr 9 9 14 ]

/-- Registers 2–9 carry secrets; addresses and constants are public. -/
def tagΓ (r : Reg) : Bool := decide (2 ≤ r ∧ r ≤ 9)

theorem tagKernel_wellTyped : WellTyped tagΓ tagKernel := by
  intro i hi
  simp [tagKernel] at hi
  rcases hi with h | h | h | h | h | h | h | h | h | h | h | h | h | h | h <;>
    subst h <;> simp [InstrOk, tagΓ]

theorem tagKernel_ct (fuel : Nat) (s₁ s₂ : State) (h : LowEq tagΓ s₁ s₂) :
    LeakageTrace tagKernel fuel 0 s₁ = LeakageTrace tagKernel fuel 0 s₂ :=
  trace_independent tagΓ tagKernel tagKernel_wellTyped fuel 0 s₁ s₂ h

end VaultCt
