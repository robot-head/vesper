import Lean4Lean.VesperControls

/-! Focused execution wrapper. It invokes the same fixed IO control suite used
by the separate machine entrypoint; it does not implement another checker. -/
open Lean

unsafe def main : IO Unit := do
  initSearchPath (← findSysroot)
  withImportModules #[{module := `Lean4Lean.VesperControls, importAll := true}] {}
      (trustLevel := 0) fun env => do
    let rows ← Vesper.Lean4Lean42Controls.runControls env.toKernelEnv
    for row in rows do
      IO.println s!"CONTROL {if row.matched then "PASS" else "FAIL"} {row.id} expected={row.expectedAccept}:{row.expectedReason} actual={row.actualAccept}:{row.actualReason}"
    if rows.isEmpty || rows.any (fun row => !row.matched) then
      throw (IO.userError "raw kernel controls failed")
    IO.println s!"CONTROLS PASSED {rows.size}"
