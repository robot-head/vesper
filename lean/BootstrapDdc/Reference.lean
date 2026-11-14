/-!
# Path B reference evaluator for the bootstrap corpus (vesperc-lean-ddc stand-in)

An executable Lean 4 lexer, parser and fuel-bounded evaluator for the scalar
subset of Vesper used by `bootstrap/corpus/*.ves`: `i64` functions, `let`,
`if`/`else`, recursion, unary minus, checked `+ - *`, truncating `/ %`,
comparisons and `handle Fail[ArithOverflow | RuntimeError]` with a value
handler. It imports only `Init`: no Rust, no Cranelift and no code shared with
the Rust seed. `bootstrap/run.sh` compiles it with `lean -c` and links the C
with the distribution's gcc (CompCert is not packaged here).

For each file argument it prints `<stem> <status>`, where status is the
program's `main` value modulo 256, the exit status a Path A executable
returns. Unsupported syntax prints `<stem> unsupported` and exits 2.

This is an independent reimplementation of the subset's semantics, not a
proof about the Rust compiler and not a theorem about this file.
-/
namespace Vesper.BootstrapDdc.Reference

inductive Tok where
  | ident (s : String)
  | num (n : Nat)
  | sym (s : String)
  deriving BEq, Repr, Inhabited

private def isIdentStart (c : Char) : Bool := c.isAlpha || c == '_'
private def isIdentChar (c : Char) : Bool := c.isAlphanum || c == '_'

/-- Two-character symbols first, then one-character ones. -/
private def symbols2 : List String := ["->", "=>", "<=", ">=", "==", "!="]
private def symbols1 : List Char :=
  ['(', ')', '{', '}', '[', ']', ',', ';', ':', '+', '-', '*', '/', '%', '<', '>', '=']

partial def lex (cs : List Char) (acc : Array Tok) : Except String (Array Tok) :=
  match cs with
  | [] => .ok acc
  | '/' :: '/' :: rest => lex (rest.dropWhile (· != '\n')) acc
  | c :: rest =>
    if c.isWhitespace then lex rest acc
    else if c.isDigit then
      let digits := cs.takeWhile Char.isDigit
      let n := digits.foldl (fun n d => n * 10 + (d.toNat - '0'.toNat)) 0
      lex (cs.drop digits.length) (acc.push (.num n))
    else if isIdentStart c then
      let word := cs.takeWhile isIdentChar
      lex (cs.drop word.length) (acc.push (.ident (String.mk word)))
    else
      match rest with
      | d :: rest' =>
        let two := String.mk [c, d]
        if symbols2.contains two then lex rest' (acc.push (.sym two))
        else if symbols1.contains c then lex rest (acc.push (.sym (String.mk [c])))
        else .error s!"unexpected character {c}"
      | [] =>
        if symbols1.contains c then lex rest (acc.push (.sym (String.mk [c])))
        else .error s!"unexpected character {c}"

inductive BinOp where
  | add | sub | mul | div | rem | lt | le | gt | ge | eq | ne
  deriving Repr, Inhabited

inductive FailKind where
  | arith | runtime
  deriving BEq, Repr, Inhabited

inductive Expr where
  | lit (n : Int)
  | var (name : String)
  | neg (e : Expr)
  | bin (op : BinOp) (a b : Expr)
  | call (name : String) (args : List Expr)
  | ite (c t e : Expr)
  | letIn (name : String) (value body : Expr)
  | handle (kind : FailKind) (body : Expr) (retVar : String) (onReturn onRaise : Expr)
  deriving Repr, Inhabited

structure FnDef where
  name : String
  params : List String
  body : Expr
  deriving Repr, Inhabited

abbrev P := StateT (List Tok) (Except String)

private def peek : P (Option Tok) := do return (← get).head?
private def advance : P Unit := modify List.tail
private def expectSym (s : String) : P Unit := do
  match ← peek with
  | some (.sym t) => if t == s then advance else throw s!"expected {s}"
  | _ => throw s!"expected {s}"
private def expectKw (k : String) : P Unit := do
  match ← peek with
  | some (.ident t) => if t == k then advance else throw s!"expected {k}"
  | _ => throw s!"expected {k}"
private def ident : P String := do
  match ← peek with
  | some (.ident t) => advance; return t
  | _ => throw "expected identifier"
private def isSym (s : String) : P Bool := do
  match ← peek with
  | some (.sym t) => return t == s
  | _ => return false
private def isKw (k : String) : P Bool := do
  match ← peek with
  | some (.ident t) => return t == k
  | _ => return false

