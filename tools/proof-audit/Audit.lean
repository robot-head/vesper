import Lean
import Std
import Lean.Replay

/-!
Trusted helper for the local artifact auditor. The Rust driver elaborates this
file with only the pinned toolchain on LEAN_PATH. Package modules are imported
at runtime without loading environment extensions or running initializers.

This is not an OS sandbox. The caller must pin the executable/toolchain and use
a bounded private artifact snapshot. The driver also rejects any Lean PANIC
output, including panics followed by an apparent successful exit.
-/

open Lean

namespace VesperProofAudit

structure Request where
  root : System.FilePath
  toolchainPrefix : System.FilePath
  moduleName : Name
  theorems : Array Name

def fail {α : Type} (message : String) : IO α :=
  throw <| IO.userError message

def validName (source : String) : Bool :=
  source.length ≤ 1024 && !(source.isEmpty) &&
    (source.splitOn ".").all fun part =>
      match part.toList with
      | [] => false
      | first :: rest =>
        (first.isAlpha && first.toNat < 128 || first == '_') &&
          rest.all (fun c => c.toNat < 128 && (c.isAlphanum || c == '_' || c == '\''))

def parseName (source : String) : IO Name := do
  unless validName source do
    fail s!"unsupported name: {source}"
  return (source.splitOn ".").foldl Name.str .anonymous

def reservedRoot (name : Name) : Bool :=
  ["Lean", "Std", "Init", "Lake"].contains name.getRoot.toString

def parseRequest (json : Json) : IO Request := do
  let schema ← IO.ofExcept <| json.getObjValAs? Nat "schema_version"
  unless schema == 1 do fail "unsupported request schema"
  let root ← IO.ofExcept <| json.getObjValAs? String "root"
  let prefixSource ← IO.ofExcept <| json.getObjValAs? String "toolchain_prefix"
  let moduleSource ← IO.ofExcept <| json.getObjValAs? String "module"
  let moduleName ← parseName moduleSource
  if reservedRoot moduleName then fail "target module uses a reserved toolchain root"
  let theoremSources ← IO.ofExcept <| json.getObjValAs? (Array String) "theorems"
  unless !theoremSources.isEmpty && theoremSources.size ≤ 128 do
    fail "request must contain between one and 128 theorem names"
  let theorems ← theoremSources.mapM parseName
  let mut seen : NameSet := {}
  for name in theorems do
    if seen.contains name then fail s!"duplicate theorem: {name}"
    seen := seen.insert name
  return {
    root := ← IO.FS.realPath root
    toolchainPrefix := ← IO.FS.realPath prefixSource
    moduleName, theorems
  }

def within (directory path : System.FilePath) : Bool :=
  directory.components.isPrefixOf path.components

def sameInfo (left right : ConstantInfo) : Bool :=
  match left, right with
  | .axiomInfo a, .axiomInfo b => a == b
  | .defnInfo a, .defnInfo b => a == b
  | .thmInfo a, .thmInfo b => a == b
  | .opaqueInfo a, .opaqueInfo b => a == b
  | .ctorInfo a, .ctorInfo b => a == b
  | .recInfo a, .recInfo b => a == b
  | .inductInfo a, .inductInfo b =>
    a.toConstantVal == b.toConstantVal && a.numParams == b.numParams &&
      a.numIndices == b.numIndices && a.all == b.all && a.ctors == b.ctors &&
      a.numNested == b.numNested && a.isRec == b.isRec &&
      a.isUnsafe == b.isUnsafe && a.isReflexive == b.isReflexive
  | .quotInfo a, .quotInfo b =>
    a.toConstantVal == b.toConstantVal &&
      match a.kind, b.kind with
      | .type, .type | .ctor, .ctor | .lift, .lift | .ind, .ind => true
      | _, _ => false
  | _, _ => false

