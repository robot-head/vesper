import ResourceModel42.Evaluator
import Lean.Data.Json

/-! Fixed data-only endpoint for the existing finite logical model. Transport
code is not a source adapter, native observer, or implementation proof. -/
namespace Vesper.ResourceOracle42
open Lean Vesper.ResourceModel42

private def requestCap : Nat := 4194304
private def responseCap : Nat := 262144
private def sessionCap : Nat := 67108864
private def controlCap : Nat := 9999999999
private def fail (s : String) : Except String α := .error s
private def arr (j : Json) : Except String (Array Json) :=
  j.getArr?.mapError (fun _ => "schema_array")
private def arity (a : Array Json) (n : Nat) : Except String Unit :=
  if a.size == n then .ok () else fail "schema_arity"
private def num (j : Json) (cap : Nat := controlCap) : Except String Nat := do
  let n ← j.getNat?.mapError (fun _ => "schema_number")
  if n > cap then fail "control_limit" else pure n
private def str (j : Json) : Except String String :=
  j.getStr?.mapError (fun _ => "schema_string")
private def natural (j : Json) : Except String Nat := do
  let s ← str j
  let bs := s.toUTF8
  if bs.isEmpty || bs.size > 20 then fail "natural_length"
  if bs.size > 1 && bs[0]! == 48 then fail "natural_canonical"
  let mut n := 0
  for b in bs do
    let c := b.toNat
    if c < 48 || c > 57 then fail "natural_digits"
    n := n * 10 + c - 48
  pure n

-- The lexical envelope is checked before the general JSON parser allocates a
-- tree. Strings have no escapes; numeric controls are canonical and bounded.
private def lexical (bytes : ByteArray) : Except String Unit := do
  if bytes.isEmpty || bytes.size > requestCap then fail "request_bytes"
  let mut quoted := false
  let mut stringBytes := 0
  let mut depth := 0
  let mut tokens := 0
  let mut digits := 0
  let mut leadingZero := false
  let mut value := 0
  for b in bytes do
    let c := b.toNat
    if quoted then
      if c == 34 then quoted := false
      else
        if c < 32 || c > 126 || c == 92 then fail "lexical_string"
        stringBytes := stringBytes + 1
        if stringBytes > 128 then fail "string_limit"
    else if c >= 48 && c <= 57 then
      if digits == 0 then
        tokens := tokens + 1
        leadingZero := c == 48
        value := 0
      else if leadingZero then fail "control_canonical"
      digits := digits + 1
      value := value * 10 + c - 48
      if digits > 10 || value > controlCap then fail "control_limit"
    else
      digits := 0
      if c == 34 then
        quoted := true
        stringBytes := 0
        tokens := tokens + 1
      else if c == 91 then
        depth := depth + 1
        tokens := tokens + 1
        if depth > 8 then fail "json_depth"
      else if c == 93 then
        if depth == 0 then fail "json_brackets"
        depth := depth - 1
      else if c != 44 && c != 32 && c != 9 && c != 10 && c != 13 then
        fail "lexical_token"
    if tokens > 1000000 then fail "token_limit"
  if quoted || depth != 0 then fail "json_unclosed"

private def mode (j : Json) : Except String Mode := do
  match ← num j 2 with
  | 0 => pure .copy | 1 => pure .affine | _ => pure .linear
private def floor (j : Json) : Except String Floor := do
  match ← num j 1 with
  | 0 => pure .copy | _ => pure .affine
private def pop (s : Array α) : Except String (α × Array α) :=
  match s.back? with
  | none => fail "postfix_underflow"
  | some a => pure (a, s.pop)
private def finish (s : Array α) : Except String α :=
  if s.size != 1 then fail "postfix_result" else
    match s.back? with | none => fail "postfix_result" | some a => pure a
private def checkedDepth (n : Nat) : Except String Nat :=
  if n > 256 then fail "tree_depth" else pure n

private def typeCode (rows : Array Json) : Except String Ty := do
  let mut s : Array (Ty × Nat) := #[]
  for row in rows do
    let a ← arr row
    if a.isEmpty then fail "schema_row"
    let tag ← num a[0]! 3
    if tag == 3 then
      arity a 4
      let f ← floor a[1]!
      let lm ← mode a[2]!
      let rm ← mode a[3]!
      let (r, rest) ← pop s
      let (l, rest) ← pop rest
      let d ← checkedDepth (1 + max l.2 r.2)
      s := rest.push (.pair f l.1 lm r.1 rm, d)
    else
      arity a 1
      s := s.push ((if tag == 0 then .unit else if tag == 1 then .nat else .ticket), 1)
  pure (← finish s).1
