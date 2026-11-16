import CodegenComposition

/-!
Control-flow composition for the existing abstract selection table. Programs
are indexed rows with an explicit label resolver; target rows contain the
actual `select` instruction lists. Target execution interprets their branches
and jumps, rather than treating control instructions as data no-ops.

The invariant and operand-range obligations remain explicit. This model does
not verify Rust lowering, physical instruction addresses, frame management,
calls, byte encodings, or execution on hardware.
-/
namespace Vesper.CodegenControlFlow
open Vesper.VerifiedCodegen

inductive Control | fall | jump (label : Nat) | halt
  deriving DecidableEq

/-- Execute a selected block up to its first control transfer or abort. -/
def runBlock : List M → Mach → Res (Control × Mach)
  | [], m => .ok (.fall, m)
  | .jmp label :: _, m => .ok (.jump label, m)
  | .bz label :: rest, m =>
      if m.reg .r0 = 0 then .ok (.jump label, m) else runBlock rest m
  | .leave :: _, m => .ok (.halt, m)
  | .call _ :: _, _ | .enter _ :: _, _ => .abort .fault
  | instruction :: rest, m => (Mach.step instruction m).bind (runBlock rest)

def DataInstruction : M → Prop
  | .jmp _ | .bz _ | .leave | .call _ | .enter _ => False
  | _ => True

theorem runBlock_data (code : List M) (m : Mach)
    (data : ∀ instruction ∈ code, DataInstruction instruction) :
    runBlock code m = (run code m).bind (fun result => .ok (.fall, result)) := by
  induction code generalizing m with
  | nil => rfl
  | cons instruction rest ih =>
    have head := data instruction (by simp)
    have tail : ∀ i ∈ rest, DataInstruction i := by
      intro i hi; exact data i (by simp [hi])
    have restRun (next : Mach) := ih next tail
    cases instruction <;> simp only [DataInstruction] at head
    all_goals try contradiction
    all_goals simp only [runBlock, run]
    all_goals cases h : Mach.step _ m <;> simp [Res.bind, restRun]

theorem selected_data_instructions (row : Lir) (data : DataRow row) :
    ∀ instruction ∈ select row, DataInstruction instruction := by
  cases row <;> simp_all [DataRow, select, binary, DataInstruction]
  case arith op k d a b =>
    cases op <;> cases k <;> simp [rangeCheck, Kind.mask, Kind.sign, or_imp, forall_and]
  case wrapping op k d a b =>
    cases k <;> simp [Kind.mask, or_imp, forall_and]

inductive Outcome (α : Type)
  | next (pc : Nat) (state : α)
  | halt (state : α)
  | abort (failure : Failure)

abbrev Labels := Nat → Option Nat

def advance (pc : Nat) : Res α → Outcome α
  | .ok state => .next (pc + 1) state
  | .abort failure => .abort failure

def jump (labels : Labels) (label : Nat) (state : α) : Outcome α :=
  match labels label with
  | some target => .next target state
  | none => .abort .unreachable

def finish (labels : Labels) (pc : Nat) : Res (Control × Mach) → Outcome Mach
  | .ok (.fall, state) => .next (pc + 1) state
  | .ok (.jump label, state) => jump labels label state
  | .ok (.halt, state) => .halt state
  | .abort failure => .abort failure

def sourceRow (labels : Labels) (pc : Nat) (row : Lir) (s : Store) : Outcome Store :=
  match row with
  | .branchZero slot label => if s (.slot slot) = 0 then jump labels label s else .next (pc + 1) s
  | .jump label => jump labels label s
  | .label _ => .next (pc + 1) s
  | .ret => .halt s
  | .call _ | .enter _ => .abort .fault
  | _ => advance pc (lirStep row s)

def targetRow (labels : Labels) (pc : Nat) (row : Lir) (m : Mach) : Outcome Mach :=
  finish labels pc (runBlock (select row) m)

def Related : Outcome Store → Outcome Mach → Prop
  | .next pc s, .next target m => pc = target ∧ Rel s m
  | .halt s, .halt m => Rel s m
  | .abort f, .abort g => f = g
  | _, _ => False

def Supported : Lir → Prop
  | .call _ | .enter _ | .trap .fault => False
  | _ => True

def Operands (row : Lir) (s : Store) : Prop :=
  match row with
  | .branchZero slot _ => InRange .bool (s (.slot slot))
  | _ => Typed row s

