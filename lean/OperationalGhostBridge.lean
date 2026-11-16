import Metatheory.Polymorphism
import GhostErasure

/-!
Composition of checked closed ghost annotations with the existing polymorphic
operational calculus. An instrumentation function attaches a finite list of
checked ghost computations at each runtime configuration. Ghost computations
have their own resource store; they cannot capture runtime owners or influence
runtime transitions. Their termination follows from typing, not an evaluator
success flag supplied by the instrumentation.

This is an operational instrumentation model, not a surface elaboration or a
proof for dependent/captured ghost terms. It reuses the actual polymorphic
runtime semantics, including recursion, closures and stateful handlers.
-/
namespace Vesper.OperationalGhostBridge
open Vesper.Metatheory.Polymorphism

structure CheckedGhost where
  term : Vesper.Metatheory.Expr
  typed : Vesper.GhostErasure.GhostProofWellTyped term

abbrev Instrumentation := Conf → List CheckedGhost

structure State where
  runtime : Conf
  pending : List CheckedGhost

def Ready (state : State) : Prop := state.pending = []

def WellTyped (Sig : Nat → EffSig) (state : State) (ty : Ty) (mode : Mode) (row : Row) : Prop :=
  WT Sig state.runtime ty mode row

-- The runtime trace deliberately excludes ghost resource identifiers. Their
-- allocation starts in a distinct store and cannot satisfy a runtime consume.
inductive AnnotatedStep (Sig : Nat → EffSig) (instrument : Instrumentation) :
    State → Option Act → State → Prop where
  | ghost {runtime proof rest minted consumed value next} :
      Vesper.Metatheory.EvalDet none proof.term 0 minted consumed
        (.val value) next →
      AnnotatedStep Sig instrument ⟨runtime, proof :: rest⟩ none ⟨runtime, rest⟩
  | runtime {before action after} : Step Sig before action after →
      AnnotatedStep Sig instrument ⟨before, []⟩ (some action) ⟨after, instrument after⟩

inductive AnnotatedSteps (Sig : Nat → EffSig) (instrument : Instrumentation) :
    State → List Act → State → Prop where
  | refl (state) : AnnotatedSteps Sig instrument state [] state
  | ghost : AnnotatedStep Sig instrument before none mid →
      AnnotatedSteps Sig instrument mid actions after →
      AnnotatedSteps Sig instrument before actions after
  | runtime : AnnotatedStep Sig instrument before (some action) mid →
      AnnotatedSteps Sig instrument mid actions after →
      AnnotatedSteps Sig instrument before (action :: actions) after

variable {Sig : Nat → EffSig} {instrument : Instrumentation}

theorem ghost_step_keeps_runtime (step : AnnotatedStep Sig instrument before none after) :
    after.runtime = before.runtime ∧ after.pending.length + 1 = before.pending.length := by
  cases step
  exact ⟨rfl, rfl⟩

theorem runtime_step_erases
    (step : AnnotatedStep Sig instrument before (some action) after) :
    Step Sig before.runtime action after.runtime := by
  cases step with
  | runtime h => exact h

theorem annotated_preservation (step : AnnotatedStep Sig instrument before action after)
    (typed : WellTyped Sig before ty mode row) : WellTyped Sig after ty mode row := by
  cases step with
  | ghost _ => exact typed
  | runtime h => exact preservation h typed

theorem annotated_steps_trans (first : AnnotatedSteps Sig instrument before a mid)
    (second : AnnotatedSteps Sig instrument mid b after) :
    AnnotatedSteps Sig instrument before (a ++ b) after := by
  induction first with
  | refl => exact second
  | ghost h _ ih => exact .ghost h (ih second)
  | runtime h _ ih => exact .runtime h (ih second)

-- Every pending proof terminates and discharges every ghost token it mints.
-- In particular, an erased annotation cannot mask divergence or an abort.
theorem checked_ghost_terminates (proof : CheckedGhost) :
    ∃ minted consumed value next,
      Vesper.Metatheory.EvalDet none proof.term 0 minted consumed (.val value) next ∧
      (∀ resource, resource ∈ minted → consumed.count resource = 1) :=
  Vesper.GhostErasure.ghost_proof_terminates proof.typed

theorem drain_annotations (state : State) :
    AnnotatedSteps Sig instrument state [] ⟨state.runtime, []⟩ := by
  obtain ⟨runtime, pending⟩ := state
  induction pending with
  | nil => exact .refl _
  | cons proof rest ih =>
      obtain ⟨_, _, _, _, eval, _⟩ := checked_ghost_terminates proof
      exact .ghost (.ghost eval) ih

