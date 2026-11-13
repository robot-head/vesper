import BeaconRedaction

/-!
Stdin driver for the attribute export model (not audited; contains `partial` IO).

Build and run (the Rust test `crates/vesper-beacon-arrow/tests/lean_model.rs`
does this):

```sh
lean -o "$out/BeaconRedaction.olean" lean/BeaconRedaction.lean
LEAN_PATH="$out:$prefix/lib/lean" lean --run lean/BeaconRedaction/Fixture.lean < cases
```

After a header naming the model and its claim boundary, each input line is
one span's attributes separated by spaces, each `pub:key:value` or
`pii:key:value` (PII under the `redact` policy). The output line lists the
exported attributes as `key=value`, or `unknown` for a line that does not
parse.
-/

namespace Vesper.BeaconRedaction.Fixture
open Vesper.BeaconRedaction

def attrOfToken (tok : String) : Option Attr :=
  match tok.splitOn ":" with
  | ["pub", k, v] => some ⟨k, .pub, v⟩
  | ["pii", k, v] => some ⟨k, .pii .redact, v⟩
  | _ => none

def render : String × Out → String
  | (k, .plain v) => k ++ "=" ++ v
  | (k, .redacted) => k ++ "=" ++ marker
  | (k, .tag h) => k ++ "=#" ++ toString h

def runLine (line : String) : String :=
  let toks := (line.splitOn " ").filter (· ≠ "")
  match toks.mapM attrOfToken with
  | none => "unknown"
  | some attrs => " ".intercalate (attrs.map (render ∘ exportAttr (fun _ => 0)))

partial def loop (stdin : IO.FS.Stream) (stdout : IO.FS.Stream) : IO Unit := do
  let line ← stdin.getLine
  if line.isEmpty then return
  stdout.putStrLn (runLine (line.trimRight))
  loop stdin stdout

def main : IO Unit := do
  let stdout ← IO.getStdout
  stdout.putStrLn (modelId ++ " | " ++ modelClaim)
  loop (← IO.getStdin) stdout

end Vesper.BeaconRedaction.Fixture

def main : IO Unit := Vesper.BeaconRedaction.Fixture.main
