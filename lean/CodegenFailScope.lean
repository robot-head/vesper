import CodegenControlFlow

/-!
# Trap scopes: caught `overflow` and `divZero` aborts (Milestone 7)

Abort-only `Fail[E]` is compiled as a structured early exit. Explicit raises,
`?` and the status test after a call are built from existing slot-IR rows
(`const`, `compare`, `branchZero`, `jump`, `storeIn`, `loadOut`, `ret`), whose
simulation `CodegenControlFlow` already proves. The one new row is
`.scope ovf div0`: from there on, in emission order, a checked instruction
that would abort with `overflow` (`divZero`) jumps to label `ovf` (`div0`)
instead, unless the label is the program-wide abort exit (`0`, `1`: the
Rust `TrapKind::label()`s), which keeps the abort.

This file extends the control-flow model of `CodegenControlFlow` with that
scope and proves that the selected code still simulates the slot IR,
including the caught case: the source row fails and control moves to the
handler label with the store the row started from, and the target block
branches to the same label with a machine state that is related to that same
store. The key fact is `abort_preserves_memory`: every selected row writes
memory only in its trailing stores, which cannot abort, so at the instruction
that aborts nothing has been written yet.

The scope in effect at a row is the last `.scope` row before it in program
order (`scopeAt`), the way the assembler applies `M.scope` during its single
linear pass; it does not depend on how control reached the row.

Not modelled: the Rust lowering (which rows it emits for a raise, a status
test or a handler), the call convention that carries the status word, byte
encodings and hardware. Those are covered by differential tests.
-/
namespace Vesper.CodegenFailScope
open Vesper.VerifiedCodegen
open Vesper.CodegenControlFlow

/-! ## Scopes -/

/-- The program-wide abort exits (`TrapKind::Overflow.label()`,
`TrapKind::DivisionByZero.label()`). -/
def ovfExit : Nat := 0
def div0Exit : Nat := 1

structure Scope where
  ovf : Nat
  div0 : Nat
  deriving DecidableEq

def Scope.default : Scope := ⟨ovfExit, div0Exit⟩

/-- The handler label a failure jumps to, or `none` when it aborts. -/
def Scope.target (sc : Scope) : Failure → Option Nat
  | .overflow => if sc.ovf = ovfExit then none else some sc.ovf
  | .divZero => if sc.div0 = div0Exit then none else some sc.div0
  | _ => none

theorem default_catches_nothing (f : Failure) : Scope.default.target f = none := by
  cases f <;> simp [Scope.target, Scope.default]

/-- Hardware faults and unreachable code are never caught. -/
theorem fault_not_caught (sc : Scope) : sc.target .fault = none ∧ sc.target .unreachable = none :=
  ⟨rfl, rfl⟩

/-- The scope in effect at row `pc`: set by the last `scope` row before it. -/
def scopeAt (rows : Nat → Option Lir) : Nat → Scope
  | 0 => .default
  | pc + 1 =>
    match rows pc with
    | some (.scope o d) => ⟨o, d⟩
    | _ => scopeAt rows pc

/-! ## Machine blocks that keep the state at an abort -/

inductive Exit (α : Type)
  | ok (a : α)
  | abort (f : Failure) (m : Mach)

def Exit.toRes {α : Type} : Exit α → Res α
  | .ok a => .ok a
  | .abort f _ => .abort f

/-- `runBlock`, but an abort also returns the machine state the aborting
instruction started from (the state its branch to the handler sees). -/
def runBlockX : List M → Mach → Exit (Control × Mach)
  | [], m => .ok (.fall, m)
  | .jmp label :: _, m => .ok (.jump label, m)
  | .bz label :: rest, m =>
      if m.reg .r0 = 0 then .ok (.jump label, m) else runBlockX rest m
  | .leave :: _, m => .ok (.halt, m)
  | .call _ :: _, m | .enter _ :: _, m => .abort .fault m
  | instruction :: rest, m =>
      match Mach.step instruction m with
      | .ok next => runBlockX rest next
      | .abort f => .abort f m

