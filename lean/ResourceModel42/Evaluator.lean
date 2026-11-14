import ResourceModel42.Checker

/-! Bounded logical evaluation. These are model actions, not backend observations. -/
namespace Vesper.ResourceModel42

structure LedgerEntry where
  mintSite : Site
  live : Bool
  deriving DecidableEq, Repr

structure Config where
  expr : RuntimeExpr
  ledger : Array LedgerEntry := #[]
  deriving DecidableEq, Repr

def Config.live (c : Config) : MetatheoryFull.RCtx := fun id =>
  match c.ledger[id]? with
  | some entry => if entry.live then some .linear else none
  | none => none

def Config.erase (c : Config) : MetatheoryFull.Conf :=
  ⟨c.expr.erase, c.live, c.ledger.size⟩

inductive EventAction where
  | mint | consume
  deriving DecidableEq, Repr

structure ResourceEvent where
  action : EventAction
  site : Site
  id : Nat
  deriving DecidableEq, Repr

inductive InvariantError where
  | openAtom | expectedPair | expectedNat | expectedHandle
  | unknownHandle (id : Nat) | deadHandle (id : Nat)
  deriving DecidableEq, Repr

inductive ResourceRefusal where
  | capacity (reason : CapacityError)
  | resources (requested cap : Nat)
  | events (requested cap : Nat)
  deriving DecidableEq, Repr

structure Focus where
  context : RuntimeContext
  redex : RuntimeExpr
  deriving DecidableEq, Repr

-- A bounded context search, not a semantic/fuel transition.
def locate : Nat → RuntimeExpr → Except CapacityError Focus
  | 0, _ => .error .runtimeDepth
  | fuel + 1, .letE initializer body =>
      match initializer.asValue with
      | some _ => .ok ⟨.hole, .letE initializer body⟩
      | none => do
          let inner ← locate fuel initializer
          pure ⟨.letC inner.context body, inner.redex⟩
  | _ + 1, e => .ok ⟨.hole, e⟩

-- Plans only retain existing terms/values; substitution is delayed until all
-- whole-state and temporary-state metric checks have succeeded.
inductive Replacement where
  | term (value : RuntimeExpr)
  | substitute (value : RuntimeValue) (body : RuntimeExpr)
  | unpack (left right : RuntimeValue) (body : RuntimeExpr)
  | handle (id : Nat)
  | unit
  deriving DecidableEq, Repr

def Replacement.materialize : Replacement → RuntimeExpr
  | .term e => e
  | .substitute value body => body.subst 0 value
  | .unpack left right body => (body.subst 0 right).subst 0 left
  | .handle id => .atom (.val (.hdl id))
  | .unit => .atom (.val .unit)

def Replacement.preflight (l : Limits) (context : RuntimeContext) :
    Replacement → Except CapacityError ExprStats
  | .term e => do context.preflightPlug l (← e.measure l)
  | .substitute value body => do
      context.preflightPlug l (← body.preflightSubst l 0 value)
  | .unpack left right body => do
      let (temporary, result) ← body.preflightUnpack l left right
      let _ ← context.preflightPlug l temporary
      context.preflightPlug l result
  | .handle _ | .unit => do
      let stats ← checkExprStats l (valueExprStats ⟨1, 1⟩)
      context.preflightPlug l stats

inductive PlannedAction where
  | silent | mint (site : Site) | consume (site : Site) (id : Nat)
  deriving DecidableEq, Repr

structure Prepared where
  replacement : Replacement
  action : PlannedAction
  deriving DecidableEq, Repr

