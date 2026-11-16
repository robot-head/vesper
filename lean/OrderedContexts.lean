import VesperUnifiedTypeTheory

/-! Ordered obligations live in a stack, not the exchangeable place context.
This is the trace checker corresponding to the acquire/release discipline of
`vesper-core::lifo::LifoState` (head here is newest, unlike its oldest-first Vec).
It does not prove that source lowering emits every required guard event. -/
namespace Vesper.OrderedContexts
open Vesper.UnifiedTypeTheory

inductive Event where
  | acquire (guardId : GuardId)
  | release (guardId : GuardId)
  deriving DecidableEq, Repr

def check (stack : List GuardId) : List Event → Option (List GuardId)
  | [] => some stack
  | .acquire guardId :: rest => if guardId ∈ stack then none else check (guardId :: stack) rest
  | .release guardId :: rest => (popLifo stack guardId).bind (fun next => check next rest)

inductive Typed : List GuardId → List Event → List GuardId → Prop where
  | done (stack) : Typed stack [] stack
  | acquire : guardId ∉ stack → Typed (guardId :: stack) events final →
      Typed stack (.acquire guardId :: events) final
  | release : Typed stack events final → Typed (guardId :: stack) (.release guardId :: events) final

theorem typed_checks (typed : Typed stack events final) : check stack events = some final := by
  induction typed with
  | done => rfl
  | acquire fresh _ ih => simp [check, fresh, ih]
  | release _ ih => simp [check, popLifo, ih]

theorem checked_is_typed : ∀ events stack final, check stack events = some final → Typed stack events final := by
  intro events
  induction events with
  | nil => intro stack final h; cases h; exact .done stack
  | cons event events ih =>
    intro stack final h
    cases event with
    | acquire guardId =>
      by_cases fresh : guardId ∈ stack
      · simp [check, fresh] at h
      · exact .acquire fresh (ih _ _ (by simpa [check, fresh] using h))
    | release guardId =>
      cases stack with
      | nil => simp [check, popLifo] at h
      | cons top stack =>
        by_cases sameGuard : top = guardId
        · subst top
          exact .release (ih _ _ (by simpa [check, popLifo] using h))
        · simp [check, popLifo, sameGuard] at h

theorem check_iff_typed : check stack events = some final ↔ Typed stack events final :=
  ⟨checked_is_typed events stack final, typed_checks⟩

/-- Prefixes also typecheck and expose exactly the stack required by the afterEvents. -/
theorem typed_prefix (typed : Typed stack (beforeEvents ++ afterEvents) final) :
    ∃ middle, Typed stack beforeEvents middle ∧ Typed middle afterEvents final := by
  induction beforeEvents generalizing stack with
  | nil => exact ⟨stack, .done stack, typed⟩
  | cons event beforeEvents ih =>
    cases typed with
    | acquire fresh rest =>
      obtain ⟨middle, first, second⟩ := ih rest
      exact ⟨middle, .acquire fresh first, second⟩
    | release rest =>
      obtain ⟨middle, first, second⟩ := ih rest
      exact ⟨middle, .release first, second⟩

def acquisitions (guardId : GuardId) : List Event → Nat
  | [] => 0
  | .acquire other :: rest => (if guardId = other then 1 else 0) + acquisitions guardId rest
  | .release _ :: rest => acquisitions guardId rest

def releases (guardId : GuardId) : List Event → Nat
  | [] => 0
  | .acquire _ :: rest => releases guardId rest
  | .release other :: rest => (if guardId = other then 1 else 0) + releases guardId rest

theorem ordered_conservation (typed : Typed stack events final) (guardId : GuardId) :
    final.count guardId + releases guardId events = stack.count guardId + acquisitions guardId events := by
  induction typed with
  | done => simp [acquisitions, releases]
  | @acquire stack other events final _ _ ih =>
    by_cases h : guardId = other
    · subst other; simp_all [acquisitions, releases] <;> omega
    · have different := Ne.symm h
      simp_all [acquisitions, releases] <;> omega
  | @release stack events final other _ ih =>
    by_cases h : guardId = other
    · subst other; simp_all [acquisitions, releases] <;> omega
    · have different := Ne.symm h
      simp_all [acquisitions, releases] <;> omega

theorem closed_trace_exhausts (typed : Typed [] events []) (guardId : GuardId) :
    releases guardId events = acquisitions guardId events := by
  simpa using ordered_conservation typed guardId

theorem out_of_order_release_rejected (outer inner : GuardId) (rest : List GuardId)
    (different : inner ≠ outer) (events : List Event) :
    check (inner :: outer :: rest) (.release outer :: events) = none := by
  simp [check, popLifo, different]

theorem duplicate_active_guard_rejected (guardId : GuardId) (stack : List GuardId)
    (active : guardId ∈ stack) (events : List Event) :
    check stack (.acquire guardId :: events) = none := by simp [check, active]

theorem nested_trace_typed :
    Typed [] [.acquire 0, .acquire 1, .release 1, .release 0] [] := by
  exact .acquire (by simp) (.acquire (by simp) (.release (.release (.done []))))

end Vesper.OrderedContexts

#print axioms Vesper.OrderedContexts.typed_checks
#print axioms Vesper.OrderedContexts.checked_is_typed
#print axioms Vesper.OrderedContexts.check_iff_typed
#print axioms Vesper.OrderedContexts.typed_prefix
#print axioms Vesper.OrderedContexts.ordered_conservation
#print axioms Vesper.OrderedContexts.closed_trace_exhausts
#print axioms Vesper.OrderedContexts.out_of_order_release_rejected
#print axioms Vesper.OrderedContexts.duplicate_active_guard_rejected
#print axioms Vesper.OrderedContexts.nested_trace_typed
