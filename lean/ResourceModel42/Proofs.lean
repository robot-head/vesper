import ResourceModel42.ErasureProofs
import ResourceModel42.LedgerProofs
import ResourceModel42.CheckerProofs
import ResourceModel42.EvaluatorProofs

/-! Checked input closure and Full-derived safety for the executable fragment.
All named theorems are public and included in the model audit catalog.
-/
namespace Vesper.ResourceModel42.Proofs
open ErasureProofs

/-- The finite declared input context uses only actual inferred type kinds. -/
def inputContext (context : Context) : MetatheoryFull.Ctx := fun index =>
  (context[index]?).map (fun info => (info.ty.erase, info.ty.kind))

theorem inputContext_nil : inputContext [] = MetatheoryFull.Ctx.empty := by
  funext i
  rfl

theorem inputContext_cons (info : TypeInfo) (context : Context) :
    inputContext (info :: context) = (inputContext context).ext (info.ty.erase, info.ty.kind) := by
  funext i
  cases i <;> rfl

theorem context_get_mem {context : Context} {index : Nat} {info : TypeInfo}
    (h : context[index]? = some info) : info ∈ context := by
  induction context generalizing index with
  | nil => cases h
  | cons head tail ih =>
    cases index with
    | zero => cases h; exact List.mem_cons_self
    | succ index => exact List.mem_cons_of_mem _ (ih h)

/-- Actual successful input checking yields Full value typing. The checker
cannot receive this proposition as a replacement for checking input data. -/
theorem checkInputValue_hasType {fuel : Nat} {ty : Ty} {value : RuntimeValue}
    (wf : ty.WF) (checked : checkInputValue fuel ty value = .ok ())
    (Sig : Nat → MetatheoryFull.EffSig) :
    MetatheoryFull.HasType Sig MetatheoryFull.Ctx.empty MetatheoryFull.RCtx.empty
      value.erase ty.erase ty.kind [] := by
  induction fuel generalizing ty value with
  | zero => cases checked
  | succ fuel ih =>
    cases ty with
    | unit =>
      cases value with
      | unit => exact .unitV MetatheoryFull.discard_empty MetatheoryFull.discardR_empty
      | nat _ | hdl _ | pair _ _ => simp [checkInputValue] at checked
    | nat =>
      cases value with
      | nat n => exact .natV n MetatheoryFull.discard_empty MetatheoryFull.discardR_empty
      | unit | hdl _ | pair _ _ => simp [checkInputValue] at checked
    | ticket => cases value <;> simp [checkInputValue] at checked
    | pair floor left leftMode right rightMode =>
      cases value with
      | unit | nat _ | hdl _ => simp [checkInputValue] at checked
      | pair a b =>
        cases wf with
        | pair wa wb ma mb =>
          cases first : checkInputValue fuel left a with
          | error error => simp [checkInputValue, first] at checked; cases checked
          | ok result =>
            cases result
            have second : checkInputValue fuel right b = .ok () := by
              simpa [checkInputValue, first, Except.bind] using checked
            exact .pair (.val (runtimeValue_isVal a)) (.val (runtimeValue_isVal b))
              (MetatheoryFull.split_left_all _) (MetatheoryFull.splitR_empty_left _)
              (ih wa first) (ih wb second) ma mb (MetatheoryFull.Mode.le_refl _)

def InputTyped (info : TypeInfo) (value : RuntimeValue) : Prop :=
  ∀ Sig, MetatheoryFull.HasType Sig MetatheoryFull.Ctx.empty MetatheoryFull.RCtx.empty
    value.erase info.ty.erase info.ty.kind []

inductive TypedInputs : Context → List RuntimeValue → Prop where
  | nil : TypedInputs [] []
  | cons {info : TypeInfo} {value : RuntimeValue} {context : Context} {values : List RuntimeValue} :
      InputTyped info value → TypedInputs context values → TypedInputs (info :: context) (value :: values)

