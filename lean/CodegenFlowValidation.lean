import CodegenValidation
import CodegenControlFlow

/-!
Finite certificates for the abstract control-flow selection model. Each row
has a finite list of range facts. The executable validator checks operands and
both branch successors, including back edges, using the proved transfer
function. Accepted certificates establish the existing operational invariant;
no concrete execution or termination is assumed. This is not a proof about
Rust lowering, native addresses, calling conventions, or byte encodings.
-/
namespace Vesper.CodegenFlowValidation
open Vesper.VerifiedCodegen
open Vesper.CodegenValidation
open Vesper.CodegenControlFlow

abbrev Annotation := List (Loc × Kind)
abbrev Certificate := List Annotation

def annotation (certificate : Certificate) (pc : Nat) : Annotation :=
  certificate[pc]?.getD []

def factsOf : Annotation → Facts
  | [], _ => none
  | (location, kind) :: rest, other =>
      if other = location then some kind else factsOf rest other

def HoldsAnnotation (facts : Annotation) (store : Store) : Prop :=
  ∀ location kind, (location, kind) ∈ facts → InRange kind (store location)

def entails (facts : Facts) (required : Annotation) : Bool :=
  required.all fun (location, kind) => supports facts location kind

def checkJump (labels : Labels) (certificate : Certificate) (facts : Facts) (label : Nat) : Bool :=
  match labels label with
  | none => true
  | some target => entails facts (annotation certificate target)

def checkNode (labels : Labels) (certificate : Certificate) (pc : Nat) (row : Lir) : Bool :=
  let facts := factsOf (annotation certificate pc)
  match row with
  | .branchZero slot label => supports facts (.slot slot) .bool &&
      entails facts (annotation certificate (pc + 1)) && checkJump labels certificate facts label
  | .jump label => checkJump labels certificate facts label
  | .label _ => entails facts (annotation certificate (pc + 1))
  | .ret => true
  | _ => checkRow facts row && entails (transfer facts row) (annotation certificate (pc + 1))

def validateFrom (labels : Labels) (certificate : Certificate) : Nat → List Lir → Bool
  | _, [] => true
  | pc, row :: rest => checkNode labels certificate pc row && validateFrom labels certificate (pc + 1) rest

def validateProgram (rows : List Lir) (labels : Labels) (certificate : Certificate) : Bool :=
  validateFrom labels certificate 0 rows

def program (rows : List Lir) (labels : Labels) : Program :=
  ⟨fun pc => rows[pc]?, labels⟩

theorem annotation_holds {facts : Annotation} {store : Store} (holds : HoldsAnnotation facts store) :
    Holds (factsOf facts) store := by
  induction facts with
  | nil => intro location kind impossible; cases impossible
  | cons head rest ih =>
    obtain ⟨location, kind⟩ := head
    intro other k known
    simp only [factsOf] at known
    split at known
    · rename_i same
      subst other
      cases known
      exact holds location kind (by simp)
    · exact ih (fun l k mem => holds l k (by simp [mem])) other k known

theorem entailment_sound {facts : Facts} {required : Annotation} {store : Store}
    (accepted : entails facts required = true) (holds : Holds facts store) :
    HoldsAnnotation required store := by
  intro location kind member
  simp only [entails, List.all_eq_true] at accepted
  exact supports_sound holds (accepted (location, kind) member)

theorem lookup_checked {labels : Labels} {certificate : Certificate} {rows : List Lir}
    {offset index : Nat} (accepted : validateFrom labels certificate offset rows = true)
    {row : Lir} (lookup : rows[index]? = some row) :
    checkNode labels certificate (offset + index) row = true := by
  induction rows generalizing offset index with
  | nil => simp at lookup
  | cons head rest ih =>
    obtain ⟨first, tail⟩ : checkNode labels certificate offset head = true ∧
        validateFrom labels certificate (offset + 1) rest = true := by simpa [validateFrom] using accepted
    cases index with
    | zero =>
      simp only [List.getElem?_cons_zero, Option.some.injEq] at lookup
      subst row
      simpa using first
    | succ index =>
      have remaining : rest[index]? = some row := by simpa using lookup
      simpa [Nat.add_assoc, Nat.add_comm, Nat.add_left_comm] using ih tail remaining