def prepare (ledger : Array LedgerEntry) : RuntimeExpr → Except InvariantError Prepared
  | .letE initializer body =>
      match initializer.asValue with
      | some value => .ok ⟨.substitute value body, .silent⟩
      | none => .error .openAtom
  | .unpack (.val (.pair left right)) body => .ok ⟨.unpack left right body, .silent⟩
  | .unpack _ _ => .error .expectedPair
  | .ifz (.val (.nat n)) zero nonzero =>
      .ok ⟨.term (if n = 0 then zero else nonzero), .silent⟩
  | .ifz _ _ _ => .error .expectedNat
  | .mint site => .ok ⟨.handle ledger.size, .mint site⟩
  | .consume site (.val (.hdl id)) =>
      match ledger[id]? with
      | none => .error (.unknownHandle id)
      | some entry => if entry.live then .ok ⟨.unit, .consume site id⟩
          else .error (.deadHandle id)
  | .consume _ _ => .error .expectedHandle
  | .atom _ | .pair _ _ => .error .openAtom

def PlannedAction.preflight (l : Limits) (ledgerSize eventCount : Nat) :
    PlannedAction → Except ResourceRefusal Unit
  | .silent => .ok ()
  | .mint _ => do
      if ledgerSize + 1 > l.resources then throw (.resources (ledgerSize + 1) l.resources)
      if eventCount + 1 > l.events then throw (.events (eventCount + 1) l.events)
      pure ()
  | .consume _ _ => do
      if eventCount + 1 > l.events then throw (.events (eventCount + 1) l.events)
      pure ()

def PlannedAction.full (ledgerSize : Nat) : PlannedAction → MetatheoryFull.Act
  | .silent => .silent
  | .mint _ => .mint ledgerSize
  | .consume _ id => .consume id

def PlannedAction.event (ledgerSize : Nat) : PlannedAction → Option ResourceEvent
  | .silent => none
  | .mint site => some ⟨.mint, site, ledgerSize⟩
  | .consume site id => some ⟨.consume, site, id⟩

def PlannedAction.apply (ledger : Array LedgerEntry) : PlannedAction → Array LedgerEntry
  | .silent => ledger
  | .mint site => ledger.push ⟨site, true⟩
  | .consume _ id => ledger.modify id (fun entry => {entry with live := false})

inductive StepResult where
  | terminal (value : RuntimeValue)
  | next (config : Config) (action : MetatheoryFull.Act) (event : Option ResourceEvent)
  | limited (reason : ResourceRefusal)
  | invariant (reason : InvariantError)
  deriving Repr

-- `step` is an unfoldable proof helper. The admitted public evaluator below
-- starts only from a checked program and a separately validated invocation.
def step (l : Limits) (eventCount : Nat) (c : Config) : StepResult :=
  match c.expr.asValue with
  | some value => .terminal value
  | none =>
      match locate l.runtimeDepth c.expr with
      | .error error => .limited (.capacity error)
      | .ok focus =>
          match prepare c.ledger focus.redex with
          | .error error => .invariant error
          | .ok plan =>
              match plan.replacement.preflight l focus.context with
              | .error error => .limited (.capacity error)
              | .ok _ =>
                  match plan.action.preflight l c.ledger.size eventCount with
                  | .error error => .limited error
                  | .ok _ =>
                      .next ⟨focus.context.plug plan.replacement.materialize,
                          plan.action.apply c.ledger⟩
                        (plan.action.full c.ledger.size) (plan.action.event c.ledger.size)

inductive EvalStatus where
  | completed (value : RuntimeValue)
  | fuelExhausted
  | resourceLimit (reason : ResourceRefusal)
  | invariantFailure (reason : InvariantError)
  deriving Repr

structure EvalResult where
  status : EvalStatus
  config : Config
  eventsRev : List ResourceEvent
  actionsRev : List MetatheoryFull.Act
  deriving Repr

def EvalResult.events (r : EvalResult) : List ResourceEvent := r.eventsRev.reverse
def EvalResult.actions (r : EvalResult) : List MetatheoryFull.Act := r.actionsRev.reverse
def EvalResult.residual (r : EvalResult) : List (Nat × LedgerEntry) :=
  (r.config.ledger.toList.zipIdx).filterMap fun (entry, id) =>
    if entry.live then some (id, entry) else none

