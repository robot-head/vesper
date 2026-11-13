import RewriteWitness
import Lean.Data.Json

/-!
Executable finite arithmetic reference. Requests contain data only. Numeric
answers and constant facts use RewriteWitness operations; this module does not
prove refinement of the Rust checker or model full language effects/resources.
-/
namespace Vesper.ArithmeticOracle
open Vesper.RewriteWitness
open Lean

inductive Scope where | input | local
inductive Callee where
  | intrinsic (op : IntOp)
  | variable (scope : Scope) (slot : Nat)
inductive Node where
  | intLiteral (n : Int) | boolLiteral (b : Bool)
  | variable (scope : Scope) (slot : Nat)
  | checked (op : IntOp) | call (callee : Callee) (arity : Nat)
instance : Inhabited Node := ⟨.intLiteral 0⟩
instance (k : Kind) : Inhabited (Value k) := ⟨⟨0⟩⟩
abbrev Code := Array Node
structure Input where
  kind : Kind
  value : Int
structure RawProgram where
  kind : Kind
  inputs : Array Input
  locals : Array Code
  result : Code
  allowFail : Bool
structure CheckInfo (k : Kind) where
  constant : Option (Value k)
  potentialOverflow : Bool
instance (k : Kind) : Inhabited (CheckInfo k) := ⟨⟨none, false⟩⟩
structure Blame where
  scope : Nat
  slot : Nat
  node : Nat
inductive CheckErrorKind where
  | unknownVariable | typeMismatch | literalOutOfRange | literalParseRange
  | missingFail | callArity | notCallable
structure CheckError where
  kind : CheckErrorKind
  blame : Blame
abbrev RequestError := String

structure ValidatedCode where
  private mk ::
  private code : Code
  private scope : Nat
  private slot : Nat
structure ValidatedProgram where
  private mk ::
  private raw : RawProgram
  private codes : Array ValidatedCode
  private resultCode : ValidatedCode
  private id : Nat
  private fuel : Nat

def ValidatedProgram.kind (p : ValidatedProgram) : Kind := p.raw.kind
structure CheckedProgram (k : Kind) where
  private mk ::
  private inputs : Array Input
  private locals : Array ValidatedCode
  private result : ValidatedCode

private def sameKind (a b : Kind) : Bool :=
  match a, b with
  | .u8, .u8 | .u32, .u32 | .u64, .u64 | .i64, .i64 => true
  | _, _ => false

private def failCheck (c : ValidatedCode) (i : Nat) (e : CheckErrorKind) :
    Except CheckError α := .error ⟨e, ⟨c.scope, c.slot, i⟩⟩

-- Shape validation precedes semantic checking. Depth uses the occurrence tree,
-- not local expansion: local reads copy an already evaluated primitive value.
private def validateCode (code : Code) (scope slot : Nat) :
    Except RequestError ValidatedCode := do
  if code.isEmpty || code.size > 63 then throw "profile_limit"
  let mut stack : Array Nat := #[]
  for node in code do
    let arity := match node with
      | .checked _ => 2 | .call _ n => n | _ => 0
    if stack.size < arity then throw "shape"
    let mut depth := 0
    for i in [stack.size - arity : stack.size] do
      depth := max depth stack[i]!
    stack := stack.extract 0 (stack.size - arity)
    depth := depth + 1
    if depth > 8 then throw "profile_limit"
    stack := stack.push depth
  if stack.size != 1 then throw "shape"
  pure ⟨code, scope, slot⟩

