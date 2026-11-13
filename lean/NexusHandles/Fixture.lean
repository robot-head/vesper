import NexusHandles

/-!
Stdin driver for the executable table model (not audited; contains `partial` IO).

Build and run (the Rust test `crates/vesper-nexus/tests/lean_model.rs` does this):

```sh
lean -o "$out/NexusHandles.olean" lean/NexusHandles.lean
LEAN_PATH="$out:$prefix/lib/lean" lean --run lean/NexusHandles/Fixture.lean < cases
```

After a header naming the model and its claim boundary, each input line
produces one output line:
* `init CAP MAXGEN` resets to `Table.init` and prints `ok`;
* `insert T V`, `lookup T IDX GEN`, `borrow T IDX GEN`, `unborrow T IDX GEN`,
  `drop T IDX GEN`, `transfer T IDX GEN TO`, `reclaim T` apply `step` and
  print `Out.render` (`ok IDX GEN`, `ok V`, `ok`, `ok N` or `err NAME`);
* anything else prints `unknown`.
-/

namespace Vesper.NexusHandles.Fixture
open Vesper.NexusHandles

def parseOp : List String → Option Op
  | ["insert", t, v] => do pure (.insert (← t.toNat?) (← v.toNat?))
  | ["lookup", t, i, g] => do pure (.lookup (← t.toNat?) ⟨← i.toNat?, ← g.toNat?⟩)
  | ["borrow", t, i, g] => do pure (.borrow (← t.toNat?) ⟨← i.toNat?, ← g.toNat?⟩)
  | ["unborrow", t, i, g] => do pure (.unborrow (← t.toNat?) ⟨← i.toNat?, ← g.toNat?⟩)
  | ["drop", t, i, g] => do pure (.drop (← t.toNat?) ⟨← i.toNat?, ← g.toNat?⟩)
  | ["transfer", t, i, g, to] =>
    do pure (.transfer (← t.toNat?) ⟨← i.toNat?, ← g.toNat?⟩ (← to.toNat?))
  | ["reclaim", t] => do pure (.reclaim (← t.toNat?))
  | _ => none

def stepLine (T : Table) (line : String) : Table × String :=
  match line.splitOn " " with
  | ["init", c, g] =>
    match c.toNat?, g.toNat? with
    | some c, some g => (Table.init c g, "ok")
    | _, _ => (T, "unknown")
  | ws =>
    match parseOp ws with
    | some o => let (T', out) := step T o; (T', out.render)
    | none => (T, "unknown")

partial def loop (stdin : IO.FS.Stream) (stdout : IO.FS.Stream) (T : Table) : IO Unit := do
  let line ← stdin.getLine
  if line.isEmpty then return
  let (T', out) := stepLine T line.trimRight
  stdout.putStrLn out
  loop stdin stdout T'

def main : IO Unit := do
  let stdout ← IO.getStdout
  stdout.putStrLn s!"{modelId} | {modelClaim}"
  loop (← IO.getStdin) stdout (Table.init 0 1)

end Vesper.NexusHandles.Fixture

def main : IO Unit := Vesper.NexusHandles.Fixture.main