def modulePath (name : Name) : IO System.FilePath := do
  unless validName name.toString do fail s!"unsupported imported module name: {name}"
  IO.FS.realPath (← findOLean name)

def load (names : Array Name) : IO Environment :=
  importModules (names.map fun moduleName => { module := moduleName }) {} 0
    (plugins := #[]) (loadExts := false)

-- BEGIN CHECKED KERNEL REPLAY
/-
Adapted from Lean/Replay.lean, Copyright (c) 2023 Kim Morrison,
released under the Apache 2.0 license. The dependency replay and postponed
constructor/recursor checks remain, but declarations enter the checked kernel
environment directly. This avoids the elaborator's privateToUserName index:
distinct full private names need not have distinct displayed names.

No declaration is inserted without kernel checking. The wrapper reconstructed
with Environment.ofKernelEnv is used only for inspection and axiom collection.
-/
namespace CheckedKernelReplay

structure Context where
  newConstants : Std.HashMap Name ConstantInfo

structure State where
  env : Kernel.Environment
  remaining : NameSet := {}
  pending : NameSet := {}
  postponedConstructors : NameSet := {}
  postponedRecursors : NameSet := {}

abbrev M := ReaderT Context <| StateRefT State IO

def expressionReferences (expression : Expr) : IO NameSet := do
  let names ← IO.mkRef ({} : NameSet)
  expression.forEach fun node => do
    match node with
    | .const name _ | .proj name _ _ => names.modify (·.insert name)
    | _ => pure ()
  names.get

/-- Include projection structure names and recursor rule expressions as well
as ordinary type/value references. The kernel still validates all expressions. -/
def references (info : ConstantInfo) : IO NameSet := do
  let mut names ← expressionReferences info.type
  match info with
  | .defnInfo value => names := names ++ (← expressionReferences value.value)
  | .thmInfo value => names := names ++ (← expressionReferences value.value)
  | .opaqueInfo value => names := names ++ (← expressionReferences value.value)
  | .inductInfo value => names := names ++ .ofList value.ctors
  | .ctorInfo value => names := names.insert value.induct
  | .recInfo value =>
    names := names ++ .ofList value.all
    for rule in value.rules do
      names := (names.insert rule.ctor) ++ (← expressionReferences rule.rhs)
  | _ => pure ()
  pure names

def isTodo (name : Name) : M Bool := do
  if (← get).remaining.contains name then
    modify fun state => { state with
      remaining := state.remaining.erase name, pending := state.pending.insert name }
    return true
  return false

def addChecked (declaration : Declaration) : M Unit := do
  match (← get).env.addDeclCore 0 declaration none with
  | .ok env => modify fun state => { state with env }
  | .error error =>
    throw <| IO.userError <| (← error.toMessageData {} |>.toString)

def lookup (name : Name) : M ConstantInfo := do
  let some info := (← read).newConstants[name]? |
    throw <| IO.userError s!"missing replay declaration: {name}"
  pure info

mutual
partial def replayConstant (name : Name) : M Unit := do
  if ← isTodo name then
    let info ← lookup name
    replayConstants (← references info)
    if (← get).pending.contains name then
      match info with
      | .defnInfo value => addChecked (.defnDecl value)
      | .thmInfo value => addChecked (.thmDecl value)
      | .axiomInfo value => addChecked (.axiomDecl value)
      | .opaqueInfo value => addChecked (.opaqueDecl value)
      | .inductInfo value =>
        unless !value.all.isEmpty && value.all.contains name do
          throw <| IO.userError "malformed replay inductive group"
        let mut members : List InductiveVal := []
        let mut seen : NameSet := {}
        for memberName in value.all do
          if seen.contains memberName then
            throw <| IO.userError "duplicate replay inductive group member"
          seen := seen.insert memberName
          let .inductInfo member ← lookup memberName |
            throw <| IO.userError "non-inductive replay group member"
          unless member.all == value.all && member.levelParams == value.levelParams &&
              member.numParams == value.numParams do
            throw <| IO.userError "inconsistent replay inductive group"
          members := members ++ [member]
        -- A single checked inductive declaration reconstructs the whole mutual
        -- block. Do not recursively try to add one of its members separately.
        for member in members do
          modify fun state => { state with
            remaining := state.remaining.erase member.name,
            pending := state.pending.erase member.name }
        let mut types : List InductiveType := []
        for member in members do
          replayConstants (← expressionReferences member.type)
          let mut constructors : List Constructor := []
          for constructorName in member.ctors do
            let .ctorInfo constructor ← lookup constructorName |
              throw <| IO.userError "non-constructor in replay inductive group"
            unless constructor.induct == member.name do
              throw <| IO.userError "replay constructor belongs to a different inductive"
            replayConstants (← references (.ctorInfo constructor))
            constructors := constructors ++ [{ name := constructor.name, type := constructor.type }]
          types := types ++ [{ name := member.name, type := member.type, ctors := constructors }]
        addChecked (.inductDecl value.levelParams value.numParams types false)
      | .ctorInfo value =>
        modify fun state => { state with
          postponedConstructors := state.postponedConstructors.insert value.name }
      | .recInfo value =>
        modify fun state => { state with
          postponedRecursors := state.postponedRecursors.insert value.name }
      | .quotInfo _ => addChecked .quotDecl
      modify fun state => { state with pending := state.pending.erase name }

partial def replayConstants (names : NameSet) : M Unit := do
  for name in names do replayConstant name
end

def checkPostponed : M Unit := do
  for name in (← get).postponedConstructors do
    match (← get).env.find? name, (← read).newConstants[name]? with
    | some (.ctorInfo actual), some (.ctorInfo expected) =>
      unless actual == expected do
        throw <| IO.userError s!"kernel replay changed constructor: {name}"
    | _, _ => throw <| IO.userError s!"kernel replay omitted constructor: {name}"
  for name in (← get).postponedRecursors do
    match (← get).env.find? name, (← read).newConstants[name]? with
    | some (.recInfo actual), some (.recInfo expected) =>
      unless actual == expected do
        throw <| IO.userError s!"kernel replay changed recursor: {name}"
    | _, _ => throw <| IO.userError s!"kernel replay omitted recursor: {name}"

def replay (newConstants : Std.HashMap Name ConstantInfo) (base : Environment) : IO Environment := do
  let mut remaining : NameSet := {}
  for (name, info) in newConstants.toList do
    unless name == info.name do
      throw <| IO.userError "kernel replay map key/name mismatch"
    if base.contains name then
      throw <| IO.userError s!"kernel replay shadows trusted declaration: {name}"
    if info.isUnsafe || info.isPartial then
      throw <| IO.userError s!"unsupported unsafe or partial replay declaration: {name}"
    remaining := remaining.insert name
  let (_, state) ← StateRefT'.run (s := { env := base.toKernelEnv, remaining }) do
    ReaderT.run (r := { newConstants }) do
      for name in remaining do replayConstant name
      checkPostponed
  unless state.remaining.isEmpty && state.pending.isEmpty do
    throw <| IO.userError "kernel replay did not finish all declarations"
  pure <| Environment.ofKernelEnv state.env

end CheckedKernelReplay
-- END CHECKED KERNEL REPLAY

def audit (request : Request) : IO Json := do
  let library ← IO.FS.realPath (request.toolchainPrefix / "lib" / "lean")
  if within library request.root || within request.root library then
    fail "artifact and toolchain roots must not overlap"

  -- Set the exact search path rather than appending ambient LEAN_PATH entries.
  searchPathRef.set [library, request.root]
  let imported ← load #[request.moduleName]
  let mut targetModules : NameSet := {}
  let mut trustedModules : NameSet := {}
  for name in imported.header.moduleNames do
    let path ← modulePath name
    if within library path then
      trustedModules := trustedModules.insert name
    else if within request.root path then
      if reservedRoot name then fail s!"artifact uses reserved module root: {name}"
      targetModules := targetModules.insert name
    else
      fail s!"module resolved outside the permitted roots: {name}"
  unless targetModules.contains request.moduleName do
    fail "target module did not resolve to the artifact snapshot"

  -- Replay only package declarations. Toolchain imports are an explicit trust
  -- base, loaded independently with the artifact root absent from the search path.
  trustedModules := trustedModules.insert `Lean |>.insert `Std |>.insert `Lean.Replay
  searchPathRef.set [library]
  let base ← load trustedModules.toArray
  for name in base.header.moduleNames do
    unless within library (← modulePath name) do
      fail s!"trusted import resolved outside the toolchain: {name}"

  -- Inspect original module entries too: importing can coalesce declarations,
  -- which must not hide a package's attempted replacement of a trusted name.
  for name in imported.header.moduleNames, data in imported.header.moduleData do
    if targetModules.contains name then
      for constant in data.constants do
        if base.contains constant.name then
          fail s!"artifact shadows a trusted declaration: {constant.name}"

  let mut additions : Std.HashMap Name ConstantInfo := {}
  for (name, info) in imported.constants.toList do
    if let some trusted := base.find? name then
      unless sameInfo trusted info do
        fail s!"trusted declaration changed in imported environment: {name}"
    else
      if info.isUnsafe || info.isPartial then
        fail s!"unsupported unsafe or partial artifact declaration: {name}"
      additions := additions.insert name info

  for name in request.theorems do
    let some (.thmInfo _) := additions[name]? |
      fail s!"requested name is not an artifact theorem: {name}"

  let replayed ← CheckedKernelReplay.replay additions base
  for (name, expected) in additions.toList do
    let some checked := replayed.toKernelEnv.find? name |
      fail s!"kernel replay omitted declaration: {name}"
    unless sameInfo expected checked do
      fail s!"kernel replay changed declaration: {name}"

  let mut theoremReports : Array Json := #[]
  for name in request.theorems do
    let some (.thmInfo checkedTheorem) := replayed.toKernelEnv.find? name |
      fail s!"kernel replay did not retain theorem: {name}"
    let (axioms, _) ← (collectAxioms name : CoreM (Array Name)).toIO
      { fileName := "<vesper-proof-audit>", fileMap := default }
      { env := replayed }
    theoremReports := theoremReports.push <| Json.mkObj [
      ("name", toJson name.toString),
      ("axioms", toJson <| (axioms.qsort Name.lt).map Name.toString),
      ("type_repr", toJson <| toString checkedTheorem.type)]

  let mut names : NameSet := trustedModules
  for name in base.header.moduleNames do names := names.insert name
  for name in imported.header.moduleNames do names := names.insert name
  searchPathRef.set [library, request.root]
  let mut modules : Array Json := #[]
  for name in names do
    modules := modules.push <| Json.mkObj [
      ("name", toJson name.toString), ("path", toJson (← modulePath name).toString)]

  return Json.mkObj [
    ("schema_version", toJson (1 : Nat)),
    ("kernel_replayed", toJson true),
    ("theorems", Json.arr theoremReports),
    ("checked_constants", toJson additions.size),
    ("modules", Json.arr modules)]

end VesperProofAudit

def main (arguments : List String) : IO UInt32 := do
  try
    let [requestPath] := arguments |
      VesperProofAudit.fail "expected one request JSON file"
    let requestJson ← IO.ofExcept <| Json.parse (← IO.FS.readFile requestPath)
    let report ← VesperProofAudit.audit (← VesperProofAudit.parseRequest requestJson)
    IO.println ("VESPER_PROOF_AUDIT_V1:" ++ report.compress)
    return (0 : UInt32)
  catch error =>
    IO.eprintln s!"proof audit failed: {error}"
    return (1 : UInt32)