-- Input kinds are type information, never facts about their invocation values.
def checkExpr (k : Kind) (inputKinds : Array Kind)
    (priorLocalFacts : Array (Option (Value k))) (allowFail : Bool)
    (code : ValidatedCode) : Except CheckError (CheckInfo k) := do
  let mut stack : Array (CheckInfo k) := #[]
  for i in [:code.code.size] do
    let node := code.code[i]!
    let info ← match node with
      | .intLiteral n => do
        if n.natAbs > 18446744073709551615 then
          failCheck code i .literalParseRange
        else if h : Fits k n then pure ⟨some (literalValue k n h), false⟩
        else failCheck code i .literalOutOfRange
      | .boolLiteral _ => failCheck code i .typeMismatch
      | .variable .input slot => do
        match inputKinds[slot]? with
        | none => failCheck code i .unknownVariable
        | some kind =>
          if sameKind kind k then pure ⟨none, false⟩
          else failCheck code i .typeMismatch
      | .variable .local slot => do
        match priorLocalFacts[slot]? with
        | none => failCheck code i .unknownVariable
        | some fact => pure ⟨fact, false⟩
      | .call (.variable scope slot) _ => do
        let found : Bool := match scope with
          | .input => decide (slot < inputKinds.size)
          | .local => decide (slot < priorLocalFacts.size)
        if found then failCheck code i .notCallable
        else failCheck code i .unknownVariable
      | .call (.intrinsic _) arity => do
        if arity != 2 then failCheck code i .callArity
        else pure ⟨none, false⟩
      | .checked _ => pure ⟨none, false⟩
    match node with
    | .checked op | .call (.intrinsic op) 2 =>
      if stack.size < 2 then return ← failCheck code i .typeMismatch
      let a := stack[stack.size - 2]!
      let b := stack[stack.size - 1]!
      let wrapping := match node with | .call _ _ => true | _ => false
      let fact := match a.constant, b.constant with
        | some x, some y =>
          if wrapping then some (wrappingValue op x y) else checkedValue op x y
        | _, _ => none
      let ownOverflow := !wrapping && fact.isNone
      if ownOverflow && !allowFail then return ← failCheck code i .missingFail
      stack := (stack.extract 0 (stack.size - 2)).push
        ⟨fact, a.potentialOverflow || b.potentialOverflow || ownOverflow⟩
    | _ => stack := stack.push info
  match stack[0]? with
  | some info => pure info
  | none => failCheck code 0 .typeMismatch

def checkProgram (p : ValidatedProgram) : Except CheckError (CheckedProgram p.kind) := do
  let mut facts : Array (Option (Value p.kind)) := #[]
  let kinds := p.raw.inputs.map Input.kind
  for code in p.codes do
    let info ← checkExpr p.kind kinds facts p.raw.allowFail code
    facts := facts.push info.constant
  let _ ← checkExpr p.kind kinds facts p.raw.allowFail p.resultCode
  pure ⟨p.raw.inputs, p.codes, p.resultCode⟩

inductive EvalResult (k : Kind) where
  | value (value : Value k) (steps : Nat)
  | overflow (steps : Nat)
  | fuelExhausted (steps : Nat)
  | invariantFailure

private def evalCode {k : Kind} (inputs : Array Input) (locals : Array (Value k))
    (code : ValidatedCode) (fuel steps : Nat) : EvalResult k := Id.run do
  let mut stack : Array (Value k) := #[]
  let mut used := steps
  for node in code.code do
    if used >= fuel then return .fuelExhausted used
    used := used + 1
    match node with
    | .intLiteral n =>
      if h : Fits k n then stack := stack.push (literalValue k n h)
      else return .invariantFailure
    | .variable .input slot =>
      match inputs[slot]? with
      | none => return .invariantFailure
      | some input =>
        if !sameKind input.kind k then return .invariantFailure
        if h : Fits k input.value then
          stack := stack.push (literalValue k input.value h)
        else return .invariantFailure
    | .variable .local slot =>
      match locals[slot]? with
      | some value => stack := stack.push value
      | none => return .invariantFailure
    | .checked op | .call (.intrinsic op) 2 =>
      if stack.size < 2 then return .invariantFailure
      let a := stack[stack.size - 2]!
      let b := stack[stack.size - 1]!
      let value := match node with
        | .checked _ => checkedValue op a b
        | _ => some (wrappingValue op a b)
      match value with
      | none => return .overflow used
      | some v => stack := (stack.extract 0 (stack.size - 2)).push v
    | _ => return .invariantFailure
  if stack.size != 1 then return .invariantFailure
  return .value stack[0]! used

