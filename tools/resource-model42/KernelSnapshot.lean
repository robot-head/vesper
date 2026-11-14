import Lean
import Std
import Lean.Replay

/-! Export a kernel-only logical snapshot. The original logical ConstantInfo
objects are preserved; this tool does not certify native code equivalence.
The existing strict auditor must independently audit the resulting artifact.
Package plugins and extension initializers are not executed. Lean import may
deserialize IR/private/extension data; none of that data is exported into the
logical snapshot. This is not an OS sandbox. -/
open Lean
namespace VesperResourceKernelSnapshot

def marker : String := "VESPER_RESOURCE_KERNEL_SNAPSHOT_V1:"
def maxRequestBytes : Nat := 65536
def maxModules : Nat := 8192
def maxConstants : Nat := 262144
def maxPackageConstants : Nat := 65536
def maxExpressionNodes : Nat := 20000000
def maxReportBytes : Nat := 16777216

def fail {α : Type} (message : String) : IO α := throw <| IO.userError message

def validName (source : String) : Bool :=
  source.utf8ByteSize ≤ 1024 && !source.isEmpty &&
    (source.splitOn ".").length ≤ 128 &&
    (source.splitOn ".").all fun part => match part.toList with
      | [] => false
      | first :: rest =>
        (first.toNat < 128 && (first.isAlpha || first == '_')) &&
          rest.all (fun c => c.toNat < 128 && (c.isAlphanum || c == '_' || c == '\''))

def parseName (source : String) : IO Name := do
  unless validName source do fail "unsupported module name"
  pure ((source.splitOn ".").foldl Name.str .anonymous)

def reservedRoot (name : Name) : Bool :=
  ["Lean","Std","Init","Lake"].contains name.getRoot.toString

def within (directory path : System.FilePath) : Bool :=
  directory.components.isPrefixOf path.components

structure Request where
  root : System.FilePath
  toolchainPrefix : System.FilePath
  moduleName : Name
  outputRoot : System.FilePath
  outputModule : Name

def readRequest (path : System.FilePath) : IO Json := do
  let handle ← IO.FS.Handle.mk path .read
  let mut bytes := ByteArray.empty
  for _ in [:maxRequestBytes+1] do
    let remaining := maxRequestBytes+1-bytes.size
    if remaining == 0 then fail "request exceeds byte limit"
    let part ← handle.read remaining.toUSize
    if part.isEmpty then
      let some source := String.fromUTF8? bytes | fail "request is not UTF-8"
      return ← IO.ofExcept (Json.parse source)
    bytes := bytes ++ part
    if bytes.size > maxRequestBytes then fail "request exceeds byte limit"
  fail "request read work limit"

def parseRequest (json : Json) : IO Request := do
  let object ← IO.ofExcept json.getObj?
  let keys := object.toArray.map Prod.fst
  unless keys.size == 6 && keys.all (fun key =>
      ["schema_version","root","toolchain_prefix","module","output_root","output_module"].contains key) do
    fail "unsupported request fields"
  let schema ← IO.ofExcept (json.getObjValAs? Nat "schema_version")
  unless schema == 1 do fail "unsupported request schema"
  let rootSource ← IO.ofExcept (json.getObjValAs? String "root")
  let prefixSource ← IO.ofExcept (json.getObjValAs? String "toolchain_prefix")
  let outputSource ← IO.ofExcept (json.getObjValAs? String "output_root")
  unless rootSource.utf8ByteSize ≤ 4096 && prefixSource.utf8ByteSize ≤ 4096 &&
      outputSource.utf8ByteSize ≤ 4096 do fail "request path limit"
  let root : System.FilePath := rootSource
  let sysroot : System.FilePath := prefixSource
  let output : System.FilePath := outputSource
  unless root.isAbsolute && sysroot.isAbsolute && output.isAbsolute do fail "paths must be absolute"
  let moduleName ← parseName (← IO.ofExcept (json.getObjValAs? String "module"))
  let outputModule ← parseName (← IO.ofExcept (json.getObjValAs? String "output_module"))
  if reservedRoot moduleName || reservedRoot outputModule then fail "reserved module root"
  unless !(← output.pathExists) do fail "output_root must be fresh and nonexistent"
  let some parent := output.parent | fail "output_root has no parent"
  let some leaf := output.fileName | fail "output_root has no final component"
  unless leaf != "." && leaf != ".." do fail "invalid output_root leaf"
  let parent ← IO.FS.realPath parent
  pure ⟨← IO.FS.realPath root,← IO.FS.realPath sysroot,moduleName,parent / leaf,outputModule⟩