theorem node_operands {labels : Labels} {certificate : Certificate} {pc : Nat} {row : Lir}
    {store : Store} (accepted : checkNode labels certificate pc row = true)
    (holds : HoldsAnnotation (annotation certificate pc) store) :
    Supported row ∧ Operands row store := by
  have facts := annotation_holds holds
  cases row <;> simp_all [checkNode, Supported, Operands, checkRow, Typed]
  case arith op k d a b =>
    exact ⟨supports_sound facts accepted.1.1.2, supports_sound facts accepted.1.2⟩
  case wrapping op k d a b =>
    exact ⟨supports_sound facts accepted.1.1.2, supports_sound facts accepted.1.2⟩
  case neg k d a => exact supports_sound facts accepted.1.2
  case not d a => exact supports_sound facts accepted.1
  case compare c k d a b =>
    exact ⟨supports_sound facts accepted.1.1, supports_sound facts accepted.1.2⟩
  case branchZero slot label => exact supports_sound facts accepted.1.1
  case trap failure => cases failure <;> simp_all

theorem jump_preserves {labels : Labels} {certificate : Certificate} {facts : Facts}
    {store state : Store} {label next : Nat}
    (accepted : checkJump labels certificate facts label = true) (holds : Holds facts store)
    (result : jump labels label store = .next next state) :
    HoldsAnnotation (annotation certificate next) state := by
  cases target : labels label with
  | none => simp [jump, target] at result
  | some pc =>
    simp only [jump, target, Outcome.next.injEq] at result
    obtain ⟨rfl, rfl⟩ := result
    exact entailment_sound (by simpa [checkJump, target] using accepted) holds

theorem data_node_check (labels : Labels) (certificate : Certificate) (pc : Nat)
    (row : Lir) (data : DataRow row) :
    checkNode labels certificate pc row =
      (checkRow (factsOf (annotation certificate pc)) row &&
        entails (transfer (factsOf (annotation certificate pc)) row) (annotation certificate (pc + 1))) := by
  cases row <;> simp_all [DataRow, checkNode]

theorem data_source_row (labels : Labels) (pc : Nat) (row : Lir) (store : Store)
    (data : DataRow row) : sourceRow labels pc row store = advance pc (lirStep row store) := by
  cases row <;> simp_all [DataRow, sourceRow]

theorem node_preserves {labels : Labels} {certificate : Certificate} {pc next : Nat}
    {row : Lir} {store state : Store} (accepted : checkNode labels certificate pc row = true)
    (holds : HoldsAnnotation (annotation certificate pc) store)
    (result : sourceRow labels pc row store = .next next state) :
    HoldsAnnotation (annotation certificate next) state := by
  have facts := annotation_holds holds
  by_cases data : DataRow row
  · rw [data_node_check labels certificate pc row data] at accepted
    obtain ⟨check, edge⟩ : checkRow (factsOf (annotation certificate pc)) row = true ∧
        entails (transfer (factsOf (annotation certificate pc)) row) (annotation certificate (pc + 1)) = true :=
      by simpa using accepted
    rw [data_source_row labels pc row store data] at result
    cases step : lirStep row store with
    | abort failure => simp [step, advance] at result
    | ok output =>
      simp only [step, advance, Outcome.next.injEq] at result
      obtain ⟨rfl, rfl⟩ := result
      exact entailment_sound edge (transfer_sound facts check step)
  · cases row with
    | branchZero slot label =>
      obtain ⟨⟨_, fall⟩, taken⟩ :
          (supports (factsOf (annotation certificate pc)) (.slot slot) .bool = true ∧
            entails (factsOf (annotation certificate pc)) (annotation certificate (pc + 1)) = true) ∧
          checkJump labels certificate (factsOf (annotation certificate pc)) label = true :=
        by simpa [checkNode] using accepted
      by_cases zero : store (.slot slot) = 0
      · exact jump_preserves taken facts (by simpa [sourceRow, zero] using result)
      · simp only [sourceRow, zero, ↓reduceIte, Outcome.next.injEq] at result
        obtain ⟨rfl, rfl⟩ := result
        exact entailment_sound fall facts
    | jump label => exact jump_preserves accepted facts result
    | label label =>
      simp only [sourceRow, Outcome.next.injEq] at result
      obtain ⟨rfl, rfl⟩ := result
      exact entailment_sound accepted facts
    | ret | call _ | enter _ => cases result
    | _ => exact False.elim (data trivial)