theorem jump_related (labels : Labels) (label : Nat) (s : Store) (m : Mach)
    (related : Rel s m) : Related (jump labels label s) (jump labels label m) := by
  unfold jump
  cases labels label <;> simp [Related, related]

theorem data_row_simulates (labels : Labels) (pc : Nat) (row : Lir)
    (s : Store) (m : Mach) (data : DataRow row) (typed : Typed row s) (related : Rel s m) :
    Related (advance pc (lirStep row s)) (targetRow labels pc row m) := by
  have sim := select_simulates row s m related data typed
  unfold targetRow
  rw [runBlock_data _ _ (selected_data_instructions row data)]
  cases hs : lirStep row s <;> cases hm : run (select row) m <;>
    simp_all [Sim, advance, finish, Res.bind, Related]

theorem row_simulates (labels : Labels) (pc : Nat) (row : Lir)
    (s : Store) (m : Mach) (supported : Supported row) (typed : Operands row s)
    (related : Rel s m) : Related (sourceRow labels pc row s) (targetRow labels pc row m) := by
  by_cases data : DataRow row
  · have operands : Typed row s := by cases row <;> simp_all [DataRow, Operands]
    have source : sourceRow labels pc row s = advance pc (lirStep row s) := by
      cases row <;> simp_all [DataRow, sourceRow]
    rw [source]
    exact data_row_simulates labels pc row s m data operands related
  · cases row with
    | branchZero slot label =>
      have range : 0 ≤ s (.slot slot) ∧ s (.slot slot) ≤ 1 := typed
      have zero : enc (s (.slot slot)) = 0 ↔ s (.slot slot) = 0 := by
        simp only [enc, W]; omega
      have mem := related (.slot slot)
      by_cases condition : s (.slot slot) = 0
      · simpa [sourceRow, targetRow, select, runBlock, Mach.step, Res.bind,
          setReg, mem, zero, condition, finish, enc] using
          jump_related labels label s (setReg m .r0 (m.mem (.slot slot))) related
      · simp [sourceRow, targetRow, select, runBlock, Mach.step, Res.bind,
          setReg, mem, zero, condition, finish, Related]
        exact related
    | jump label => exact jump_related labels label s m related
    | label label => simpa [sourceRow, targetRow, select, runBlock, Mach.step,
        Res.bind, finish, Related] using related
    | ret => exact related
    | call label => exact False.elim supported
    | enter words => exact False.elim supported
    | _ => exact False.elim (data trivial)

structure Program where
  rows : Nat → Option Lir
  labels : Labels

def sourceStep (program : Program) (pc : Nat) (s : Store) : Outcome Store :=
  match program.rows pc with
  | some row => sourceRow program.labels pc row s
  | none => .abort .unreachable

def targetStep (program : Program) (pc : Nat) (m : Mach) : Outcome Mach :=
  match program.rows pc with
  | some row => targetRow program.labels pc row m
  | none => .abort .unreachable

/-- An explicit inductive verification condition, not an assumed execution. -/
structure Invariant (program : Program) (condition : Nat → Store → Prop) : Prop where
  operands : ∀ pc s row, condition pc s → program.rows pc = some row →
    Supported row ∧ Operands row s
  preserved : ∀ pc s next state, condition pc s →
    sourceStep program pc s = .next next state → condition next state

theorem step_simulates (program : Program) (condition : Nat → Store → Prop)
    (invariant : Invariant program condition) (pc : Nat) (s : Store) (m : Mach)
    (holds : condition pc s) (related : Rel s m) :
    Related (sourceStep program pc s) (targetStep program pc m) := by
  cases row : program.rows pc with
  | none => simp [sourceStep, targetStep, row, Related]
  | some instruction =>
    obtain ⟨supported, operands⟩ := invariant.operands pc s instruction holds row
    simpa [sourceStep, targetStep, row] using
      row_simulates program.labels pc instruction s m supported operands related

/-- Fuel limits observations; exhaustion returns the residual state, not an abort. -/
def runSource : Nat → Program → Nat → Store → Outcome Store
  | 0, _, pc, s => .next pc s
  | fuel + 1, program, pc, s =>
      match sourceStep program pc s with
      | .next next state => runSource fuel program next state
      | .halt state => .halt state
      | .abort failure => .abort failure

def runTarget : Nat → Program → Nat → Mach → Outcome Mach
  | 0, _, pc, m => .next pc m
  | fuel + 1, program, pc, m =>
      match targetStep program pc m with
      | .next next state => runTarget fuel program next state
      | .halt state => .halt state
      | .abort failure => .abort failure

