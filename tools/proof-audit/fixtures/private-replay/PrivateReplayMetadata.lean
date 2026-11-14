import PrivateReplayBoth
import Lean

open Lean Elab Command

run_cmd do
  let env ← getEnv
  let modules := [`PrivateReplayLeft, `PrivateReplayRight, `PrivateReplayBoth]
  let mut records : Array ConstantInfo := #[]
  let mut moduleReports : Array Json := #[]
  for (moduleName, data) in env.header.moduleNames.zip env.header.moduleData do
    if modules.contains moduleName then
      moduleReports := moduleReports.push <| Json.mkObj [
        ("name", toJson moduleName.toString), ("declaration_count", toJson data.constants.size)]
      records := records ++ data.constants
  unless records.size == 4 do
    throwError "unexpected actual fixture declaration count {records.size}"
  let privateRecords := records.filter fun info =>
    isPrivateName info.name && privateToUserName info.name == `ProofAuditPrivateReplay.shared
  unless privateRecords.size == 2 do
    throwError "expected two actual private shared declarations"
  unless privateRecords[0]!.name != privateRecords[1]!.name do
    throwError "private full names were coalesced"
  let some (.thmInfo left) := env.find? `ProofAuditPrivateReplay.left | throwError "missing left"
  let some (.thmInfo right) := env.find? `ProofAuditPrivateReplay.right | throwError "missing right"
  let leftDeps := left.value.getUsedConstants.filter fun name =>
    privateRecords.any fun info => info.name == name
  let rightDeps := right.value.getUsedConstants.filter fun name =>
    privateRecords.any fun info => info.name == name
  unless leftDeps.size == 1 && rightDeps.size == 1 do
    throwError "public root not bound to exactly one private proof"
  unless leftDeps[0]! != rightDeps[0]! do
    throwError "public roots refer to same private proof"
  let privateReports := privateRecords.map fun info => Json.mkObj [
    ("full_name", toJson info.name.toString),
    ("user_name", toJson (privateToUserName info.name).toString),
    ("unsafe", toJson info.isUnsafe), ("partial", toJson info.isPartial)]
  let rootReports := #[
    Json.mkObj [("name", toJson "ProofAuditPrivateReplay.left"),
      ("private_dependency", toJson leftDeps[0]!.toString)],
    Json.mkObj [("name", toJson "ProofAuditPrivateReplay.right"),
      ("private_dependency", toJson rightDeps[0]!.toString)]]
  let report := Json.mkObj [
    ("schema_version", toJson (1 : Nat)), ("declaration_count", toJson records.size),
    ("modules", Json.arr moduleReports), ("private_declarations", Json.arr privateReports),
    ("public_roots", Json.arr rootReports)]
  liftIO <| IO.println ("VESPER_PRIVATE_REPLAY_METADATA_V1:" ++ report.compress)

-- This probe is executed from a separate source directory. It is never an
-- audited artifact module, and it does not add an olean to the fixture root.
def main : IO Unit := pure ()