def evalSteps {k : Kind} (p : CheckedProgram k) (fuel : Nat) : EvalResult k := Id.run do
  let mut locals : Array (Value k) := #[]
  let mut steps := 0
  for code in p.locals do
    match evalCode p.inputs locals code fuel steps with
    | .value v n => locals := locals.push v; steps := n
    | other => return other
  return evalCode p.inputs locals p.result fuel steps

private def array (j : Json) : Except RequestError (Array Json) :=
  j.getArr?.mapError (fun _ => "schema")
private def nat (j : Json) (limit : Nat) (category := "schema") : Except RequestError Nat := do
  let n ← j.getNat?.mapError (fun _ => category)
  if n > limit then throw category
  pure n
private def decimal (j : Json) : Except RequestError Int := do
  let s ← j.getStr?.mapError (fun _ => "decimal")
  if s.isEmpty || s.utf8ByteSize > 40 then throw "decimal"
  let n ← match s.toInt? with
    | some n => pure n | none => throw "decimal"
  if toString n != s then throw "decimal"
  if n < -170141183460469231731687303715884105728 ||
      n > 170141183460469231731687303715884105727 then throw "decimal"
  pure n
private def kindOf (j : Json) : Except RequestError Kind := do
  match ← nat j 3 "kind_tag" with
  | 0 => pure .u8 | 1 => pure .u32 | 2 => pure .u64 | _ => pure .i64
private def opOf (j : Json) : Except RequestError IntOp := do
  match ← nat j 2 "op_tag" with
  | 0 => pure .add | 1 => pure .sub | _ => pure .mul
private def nodeOf (j : Json) : Except RequestError Node := do
  let a ← array j
  if a.isEmpty then throw "schema"
  let tag ← nat a[0]! 5
  match tag with
  | 0 =>
    if a.size != 2 then throw "schema"
    pure (.intLiteral (← decimal a[1]!))
  | 1 | 2 =>
    if a.size != 2 then throw "schema"
    pure (.variable (if tag == 1 then .input else .local) (← nat a[1]! 65535))
  | 3 =>
    if a.size != 2 then throw "schema"
    pure (.checked (← opOf a[1]!))
  | 4 =>
    if a.size != 4 then throw "schema"
    let target ← nat a[1]! 2
    let callee ← if target == 0 then pure (.intrinsic (← opOf a[2]!))
      else pure (.variable (if target == 1 then .input else .local) (← nat a[2]! 65535))
    pure (.call callee (← nat a[3]! 3 "profile_limit"))
  | _ =>
    if a.size != 2 then throw "schema"
    pure (.boolLiteral ((← nat a[1]! 1) == 1))
private def codeOf (j : Json) (scope slot : Nat) : Except RequestError ValidatedCode := do
  let a ← array j
  if a.size > 63 then throw "profile_limit"
  let code ← a.mapM nodeOf
  validateCode code scope slot

def decodeRequest (j : Json) : Except RequestError ValidatedProgram := do
  let a ← array j
  if a.size != 8 then throw "schema"
  if (← nat a[0]! 1) != 1 then throw "schema"
  let id ← nat a[1]! 9999999999
  let kind ← kindOf a[2]!
  let fuel ← nat a[3]! 4096 "profile_limit"
  let allowFail := (← nat a[4]! 1) == 1
  let rawInputs ← array a[5]!
  let rawLocals ← array a[6]!
  if rawInputs.size > 3 || rawLocals.size > 3 then throw "profile_limit"
  let mut inputs := #[]
  for item in rawInputs do
    let item ← array item
    if item.size != 2 then throw "schema"
    let ik ← kindOf item[0]!
    let value ← decimal item[1]!
    if !(decide (Fits ik value)) then throw "input_range"
    inputs := inputs.push ⟨ik, value⟩
  let mut codes := #[]
  let mut total := 0
  for i in [:rawLocals.size] do
    let code ← codeOf rawLocals[i]! 1 i
    total := total + code.code.size
    codes := codes.push code
  let result ← codeOf a[7]! 2 0
  if total + result.code.size > 63 then throw "profile_limit"
  let raw := RawProgram.mk kind inputs (codes.map ValidatedCode.code) result.code allowFail
  pure ⟨raw, codes, result, id, fuel⟩