private def failKind : String → P FailKind
  | "ArithOverflow" => return .arith
  | "RuntimeError" => return .runtime
  | k => throw s!"unsupported failure {k}"

mutual
  partial def block : P Expr := do
    expectSym "{"
    let e ← blockBody
    expectSym "}"
    return e

  partial def blockBody : P Expr := do
    if ← isKw "let" then
      advance
      let name ← ident
      expectSym "="
      let value ← expr
      expectSym ";"
      return .letIn name value (← blockBody)
    else expr

  partial def expr : P Expr := do
    let a ← additive
    let op? : Option BinOp ← match ← peek with
      | some (.sym "<") => pure (some .lt)
      | some (.sym "<=") => pure (some .le)
      | some (.sym ">") => pure (some .gt)
      | some (.sym ">=") => pure (some .ge)
      | some (.sym "==") => pure (some .eq)
      | some (.sym "!=") => pure (some .ne)
      | _ => pure none
    match op? with
    | some op => advance; return .bin op a (← additive)
    | none => return a

  partial def additiveRest (a : Expr) : P Expr := do
    match ← peek with
    | some (.sym "+") => advance; additiveRest (.bin .add a (← multiplicative))
    | some (.sym "-") => advance; additiveRest (.bin .sub a (← multiplicative))
    | _ => return a

  partial def additive : P Expr := do additiveRest (← multiplicative)

  partial def multiplicativeRest (a : Expr) : P Expr := do
    match ← peek with
    | some (.sym "*") => advance; multiplicativeRest (.bin .mul a (← unary))
    | some (.sym "/") => advance; multiplicativeRest (.bin .div a (← unary))
    | some (.sym "%") => advance; multiplicativeRest (.bin .rem a (← unary))
    | _ => return a

  partial def multiplicative : P Expr := do multiplicativeRest (← unary)

  partial def unary : P Expr := do
    if ← isSym "-" then advance; return .neg (← unary) else primary

  partial def args : P (List Expr) := do
    if ← isSym ")" then advance; return []
    let first ← expr
    if ← isSym "," then advance; return first :: (← args)
    expectSym ")"
    return [first]

  partial def ifExpr : P Expr := do
    let c ← expr
    let t ← block
    expectKw "else"
    if ← isKw "if" then advance; return .ite c t (← ifExpr)
    return .ite c t (← block)

  partial def handleExpr : P Expr := do
    expectKw "Fail"
    expectSym "["
    let kind ← failKind (← ident)
    expectSym "]"
    expectKw "in"
    let body ← block
    expectKw "with"
    expectSym "{"
    expectKw "return"
    let v ← ident
    expectSym "=>"
    let onReturn ← expr
    expectSym ","
    expectKw "raise"
    expectSym "("
    let _ ← ident
    expectSym ","
    let _ ← ident
    expectSym ")"
    expectSym "=>"
    let onRaise ← expr
    if ← isSym "," then advance
    expectSym "}"
    return .handle kind body v onReturn onRaise

  partial def primary : P Expr := do
    match ← peek with
    | some (.num n) => advance; return .lit n
    | some (.sym "(") => advance; let e ← expr; expectSym ")"; return e
    | some (.sym "{") => block
    | some (.ident "if") => advance; ifExpr
    | some (.ident "handle") => advance; handleExpr
    | some (.ident name) =>
      advance
      if ← isSym "(" then advance; return .call name (← args)
      return .var name
    | _ => throw "expected expression"
end

/-- Skip a bracketed effect row; its contents do not affect evaluation. -/
private partial def skipRow (depth : Nat) : P Unit := do
  match ← peek with
  | some (.sym "[") => advance; skipRow (depth + 1)
  | some (.sym "]") => advance; if depth > 1 then skipRow (depth - 1)
  | some _ => advance; skipRow depth
  | none => throw "unterminated effect row"

private partial def params : P (List String) := do
  if ← isSym ")" then advance; return []
  let name ← ident
  expectSym ":"
  expectKw "i64"
  if ← isSym "," then advance; return name :: (← params)
  expectSym ")"
  return [name]

private def fnDef : P FnDef := do
  expectKw "fn"
  let name ← ident
  expectSym "("
  let ps ← params
  expectSym "->"
  expectKw "i64"
  if ← isKw "eff" then
    advance
    expectSym "["
    skipRow 1
  return { name, params := ps, body := ← block }

partial def program : P (List FnDef) := do
  match ← peek with
  | none => return []
  | some _ => return (← fnDef) :: (← program)

/-! ## Evaluation -/

inductive Val where
  | int (n : Int)
  | bool (b : Bool)
  deriving BEq, Repr, Inhabited

