import Lean4Lean.Environment

/-!
Fixed raw Declaration/Expr controls for Vesper's reviewed Lean 4.24 port.
Adapted from upstream Lean4Lean tests at the commits listed in known-fixes.json.
Upstream is Apache-2.0; the base LICENSE remains part of the pinned source.
No invalid case is delegated to native elaboration: every verdict below calls
actual Lean4Lean.addDecl or its checker/primitive entrypoint directly.
-/
namespace Vesper.Lean4Lean42Controls
open Lean Lean4Lean TypeChecker

structure ControlResult where
  id : String
  expectedAccept : Bool
  actualAccept : Bool
  expectedReason : String
  actualReason : String
  matched : Bool
  deriving Repr

private abbrev ControlM := ExceptT String (StateT (Array ControlResult) IO)
private def mentions (pat s : String) : Bool := (s.splitOn pat).length > 1
private def classify : Kernel.Exception → String
  | .declHasFVars .. => "declaration_fvar"
  | .declHasMVars .. => "declaration_mvar"
  | .invalidProj .. => "invalid_projection"
  | .appTypeMismatch .. => "application_type_mismatch"
  | .declTypeMismatch .. => "declaration_type_mismatch"
  | .alreadyDeclared .. => "already_declared"
  | .typeExpected .. => "type_expected"
  | .exprTypeMismatch .. => "expression_type_mismatch"
  | .deepRecursion => "deep_recursion"
  | .other msg =>
    if mentions "same universe level parameters" msg then "mutual_universe_headers"
    else if mentions "duplicate declaration name" msg then "mutual_duplicate_name"
    else if mentions "reserved prefix '_nested'" msg then "reserved_nested_name"
    else if mentions "universe level of type_of" msg then "constructor_universe"
    else if mentions "unexpected use of primitive name" msg then "reserved_primitive_name"
    else if mentions "invalid form for primitive def" msg then "primitive_equation"
    else if mentions "loose bound variables" msg then "loose_bound_variables"
    else if mentions "control condition failed" msg then "condition_rejected"
    else "other"
  | _ => "other"

private def record (id : String) (expected actual : Bool) (expectedReason actualReason : String) :
    ControlM Unit := modify fun rows => rows.push
      ⟨id, expected, actual, expectedReason, actualReason,
        expected == actual && expectedReason == actualReason⟩
private def expectError (id reason : String) (r : Except Kernel.Exception α) : ControlM Unit :=
  match r with
  | .ok _ => record id false true reason "accepted"
  | .error e => record id false false reason (classify e)
private def expectOk (id : String) (r : Except Kernel.Exception α) : ControlM α := do
  match r with
  | .ok a => record id true true "accepted" "accepted"; pure a
  | .error e =>
    record id true false "accepted" (classify e)
    throw ("fixture failure: " ++ id)
private def assertTrue (id : String) (b : Bool) : ControlM Unit :=
  record id true b "condition_true" (if b then "condition_true" else "condition_false")


open Lean

/-- `(fun _ : False → False => False) (fun h : False => h)`.

This beta-reduces to `False`, so it is a legitimate type, but inferring it pushes
`_kernel_fresh.1 : False → False` and `_kernel_fresh.2 : False` through the local context,
leaving them in the inference cache. -/
def cachePrimingType : Expr :=
  let falseE := mkConst ``False
  let falseToFalse := Expr.forallE `h falseE falseE .default
  let identity := Expr.lam `h falseE (.bvar 0) .default
  .app (.lam `_ falseToFalse falseE .default) identity

/-- The value that only the inference cache still knows about. -/
def leakedFVar : Expr := .fvar { name := .num `_kernel_fresh 2 }

def thmDecl (name : Name) (value : Expr) : Declaration :=
  .thmDecl { name, levelParams := [], type := cachePrimingType, value }

def defnDecl (name : Name) (value : Expr) : Declaration :=
  .defnDecl { name, levelParams := [], type := cachePrimingType, value
              hints := .abbrev, safety := .safe }

def opaqueDecl (name : Name) (value : Expr) : Declaration :=
  .opaqueDecl { name, levelParams := [], type := cachePrimingType, value, isUnsafe := false }

