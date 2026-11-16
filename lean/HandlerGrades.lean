import EffectPlaces

/-!
Additive grading for a finite, one-shot, stateful handler fragment. Operation
clauses may stop or resume once, with work before/after the continuation.
The operational evaluator is connected to the repository's HandlerCPS encoding.
The bound counts continuation work once; clause overhead excludes that work.
This does not infer source budgets, prove machine costs, or grade linear state.
-/
namespace Vesper.HandlerGrades
open Vesper.EffectPlaces

structure Cost (α : Type) where
  value : α
  work : Nat

instance : Monad Cost where
  pure value := ⟨value, 0⟩
  bind first next :=
    let second := next first.value
    ⟨second.value, first.work + second.work⟩

def charge (work : Nat) (result : Cost α) : Cost α := ⟨result.value, work + result.work⟩

inductive Outcome where
  | value (result : Nat)
  | aborted
  deriving DecidableEq

abbrev Result := Cost (Outcome × Nat)

inductive Clause where
  | stop (result state work : Nat)
  | resume (argument state before after : Nat)

structure Handler where
  operation : Nat → Nat → Clause
  returnWork : Nat → Nat → Nat

inductive Program where
  | ret (value : Nat)
  | spend (work : Nat) (next : Program)
  | request (argument : Nat) (next : Nat → Program)
  | abort

def finish (handler : Handler) (value state : Nat) : Result :=
  ⟨(.value value, state), handler.returnWork value state⟩

-- An outer abort bypasses code after the resumption, including its cost.
def resumed (before after : Nat) (result : Result) : Result :=
  charge before (match result.value.1 with
    | .aborted => result
    | .value _ => charge after result)

def run (handler : Handler) : Program → Nat → Result
  | .ret value, state => finish handler value state
  | .spend work next, state => charge work (run handler next state)
  | .request argument next, state =>
      match handler.operation argument state with
      | .stop value state work => (⟨(.value value, state), work⟩ : Result)
      | .resume value state before after => resumed before after (run handler (next value) state)
  | .abort, state => ⟨(.aborted, state), 0⟩

def emit (work : Nat) : HandlerCPS Nat (Outcome × Nat) Cost Unit :=
  fun continuation state => charge work (continuation () state)

def operation (handler : Handler) (argument : Nat) : HandlerCPS Nat (Outcome × Nat) Cost Nat :=
  fun continuation state => match handler.operation argument state with
    | .stop value state work => (⟨(.value value, state), work⟩ : Result)
    | .resume value state before after => resumed before after (continuation value state)

def extract (handler : Handler) : Program → HandlerCPS Nat (Outcome × Nat) Cost Nat
  | .ret value => pure value
  | .spend work next => emit work >>= fun _ => extract handler next
  | .request argument next => operation handler argument >>= fun value => extract handler (next value)
  | .abort => fun _ state => ⟨(.aborted, state), 0⟩

theorem evaluator_matches_cps (handler : Handler) (program : Program) :
    ∀ state, run handler program state = extract handler program (finish handler) state := by
  induction program with
  | ret value => intro state; rfl
  | abort => intro state; rfl
  | spend work next ih =>
      intro state
      change charge work (run handler next state) = charge work (extract handler next (finish handler) state)
      rw [ih]
  | request argument next ih =>
      intro state
      cases action : handler.operation argument state with
      | stop value state' work => simp [run, extract, Bind.bind, operation, action]
      | resume value state' before after =>
          simp [run, extract, Bind.bind, operation, action, ih]


-- The base budget accounts for explicit body work. Every possible operation
-- result has the same upper bound; recursion needs a separate bounded derivation.
inductive Budget : Program → Nat → Nat → Prop where
  | ret (value) : Budget (.ret value) 0 0
  | abort : Budget .abort 0 0
  | spend : Budget next baseWork operations → Budget (.spend work next) (work + baseWork) operations
  | request : (∀ value, Budget (next value) baseWork operations) →
      Budget (.request argument next) baseWork (operations + 1)

def HandlerBound (handler : Handler) (clauseWork returnWork : Nat) : Prop :=
  (∀ argument state, match handler.operation argument state with
    | .stop _ _ work => work ≤ clauseWork
    | .resume _ _ before after => before + after ≤ clauseWork) ∧
  (∀ value state, handler.returnWork value state ≤ returnWork)

theorem resumed_work_bound (before after : Nat) (result : Result) :
    (resumed before after result).work ≤ before + result.work + after := by
  cases outcome : result.value.1 <;> simp [resumed, outcome, charge] <;> omega

theorem handler_budget_sound (budget : Budget program baseWork operations)
    (bound : HandlerBound handler clauseWork returnWork) :
    ∀ state, (run handler program state).work ≤ baseWork + operations * clauseWork + returnWork := by
  induction budget with
  | ret value => intro state; simpa [run, finish] using bound.2 value state
  | abort => intro state; simp [run]
  | spend budget ih =>
      intro state
      have next := ih state
      simp only [run, charge]
      omega
  | @request next baseWork operations argument budget ih =>
      intro state
      have clause := bound.1 argument state
      cases action : handler.operation argument state with
      | stop value state' work =>
          simp only [action] at clause
          simp only [run, action, Nat.add_mul, Nat.one_mul]
          omega
      | resume value state' before after =>
          simp only [action] at clause
          have continuation := ih value state'
          have resumedBound := resumed_work_bound before after (run handler (next value) state')
          simp only [run, action, Nat.add_mul, Nat.one_mul]
          omega

theorem extracted_handler_budget (budget : Budget program baseWork operations)
    (bound : HandlerBound handler clauseWork returnWork) (state : Nat) :
    (extract handler program (finish handler) state).work ≤ baseWork + operations * clauseWork + returnWork := by
  rw [← evaluator_matches_cps]
  exact handler_budget_sound budget bound state

def allocatingHandler : Handler :=
  ⟨fun argument state => .resume argument (state + 1) 64 16, fun _ _ => 5⟩

def twoRequests : Program := .request 0 (fun _ => .request 1 Program.ret)

theorem two_requests_budget : Budget twoRequests 0 2 :=
  .request (fun _ => .request (fun value => .ret value))

theorem allocating_handler_bound : HandlerBound allocatingHandler 80 5 := by
  constructor <;> intros <;> simp [allocatingHandler]

theorem deep_handler_has_positive_clause_budget :
    (run allocatingHandler twoRequests 0).work = 165 ∧
      (run allocatingHandler twoRequests 0).value = (.value 1, 2) := by decide

theorem outer_abort_skips_post_resumption_work :
    (run allocatingHandler (.request 0 (fun _ => .abort)) 0).work = 64 := by decide

theorem stopping_clause_skips_continuation :
    (run ⟨fun _ state => .stop 7 state 3, fun _ _ => 5⟩
      (.request 0 (fun _ => .spend 100 (.ret 9))) 0).work = 3 := by decide

end Vesper.HandlerGrades

#print axioms Vesper.HandlerGrades.evaluator_matches_cps
#print axioms Vesper.HandlerGrades.resumed_work_bound
#print axioms Vesper.HandlerGrades.handler_budget_sound
#print axioms Vesper.HandlerGrades.extracted_handler_budget
#print axioms Vesper.HandlerGrades.two_requests_budget
#print axioms Vesper.HandlerGrades.allocating_handler_bound
#print axioms Vesper.HandlerGrades.deep_handler_has_positive_clause_budget
#print axioms Vesper.HandlerGrades.outer_abort_skips_post_resumption_work
#print axioms Vesper.HandlerGrades.stopping_clause_skips_continuation