theorem runBlockX_toRes (code : List M) (m : Mach) :
    (runBlockX code m).toRes = runBlock code m := by
  induction code generalizing m with
  | nil => rfl
  | cons i rest ih =>
    cases i
    all_goals simp only [runBlockX, runBlock]
    all_goals try rfl
    all_goals try (split <;> simp_all [Exit.toRes])
    all_goals (cases h : Mach.step _ m <;> simp [Res.bind, Exit.toRes, ih])

def isSt : M → Bool
  | .st _ _ => true
  | _ => false

/-- Memory is written only by a trailing run of stores. -/
def storesLast : List M → Bool
  | [] => true
  | i :: rest => if isSt i then rest.all isSt else storesLast rest

theorem step_memory (i : M) (m next : Mach) (h : Mach.step i m = .ok next)
    (hst : isSt i = false) : next.mem = m.mem := by
  cases i
  all_goals simp only [isSt] at hst
  case st => contradiction
  all_goals simp only [Mach.step] at h
  all_goals repeat' (first | contradiction | (injection h with h; subst h; rfl) | split at h)

theorem stores_never_abort (code : List M) (m : Mach) (h : code.all isSt = true)
    (f : Failure) (m' : Mach) : runBlockX code m ≠ .abort f m' := by
  induction code generalizing m with
  | nil => simp [runBlockX]
  | cons i rest ih =>
    simp only [List.all_cons, Bool.and_eq_true] at h
    obtain ⟨head, tail⟩ := h
    cases i <;> simp only [isSt] at head
    all_goals try contradiction
    simp only [runBlockX, Mach.step]
    exact ih _ tail

theorem abort_preserves_memory (code : List M) (m : Mach) (h : storesLast code = true)
    (f : Failure) (m' : Mach) (abort : runBlockX code m = .abort f m') : m'.mem = m.mem := by
  induction code generalizing m with
  | nil => simp [runBlockX] at abort
  | cons i rest ih =>
    by_cases hst : isSt i = true
    · have tail : rest.all isSt = true := by simpa [storesLast, hst] using h
      cases i <;> simp only [isSt] at hst
      all_goals try contradiction
      simp only [runBlockX, Mach.step] at abort
      exact absurd abort (stores_never_abort rest _ tail f m')
    · have tail : storesLast rest = true := by simpa [storesLast, hst] using h
      have hst' : isSt i = false := by simpa using hst
      cases i
      all_goals simp only [runBlockX] at abort
      all_goals try (cases abort <;> rfl)
      all_goals try (split at abort <;> first | cases abort | exact ih m tail abort)
      all_goals
        (cases hs : Mach.step _ m with
          | ok next =>
            rw [hs] at abort
            rw [ih next tail abort]
            exact step_memory _ m next hs (by simpa using hst')
          | abort g =>
            rw [hs] at abort
            cases abort; rfl)

theorem select_storesLast (row : Lir) : storesLast (select row) = true := by
  cases row
  case arith op k d a b =>
    cases op <;> cases k <;> simp [select, binary, rangeCheck, Kind.mask, Kind.sign,
      storesLast, isSt]
  case wrapping op k d a b =>
    cases k <;> simp [select, binary, Kind.mask, storesLast, isSt]
  all_goals simp [select, binary, storesLast, isSt]

/-! ## Rows under a scope -/

/-- A row of the slot IR under scope `sc`: a caught failure jumps to its
handler label with the store the row started from. -/
def sourceRowC (labels : Labels) (sc : Scope) (pc : Nat) (row : Lir) (s : Store) :
    Outcome Store :=
  match sourceRow labels pc row s with
  | .abort f =>
    match sc.target f with
    | some l => jump labels l s
    | none => .abort f
  | other => other

/-- The selected block under scope `sc`: an aborting checked instruction
branches to the scope's label instead. -/
def targetRowC (labels : Labels) (sc : Scope) (pc : Nat) (row : Lir) (m : Mach) :
    Outcome Mach :=
  match runBlockX (select row) m with
  | .abort f m' =>
    match sc.target f with
    | some l => jump labels l m'
    | none => .abort f
  | .ok r => finish labels pc (.ok r)

theorem targetRow_eq (labels : Labels) (pc : Nat) (row : Lir) (m : Mach) :
    targetRow labels pc row m =
      match runBlockX (select row) m with
      | .abort f _ => .abort f
      | .ok r => finish labels pc (.ok r) := by
  unfold targetRow
  rw [← runBlockX_toRes]
  cases runBlockX (select row) m <;> rfl

theorem rel_of_memory (s : Store) (m m' : Mach) (related : Rel s m) (h : m'.mem = m.mem) :
    Rel s m' := by
  intro l; rw [h]; exact related l

/-- Forward simulation of one row under a scope, caught failures included. -/
theorem row_simulatesC (labels : Labels) (sc : Scope) (pc : Nat) (row : Lir)
    (s : Store) (m : Mach) (supported : Supported row) (typed : Operands row s)
    (related : Rel s m) :
    Related (sourceRowC labels sc pc row s) (targetRowC labels sc pc row m) := by
  have sim := row_simulates labels pc row s m supported typed related
  rw [targetRow_eq] at sim
  unfold sourceRowC targetRowC
  cases hx : runBlockX (select row) m with
  | abort f m' =>
    rw [hx] at sim
    dsimp only at sim ⊢
    have mem := abort_preserves_memory _ m (select_storesLast row) f m' hx
    cases hs : sourceRow labels pc row s with
    | abort g =>
      rw [hs] at sim
      have same : g = f := by simpa [Related] using sim
      subst same
      dsimp only
      cases ht : sc.target g with
      | none => simp [Related]
      | some l =>
        simpa using jump_related labels l s m' (rel_of_memory s m m' related mem)
    | next q st => rw [hs] at sim; simp [Related] at sim
    | halt st => rw [hs] at sim; simp [Related] at sim
  | ok r =>
    rw [hx] at sim
    dsimp only at sim ⊢
    cases hs : sourceRow labels pc row s with
    | abort g =>
      rw [hs] at sim
      dsimp only
      -- A non-aborting block only "aborts" through an unresolved jump label.
      cases hf : finish labels pc (.ok r) with
      | abort g' =>
        rw [hf] at sim
        have same : g = g' := by simpa [Related] using sim
        subst same
        have unreachable : g = .unreachable := by
          obtain ⟨c, mr⟩ := r
          cases c <;> simp [finish, jump] at hf
          split at hf <;> simp_all
        subst unreachable
        simp [Scope.target, Related]
      | next q mt => rw [hf] at sim; simp [Related] at sim
      | halt mt => rw [hf] at sim; simp [Related] at sim
    | next q st => rw [hs] at sim; simpa using sim
    | halt st => rw [hs] at sim; simpa using sim

/-! ## Programs -/

def sourceStepC (program : Program) (pc : Nat) (s : Store) : Outcome Store :=
  match program.rows pc with
  | some row => sourceRowC program.labels (scopeAt program.rows pc) pc row s
  | none => .abort .unreachable

def targetStepC (program : Program) (pc : Nat) (m : Mach) : Outcome Mach :=
  match program.rows pc with
  | some row => targetRowC program.labels (scopeAt program.rows pc) pc row m
  | none => .abort .unreachable

structure InvariantC (program : Program) (condition : Nat → Store → Prop) : Prop where
  operands : ∀ pc s row, condition pc s → program.rows pc = some row →
    Supported row ∧ Operands row s
  preserved : ∀ pc s next state, condition pc s →
    sourceStepC program pc s = .next next state → condition next state

theorem step_simulatesC (program : Program) (condition : Nat → Store → Prop)
    (invariant : InvariantC program condition) (pc : Nat) (s : Store) (m : Mach)
    (holds : condition pc s) (related : Rel s m) :
    Related (sourceStepC program pc s) (targetStepC program pc m) := by
  cases row : program.rows pc with
  | none => simp [sourceStepC, targetStepC, row, Related]
  | some instruction =>
    obtain ⟨supported, operands⟩ := invariant.operands pc s instruction holds row
    simpa [sourceStepC, targetStepC, row] using
      row_simulatesC program.labels _ pc instruction s m supported operands related

def runSourceC : Nat → Program → Nat → Store → Outcome Store
  | 0, _, pc, s => .next pc s
  | fuel + 1, program, pc, s =>
      match sourceStepC program pc s with
      | .next next state => runSourceC fuel program next state
      | .halt state => .halt state
      | .abort failure => .abort failure

def runTargetC : Nat → Program → Nat → Mach → Outcome Mach
  | 0, _, pc, m => .next pc m
  | fuel + 1, program, pc, m =>
      match targetStepC program pc m with
      | .next next state => runTargetC fuel program next state
      | .halt state => .halt state
      | .abort failure => .abort failure

/-- Every finite execution prefix agrees under trap scopes: the same rows
run, the same handler labels are reached, and the same failures abort. -/
theorem prefixes_simulateC (program : Program) (condition : Nat → Store → Prop)
    (invariant : InvariantC program condition) (fuel pc : Nat) (s : Store) (m : Mach)
    (holds : condition pc s) (related : Rel s m) :
    Related (runSourceC fuel program pc s) (runTargetC fuel program pc m) := by
  induction fuel generalizing pc s m with
  | zero => exact ⟨rfl, related⟩
  | succ fuel ih =>
    have sim := step_simulatesC program condition invariant pc s m holds related
    simp only [runSourceC, runTargetC]
    cases hs : sourceStepC program pc s with
    | next next state =>
      cases ht : targetStepC program pc m with
      | next target machine =>
        obtain ⟨same, rel⟩ : next = target ∧ Rel state machine := by
          simpa [hs, ht, Related] using sim
        subst target
        exact ih next state machine (invariant.preserved pc s next state holds hs) rel
      | halt machine => simp [hs, ht, Related] at sim
      | abort failure => simp [hs, ht, Related] at sim
    | halt state =>
      cases ht : targetStepC program pc m with
      | halt machine => simpa [hs, ht, Related] using sim
      | next target machine => simp [hs, ht, Related] at sim
      | abort failure => simp [hs, ht, Related] at sim
    | abort failure =>
      cases ht : targetStepC program pc m with
      | abort target => simpa [hs, ht, Related] using sim
      | next target machine => simp [hs, ht, Related] at sim
      | halt machine => simp [hs, ht, Related] at sim

/-- With no `scope` row the scoped semantics is the original one. -/
theorem scopeAt_default (rows : Nat → Option Lir)
    (none : ∀ pc o d, rows pc ≠ some (.scope o d)) (pc : Nat) :
    scopeAt rows pc = .default := by
  induction pc with
  | zero => rfl
  | succ pc ih =>
    simp only [scopeAt]
    split
    · rename_i o d h; exact absurd h (none pc o d)
    · exact ih

theorem sourceRowC_default (labels : Labels) (pc : Nat) (row : Lir) (s : Store) :
    sourceRowC labels .default pc row s = sourceRow labels pc row s := by
  unfold sourceRowC
  cases sourceRow labels pc row s <;> simp [default_catches_nothing]

theorem targetRowC_default (labels : Labels) (pc : Nat) (row : Lir) (m : Mach) :
    targetRowC labels .default pc row m = targetRow labels pc row m := by
  rw [targetRow_eq]
  unfold targetRowC
  cases runBlockX (select row) m <;> simp [default_catches_nothing]

/-! ## Example: `u8` overflow caught by a handler -/

/-- `scope 5 1; 255 + 1 (u8); ret; label 5 (the handler); ret`. -/
def caughtProgram : Program where
  rows := fun pc =>
    match pc with
    | 0 => some (.scope 5 div0Exit)
    | 1 => some (.const 0 255)
    | 2 => some (.const 1 1)
    | 3 => some (.arith .add .u8 2 0 1)
    | 4 => some .ret
    | 5 => some (.label 5)
    | 6 => some .ret
    | _ => none
  labels := fun l => if l = 5 then some 5 else none

/-- The source reaches the handler and halts there. -/
theorem caught_source (s : Store) :
    runSourceC 6 caughtProgram 0 s = .halt (upd (upd s (.slot 0) 255) (.slot 1) 1) := by
  simp [runSourceC, sourceStepC, sourceRowC, sourceRow, caughtProgram, scopeAt, lirStep,
    advance, checkedArith, fits, Kind.lo, Kind.hi, upd, Scope.target, ovfExit, div0Exit,
    jump, Res.bind]

/-- The operand ranges the example's rows rely on, as an invariant. -/
def caughtCondition (pc : Nat) (s : Store) : Prop :=
  (pc = 3 → s (.slot 0) = 255 ∧ s (.slot 1) = 1) ∧ (pc = 2 → s (.slot 0) = 255)

theorem caught_invariant : InvariantC caughtProgram caughtCondition := by
  constructor
  · intro pc s row h fetch
    simp only [caughtProgram] at fetch
    split at fetch <;> simp only [Option.some.injEq, reduceCtorEq] at fetch <;> subst fetch
    all_goals simp [Supported, Operands, Typed]
    obtain ⟨h0, h1⟩ := h.1 rfl
    simp [InRange, Kind.lo, Kind.hi, h0, h1]
  · intro pc s next state h step
    rcases pc with _ | _ | _ | _ | _ | _ | _ | pc
    all_goals simp [sourceStepC, sourceRowC, sourceRow, lirStep, advance, scopeAt, caughtProgram,
      checkedArith, fits, Kind.lo, Kind.hi, Scope.target, ovfExit, div0Exit, jump,
      Res.bind] at step
    all_goals try (obtain ⟨rfl, rfl⟩ := step)
    all_goals simp_all [caughtCondition, upd]

/-- The compiled code takes the same path, and the general theorem gives a
machine state related to the source's at the handler. -/
theorem caught_target (s : Store) (m : Mach) (related : Rel s m) :
    ∃ m', runTargetC 6 caughtProgram 0 m = .halt m' ∧
      Rel (upd (upd s (.slot 0) 255) (.slot 1) 1) m' := by
  have sim := prefixes_simulateC caughtProgram caughtCondition caught_invariant 6 0 s m
    (by simp [caughtCondition]) related
  rw [caught_source] at sim
  cases ht : runTargetC 6 caughtProgram 0 m with
  | halt m' => exact ⟨m', rfl, by simpa [ht, Related] using sim⟩
  | next q m' => simp [ht, Related] at sim
  | abort f => simp [ht, Related] at sim

end Vesper.CodegenFailScope

#print axioms Vesper.CodegenFailScope.default_catches_nothing
#print axioms Vesper.CodegenFailScope.fault_not_caught
#print axioms Vesper.CodegenFailScope.runBlockX_toRes
#print axioms Vesper.CodegenFailScope.step_memory
#print axioms Vesper.CodegenFailScope.stores_never_abort
#print axioms Vesper.CodegenFailScope.abort_preserves_memory
#print axioms Vesper.CodegenFailScope.select_storesLast
#print axioms Vesper.CodegenFailScope.targetRow_eq
#print axioms Vesper.CodegenFailScope.rel_of_memory
#print axioms Vesper.CodegenFailScope.row_simulatesC
#print axioms Vesper.CodegenFailScope.step_simulatesC
#print axioms Vesper.CodegenFailScope.prefixes_simulateC
#print axioms Vesper.CodegenFailScope.scopeAt_default
#print axioms Vesper.CodegenFailScope.sourceRowC_default
#print axioms Vesper.CodegenFailScope.targetRowC_default
#print axioms Vesper.CodegenFailScope.caught_source
#print axioms Vesper.CodegenFailScope.caught_invariant
#print axioms Vesper.CodegenFailScope.caught_target