-- The recursion is bounded by admitted global fuel. Reversed lists avoid
-- repeatedly copying a growing successful prefix. No failure appends cleanup.
def runMachine (l : Limits) : Nat → Config → List ResourceEvent → List MetatheoryFull.Act → EvalResult
  | fuel, c, events, actions =>
      match c.expr.asValue with
      | some value => ⟨.completed value, c, events, actions⟩
      | none =>
          match fuel with
          | 0 => ⟨.fuelExhausted, c, events, actions⟩
          | remaining + 1 =>
              match step l events.length c with
              | .terminal value => ⟨.completed value, c, events, actions⟩
              | .limited error => ⟨.resourceLimit error, c, events, actions⟩
              | .invariant error => ⟨.invariantFailure error, c, events, actions⟩
              | .next next action event =>
                  runMachine l remaining next
                    (match event with | none => events | some e => e :: events)
                    (action :: actions)

inductive InputError where
  | shape | naturalRange | handle | depth | noncopy
  deriving DecidableEq, Repr

inductive InvocationError where
  | inputCount (expected actual : Nat)
  | inputValue (index : Nat) (reason : InputError)
  | capacity (reason : CapacityError)
  | fuelBound (requested cap : Nat)
  deriving DecidableEq, Repr

-- Types and values have independent prior metric bounds; this shape/range
-- traversal is additionally depth-bounded and never admits resource input IDs.
def checkInputValue : Nat → Ty → RuntimeValue → Except InputError Unit
  | 0, _, _ => .error .depth
  | _ + 1, .unit, .unit => .ok ()
  | _ + 1, .nat, .nat n =>
      if n < MetatheoryFull.u64Bound then .ok () else .error .naturalRange
  | fuel + 1, .pair _ left _ right _, .pair a b => do
      checkInputValue fuel left a
      checkInputValue fuel right b
  | _ + 1, _, .hdl _ => .error .handle
  | _ + 1, _, _ => .error .shape

def checkInputList (l : Limits) : Nat → List Ty → List RuntimeValue → Except InvocationError Unit
  | _, [], [] => .ok ()
  | index, ty :: types, value :: values => do
      if ty.kind ≠ .copy then throw (.inputValue index .noncopy)
      match checkInputValue l.valueDepth ty value with
      | .error error => throw (.inputValue index error)
      | .ok _ => pure ()
      checkInputList l (index + 1) types values
  | _, types, values => .error (.inputCount types.length values.length)

def admitInputs (l : Limits) (types : Array Ty) (values : Array RuntimeValue)
    (body : RuntimeExpr) : Except InvocationError ExprStats := do
  if values.size ≠ types.size then throw (.inputCount types.size values.size)
  -- Measure each supplied value and the complete closing term before copying
  -- input lets or starting an execution/ledger epoch.
  let stats ← match preflightInputs l values body with
    | .error error => .error (.capacity error)
    | .ok stats => .ok stats
  checkInputList l 0 types.toList values.toList
  pure stats

structure Invocation where
  private mk ::
  program : CheckedProgram
  values : Array RuntimeValue
  stats : ExprStats
  admission_ok : admitInputs program.program.limits program.program.raw.inputTypes values
    program.program.raw.body.toRuntime = .ok stats

def validateInvocation (program : CheckedProgram) (values : Array RuntimeValue) :
    Except InvocationError Invocation :=
  match h : admitInputs program.program.limits program.program.raw.inputTypes values
      program.program.raw.body.toRuntime with
  | .error error => .error error
  | .ok stats => .ok ⟨program, values, stats, h⟩

def Invocation.initial (invocation : Invocation) : Config :=
  ⟨closeInputs invocation.values invocation.program.program.raw.body.toRuntime, #[]⟩

def evalSteps (invocation : Invocation) (fuel : Nat) : Except InvocationError EvalResult :=
  let limits := invocation.program.program.limits
  if fuel > limits.fuel then .error (.fuelBound fuel limits.fuel)
  else .ok (runMachine limits fuel invocation.initial [] [])

end Vesper.ResourceModel42