private def valueCode (rows : Array Json) : Except String RuntimeValue := do
  let mut s : Array (RuntimeValue × Nat) := #[]
  for row in rows do
    let a ← arr row
    if a.isEmpty then fail "schema_row"
    let tag ← num a[0]! 3
    match tag with
    | 0 => arity a 1; s := s.push (.unit, 1)
    | 1 => arity a 2; s := s.push (.nat (← natural a[1]!), 1)
    | 2 => arity a 2; s := s.push (.hdl (← num a[1]!), 1)
    | _ =>
      arity a 1
      let (r, rest) ← pop s
      let (l, rest) ← pop rest
      let d ← checkedDepth (1 + max l.2 r.2)
      s := rest.push (.pair l.1 r.1, d)
  pure (← finish s).1

private structure SourceCell where
  expr : SourceExpr
  atom : Option SourceAtom
  depth : Nat
private def atomCell (a : SourceAtom) : SourceCell := ⟨.atom a, some a, 1⟩
private def requireAtom (c : SourceCell) : Except String SourceAtom :=
  match c.atom with | some a => pure a | none => fail "atom_operand"
private def sourceCode (rows : Array Json) : Except String SourceExpr := do
  let mut s : Array SourceCell := #[]
  for row in rows do
    let a ← arr row
    if a.size < 2 then fail "schema_row"
    let tag ← num a[0]! 8
    let site ← num a[1]!
    match tag with
    | 0 => arity a 2; s := s.push (atomCell (.unit site))
    | 1 => arity a 3; s := s.push (atomCell (.nat site (← natural a[2]!)))
    | 2 => arity a 3; s := s.push (atomCell (.var site (← num a[2]!)))
    | 7 => arity a 2; s := s.push ⟨.mint site, none, 1⟩
    | 8 =>
      arity a 2
      let (x, rest) ← pop s
      let atom ← requireAtom x
      let d ← checkedDepth (1 + x.depth)
      s := rest.push ⟨.consume site atom, none, d⟩
    | 6 =>
      arity a 2
      let (n, rest) ← pop s
      let (z, rest) ← pop rest
      let (x, rest) ← pop rest
      let atom ← requireAtom x
      let d ← checkedDepth (1 + max x.depth (max z.depth n.depth))
      s := rest.push ⟨.ifz site atom z.expr n.expr, none, d⟩
    | _ =>
      arity a (if tag == 3 then 5 else 2)
      let (r, rest) ← pop s
      let (l, rest) ← pop rest
      let d ← checkedDepth (1 + max l.depth r.depth)
      let e ← if tag == 3 then do
          let f ← floor a[2]!
          let lm ← mode a[3]!
          let rm ← mode a[4]!
          pure (.pair site f (← requireAtom l) lm (← requireAtom r) rm)
        else if tag == 4 then pure (.letE site l.expr r.expr)
        else pure (.unpack site (← requireAtom l) r.expr)
      s := rest.push ⟨e, none, d⟩
  pure (← finish s).expr

private structure Request where
  sequence : Nat
  limits : Limits
  raw : RawProgram
  values : Array RuntimeValue
  fuel : Nat

private def codeRows (j : Json) : Except String (Array Json) := do
  let a ← arr j
  if a.isEmpty || a.size > 2048 then fail "code_rows"
  pure a
private def decode (j : Json) : Except String Request := do
  let a ← arr j
  arity a 8
  if (← num a[0]! 1) != 1 || (← str a[1]!) != "eval" then fail "schema_version"
  let seq ← num a[2]!
  let ls ← arr a[3]!
  arity ls 14
  let ls ← ls.mapM num
  let limits : Limits := ⟨ls[0]!,ls[1]!,ls[2]!,ls[3]!,ls[4]!,ls[5]!,ls[6]!,
    ls[7]!,ls[8]!,ls[9]!,ls[10]!,ls[11]!,ls[12]!,ls[13]!⟩
  let types ← arr a[4]!
  let values ← arr a[6]!
  if types.size > 65 || values.size > 65 then fail "input_arrays"
  -- Count ALL rows before constructing any model tree. Code arrays contain
  -- only immediate JSON rows; decoding cannot introduce sharing or expansion.
  let typeRows ← types.mapM codeRows
  let sourceRows ← codeRows a[5]!
  let valueRows ← values.mapM codeRows
  let total := typeRows.foldl (fun n rows => n + rows.size) sourceRows.size +
    valueRows.foldl (fun n rows => n + rows.size) 0
  if total > 131072 then fail "total_rows"
  let fuel ← num a[7]!
  let inputTypes ← typeRows.mapM typeCode
  let body ← sourceCode sourceRows
  let inputValues ← valueRows.mapM valueCode
  pure ⟨seq,limits,⟨inputTypes,body⟩,inputValues,fuel⟩