/-- The executable certificate check constructs the operational invariant. -/
theorem validation_invariant {rows : List Lir} {labels : Labels} {certificate : Certificate}
    (accepted : validateProgram rows labels certificate = true) :
    Invariant (program rows labels) (fun pc store => HoldsAnnotation (annotation certificate pc) store) := by
  constructor
  · intro pc store row holds lookup
    exact node_operands (by simpa using lookup_checked accepted lookup) holds
  · intro pc store next state holds step
    cases lookup : rows[pc]? with
    | none => simp [sourceStep, program, lookup] at step
    | some row =>
      exact node_preserves (by simpa using lookup_checked accepted lookup) holds
        (by simpa [sourceStep, program, lookup] using step)

theorem validated_prefixes {rows : List Lir} {labels : Labels} {certificate : Certificate}
    (accepted : validateProgram rows labels certificate = true) (fuel pc : Nat)
    (store : Store) (machine : Mach) (holds : HoldsAnnotation (annotation certificate pc) store)
    (related : Rel store machine) :
    Related (runSource fuel (program rows labels) pc store) (runTarget fuel (program rows labels) pc machine) :=
  prefixes_simulate _ _ (validation_invariant accepted) fuel pc store machine holds related

theorem validated_fault_exclusion {rows : List Lir} {labels : Labels} {certificate : Certificate}
    (accepted : validateProgram rows labels certificate = true) (fuel pc : Nat)
    (store : Store) (machine : Mach) (holds : HoldsAnnotation (annotation certificate pc) store)
    (related : Rel store machine) :
    runTarget fuel (program rows labels) pc machine ≠ .abort .fault :=
  target_prefix_has_no_fault _ _ (validation_invariant accepted) fuel pc store machine holds related

theorem validated_divergence_iff {rows : List Lir} {labels : Labels} {certificate : Certificate}
    (accepted : validateProgram rows labels certificate = true) (pc : Nat)
    (store : Store) (machine : Mach) (holds : HoldsAnnotation (annotation certificate pc) store)
    (related : Rel store machine) :
    SourceDiverges (program rows labels) pc store ↔ TargetDiverges (program rows labels) pc machine :=
  divergence_iff _ _ (validation_invariant accepted) pc store machine holds related

theorem empty_annotation_holds (store : Store) : HoldsAnnotation [] store := by
  intro location kind impossible
  cases impossible

theorem loop_validated : validateProgram [.jump 0] loopProgram.labels [] = true := by decide

theorem loop_program_agrees : program [.jump 0] loopProgram.labels = loopProgram := by
  unfold program loopProgram
  congr 1
  funext pc
  cases pc <;> rfl

/-- A finite accepted certificate proves safety even for a nonterminating program. -/
theorem validated_loop_diverges (store : Store) (machine : Mach) (related : Rel store machine) :
    TargetDiverges loopProgram 0 machine := by
  have source : SourceDiverges (program [.jump 0] loopProgram.labels) 0 store := by
    rw [loop_program_agrees]
    exact fun fuel => ⟨0, store, loop_prefix fuel store⟩
  have target := (validated_divergence_iff loop_validated 0 store machine (empty_annotation_holds store) related).mp source
  simpa [loop_program_agrees] using target

def branchRows : List Lir :=
  [.const 0 1, .branchZero 0 9, .const 1 42, .jump 10, .const 1 7, .ret]

def branchLabels : Labels := fun label => if label = 9 then some 4 else if label = 10 then some 5 else none