inductive Err where
  | fail (k : FailKind)
  | fuel
  | stuck (msg : String)
  deriving Repr, Inhabited

def minI64 : Int := -(2 ^ 63)
def maxI64 : Int := 2 ^ 63 - 1

def checked (v : Int) : Except Err Val :=
  if minI64 ≤ v ∧ v ≤ maxI64 then .ok (.int v) else .error (.fail .arith)

def applyBin (op : BinOp) (a b : Int) : Except Err Val :=
  match op with
  | .add => checked (a + b)
  | .sub => checked (a - b)
  | .mul => checked (a * b)
  | .div => if b == 0 then .error (.fail .runtime) else checked (a.tdiv b)
  -- `i64::MIN % -1` is 0 (crates/vesper-core numeric contract); only `/` overflows.
  | .rem => if b == 0 then .error (.fail .runtime) else checked (a.tmod b)
  | .lt => .ok (.bool (decide (a < b)))
  | .le => .ok (.bool (decide (a ≤ b)))
  | .gt => .ok (.bool (decide (a > b)))
  | .ge => .ok (.bool (decide (a ≥ b)))
  | .eq => .ok (.bool (a == b))
  | .ne => .ok (.bool (a != b))

def lookup (env : List (String × Val)) (x : String) : Except Err Val :=
  match env.lookup x with
  | some v => .ok v
  | none => .error (.stuck s!"unbound {x}")

def asInt : Val → Except Err Int
  | .int n => .ok n
  | .bool _ => .error (.stuck "expected an integer")

/-- Fuel-bounded big-step evaluation; each nested evaluation spends one unit. -/
def eval (fns : List FnDef) : Nat → List (String × Val) → Expr → Except Err Val
  | 0, _, _ => .error .fuel
  | _ + 1, _, .lit n => checked n
  | _ + 1, env, .var x => lookup env x
  | f + 1, env, .neg e => do checked (-(← asInt (← eval fns f env e)))
  | f + 1, env, .bin op a b => do
    let x ← asInt (← eval fns f env a)
    let y ← asInt (← eval fns f env b)
    applyBin op x y
  | f + 1, env, .ite c t e => do
    match ← eval fns f env c with
    | .bool true => eval fns f env t
    | .bool false => eval fns f env e
    | .int _ => .error (.stuck "expected a boolean")
  | f + 1, env, .letIn x v body => do
    let value ← eval fns f env v
    eval fns f ((x, value) :: env) body
  | f + 1, env, .call name actuals => do
    let some fn := fns.find? (·.name == name) | .error (.stuck s!"unknown function {name}")
    if fn.params.length != actuals.length then .error (.stuck "arity mismatch")
    let values ← actuals.attach.mapM (fun ⟨a, _⟩ => eval fns f env a)
    eval fns f (fn.params.zip values) fn.body
  | f + 1, env, .handle kind body v onReturn onRaise =>
    match eval fns f env body with
    | .ok value => eval fns f ((v, value) :: env) onReturn
    | .error (.fail k) => if k == kind then eval fns f env onRaise else .error (.fail k)
    | .error e => .error e

/-- Enough for the corpus (Ackermann(2, 3) is the deepest), small enough to stop runaways. -/
def defaultFuel : Nat := 1000000

def runSource (src : String) : Except String Int := do
  let toks ← lex src.toList #[]
  let (fns, rest) ← program.run toks.toList
  if !rest.isEmpty then throw "trailing tokens"
  let some main := fns.find? (·.name == "main") | throw "missing main"
  if !main.params.isEmpty then throw "main takes arguments"
  match eval fns defaultFuel [] main.body with
  | .ok (.int n) => return n
  | .ok (.bool _) => throw "main returned a boolean"
  | .error (.fail .arith) => throw "unhandled ArithOverflow"
  | .error (.fail .runtime) => throw "unhandled RuntimeError"
  | .error .fuel => throw "fuel exhausted"
  | .error (.stuck msg) => throw msg

/-- The exit status a Path A executable returns for `main`'s value. -/
def exitStatus (n : Int) : Nat := (n % 256).toNat

end Vesper.BootstrapDdc.Reference

open Vesper.BootstrapDdc.Reference in
def main (args : List String) : IO UInt32 := do
  let mut status : UInt32 := 0
  for path in args do
    let stem := (System.FilePath.mk path).fileStem.getD path
    let src ← IO.FS.readFile path
    match runSource src with
    | .ok n => IO.println s!"{stem} {exitStatus n}"
    | .error msg =>
      IO.eprintln s!"{stem}: {msg}"
      IO.println s!"{stem} unsupported"
      status := 2
  return status