private def jn (n : Nat) : Json := toJson n
private def ja (a : Array Json) : Json := .arr a
private def je (name : String) (fields : Array Json := #[]) : Json := ja (#[.str name] ++ fields)
private def modeNum : Mode → Nat | .copy => 0 | .affine => 1 | .linear => 2
private def floorNum : Floor → Nat | .copy => 0 | .affine => 1
private def encodeType : Ty → Array Json
  | .unit => #[ja #[jn 0]] | .nat => #[ja #[jn 1]] | .ticket => #[ja #[jn 2]]
  | .pair f a ma b mb => encodeType a ++ encodeType b ++
      #[ja #[jn 3,jn (floorNum f),jn (modeNum ma),jn (modeNum mb)]]
private def encodeValue : RuntimeValue → Array Json
  | .unit => #[ja #[jn 0]]
  | .nat n => #[ja #[jn 1,.str (toString n)]]
  | .hdl id => #[ja #[jn 2,jn id]]
  | .pair a b => encodeValue a ++ encodeValue b ++ #[ja #[jn 3]]
private def typeError : TypeError → Json
  | .nodes => je "nodes" | .depth => je "depth" | .fieldMode => je "fieldMode"
private def validationError : ValidationError → Json
  | .invalidLimits => je "invalidLimits" | .inputCount => je "inputCount"
  | .noncopyInput i => je "noncopyInput" #[jn i]
  | .inputType i e => je "inputType" #[jn i,typeError e]
  | .rawNodes => je "rawNodes" | .rawDepth => je "rawDepth" | .context => je "context"
  | .site s => je "site" #[jn s] | .duplicateSite s => je "duplicateSite" #[jn s]
  | .variableBound s => je "variableBound" #[jn s]
private def capacityError : CapacityError → Json
  | .expressionNodes => je "expressionNodes" | .runtimeDepth => je "runtimeDepth"
  | .valueNodes => je "valueNodes" | .valueDepth => je "valueDepth"
  | .initialValues => je "initialValues" | .inputCount => je "inputCount"
private def checkKind : CheckErrorKind → String
  | .unknownVariable => "unknownVariable" | .typeMismatch => "typeMismatch"
  | .duplicateUse => "duplicateUse" | .unusedLinear => "unusedLinear"
  | .fieldModality => "fieldModality" | .literalOutOfRange => "literalOutOfRange"
  | .branchOwnership => "branchOwnership"
private def checkError (e : CheckError) : Json :=
  je (checkKind e.kind) #[jn e.site,ja (match e.slot with | none => #[] | some n => #[jn n])]
private def checkProfile : CheckProfile → Json
  | .typeBound s e => je "typeBound" #[jn s,typeError e]
  | .context s => je "context" #[jn s]
  | .affineBranchDemand s i => je "affineBranchDemand" #[jn s,jn i]
private def inputError : InputError → Json
  | .shape => je "shape" | .naturalRange => je "naturalRange" | .handle => je "handle"
  | .depth => je "depth" | .noncopy => je "noncopy"
private def invocationError : InvocationError → Json
  | .inputCount expected actual => je "inputCount" #[jn expected,jn actual]
  | .inputValue i e => je "inputValue" #[jn i,inputError e]
  | .capacity e => je "capacity" #[capacityError e]
  | .fuelBound requested cap => je "fuelBound" #[jn requested,jn cap]
private def refusal : ResourceRefusal → Json
  | .capacity e => je "capacity" #[capacityError e]
  | .resources requested cap => je "resources" #[jn requested,jn cap]
  | .events requested cap => je "events" #[jn requested,jn cap]
private def invariantError : InvariantError → Json
  | .openAtom => je "openAtom" | .expectedPair => je "expectedPair"
  | .expectedNat => je "expectedNat" | .expectedHandle => je "expectedHandle"
  | .unknownHandle id => je "unknownHandle" #[jn id]
  | .deadHandle id => je "deadHandle" #[jn id]
private def status : EvalStatus → Json
  | .completed v => je "completed" #[ja (encodeValue v)]
  | .fuelExhausted => je "fuelExhausted"
  | .resourceLimit e => je "resourceLimit" #[refusal e]
  | .invariantFailure e => je "invariantFailure" #[invariantError e]
private def sourceStats (s : SourceStats) : Json :=
  ja #[jn s.nodes,jn s.depth,jn s.maxContext,jn s.mints,jn s.consumes]
private def exprStats (s : ExprStats) : Json :=
  ja #[jn s.nodes,jn s.depth,jn s.maxValueNodes,jn s.maxValueDepth,
    ja (match s.value with | none => #[] | some v => #[jn v.nodes,jn v.depth])]
private def checkedInfo (p : CheckedProgram) : Json :=
  ja #[ja (encodeType p.resultType),ja (p.uses.toArray.map (fun b => jn (if b then 1 else 0))),
    sourceStats p.program.stats]

-- The only model execution path. Refusals return immediately without any
-- fabricated execution payload. The stage index is only a transport counter.
private def execute (r : Request) : String × Array Json × Nat :=
  match validate r.raw r.limits with
  | .error e => ("validation_error",#[validationError e],0)
  | .ok p => match checkProgram p with
    | .error (.semantic e) => ("check_semantic",#[checkError e,sourceStats p.stats],1)
    | .error (.profile e) => ("check_profile",#[checkProfile e,sourceStats p.stats],2)
    | .ok c => match validateInvocation c r.values with
      | .error e => ("invocation_error",#[checkedInfo c,invocationError e],3)
      | .ok invocation => match evalSteps invocation r.fuel with
        | .error e => ("eval_admission_error",#[checkedInfo c,exprStats invocation.stats,invocationError e],4)
        | .ok result =>
          let events := result.events.toArray.map fun e =>
            ja #[jn (match e.action with | .mint => 0 | .consume => 1),jn e.site,jn e.id]
          let residual := result.residual.toArray.map fun (id,e) => ja #[jn id,jn e.mintSite]
          let ledger := result.config.ledger.map fun e => ja #[jn e.mintSite,jn (if e.live then 1 else 0)]
          ("evaluation",#[checkedInfo c,exprStats invocation.stats,status result.status,
            ja events,ja residual,ja ledger,jn result.actions.length],5)

private def bounds : Array Json :=
  #[4194304,262144,8,1000000,131072,2048,65,256,9999999999,20,4096,67108864].map jn
private def ready : Json := ja #[jn 1,.str "ready",.str "vesper.resource-model42/1",
  .str "4.24.0",.str "797c613eb9b6d4ec95db23e3e00af9ac6657f24b",ja bounds]
private def decodeBytes (bytes : ByteArray) : Except String Request := do
  lexical bytes
  let text ← match String.fromUTF8? bytes with
    | none => fail "utf8" | some s => pure s
  let j ← (Json.parse text).mapError (fun _ => "json")
  decode j

private def readExact (input : IO.FS.Stream) (n : Nat) : IO ByteArray := do
  let mut out := ByteArray.emptyWithCapacity n
  while out.size < n do
    let chunk ← input.read (USize.ofNat (n - out.size))
    if chunk.isEmpty then break
    out := out ++ chunk
  pure out
private def die (reason : String) : IO α := throw (IO.userError ("resource_oracle42: " ++ reason))
private def send (output : IO.FS.Stream) (used : Nat) (j : Json) : IO Nat := do
  let bytes := j.compress.toUTF8
  let n := bytes.size
  if n == 0 || n > responseCap then die "response_bytes"
  if used + 4 + n > sessionCap then die "session_bytes"
  output.write ⟨#[UInt8.ofNat n,UInt8.ofNat (n / 256),
    UInt8.ofNat (n / 65536),UInt8.ofNat (n / 16777216)]⟩
  output.write bytes
  output.flush
  pure (used + 4 + n)
private def serve (input output : IO.FS.Stream) : IO Unit := do
  let mut used ← send output 0 ready
  let mut next := 0
  let mut counts : Array Nat := #[0,0,0,0,0,0,0]
  repeat
    let header ← readExact input 4
    if header.isEmpty then
      let _ ← send output used (ja #[jn 1,.str "bye",jn next,ja (counts.map jn)])
      return
    if header.size != 4 then die "truncated_header"
    let n := header[0]!.toNat + header[1]!.toNat * 256 +
      header[2]!.toNat * 65536 + header[3]!.toNat * 16777216
    if n == 0 || n > requestCap then die "request_bytes"
    if next >= 4096 then die "request_count"
    -- Prepay the complete request and a maximum response before allocating the
    -- payload or invoking the model, retaining a 512-byte complete BYE-frame
    -- reserve. Actual emitted bytes are accounted below.
    if used + 4 + n + 4 + responseCap + 512 > sessionCap then die "session_bytes"
    let payload ← readExact input n
    if payload.size != n then die "truncated_payload"
    let r ← match decodeBytes payload with
      | .error reason => die reason | .ok r => pure r
    if r.sequence != next then die "sequence"
    let (stage,body,index) := execute r
    used ← send output (used + 4 + n)
      (ja #[jn 1,.str "result",jn next,.str stage,ja body])
    counts := counts.modify 0 (· + 1)
    counts := counts.modify (index + 1) (· + 1)
    next := next + 1
  return

def run : IO Unit := do
  let input ← IO.getStdin
  let output ← IO.getStdout
  -- Do not echo untrusted exception text or partial model answers on failure.
  try serve input output catch _ => die "session_failed"

end Vesper.ResourceOracle42

def main : IO Unit := Vesper.ResourceOracle42.run
