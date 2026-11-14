import ResourceModel42.Syntax

/-! Computable finite type and ownership inference. Invocation values are not
inputs to checking. This module does not assume source/compiler refinement. -/
namespace Vesper.ResourceModel42

abbrev Context := List TypeInfo
abbrev Usage := List Bool

inductive CheckErrorKind where
  | unknownVariable | typeMismatch | duplicateUse | unusedLinear
  | fieldModality | literalOutOfRange | branchOwnership
  deriving DecidableEq
structure CheckError where
  kind : CheckErrorKind
  site : Site
  slot : Option Nat := none
  deriving DecidableEq
inductive CheckProfile where
  | typeBound (site : Site) (error : TypeError)
  | context (site : Site)
  | affineBranchDemand (site : Site) (slot : Nat)
  deriving DecidableEq
inductive CheckFailure where
  | semantic (error : CheckError)
  | profile (reason : CheckProfile)
  deriving DecidableEq
structure InferInfo where
  result : TypeInfo
  uses : Usage

def failCheck (kind : CheckErrorKind) (site : Site) (slot : Option Nat := none) :
    Except CheckFailure α := .error (.semantic ⟨kind, site, slot⟩)

def zeroUsage : Context → Usage
  | [] => []
  | _ :: rest => false :: zeroUsage rest

def singletonUsage : Context → Nat → Usage
  | [], _ => []
  | _ :: rest, 0 => true :: zeroUsage rest
  | _ :: rest, index + 1 => false :: singletonUsage rest index

/-- Demand is a union even for copy locals: a true copy bit records a real
reference, while sequential overlap is rejected only for noncopy locals. -/
def usageUnion : Context → Usage → Usage → Usage
  | [], _, _ => []
  | _ :: rest, left, right =>
      (left.headD false || right.headD false) :: usageUnion rest left.tail right.tail

def firstOverlap : Context → Usage → Usage → Nat → Option Nat
  | [], _, _, _ => none
  | info :: rest, left, right, index =>
      if left.headD false && right.headD false && decide (info.ty.kind ≠ .copy)
      then some index
      else firstOverlap rest left.tail right.tail (index + 1)

def mergeUsage (context : Context) (left right : Usage) (site : Site) :
    Except CheckFailure Usage :=
  match firstOverlap context left right 0 with
  | some index => failCheck .duplicateUse site (some index)
  | none => .ok (usageUnion context left right)

def firstDemandMismatch (mode : Mode) : Context → Usage → Usage → Nat → Option Nat
  | [], _, _, _ => none
  | info :: rest, left, right, index =>
      if info.ty.kind = mode ∧ left.headD false ≠ right.headD false
      then some index
      else firstDemandMismatch mode rest left.tail right.tail (index + 1)

/-- Linear disagreement has global priority; affine-only disagreement is a
conservative profile refusal, not a semantic full-language ownership error. -/
def mergeBranchUsage (context : Context) (left right : Usage) (site : Site) :
    Except CheckFailure Usage :=
  match firstDemandMismatch .linear context left right 0 with
  | some index => failCheck .branchOwnership site (some index)
  | none =>
      match firstDemandMismatch .affine context left right 0 with
      | some index => .error (.profile (.affineBranchDemand site index))
      | none => .ok (usageUnion context left right)

def checkContext (limits : Limits) (context : Context) (added : Nat) (site : Site) :
    Except CheckFailure Unit :=
  if context.length + added > limits.context then .error (.profile (.context site)) else .ok ()

def checkBinderUse (info : TypeInfo) (used : Bool) (site : Site) (index : Nat) :
    Except CheckFailure Unit :=
  if info.ty.kind = .linear ∧ used = false
  then failCheck .unusedLinear site (some index) else .ok ()

def liftType (site : Site) (result : Except TypeError TypeInfo) :
    Except CheckFailure TypeInfo :=
  match result with
  | .ok info => .ok info
  | .error .fieldMode => failCheck .fieldModality site
  | .error error => .error (.profile (.typeBound site error))

/-- Even public inference helpers cannot use an info created under a larger
configuration to evade the current result-type bounds. TypeInfo metrics are
proved equal to its actual type metrics by Syntax. -/
def checkTypeBounds (limits : Limits) (info : TypeInfo) (site : Site) :
    Except CheckFailure Unit := do
  if info.nodes > limits.typeNodes then throw (.profile (.typeBound site .nodes))
  if info.depth > limits.typeDepth then throw (.profile (.typeBound site .depth))

