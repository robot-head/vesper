import WarpUmem

/-!
Stdin driver for the executable count model (not audited; contains `partial` IO).

Build and run (the Rust test `crates/vesper-warp-umem/tests/lean_model.rs` does this):

```sh
lean -o "$out/WarpUmem.olean" lean/WarpUmem.lean
LEAN_PATH="$out:$prefix/lib/lean" lean --run lean/WarpUmem/Fixture.lean < cases
```

After a header naming the model and its claim boundary, each input line
produces one output line:
* `init N` resets to `Census.init N` and prints the counts;
* a transition name (`harvest`, `submit`, ...) applies it with `apply` and
  prints the counts, or `reject` if the source location is empty;
* `step O` applies `quicStep` for outcome `O` (`malformedDescriptor`,
  `headerSplitFailed`, `cidMiss`, `reply`), printing counts or `reject`;
* anything else prints `unknown`.
Counts are the eleven locations in `Location` order.
-/

namespace Vesper.WarpUmem.Fixture
open Vesper.WarpUmem

def outcomeOfName : String → Option RxOutcome
  | "malformedDescriptor" => some .malformedDescriptor
  | "headerSplitFailed" => some .headerSplitFailed
  | "cidMiss" => some .cidMiss
  | "reply" => some .reply
  | _ => none

def stepLine (c : Census) (line : String) : Census × String :=
  match line.splitOn " " with
  | ["init", n] =>
    match n.toNat? with
    | some n => let c' := Census.init n; (c', c'.render)
    | none => (c, "unknown")
  | ["step", o] =>
    match outcomeOfName o with
    | some o =>
      match quicStep c o with
      | some c' => (c', c'.render)
      | none => (c, "reject")
    | none => (c, "unknown")
  | [name] =>
    match Trans.ofName name with
    | some t =>
      match apply c t with
      | some c' => (c', c'.render)
      | none => (c, "reject")
    | none => (c, "unknown")
  | _ => (c, "unknown")

partial def loop (stdin : IO.FS.Stream) (stdout : IO.FS.Stream) (c : Census) : IO Unit := do
  let line ← stdin.getLine
  if line.isEmpty then return
  let (c', out) := stepLine c line.trimRight
  stdout.putStrLn out
  loop stdin stdout c'

end Vesper.WarpUmem.Fixture

def main : IO Unit := do
  let stdout ← IO.getStdout
  stdout.putStrLn s!"{Vesper.WarpUmem.modelId} | {Vesper.WarpUmem.modelClaim}"
  Vesper.WarpUmem.Fixture.loop (← IO.getStdin) stdout (Vesper.WarpUmem.Census.init 0)
