/-!
Logical admission rules for delayed propositions and public quotient operations.
Clock quantification is represented by validity at every step index. This model
makes no claim that the surface language already implements clock binders.
Quotient operations require actual Lean congruence proofs; a caller-supplied
Rust status flag cannot construct these proofs.
-/
namespace Vesper.ClockQuotientSafety

abbrev ClockedProp := Nat → Prop

def Later (P : ClockedProp) : ClockedProp
  | 0 => True
  | n + 1 => P n

def ClockClosed (P : ClockedProp) : Prop := ∀ tick, P tick

/-- Only validity at every fresh-clock index permits elimination independently
of the current tick. The context supplying this premise must be clock-closed. -/
theorem force_clock_closed {P : ClockedProp} (proof : ClockClosed (Later P)) :
    ClockClosed P := fun n => proof (n + 1)

/-- Merely matching a clock name cannot eliminate a delayed proposition. -/
theorem fixed_clock_force_unsound :
    ¬ (∀ P : ClockedProp, ∀ tick, Later P tick → P tick) := by
  intro force
  exact force (fun _ => False) 0 True.intro

theorem delayed_false_at_initial_tick : Later (fun _ => False) 0 := True.intro

theorem no_clock_closed_delayed_false : ¬ ClockClosed (Later (fun _ => False)) := by
  intro proof
  exact force_clock_closed proof 0

/-- A total guarded step constructs an index-valid proposition by induction.
This is a logical, pure, reusable function. It does not license duplication of
runtime affine/linear closure captures or effects. -/
theorem guarded_fix (step : ∀ tick, Later P tick → P tick) : ClockClosed P := by
  intro tick
  induction tick with
  | zero => exact step 0 True.intro
  | succ n ih => exact step (n + 1) ih

theorem no_guarded_false_step :
    ¬ (∀ tick, Later (fun _ => False) tick → False) := by
  intro step
  exact guarded_fix step 0

/-- The evidence demanded by an observation returning an ordinary value. -/
def Respects (relation : A → A → Prop) (observe : A → B) : Prop :=
  ∀ a b, relation a b → observe a = observe b

def publicObserve (relation : A → A → Prop) (observe : A → B)
    (certificate : Respects relation observe) : Quot relation → B :=
  Quot.lift observe certificate

theorem observation_independent_of_representative (relation : A → A → Prop)
    (observe : A → B) (certificate : Respects relation observe) (a b : A)
    (related : relation a b) :
    publicObserve relation observe certificate (Quot.mk relation a) =
      publicObserve relation observe certificate (Quot.mk relation b) :=
  congrArg (publicObserve relation observe certificate) (Quot.sound related)

/-- An `inout` operation must preserve both its visible result and the abstract
state. Proving only one of these does not justify quotient lifting. -/
def StepRespects (relation : A → A → Prop) (step : A → B × A) : Prop :=
  ∀ a b, relation a b → (step a).1 = (step b).1 ∧ relation (step a).2 (step b).2

def publicStep (relation : A → A → Prop) (step : A → B × A)
    (certificate : StepRespects relation step) : Quot relation → B × Quot relation :=
  Quot.lift (fun a => ((step a).1, Quot.mk relation (step a).2))
    (fun a b related => Prod.ext (certificate a b related).1
      (Quot.sound (certificate a b related).2))

theorem step_independent_of_representative (relation : A → A → Prop)
    (step : A → B × A) (certificate : StepRespects relation step) (a b : A)
    (related : relation a b) :
    publicStep relation step certificate (Quot.mk relation a) =
      publicStep relation step certificate (Quot.mk relation b) :=
  congrArg (publicStep relation step certificate) (Quot.sound related)

/-- If all capacities represent the same abstract value, exposing capacity
cannot be equipped with the required congruence proof. -/
theorem representation_observer_rejected :
    ¬ Respects (fun _ _ : Nat => True) (fun capacity => capacity) := by
  intro certificate
  have impossible : (0 : Nat) = 1 := certificate 0 1 True.intro
  cases impossible

/-- Resetting state to one representative is insufficient when the returned
value still reveals the old representation. -/
theorem state_only_congruence_insufficient :
    ¬ StepRespects (fun _ _ : Nat => True) (fun capacity => (capacity, 0)) := by
  intro certificate
  have impossible : (0 : Nat) = 1 := (certificate 0 1 True.intro).1
  cases impossible

end Vesper.ClockQuotientSafety

#print axioms Vesper.ClockQuotientSafety.force_clock_closed
#print axioms Vesper.ClockQuotientSafety.fixed_clock_force_unsound
#print axioms Vesper.ClockQuotientSafety.delayed_false_at_initial_tick
#print axioms Vesper.ClockQuotientSafety.no_clock_closed_delayed_false
#print axioms Vesper.ClockQuotientSafety.guarded_fix
#print axioms Vesper.ClockQuotientSafety.no_guarded_false_step
#print axioms Vesper.ClockQuotientSafety.observation_independent_of_representative
#print axioms Vesper.ClockQuotientSafety.step_independent_of_representative
#print axioms Vesper.ClockQuotientSafety.representation_observer_rejected
#print axioms Vesper.ClockQuotientSafety.state_only_congruence_insufficient
