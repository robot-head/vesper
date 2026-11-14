import CrucibleDpor

/-! Differential fixture: each input line is a program, processes separated
by `;`, operations by `,`: `rC` reads cell `C` into the process's log, `wC`
writes `100 * (p + 1) + k + 1` (process `p`, operation `k`) to cell `C`.
For each program it runs the model's `explore` and prints the number of
explored executions and the final state of each, as
`count|cells/log:log:... cells/log:...`. The Rust test compares the set of
final states with Crucible's DPOR and exhaustive explorers. -/

open CrucibleDpor

inductive Op where
  | r (c : Nat)
  | w (c : Nat)

structure St where
  pcs : List Nat
  cells : List Nat
  logs : List (List Nat)

def opAt (prog : List (List Op)) (p k : Nat) : Option Op :=
  (prog[p]?).bind (·[k]?)

def conflict : Op → Op → Bool
  | .r _, .r _ => false
  | .r c, .w d | .w c, .r d | .w c, .w d => c == d

def stepI (prog : List (List Op)) (s : St) (e : Nat × Nat) : Option St :=
  match opAt prog e.1 e.2 with
  | none => none
  | some op =>
    if s.pcs.getD e.1 0 = e.2 then
      let pcs := s.pcs.set e.1 (e.2 + 1)
      match op with
      | .r c => some { s with pcs, logs := s.logs.set e.1 (s.logs.getD e.1 [] ++ [s.cells.getD c 0]) }
      | .w c => some { s with pcs, cells := s.cells.set c (100 * (e.1 + 1) + e.2 + 1) }
    else none

def indepI (prog : List (List Op)) (a b : Nat × Nat) : Bool :=
  a.1 != b.1 &&
    match opAt prog a.1 a.2, opAt prog b.1 b.2 with
    | some x, some y => !conflict x y
    | _, _ => true

def actsI (prog : List (List Op)) : List (Nat × Nat) :=
  (List.range prog.length).flatMap fun p => (List.range (prog.getD p []).length).map (p, ·)

def parseOp (s : String) : Option Op :=
  match s.toList with
  | 'r' :: rest => (String.mk rest).toNat?.map .r
  | 'w' :: rest => (String.mk rest).toNat?.map .w
  | _ => none

def parseProg (line : String) : Option (List (List Op)) :=
  (line.splitOn ";").mapM fun p => if p.isEmpty then some [] else (p.splitOn ",").mapM parseOp

def showNats (xs : List Nat) : String := ".".intercalate (xs.map toString)

def showSt (s : St) : String :=
  showNats s.cells ++ "/" ++ ":".intercalate (s.logs.map showNats)

def cellsUsed : Nat := 3

def runProg (prog : List (List Op)) : String :=
  let s0 : St := { pcs := prog.map fun _ => 0, cells := List.replicate cellsUsed 0,
                   logs := prog.map fun _ => [] }
  let fuel := (prog.map List.length).foldl (· + ·) 0
  let words := explore (stepI prog) (actsI prog) (indepI prog) fuel s0 []
  let finals := words.filterMap fun v => (run (stepI prog) s0 v).map showSt
  s!"{words.length}|" ++ " ".intercalate finals

partial def loop (stdin : IO.FS.Stream) : IO Unit := do
  let line ← stdin.getLine
  if line.isEmpty then return
  match parseProg line.trimRight with
  | some prog => IO.println (runProg prog)
  | none => IO.println "parse-error"
  loop stdin

def main : IO Unit := do
  IO.println s!"{modelId} | {modelClaim}"
  loop (← IO.getStdin)