theorem checkInputList_typed {limits : Limits} {index : Nat}
    {context : Context} {values : List RuntimeValue}
    (checked : checkInputList limits index (context.map TypeInfo.ty) values = .ok ()) :
    TypedInputs context values := by
  induction context generalizing index values with
  | nil =>
    cases values with
    | nil => exact .nil
    | cons _ _ => simp [checkInputList] at checked
  | cons info rest ih =>
    cases values with
    | nil => simp [checkInputList] at checked
    | cons value values =>
      by_cases copy : info.ty.kind = .copy
      · cases first : checkInputValue limits.valueDepth info.ty value with
        | error error => simp [checkInputList, copy, first] at checked; cases checked
        | ok result =>
          cases result
          have tail : checkInputList limits (index+1) (rest.map TypeInfo.ty) values = .ok () := by
            simpa [checkInputList, copy, first, Except.bind] using checked
          exact .cons (fun Sig => checkInputValue_hasType info.wf first Sig) (ih tail)
      · simp [checkInputList, copy] at checked; cases checked

theorem admitInputs_checked {limits : Limits} {types : Array Ty} {values : Array RuntimeValue}
    {body : RuntimeExpr} {stats : ExprStats}
    (accepted : admitInputs limits types values body = .ok stats) :
    checkInputList limits 0 types.toList values.toList = .ok () := by
  by_cases count : values.size = types.size
  · cases measured : preflightInputs limits values body with
    | error error => simp [admitInputs, count, measured] at accepted; cases accepted
    | ok result =>
      cases inputs : checkInputList limits 0 types.toList values.toList with
      | error error => simp [admitInputs, count, measured, inputs] at accepted; cases accepted
      | ok result => cases result; rfl
  · simp [admitInputs, count] at accepted; cases accepted

theorem invocation_inputs_typed (invocation : Invocation) :
    TypedInputs invocation.program.program.inputInfo.toList invocation.values.toList := by
  apply checkInputList_typed (index := 0) (limits := invocation.program.program.limits)
  rw [ValidatedProgram.inputInfo_types]
  exact admitInputs_checked invocation.admission_ok

/-- This is pure input closure, not a claim that input invocation values are
static facts. The innermost binding is input zero, exactly as closeInputs. -/
theorem closeInputList_hasType (Sig : Nat → MetatheoryFull.EffSig)
    {context : Context} {values : List RuntimeValue}
    (inputs : TypedInputs context values) :
    ∀ {body : RuntimeExpr} {ty : MetatheoryFull.Ty} {mode : Mode},
    MetatheoryFull.HasType Sig (inputContext context) MetatheoryFull.RCtx.empty body.erase ty mode [] →
    MetatheoryFull.HasType Sig MetatheoryFull.Ctx.empty MetatheoryFull.RCtx.empty
      (values.foldl (fun e v => .letE (.atom (.val v)) e) body).erase ty mode [] := by
  induction inputs with
  | nil => intro body ty mode typed; simpa [inputContext_nil] using typed
  | @cons info value rest values first tail ih =>
    intro body ty mode typed
    have closed : MetatheoryFull.HasType Sig (inputContext rest) MetatheoryFull.RCtx.empty
        (RuntimeExpr.letE (.atom (.val value)) body).erase ty mode [] := by
      rw [inputContext_cons] at typed
      have split : MetatheoryFull.Split (inputContext rest) MetatheoryFull.Ctx.empty
          (inputContext rest) := by
        intro i
        cases entry : inputContext rest i with
        | none => exact .none
        | some binding => exact .right binding
      have frame : MetatheoryFull.FrameOK (inputContext rest) MetatheoryFull.RCtx.empty
          (MetatheoryFull.res Sig []) :=
        ⟨fun _ _ mode _ => MetatheoryFull.Mode.le_linear mode,
          fun _ mode _ => MetatheoryFull.Mode.le_linear mode.toMode⟩
      exact .letE split (MetatheoryFull.splitR_empty_left _)
        (first Sig) typed frame (MetatheoryFull.rowSub_refl _) (MetatheoryFull.rowSub_refl _)
    exact ih closed