/-- Every finite execution prefix agrees, including terminal outcomes. -/
theorem prefixes_simulate (program : Program) (condition : Nat → Store → Prop)
    (invariant : Invariant program condition) (fuel pc : Nat) (s : Store) (m : Mach)
    (holds : condition pc s) (related : Rel s m) :
    Related (runSource fuel program pc s) (runTarget fuel program pc m) := by
  induction fuel generalizing pc s m with
  | zero => exact ⟨rfl, related⟩
  | succ fuel ih =>
    have sim := step_simulates program condition invariant pc s m holds related
    simp only [runSource, runTarget]
    cases hs : sourceStep program pc s with
    | next next state =>
      cases ht : targetStep program pc m with
      | next target machine =>
        obtain ⟨same, rel⟩ : next = target ∧ Rel state machine := by
          simpa [hs, ht, Related] using sim
        subst target
        exact ih next state machine (invariant.preserved pc s next state holds hs) rel
      | halt machine => simp [hs, ht, Related] at sim
      | abort failure => simp [hs, ht, Related] at sim
    | halt state =>
      cases ht : targetStep program pc m with
      | halt machine => simpa [hs, ht, Related] using sim
      | next target machine => simp [hs, ht, Related] at sim
      | abort failure => simp [hs, ht, Related] at sim
    | abort failure =>
      cases ht : targetStep program pc m with
      | abort target => simpa [hs, ht, Related] using sim
      | next target machine => simp [hs, ht, Related] at sim
      | halt machine => simp [hs, ht, Related] at sim

def SourceDiverges (program : Program) (pc : Nat) (s : Store) : Prop :=
  ∀ fuel, ∃ next state, runSource fuel program pc s = .next next state

def TargetDiverges (program : Program) (pc : Nat) (m : Mach) : Prop :=
  ∀ fuel, ∃ next state, runTarget fuel program pc m = .next next state

/-- Nontermination is preserved and reflected at the selected-block level. -/
theorem divergence_iff (program : Program) (condition : Nat → Store → Prop)
    (invariant : Invariant program condition) (pc : Nat) (s : Store) (m : Mach)
    (holds : condition pc s) (related : Rel s m) :
    SourceDiverges program pc s ↔ TargetDiverges program pc m := by
  constructor
  · intro diverges fuel
    obtain ⟨next, state, source⟩ := diverges fuel
    have sim := prefixes_simulate program condition invariant fuel pc s m holds related
    cases target : runTarget fuel program pc m with
    | next q machine => exact ⟨q, machine, rfl⟩
    | halt machine => simp [source, target, Related] at sim
    | abort failure => simp [source, target, Related] at sim
  · intro diverges fuel
    obtain ⟨next, state, target⟩ := diverges fuel
    have sim := prefixes_simulate program condition invariant fuel pc s m holds related
    cases source : runSource fuel program pc s with
    | next q store => exact ⟨q, store, rfl⟩
    | halt store => simp [source, target, Related] at sim
    | abort failure => simp [source, target, Related] at sim

theorem source_row_has_no_fault (labels : Labels) (pc : Nat) (row : Lir) (s : Store)
    (supported : Supported row) : sourceRow labels pc row s ≠ .abort .fault := by
  by_cases data : DataRow row
  · have safe : row ≠ .trap .fault := by intro same; subst row; exact supported
    have noFault := Vesper.CodegenComposition.row_has_no_fault row s safe
    have source : sourceRow labels pc row s = advance pc (lirStep row s) := by
      cases row <;> simp_all [DataRow, sourceRow]
    rw [source]
    cases step : lirStep row s <;> simp_all [advance]
  · cases row with
    | branchZero slot label =>
      by_cases condition : s (.slot slot) = 0
      · simp [sourceRow, condition, jump]
        cases labels label <;> simp
      · simp [sourceRow, condition]
    | jump label =>
      simp [sourceRow, jump]
      cases labels label <;> simp
    | label label => simp [sourceRow]
    | ret => simp [sourceRow]
    | call label => exact False.elim supported
    | enter words => exact False.elim supported
    | _ => exact False.elim (data trivial)

theorem source_step_has_no_fault (program : Program) (condition : Nat → Store → Prop)
    (invariant : Invariant program condition) (pc : Nat) (s : Store)
    (holds : condition pc s) : sourceStep program pc s ≠ .abort .fault := by
  cases row : program.rows pc with
  | none => simp [sourceStep, row]
  | some instruction =>
    simpa [sourceStep, row] using source_row_has_no_fault program.labels pc instruction s
      (invariant.operands pc s instruction holds row).1

