import ChronoReplay

/-!
Stdin driver for the lineage log model (not audited; contains `partial` IO).

Build and run (the Rust test `crates/vesper-chrono/tests/lean_model.rs` does this):

```sh
lean -o "$out/ChronoReplay.olean" lean/ChronoReplay.lean
LEAN_PATH="$out:$prefix/lib/lean" lean --run lean/ChronoReplay/Fixture.lean < cases
```

After a header naming the model and its claim boundary, each input line is a
hop log, hops separated by spaces, each `kind:id` with kind one of `mint`,
`move`, `proj`, `handoff`, `discharge`, `leak`. The output line is `ok` with
the sorted ids that reached a terminal hop, `reject`, or `unknown` for a line
that does not parse.
-/

namespace Vesper.ChronoReplay.Fixture
open Vesper.ChronoReplay

def hopOfToken (tok : String) : Option Hop :=
  match tok.splitOn ":" with
  | [kind, id] =>
    match id.toNat? with
    | none => none
    | some i =>
      match kind with
      | "mint" => some (.mint i)
      | "move" => some (.move i)
      | "proj" => some (.proj i)
      | "handoff" => some (.handoff i)
      | "discharge" => some (.discharge i)
      | "leak" => some (.leak i)
      | _ => none
  | _ => none

def insertSorted (x : Nat) : List Nat → List Nat
  | [] => [x]
  | y :: ys => if x ≤ y then x :: y :: ys else y :: insertSorted x ys

def render (st : St) : String :=
  let done := st.done.foldl (fun acc x => insertSorted x acc) []
  String.intercalate " " ("ok" :: done.map toString)

def lineOut (line : String) : String :=
  let toks := (line.splitOn " ").filter (· ≠ "")
  match toks.mapM hopOfToken with
  | none => "unknown"
  | some log =>
    match checkLog ⟨[], []⟩ log with
    | some st => render st
    | none => "reject"

partial def loop (stdin : IO.FS.Stream) (stdout : IO.FS.Stream) : IO Unit := do
  let line ← stdin.getLine
  if line.isEmpty then return
  stdout.putStrLn (lineOut line.trimRight)
  loop stdin stdout

end Vesper.ChronoReplay.Fixture

def main : IO Unit := do
  let stdout ← IO.getStdout
  stdout.putStrLn s!"{Vesper.ChronoReplay.modelId} | {Vesper.ChronoReplay.modelClaim}"
  Vesper.ChronoReplay.Fixture.loop (← IO.getStdin) stdout