private def jn (n : Nat) : Json := toJson n
private def response (tag : String) (id : Nat) (rest : Array Json := #[]) : Json :=
  Json.arr (#[jn 1, Json.str tag, jn id] ++ rest)
private def diagnostic (id : Nat) (e : CheckError) : Json :=
  let (code, name) := match e.kind with
    | .unknownVariable => ("E0201", "unknown_variable")
    | .typeMismatch => ("E0200", "type_mismatch")
    | .literalOutOfRange => ("E0209", "literal_range")
    | .literalParseRange => ("E0002", "literal_parse_range")
    | .missingFail => ("E0401", "missing_fail")
    | .callArity => ("E0207", "call_arity")
    | .notCallable => ("E0206", "not_callable")
  response "diagnostic" id #[Json.str code, Json.str name,
    Json.arr #[jn e.blame.scope, jn e.blame.slot, jn e.blame.node]]

def processRequest (expectedId : Nat) (j : Json) : Json :=
  match decodeRequest j with
  | .error e => response "invalid_request" expectedId #[Json.str e]
  | .ok p =>
    if p.id != expectedId then Json.arr #[jn 1, Json.str "fatal", Json.str "sequence"]
    else match checkProgram p with
      | .error e => diagnostic p.id e
      | .ok checked => match evalSteps checked p.fuel with
        | .value v steps => response "value" p.id #[Json.str (toString (toInt v)), jn steps]
        | .overflow steps => response "overflow" p.id #[jn steps]
        | .fuelExhausted steps => response "fuel_exhausted" p.id #[jn steps]
        | .invariantFailure => response "invariant_failure" p.id

-- This runs before the general JSON parser, bounding its recursion, scalar
-- lengths and allocation envelope. Syntax beyond this lexical subset never
-- reaches Json.parse (notably objects, escapes, exponents or huge numbers).
private def lexicalCheck (bytes : ByteArray) : Except RequestError Unit := do
  if bytes.size > 65536 then throw "profile_limit"
  let mut quoted := false
  let mut stringBytes := 0
  let mut depth := 0
  let mut digits := 0
  let mut tokens := 0
  for i in [:bytes.size] do
    let b := bytes[i]!.toNat
    if b > 127 then throw "lexical"
    if quoted then
      if b == 34 then quoted := false
      else
        if b < 32 || b == 92 then throw "lexical"
        stringBytes := stringBytes + 1
        if stringBytes > 40 then throw "lexical"
    else if b == 34 then
      quoted := true
      stringBytes := 0
      digits := 0
      tokens := tokens + 1
    else if b >= 48 && b <= 57 then
      if digits == 0 then tokens := tokens + 1
      digits := digits + 1
      if digits > 10 then throw "lexical"
    else
      digits := 0
      if b == 91 then
        depth := depth + 1
        if depth > 4 then throw "lexical"
      else if b == 93 then
        if depth == 0 then throw "json"
        depth := depth - 1
      else if b != 44 && b != 32 && b != 9 && b != 10 && b != 13 then
        throw "lexical"
    if tokens > 1024 then throw "lexical"
  if quoted || depth != 0 then throw "json"

private def processBytes (id : Nat) (bytes : ByteArray) : Json :=
  match lexicalCheck bytes with
  | .error e => response "invalid_request" id #[Json.str e]
  | .ok _ => match String.fromUTF8? bytes with
    | none => response "invalid_request" id #[Json.str "lexical"]
    | some s => match Json.parse s with
      | .error _ => response "invalid_request" id #[Json.str "json"]
      | .ok j => processRequest id j

private def ready : Json := Json.arr #[jn 1, Json.str "ready", jn 1,
  Json.str "4.24.0", Json.str "vesper.arith.checked/1", jn 63, jn 8, jn 3, jn 3, jn 4096]
private def fatalResponse (category : String) : Json :=
  Json.arr #[jn 1, Json.str "fatal", Json.str category]

-- All callers establish n <= 65536 before this allocation. Repeated short
-- reads are accumulated; EOF is distinct from an empty, complete payload.
private def readExact (input : IO.FS.Stream) (n : Nat) : IO ByteArray := do
  let mut out := ByteArray.emptyWithCapacity n
  while out.size < n do
    let chunk ← input.read (USize.ofNat (n - out.size))
    if chunk.isEmpty then break
    out := out ++ chunk
  pure out

private def readFrame (input : IO.FS.Stream) : IO (Except String (Option ByteArray)) := do
  let header ← readExact input 4
  if header.isEmpty then return .ok none
  if header.size != 4 then return .error "truncated_header"
  let n := header[0]!.toNat + header[1]!.toNat * 256 +
    header[2]!.toNat * 65536 + header[3]!.toNat * 16777216
  if n > 65536 then return .error "frame_limit"
  let payload ← readExact input n
  if payload.size != n then return .error "truncated_payload"
  return .ok (some payload)

private def send (output : IO.FS.Stream) (json : Json) : IO Unit := do
  let bytes := json.compress.toUTF8
  let n := bytes.size
  if n > 512 then throw (IO.userError "oracle42 response limit")
  let header : ByteArray := ⟨#[UInt8.ofNat n, UInt8.ofNat (n / 256),
    UInt8.ofNat (n / 65536), UInt8.ofNat (n / 16777216)]⟩
  output.write header
  output.write bytes
  output.flush

