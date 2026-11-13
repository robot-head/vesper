import Lean4Lean.Environment

open Lean

def universeControl (name : Name) (small : Bool) : Declaration :=
  let result := if small then Level.succ Level.zero else Level.succ (Level.succ Level.zero)
  let constructorType :=
    Expr.forallE `A (.sort (.succ .zero))
      (.forallE `value (.bvar 0) (.const name []) .default) .default
  .inductDecl [] 0
    [{ name, type := .sort result,
       ctors := [{ name := name ++ `mk, type := constructorType }] }] false

def accepted (result : Except Kernel.Exception Kernel.Environment) : Bool :=
  match result with | .ok _ => true | .error _ => false

def main : IO Unit := do
  let env := Kernel.Environment.empty `UniverseControls
  for (name, small) in [(`UniverseControls.Good, false), (`UniverseControls.TooSmall, true)] do
    let declaration := universeControl name small
    let reference := accepted (env.addDecl {} declaration)
    let externalResult := Lean4Lean.addDecl env declaration
    let external := accepted externalResult
    unless reference == !small && external == reference do
      throw <| IO.userError s!"universe control {name}: kernel={reference}, external={external}"
    if small then
      match externalResult with
      | .error (.other message) =>
        unless "universe level".isPrefixOf message do
          throw <| IO.userError "Invalid universe was rejected by an unrelated check."
      | _ => throw <| IO.userError "Expected the retained inductive universe guard."
  IO.println "Actual kernel and external checker accept the valid datatype and reject the undersized universe."