theorem source_prefix_has_no_fault (program : Program) (condition : Nat → Store → Prop)
    (invariant : Invariant program condition) (fuel pc : Nat) (s : Store)
    (holds : condition pc s) : runSource fuel program pc s ≠ .abort .fault := by
  induction fuel generalizing pc s with
  | zero => simp [runSource]
  | succ fuel ih =>
    simp only [runSource]
    cases step : sourceStep program pc s with
    | next next state => exact ih next state (invariant.preserved pc s next state holds step)
    | halt state => simp
    | abort failure =>
      have safe := source_step_has_no_fault program condition invariant pc s holds
      simpa [step] using safe

theorem target_prefix_has_no_fault (program : Program) (condition : Nat → Store → Prop)
    (invariant : Invariant program condition) (fuel pc : Nat) (s : Store) (m : Mach)
    (holds : condition pc s) (related : Rel s m) :
    runTarget fuel program pc m ≠ .abort .fault := by
  have sim := prefixes_simulate program condition invariant fuel pc s m holds related
  have safe := source_prefix_has_no_fault program condition invariant fuel pc s holds
  intro bad
  cases source : runSource fuel program pc s with
  | next next state => simp [source, bad, Related] at sim
  | halt state => simp [source, bad, Related] at sim
  | abort failure =>
    have same : failure = .fault := by simpa [source, bad, Related] using sim
    exact safe (source.trans (congrArg Outcome.abort same))

def loopProgram : Program where
  rows := fun pc => if pc = 0 then some (.jump 0) else none
  labels := fun label => if label = 0 then some 0 else none

theorem loop_invariant : Invariant loopProgram (fun pc _ => pc = 0) := by
  constructor
  · intro pc s row atEntry fetch
    subst pc
    simp [loopProgram] at fetch
    subst row
    trivial
  · intro pc s next state atEntry step
    subst pc
    simp [sourceStep, sourceRow, loopProgram, jump] at step
    exact step.1.symm

theorem loop_prefix (fuel : Nat) (s : Store) :
    runSource fuel loopProgram 0 s = .next 0 s := by
  induction fuel with
  | zero => rfl
  | succ fuel ih => simpa [runSource, sourceStep, sourceRow, loopProgram, jump] using ih

theorem loop_diverges (s : Store) (m : Mach) (related : Rel s m) :
    SourceDiverges loopProgram 0 s ∧ TargetDiverges loopProgram 0 m := by
  have source : SourceDiverges loopProgram 0 s := fun fuel => ⟨0, s, loop_prefix fuel s⟩
  exact ⟨source, (divergence_iff loopProgram _ loop_invariant 0 s m rfl related).mp source⟩

/-- Without the Boolean range premise, encoding can change a branch decision. -/
theorem branch_range_required :
    let s : Store := fun _ => W
    let m : Mach := ⟨fun _ => 0, fun _ => 0⟩
    Rel s m ∧ ¬ Related
      (sourceRow (fun _ => some 5) 0 (.branchZero 0 0) s)
      (targetRow (fun _ => some 5) 0 (.branchZero 0 0) m) := by
  simp [Rel, sourceRow, targetRow, select, runBlock, Mach.step, Res.bind,
    finish, jump, Related, setReg, enc, W]

end Vesper.CodegenControlFlow

#print axioms Vesper.CodegenControlFlow.runBlock_data
#print axioms Vesper.CodegenControlFlow.selected_data_instructions
#print axioms Vesper.CodegenControlFlow.jump_related
#print axioms Vesper.CodegenControlFlow.data_row_simulates
#print axioms Vesper.CodegenControlFlow.row_simulates

#print axioms Vesper.CodegenControlFlow.step_simulates
#print axioms Vesper.CodegenControlFlow.prefixes_simulate
#print axioms Vesper.CodegenControlFlow.divergence_iff

#print axioms Vesper.CodegenControlFlow.source_row_has_no_fault
#print axioms Vesper.CodegenControlFlow.source_step_has_no_fault
#print axioms Vesper.CodegenControlFlow.source_prefix_has_no_fault
#print axioms Vesper.CodegenControlFlow.target_prefix_has_no_fault
#print axioms Vesper.CodegenControlFlow.loop_invariant
#print axioms Vesper.CodegenControlFlow.loop_prefix
#print axioms Vesper.CodegenControlFlow.loop_diverges
#print axioms Vesper.CodegenControlFlow.branch_range_required