private def tagOf (j : Json) : String :=
  match j.getArr? with
  | .ok a => match a[1]? with
    | some (Json.str s) => s | _ => ""
  | .error _ => ""

private def serve (input output : IO.FS.Stream) : IO (Except String Unit) := do
  send output ready
  let mut seen := 0
  let mut checked := 0
  let mut diagnostics := 0
  let mut invalid := 0
  let mut values := 0
  let mut overflows := 0
  let mut exhausted := 0
  repeat
    match ← readFrame input with
    | .error category =>
      send output (fatalResponse category)
      return .error category
    | .ok none =>
      send output (Json.arr #[jn 1, Json.str "bye", jn seen, jn checked,
        jn diagnostics, jn invalid, jn values, jn overflows, jn exhausted])
      return .ok ()
    | .ok (some payload) =>
      -- Reserve the terminal count within the ten-digit control domain. EOF
      -- above remains legal at the limit; another frame cannot be dispatched.
      if seen >= 9999999999 then
        send output (fatalResponse "sequence_limit")
        return .error "sequence_limit"
      let answer := processBytes seen payload
      send output answer
      match tagOf answer with
      | "value" => values := values + 1; checked := checked + 1
      | "overflow" => overflows := overflows + 1; checked := checked + 1
      | "fuel_exhausted" => exhausted := exhausted + 1; checked := checked + 1
      | "diagnostic" => diagnostics := diagnostics + 1
      | "invalid_request" => invalid := invalid + 1
      | "fatal" => return .error "sequence"
      | _ => return .error "invariant_failure"
      seen := seen + 1
  return .ok ()

/-- Static server; stdout contains only framed JSON. Compile this module with
`lean -o`; execute its root main only when running an actual oracle session. -/
def run : IO Unit := do
  let input ← IO.getStdin
  let output ← IO.getStdout
  let result ← try serve input output catch _ => do
    send output (fatalResponse "io")
    pure (.error "io")
  match result with
  | .ok _ => pure ()
  | .error category => throw (IO.userError ("oracle42: " ++ category))

end Vesper.ArithmeticOracle

def main : IO Unit := Vesper.ArithmeticOracle.run
