import SimdScanner

/-!
Stdin driver for the executable Lean model (not audited; contains `partial` IO).

Build and run (the Rust test `crates/vesper-simd/tests/model.rs` does this):

```sh
lean -o "$out/SimdScanner.olean" lean/SimdScanner.lean
LEAN_PATH="$out:$prefix/lib/lean" lean --run lean/SimdScanner/Fixture.lean < cases
```

Each input line is `base delim hexbytes` (decimal base address and delimiter,
lowercase hex slice bytes; an empty slice omits the third field). Each output
line is `count xor reads in_bounds`, computed by the modeled machine program
`scan`, after one header naming the model and its claim boundary.
-/

namespace Vesper.SimdScanner.Fixture
open Vesper.SimdScanner

def hexNibble (c : Char) : Option Nat :=
  if '0' ≤ c ∧ c ≤ '9' then some (c.toNat - '0'.toNat)
  else if 'a' ≤ c ∧ c ≤ 'f' then some (c.toNat - 'a'.toNat + 10)
  else none

def parseHex : List Char → Option (List Byte)
  | [] => some []
  | a :: b :: rest => do
    let hi ← hexNibble a
    let lo ← hexNibble b
    let tl ← parseHex rest
    pure (BitVec.ofNat 8 (hi * 16 + lo) :: tl)
  | [_] => none

partial def loop (stdin : IO.FS.Stream) (stdout : IO.FS.Stream) : IO Unit := do
  let line ← stdin.getLine
  if line.isEmpty then return
  let fields := line.trimRight.splitOn " "
  -- An empty slice is sent as `base delim` with no third field.
  let fields := if fields.length == 2 then fields ++ [""] else fields
  match fields with
  | [base, delim, hex] =>
    match base.toNat?, delim.toNat?, parseHex hex.toList with
    | some b, some dv, some bytes =>
      if dv < 256 then
        match runFixture (BitVec.ofNat 64 b) (BitVec.ofNat 8 dv) bytes.toArray with
        | some o =>
          let inBounds := o.reads.all fun r =>
            let (lo, hi) := r.range
            decide (b ≤ lo ∧ hi ≤ b + bytes.length)
          stdout.putStrLn s!"{o.count.toNat} {o.xor.toNat} {o.reads.length} {inBounds}"
        | none => stdout.putStrLn "error fuel"
      else stdout.putStrLn "error delimiter"
    | _, _, _ => stdout.putStrLn "error parse"
  | _ => stdout.putStrLn "error fields"
  stdout.flush
  loop stdin stdout

end Vesper.SimdScanner.Fixture

def main : IO Unit := do
  let stdout ← IO.getStdout
  stdout.putStrLn s!"{Vesper.SimdScanner.modelId} | {Vesper.SimdScanner.modelClaim}"
  stdout.flush
  Vesper.SimdScanner.Fixture.loop (← IO.getStdin) stdout

