/-
Vesper Stage 0b reference checker and evaluator (§42.1 pillar 1, wave eight
lane 4, issue #453).

An executable Lean 4 reference for the core calculus that `vesper check`
accepts today: i64/u64 scalars with `vesper.arith.checked/1` arithmetic and
the explicit wrapping intrinsics, bools, `let`, `if`, first-order calls,
copy structs and field projection, copy enums with `match`, `Fail[E]`
handlers (abortive, affine continuation) and one-shot handlers of linear
user effects with `resume`.

* `checkProgram` / `checkExpr` is a computable bidirectional checker. It
  mirrors the observable rules of the Rust checker for this fragment:
  contextual literal typing, the constant and non-zero facts that discharge
  `Fail[ArithOverflow]` and `Fail[RuntimeError]` obligations, the dead
  right operand of `false && _` / `true || _`, effect rows (E0401),
  one-shot continuation consumption (E0302, E0303, E0309) and the abort
  frame condition (E0402, E0410). It stops at the first diagnostic and
  returns its code; it also returns the program elaborated with the integer
  kind of every arithmetic operation.
* `evalSteps` is a fuel-stepped CEK machine with first-order frames and
  deep handlers. Captured continuations live in a store; resuming one
  removes it, so the store is the dynamic one-shot check, and every capture
  and resume is recorded in an event trace.
* `main` reads programs in the S-expression wire format written by
  `tests/lockstep.rs`, one per line, and prints one verdict per line.

The theorems at the end are about this Lean model. `ReferenceEval/Soundness.lean`
relates the checker to `lean/MetatheoryFull.lean` (Appendix A) on their
overlap. Nothing here is a theorem about the Rust compiler: agreement with
`vesperc-rs` is tested, program by program, by the lockstep harness.
-/
namespace Vesper.ReferenceEval

/-! ## Integer kinds and `vesper.arith.checked/1` -/

inductive IntK : Type where
  | i64 | u64
  deriving DecidableEq, Repr, BEq

namespace IntK
def lo : IntK → Int
  | i64 => -9223372036854775808 | u64 => 0
def hi : IntK → Int
  | i64 => 9223372036854775807 | u64 => 18446744073709551615
def fits (k : IntK) (n : Int) : Bool := decide (k.lo ≤ n) && decide (n ≤ k.hi)
def name : IntK → String
  | i64 => "i64" | u64 => "u64"
/-- `IntegerKind::default_for`: literals above `i64::MAX` default to u64. -/
def defaultFor (n : Int) : IntK := if n > i64.hi then u64 else i64
/-- Wrap an exact result into the kind's two's-complement range. -/
def wrap (k : IntK) (n : Int) : Int :=
  match k with
  | i64 => (n + 9223372036854775808) % 18446744073709551616 - 9223372036854775808
  | u64 => n % 18446744073709551616
end IntK

inductive BinOp : Type where
  | add | sub | mul | div | rem | eq | ne | lt | le | gt | ge | and | or
  deriving DecidableEq, Repr, BEq

namespace BinOp
def arith : BinOp → Bool
  | add | sub | mul | div | rem => true | _ => false
def order : BinOp → Bool
  | lt | le | gt | ge => true | _ => false
def logic : BinOp → Bool
  | and | or => true | _ => false
def symbol : BinOp → String
  | add => "+" | sub => "-" | mul => "*" | div => "/" | rem => "%"
  | eq => "==" | ne => "!=" | lt => "<" | le => "<=" | gt => ">" | ge => ">="
  | and => "&&" | or => "||"
/-- `numeric::may_overflow`. -/
def mayOverflow (op : BinOp) (k : IntK) : Bool :=
  match op with
  | add | sub | mul => true
  | div => k == .i64
  | _ => false
end BinOp

/-- A primitive arithmetic failure. -/
inductive Failure : Type where
  | overflow | divZero
  deriving DecidableEq, Repr

/-- `IntegerKind::checked` for `+ - * / %`: the exact result if it is in
range, otherwise the failure. Division truncates toward zero. -/
def checkedOp (k : IntK) (op : BinOp) (a b : Int) : Except Failure Int :=
  let exact : Except Failure Int :=
    match op with
    | .add => .ok (a + b)
    | .sub => .ok (a - b)
    | .mul => .ok (a * b)
    | .div => if b = 0 then .error .divZero else .ok (Int.tdiv a b)
    | .rem => if b = 0 then .error .divZero else .ok (Int.tmod a b)
    | _ => .error .overflow
  match exact with
  | .ok r => if k.fits r then .ok r else .error .overflow
  | .error f => .error f

def checkedNeg (k : IntK) (a : Int) : Option Int :=
  if k.fits (-a) then some (-a) else none

inductive WrapOp : Type where
  | add | sub | mul
  deriving DecidableEq, Repr, BEq

def WrapOp.toBin : WrapOp → BinOp
  | .add => .add | .sub => .sub | .mul => .mul

def WrapOp.exact (op : WrapOp) (a b : Int) : Int :=
  match op with
  | .add => a + b | .sub => a - b | .mul => a * b

/-! ## Types and syntax -/

/-- An effect instance: `Fail[T]` for a payload type name, or a user effect. -/
inductive Eff : Type where
  | fail (payload : String)
  | user (name : String)
  deriving DecidableEq, Repr, BEq

def Eff.render : Eff → String
  | .fail p => s!"Fail[{p}]"
  | .user n => n

/-- Source types. `lit n` is the checker's type of an unconstrained integer
literal (Rust `Type::IntLiteral`); `kont a r lin` is a resumption accepting
`a` and answering `r`, linear (one-shot, exactly once) or affine. -/
inductive Ty : Type where
  | int (k : IntK)
  | lit (n : Int)
  | bool
  | unit
  | adt (name : String)
  | never
  | kont (arg res : Ty) (linear : Bool) (effs : List Eff)
  deriving DecidableEq, Repr, BEq

def Ty.render : Ty → String
  | .int k => k.name
  | .lit n => s!"integer literal {n}"
  | .bool => "bool"
  | .unit => "()"
  | .adt n => n
  | .never => "Never"
  | .kont a r _ _ => s!"fn({a.render}) -> {r.render}"

def Ty.isLinear : Ty → Bool
  | .kont _ _ l _ => l
  | _ => false

inductive Pat : Type where
  | wild
  | bind (x : String)
  | ctor (name : String) (subs : List Pat)
  deriving Repr, BEq, Inhabited

/-- Expressions. Integer kinds on operations are filled in by the checker
(elaboration); the wire format carries `i64` placeholders. -/
inductive Expr : Type where
  | int (n : Int)
  | bool (b : Bool)
  | unit
  | var (x : String)
  | neg (k : IntK) (e : Expr)
  | not (e : Expr)
  | bin (op : BinOp) (k : IntK) (a b : Expr)
  | wrap (op : WrapOp) (k : IntK) (a b : Expr)
  | letE (x : String) (ann : Option Ty) (v body : Expr)
  | seq (a b : Expr)
  | ite (c t e : Expr)
  | call (f : String) (args : List Expr)
  | mkStruct (name : String) (fields : List (String × Expr))
  | field (e : Expr) (f : String)
  | ctor (name : String) (args : List Expr)
  | matchE (scrut : Expr) (arms : List (Pat × Expr))
  /-- `handle eff in { body } with { return retX => ret, op(params.., k) => opBody }` -/
  | handle (eff : Eff) (body : Expr) (retX : String) (ret : Expr) (op : String)
      (opParams : List String) (k : String) (opBody : Expr)
  | perform (eff op : String) (args : List Expr)
  | resume (k : String) (arg : Expr)
  deriving Repr, BEq, Inhabited

structure StructDecl where
  name : String
  fields : List (String × Ty)
  deriving Repr

structure VariantDecl where
  name : String
  payload : List Ty
  deriving Repr

structure EnumDecl where
  name : String
  variants : List VariantDecl
  deriving Repr

/-- A linear user effect with one operation. -/
structure EffDecl where
  name : String
  op : String
  params : List Ty
  res : Ty
  deriving Repr

structure FnDecl where
  name : String
  params : List (String × Ty)
  result : Ty
  effs : List Eff
  body : Expr
  deriving Repr

structure Program where
  structs : List StructDecl
  enums : List EnumDecl
  effects : List EffDecl
  fns : List FnDecl
  deriving Repr

/-- The prelude the Rust checker adds to every module. -/
def builtinStructs : List StructDecl := [⟨"ArithOverflow", []⟩]
def builtinEnums : List EnumDecl :=
  [⟨"RuntimeError", [⟨"DivisionByZero", []⟩, ⟨"IndexOutOfBounds", []⟩]⟩]

namespace Program
def structOf (P : Program) (n : String) : Option StructDecl :=
  (builtinStructs ++ P.structs).find? (·.name == n)
def ctorOf (P : Program) (n : String) : Option (EnumDecl × VariantDecl) :=
  (builtinEnums ++ P.enums).findSome? fun e =>
    (e.variants.find? (·.name == n)).map (e, ·)
def enumOf (P : Program) (n : String) : Option EnumDecl :=
  (builtinEnums ++ P.enums).find? (·.name == n)
def fnOf (P : Program) (n : String) : Option FnDecl := P.fns.find? (·.name == n)
def effOf (P : Program) (n : String) : Option EffDecl := P.effects.find? (·.name == n)
end Program

/-! ## The reference checker -/

structure Diag where
  code : String
  msg : String
  deriving Repr, BEq

/-- A binding of the checking environment, with the constant fact
(`integer`) and the non-zero fact the Rust checker tracks. -/
structure Binding where
  name : String
  ty : Ty
  fact : Option Int
  nonzero : Bool
  moved : Bool
  deriving Repr, BEq

abbrev Env := List Binding

/-- Checker state: the environment and the effects observed since the
innermost enclosing handler started (`start_observing`). -/
structure St where
  env : Env
  obs : List Eff

abbrev CM := StateT St (Except Diag)

def getEnv : CM Env := do return (← get).env
def setEnv (e : Env) : CM Unit := modify fun s => { s with env := e }
def getObs : CM (List Eff) := do return (← get).obs
def setObs (o : List Eff) : CM Unit := modify fun s => { s with obs := o }

def err {α : Type} (code msg : String) : CM α := throw ⟨code, msg⟩

/-- Read-only checking context: the enclosing function's effect row, the
lexical handlers in scope (innermost first), whether the code is statically
dead, and whether a resumption's argument is being evaluated (its
continuation is then a live linear temporary). -/
structure Cx where
  prog : Program
  allowed : List Eff
  caps : List Eff
  dead : Bool
  inResumeArg : Bool

