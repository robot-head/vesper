import CrucibleShrink

/-! Differential fixture: reads lines `a1,a2,...;b1,b2,...` (either side may
be empty) and prints `smallerB a b` for each, after a header line. The Rust
test compares the answers with `ShrinkKey::smaller_than`. -/

open CrucibleShrink

def parseList (s : String) : Option (List Nat) :=
  if s.isEmpty then some [] else (s.splitOn ",").mapM String.toNat?

partial def loop (stdin : IO.FS.Stream) : IO Unit := do
  let line ← stdin.getLine
  if line.isEmpty then return
  let line := line.trimRight
  match line.splitOn ";" with
  | [a, b] =>
    match parseList a, parseList b with
    | some xs, some ys => IO.println (if smallerB xs ys then "true" else "false")
    | _, _ => IO.println "parse-error"
  | _ => IO.println "parse-error"
  loop stdin

def main : IO Unit := do
  IO.println s!"{modelId} | {modelClaim}"
  loop (← IO.getStdin)