theorem annotated_progress (typed : WellTyped Sig state ty mode row)
    (closed : ∀ label, Label.user label ∉ row) :
    (Ready state ∧ (IsVal state.runtime.e ∨ state.runtime.e = .abort)) ∨
      ∃ action after, AnnotatedStep Sig instrument state action after := by
  obtain ⟨runtime, pending⟩ := state
  cases pending with
  | cons proof rest =>
      obtain ⟨_, _, _, _, eval, _⟩ := checked_ghost_terminates proof
      exact .inr ⟨none, _, .ghost eval⟩
  | nil =>
      rcases progress typed closed with value | aborted | ⟨action, after, step⟩
      · exact .inl ⟨rfl, .inl value⟩
      · exact .inl ⟨rfl, .inr aborted⟩
      · exact .inr ⟨some action, _, .runtime step⟩

theorem erase_annotated_steps (steps : AnnotatedSteps Sig instrument before actions after) :
    Steps Sig before.runtime actions after.runtime := by
  induction steps with
  | refl => exact .refl _
  | ghost step _ ih =>
      have same := (ghost_step_keeps_runtime step).1
      simpa [same] using ih
  | runtime step _ ih => exact .cons (runtime_step_erases step) ih

theorem lift_runtime_steps (steps : Steps Sig before actions after) (pending : List CheckedGhost) :
    AnnotatedSteps Sig instrument ⟨before, pending⟩ actions ⟨after, []⟩ := by
  induction steps generalizing pending with
  | refl => exact drain_annotations _
  | cons step _ ih =>
      have tail := AnnotatedSteps.runtime (AnnotatedStep.runtime step) (ih (instrument _))
      exact annotated_steps_trans (drain_annotations _) tail

theorem erasure_trace_equivalence (before after : Conf) (pending : List CheckedGhost)
    (actions : List Act) :
    AnnotatedSteps Sig instrument ⟨before, pending⟩ actions ⟨after, []⟩ ↔
      Steps Sig before actions after :=
  ⟨erase_annotated_steps, fun h => lift_runtime_steps h pending⟩

theorem instrumentation_noninterference (first second : Instrumentation)
    (before after : Conf) (pending₁ pending₂ : List CheckedGhost) (actions : List Act) :
    AnnotatedSteps Sig first ⟨before, pending₁⟩ actions ⟨after, []⟩ ↔
      AnnotatedSteps Sig second ⟨before, pending₂⟩ actions ⟨after, []⟩ := by
  rw [erasure_trace_equivalence, erasure_trace_equivalence]

-- Ghost-only stuttering cannot be infinite: every such step strictly decreases
-- the finite pending list. Runtime silent actions still count as runtime steps.
theorem no_infinite_ghost_stuttering :
    ¬ ∃ states : Nat → State, ∀ n, AnnotatedStep Sig instrument (states n) none (states (n + 1)) := by
  rintro ⟨states, steps⟩
  have decrease (n) := (ghost_step_keeps_runtime (steps n)).2
  have bound : ∀ n, (states n).pending.length + n = (states 0).pending.length := by
    intro n
    induction n with
    | zero => simp
    | succ n ih => have h := decrease n; omega
  have impossible := bound ((states 0).pending.length + 1)
  omega

theorem annotated_type_safety (steps : AnnotatedSteps Sig instrument before actions after)
    (typed : WellTyped Sig before ty mode row) (closed : ∀ label, Label.user label ∉ row) :
    WellTyped Sig after ty mode row ∧
      ((Ready after ∧ (IsVal after.runtime.e ∨ after.runtime.e = .abort)) ∨
        ∃ action next, AnnotatedStep Sig instrument after action next) := by
  have runtimeTyped := (type_safety (erase_annotated_steps steps) typed closed).1
  exact ⟨runtimeTyped, annotated_progress runtimeTyped closed⟩

theorem annotated_linear_exhaustion
    (typed : HasType Sig Ctx.empty RCtx.empty expression ty .copy row)
    (closed : ∀ label, Label.user label ∉ row)
    (steps : AnnotatedSteps Sig instrument ⟨⟨expression, RCtx.empty, 0⟩, pending⟩ actions after)
    (finished : IsVal after.runtime.e ∨ after.runtime.e = .abort) :
    ∀ resource, (Act.mint resource ∈ actions ∨ Act.capture resource .linear ∈ actions) →
      relCount resource actions = 1 :=
  total_linear_exhaustion typed closed (erase_annotated_steps steps) finished

