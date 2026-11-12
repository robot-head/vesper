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

  let replayed ← Environment.replay additions base
  for (name, expected) in additions.toList do
    let some checked := replayed.find? name |
      fail s!"kernel replay omitted declaration: {name}"
    unless sameInfo expected checked do
      fail s!"kernel replay changed declaration: {name}"

  let mut theoremReports : Array Json := #[]
  for name in request.theorems do
    let some (.thmInfo checkedTheorem) := replayed.find? name |
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