def branchCertificate : Certificate :=
  [[], [(.slot 0, .bool)], [], [(.slot 1, .u8)], [], [(.slot 1, .u8)]]

theorem branch_validated : validateProgram branchRows branchLabels branchCertificate = true := by decide

theorem branch_source_result (store : Store) :
    runSource 6 (program branchRows branchLabels) 0 store = .halt (upd (upd store (.slot 0) 1) (.slot 1) 42) := by
  simp [runSource, sourceStep, sourceRow, program, branchRows, branchLabels, advance, lirStep, upd, jump]

theorem validated_branch_result (store : Store) (machine : Mach) (related : Rel store machine) :
    ∃ result, runTarget 6 (program branchRows branchLabels) 0 machine = .halt result ∧
      result.mem (.slot 1) = 42 := by
  have sim := validated_prefixes branch_validated 6 0 store machine (empty_annotation_holds store) related
  rw [branch_source_result] at sim
  cases outcome : runTarget 6 (program branchRows branchLabels) 0 machine with
  | next pc target => simp [outcome, Related] at sim
  | abort failure => simp [outcome, Related] at sim
  | halt target =>
    refine ⟨target, rfl, ?_⟩
    have relation : Rel (upd (upd store (.slot 0) 1) (.slot 1) 42) target :=
      by simpa [outcome, Related] using sim
    simpa [upd, enc, W] using relation (.slot 1)

/-- A declared Boolean at the target cannot hide a wide incoming integer. -/
theorem forged_branch_range_rejected :
    validateProgram [.const 0 W, .branchZero 0 0, .ret] (fun _ => some 2)
      [[], [(.slot 0, .bool)], []] = false := by decide

/-- The loop edge is checked even though the first iteration starts in range. -/
theorem bad_back_edge_rejected :
    validateProgram [.const 0 1, .label 0, .not 0 0, .const 0 2, .jump 0]
      (fun _ => some 1)
      [[], [(.slot 0, .bool)], [(.slot 0, .bool)], [], [(.slot 0, .u8)]] = false := by decide

/-- Both successors are checked, even if one is unreachable from this entry value. -/
theorem bad_branch_arm_rejected :
    validateProgram [.const 0 1, .branchZero 0 9, .const 1 42, .jump 10, .const 1 W, .ret]
      branchLabels branchCertificate = false := by decide

end Vesper.CodegenFlowValidation

#print axioms Vesper.CodegenFlowValidation.annotation_holds
#print axioms Vesper.CodegenFlowValidation.entailment_sound
#print axioms Vesper.CodegenFlowValidation.lookup_checked
#print axioms Vesper.CodegenFlowValidation.node_operands
#print axioms Vesper.CodegenFlowValidation.jump_preserves
#print axioms Vesper.CodegenFlowValidation.data_node_check
#print axioms Vesper.CodegenFlowValidation.data_source_row
#print axioms Vesper.CodegenFlowValidation.node_preserves
#print axioms Vesper.CodegenFlowValidation.validation_invariant
#print axioms Vesper.CodegenFlowValidation.validated_prefixes
#print axioms Vesper.CodegenFlowValidation.validated_fault_exclusion
#print axioms Vesper.CodegenFlowValidation.validated_divergence_iff
#print axioms Vesper.CodegenFlowValidation.empty_annotation_holds
#print axioms Vesper.CodegenFlowValidation.loop_validated
#print axioms Vesper.CodegenFlowValidation.loop_program_agrees
#print axioms Vesper.CodegenFlowValidation.validated_loop_diverges
#print axioms Vesper.CodegenFlowValidation.branch_validated
#print axioms Vesper.CodegenFlowValidation.branch_source_result
#print axioms Vesper.CodegenFlowValidation.validated_branch_result
#print axioms Vesper.CodegenFlowValidation.forged_branch_range_rejected
#print axioms Vesper.CodegenFlowValidation.bad_back_edge_rejected
#print axioms Vesper.CodegenFlowValidation.bad_branch_arm_rejected
