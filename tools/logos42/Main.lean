/-
Copyright (c) 2023 Scott Morrison. All rights reserved.
Released under Apache 2.0 license as described in the file LICENSE.
Authors: Scott Morrison

Vesper's fixed adapter binds exact Lean artifacts to the pinned Logos checker.
The exporter is part of the trusted translation boundary; only Logos acceptance
and exact checked-environment coverage permit successful-check frames.
-/
import Lean.CoreM
import Lean.Util.FoldConsts
import Lean.Data.Json
import Export
import LogosControls

namespace Vesper.Logos42Machine
open Lean hiding Environment Exception

structure Failure where
  category : String
  detail : String
abbrev MachineM := ExceptT Failure IO

private def fail (category detail : String) : MachineM α := throw ⟨category, detail⟩
private def limit (detail : String) : MachineM α := fail "limit" detail
private def coverageError (detail : String) : MachineM α := fail "coverage" detail
private def jn (n : Nat) : Json := toJson n
private def jb (b : Bool) : Json := jn (if b then 1 else 0)
private def frame (tag : Nat) (rest : Array Json := #[]) : Json := .arr (#[jn 1, jn tag] ++ rest)
private def natArray (a : Array Nat) : Json := .arr (a.map jn)
private def hardConstants : Nat := 524288
private def hardNameBytes : Nat := 33554432
private def hardInventoryBytes : Nat := 134217728
private def hardStdout : Nat := 67108864

structure Output where
  handle : IO.FS.Stream
  bytes : IO.Ref Nat

def send (out : Output) (message : Json) : MachineM Unit := do
  let payload := message.compress.toUTF8
  let n := payload.size
  if n == 0 || n > 65536 then limit "outgoing frame limit"
  let used ← out.bytes.get
  if used + n + 4 > hardStdout then limit "stdout limit"
  let header : ByteArray := ⟨#[UInt8.ofNat n, UInt8.ofNat (n / 256),
    UInt8.ofNat (n / 65536), UInt8.ofNat (n / 16777216)]⟩
  let result ← (try
    -- FS.Stream.write writes the full buffer or raises an IO error.
    out.handle.write header
    out.handle.write payload
    out.handle.flush
    out.bytes.set (used + n + 4)
    pure true
  catch _ => pure false : IO Bool)
  if !result then fail "io" "writing framed output failed"