/-- Closed counterparts, so that a blanket rejection cannot pass this test. -/
def goodThm : Declaration :=
  .thmDecl { name := `GoodThm, levelParams := [], type := mkConst ``True,
             value := mkConst ``True.intro }
def goodDefn : Declaration :=
  .defnDecl { name := `GoodDefn, levelParams := [], type := mkConst ``Nat,
              value := mkRawNatLit 0, hints := .abbrev, safety := .safe }
def goodOpaque : Declaration :=
  .opaqueDecl { name := `GoodOpaque, levelParams := [], type := mkConst ``Nat,
                value := mkRawNatLit 0, isUnsafe := false }



open Lean

/-- `inductive Tree0 (α : Type) | node : Array (Tree0 α) → Tree0 α` -/
def treeDecl : Declaration :=
  let tree := fun a => mkApp (mkConst `Tree0 []) a
  .inductDecl [] 1
    [{ name := `Tree0
       type := .forallE `α (.sort 1) (.sort 1) .default
       ctors := [{
         name := `Tree0.node
         type := .forallE `α (.sort 1)
           (.forallE `es (mkApp (mkConst ``Array [.zero]) (tree (.bvar 0)))
             (tree (.bvar 1)) .default) .default }] }]
    false

/-- As above, but the dropped parametric argument `Tree0 Bool.true` is ill typed. -/
def badDecl : Declaration :=
  .inductDecl [] 1
    [{ name := `Bad0
       type := .forallE `α (.sort 1) (.sort 1) .default
       ctors := [{
         name := `Bad0.node
         type := .forallE `α (.sort 1)
           (.forallE `es
             (mkApp (mkConst ``Array [.zero])
               (mkApp (mkConst `Bad0 []) (mkConst ``Bool.true)))
             (mkApp (mkConst `Bad0 []) (.bvar 1)) .default) .default }] }]
    false

private def mkPartial (n : Name) (lparams : List Name) (type value : Expr) : DefinitionVal :=
  { name := n, levelParams := lparams, type, value, hints := .opaque, safety := .partial }

private def universeTy : Expr :=
  .forallE `x (.sort (.param `u)) (.sort (.param `u)) .default

private def universeVal : Expr :=
  .lam `x (.sort (.param `u)) (.bvar 0) .default

private def imaxProp : Expr := .sort (.imax (.succ .zero) .zero)

private def imaxDataDecl : Declaration :=
  .inductDecl [] 0 [{
    name := `L4LKIPData
    type := imaxProp
    ctors := [{
      name := `L4LKIPData.mk
      type := .forallE `b (.const ``Bool []) (.const `L4LKIPData []) .default }]
  }] false

/-- The auxiliary name the kernel generates for a nested `List` occurrence. -/
private def auxListName : Name := (`_nested ++ `List).appendIndexAfter 1

/-- lean4#14616.  `mk` nests `List L4LKNReal`, so eliminating it makes the kernel generate
`_nested.List_1`; `bad` then names that auxiliary.  This is the form that *discriminates*:
without the check the declaration is accepted, and `restoreNested` rewrites the stored type of
`bad` to `List L4LKNReal → L4LKNReal`, which the kernel never checked.  A declaration naming an
auxiliary that never exists is instead rejected as an unknown constant either way. -/
private def nestedAuxRealDecl : Declaration :=
  .inductDecl [] 0 [{
    name := `L4LKNReal
    type := .sort 1
    ctors := [
      { name := `L4LKNReal.mk
        type := .forallE `xs (.app (.const ``List [.zero]) (.const `L4LKNReal []))
          (.const `L4LKNReal []) .default },
      { name := `L4LKNReal.bad
        type := .forallE `y (.const auxListName []) (.const `L4LKNReal []) .default }]
  }] false

private def nestedAuxProjDecl : Declaration :=
  .inductDecl [] 0 [{
    name := `L4LKNProj
    type := .sort .zero
    ctors := [{
      name := `L4LKNProj.mk
      type := .forallE `x (.const ``Nat [])
        (.forallE `y (.proj `_nested.L4LHost_1 0 (.bvar 0))
          (.const `L4LKNProj []) .default) .default }]
  }] false

private def nestedBadDecl (bad : Expr) (name : Name) : Declaration :=
  let ind := fun a => .app (.const name []) a
  .inductDecl [] 1 [{
    name
    type := .forallE `α (.sort 1) (.sort 1) .default
    ctors := [{
      name := name ++ `mk
      type := .forallE `α (.sort 1)
        (.forallE `xs (.app (.const ``Array [.zero]) (ind bad))
          (ind (.bvar 1)) .default) .default }]
  }] false

/-- lean4#14613: projecting the field back out of a `Sort (imax 1 0)` proof would break proof
irrelevance, so `inferProj` must reject it. -/
private def imaxLeakDecl : Declaration :=
  .defnDecl {
    name := `L4LKIPLeak
    levelParams := []
    type := .forallE `proof (.const `L4LKIPData []) (.const ``Bool []) .default
    value := .lam `proof (.const `L4LKIPData []) (.proj `L4LKIPData 0 (.bvar 0)) .default
    hints := .abbrev, safety := .safe }

structure L4LKC where b : Bool
inductive L4LKW : Type where | mk (p : Bool)
inductive L4LKL (α : Type) (b : Bool) : Type where | mk

/-- lean4#14576/#14577: the parametric arguments of a nested occurrence are dropped from the
auxiliary declaration, so they escape checking unless they are checked against the environment
that results from the declaration. Here `w.1.1` is ill typed. -/
private def nestedIllTypedParams : Declaration :=
  let w : Expr := .bvar 0
  let Ew : Expr := .app (.const `L4LKE []) w
  let b : Expr := .proj ``L4LKC 0 (.proj ``L4LKC 0 w)
  let l : Expr := mkApp2 (.const ``L4LKL []) Ew b
  .inductDecl [] 1 [{
    name := `L4LKE
    type := .forallE `w (.const ``L4LKW []) (.sort 1) .default
    ctors := [{
      name := `L4LKE.mk
      type := .forallE `w (.const ``L4LKW [])
        (.forallE `l l (.app (.const `L4LKE []) (.bvar 1)) .default) .default }]
  }] false

private def deepNat : Nat → Expr
  | 0 => .const ``Nat.zero []
  | n + 1 => .app (.const ``Nat.succ []) (deepNat n)

structure ProjB where b : Nat


private def core (env : Kernel.Environment) : ControlM Unit := do
  for (kind, mk) in [("theorem", thmDecl), ("definition", defnDecl), ("opaque", opaqueDecl)] do
    expectError ("closure.fvar." ++ kind) "declaration_fvar" <|
      Lean4Lean.addDecl env (mk `Bad leakedFVar)
    expectError ("closure.mvar." ++ kind) "declaration_mvar" <|
      Lean4Lean.addDecl env (mk `Bad (.mvar { name := `badMVar }))
  let _ ← expectOk "closure.closed.theorem" (Lean4Lean.addDecl env goodThm)
  let _ ← expectOk "closure.closed.definition" (Lean4Lean.addDecl env goodDefn)
  let _ ← expectOk "closure.closed.opaque" (Lean4Lean.addDecl env goodOpaque)
  let treeEnv ← expectOk "nested.valid.parameter" (Lean4Lean.addDecl env treeDecl)
  let some ctor := treeEnv.find? `Tree0.node | throw "missing nested constructor"
  let _ ← expectOk "nested.valid.stored-constructor-type" <|
    TypeChecker.M.run treeEnv (lparams := ctor.levelParams) (x := checkType ctor.type)
  let some recursor := treeEnv.find? `Tree0.rec | throw "missing nested recursor"
  let _ ← expectOk "nested.valid.stored-recursor-type" <|
    TypeChecker.M.run treeEnv (lparams := recursor.levelParams) (x := checkType recursor.type)
  expectError "nested.discarded.bad-argument" "application_type_mismatch" <|
    Lean4Lean.addDecl env badDecl
  expectError "nested.discarded.double-projection" "invalid_projection" <|
    Lean4Lean.addDecl env nestedIllTypedParams
  expectError "nested.original.fvar" "declaration_fvar" <|
    Lean4Lean.addDecl env (nestedBadDecl (.fvar { name := `l4lBadFVar }) `L4LNestedFVar)
  expectError "nested.original.mvar" "declaration_mvar" <|
    Lean4Lean.addDecl env (nestedBadDecl (.mvar { name := `l4lBadMVar }) `L4LNestedMVar)
  expectError "nested.reserved.actual-auxiliary" "reserved_nested_name" <|
    Lean4Lean.addDecl env nestedAuxRealDecl
  expectError "nested.reserved.projection-name" "reserved_nested_name" <|
    Lean4Lean.addDecl env nestedAuxProjDecl
  let ep ← expectOk "prop.normalized.constructor" (Lean4Lean.addDecl env imaxDataDecl)
  let some (.recInfo recInfo) := ep.find? `L4LKIPData.rec
    | throw "missing normalized-Prop recursor"
  assertTrue "prop.normalized.no-large-elimination" recInfo.levelParams.isEmpty
  assertTrue "prop.normalized.not-k-target" (!recInfo.k)
  expectError "prop.normalized.reject-bool-projection" "invalid_projection" <|
    Lean4Lean.addDecl ep imaxLeakDecl
  expectError "mutual.unequal-universe-headers" "mutual_universe_headers" <|
    Lean4Lean.addDecl env (.mutualDefnDecl [
      mkPartial `L4LMutA [`u] universeTy universeVal,
      mkPartial `L4LMutB [] universeTy universeVal])
  expectError "mutual.duplicate-name-overwrite" "mutual_duplicate_name" <|
    Lean4Lean.addDecl env (.mutualDefnDecl [
      mkPartial `L4LMutDup [] (.const ``Nat []) (mkRawNatLit 0),
      mkPartial `L4LMutDup [] (.const ``Bool []) (.const ``Bool.true [])])
  let _ ← expectOk "mutual.valid-distinct" <|
    Lean4Lean.addDecl env (.mutualDefnDecl [
      mkPartial `L4LMutGoodA [] (.const ``Nat []) (mkRawNatLit 0),
      mkPartial `L4LMutGoodB [] (.const ``Bool []) (.const ``Bool.true [])])
  let b : Expr := .app (.const ``ProjB.mk []) (mkRawNatLit 7)
  let good : Expr := .proj ``ProjB 0 b
  let huge : Expr := .proj ``ProjB 4294967296 b
  let ty ← expectOk "projection.valid.type" <| TypeChecker.M.run env (x := checkType good)
  assertTrue "projection.valid.nat" (ty.isConstOf ``Nat)
  let goodWhnf ← expectOk "projection.valid.whnf" <| TypeChecker.M.run env (x := whnf good)
  assertTrue "projection.valid.value" (goodWhnf == mkRawNatLit 7)
  let hugeWhnf ← expectOk "projection.large-index.stuck" <| TypeChecker.M.run env (x := whnf huge)
  assertTrue "projection.large-index.no-truncation" (hugeWhnf == huge)
  expectError "projection.large-index.type-rejection" "invalid_projection" <|
    TypeChecker.M.run env (x := checkType huge)
  expectError "projection.wrong-structure-name" "invalid_projection" <|
    TypeChecker.M.run env (x := checkType (.proj ``L4LKC 0 b))

end Vesper.Lean4Lean42Controls

namespace Vesper.Lean4Lean42Controls
open Lean Lean4Lean TypeChecker

-- Copy already imported genuine constants, excluding the candidate name.
-- The unverified candidate is never inserted before its actual checks.
open private Lean.Kernel.Environment.mk from Lean.Environment in
private def without (env : Kernel.Environment) (name : Name) : Kernel.Environment :=
  let constants := env.constants.toList.foldl (init := ({} : Std.HashMap Name ConstantInfo))
    fun m (n, ci) => if n == name then m else m.insert n ci
  Lean.Kernel.Environment.mk
    (constants := SMap.fromHashMap constants false) (quotInit := env.quotInit)
    (diagnostics := {}) (const2ModIdx := {}) (extensions := #[]) (irBaseExts := #[])
    (header := { mainModule := `RawControlBaseline })

private def twoNatZero : Expr :=
  .lam `x (.const ``Nat []) (.lam `y (.const ``Nat []) (mkRawNatLit 0) .default) .default
private def threeBitwiseZero : Expr :=
  .lam `f (.arrow (.const ``Bool []) (.arrow (.const ``Bool []) (.const ``Bool [])))
    twoNatZero .default
private def natNatTrue : Expr :=
  .lam `x (.const ``Nat []) (.lam `y (.const ``Nat []) (.const ``Bool.true []) .default) .default

private def levelFoo : Declaration :=
  let v := Level.param `v
  let foo (a : Expr) := mkApp (.const `LevelFoo [v]) a
  .inductDecl [`v] 1 [{
    name := `LevelFoo
    type := .forallE `b (.sort v) (.sort (.max 2 v)) .default
    ctors := [{
      name := `LevelFoo.mk
      type := .forallE `b (.sort v)
        (.forallE `f (.forallE `_ (.sort 1) (.bvar 1) .default)
          (foo (.bvar 1)) .default) .default}]
  }] false
private def universeData (name : Name) (level : Level) : Declaration :=
  .inductDecl [] 0 [{name, type := .sort level, ctors := [{
    name := name ++ `mk
    type := .forallE `α (.sort 1)
      (.forallE `a (.bvar 0) (.const name []) .default) .default}]}] false

private def extended (env : Kernel.Environment) : ControlM Unit := do
  let _ ← expectOk "mutual.valid-shared-universe" <|
    Lean4Lean.addDecl env (.mutualDefnDecl [
      mkPartial `L4LMutUGoodA [`u] universeTy universeVal,
      mkPartial `L4LMutUGoodB [`u] universeTy universeVal])
  for (id, bad, pat) in [("fvar", Expr.fvar {name := `bad}, "declaration_fvar"),
      ("mvar", Expr.mvar {name := `bad}, "declaration_mvar")] do
    expectError ("inductive.original-type." ++ id) pat <|
      Lean4Lean.addDecl env (.inductDecl [] 0 [{
        name := `BadOriginalType, type := bad, ctors := []}] false)
  let .inductDecl ps np [listType] safety := nestedAuxRealDecl | throw "bad fixture"
  let _ ← expectOk "nested.valid-list-auxiliary" <|
    Lean4Lean.addDecl env (.inductDecl ps np [{listType with ctors := listType.ctors.take 1}] safety)
  let ep ← expectOk "prop.eta.fixture" (Lean4Lean.addDecl env imaxDataDecl)
  let proofStays ← expectOk "prop.eta.check" <| TypeChecker.M.run ep (x := do
    withLocalDecl `p (.const `L4LKIPData []) .default fun p => do
      let e ← Lean4Lean.toCtorWhenStruct ep TypeChecker.whnf TypeChecker.inferType `L4LKIPData p
      pure (e == p))
  assertTrue "prop.eta.no-proof-expansion" proofStays
  let dataExpands ← expectOk "projection.eta.data-check" <| TypeChecker.M.run env (x := do
    withLocalDecl `p (.const ``L4LKC []) .default fun p => do
      let e ← Lean4Lean.toCtorWhenStruct env TypeChecker.whnf TypeChecker.inferType ``L4LKC p
      pure (e.getAppFn.isConstOf ``L4LKC.mk))
  assertTrue "projection.eta.data-expansion" dataExpands
  let _ ← expectOk "universe.valid-imax-bound-datatype" (Lean4Lean.addDecl env levelFoo)
  let _ ← expectOk "universe.valid-two-fields" <|
    Lean4Lean.addDecl env (universeData `ValidUniverse 2)
  expectError "universe.undersized-rejected" "constructor_universe" <|
    Lean4Lean.addDecl env (universeData `InvalidUniverse 1)
  -- Build Eq through the candidate itself before quotient initialization.
  let some (.inductInfo eqInfo) := env.find? ``Eq | throw "missing Eq"
  let ctors ← eqInfo.ctors.mapM fun n => do
    let some ci := env.find? n | throw "missing Eq constructor"
    pure { name := ci.name, type := ci.type : Constructor }
  let eqEnv ← expectOk "quot.eq-bootstrap" <|
    Lean4Lean.addDecl (Lean.Kernel.Environment.empty `QuotControl) <|
      .inductDecl eqInfo.levelParams eqInfo.numParams
        [{name := eqInfo.name, type := eqInfo.type, ctors}] false
  for n in [``Quot, ``Quot.mk, ``Quot.lift, ``Quot.ind] do
    let occupied ← expectOk ("quot.collision-setup." ++ n.toString) <|
      Lean4Lean.addDecl eqEnv (.axiomDecl {
        name := n
        type := .sort 0
        levelParams := []
        isUnsafe := false})
    expectError ("quot.collision-rejected." ++ n.toString) "already_declared" <|
      Lean4Lean.addDecl occupied .quotDecl
  let qenv ← expectOk "quot.clean-initialization" (Lean4Lean.addDecl eqEnv .quotDecl)
  let qenv' ← expectOk "quot.idempotent-initialization" (Lean4Lean.addDecl qenv .quotDecl)
  assertTrue "quot.idempotent-state" (qenv'.quotInit &&
    [``Quot, ``Quot.mk, ``Quot.lift, ``Quot.ind].all fun n =>
      (qenv.find? n).map ConstantInfo.type == (qenv'.find? n).map ConstantInfo.type)
  for n in [``Nat.add, ``Nat.mod, ``Nat.div, ``Nat.gcd, ``Nat.bitwise] do
    let some (.defnInfo genuine) := env.find? n | throw "missing primitive {n}"
    let base := without env n
    let _ ← expectOk ("primitive.genuine." ++ n.toString) <|
      Lean4Lean.addDecl base (.defnDecl genuine)
    let changed := {genuine with value := if n == ``Nat.bitwise then threeBitwiseZero else twoNatZero}
    expectError ("primitive.changed-equation." ++ n.toString) "primitive_equation" <|
      Lean4Lean.addDecl base (.defnDecl changed)
  let some (.defnInfo add) := env.find? ``Nat.add | throw "missing Nat.add"
  let addBase := without env ``Nat.add
  let partialAdd := {add with safety := .partial}
  let recognized ← expectOk "primitive.partial.recognizer-call" <|
    (Lean4Lean.Environment.checkPrimitiveDef partialAdd).run env (lparams := add.levelParams)
  assertTrue "primitive.partial.not-recognized" (!recognized)
  expectError "primitive.partial.not-admitted" "reserved_primitive_name" <|
    Lean4Lean.addDecl addBase (.defnDecl partialAdd)
  expectError "primitive.malformed-type" "declaration_type_mismatch" <|
    Lean4Lean.addDecl addBase (.defnDecl {add with type := .const ``Bool []})
  expectError "primitive.malformed-body" "declaration_type_mismatch" <|
    Lean4Lean.addDecl addBase (.defnDecl {add with value := .const ``Bool.true []})
  let cond := Lean4Lean.Environment.Condition.natLE
  let .reflectNatNat asBool reflect _ := cond.impl | throw "missing reflection fixture"
  let badCond := {cond with impl := .reflectNatNat asBool reflect natNatTrue}
  expectError "primitive.malformed-reflection-evidence" "application_type_mismatch" <|
    (Lean4Lean.Environment.Condition.check badCond
      (throw (.other "control condition failed")) (ite := true) (dite := true)).run env

end Vesper.Lean4Lean42Controls

namespace Vesper.Lean4Lean42Controls
open Lean

open Lean4Lean TypeChecker

-- Independent bounded semantic interpretation; mvars are rejected, not assigned
-- invented values. These are tests, not a global proof of level completeness.
private def evalLevel (σ : Name → Nat) : Level → Option Nat
  | .zero => some 0
  | .succ l => (evalLevel σ l).map (· + 1)
  | .max a b => do pure (max (← evalLevel σ a) (← evalLevel σ b))
  | .imax a b => do
    let y ← evalLevel σ b
    if y == 0 then pure 0 else pure (max (← evalLevel σ a) y)
  | .param n => some (σ n)
  | .mvar _ => none
private def levelsUpTo (n : Nat) : Array (Array Level) := Id.run do
  let mut tbl : Array (Array Level) := #[#[]]
  for k in [1:n+1] do
    if k == 1 then tbl := tbl.push #[.zero, .param `u, .param `v, .param `w]
    else
      let mut out := tbl[k-1]!.map Level.succ
      for i in [1:k-1] do
        for a in tbl[i]! do
          for b in tbl[k-1-i]! do
            out := out.push (.max a b)
            out := out.push (.imax a b)
      tbl := tbl.push out
  return tbl
private def valuations (hi : Nat) : Array (Name → Nat) := Id.run do
  let mut out := #[]
  for i in [:hi+1] do
    for j in [:hi+1] do
      for k in [:hi+1] do
        out := out.push fun n => if n == `u then i else if n == `v then j else k
  return out
private def levelVector (vals : Array (Name → Nat)) (l : Level) : ControlM (Array Nat) :=
  match vals.mapM (evalLevel · l) with
  | some a => pure a
  | none => throw "unsupported metavariable in level control"

private def levels (_env : Kernel.Environment) : ControlM Unit := do
  let u := Level.param `u; let v := Level.param `v
  let w := Level.param `w; let x := Level.param `x
  assertTrue "level.constant-order.valid" ((Level.ofNat 2).geq' (.ofNat 1))
  assertTrue "level.constant-order.invalid" (!((Level.ofNat 1).geq' (.ofNat 2)))
  assertTrue "level.per-sublevel-order" ((Level.max 2 v).geq' (.imax 2 v))
  assertTrue "level.subsumption-drained-key" ((Level.imax u (.max u v)).isEquiv' (.max u v))
  let left := Level.max v (.max w (.imax (.imax (.imax u v) w) x))
  let right := Level.max v (.max w (.imax (.imax (.imax u w) v) x))
  assertTrue "level.canonical-scaffolding" (left.normalize' == right.normalize')
  assertTrue "level.canonical-scaffolding-equivalence" (left.isEquiv' right)
  let vals := valuations 7
  let mut buckets : Std.HashMap (Array Nat) (Level × Level) := {}
  let mut semanticsOk := true
  let mut canonicalOk := true
  let mut count := 0
  for group in levelsUpTo 5 do
    for l in group do
      count := count + 1
      let key ← levelVector vals l
      let normalized := l.normalize'
      let normalizedKey ← levelVector vals normalized
      if key != normalizedKey then semanticsOk := false
      match buckets[key]? with
      | none => buckets := buckets.insert key (l, normalized)
      | some (other, normal) =>
        if normal != normalized || !l.isEquiv' other then canonicalOk := false
  assertTrue "level.bounded-generation-count" (count == 852)
  assertTrue "level.bounded-interpretation" semanticsOk
  assertTrue "level.bounded-canonicity" canonicalOk
  let smallVals := valuations 4
  let mut small : Array (Level × Array Nat) := #[]
  for group in levelsUpTo 3 do
    for l in group do small := small.push (l, ← levelVector smallVals l)
  let mut comparisonOk := true
  for (a, av) in small do
    for (b, bv) in small do
      let expected := (av.zip bv).all fun (x, y) => decide (y ≤ x)
      if a.geq' b != expected then comparisonOk := false
  assertTrue "level.bounded-order-interpretation" comparisonOk

private def edges (env : Kernel.Environment) : ControlM Unit := do
  let erased ← expectOk "reduction.erased-local.check" <| TypeChecker.M.run env (x := do
    withLocalDecl `x (.const ``Nat []) .default fun x => do
      let three := .app (.lam `_ (.const ``Nat []) (mkRawNatLit 3) .default) x
      let sum := mkApp2 (.const ``Nat.add []) three (mkRawNatLit 4)
      TypeChecker.whnf sum)
  assertTrue "reduction.erased-local.value" (erased == mkRawNatLit 7)
  let shallow ← expectOk "fuel.shallow-positive" <|
    TypeChecker.M.run env (x := checkType (deepNat 16))
  assertTrue "fuel.shallow-type" (shallow.isConstOf ``Nat)
  expectError "fuel.deep-refusal" "deep_recursion" <|
    TypeChecker.M.run env (x := whnf (deepNat 1200))

/-- Execute the fixed raw controls against the caller's fixed umbrella baseline.
The baseline consists of the compiled control module and its genuine imports;
unverified candidates are supplied only to the actual candidate checker. -/
def runControls (baseline : Kernel.Environment) : IO (Array ControlResult) := do
  let (result, rows) ← ((do core baseline; extended baseline; levels baseline; edges baseline : ControlM Unit).run).run #[]
  let complete := result.isOk
  pure (rows.push ⟨"suite.complete", true, complete, "completed",
    if complete then "completed" else "fixture_failure", complete⟩)

end Vesper.Lean4Lean42Controls
