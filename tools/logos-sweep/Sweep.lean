/-
Exports the eligible declarations of the joint import closure of the given
modules with the lean4export library pinned by `tools/logos42`, writes the
exact name/kind inventory that the sealed `logos-kernel` bridge compares with
Logos' checked environment, and summarizes per-module coverage.

This program is part of the trusted translation boundary (with the exporter);
it decides nothing itself. Acceptance comes only from Logos and the bridge's
exact coverage check.
-/
import Lean
import Export

namespace Vesper.LogosSweep
open Lean

private def formOf : ConstantInfo → Nat
  | .axiomInfo _ => 0 | .defnInfo _ => 1 | .thmInfo _ => 2 | .opaqueInfo _ => 3
  | .inductInfo _ => 4 | .ctorInfo _ => 5 | .recInfo _ => 6 | .quotInfo _ => 7

private def eligible (ci : ConstantInfo) : Bool := !ci.isUnsafe && !ci.isPartial

/-- Root-first name components, matching the bridge's `name` decoder. -/
private def encodeName (name : Name) : Json :=
  let rec go : Name → List Json → List Json
    | .anonymous, acc => acc
    | .str p s, acc => go p (.arr #[toJson (0 : Nat), .str s] :: acc)
    | .num p n, acc => go p (.arr #[toJson (1 : Nat), .str (toString n)] :: acc)
  .arr (go name []).toArray

private def moduleOf (env : Environment) (name : Name) : Name :=
  match env.getModuleIdxFor? name with
  | some idx => env.header.moduleNames[idx.toNat]?.getD .anonymous
  | none => .anonymous

/-- `excluded` plus every eligible declaration that transitively uses one. -/
private def withDependents (all : Array (Name × ConstantInfo)) (excluded : NameHashSet) :
    NameHashSet := Id.run do
  let mut users : Std.HashMap Name (Array Name) := {}
  for (n, ci) in all do
    for d in ci.getUsedConstantsAsSet do
      users := users.insert d ((users.getD d #[]).push n)
  let mut out := excluded
  let mut stack := excluded.toArray
  while h : stack.size > 0 do
    let x := stack.back
    stack := stack.pop
    for u in users.getD x #[] do
      unless out.contains u do
        out := out.insert u
        stack := stack.push u
  out

unsafe def run (exportPath manifestPath summaryPath excludePath : String)
    (modules : List String) : IO UInt32 := do
  if Lean.versionString != "4.35.0" then
    throw <| IO.userError s!"unsupported Lean version {Lean.versionString}"
  initSearchPath (← findSysroot)
  let roots := modules.toArray.map String.toName
  let imports := roots.map fun m => ({ module := m, importAll := true } : Import)
  Lean.withImportModules imports {} (trustLevel := 0) fun env => do
    -- Deterministic order: sorted by name.
    let all := env.constants.fold (fun acc n ci => acc.push (n, ci)) #[]
    let all := all.qsort (fun a b => Name.cmp a.1 b.1 == .lt)
    -- Declarations the caller withholds (Logos declined them), and their users.
    let requested : Array Name ← if excludePath == "-" then pure #[] else do
      match Json.parse (← IO.FS.readFile excludePath) >>= fromJson? (α := Array String) with
      | .ok names => pure (names.map String.toName)
      | .error e => throw <| IO.userError s!"bad exclusion list: {e}"
    for n in requested do
      unless env.contains n do throw <| IO.userError s!"excluded {n} is not in the closure"
    let withheld := withDependents all (requested.foldl (·.insert ·) {})
    let chosen := all.filter fun (n, ci) => eligible ci && !withheld.contains n
    let handle ← IO.FS.Handle.mk exportPath .write
    let visited ← IO.withStdout (IO.FS.Stream.ofHandle handle) <| _root_.M.run env do
      _root_.initState env
      _root_.dumpMetadata
      for (n, _) in chosen do _root_.dumpConstant n
      pure (← get).visitedConstants
    handle.flush
    -- The exporter must emit exactly the eligible set: no skipped declaration
    -- may be pulled in as a dependency, and none may be dropped.
    if visited.size != chosen.size then
      throw <| IO.userError s!"exporter visited {visited.size} declarations, expected {chosen.size}"
    for (n, _) in chosen do
      unless visited.contains n do
        throw <| IO.userError s!"exporter omitted {n}"
    let manifest := Json.arr <| chosen.map fun (n, ci) => .arr #[encodeName n, toJson (formOf ci)]
    IO.FS.writeFile manifestPath manifest.compress
    -- Per-module accounting for the report.
    let mut perModule : Std.HashMap Name (Nat × Array String × Array String) := {}
    for (n, ci) in all do
      let m := moduleOf env n
      let (c, skipped, held) := perModule.getD m (0, #[], #[])
      perModule := perModule.insert m <|
        if !eligible ci then (c, skipped.push n.toString, held)
        else if withheld.contains n then (c, skipped, held.push n.toString)
        else (c + 1, skipped, held)
    let rows := roots.map fun m =>
      let (c, skipped, held) := perModule.getD m (0, #[], #[])
      Json.mkObj [("module", .str m.toString), ("checked", toJson c),
        ("skipped_unsafe_or_partial", toJson skipped), ("withheld", toJson held)]
    let summary := Json.mkObj [
      ("lean", .str Lean.versionString),
      ("modules_in_closure", toJson env.header.moduleNames.size),
      ("constants", toJson all.size),
      ("exported", toJson chosen.size),
      ("skipped_unsafe_or_partial", toJson (all.filter (!eligible ·.2)).size),
      ("withheld", toJson (all.filter fun (n, ci) => eligible ci && withheld.contains n).size),
      ("roots", .arr rows)]
    IO.FS.writeFile summaryPath summary.pretty
  pure 0


/-- A conservative subset of the duplicates that Lean's importer merges: the
same realized theorem statement generated in two modules. A misjudged batch
only fails at import, never passes unchecked. -/
private def mergeable (a b : ConstantInfo) : Bool :=
  match a, b with
  | .thmInfo _, .thmInfo _ | .thmInfo _, .axiomInfo _ | .axiomInfo _, .thmInfo _ =>
    a.name == b.name && a.type == b.type && a.levelParams == b.levelParams
  | _, _ => false

/-- Greedy partition of root modules into batches whose joint import closures
declare every name once, so that each batch imports into one environment. -/
unsafe def plan (planPath : String) (modules : List String) : IO UInt32 := do
  initSearchPath (← findSysroot)
  let mut own : Std.HashMap Name (Array ConstantInfo) := {}
  let mut deps : Std.HashMap Name (Array Name) := {}
  -- Load the module graph of every root without importing it.
  let mut todo := modules.toArray.map String.toName
  while h : todo.size > 0 do
    let m := todo.back
    todo := todo.pop
    if own.contains m then continue
    let (data, _) ← readModuleData (← findOLean m)
    own := own.insert m data.constants
    let ds := data.imports.map (·.module)
    deps := deps.insert m ds
    todo := todo ++ ds.filter (!own.contains ·)
  let closure (m : Name) : Array Name := Id.run do
    let mut seen : NameHashSet := {}
    let mut out := #[]
    let mut stack := #[m]
    while h : stack.size > 0 do
      let x := stack.back
      stack := stack.pop
      if seen.contains x then continue
      seen := seen.insert x
      out := out.push x
      stack := stack ++ (deps.getD x #[])
    out
  -- Each batch: (roots, modules in closure, declared name -> module).
  let mut batches : Array (Array Name × NameHashSet × Std.HashMap Name ConstantInfo) := #[]
  for root in modules.map String.toName do
    let mods := closure root
    let mut placed := false
    for i in [:batches.size] do
      let (roots, inBatch, names) := batches[i]!
      let fresh := mods.filter (!inBatch.contains ·)
      let clash := fresh.any fun x => (own.getD x #[]).any fun ci =>
        match names[ci.name]? with
        | some prev => !mergeable prev ci
        | none => false
      if clash then continue
      let mut names := names
      let mut inBatch := inBatch
      for x in fresh do
        inBatch := inBatch.insert x
        for ci in own.getD x #[] do names := names.insert ci.name ci
      batches := batches.set! i (roots.push root, inBatch, names)
      placed := true
      break
    unless placed do
      let mut names : Std.HashMap Name ConstantInfo := {}
      let mut inBatch : NameHashSet := {}
      for x in mods do
        inBatch := inBatch.insert x
        -- Lean's importer already accepted this closure on its own.
        for ci in own.getD x #[] do names := names.insert ci.name ci
      batches := batches.push (#[root], inBatch, names)
  let json := Json.arr <| batches.map fun (roots, _, _) => .arr (roots.map (.str ·.toString))
  IO.FS.writeFile planPath json.compress
  pure 0

end Vesper.LogosSweep

unsafe def main : List String → IO UInt32
  | "--plan" :: planPath :: modules@(_ :: _) => Vesper.LogosSweep.plan planPath modules
  | exportPath :: manifestPath :: summaryPath :: excludePath :: modules@(_ :: _) =>
    Vesper.LogosSweep.run exportPath manifestPath summaryPath excludePath modules
  | _ => do
    IO.eprintln "usage: logos-sweep --plan PLAN MODULE... | logos-sweep EXPORT MANIFEST SUMMARY EXCLUDE|- MODULE..."
    pure 2
