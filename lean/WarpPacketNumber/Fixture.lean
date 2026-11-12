import WarpPacketNumber

/-!
Stdin driver for the executable Lean model (not audited; contains `partial` IO).

The Rust test `crates/vesper-warp/tests/lean_model.rs` compiles
`lean/WarpPacketNumber.lean` to an `.olean` and runs this file with
`LEAN_PATH` pointing at it. Each input line is `largest truncated nbits` in
decimal, where `largest` is `-` when no packet was received in the space. Each
output line is the decoded packet number in decimal, or `none`. The first
output line names the model and its claim boundary.
-/

namespace Vesper.WarpPacketNumber.Fixture
open Vesper.WarpPacketNumber

partial def loop (stdin : IO.FS.Stream) (stdout : IO.FS.Stream) : IO Unit := do
  let line ← stdin.getLine
  if line.isEmpty then return
  match line.trimRight.splitOn " " with
  | [largest, t, n] =>
    let largest? : Option (Option Nat) :=
      if largest == "-" then some none else largest.toNat?.map some
    match largest?, t.toNat?, n.toNat? with
    | some l, some tv, some nv =>
      match decodeLargest l tv nv with
      | some r => stdout.putStrLn s!"{r}"
      | none => stdout.putStrLn "none"
    | _, _, _ => stdout.putStrLn "error parse"
  | _ => stdout.putStrLn "error fields"
  stdout.flush
  loop stdin stdout

end Vesper.WarpPacketNumber.Fixture

def main : IO Unit := do
  let stdout ← IO.getStdout
  stdout.putStrLn s!"{Vesper.WarpPacketNumber.modelId} | {Vesper.WarpPacketNumber.modelClaim}"
  Vesper.WarpPacketNumber.Fixture.loop (← IO.getStdin) stdout