/-- Bounded component walk before recursive comparison/JSON conversion. -/
def encodeName (name : Name) : Except Failure (Json × Nat) := do
  let mut current := name
  let mut components : Array Json := #[]
  let mut bytes := 2
  let mut complete := false
  for _ in [:129] do
    match current with
    | .anonymous => complete := true; break
    | .str parent text =>
      if components.size >= 128 || text.isEmpty || text.utf8ByteSize > 1024 then
        throw ⟨"limit", "name component limit"⟩
      bytes := bytes + 5 + text.utf8ByteSize
      if bytes > 4096 then throw ⟨"limit", "encoded name limit"⟩
      components := components.push (.arr #[jn 0, .str text])
      current := parent
    | .num parent number =>
      if components.size >= 128 || number > 18446744073709551615 then
        throw ⟨"limit", "numeric name component limit"⟩
      bytes := bytes + 9
      if bytes > 4096 then throw ⟨"limit", "encoded name limit"⟩
      components := components.push (.arr #[jn 1, .str (toString number)])
      current := parent
  if !complete || components.isEmpty then throw ⟨"limit", "empty or deep name"⟩
  pure (.arr components.reverse, bytes)

private def encoded (name : Name) : MachineM (Json × Nat) :=
  match encodeName name with | .ok x => pure x | .error e => throw e

private def formOf : ConstantInfo → Nat
  | .axiomInfo _ => 0 | .defnInfo _ => 1 | .thmInfo _ => 2 | .opaqueInfo _ => 3
  | .inductInfo _ => 4 | .ctorInfo _ => 5 | .recInfo _ => 6 | .quotInfo _ => 7
private def skipOf (ci : ConstantInfo) : Nat :=
  (if ci.isUnsafe then 1 else 0) + (if ci.isPartial then 2 else 0)
private def quotientNames : Array Name := #[``Quot, ``Quot.mk, ``Quot.lift, ``Quot.ind]
private def quotientKind : QuotKind → Nat
  | .type => 0 | .ctor => 1 | .lift => 2 | .ind => 3
private def quotientMatches (a b : QuotVal) : Bool :=
  a.toConstantVal == b.toConstantVal && quotientKind a.kind == quotientKind b.kind

def targetName : Nat → Name
  | 0 => `RewriteWitness | 1 => `ArithmeticOracle
  | 2 => `LogosControls | 3 => `ResourceModel42Kernel
  | _ => .anonymous
private def rwRoot (s : String) : Name := .str (.str (.str .anonymous "Vesper") "RewriteWitness") s
private def arRoot (s : String) : Name := .str (.str (.str .anonymous "Vesper") "ArithmeticOracle") s
private def controlRoot : Name := `Vesper.Logos42Controls.runControls
private def witnessRoots : Array Name := (#[
  "r01_add_zero", "r02_zero_add", "r03_sub_zero", "r04_mul_one", "r05_one_mul",
  "r06_mul_zero", "r07_zero_mul", "r08_self_sub", "r09_wadd_zero", "r10_wsub_zero",
  "r11_wmul_one", "r12_wadd_comm", "r13_wmul_comm", "r14_wadd_assoc", "r15_wmul_assoc",
  "r16_wadd_cancel", "r17_wmul_zero", "r18_wself_sub", "r19_checked_add_constants",
  "r20_checked_sub_constants", "r21_checked_mul_constants", "r22_wrapping_add_constants",
  "r23_wrapping_sub_constants", "r24_wrapping_mul_constants", "value_fits", "literal_exact",
  "value_toInt_injective", "literal_roundtrip", "total_const", "total_var", "total_of_eq",
  "congr_checked", "congr_wrapping"] : Array String).map rwRoot
private def arithmeticRoots : Array Name :=
  (#["checkExpr", "checkProgram", "evalSteps", "processRequest"] : Array String).map arRoot

structure ModuleRecord where
  name : Name
  path : String
  deriving Inhabited
structure Entry where
  name : Name
  info : ConstantInfo
  moduleId : Nat
  form : Nat
  skip : Nat
  owners : Array Nat := #[]
  deriving Inhabited
structure Inventory where
  target : Nat
  modules : Array ModuleRecord
  entries : Array Entry
  index : Std.HashMap Name Nat
  forms : Array Nat
  eligibility : Array Nat
  targetEligibility : Array Nat
  nameBytes : Nat
  required : Array Nat

private def ownerGroup (index : Std.HashMap Name Nat) (names : List Name) : MachineM (Array Nat) := do
  if names.isEmpty || names.length > 64 then coverageError "invalid metadata owner group size"
  let mut ids : Array Nat := #[]
  for name in names do
    let some id := index[name]? | coverageError "metadata owner missing from inventory"
    ids := ids.push id
  ids := ids.qsort (· < ·)
  for i in [1:ids.size] do
    if ids[i-1]! == ids[i]! then coverageError "duplicate metadata owner"
  pure ids

/-- Native loaded data are bounded before constructing the retained index. -/
def buildInventory (env : Lean.Environment) (target : Nat) : MachineM Inventory := do
  let constants := env.constants
  let count := constants.fold (fun n _ _ => n + 1) 0
  if count == 0 || count > hardConstants then limit "constant inventory count"
  if env.header.modules.isEmpty || env.header.modules.size > 8192 then limit "module inventory count"
  let moduleNames := env.header.moduleNames
  let mut moduleBytes := 0
  for name in moduleNames do
    let (_, n) ← encoded name
    moduleBytes := moduleBytes + n
  let sortedModules := moduleNames.qsort (fun a b => Name.cmp a b == .lt)
  let mut modules : Array ModuleRecord := #[]
  let mut moduleIndex : Std.HashMap Name Nat := {}
  for name in sortedModules do
    if moduleIndex.contains name then coverageError "duplicate module name"
    let resolved ← findOLean name
    let path := resolved.toString
    if path.isEmpty || path.utf8ByteSize > 4096 then limit "resolved module path limit"
    moduleBytes := moduleBytes + path.utf8ByteSize + 64
    if moduleBytes + count * 256 > hardInventoryBytes then limit "retained inventory metadata"
    moduleIndex := moduleIndex.insert name modules.size
    modules := modules.push ⟨name, path⟩
  let mut entries : Array Entry := #[]
  let mut nameBytes := 0
  let mut forms := Array.replicate 8 0
  let mut eligibility := Array.replicate 4 0
  let mut targetEligibility := Array.replicate 4 0
  for (name, ci) in constants do
    let (_, n) ← encoded name
    nameBytes := nameBytes + n
    if nameBytes > hardNameBytes || nameBytes + moduleBytes + count * 256 > hardInventoryBytes then
      limit "aggregate inventory names or metadata"
    if ci.name != name then coverageError "constant map key mismatch"
    let some originalModule := env.getModuleIdxFor? name | coverageError "constant origin missing"
    let some original := env.header.modules[originalModule.toNat]? |
      coverageError "constant origin outside module array"
    let some moduleId := moduleIndex[original.module]? | coverageError "origin module absent"
    let form := formOf ci
    let skip := skipOf ci
    forms := forms.modify form (· + 1)
    eligibility := eligibility.modify skip (· + 1)
    if original.module == targetName target then
      targetEligibility := targetEligibility.modify skip (· + 1)
    entries := entries.push ⟨name, ci, moduleId, form, skip, #[]⟩
  entries := entries.qsort (fun a b => Name.cmp a.name b.name == .lt)
  let mut index : Std.HashMap Name Nat := {}
  for i in [:entries.size] do
    let e := entries[i]!
    if index.contains e.name then coverageError "duplicate constant name"
    index := index.insert e.name i
  let quotientIds ← ownerGroup index quotientNames.toList
  let mut ownerBytes := 0
  for i in [:entries.size] do
    let entry := entries[i]!
    let owners ← match entry.info with
      | .inductInfo info => ownerGroup index info.all
      | .ctorInfo info => ownerGroup index [info.induct]
      | .recInfo info => ownerGroup index info.all
      | .quotInfo info => do
        let some nameIndex := quotientNames.findIdx? (· == info.name) |
          coverageError "unexpected quotient name"
        if quotientKind info.kind != nameIndex then coverageError "quotient kind/name mismatch"
        pure quotientIds
      | _ => pure #[]
    ownerBytes := ownerBytes + owners.size * 4 + 2
    if nameBytes + moduleBytes + count * 256 + ownerBytes > hardInventoryBytes then
      limit "owner inventory metadata"
    entries := entries.set! i {entry with owners}
  for i in [:entries.size] do
    let entry := entries[i]!
    if entry.form == 4 || entry.form == 6 then
      for owner in entry.owners do
        let some e := entries[owner]? | coverageError "owner outside inventory"
        if e.form != 4 || e.skip != entry.skip || e.owners != entry.owners ||
            e.moduleId != entry.moduleId then
          coverageError "inconsistent mutual owner metadata"
      if entry.form == 4 && !entry.owners.contains i then coverageError "inductive omits itself"
    else if entry.form == 5 then
      let some owner := entry.owners[0]? | coverageError "constructor owner missing"
      let some e := entries[owner]? | coverageError "constructor owner outside inventory"
      if e.form != 4 || e.skip != entry.skip || e.moduleId != entry.moduleId then
        coverageError "invalid constructor owner"
    else if entry.form == 7 then
      if entry.skip != 0 || entry.owners != quotientIds then coverageError "invalid quotient group"
      for owner in quotientIds do
        let some e := entries[owner]? | coverageError "quotient ID missing"
        if e.form != 7 || e.moduleId != entry.moduleId then coverageError "quotient group form or origin"
  if target == 3 then
    for e in entries do
      if modules[e.moduleId]!.name == targetName target && e.skip != 0 then
        coverageError "resource projection contains skipped declarations"
  let mut required := if target == 3 then Array.mkEmpty targetEligibility[0]! else #[]
  if target == 3 then
    for i in [:entries.size] do
      let e := entries[i]!
      if modules[e.moduleId]!.name == targetName target then
        required := required.push i
  else
    let roots := if target == 2 then #[controlRoot]
      else if target == 0 then witnessRoots else witnessRoots ++ arithmeticRoots
    for root in roots do
      let some id := index[root]? | coverageError "required root absent"
      let e := entries[id]!
      let expectedModule := if target == 2 then targetName 2
        else if witnessRoots.contains root then targetName 0 else targetName 1
      if e.skip != 0 || modules[e.moduleId]!.name != expectedModule then
        coverageError "required root origin or eligibility"
      if (witnessRoots.contains root && e.form != 2) ||
          (!witnessRoots.contains root && e.form != 1) then coverageError "required root form"
      required := required.push id
  if targetEligibility[0]! == 0 then coverageError "no target-owned eligible constants"
  pure ⟨target, modules, entries, index, forms, eligibility, targetEligibility, nameBytes, required⟩

def emitInventory (out : Output) (inv : Inventory) : MachineM Unit := do
  send out (frame 1 #[jn inv.modules.size, natArray inv.forms, natArray inv.eligibility,
    natArray inv.targetEligibility, jn inv.nameBytes])
  for i in [:inv.modules.size] do
    let m := inv.modules[i]!
    let (name, _) ← encoded m.name
    send out (frame 2 #[jn i, name, .str m.path])
  for i in [:inv.entries.size] do
    let e := inv.entries[i]!
    let (name, _) ← encoded e.name
    send out (frame 3 #[jn i, name, jn e.moduleId, jn e.form, jn e.skip, natArray e.owners])
  send out (frame 4)

structure ReplayContext where
  inventory : Inventory
  output : Output
structure ReplayState where
  remaining : Array Bool
  pending : Array Bool
  covered : Array Bool
  ctorPending : Array Bool
  recPending : Array Bool
  remainingCount : Nat
  pendingCount : Nat := 0
  ctorPendingCount : Nat := 0
  recPendingCount : Nat := 0
  additions : Nat := 0
  coveredCount : Nat := 0
  ctorCompares : Nat := 0
  recCompares : Nat := 0
  targetCovered : Nat := 0
  eventSeq : Nat := 0
abbrev ReplayM := ReaderT ReplayContext (StateRefT ReplayState MachineM)

private def entryAt (id : Nat) : ReplayM Entry := do
  let some e := (← read).inventory.entries[id]? | coverageError "unknown inventory ordinal"
  pure e
private def lookupId (name : Name) : ReplayM Nat := do
  let some id := (← read).inventory.index[name]? | coverageError "dependency absent from inventory"
  pure id
private def clearWork (ids : Array Nat) : ReplayM Unit := do
  for id in ids do
    let _ ← entryAt id
    let s ← get
    let r := s.remaining[id]!
    let p := s.pending[id]!
    if (r && s.remainingCount == 0) || (p && s.pendingCount == 0) then
      coverageError "work counter underflow"
    modify fun s => {s with
      remaining := s.remaining.set! id false
      pending := s.pending.set! id false
      remainingCount := s.remainingCount - (if r then 1 else 0)
      pendingCount := s.pendingCount - (if p then 1 else 0)}

private def recordCoverage (action : Nat) (ids : Array Nat) : ReplayM Unit := do
  if ids.isEmpty || ids.size > 64 then coverageError "coverage group size"
  for i in [1:ids.size] do
    if ids[i-1]! >= ids[i]! then coverageError "coverage group order"
  let inv := (← read).inventory
  for id in ids do
    let e ← entryAt id
    if e.skip != 0 || (← get).covered[id]! then coverageError "ineligible or duplicate coverage"
    match action with
    | 0 => if e.form > 3 || ids.size != 1 then coverageError "scalar coverage form"
    | 1 => if e.form != 4 || e.owners != ids then coverageError "inductive coverage group"
    | 2 => if e.form != 7 || e.owners != ids || ids.size != 4 then coverageError "quotient coverage group"
    | 3 | 4 =>
      if ids.size != 1 || e.form != (if action == 3 then 5 else 6) then
        coverageError "postponed coverage form"
      for owner in e.owners do
        if !(← get).covered[owner]! then coverageError "postponed owner not covered"
    | _ => coverageError "unknown coverage action"
  let sequence := (← get).eventSeq
  for id in ids do
    let e := inv.entries[id]!
    let target := inv.modules[e.moduleId]!.name == targetName inv.target
    modify fun s => {s with
      covered := s.covered.set! id true
      coveredCount := s.coveredCount + 1
      targetCovered := s.targetCovered + (if target then 1 else 0)}
  modify fun s => {s with
    eventSeq := s.eventSeq + 1
    additions := s.additions + (if action <= 2 then 1 else 0)
    ctorCompares := s.ctorCompares + (if action == 3 then 1 else 0)
    recCompares := s.recCompares + (if action == 4 then 1 else 0)}
  send (← read).output (frame 5 #[jn sequence, jn action, natArray ids])

private def checkComplete : ReplayM Unit := do
  let inv := (← read).inventory
  let s ← get
  if s.remainingCount != 0 || s.pendingCount != 0 || s.ctorPendingCount != 0 ||
      s.recPendingCount != 0 then coverageError "unresolved replay work"
  if s.coveredCount != inv.eligibility[0]! || s.targetCovered != inv.targetEligibility[0]! then
    coverageError "incomplete eligible coverage"
  let mut ctorExpected := 0
  let mut recExpected := 0
  for id in [:inv.entries.size] do
    let e := inv.entries[id]!
    if s.remaining[id]! || s.pending[id]! || s.ctorPending[id]! || s.recPending[id]! then
      coverageError "nonempty replay work bitset"
    if s.covered[id]! != (e.skip == 0) then coverageError "coverage eligibility bitset mismatch"
    if e.skip == 0 && e.form == 5 then ctorExpected := ctorExpected + 1
    if e.skip == 0 && e.form == 6 then recExpected := recExpected + 1
  if s.ctorCompares != ctorExpected || s.recCompares != recExpected then
    coverageError "incomplete postponed comparisons"
  for id in inv.required do
    if !s.covered[id]! then coverageError "required root uncovered"

private def nativeChecker : IO System.FilePath := do
  let path := ((← IO.appPath).parent.getD ".") / "../rust/release/logos-kernel"
  pure <| if System.Platform.isWindows then path.withExtension "exe" else path

private def exportClosure (env : Lean.Environment) (inv : Inventory)
    (handle : IO.FS.Handle) : IO NameHashSet :=
  IO.withStdout (IO.FS.Stream.ofHandle handle) <| _root_.M.run env do
    _root_.initState env
    _root_.dumpMetadata
    for entry in inv.entries do
      if entry.skip == 0 then _root_.dumpConstant entry.name
    pure (← get).visitedConstants

private def runLogos (env : Lean.Environment) (inv : Inventory) : MachineM Unit := do
  IO.FS.withTempFile (m := MachineM) fun exportHandle exportPath => do
    let exported ← exportClosure env inv exportHandle
    exportHandle.flush
    if (← exportPath.metadata).byteSize > 2147483648 then limit "export byte limit"
    if exported.size != inv.eligibility[0]! then coverageError "exported declaration count changed"
    let mut expected : Array Json := #[]
    for entry in inv.entries do
      if exported.contains entry.name != (entry.skip == 0) then
        coverageError "exporter omitted or added an eligible declaration"
      if entry.skip == 0 then
        let (name, _) ← encoded entry.name
        expected := expected.push (.arr #[name, jn entry.form])
    IO.FS.withTempFile (m := MachineM) fun manifestHandle manifestPath => do
      let bytes := (Json.arr expected).compress.toUTF8
      if bytes.size > 67108864 then limit "export inventory byte limit"
      manifestHandle.write bytes
      manifestHandle.flush
      let result ← IO.Process.output {
        cmd := (← nativeChecker).toString
        args := #["--check-manifest", exportPath.toString, manifestPath.toString] }
      if result.exitCode != 0 then fail "checker" (result.stderr.take 4096).toString
      if result.stdout.utf8ByteSize > 65536 || result.stderr.utf8ByteSize > 65536 then
        limit "Logos result byte limit"
      let summary ← match Json.parse result.stdout with
        | .ok summary => pure summary | .error _ => fail "checker" "invalid Logos result"
      let checked ← match summary.getObjValAs? Nat "checked" with
        | .ok count => pure count | .error _ => fail "checker" "missing Logos checked count"
      unless checked == inv.eligibility[0]! do coverageError "Logos environment coverage changed"

private def runReplay (out : Output) (env : Lean.Environment) (inv : Inventory) : MachineM ReplayState := do
  -- No successful coverage is emitted before both the Logos verdict and the
  -- exact declaration-name/kind association have passed in its checked environment.
  runLogos env inv
  for id in [:inv.entries.size] do
    let e := inv.entries[id]!
    if e.skip != 0 then send out (frame 6 #[jn id, jn e.skip])
  let flags := Array.replicate inv.entries.size false
  let initial : ReplayState := {
    remaining := inv.entries.map (fun e => e.skip == 0)
    pending := flags, covered := flags, ctorPending := flags, recPending := flags
    remainingCount := inv.eligibility[0]! }
  let (_, state) ← StateRefT'.run (s := initial) do
    ReaderT.run (r := {inventory := inv, output := out}) do
      for id in [:inv.entries.size] do
        let entry := inv.entries[id]!
        if entry.skip == 0 && !(← get).covered[id]! then
          if entry.form ≤ 3 then
            clearWork #[id]
            recordCoverage 0 #[id]
          else if entry.form == 4 || entry.form == 7 then
            clearWork entry.owners
            recordCoverage (if entry.form == 4 then 1 else 2) entry.owners
      -- Logos reconstructs and validates these headers/rules as part of each
      -- inductive block. Record their checked metadata only after their owners.
      for id in [:inv.entries.size] do
        let entry := inv.entries[id]!
        if entry.skip == 0 && (entry.form == 5 || entry.form == 6) then
          clearWork #[id]
          recordCoverage (if entry.form == 5 then 3 else 4) #[id]
      checkComplete
  pure state

structure Arguments where
  mode : Nat
  target : Nat
  token : String

private def validToken (token : String) : Bool :=
  if token.utf8ByteSize != 32 then false else
    token.toList.all fun c => ('0' <= c && c <= '9') || ('a' <= c && c <= 'f')

def parseArguments : List String → Except Failure Arguments
  | ["--vesper42", mode, target, token] => do
    let mode ← match mode with
      | "0" => pure 0 | "1" => pure 1 | "2" => pure 2 | "3" => pure 3
      | _ => throw ⟨"io", "invalid machine mode"⟩
    let target ← match target with
      | "0" => pure 0 | "1" => pure 1 | "2" => pure 2 | "3" => pure 3
      | _ => throw ⟨"io", "invalid machine target"⟩
    if !validToken token then throw ⟨"io", "invalid machine token"⟩
    if (mode == 1 && target == 2) || (mode >= 2 && target != 2) then
      throw ⟨"io", "invalid mode/target pair"⟩
    pure ⟨mode, target, token⟩
  | _ => .error ⟨"io", "expected exactly --vesper42 MODE TARGET TOKEN"⟩

private def done (out : Output) (args : Arguments)
    (additions covered ctorCompares recCompares unsafeOnly partialOnly both targetCovered
      remaining pending ctorPending recPending controls matchedControls : Nat := 0) : MachineM Unit :=
  send out (frame 9 #[.str args.token, jn args.mode, jn args.target,
    jn additions, jn covered, jn ctorCompares, jn recCompares,
    jn unsafeOnly, jn partialOnly, jn both, jn targetCovered,
    jn remaining, jn pending, jn ctorPending, jn recPending,
    jn controls, jn matchedControls])

private def printableBounded (s : String) : Bool :=
  if s.isEmpty || s.utf8ByteSize > 128 then false
  else s.toList.all fun c => c.toNat >= 32 && c.toNat <= 126

private def checkControls (out : Output) (args : Arguments) (env : Lean.Environment) : MachineM Unit := do
  let count := env.constants.fold (fun n _ _ => n + 1) 0
  if count == 0 || count > hardConstants then limit "control baseline constant count"
  let some ci := env.find? controlRoot | coverageError "control root absent"
  let some origin := env.getModuleIdxFor? controlRoot | coverageError "control root origin absent"
  let some module := env.header.modules[origin.toNat]? | coverageError "control root origin index"
  if module.module != targetName 2 || skipOf ci != 0 || formOf ci != 1 then
    coverageError "control root binding mismatch"
  let bin := (← IO.appPath).parent.getD "."
  let result ← IO.Process.output {
    cmd := (← nativeChecker).toString
    args := #["--controls", (bin / "../../../controls").toString] }
  if result.exitCode != 0 then fail "controls" (result.stderr.take 4096).toString
  if result.stdout.utf8ByteSize > 65536 || result.stderr.utf8ByteSize > 65536 then
    limit "control output byte limit"
  let rows ← match Json.parse result.stdout >>= Json.getArr? with
    | .ok rows => pure rows | .error _ => fail "controls" "invalid Logos control rows"
  if rows.isEmpty || rows.size > 256 then limit "control count"
  send out (frame 7 #[jn rows.size])
  let mut seen : Std.HashSet String := {}
  let mut matched := 0
  for i in [:rows.size] do
    let r := rows[i]!
    let getString (field : String) : MachineM String :=
      match r.getObjValAs? String field with
      | .ok value => pure value | .error _ => fail "controls" "missing control string"
    let getBool (field : String) : MachineM Bool :=
      match r.getObjValAs? Bool field with
      | .ok value => pure value | .error _ => fail "controls" "missing control boolean"
    let id ← getString "id"
    let expectedAccept ← getBool "expected_accept"
    let actualAccept ← getBool "actual_accept"
    let expectedReason ← getString "expected_reason"
    let actualReason ← getString "actual_reason"
    let claimedMatch ← getBool "matched"
    if !printableBounded id || !printableBounded expectedReason || !printableBounded actualReason then
      limit "control ID or reason limit"
    if seen.contains id then fail "controls" "duplicate control ID"
    seen := seen.insert id
    let actualMatched := expectedAccept == actualAccept && expectedReason == actualReason
    if claimedMatch != actualMatched then fail "controls" "incorrect returned control match"
    send out (frame 8 #[jn i, .str id, jb expectedAccept, jb actualAccept,
      .str expectedReason, .str actualReason, jb actualMatched])
    if actualMatched then matched := matched + 1
  if matched != rows.size then fail "controls" "fixed control refusal or mismatch"
  done out args (controls := rows.size) (matchedControls := matched)

private def cutDetail (s : String) : String := Id.run do
  if s.utf8ByteSize <= 4096 then return s
  let bytes := s.toUTF8
  let mut n := 4096
  for _ in [:4] do
    if let some beginning := String.fromUTF8? (bytes.extract 0 n) then return beginning
    n := n - 1
  return "diagnostic exceeded limit"

private unsafe def withClosure (target : Nat) (action : Lean.Environment → MachineM Unit) : MachineM Unit :=
  ExceptT.mk do
    try
      Lean.withImportModules #[{module := targetName target, importAll := true}] {}
        (trustLevel := 0) fun env => (action env).run
    catch _ => pure (.error ⟨"import", "native target import failed"⟩)

private unsafe def machine (out : Output) (args : Arguments) : MachineM Unit := do
  if Lean.versionString != "4.35.0" then fail "internal" "unsupported Lean version"
  send out (frame 0 #[.str args.token, jn args.mode, jn args.target,
    .str "4.35.0", .str "vesper.logos42/2"])
  let input ← IO.getStdin
  let unexpected ← input.read 1
  if !unexpected.isEmpty then fail "io" "machine mode accepts no stdin language"
  if args.mode == 3 then done out args
  else if args.mode == 2 then withClosure 2 (checkControls out args)
  else withClosure args.target fun env => do
    let inv ← buildInventory env args.target
    emitInventory out inv
    if args.mode == 0 then
      done out args (unsafeOnly := inv.eligibility[1]!)
        (partialOnly := inv.eligibility[2]!) (both := inv.eligibility[3]!)
    else
      let s ← runReplay out env inv
      done out args (additions := s.additions) (covered := s.coveredCount)
        (ctorCompares := s.ctorCompares) (recCompares := s.recCompares)
        (unsafeOnly := inv.eligibility[1]!) (partialOnly := inv.eligibility[2]!)
        (both := inv.eligibility[3]!) (targetCovered := s.targetCovered)
        (remaining := s.remainingCount) (pending := s.pendingCount)
        (ctorPending := s.ctorPendingCount) (recPending := s.recPendingCount)

unsafe def run (argv : List String) : IO UInt32 := do
  let handle ← IO.getStdout
  let bytes ← IO.mkRef 0
  let out : Output := ⟨handle, bytes⟩
  let result ← try
    match parseArguments argv with
    | .error e => pure (.error e)
    | .ok args =>
      initSearchPath (← findSysroot)
      (machine out args).run
  catch _ => pure (.error ⟨"io", "machine IO failure"⟩)
  match result with
  | .ok _ => pure 0
  | .error error =>
    let _ ← (send out (frame 10 #[.str error.category, .str (cutDetail error.detail)])).run
    pure 1

end Vesper.Logos42Machine

unsafe def main (args : List String) : IO UInt32 := Vesper.Logos42Machine.run args