def sameInfo (left right : ConstantInfo) : Bool :=
  match left,right with
  | .axiomInfo a,.axiomInfo b => a == b
  | .defnInfo a,.defnInfo b => a == b
  | .thmInfo a,.thmInfo b => a == b
  | .opaqueInfo a,.opaqueInfo b => a == b
  | .ctorInfo a,.ctorInfo b => a == b
  | .recInfo a,.recInfo b => a == b
  | .inductInfo a,.inductInfo b =>
      a.toConstantVal == b.toConstantVal && a.numParams == b.numParams &&
      a.numIndices == b.numIndices && a.all == b.all && a.ctors == b.ctors &&
      a.numNested == b.numNested && a.isRec == b.isRec &&
      a.isUnsafe == b.isUnsafe && a.isReflexive == b.isReflexive
  | .quotInfo a,.quotInfo b =>
      a.toConstantVal == b.toConstantVal && match a.kind,b.kind with
        | .type,.type | .ctor,.ctor | .lift,.lift | .ind,.ind => true
        | _,_ => false
  | _,_ => false

def load (names : Array Name) : IO Environment :=
  importModules (names.map fun name => {module := name}) {} 0 (plugins := #[]) (loadExts := false)

def modulePath (name : Name) : IO System.FilePath := do
  unless validName name.toString do fail "unsupported imported module name"
  IO.FS.realPath (← findOLean name)

def checkConstantName (name : Name) : IO Unit := do
  let mut current := name
  let mut bytes := 0
  for _ in [:512] do
    match current with
    | .anonymous => return
    | .str parent part =>
      bytes := bytes + part.utf8ByteSize + 1
      if bytes > 4096 then fail "constant name byte limit"
      current := parent
    | .num parent number =>
      if number ≥ 2^64 then fail "constant name number limit"
      bytes := bytes + 21
      if bytes > 4096 then fail "constant name byte limit"
      current := parent
  fail "constant name component limit"

/-- This is an exclusion shape check, never evidence of native implementation
provenance or runtime equivalence. Explicit unsafe declarations never qualify. -/
def matchedPartial (all : Std.HashMap Name ConstantInfo) (owners : Std.HashMap Name Name)
    (info : ConstantInfo) : IO Name := do
  let .defnInfo auxiliary := info | fail "partial declaration is not a definition"
  unless auxiliary.safety == .partial do fail "omitted declaration is not partial"
  let .str parent "_unsafe_rec" := auxiliary.name | fail "unmatched partial declaration name"
  let some (.defnInfo owner) := all[parent]? | fail "partial auxiliary lacks safe definition owner"
  unless owner.safety == .safe do fail "partial auxiliary owner is not safe"
  let some ownerModule := owners[parent]? | fail "partial owner has no module association"
  unless owners[auxiliary.name]? == some ownerModule do fail "partial auxiliary crosses module ownership"
  unless !owner.all.isEmpty && owner.all.contains parent && owner.all.length ≤ maxPackageConstants do
    fail "partial owner has malformed mutual group"
  unless auxiliary.type == owner.type && auxiliary.levelParams == owner.levelParams &&
      auxiliary.all == owner.all.map (fun name => .str name "_unsafe_rec") &&
      auxiliary.hints == .opaque do fail "partial auxiliary metadata mismatch"
  let mut group : NameSet := {}
  for member in owner.all do
    if group.contains member then fail "duplicate partial owner mutual group member"
    group := group.insert member
    unless owners[member]? == some ownerModule &&
        owners[Name.str member "_unsafe_rec"]? == some ownerModule do
      fail "partial mutual group crosses module ownership"
    let some (.defnInfo memberOwner) := all[member]? | fail "missing safe mutual owner"
    let some (.defnInfo memberAux) := all[Name.str member "_unsafe_rec"]? |
      fail "missing partial mutual auxiliary"
    unless memberOwner.safety == .safe && memberAux.safety == .partial &&
        memberOwner.all == owner.all && memberAux.all == auxiliary.all &&
        memberOwner.type == memberAux.type && memberOwner.levelParams == memberAux.levelParams &&
        memberAux.hints == .opaque do fail "partial mutual auxiliary metadata mismatch"
  pure parent

/-- Full expression references include projection structure names, which the
stock foldConsts helper intentionally does not collect. -/
def scanExpr (expression : Expr) (initial : NameSet) (work : Nat) : IO (NameSet × Nat) := do
  let mut pending := #[expression]
  let mut visited : Std.HashSet Expr := {}
  let mut names := initial
  let mut work := work
  for _ in [:maxExpressionNodes+1] do
    if pending.isEmpty then return (names,work)
    let node := pending.back!
    pending := pending.pop
    if visited.contains node then continue
    if work ≥ maxExpressionNodes then fail "logical expression traversal limit"
    work := work + 1
    visited := visited.insert node
    match node with
    | .const name _ => names := names.insert name
    | .proj name _ body =>
      names := names.insert name
      pending := pending.push body
    | .forallE _ domain body _ | .lam _ domain body _ =>
      pending := (pending.push domain).push body
    | .letE _ type value body _ => pending := ((pending.push type).push value).push body
    | .app function argument => pending := (pending.push function).push argument
    | .mdata _ body => pending := pending.push body
    | .mvar _ | .fvar _ => fail "free or metavariable in logical declaration"
    | _ => pure ()
    if pending.size > maxExpressionNodes - work then fail "logical expression pending limit"
    if names.size > maxConstants then fail "logical reference count limit"
  fail "logical expression traversal limit"

def constantReferences (info : ConstantInfo) (work : Nat) : IO (NameSet × Nat) := do
  let (names,work) ← scanExpr info.type {} work
  match info with
  | .defnInfo value =>
    let (names,work) ← scanExpr value.value names work
    pure (value.all.foldl (fun names name => names.insert name) names,work)
  | .thmInfo value =>
    let (names,work) ← scanExpr value.value names work
    pure (value.all.foldl (fun names name => names.insert name) names,work)
  | .opaqueInfo value => scanExpr value.value names work
  | .inductInfo value =>
    pure ((value.all ++ value.ctors).foldl (fun names name => names.insert name) names,work)
  | .ctorInfo value => pure (names.insert value.induct,work)
  | .recInfo value =>
    let mut names := value.all.foldl (fun names name => names.insert name) names
    let mut work := work
    for rule in value.rules do
      names := names.insert rule.ctor
      let scanned ← scanExpr rule.rhs names work
      names := scanned.1
      work := scanned.2
    pure (names,work)
  | _ => pure (names,work)

/-- All retained package declarations are roots, not only requested theorems.
The complete logical dependency closure, including toolchain nodes, is checked. -/
def checkLogicalClosure (original : Environment) (retained : Std.HashMap Name ConstantInfo)
    (alternates : Array ConstantInfo := #[]) : IO Nat := do
  let mut queued : NameSet := {}
  let mut pending : Array Name := #[]
  for (name,_) in retained.toList do
    queued := queued.insert name
    pending := pending.push name
  let mut work := 0
  for alternate in alternates do
    if alternate.isUnsafe || alternate.isPartial then fail "unsafe alternate declaration"
    let (references,nextWork) ← constantReferences alternate work
    work := nextWork
    for dependency in references do
      if queued.contains dependency then continue
      if pending.size ≥ maxConstants then fail "logical dependency count limit"
      queued := queued.insert dependency
      pending := pending.push dependency
  for index in [:maxConstants] do
    if index ≥ pending.size then return work
    let name := pending[index]!
    let some info := original.find? name | fail "missing logical dependency"
    if info.isUnsafe || info.isPartial then fail s!"unsafe or partial logical dependency: {name}"
    let (references,nextWork) ← constantReferences info work
    work := nextWork
    for dependency in references do
      if queued.contains dependency then continue
      if pending.size ≥ maxConstants then fail "logical dependency count limit"
      queued := queued.insert dependency
      pending := pending.push dependency
  fail "logical dependency count limit"

structure ModuleRecord where
  name : Name
  path : System.FilePath

def moduleJson (record : ModuleRecord) : Json := Json.mkObj [
  ("name",toJson record.name.toString),("path",toJson record.path.toString)]

/-- Check every retained ConstantInfo, every regenerated constructor/recursor,
and the exact complete inventory. No package declaration is permitted to appear
silently or be dropped merely because no requested theorem references it. -/
def verifyEnvironment (expected : Std.HashMap Name ConstantInfo) (base actual : Environment) : IO Unit := do
  for (name,info) in expected.toList do
    let some observed := actual.toKernelEnv.find? name | fail s!"missing logical declaration: {name}"
    unless sameInfo info observed do fail s!"logical ConstantInfo changed: {name}"
  let count ← actual.constants.foldM (init := 0) fun count name info => do
    if let some trusted := base.find? name then
      unless sameInfo trusted info do fail "trusted constant changed"
      pure count
    else
      unless expected.contains name do fail s!"unexpected logical declaration: {name}"
      if count ≥ maxPackageConstants then fail "package constant limit"
      pure (count+1)
  unless count == expected.size do fail "logical inventory count mismatch"

structure RawOccurrence where
  info : ConstantInfo
  moduleName : Name
  index : Nat

def sameTheoremHeader (left right : ConstantInfo) : Bool :=
  match left,right with
  | .thmInfo a,.thmInfo b => a.toConstantVal == b.toConstantVal && a.all == b.all
  | _,_ => false

def checkTheoremGroup (value : TheoremVal) : IO Unit := do
  unless !value.all.isEmpty && value.all.contains value.name && value.all.length ≤ maxPackageConstants do
    fail "malformed alternate theorem group"
  let mut seen : NameSet := {}
  for name in value.all do
    checkConstantName name
    if seen.contains name then fail "duplicate alternate theorem group member"
    seen := seen.insert name


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

def snapshot (request : Request) : IO Json := do
  let actualPrefix ← IO.FS.realPath (← getBuildDir)
  unless actualPrefix == request.toolchainPrefix do fail "toolchain prefix does not match running Lean"
  let library ← IO.FS.realPath (request.toolchainPrefix / "lib" / "lean")
  if within library request.root || within request.root library ||
      within request.root request.outputRoot || within request.outputRoot request.root ||
      within library request.outputRoot || within request.outputRoot library then
    fail "source, projection and toolchain roots must be disjoint"
  searchPathRef.set [library,request.root]
  let original ← load #[request.moduleName]
  if original.header.moduleNames.size > maxModules then fail "module count limit"
  let mut packages : NameSet := {}
  let mut trusted : NameSet := {}
  let mut originalModules : Array ModuleRecord := #[]
  for name in original.header.moduleNames do
    let path ← modulePath name
    if within library path then
      trusted := trusted.insert name
    else if within request.root path then
      if reservedRoot name then fail "package uses reserved module root"
      packages := packages.insert name
      originalModules := originalModules.push ⟨name,path⟩
    else fail "import escaped artifact/toolchain roots"
  unless packages.contains request.moduleName do fail "original module is not a package artifact"
  trusted := trusted.insert `Lean |>.insert `Std |>.insert `Lean.Replay
  let trustedNames := trusted.toArray.qsort Name.lt
  searchPathRef.set [library]
  let base ← load trustedNames
  let mut trustedModules : Array ModuleRecord := #[]
  for name in base.header.moduleNames do
    let path ← modulePath name
    unless within library path do fail "trusted import escaped toolchain"
    trustedModules := trustedModules.push ⟨name,path⟩
  let mut raw : Std.HashMap Name ConstantInfo := {}
  let mut owners : Std.HashMap Name Name := {}
  let mut duplicates : Array Json := #[]
  let mut rawOccurrences : Array RawOccurrence := #[]
  for name in original.header.moduleNames, data in original.header.moduleData do
    if packages.contains name then
      unless data.constNames == data.constants.map ConstantInfo.name do
        fail "original redundant constant-name inventory mismatch"
      for index in [:data.constants.size] do
        let info := data.constants[index]!
        if rawOccurrences.size ≥ maxPackageConstants * 2 then fail "original occurrence count limit"
        checkConstantName info.name
        if base.contains info.name then fail s!"package shadows trusted declaration: {info.name}"
        if info.isUnsafe then fail s!"explicit unsafe declaration: {info.name}"
        rawOccurrences := rawOccurrences.push ⟨info,name,index⟩
        if let some previous := raw[info.name]? then
          unless sameTheoremHeader previous info && !previous.isUnsafe && !info.isUnsafe &&
              !previous.isPartial && !info.isPartial do
            fail s!"non-theorem or changed theorem header duplicate: {info.name}"
          if duplicates.size ≥ maxPackageConstants then fail "duplicate occurrence count limit"
          let some firstModule := owners[info.name]? | fail "duplicate has no first module association"
          duplicates := duplicates.push <| Json.mkObj [
            ("name",toJson info.name.toString),("first_module",toJson firstModule.toString),
            ("repeated_module",toJson name.toString),
            ("reason",toJson (if sameInfo previous info then "identical_safe_theorem_realization"
              else "safe_theorem_proof_variant"))]
          continue
        if raw.size ≥ maxPackageConstants then fail "package constant limit"
        raw := raw.insert info.name info
        owners := owners.insert info.name name
  -- The canonical logical world is exactly the ORIGINAL imported environment,
  -- not whichever physical theorem realization we happened to encounter first.
  let mut canonical : Std.HashMap Name ConstantInfo := {}
  for (name,first) in raw.toList do
    let some info := original.find? name | fail "original import omitted raw declaration"
    unless sameInfo first info || sameTheoremHeader first info do
      fail "canonical original/import ConstantInfo header mismatch"
    canonical := canonical.insert name info
  let mut canonicalOwners : Std.HashMap Name Name := {}
  for occurrence in rawOccurrences do
    let some info := canonical[occurrence.info.name]? | fail "unassociated raw occurrence"
    unless sameInfo occurrence.info info || sameTheoremHeader occurrence.info info do
      fail "raw occurrence changed canonical theorem header"
    if sameInfo occurrence.info info && !canonicalOwners.contains info.name then
      canonicalOwners := canonicalOwners.insert info.name occurrence.moduleName
  unless canonicalOwners.size == canonical.size do
    fail "canonical ConstantInfo does not match an actual raw occurrence"
  let rawCount ← original.constants.foldM (init := 0) fun count name info => do
    if let some trustedInfo := base.find? name then
      unless sameInfo trustedInfo info do fail "trusted declaration changed in original import"
      pure count
    else
      let some expected := canonical[name]? | fail "original import has unassociated package declaration"
      unless sameInfo expected info do fail "original canonical ConstantInfo mismatch"
      if count ≥ maxPackageConstants then fail "package constant limit"
      pure (count+1)
  unless rawCount == canonical.size do fail "original raw/import inventories differ"
  unless rawOccurrences.size == canonical.size + duplicates.size do fail "original occurrence partition mismatch"
  let mut retained : Std.HashMap Name ConstantInfo := {}
  let mut omitted : Array Json := #[]
  for (name,info) in canonical.toList.toArray.qsort (fun a b => Name.lt a.1 b.1) do
    if info.isUnsafe then fail s!"explicit unsafe declaration: {name}"
    if info.isPartial then
      let parent ← matchedPartial canonical owners info
      let some owner := owners[name]? | fail "missing original module association"
      omitted := omitted.push <| Json.mkObj [
        ("name",toJson name.toString),("parent",toJson parent.toString),
        ("module",toJson owner.toString),("reason",toJson "matched_partial_unsafe_rec")]
    else
      if info matches .opaqueInfo _ then fail s!"unsupported package opaque declaration: {name}"
      retained := retained.insert name info
  unless canonical.size == retained.size + omitted.size do fail "partition count mismatch"
  let mut alternates : Array ConstantInfo := #[]
  for occurrence in rawOccurrences do
    if let some info := retained[occurrence.info.name]? then
      unless sameInfo occurrence.info info do
        unless sameTheoremHeader occurrence.info info do fail "alternate is not a safe theorem header copy"
        if alternates.size ≥ maxPackageConstants then fail "alternate proof count limit"
        alternates := alternates.push occurrence.info
  let work ← checkLogicalClosure original retained alternates
  -- Prove the entire unchanged canonical world before checking any alias.
  let replayed ← CheckedKernelReplay.replay retained base
  verifyEnvironment retained base replayed
  let mut exported := retained
  let mut checkedAliases := replayed
  let mut proofAliases : Array Json := #[]
  let mut occurrenceReports : Array Json := #[]
  for occurrence in rawOccurrences do
    let name := occurrence.info.name
    let mut alias : Option String := none
    let mut disposition := "omitted_auxiliary"
    if let some canonicalInfo := retained[name]? then
      if sameInfo occurrence.info canonicalInfo then
        disposition := "retained_canonical_info"
      else
        let .thmInfo value := occurrence.info | fail "alternate is not a theorem"
        unless sameTheoremHeader occurrence.info canonicalInfo do fail "alternate theorem header mismatch"
        checkTheoremGroup value
        if exported.size ≥ maxPackageConstants then fail "expanded output constant limit"
        let aliasName := Name.str (Name.mkSimple "ResourceKernelProofAliases")
          ("realization_" ++ toString proofAliases.size)
        if original.contains aliasName || base.contains aliasName || exported.contains aliasName then
          fail "synthetic proof alias name collision"
        -- Only the declaration label/group changes. Its type, universe names
        -- and actual raw proof body are copied exactly, never rewritten.
        let aliasValue : TheoremVal := {value with name := aliasName, all := [aliasName]}
        unless aliasValue.type == value.type && aliasValue.levelParams == value.levelParams &&
            aliasValue.value == value.value do fail "alternate proof body association changed"
        match checkedAliases.toKernelEnv.addDeclCore 0 (.thmDecl aliasValue) none with
        | .error _ => fail s!"kernel rejected alternate proof alias: {aliasName}"
        | .ok checked => checkedAliases := Environment.ofKernelEnv checked
        exported := exported.insert aliasName (.thmInfo aliasValue)
        let some canonicalModule := canonicalOwners[name]? | fail "canonical theorem lacks raw occurrence"
        proofAliases := proofAliases.push <| Json.mkObj [
          ("name",toJson aliasName.toString),("original_name",toJson name.toString),
          ("source_module",toJson occurrence.moduleName.toString),
          ("canonical_module",toJson canonicalModule.toString),
          ("source_occurrence",toJson occurrence.index),
          ("original_group",toJson (value.all.map Name.toString)),
          ("reason",toJson "checked_safe_theorem_variant")]
        alias := some aliasName.toString
        disposition := "proof_alias"
    occurrenceReports := occurrenceReports.push <| Json.mkObj [
      ("name",toJson name.toString),("module",toJson occurrence.moduleName.toString),
      ("index",toJson occurrence.index),("disposition",toJson disposition),("alias",toJson alias)]
  unless proofAliases.size == alternates.size do fail "alternate proof mapping count mismatch"
  verifyEnvironment exported base checkedAliases
  let constants := exported.toList.toArray.qsort (fun a b => Name.lt a.1 b.1) |>.map Prod.snd
  let data : ModuleData := {
    isModule := false
    imports := trustedNames.map fun name => {module := name}
    constNames := constants.map ConstantInfo.name
    constants := constants
    extraConstNames := #[]
    entries := #[] }
  IO.FS.createDir request.outputRoot
  let output := request.outputModule.toString.splitOn "." |>.foldl (fun path part => path / part) request.outputRoot
  let output := output.withExtension "olean"
  if let some parent := output.parent then IO.FS.createDirAll parent
  saveModuleData output request.outputModule data
  searchPathRef.set [library,request.outputRoot]
  let reimported ← load #[request.outputModule]
  let mut projectedCount := 0
  for name in reimported.header.moduleNames, data in reimported.header.moduleData do
    let path ← modulePath name
    if within library path then continue
    unless name == request.outputModule && path == (← IO.FS.realPath output) do
      fail "projection imports unexpected package artifact"
    unless data.entries.isEmpty && data.extraConstNames.isEmpty do fail "projection contains extensions or IR metadata"
    unless data.constNames == data.constants.map ConstantInfo.name do
      fail "projection redundant constant-name inventory mismatch"
    unless data.imports.map Import.module == trustedNames do fail "projection import inventory changed"
    unless data.constants.size == exported.size do fail "projection raw constant count mismatch"
    for info in data.constants do
      let some expected := exported[info.name]? | fail "projection contains unexpected constant"
      unless sameInfo expected info do fail "projection raw ConstantInfo changed"
    projectedCount := projectedCount+1
  unless projectedCount == 1 do fail "projection module participation mismatch"
  verifyEnvironment exported base reimported
  pure <| Json.mkObj [
    ("schema_version",toJson (1 : Nat)),("scope",toJson "kernel_logical_projection"),
    ("original_module",toJson request.moduleName.toString),
    ("projection_module",toJson request.outputModule.toString),
    ("projection_path",toJson (← IO.FS.realPath output).toString),
    ("original_modules",toJson (originalModules.map moduleJson)),
    ("trusted_modules",toJson (trustedModules.map moduleJson)),
    ("original_constants",toJson canonical.size),("retained_constants",toJson retained.size),
    ("original_constant_occurrences",toJson rawOccurrences.size),
    ("duplicate_constants",toJson duplicates.size),("duplicates",Json.arr duplicates),
    ("occurrences",Json.arr occurrenceReports),
    ("alias_scope",toJson "synthetic_theorem_proof_variants"),
    ("proof_alias_count",toJson proofAliases.size),("proof_aliases",Json.arr proofAliases),
    ("output_constants",toJson exported.size),
    ("omitted_constants",toJson omitted.size),("omitted",Json.arr omitted),
    ("logical_expression_nodes",toJson work),
    ("exact_inventory",toJson true),("constant_info_equal",toJson true),
    ("kernel_replayed",toJson true),("reimport_verified",toJson true),
    ("extensions_empty",toJson true),("ir_empty",toJson true)]

end VesperResourceKernelSnapshot

def main (arguments : List String) : IO UInt32 := do
  try
    let [path] := arguments | VesperResourceKernelSnapshot.fail "expected one request JSON path"
    let request ← VesperResourceKernelSnapshot.parseRequest (← VesperResourceKernelSnapshot.readRequest path)
    let report ← VesperResourceKernelSnapshot.snapshot request
    let text := report.compress
    if text.utf8ByteSize > VesperResourceKernelSnapshot.maxReportBytes then
      VesperResourceKernelSnapshot.fail "report byte limit"
    IO.println (VesperResourceKernelSnapshot.marker ++ text)
    pure 0
  catch error =>
    IO.eprintln ("kernel snapshot failed: " ++ error.toString.take 4096)
    pure 1