theorem checked_body_hasType (program : CheckedProgram) (Sig : Nat → MetatheoryFull.EffSig) :
    MetatheoryFull.HasType Sig (inputContext program.program.inputInfo.toList)
      MetatheoryFull.RCtx.empty program.program.raw.body.toRuntime.erase
      program.resultType.erase program.resultType.kind [] := by
  rw [sourceExpr_erase_toRuntime]
  exact CheckerProofs.checkedProgram_declared_sound program Sig

theorem invocation_initial_hasType (invocation : Invocation) (Sig : Nat → MetatheoryFull.EffSig) :
    MetatheoryFull.HasType Sig MetatheoryFull.Ctx.empty MetatheoryFull.RCtx.empty
      invocation.initial.expr.erase invocation.program.resultType.erase invocation.program.resultType.kind [] := by
  exact closeInputList_hasType Sig (invocation_inputs_typed invocation)
    (checked_body_hasType invocation.program Sig)

theorem invocation_initial_live (invocation : Invocation) :
    invocation.initial.live = MetatheoryFull.RCtx.empty := by
  funext id
  simp [Invocation.initial, Config.live, MetatheoryFull.RCtx.empty]

theorem invocation_initial_WT (invocation : Invocation) (Sig : Nat → MetatheoryFull.EffSig) :
    MetatheoryFull.WT Sig invocation.initial.erase
      invocation.program.resultType.erase invocation.program.resultType.kind [] := by
  constructor
  · simpa only [Config.erase, invocation_initial_live] using invocation_initial_hasType invocation Sig
  · exact LedgerProofs.erase_fresh _


theorem invocation_initial_erase (invocation : Invocation) :
    invocation.initial.erase =
      ⟨invocation.initial.expr.erase, MetatheoryFull.RCtx.empty, 0⟩ := by
  change (⟨invocation.initial.expr.erase, invocation.initial.live, invocation.initial.ledger.size⟩ :
    MetatheoryFull.Conf) = _
  rw [invocation_initial_live]
  rfl

/-- This equality is obtained from the actual public evaluator call, not a
caller-supplied execution relation. -/
theorem evalSteps_result {invocation : Invocation} {fuel : Nat} {result : EvalResult}
    (ran : evalSteps invocation fuel = .ok result) :
    result = runMachine invocation.program.program.limits fuel invocation.initial [] [] := by
  by_cases bound : fuel > invocation.program.program.limits.fuel
  · simp [evalSteps, bound] at ran
  · simpa [evalSteps, bound] using ran.symm

theorem evalSteps_prefix (Sig : Nat → MetatheoryFull.EffSig)
    {invocation : Invocation} {fuel : Nat} {result : EvalResult}
    (ran : evalSteps invocation fuel = .ok result) :
    MetatheoryFull.Steps Sig invocation.initial.erase result.actions result.config.erase := by
  rw [evalSteps_result ran]
  exact EvaluatorProofs.runMachine_steps Sig _ _ _

theorem evalSteps_trace_actions {invocation : Invocation} {fuel : Nat} {result : EvalResult}
    (ran : evalSteps invocation fuel = .ok result) :
    result.events.map EvaluatorProofs.eventAct = result.actions.filter EvaluatorProofs.resourceAction := by
  rw [evalSteps_result ran]
  exact EvaluatorProofs.runMachine_trace_actions _ _ _

theorem evalSteps_preservation (Sig : Nat → MetatheoryFull.EffSig)
    {invocation : Invocation} {fuel : Nat} {result : EvalResult}
    (ran : evalSteps invocation fuel = .ok result) :
    MetatheoryFull.WT Sig result.config.erase invocation.program.resultType.erase
      invocation.program.resultType.kind [] :=
  (MetatheoryFull.type_safety (evalSteps_prefix Sig ran) (invocation_initial_WT invocation Sig)
    (fun _ member => by cases member)).1

theorem evalSteps_completed_isVal {invocation : Invocation} {fuel : Nat} {result : EvalResult}
    {value : RuntimeValue} (ran : evalSteps invocation fuel = .ok result)
    (completed : result.status = .completed value) : MetatheoryFull.IsVal result.config.expr.erase := by
  have actual := evalSteps_result ran
  rw [actual] at completed ⊢
  exact EvaluatorProofs.runMachine_completed_isVal _ _ _ _ _ _ completed

