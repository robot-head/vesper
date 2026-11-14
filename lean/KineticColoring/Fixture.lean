import KineticColoring

/-!
Stdin driver for the executable Lean model (not audited; contains `partial` IO).

Build and run (the Rust test `crates/vesper-kinetic/tests/lean_model.rs`
does this):

```sh
(cd lean && lean -o "$out/KineticColoring.olean" KineticColoring.lean)
LEAN_PATH="$out" lean --run lean/KineticColoring/Fixture.lean < cases
```

Each input line is space-separated decimal naturals: `0 x y z` prints
`interleave 10 x y z` and the three deinterleaved axes; `1 a1 b1 a2 b2 ...`
prints the colours `colorAll` assigns to the constraints `(a1, b1), (a2, b2),
...` in that order; `3 x y z` prints `interleave 21 x y z` (the LBVH's
63-bit code). After one header naming the model and its claim boundary,
each output line answers one input line. An unparseable line prints `error`.
-/

namespace Vesper.KineticColoring.Fixture
open Vesper.KineticColoring

def pairs : List Nat → Option (List Constraint)
  | [] => some []
  | a :: b :: rest => (pairs rest).map ((a, b) :: ·)
  | [_] => none

def runCase : List Nat → Option String
  | [0, x, y, z] =>
    let m := interleave 10 x y z
    some s!"{m} {deinterleaveX 10 m} {deinterleaveY 10 m} {deinterleaveZ 10 m}"
  | [3, x, y, z] => some s!"{interleave 21 x y z}"
  | 1 :: rest => (pairs rest).map fun cs =>
    " ".intercalate ((colorAll cs).map (fun x => toString x.2))
  | _ => none

partial def loop (stdin : IO.FS.Stream) (stdout : IO.FS.Stream) : IO Unit := do
  let line ← stdin.getLine
  if line.isEmpty then return
  let fields := (line.trimRight.splitOn " ").filter (· ≠ "") |>.map String.toNat?
  let out :=
    if fields.all Option.isSome then
      (runCase (fields.filterMap id)).getD "error"
    else "error"
  stdout.putStrLn out
  loop stdin stdout

end Vesper.KineticColoring.Fixture

def main : IO Unit := do
  let stdout ← IO.getStdout
  stdout.putStrLn s!"{Vesper.KineticColoring.modelId} | {Vesper.KineticColoring.modelClaim}"
  Vesper.KineticColoring.Fixture.loop (← IO.getStdin) stdout
