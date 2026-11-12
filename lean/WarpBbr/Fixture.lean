import WarpBbr

/-!
Stdin driver for the executable Lean model (not audited; contains `partial` IO).

Build and run (the Rust test `crates/vesper-warp-bbr/tests/lean_model.rs`
does this):

```sh
(cd lean && lean -o "$out/WarpBbr.olean" WarpBbr.lean)
LEAN_PATH="$out" lean --run lean/WarpBbr/Fixture.lean < cases
```

Each input line is space-separated decimal naturals: a case tag and its
arguments, `0 bytes rate` (pacing delay), `1 prev now delay` (EDT step),
`2 cwnd mss hi` (target cwnd) or `3 inflight cwnd` (send guard). Each output
line is the result, after one header naming the model and its claim boundary.
An unparseable line prints `error`.
-/

namespace Vesper.WarpBbr.Fixture
open Vesper.WarpBbr

partial def loop (stdin : IO.FS.Stream) (stdout : IO.FS.Stream) : IO Unit := do
  let line ← stdin.getLine
  if line.isEmpty then return
  let fields := (line.trimRight.splitOn " ").map String.toNat?
  let out :=
    if fields.all Option.isSome then
      (runCase (fields.filterMap id)).getD "error"
    else "error"
  stdout.putStrLn out
  loop stdin stdout

end Vesper.WarpBbr.Fixture

def main : IO Unit := do
  let stdout ← IO.getStdout
  stdout.putStrLn s!"{Vesper.WarpBbr.modelId} | {Vesper.WarpBbr.modelClaim}"
  Vesper.WarpBbr.Fixture.loop (← IO.getStdin) stdout