theorem evalSteps_completed_value {invocation : Invocation} {fuel : Nat} {result : EvalResult}
    {value : RuntimeValue} (ran : evalSteps invocation fuel = .ok result)
    (completed : result.status = .completed value) : result.config.expr.erase = value.erase := by
  have actual := evalSteps_result ran
  rw [actual] at completed ⊢
  exact EvaluatorProofs.completed_value_erase _ _ _ _ _ _ completed

theorem evalSteps_noInvariant (Sig : Nat → MetatheoryFull.EffSig)
    {invocation : Invocation} {fuel : Nat} {result : EvalResult}
    (ran : evalSteps invocation fuel = .ok result) (error : InvariantError) :
    result.status ≠ .invariantFailure error := by
  rw [evalSteps_result ran]
  exact EvaluatorProofs.runMachine_noInvariant Sig _ _ _ _ _ (invocation_initial_WT invocation Sig) error

theorem evalSteps_reachable_step_noInvariant (Sig : Nat → MetatheoryFull.EffSig)
    {invocation : Invocation} {fuel : Nat} {result : EvalResult}
    (ran : evalSteps invocation fuel = .ok result) (count : Nat) (error : InvariantError) :
    step invocation.program.program.limits count result.config ≠ .invariant error :=
  EvaluatorProofs.step_noInvariant Sig (evalSteps_preservation Sig ran).1 (fun _ _ h => h) error

theorem runtimeValue_not_abort (value : RuntimeValue) : value.erase ≠ MetatheoryFull.Expr.abort := by
  cases value <;> simp [RuntimeValue.erase]

theorem runtimeAtom_not_abort (atom : RuntimeAtom) : atom.erase ≠ MetatheoryFull.Expr.abort := by
  cases atom <;> simp [RuntimeAtom.erase, runtimeValue_not_abort]

theorem runtimeExpr_not_abort (expression : RuntimeExpr) : expression.erase ≠ MetatheoryFull.Expr.abort := by
  cases expression <;> simp [RuntimeExpr.erase, runtimeAtom_not_abort]

/-- Progress is semantic Full progress. The separate metric/refusal layer
justifies actual capacity stops rather than claiming a successful next step. -/
theorem evalSteps_Full_progress (Sig : Nat → MetatheoryFull.EffSig)
    {invocation : Invocation} {fuel : Nat} {result : EvalResult}
    (ran : evalSteps invocation fuel = .ok result) :
    MetatheoryFull.IsVal result.config.expr.erase ∨
      ∃ action next, MetatheoryFull.Step Sig result.config.erase action next := by
  have progress := (MetatheoryFull.type_safety (evalSteps_prefix Sig ran)
    (invocation_initial_WT invocation Sig) (fun _ member => by cases member)).2
  rcases progress with value | aborted | next
  · exact Or.inl value
  · exact False.elim (runtimeExpr_not_abort _ aborted)
  · exact Or.inr next

theorem evalSteps_completed_copy_discard (Sig : Nat → MetatheoryFull.EffSig)
    {invocation : Invocation} {fuel : Nat} {result : EvalResult} {value : RuntimeValue}
    (ran : evalSteps invocation fuel = .ok result) (completed : result.status = .completed value)
    (copy : invocation.program.resultType.kind = .copy) : MetatheoryFull.DiscardR result.config.live :=
  MetatheoryFull.terminal_no_linear (evalSteps_preservation Sig ran)
    (Or.inl ⟨evalSteps_completed_isVal ran completed, copy⟩)

theorem evalSteps_completed_copy_residual (Sig : Nat → MetatheoryFull.EffSig)
    {invocation : Invocation} {fuel : Nat} {result : EvalResult} {value : RuntimeValue}
    (ran : evalSteps invocation fuel = .ok result) (completed : result.status = .completed value)
    (copy : invocation.program.resultType.kind = .copy) : result.residual = [] :=
  LedgerProofs.discard_residual_nil result (evalSteps_completed_copy_discard Sig ran completed copy)