def inferAtom (limits : Limits) (context : Context) (atom : SourceAtom) :
    Except CheckFailure InferInfo := do
  checkContext limits context 0 atom.site
  match atom with
  | .unit site =>
      let info ← liftType site (TypeInfo.ofTy limits .unit)
      pure ⟨info, zeroUsage context⟩
  | .nat site n =>
      if n ≥ MetatheoryFull.u64Bound then failCheck .literalOutOfRange site
      else
        let info ← liftType site (TypeInfo.ofTy limits .nat)
        pure ⟨info, zeroUsage context⟩
  | .var site index =>
      match context[index]? with
      | none => failCheck .unknownVariable site (some index)
      | some info =>
          checkTypeBounds limits info site
          pure ⟨info, singletonUsage context index⟩

/-- Recursive inference follows both arms and every strict initializer. Local
modes come only from the actual inferred type kind. No invocation values,
accepted flags, split proofs, or host ownership masks occur in this API. -/
def infer (limits : Limits) (context : Context) : SourceExpr → Except CheckFailure InferInfo
  | .atom atom => inferAtom limits context atom
  | .mint site => do
      checkContext limits context 0 site
      let info ← liftType site (TypeInfo.ofTy limits .ticket)
      pure ⟨info, zeroUsage context⟩
  | .consume site atom => do
      let value ← inferAtom limits context atom
      if value.result.ty ≠ .ticket then failCheck .typeMismatch site
      else
        let info ← liftType site (TypeInfo.ofTy limits .unit)
        pure ⟨info, value.uses⟩
  | .pair site floor left leftMode right rightMode => do
      let a ← inferAtom limits context left
      let b ← inferAtom limits context right
      let uses ← mergeUsage context a.uses b.uses site
      -- Syntax prepays expanded nodes/depth before constructing the pair.
      let result ← liftType site (TypeInfo.pair limits floor a.result leftMode b.result rightMode)
      pure ⟨result, uses⟩
  | .letE site initializer body => do
      let value ← infer limits context initializer
      checkContext limits context 1 site
      let result ← infer limits (value.result :: context) body
      checkBinderUse value.result (result.uses.headD false) site 0
      let uses ← mergeUsage context value.uses result.uses.tail site
      pure ⟨result.result, uses⟩
  | .unpack site atom body => do
      let value ← inferAtom limits context atom
      match value.result.ty with
      | .pair _ left _ right _ =>
          let leftInfo ← liftType site (TypeInfo.ofTy limits left)
          let rightInfo ← liftType site (TypeInfo.ofTy limits right)
          checkContext limits context 2 site
          -- Full binds the left component at one and the right at zero.
          let result ← infer limits (rightInfo :: leftInfo :: context) body
          checkBinderUse leftInfo (result.uses.tail.headD false) site 1
          checkBinderUse rightInfo (result.uses.headD false) site 0
          let uses ← mergeUsage context value.uses (result.uses.drop 2) site
          pure ⟨result.result, uses⟩
      | _ => failCheck .typeMismatch site
  | .ifz site atom zero nonzero => do
      let condition ← inferAtom limits context atom
      if condition.result.ty ≠ .nat then failCheck .typeMismatch site
      else
        let a ← infer limits context zero
        let b ← infer limits context nonzero
        if a.result.ty ≠ b.result.ty then failCheck .typeMismatch site
        else
          let arms ← mergeBranchUsage context a.uses b.uses site
          let uses ← mergeUsage context condition.uses arms site
          pure ⟨a.result, uses⟩

structure CheckedProgram where
  private mk ::
  private validated : ValidatedProgram
  private inferred : InferInfo
  private checked : infer validated.limits validated.inputInfo.toList validated.raw.body = .ok inferred

def CheckedProgram.program (program : CheckedProgram) : ValidatedProgram := program.validated
def CheckedProgram.info (program : CheckedProgram) : InferInfo := program.inferred
def CheckedProgram.resultType (program : CheckedProgram) : Ty := program.inferred.result.ty
def CheckedProgram.uses (program : CheckedProgram) : Usage := program.inferred.uses

theorem CheckedProgram.check_ok (program : CheckedProgram) :
    infer program.program.limits program.program.inputInfo.toList program.program.raw.body = .ok program.info :=
  program.checked

def checkProgram (program : ValidatedProgram) : Except CheckFailure CheckedProgram :=
  match h : infer program.limits program.inputInfo.toList program.raw.body with
  | .error error => .error error
  | .ok info => .ok ⟨program, info, h⟩

end Vesper.ResourceModel42