/-- `check::compatible` on this fragment. -/
def compatible : Ty → Ty → Bool
  | _, .never => true
  | .lit _, .lit _ => true
  | .lit n, .int k => k.fits n
  | .int k, .lit n => k.fits n
  | a, b => a == b

def expectTy (exp act : Ty) : CM Unit :=
  if compatible exp act then pure ()
  else err "E0200" s!"expected {exp.render}, found {act.render}"

/-- `numeric::default_type`. -/
def defaultTy : Ty → Ty
  | .lit n => .int (IntK.defaultFor n)
  | t => t

def numeric : Ty → Bool
  | .int _ | .lit _ => true
  | _ => false

/-- `IntegerKind::from_type`. -/
def kindOf : Ty → Option IntK
  | .int k => some k
  | .lit n => some (IntK.defaultFor n)
  | _ => none

/-- `join_types`. -/
def joinTy : Ty → Ty → Ty
  | .never, b => b
  | a, .never => a
  | .lit x, .lit y =>
      .lit (if max x y > IntK.i64.hi then max x y else if min x y < 0 then min x y else max x y)
  | .lit _, b => b
  | a, _ => a

/-- `join_types`' representability check: two literals must both fit the
kind of their join. -/
def joinCheck : Ty → Ty → CM Unit
  | .lit x, .lit y =>
      let n := if max x y > IntK.i64.hi then max x y else if min x y < 0 then min x y else max x y
      let k := IntK.defaultFor n
      if k.fits x && k.fits y then pure ()
      else err "E0200" "branches have no common integer type"
  | _, _ => pure ()

def lookup (x : String) : CM (Option Binding) := do
  return (← getEnv).find? (·.name == x)

def markMoved (x : String) : Env → Env
  | [] => []
  | b :: bs => if b.name == x then { b with moved := true } :: bs else b :: markMoved x bs

def setMoved (x : String) : CM Unit :=
  modify fun st => { st with env := markMoved x st.env }

def push (b : Binding) : CM Unit := modify fun st => { st with env := b :: st.env }

/-- Leave a scope: drop the innermost binding, which must not be a live
linear value (`exhaust`, E0302). -/
def pop : CM Unit := do
  match ← getEnv with
  | b :: bs =>
      if b.ty.isLinear && !b.moved then
        err "E0302" s!"linear value '{b.name}' must be consumed exactly once"
      else setEnv bs
  | [] => pure ()

def effName : Eff → String
  | .fail _ => "Fail"
  | .user n => n

/-- `require_effect`: the instance must be allowed here; an abortable
(`Fail`) checkpoint must not abandon a live linear value. -/
def requireEff (cx : Cx) (e : Eff) : CM Unit := do
  if cx.dead then return
  let o ← getObs
  unless o.contains e do setObs (o ++ [e])
  -- `active_effect`: the lexical capabilities are the handlers in scope and
  -- the function's row. A request selects one of exactly this instance, else
  -- the innermost of the same effect name, whose arguments must then match
  -- (E0400); with neither it is unhandled (E0401).
  let caps := cx.caps ++ cx.allowed
  if !caps.contains e then
    if caps.any (fun c => effName c == effName e) then
      err "E0400" s!"effect '{effName e}' type arguments do not match its lexical capability"
    else
      err "E0401" s!"unhandled effect '{e.render}'"
  match e with
  | .fail _ =>
      if cx.inResumeArg then
        err "E0410" "abortable effect may abandon a linear temporary"
      for b in ← getEnv do
        if b.ty.isLinear && !b.moved then
          err "E0402" s!"potentially abortable effect may discard live linear value '{b.name}'"
  | .user _ => pure ()

/-- `refine_condition`: `x == 0` / `x != 0` facts on the two branches. -/
def refine (c : Expr) (truth : Bool) (env : Env) : Env :=
  let target : Option (BinOp × String) :=
    match c with
    | .bin op _ (.var x) (.int 0) => some (op, x)
    | .bin op _ (.int 0) (.var x) => some (op, x)
    | _ => none
  match target with
  | none => env
  | some (op, x) =>
      let upd (b : Binding) : Binding :=
        match op, truth with
        | .ne, true | .eq, false => { b with nonzero := true }
        | .eq, true | .ne, false => { b with fact := some 0, nonzero := false }
        | _, _ => b
      let rec go : Env → Env
        | [] => []
        | b :: bs => if b.name == x then upd b :: bs else b :: go bs
      go env