theorem annotated_exhaustion_per_kind
    (typed : HasType Sig Ctx.empty RCtx.empty expression ty .copy row)
    (closed : ∀ label, Label.user label ∉ row)
    (steps : AnnotatedSteps Sig instrument ⟨⟨expression, RCtx.empty, 0⟩, pending⟩ actions after)
    (finished : IsVal after.runtime.e ∨ after.runtime.e = .abort) :
    (∀ r, Act.mint r ∈ actions → actions.count (.consume r) = 1 ∧ actions.count (.resume r) = 0) ∧
    (∀ r, Act.capture r .linear ∈ actions → actions.count (.resume r) = 1 ∧ actions.count (.consume r) = 0) :=
  total_linear_exhaustion_per_kind typed closed (erase_annotated_steps steps) finished

def resourceGhost : CheckedGhost :=
  ⟨Vesper.Metatheory.mintConsume, Vesper.GhostErasure.resourceful_ghost_proof_accepted⟩

def everyStep : Instrumentation := fun _ => [resourceGhost]

theorem recursive_count_with_ghosts :
    AnnotatedSteps exSig everyStep ⟨⟨.app countF (.natV 1), RCtx.empty, 0⟩, [resourceGhost]⟩
      (List.replicate 6 .silent) ⟨⟨.natV 0, RCtx.empty, 0⟩, []⟩ :=
  lift_runtime_steps count_runs [resourceGhost]

theorem stateful_handler_with_ghosts :
    ∃ live next, AnnotatedSteps exSig everyStep ⟨⟨stHandler, RCtx.empty, 0⟩, [resourceGhost]⟩
      stActs ⟨⟨.natV 7, live, next⟩, []⟩ := by
  obtain ⟨live, next, steps⟩ := st_handler_runs
  exact ⟨live, next, lift_runtime_steps steps [resourceGhost]⟩

theorem annotations_cannot_terminate_loop
    (steps : AnnotatedSteps exSig instrument ⟨⟨loop, live, next⟩, pending⟩ actions after) :
    ¬ IsVal after.runtime.e ∧ after.runtime.e ≠ .abort := by
  have unchanged := (loop_diverges (erase_annotated_steps steps)).1
  rw [unchanged]
  constructor
  · intro value; cases value
  · intro impossible; cases impossible

theorem leaking_ghost_cannot_be_instrumented :
    ¬ ∃ proof : CheckedGhost, proof.term = .letSeq .mintLinear (.litNat 0) := by
  rintro ⟨proof, term⟩
  exact Vesper.GhostErasure.ghost_resource_leak_rejected (term ▸ proof.typed)

theorem aborting_ghost_cannot_be_instrumented :
    ¬ ∃ proof : CheckedGhost, proof.term = .raiseFail := by
  rintro ⟨proof, term⟩
  exact Vesper.GhostErasure.ghost_failure_rejected (term ▸ proof.typed)

end Vesper.OperationalGhostBridge

#print axioms Vesper.OperationalGhostBridge.ghost_step_keeps_runtime
#print axioms Vesper.OperationalGhostBridge.runtime_step_erases
#print axioms Vesper.OperationalGhostBridge.annotated_preservation
#print axioms Vesper.OperationalGhostBridge.annotated_steps_trans
#print axioms Vesper.OperationalGhostBridge.checked_ghost_terminates
#print axioms Vesper.OperationalGhostBridge.drain_annotations
#print axioms Vesper.OperationalGhostBridge.annotated_progress
#print axioms Vesper.OperationalGhostBridge.erase_annotated_steps
#print axioms Vesper.OperationalGhostBridge.lift_runtime_steps
#print axioms Vesper.OperationalGhostBridge.erasure_trace_equivalence
#print axioms Vesper.OperationalGhostBridge.instrumentation_noninterference
#print axioms Vesper.OperationalGhostBridge.no_infinite_ghost_stuttering
#print axioms Vesper.OperationalGhostBridge.annotated_type_safety
#print axioms Vesper.OperationalGhostBridge.annotated_linear_exhaustion
#print axioms Vesper.OperationalGhostBridge.annotated_exhaustion_per_kind
#print axioms Vesper.OperationalGhostBridge.recursive_count_with_ghosts
#print axioms Vesper.OperationalGhostBridge.stateful_handler_with_ghosts
#print axioms Vesper.OperationalGhostBridge.annotations_cannot_terminate_loop
#print axioms Vesper.OperationalGhostBridge.leaking_ghost_cannot_be_instrumented
#print axioms Vesper.OperationalGhostBridge.aborting_ghost_cannot_be_instrumented