theorem evalSteps_completed_copy_exact_once (Sig : Nat → MetatheoryFull.EffSig)
    {invocation : Invocation} {fuel : Nat} {result : EvalResult} {value : RuntimeValue}
    (ran : evalSteps invocation fuel = .ok result) (completed : result.status = .completed value)
    (copy : invocation.program.resultType.kind = .copy) (id : Nat)
    (minted : MetatheoryFull.Act.mint id ∈ result.actions) :
    MetatheoryFull.relCount id result.actions = 1 := by
  have typing := invocation_initial_hasType invocation Sig
  rw [copy] at typing
  have history := evalSteps_prefix Sig ran
  rw [invocation_initial_erase] at history
  exact MetatheoryFull.total_linear_exhaustion typing (fun _ member => by cases member) history
    (Or.inl (evalSteps_completed_isVal ran completed)) id (Or.inl minted)

def consumeEvent (id : Nat) (event : ResourceEvent) : Bool :=
  match event.action with | .mint => false | .consume => event.id == id

theorem event_release (id : Nat) (event : ResourceEvent) :
    MetatheoryFull.isRel id (EvaluatorProofs.eventAct event) = consumeEvent id event := by
  cases event with
  | mk action site serial => cases action <;> rfl

theorem release_filter (id : Nat) (actions : List MetatheoryFull.Act) :
    MetatheoryFull.relCount id (actions.filter EvaluatorProofs.resourceAction) =
      MetatheoryFull.relCount id actions := by
  unfold MetatheoryFull.relCount
  rw [List.countP_filter]
  congr 1
  funext action
  cases action <;> simp [MetatheoryFull.isRel, EvaluatorProofs.resourceAction]

theorem event_consume_count {events : List ResourceEvent} {actions : List MetatheoryFull.Act}
    (projection : events.map EvaluatorProofs.eventAct = actions.filter EvaluatorProofs.resourceAction)
    (id : Nat) : events.countP (consumeEvent id) = MetatheoryFull.relCount id actions := by
  calc
    events.countP (consumeEvent id) =
        MetatheoryFull.relCount id (events.map EvaluatorProofs.eventAct) := by
      simp only [MetatheoryFull.relCount, List.countP_map, Function.comp_def, event_release]
    _ = MetatheoryFull.relCount id (actions.filter EvaluatorProofs.resourceAction) := by rw [projection]
    _ = MetatheoryFull.relCount id actions := release_filter id actions

theorem event_mint_member {events : List ResourceEvent} {actions : List MetatheoryFull.Act}
    (projection : events.map EvaluatorProofs.eventAct = actions.filter EvaluatorProofs.resourceAction)
    {site id : Nat} (minted : ResourceEvent.mk .mint site id ∈ events) :
    MetatheoryFull.Act.mint id ∈ actions := by
  have member : MetatheoryFull.Act.mint id ∈ events.map EvaluatorProofs.eventAct :=
    List.mem_map.2 ⟨_, minted, rfl⟩
  rw [projection] at member
  exact (List.mem_filter.1 member).1

/-- The public ordered resource trace, not only the hidden silent-step list,
has exactly one consume per observed mint on completed copy-kind roots. -/
theorem evalSteps_public_trace_exact_once (Sig : Nat → MetatheoryFull.EffSig)
    {invocation : Invocation} {fuel : Nat} {result : EvalResult} {value : RuntimeValue}
    (ran : evalSteps invocation fuel = .ok result) (completed : result.status = .completed value)
    (copy : invocation.program.resultType.kind = .copy) {site id : Nat}
    (minted : ResourceEvent.mk .mint site id ∈ result.events) :
    result.events.countP (consumeEvent id) = 1 := by
  rw [event_consume_count (evalSteps_trace_actions ran)]
  exact evalSteps_completed_copy_exact_once Sig ran completed copy id
    (event_mint_member (evalSteps_trace_actions ran) minted)

end Vesper.ResourceModel42.Proofs