/-- `merge` of two branch environments over the same bindings: facts
survive only where both branches agree; a linear value must be consumed on
both paths or neither (E0309). -/
def mergeEnv : Env → Env → Except Diag Env
  | a :: as, b :: bs => do
      if a.ty.isLinear && a.moved != b.moved then
        throw ⟨"E0309", s!"linear value '{a.name}' is consumed on only some control-flow paths"⟩
      let rest ← mergeEnv as bs
      return { a with
        fact := if a.fact == b.fact then a.fact else none,
        nonzero := a.nonzero && b.nonzero,
        moved := a.moved || b.moved } :: rest
  | _, _ => pure []

def liftE {α : Type} (x : Except Diag α) : CM α := fun s => (fun a => (a, s)) <$> x

def factFits (t : Ty) (f : Option Int) : Option Int :=
  match f, kindOf t with
  | some n, some k => if k.fits n then some n else none
  | f, _ => f

/-- Does `x` occur free in `e`? Used for the handler-clause capture rule. -/
def mentions (x : String) : Expr → Bool
  | .var y => x == y
  | .resume k a => x == k || mentions x a
  | .neg _ e | .not e | .field e _ => mentions x e
  | .bin _ _ a b | .wrap _ _ a b | .seq a b => mentions x a || mentions x b
  | .letE y _ v b => mentions x v || (x != y && mentions x b)
  | .ite c t e => mentions x c || mentions x t || mentions x e
  | .call _ as | .ctor _ as | .perform _ _ as => mentionsList x as
  | .mkStruct _ fs => mentionsFields x fs
  | .matchE s arms => mentions x s || mentionsArms x arms
  | .handle _ b rx r _ ps k o =>
      mentions x b || (x != rx && mentions x r) || (!(ps.contains x) && x != k && mentions x o)
  | _ => false
where
  mentionsList (x : String) : List Expr → Bool
    | [] => false
    | e :: es => mentions x e || mentionsList x es
  mentionsFields (x : String) : List (String × Expr) → Bool
    | [] => false
    | (_, e) :: es => mentions x e || mentionsFields x es
  mentionsArms (x : String) : List (Pat × Expr) → Bool
    | [] => false
    | (_, e) :: es => mentions x e || mentionsArms x es

def patBinds : Pat → List String
  | .wild => []
  | .bind x => [x]
  | .ctor _ subs => patBindsList subs
where
  patBindsList : List Pat → List String
    | [] => []
    | p :: ps => patBinds p ++ patBindsList ps

/-- Coverage of a match on an enum: a top-level catch-all, or every variant
covered by an arm whose sub-patterns are all catch-alls. -/
def irrefutable : Pat → Bool
  | .wild | .bind _ => true
  | .ctor _ _ => false

def covers (arms : List (Pat × Expr)) (d : EnumDecl) : Bool :=
  arms.any (fun a => irrefutable a.1) ||
  d.variants.all fun v => arms.any fun a =>
    match a.1 with
    | .ctor n subs => n == v.name && subs.all irrefutable
    | _ => false

/-- Bind a pattern against a type (payload sub-patterns are bindings or
wildcards in this fragment). -/
def bindPat (P : Program) (p : Pat) (t : Ty) : CM Nat := do
  match p with
  | .wild => return 0
  | .bind x => push ⟨x, t, none, false, false⟩; return 1
  | .ctor n subs =>
      match P.ctorOf n with
      | none => err "E0201" s!"unknown constructor '{n}'"
      | some (d, v) =>
          if t != .adt d.name then err "E0200" s!"pattern '{n}' does not match {t.render}"
          else if subs.length != v.payload.length then
            err "E0204" s!"constructor pattern '{n}' requires {v.payload.length} fields"
          else
            let mut count := 0
            for (sp, st) in subs.zip v.payload do
              match sp with
              | .wild => pure ()
              | .bind x => push ⟨x, st, none, false, false⟩; count := count + 1
              | .ctor _ _ => err "E0213" "nested constructor patterns are outside the reference fragment"
            return count

def popN : Nat → CM Unit
  | 0 => pure ()
  | n + 1 => do pop; popN n

/-- A checked expression: its elaboration, type and constant fact. -/
abbrev Out := Expr × Ty × Option Int

/-- The expected type of a binary operator's left operand. -/
def binGoal (op : BinOp) (exp : Option Ty) : Option Ty :=
  if op.logic then some .bool
  else if op.arith then (match exp with | some t => if numeric t then some t else none | none => none)
  else none

/-- The expected type of the right operand, given the left operand's type. -/
def binRGoal (goal : Option Ty) (a : Ty) : Option Ty :=
  match goal with
  | some g => some g
  | none => match a with | .lit _ => none | t => some t

/-- The rest of a binary operator once both operands are checked: the common
operand type, and the `Fail[RuntimeError]` / `Fail[ArithOverflow]`
obligations that the facts do not discharge. -/
def binTail (cx : Cx) (op : BinOp) (r l' r' : Expr) (a b : Ty) (lf rf : Option Int) : CM Out := do
  let common : Ty :=
    match a, b with
    | .lit x, .lit y => .int (IntK.defaultFor (max x y))
    | .lit _, t => t
    | t, _ => t
  expectTy common a
  expectTy common b
  if (op.arith || op.order) && !numeric common then
    err "E0200" "this operator requires numeric operands"
  let k := (kindOf common).getD .i64
  let envNow ← getEnv
  let rnz := (rf.any (· != 0)) || (match r with
    | .var x => envNow.any (fun bd => bd.name == x && bd.nonzero)
    | _ => false)
  if (op == .div || op == .rem) && numeric common && !rnz then
    requireEff cx (.fail "RuntimeError")
  let inRange : Bool :=
    match lf, rf with
    | some x, some y => (match checkedOp k op x y with | .error .overflow => false | _ => true)
    | _, _ => op == .div && ((rf.any (· != -1)) || (lf.any (· != k.lo)))
  if (kindOf common).isSome && op.mayOverflow k && !inRange then
    requireEff cx (.fail "ArithOverflow")
  let fact : Option Int :=
    if op.arith then
      match lf, rf with
      | some x, some y => (match checkedOp k op x y with | .ok v => some v | .error _ => none)
      | _, _ => none
    else none
  let ty := if op.logic || op.order || op == .eq || op == .ne then .bool else common
  return (.bin op k l' r', ty, fact)


mutual
/-- `checkExpr fuel cx e exp`: bidirectional checking of `e` against the optional
expected type, threading the binding environment. -/
def checkExpr : Nat → Cx → Expr → Option Ty → CM Out
  | 0, _, _, _ => err "E0999" "expression nesting exceeds the checker's depth bound"
  | fuel + 1, cx, e, exp => do
  let (e', t, f) ← inferExpr fuel cx e exp
  if let some x := exp then expectTy x t
  return (e', t, factFits t f)

def inferExpr : Nat → Cx → Expr → Option Ty → CM Out
  | 0, _, _, _ => err "E0999" "expression nesting exceeds the checker's depth bound"
  | fuel + 1, cx, e, exp =>
  match e, exp with
  | .int n, exp => do
      match exp.bind kindOf with
      | some k =>
          if k.fits n then return (.int n, .int k, some n)
          else err "E0209" s!"integer literal {n} is outside the range of {k.name}"
      | none =>
          if (IntK.defaultFor n).fits n then return (.int n, .lit n, some n)
          else err "E0209" "integer literal is outside the supported ranges"
  | .bool b, _ => return (.bool b, .bool, none)
  | .unit, _ => return (.unit, .unit, none)
  | .var x, _ => do
      match ← lookup x with
      | some b =>
          if b.ty.isLinear then
            if b.moved then err "E0303" s!"use of moved place '{x}'"
            setMoved x
          return (.var x, b.ty, b.fact)
      | none =>
          match cx.prog.ctorOf x with
          | some (d, v) =>
              if v.payload.isEmpty then return (.ctor x [], .adt d.name, none)
              else err "E0203" "constructor requires arguments"
          | none => err "E0201" s!"unknown name '{x}'"
  | .neg _ v, exp => do
      let (v', t, f) ← checkExpr fuel cx v exp
      let t := defaultTy t
      unless numeric t do err "E0200" "negation requires a number"
      match kindOf t with
      | some k =>
          let r := f.bind (checkedNeg k)
          if r.isNone then requireEff cx (.fail "ArithOverflow")
          return (.neg k v', t, r)
      | none => return (.neg .i64 v', t, none)
  | .not v, exp => do
      let (v', t, _) ← checkExpr fuel cx v exp
      expectTy .bool (defaultTy t)
      return (.not v', .bool, none)
  | .bin op _ l r, exp => do
      let goal := binGoal op exp
      let (l', a, lf) ← checkExpr fuel cx l goal
      let rgoal := binRGoal goal a
      let (r', b, rf) ←
        if op.logic then do
          let dead := match l, op with
            | .bool false, .and | .bool true, .or => true
            | _, _ => false
          let before ← getEnv
          let res ← checkExpr fuel { cx with dead := cx.dead || dead } r rgoal
          if dead then setEnv before
          else do
            let after ← getEnv
            setEnv (← liftE (mergeEnv before after))
          pure res
        else checkExpr fuel cx r rgoal
      binTail cx op r l' r' a b lf rf
  | .wrap op _ x y, exp => do
      let goal : Option Ty := match exp with
        | some (.int k) => some (.int k)
        | _ => none
      let (x', tx, fx) ← checkExpr fuel cx x goal
      let common1 : Option Ty := match goal with
        | some g => some g
        | none => some tx
      let goal2 : Option Ty := match common1 with
        | some (.lit _) => none
        | c => c
      let (y', ty, fy) ← checkExpr fuel cx y goal2
      let common : Ty := match common1 with
        | some (.lit a) => (match ty with | .lit b => .lit (max a b) | t => t)
        | some c => c
        | none => ty
      let common := defaultTy common
      match kindOf common with
      | none => err "E0200" s!"'wrapping' requires integer operands of one kind, found {common.render}"
      | some k =>
          expectTy common tx
          expectTy common ty
          let fact := match fx, fy with
            | some a, some b => some (k.wrap (op.exact a b))
            | _, _ => none
          return (.wrap op k x' y', common, fact)
  | .letE x ann v body, exp => do
      let (v', actual, f) ← checkExpr fuel cx v ann
      let t := ann.getD (defaultTy actual)
      let envNow ← getEnv
      let nz := (f.any (· != 0)) || (match v with
        | .var y => envNow.any (fun bd => bd.name == y && bd.nonzero)
        | _ => false)
      push ⟨x, t, f, nz, false⟩
      let (b', bt, _) ← checkExpr fuel cx body exp
      pop
      -- a block has no constant fact (`integer_fact` sees only literals,
      -- names, negation, binary operators and wrapping calls)
      return (.letE x ann v' b', bt, none)
  | .seq a b, exp => do
      let (a', aTy, _) ← checkExpr fuel cx a none
      if aTy.isLinear then err "E0302" "unused linear result must be consumed"
      let (b', bt, _) ← checkExpr fuel cx b exp
      return (.seq a' b', bt, none)
  | .ite c th el, exp => do
      let (c', ct, _) ← checkExpr fuel cx c none
      expectTy .bool ct
      let env ← getEnv
      setEnv (refine c true env)
      let (t', a, _) ← checkExpr fuel cx th exp
      let left ← getEnv
      setEnv (refine c false env)
      let (e', b, _) ← checkExpr fuel cx el exp
      let right ← getEnv
      joinCheck a b
      let result := joinTy a b
      expectTy result a
      expectTy result b
      if a == .never then setEnv right
      else if b == .never then setEnv left
      else setEnv (← liftE (mergeEnv left right))
      return (.ite c' t' e', result, none)
  | .call f args, exp => do
      match ← lookup f with
      | some ⟨_, .kont _ _ _ _, _, _, _⟩ =>
          -- a local holding a continuation (`let j = k; j(v)`) resumes it
          match args with
          | [a] => inferExpr fuel cx (.resume f a) exp
          | _ => err "E0207" s!"'{f}' expects 1 argument, found {args.length}"
      | some _ => err "E0206" s!"'{f}' is not a function"
      | none =>
      match cx.prog.ctorOf f with
      | some (d, v) =>
          let args' ← checkFields fuel cx (args.zip v.payload) 0
          if args.length > v.payload.length then err "E0202" "unknown field"
          if args.length < v.payload.length then
            err "E0204" s!"constructor '{f}' requires {v.payload.length} fields"
          return (.ctor f args', .adt d.name, none)
      | none =>
      match cx.prog.fnOf f with
      | none => err "E0201" s!"unknown name '{f}'"
      | some fd =>
          if args.length != fd.params.length then
            err "E0207" s!"'{f}' expects {fd.params.length} arguments, found {args.length}"
          let args' ← checkFields fuel cx (args.zip (fd.params.map (·.2))) 0
          for eff in fd.effs do requireEff cx eff
          return (.call f args', fd.result, none)
  | .ctor n args, exp => inferExpr fuel cx (if args.isEmpty then .var n else .call n args) exp
  | .mkStruct n fields, _ => do
      match cx.prog.structOf n with
      | none => err "E0201" s!"unknown constructor '{n}'"
      | some d =>
          let fields' ← checkStructFields fuel cx d fields []
          if fields.length != d.fields.length then
            err "E0204" s!"constructor '{n}' requires {d.fields.length} fields"
          return (.mkStruct n fields', .adt n, none)
  | .field b f, _ => do
      let (b', t, _) ← checkExpr fuel cx b none
      match t with
      | .adt n =>
          match (cx.prog.structOf n).bind (fun d => d.fields.lookup f) with
          | some ft => return (.field b' f, ft, none)
          | none => err "E0202" s!"unknown field '{f}'"
      | _ => err "E0202" s!"unknown field '{f}'"
  | .matchE s arms, exp => do
      let (s', t, _) ← checkExpr fuel cx s none
      match t with
      | .adt n =>
          match cx.prog.enumOf n with
          | none => err "E0212" "match requires an enum"
          | some d =>
              unless covers arms d do
                err "E0212" "non-exhaustive match; cover every variant and payload or add a wildcard"
              let env ← getEnv
              let (arms', ty, envs) ← checkArms fuel cx arms t exp env .never
              match envs with
              | [] => setEnv env
              | first :: rest =>
                  let mut acc := first
                  for e in rest do acc ← liftE (mergeEnv acc e)
                  setEnv acc
              return (.matchE s' arms', ty, none)
      | _ => err "E0212" "match requires an enum scrutinee in the reference fragment"
  | .handle eff body rx ret opName ps k ob, exp => do
      -- the handled effect must exist
      let (opTys, opRes, linear) ← (match eff with
        | .fail p =>
            if (cx.prog.structOf p).isSome || (cx.prog.enumOf p).isSome then
              pure ([Ty.adt p], Ty.never, false)
            else err "E0400" "unknown handled effect payload"
        | .user n =>
            match cx.prog.effOf n with
            | some d => pure (d.params, d.res, true)
            | none => err "E0400" "unknown handled effect" : CM (List Ty × Ty × Bool))
      let saved ← getObs
      setObs []
      let (b', bt, _) ← checkExpr fuel { cx with caps := eff :: cx.caps, inResumeArg := false } body none
      -- effects of the body that this handler does not discharge
      let bodyObs := (← getObs).filter (· != eff)
      let outer ← getEnv
      -- clauses see only the captured outer bindings, which must be owned copy
      -- (`clause_free_names`: each clause's own binders are not captures)
      let captured := outer.filter fun bd =>
        (bd.name != rx && mentions bd.name ret) ||
          (!(ps.contains bd.name) && bd.name != k && mentions bd.name ob)
      for bd in captured do
        if bd.ty.isLinear then
          err "E0404" s!"handler clause capture '{bd.name}' must be owned copy"
      let ccx := { cx with inResumeArg := false }
      -- return clause
      setEnv captured
      setObs []
      push ⟨rx, bt, none, false, false⟩
      let (r', rt, _) ← checkExpr fuel ccx ret exp
      pop
      let retObs ← getObs
      let out : Ty := if rt == .never then exp.getD rt else rt
      -- the continuation performs what the body and return clause perform
      let resumeEffs := bodyObs ++ retObs.filter (fun e => !bodyObs.contains e)
      -- operation clause
      let declared := match eff with
        | .fail _ => "raise"
        | .user n => (cx.prog.effOf n).map (·.op) |>.getD ""
      if opName != declared then err "E0405" s!"unknown operation '{opName}'"
      if ps.length != opTys.length then
        err "E0405" s!"operation clause expects {opTys.length + 1} patterns"
      setEnv captured
      setObs []
      for (p, t) in ps.zip opTys do push ⟨p, t, none, false, false⟩
      push ⟨k, .kont opRes out linear resumeEffs, none, false, false⟩
      let (o', ot, _) ← checkExpr fuel ccx ob (some out)
      expectTy out ot
      popN (ps.length + 1)
      let opObs ← getObs
      setEnv outer
      let outgoing := resumeEffs ++ opObs.filter (fun e => !resumeEffs.contains e)
      setObs (saved ++ outgoing.filter (fun e => !saved.contains e))
      return (.handle eff b' rx r' opName ps k o', out, none)
  | .perform en op args, _ => do
      match cx.prog.effOf en with
      | none => err "E0201" s!"unknown name '{en}::{op}'"
      | some d =>
          if d.op != op then err "E0201" s!"unknown name '{en}::{op}'"
          if args.length != d.params.length then
            err "E0207" s!"'{op}' expects {d.params.length} arguments, found {args.length}"
          let args' ← checkFields fuel cx (args.zip d.params) 0
          requireEff cx (.user en)
          return (.perform en op args', d.res, none)
  | .resume k a, _ => do
      match ← lookup k with
      | some ⟨_, .kont argT rt lin effs, _, _, moved⟩ =>
          if lin && moved then err "E0303" s!"use of moved place '{k}'"
          if lin then setMoved k
          let (a', _, _) ← checkExpr fuel { cx with inResumeArg := cx.inResumeArg || lin } a (some argT)
          for e in effs do requireEff cx e
          return (.resume k a', rt, none)
      | some b => err "E0206" s!"{b.ty.render} is not callable"
      | none => err "E0201" s!"unknown name '{k}'"

/-- Check positional arguments against their parameter types. -/
def checkFields : Nat → Cx → List (Expr × Ty) → Nat → CM (List Expr)
  | 0, _, _, _ => err "E0999" "expression nesting exceeds the checker's depth bound"
  | _, _, [], _ => return []
  | fuel + 1, cx, (e, t) :: rest, i => do
      let (e', _, _) ← checkExpr fuel cx e (some t)
      let rest' ← checkFields fuel cx rest (i + 1)
      return e' :: rest'

def checkStructFields : Nat → Cx → StructDecl →
    List (String × Expr) → List String → CM (List (String × Expr))
  | 0, _, _, _, _ => err "E0999" "expression nesting exceeds the checker's depth bound"
  | _, _, _, [], _ => return []
  | fuel + 1, cx, d, (n, e) :: rest, seen => do
      let target := d.fields.lookup n
      let (e', _, _) ← checkExpr fuel cx e target
      if seen.contains n then err "E0204" "duplicate constructor field"
      if target.isNone then err "E0202" s!"unknown field '{n}'"
      let rest' ← checkStructFields fuel cx d rest (n :: seen)
      return (n, e') :: rest'

/-- Check match arms from the scrutinee environment; returns the elaborated
arms, the joined type and the exit environment of every non-diverging arm. -/
def checkArms : Nat → Cx → List (Pat × Expr) → Ty → Option Ty → Env → Ty →
    CM (List (Pat × Expr) × Ty × List Env)
  | 0, _, _, _, _, _, _ => err "E0999" "expression nesting exceeds the checker's depth bound"
  | _, _, [], _, _, _, acc => return ([], acc, [])
  | fuel + 1, cx, (p, body) :: rest, t, exp, env, acc => do
      setEnv env
      let n ← bindPat cx.prog p t
      let (b', bt, _) ← checkExpr fuel cx body exp
      popN n
      let armEnv ← getEnv
      let acc' ← if acc == .never then pure bt else do
        joinCheck acc bt
        let common := joinTy acc bt
        expectTy common acc
        expectTy common bt
        pure common
      let (rest', ty, envs) ← checkArms fuel cx rest t exp env acc'
      return ((p, b') :: rest', ty, if bt == .never then envs else armEnv :: envs)
end

def tyWellFormed (P : Program) : Ty → Bool
  | .adt n => (P.structOf n).isSome || (P.enumOf n).isSome
  | .kont _ _ _ _ | .lit _ | .never => false
  | _ => true

/-- The checker's nesting bound (the Rust parser's default depth is lower). -/
def checkDepth : Nat := 100000

/-- Check one function; returns its elaborated body. -/
def checkFn (P : Program) (f : FnDecl) : Except Diag FnDecl := do
  for (_, t) in f.params do
    unless tyWellFormed P t do throw ⟨"E0101", s!"unknown type '{t.render}'"⟩
  unless tyWellFormed P f.result do throw ⟨"E0101", s!"unknown type '{f.result.render}'"⟩
  let env : Env := f.params.reverse.map fun (x, t) => ⟨x, t, none, false, false⟩
  let cx : Cx := ⟨P, f.effs, [], false, false⟩
  let ((body, _, _), _) ← (checkExpr checkDepth cx f.body (some f.result)).run (St.mk env [])
  return { f with body }

/-- `checkProgram`: the first diagnostic, or the elaborated program. -/
def checkProgram (P : Program) : Except Diag Program := do
  let mut names : List String := []
  for f in P.fns do
    if names.contains f.name then throw ⟨"E0100", s!"duplicate function '{f.name}'"⟩
    names := f.name :: names
  let check (t : Ty) : Except Diag Unit :=
    unless tyWellFormed P t do throw ⟨"E0101", s!"unknown type '{t.render}'"⟩
  for d in P.structs do
    for (_, t) in d.fields do check t
  for d in P.enums do
    for v in d.variants do
      for t in v.payload do check t
  for d in P.effects do
    for t in d.params do check t
    check d.res
  let fns ← P.fns.mapM (checkFn P)
  return { P with fns }

/-! ## The reference evaluator: a fuel-stepped CEK machine -/

inductive Val : Type where
  | int (n : Int)
  | bool (b : Bool)
  | unit
  | struct (name : String) (fields : List (String × Val))
  | variant (name : String) (args : List Val)
  /-- a one-shot continuation, by store id -/
  | kont (id : Nat)
  deriving Repr, BEq, Inhabited

mutual
/-- `runtime::equal`: struct fields compare by name, not by position. -/
def Val.eqv : Val → Val → Bool
  | .struct a af, .struct b bf => a == b && af.length == bf.length && Val.eqvFields af bf
  | .variant a xs, .variant b ys => a == b && Val.eqvList xs ys
  | x, y => x == y
def Val.eqvFields : List (String × Val) → List (String × Val) → Bool
  | [], _ => true
  | (n, a) :: rest, bf =>
      (match bf.find? (·.1 == n) with
        | some (_, b) => Val.eqv a b
        | none => false) && Val.eqvFields rest bf
def Val.eqvList : List Val → List Val → Bool
  | [], [] => true
  | a :: as, b :: bs => Val.eqv a b && Val.eqvList as bs
  | _, _ => false
end

abbrev VEnv := List (String × Val)

structure Handler where
  eff : Eff
  retX : String
  ret : Expr
  opParams : List String
  k : String
  opBody : Expr
  env : VEnv
  deriving Repr, BEq

inductive Target : Type where
  | fn (name : String)
  | ctor (name : String)
  | perform (eff op : String)
  deriving Repr, BEq

inductive Frame : Type where
  | letK (x : String) (body : Expr) (env : VEnv)
  | seqK (b : Expr) (env : VEnv)
  | negK (k : IntK)
  | notK
  | binL (op : BinOp) (k : IntK) (b : Expr) (env : VEnv)
  | binR (op : BinOp) (k : IntK) (a : Val)
  | logicK (op : BinOp) (b : Expr) (env : VEnv)
  | wrapL (op : WrapOp) (k : IntK) (b : Expr) (env : VEnv)
  | wrapR (op : WrapOp) (k : IntK) (a : Val)
  | ifK (t e : Expr) (env : VEnv)
  | argsK (target : Target) (done : List Val) (rest : List Expr) (env : VEnv)
  | structK (name : String) (done : List (String × Val)) (field : String)
      (rest : List (String × Expr)) (env : VEnv)
  | fieldK (f : String)
  | matchK (arms : List (Pat × Expr)) (env : VEnv)
  | handleK (h : Handler)
  | resumeK (id : Nat)
  deriving Repr, BEq

inductive Ctrl : Type where
  | eval (e : Expr) (env : VEnv)
  | ret (v : Val)
  | raise (eff : Eff) (args : List Val)
  deriving Repr, BEq

/-- Resource events: a continuation captured (linear or affine) or resumed. -/
inductive Event : Type where
  | capture (id : Nat) (linear : Bool)
  | resume (id : Nat)
  deriving Repr, BEq

structure Store where
  /-- id ↦ (linear?, frames while live; `none` once resumed) -/
  conts : List (Nat × Bool × Option (List Frame))
  next : Nat
  trace : List Event
  deriving Repr

structure Machine where
  prog : Program
  ctrl : Ctrl
  stack : List Frame
  store : Store

inductive Outcome : Type where
  | value (v : Val)
  | uncaught (eff : Eff)
  | stuck (msg : String)
  deriving Repr, BEq

def failVal : Failure → Eff × Val
  | .overflow => (.fail "ArithOverflow", .struct "ArithOverflow" [])
  | .divZero => (.fail "RuntimeError", .variant "DivisionByZero" [])

def matchPat : Pat → Val → Option VEnv
  | .wild, _ => some []
  | .bind x, v => some [(x, v)]
  | .ctor n subs, .variant m args =>
      if n == m && subs.length == args.length then matchList subs args else none
  | .ctor _ _, _ => none
where
  matchList : List Pat → List Val → Option VEnv
    | [], [] => some []
    | p :: ps, v :: vs => do
        let a ← matchPat p v
        let b ← matchList ps vs
        return b ++ a
    | _, _ => none

def selectArm : List (Pat × Expr) → Val → Option (Expr × VEnv)
  | [], _ => none
  | (p, e) :: rest, v =>
      match matchPat p v with
      | some binds => some (e, binds)
      | none => selectArm rest v

/-- Split the stack at the innermost handler of `eff`: the captured frames
(up to and including the handler, which a deep handler reinstalls) and the
frames below it. -/
def splitAtHandler (eff : Eff) : List Frame → Option (List Frame × Handler × List Frame)
  | [] => none
  | f :: fs =>
      match f with
      | .handleK h =>
          if h.eff == eff then some ([f], h, fs)
          else (splitAtHandler eff fs).map fun (c, h', r) => (f :: c, h', r)
      | _ => (splitAtHandler eff fs).map fun (c, h', r) => (f :: c, h', r)

/-- One machine step: `.inl` a final outcome, `.inr` the next machine. -/
def step (m : Machine) : Outcome ⊕ Machine :=
  let goto (c : Ctrl) (s : List Frame) : Outcome ⊕ Machine := .inr { m with ctrl := c, stack := s }
  let stuck (msg : String) : Outcome ⊕ Machine := .inl (.stuck msg)
  match m.ctrl with
  | .eval e env =>
      match e with
      | .int n => goto (.ret (.int n)) m.stack
      | .bool b => goto (.ret (.bool b)) m.stack
      | .unit => goto (.ret .unit) m.stack
      | .var x =>
          match env.lookup x with
          | some v => goto (.ret v) m.stack
          | none => stuck s!"unbound {x}"
      | .neg k a => goto (.eval a env) (.negK k :: m.stack)
      | .not a => goto (.eval a env) (.notK :: m.stack)
      | .bin op k a b =>
          if op.logic then goto (.eval a env) (.logicK op b env :: m.stack)
          else goto (.eval a env) (.binL op k b env :: m.stack)
      | .wrap op k a b => goto (.eval a env) (.wrapL op k b env :: m.stack)
      | .letE x _ v b => goto (.eval v env) (.letK x b env :: m.stack)
      | .seq a b => goto (.eval a env) (.seqK b env :: m.stack)
      | .ite c t f => goto (.eval c env) (.ifK t f env :: m.stack)
      | .call f args => evalArgs (.fn f) args env
      | .ctor n args => evalArgs (.ctor n) args env
      | .perform en op args => evalArgs (.perform en op) args env
      | .mkStruct n fs =>
          match fs with
          | [] => goto (.ret (.struct n [])) m.stack
          | (f, e) :: rest => goto (.eval e env) (.structK n [] f rest env :: m.stack)
      | .field b f => goto (.eval b env) (.fieldK f :: m.stack)
      | .matchE s arms => goto (.eval s env) (.matchK arms env :: m.stack)
      | .handle eff body rx ret _ ps k ob =>
          goto (.eval body env) (.handleK ⟨eff, rx, ret, ps, k, ob, env⟩ :: m.stack)
      | .resume k a =>
          match env.lookup k with
          | some (.kont id) => goto (.eval a env) (.resumeK id :: m.stack)
          | _ => stuck s!"{k} is not a continuation"
  | .ret v =>
      match m.stack with
      | [] => .inl (.value v)
      | fr :: s =>
          match fr with
          | .letK x b env => goto (.eval b ((x, v) :: env)) s
          | .seqK b env => goto (.eval b env) s
          | .negK k =>
              match v with
              | .int n =>
                  match checkedNeg k n with
                  | some r => goto (.ret (.int r)) s
                  | none => let (e, p) := failVal .overflow; goto (.raise e [p]) s
              | _ => stuck "negation of a non-integer"
          | .notK =>
              match v with
              | .bool b => goto (.ret (.bool !b)) s
              | _ => stuck "not of a non-bool"
          | .binL op k b env => goto (.eval b env) (.binR op k v :: s)
          | .binR op k a =>
              match op, a, v with
              | .eq, x, y => goto (.ret (.bool (x.eqv y))) s
              | .ne, x, y => goto (.ret (.bool (!x.eqv y))) s
              | .lt, .int x, .int y => goto (.ret (.bool (decide (x < y)))) s
              | .le, .int x, .int y => goto (.ret (.bool (decide (x ≤ y)))) s
              | .gt, .int x, .int y => goto (.ret (.bool (decide (x > y)))) s
              | .ge, .int x, .int y => goto (.ret (.bool (decide (x ≥ y)))) s
              | op, .int x, .int y =>
                  match checkedOp k op x y with
                  | .ok r => goto (.ret (.int r)) s
                  | .error f => let (e, p) := failVal f; goto (.raise e [p]) s
              | _, _, _ => stuck "ill-typed operands"
          | .logicK op b env =>
              match op, v with
              | .and, .bool false => goto (.ret (.bool false)) s
              | .or, .bool true => goto (.ret (.bool true)) s
              | _, .bool _ => goto (.eval b env) s
              | _, _ => stuck "logic on a non-bool"
          | .wrapL op k b env => goto (.eval b env) (.wrapR op k v :: s)
          | .wrapR op k a =>
              match a, v with
              | .int x, .int y => goto (.ret (.int (k.wrap (op.exact x y)))) s
              | _, _ => stuck "wrapping of non-integers"
          | .ifK t f env =>
              match v with
              | .bool true => goto (.eval t env) s
              | .bool false => goto (.eval f env) s
              | _ => stuck "if on a non-bool"
          | .argsK tgt done rest env =>
              let done := done ++ [v]
              match rest with
              | e :: rest => goto (.eval e env) (.argsK tgt done rest env :: s)
              | [] => apply tgt done s
          | .structK n done f rest env =>
              let done := done ++ [(f, v)]
              match rest with
              | (f', e) :: rest => goto (.eval e env) (.structK n done f' rest env :: s)
              | [] => goto (.ret (.struct n done)) s
          | .fieldK f =>
              match v with
              | .struct _ fs =>
                  match fs.lookup f with
                  | some w => goto (.ret w) s
                  | none => stuck s!"no field {f}"
              | _ => stuck "field of a non-struct"
          | .matchK arms env =>
              match selectArm arms v with
              | some (e, binds) => goto (.eval e (binds ++ env)) s
              | none => stuck "no matching arm"
          | .handleK h => goto (.eval h.ret ((h.retX, v) :: h.env)) s
          | .resumeK id =>
              match m.store.conts.lookup id with
              | some (lin, some frames) =>
                  let conts := m.store.conts.map (fun c => if c.1 == id then (c.1, lin, none) else c)
                  let store : Store := { m.store with conts := conts, trace := m.store.trace ++ [.resume id] }
                  .inr { m with ctrl := .ret v, stack := frames ++ s, store := store }
              | some (_, none) => stuck "continuation resumed twice"
              | none => stuck "unknown continuation"
  | .raise eff args =>
      match splitAtHandler eff m.stack with
      | none => .inl (.uncaught eff)
      | some (captured, h, below) =>
          let id := m.store.next
          let lin : Bool := (match eff with | .user _ => true | .fail _ => false)
          let env : VEnv := (h.k, .kont id) :: (h.opParams.zip args).reverse ++ h.env
          let store : Store :=
            { conts := (id, lin, some captured) :: m.store.conts, next := id + 1,
              trace := m.store.trace ++ [.capture id lin] }
          .inr { m with ctrl := .eval h.opBody env, stack := below, store := store }
where
  evalArgs (tgt : Target) (args : List Expr) (env : VEnv) : Outcome ⊕ Machine :=
    match args with
    | [] => apply tgt [] m.stack
    | e :: rest => .inr { m with ctrl := .eval e env, stack := .argsK tgt [] rest env :: m.stack }
  apply (tgt : Target) (vals : List Val) (s : List Frame) : Outcome ⊕ Machine :=
    match tgt with
    | .ctor n => .inr { m with ctrl := .ret (.variant n vals), stack := s }
    | .perform en _ => .inr { m with ctrl := .raise (.user en) vals, stack := s }
    | .fn f =>
        match m.prog.fnOf f with
        | some fd =>
            if fd.params.length == vals.length then
              .inr { m with ctrl := .eval fd.body ((fd.params.map (·.1)).zip vals).reverse, stack := s }
            else .inl (.stuck s!"arity of {f}")
        | none => .inl (.stuck s!"unknown function {f}")

/-- `evalSteps fuel m`: run at most `fuel` steps. `none` means out of fuel;
otherwise the outcome, the final store and the number of steps taken. -/
def evalSteps : Nat → Machine → Option (Outcome × Store × Nat)
  | 0, _ => none
  | n + 1, m =>
      match step m with
      | .inl o => some (o, m.store, 1)
      | .inr m' => (evalSteps n m').map fun (o, s, k) => (o, s, k + 1)

def initial (P : Program) (entry : String) : Option Machine :=
  (P.fnOf entry).map fun f =>
    ⟨P, .eval f.body [], [], ⟨[], 0, []⟩⟩

/-- Live linear continuations left in a final store (Theorem 2 analogue). -/
def Store.liveLinear (s : Store) : Nat :=
  (s.conts.filter fun c => c.2.1 && c.2.2.isSome).length

/-! ## Rendering, as `runtime::Value`'s `Display` -/

def insertSorted (p : String × String) : List (String × String) → List (String × String)
  | [] => [p]
  | q :: qs => if decide (p.1 < q.1) then p :: q :: qs else q :: insertSorted p qs

mutual
def Val.render : Val → String
  | .int n => toString n
  | .bool b => toString b
  | .unit => "()"
  | .kont _ => "<one-shot continuation>"
  | .variant n [] => n
  | .variant n args => n ++ "(" ++ ", ".intercalate (Val.renderList args) ++ ")"
  | .struct n [] => n
  | .struct n fs =>
      let sorted := (Val.renderFields fs).foldl (fun acc p => insertSorted p acc) []
      n ++ " { " ++ ", ".intercalate (sorted.map fun (k, v) => k ++ ": " ++ v) ++ " }"
def Val.renderList : List Val → List String
  | [] => []
  | v :: vs => v.render :: Val.renderList vs
def Val.renderFields : List (String × Val) → List (String × String)
  | [] => []
  | (k, v) :: fs => (k, v.render) :: Val.renderFields fs
end

/-! ## Wire format: S-expressions -/

inductive SExp : Type where
  | atom (s : String)
  | list (xs : List SExp)
  deriving Repr, Inhabited

/-- Tokenize: parentheses and whitespace-separated atoms. -/
def tokenize (s : String) : List String :=
  let push (acc : List String) (cur : String) := if cur.isEmpty then acc else cur :: acc
  let (acc, cur) := s.foldl (fun (acc, cur) c =>
    if c == '(' || c == ')' then ((c.toString :: push acc cur), "")
    else if c.isWhitespace then (push acc cur, "")
    else (acc, cur.push c)) ([], "")
  (push acc cur).reverse

/-- Parse with an explicit stack (no recursion). -/
def parseSExp (toks : List String) : Except String SExp := do
  let mut stack : List (List SExp) := [[]]
  for t in toks do
    if t == "(" then stack := [] :: stack
    else if t == ")" then
      match stack with
      | top :: next :: rest => stack := (SExp.list top.reverse :: next) :: rest
      | _ => throw "unbalanced ')'"
    else
      match stack with
      | top :: rest => stack := (SExp.atom t :: top) :: rest
      | [] => throw "empty stack"
  match stack with
  | [[x]] => return x
  | _ => throw "expected exactly one expression"

def parseInt (s : String) : Except String Int :=
  match s.toInt? with
  | some n => .ok n
  | none => .error s!"bad integer {s}"

def parseTy : SExp → Except String Ty
  | .atom "bool" => .ok .bool
  | .atom "unit" => .ok .unit
  | .list [.atom "int", .atom "i64"] => .ok (.int .i64)
  | .list [.atom "int", .atom "u64"] => .ok (.int .u64)
  | .list [.atom "adt", .atom n] => .ok (.adt n)
  | s => .error s!"bad type {repr s}"

def parseEff : SExp → Except String Eff
  | .list [.atom "fail", .atom p] => .ok (.fail p)
  | .list [.atom "user", .atom n] => .ok (.user n)
  | s => .error s!"bad effect {repr s}"

def parseBinOp : String → Except String BinOp
  | "add" => .ok .add | "sub" => .ok .sub | "mul" => .ok .mul | "div" => .ok .div
  | "rem" => .ok .rem | "eq" => .ok .eq | "ne" => .ok .ne | "lt" => .ok .lt
  | "le" => .ok .le | "gt" => .ok .gt | "ge" => .ok .ge | "and" => .ok .and
  | "or" => .ok .or | s => .error s!"bad operator {s}"

def parseWrapOp : String → Except String WrapOp
  | "add" => .ok .add | "sub" => .ok .sub | "mul" => .ok .mul
  | s => .error s!"bad wrapping operator {s}"

def atom : SExp → Except String String
  | .atom s => .ok s
  | s => .error s!"expected an atom, found {repr s}"

/-- Parse a pattern. Patterns nest at most `fuel` deep. -/
def parsePat : Nat → SExp → Except String Pat
  | _, .atom "_" => .ok .wild
  | _, .list [.atom "bind", .atom x] => .ok (.bind x)
  | n + 1, .list (.atom "ctor" :: .atom c :: subs) => do
      return .ctor c (← subs.mapM (parsePat n))
  | _, s => .error s!"bad pattern {repr s}"

/-- Parse an expression; `fuel` bounds the nesting depth. -/
def parseExpr : Nat → SExp → Except String Expr
  | 0, _ => .error "expression nesting too deep"
  | n + 1, s =>
    let p := parseExpr n
    match s with
    | .atom "true" => .ok (.bool true)
    | .atom "false" => .ok (.bool false)
    | .atom "unit" => .ok .unit
    | .list [.atom "lit", .atom v] => do return .int (← parseInt v)
    | .list [.atom "var", .atom x] => .ok (.var x)
    | .list [.atom "neg", e] => do return .neg .i64 (← p e)
    | .list [.atom "not", e] => do return .not (← p e)
    | .list [.atom "bin", .atom op, a, b] => do return .bin (← parseBinOp op) .i64 (← p a) (← p b)
    | .list [.atom "wrap", .atom op, a, b] => do return .wrap (← parseWrapOp op) .i64 (← p a) (← p b)
    | .list [.atom "let", .atom x, .atom "_", v, b] => do return .letE x none (← p v) (← p b)
    | .list [.atom "let", .atom x, t, v, b] => do return .letE x (some (← parseTy t)) (← p v) (← p b)
    | .list [.atom "seq", a, b] => do return .seq (← p a) (← p b)
    | .list [.atom "if", c, t, e] => do return .ite (← p c) (← p t) (← p e)
    | .list (.atom "call" :: .atom f :: args) => do return .call f (← args.mapM p)
    | .list (.atom "ctor" :: .atom c :: args) => do return .ctor c (← args.mapM p)
    | .list (.atom "struct" :: .atom name :: fields) => do
        let fs ← fields.mapM fun
          | .list [.atom f, e] => do return (f, ← p e)
          | x => .error s!"bad field {repr x}"
        return .mkStruct name fs
    | .list [.atom "field", e, .atom f] => do return .field (← p e) f
    | .list (.atom "match" :: e :: arms) => do
        let as ← arms.mapM fun
          | .list [.atom "arm", pat, body] => do return (← parsePat 8 pat, ← p body)
          | x => .error s!"bad arm {repr x}"
        return .matchE (← p e) as
    | .list [.atom "handle", eff, body, .atom rx, ret, .atom op, .list ps, .atom k, ob] => do
        let ps ← ps.mapM atom
        return .handle (← parseEff eff) (← p body) rx (← p ret) op ps k (← p ob)
    | .list (.atom "perform" :: .atom en :: .atom op :: args) => do
        return .perform en op (← args.mapM p)
    | .list [.atom "resume", .atom k, a] => do return .resume k (← p a)
    | s => .error s!"bad expression {repr s}"

def parseDecl (P : Program) : SExp → Except String Program
  | .list (.atom "struct" :: .atom n :: fields) => do
      let fs ← fields.mapM fun
        | .list [.atom f, t] => do return (f, ← parseTy t)
        | x => .error s!"bad struct field {repr x}"
      return { P with structs := P.structs ++ [⟨n, fs⟩] }
  | .list (.atom "enum" :: .atom n :: vs) => do
      let vs ← vs.mapM fun
        | .list (.atom v :: ts) => do return ⟨v, ← ts.mapM parseTy⟩
        | x => .error s!"bad variant {repr x}"
      return { P with enums := P.enums ++ [⟨n, vs⟩] }
  | .list [.atom "effect", .atom n, .atom op, .list ps, r] => do
      return { P with effects := P.effects ++ [⟨n, op, ← ps.mapM parseTy, ← parseTy r⟩] }
  | .list [.atom "fn", .atom f, .list ps, r, .list effs, body] => do
      let ps ← ps.mapM fun
        | .list [.atom x, t] => do return (x, ← parseTy t)
        | x => .error s!"bad parameter {repr x}"
      return { P with fns := P.fns ++ [⟨f, ps, ← parseTy r, ← effs.mapM parseEff, ← parseExpr 4096 body⟩] }
  | x => .error s!"bad declaration {repr x}"

def parseProgram (line : String) : Except String Program := do
  match ← parseSExp (tokenize line) with
  | .list (.atom "program" :: decls) => decls.foldlM parseDecl ⟨[], [], [], []⟩
  | _ => throw "expected (program ...)"

/-! ## Verdicts -/

def defaultFuel : Nat := 200000

def renderOutcome : Option (Outcome × Store × Nat) → String
  | none => "fuel"
  | some (o, s, n) =>
      let res := match o with
        | .value v => "value " ++ v.render
        | .uncaught e => "uncaught " ++ e.render
        | .stuck msg => "stuck " ++ msg.replace " " "_"
      let captures := (s.trace.filter fun | .capture _ _ => true | _ => false).length
      let resumes := (s.trace.filter fun | .resume _ => true | _ => false).length
      s!"{res} ; captures={captures} resumes={resumes} live_linear={s.liveLinear} steps={n}"

/-- The verdict line for one wire-format program. -/
def verdict (line : String) : String :=
  match parseProgram line with
  | .error e => "malformed " ++ e
  | .ok P =>
      match checkProgram P with
      | .error d => "reject " ++ d.code
      | .ok P' =>
          match P'.fnOf "main" with
          | none => "accept-no-main"
          | some f =>
              let run := (initial P' "main").bind (evalSteps defaultFuel)
              s!"accept {f.result.render} {renderOutcome run}"

end Vesper.ReferenceEval

open Vesper.ReferenceEval in
/-- `lean --run lean/ReferenceEval.lean INPUT`: one verdict per input line. -/
def main (args : List String) : IO UInt32 := do
  match args with
  | [path] =>
      let input ← IO.FS.readFile path
      let out ← IO.getStdout
      for line in input.splitOn "\n" do
        if !line.trim.isEmpty then out.putStrLn (verdict line.trim)
      return 0
  | _ =>
      IO.eprintln "usage: lean --run ReferenceEval.lean PROGRAMS"
      return 2
