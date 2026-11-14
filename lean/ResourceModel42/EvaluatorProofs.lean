import ResourceModel42.ErasureProofs
import ResourceModel42.LedgerProofs

namespace Vesper.ResourceModel42.EvaluatorProofs
open ErasureProofs LedgerProofs

theorem locate_plug {fuel : Nat} {e : RuntimeExpr} {focus : Focus}
    (h : locate fuel e = .ok focus) : focus.context.plug focus.redex = e := by
  induction fuel generalizing e focus with
  | zero => simp [locate] at h
  | succ fuel ih =>
      cases e with
      | letE initializer body =>
          cases hv : initializer.asValue with
          | some value => simp [locate, hv] at h; subst focus; rfl
          | none =>
              cases hi : locate fuel initializer with
              | error error =>
                  simp only [locate, hv] at h
                  rw [hi] at h
                  change Except.error error = Except.ok focus at h
                  contradiction
              | ok inner =>
                  simp only [locate, hv] at h
                  rw [hi] at h
                  change Except.ok (Focus.mk (.letC inner.context body) inner.redex) = Except.ok focus at h
                  cases h
                  simp [RuntimeContext.plug, ih hi]
      | atom a => simp [locate] at h; subst focus; rfl
      | pair a b => simp [locate] at h; subst focus; rfl
      | unpack a body => simp [locate] at h; subst focus; rfl
      | ifz a zero nonzero => simp [locate] at h; subst focus; rfl
      | mint site => simp [locate] at h; subst focus; rfl
      | consume site a => simp [locate] at h; subst focus; rfl

theorem prepare_sound (Sig : Nat → MetatheoryFull.EffSig) (c : Config)
    {e : RuntimeExpr} {plan : Prepared} (h : prepare c.ledger e = .ok plan) :
    MetatheoryFull.Head Sig e.erase c.live c.ledger.size
      (plan.action.full c.ledger.size) plan.replacement.materialize.erase
      (Config.mk plan.replacement.materialize (plan.action.apply c.ledger)).live
      (plan.action.apply c.ledger).size := by
  cases e with
  | atom a => simp [prepare] at h
  | pair a b => simp [prepare] at h
  | letE initializer body =>
      cases hv : initializer.asValue with
      | none => simp [prepare, hv] at h
      | some value =>
          simp [prepare, hv] at h
          subst plan
          simpa [Replacement.materialize, PlannedAction.full, PlannedAction.apply,
            Config.live, RuntimeExpr.erase, asValue_erase hv, runtimeExpr_erase_subst] using
            (MetatheoryFull.Head.letV (Sig := Sig) (L := c.live)
              (n := c.ledger.size) (e2 := body.erase) (runtimeValue_isVal value))
  | unpack a body =>
      cases a with
      | var _ => simp [prepare] at h
      | val value =>
          cases value with
          | unit => simp [prepare] at h
          | nat _ => simp [prepare] at h
          | hdl _ => simp [prepare] at h
          | pair left right =>
              simp [prepare] at h
              subst plan
              simpa [Replacement.materialize, PlannedAction.full, PlannedAction.apply,
                Config.live, RuntimeExpr.erase, RuntimeAtom.erase, RuntimeValue.erase,
                runtimeExpr_erase_unpack_subst] using
                (MetatheoryFull.Head.letPairV (Sig := Sig) (L := c.live)
                  (n := c.ledger.size) (e := body.erase)
                  (runtimeValue_isVal left) (runtimeValue_isVal right))
  | ifz a zero nonzero =>
      cases a with
      | var _ => simp [prepare] at h
      | val value =>
          cases value with
          | unit => simp [prepare] at h
          | hdl _ => simp [prepare] at h
          | pair _ _ => simp [prepare] at h
          | nat n =>
              cases n with
              | zero =>
                  simp [prepare] at h
                  subst plan
                  exact MetatheoryFull.Head.ifzZ
              | succ n =>
                  simp [prepare] at h
                  subst plan
                  exact MetatheoryFull.Head.ifzS
  | mint site =>
      simp [prepare] at h
      subst plan
      simpa [Replacement.materialize, PlannedAction.full, PlannedAction.apply,
        live_push, RuntimeExpr.erase, RuntimeAtom.erase, RuntimeValue.erase] using
        (MetatheoryFull.Head.mint (Sig := Sig) (L := c.live) (n := c.ledger.size))
  | consume site a =>
      cases a with
      | var _ => simp [prepare] at h
      | val value =>
          cases value with
          | unit => simp [prepare] at h
          | nat _ => simp [prepare] at h
          | pair _ _ => simp [prepare] at h
          | hdl id =>
              cases hl : c.ledger[id]? with
              | none => simp [prepare, hl] at h
              | some entry =>
                  cases hb : entry.live with
                  | false => simp [prepare, hl, hb] at h
                  | true =>
                      simp [prepare, hl, hb] at h
                      subst plan
                      simpa [Replacement.materialize, PlannedAction.full, PlannedAction.apply,
                        live_modify_dead, RuntimeExpr.erase, RuntimeAtom.erase, RuntimeValue.erase] using
                        (MetatheoryFull.Head.consume (Sig := Sig) (n := c.ledger.size)
                          (live_lookup c hl hb))

theorem step_sound (Sig : Nat → MetatheoryFull.EffSig) {l : Limits} {count : Nat}
    {c next : Config} {action : MetatheoryFull.Act} {event : Option ResourceEvent}
    (h : step l count c = .next next action event) :
    MetatheoryFull.Step Sig c.erase action next.erase := by
  cases hv : c.expr.asValue with
  | some value => simp [step, hv] at h
  | none =>
      cases hf : locate l.runtimeDepth c.expr with
      | error error => simp [step, hv, hf] at h
      | ok focus =>
          cases hp : prepare c.ledger focus.redex with
          | error error => simp [step, hv, hf, hp] at h
          | ok plan =>
              cases hs : plan.replacement.preflight l focus.context with
              | error error => simp [step, hv, hf, hp, hs] at h
              | ok stats =>
                  cases ha : plan.action.preflight l c.ledger.size count with
                  | error error => simp [step, hv, hf, hp, hs, ha] at h
                  | ok okValue =>
                      simp [step, hv, hf, hp, hs, ha] at h
                      rcases h with ⟨rfl, rfl, rfl⟩
                      have source := congrArg RuntimeExpr.erase (locate_plug hf)
                      rw [runtimeContext_erase_plug] at source
                      have head := prepare_sound Sig c hp
                      have result := MetatheoryFull.Step.ctx focus.context.erase head
                      simpa [Config.erase, ← source, runtimeContext_erase_plug, Config.live] using result

theorem terminal_isVal {l : Limits} {count : Nat} {c : Config} {value : RuntimeValue}
    (h : step l count c = .terminal value) : MetatheoryFull.IsVal c.expr.erase := by
  cases hv : c.expr.asValue with
  | some v => exact asValue_sound hv
  | none =>
      cases hf : locate l.runtimeDepth c.expr with
      | error error => simp [step, hv, hf] at h
      | ok focus =>
          cases hp : prepare c.ledger focus.redex with
          | error error => simp [step, hv, hf, hp] at h
          | ok plan =>
              cases hs : plan.replacement.preflight l focus.context with
              | error error => simp [step, hv, hf, hp, hs] at h
              | ok stats =>
                  cases ha : plan.action.preflight l c.ledger.size count with
                  | error error => simp [step, hv, hf, hp, hs, ha] at h
                  | ok okValue => simp [step, hv, hf, hp, hs, ha] at h

theorem runMachine_completed_isVal (l : Limits) (fuel : Nat) (c : Config)
    (events : List ResourceEvent) (actions : List MetatheoryFull.Act) (value : RuntimeValue)
    (h : (runMachine l fuel c events actions).status = .completed value) :
    MetatheoryFull.IsVal (runMachine l fuel c events actions).config.expr.erase := by
  induction fuel generalizing c events actions with
  | zero =>
      cases hv : c.expr.asValue with
      | some v => simpa [runMachine, hv] using asValue_sound hv
      | none => simp [runMachine, hv] at h
  | succ fuel ih =>
      cases hv : c.expr.asValue with
      | some v => simpa [runMachine, hv] using asValue_sound hv
      | none =>
          cases hs : step l events.length c with
          | terminal v => simpa [runMachine, hv, hs] using terminal_isVal hs
          | limited error => simp [runMachine, hv, hs] at h
          | invariant error => simp [runMachine, hv, hs] at h
          | next next action event =>
              cases event with
              | none =>
                  simpa [runMachine, hv, hs] using
                    ih next events (action :: actions) (by simpa [runMachine, hv, hs] using h)
              | some ev =>
                  simpa [runMachine, hv, hs] using
                    ih next (ev :: events) (action :: actions) (by simpa [runMachine, hv, hs] using h)

theorem runMachine_prefix (Sig : Nat → MetatheoryFull.EffSig) (l : Limits) (fuel : Nat)
    (c : Config) (events : List ResourceEvent) (actions : List MetatheoryFull.Act) :
    ∃ suffix, (runMachine l fuel c events actions).actionsRev = suffix.reverse ++ actions ∧
      MetatheoryFull.Steps Sig c.erase suffix (runMachine l fuel c events actions).config.erase := by
  induction fuel generalizing c events actions with
  | zero =>
      cases hv : c.expr.asValue <;>
        exact ⟨[], by simp [runMachine, hv], by simpa [runMachine, hv] using MetatheoryFull.Steps.refl c.erase⟩
  | succ fuel ih =>
      cases hv : c.expr.asValue with
      | some value =>
          exact ⟨[], by simp [runMachine, hv], by simpa [runMachine, hv] using MetatheoryFull.Steps.refl c.erase⟩
      | none =>
          cases hs : step l events.length c with
          | terminal value =>
              exact ⟨[], by simp [runMachine, hv, hs], by simpa [runMachine, hv, hs] using MetatheoryFull.Steps.refl c.erase⟩
          | limited error =>
              exact ⟨[], by simp [runMachine, hv, hs], by simpa [runMachine, hv, hs] using MetatheoryFull.Steps.refl c.erase⟩
          | invariant error =>
              exact ⟨[], by simp [runMachine, hv, hs], by simpa [runMachine, hv, hs] using MetatheoryFull.Steps.refl c.erase⟩
          | next next action event =>
              cases event with
              | none =>
                  obtain ⟨suffix, history, steps⟩ := ih next events (action :: actions)
                  refine ⟨action :: suffix, ?_, ?_⟩
                  · simpa [runMachine, hv, hs, List.reverse_cons, List.append_assoc] using history
                  · simpa [runMachine, hv, hs] using MetatheoryFull.Steps.cons (step_sound Sig hs) steps
              | some ev =>
                  obtain ⟨suffix, history, steps⟩ := ih next (ev :: events) (action :: actions)
                  refine ⟨action :: suffix, ?_, ?_⟩
                  · simpa [runMachine, hv, hs, List.reverse_cons, List.append_assoc] using history
                  · simpa [runMachine, hv, hs] using MetatheoryFull.Steps.cons (step_sound Sig hs) steps

theorem runMachine_steps (Sig : Nat → MetatheoryFull.EffSig) (l : Limits)
    (fuel : Nat) (c : Config) :
    MetatheoryFull.Steps Sig c.erase (runMachine l fuel c [] []).actions
      (runMachine l fuel c [] []).config.erase := by
  obtain ⟨suffix, history, steps⟩ := runMachine_prefix Sig l fuel c [] []
  have eq : (runMachine l fuel c [] []).actions = suffix := by
    simp [EvalResult.actions, history]
  rw [eq]
  exact steps

theorem terminal_value_erase {l : Limits} {count : Nat} {c : Config} {value : RuntimeValue}
    (h : step l count c = .terminal value) : c.expr.erase = value.erase := by
  cases hv : c.expr.asValue with
  | some actual =>
      simp [step, hv] at h
      subst value
      exact asValue_erase hv
  | none =>
      obtain ⟨actual, ha⟩ := asValue_complete (terminal_isVal h)
      rw [hv] at ha
      contradiction

theorem completed_value_erase (l : Limits) (fuel : Nat) (c : Config)
    (events : List ResourceEvent) (actions : List MetatheoryFull.Act) (value : RuntimeValue)
    (h : (runMachine l fuel c events actions).status = .completed value) :
    (runMachine l fuel c events actions).config.expr.erase = value.erase := by
  induction fuel generalizing c events actions with
  | zero =>
      cases hv : c.expr.asValue with
      | some actual =>
          simp [runMachine, hv] at h
          subst value
          simpa [runMachine, hv] using asValue_erase hv
      | none => simp [runMachine, hv] at h
  | succ fuel ih =>
      cases hv : c.expr.asValue with
      | some actual =>
          simp [runMachine, hv] at h
          subst value
          simpa [runMachine, hv] using asValue_erase hv
      | none =>
          cases hs : step l events.length c with
          | terminal actual =>
              simp [runMachine, hv, hs] at h
              subst value
              simpa [runMachine, hv, hs] using terminal_value_erase hs
          | limited error => simp [runMachine, hv, hs] at h
          | invariant error => simp [runMachine, hv, hs] at h
          | next next action event =>
              cases event with
              | none =>
                  simpa [runMachine, hv, hs] using
                    ih next events (action :: actions) (by simpa [runMachine, hv, hs] using h)
              | some ev =>
                  simpa [runMachine, hv, hs] using
                    ih next (ev :: events) (action :: actions) (by simpa [runMachine, hv, hs] using h)

def eventAct (event : ResourceEvent) : MetatheoryFull.Act :=
  match event.action with | .mint => .mint event.id | .consume => .consume event.id

def resourceAction (action : MetatheoryFull.Act) : Bool := action != .silent

theorem planned_event_projection (size : Nat) (action : PlannedAction) :
    (action.event size).toList.map eventAct = [action.full size].filter resourceAction := by
  cases action <;> simp [PlannedAction.event, PlannedAction.full, eventAct, resourceAction]

theorem step_event_projection {l : Limits} {count : Nat} {c next : Config}
    {action : MetatheoryFull.Act} {event : Option ResourceEvent}
    (h : step l count c = .next next action event) :
    event.toList.map eventAct = [action].filter resourceAction := by
  cases hv : c.expr.asValue with
  | some value => simp [step, hv] at h
  | none =>
      cases hf : locate l.runtimeDepth c.expr with
      | error error => simp [step, hv, hf] at h
      | ok focus =>
          cases hp : prepare c.ledger focus.redex with
          | error error => simp [step, hv, hf, hp] at h
          | ok plan =>
              cases hs : plan.replacement.preflight l focus.context with
              | error error => simp [step, hv, hf, hp, hs] at h
              | ok stats =>
                  cases ha : plan.action.preflight l c.ledger.size count with
                  | error error => simp [step, hv, hf, hp, hs, ha] at h
                  | ok okValue =>
                      simp [step, hv, hf, hp, hs, ha] at h
                      rcases h with ⟨rfl, rfl, rfl⟩
                      exact planned_event_projection _ _

theorem runMachine_event_projection (l : Limits) (fuel : Nat) (c : Config)
    (events : List ResourceEvent) (actions : List MetatheoryFull.Act)
    (seed : events.map eventAct = actions.filter resourceAction) :
    (runMachine l fuel c events actions).eventsRev.map eventAct =
      (runMachine l fuel c events actions).actionsRev.filter resourceAction := by
  induction fuel generalizing c events actions with
  | zero => cases hv : c.expr.asValue <;> simpa [runMachine, hv] using seed
  | succ fuel ih =>
      cases hv : c.expr.asValue with
      | some value => simpa [runMachine, hv] using seed
      | none =>
          cases hs : step l events.length c with
          | terminal value => simpa [runMachine, hv, hs] using seed
          | limited error => simpa [runMachine, hv, hs] using seed
          | invariant error => simpa [runMachine, hv, hs] using seed
          | next next action event =>
              have projection := step_event_projection hs
              cases event with
              | none =>
                  have nextSeed : events.map eventAct = (action :: actions).filter resourceAction := by
                    have joined := congrArg (fun xs => xs ++ events.map eventAct) projection
                    simpa [seed, ← List.filter_append] using joined
                  simpa [runMachine, hv, hs] using ih next events (action :: actions) nextSeed
              | some ev =>
                  have nextSeed : (ev :: events).map eventAct = (action :: actions).filter resourceAction := by
                    have joined := congrArg (fun xs => xs ++ events.map eventAct) projection
                    simpa [seed, ← List.filter_append] using joined
                  simpa [runMachine, hv, hs] using ih next (ev :: events) (action :: actions) nextSeed

theorem runMachine_trace_actions (l : Limits) (fuel : Nat) (c : Config) :
    (runMachine l fuel c [] []).events.map eventAct =
      (runMachine l fuel c [] []).actions.filter resourceAction := by
  have h := runMachine_event_projection l fuel c [] [] rfl
  simpa [EvalResult.events, EvalResult.actions] using congrArg List.reverse h

theorem typed_atom_value (Sig : Nat → MetatheoryFull.EffSig) {R τ m ε}
    {a : RuntimeAtom} (typing : MetatheoryFull.HasType Sig MetatheoryFull.Ctx.empty R a.erase τ m ε) :
    ∃ value, a = .val value := by
  apply runtimeAtom_value_of_isVal
  apply MetatheoryFull.atom_closed _ typing
  cases a with
  | var index => exact .var index
  | val value => exact .val (runtimeValue_isVal value)

theorem typed_atom_pair (Sig : Nat → MetatheoryFull.EffSig) {R floor left ma right mb m ε}
    {a : RuntimeAtom}
    (typing : MetatheoryFull.HasType Sig MetatheoryFull.Ctx.empty R a.erase
      (.pair floor left ma right mb) m ε) :
    ∃ x y, a = .val (.pair x y) := by
  obtain ⟨value, rfl⟩ := typed_atom_value Sig typing
  obtain ⟨x, y, shape, _, _⟩ := MetatheoryFull.canon_pair typing (runtimeValue_isVal value)
  cases value <;> simp_all [RuntimeAtom.erase, RuntimeValue.erase]

theorem typed_atom_nat (Sig : Nat → MetatheoryFull.EffSig) {R m ε} {a : RuntimeAtom}
    (typing : MetatheoryFull.HasType Sig MetatheoryFull.Ctx.empty R a.erase .nat m ε) :
    ∃ n, a = .val (.nat n) := by
  obtain ⟨value, rfl⟩ := typed_atom_value Sig typing
  obtain ⟨n, shape⟩ := MetatheoryFull.canon_nat typing (runtimeValue_isVal value)
  cases value <;> simp_all [RuntimeAtom.erase, RuntimeValue.erase]

theorem typed_atom_handle (Sig : Nat → MetatheoryFull.EffSig) {R m ε} {a : RuntimeAtom}
    (typing : MetatheoryFull.HasType Sig MetatheoryFull.Ctx.empty R a.erase .hdl m ε) :
    ∃ id, a = .val (.hdl id) ∧ R id = some .linear := by
  obtain ⟨value, rfl⟩ := typed_atom_value Sig typing
  obtain ⟨id, shape, owned⟩ := MetatheoryFull.canon_hdl typing (runtimeValue_isVal value)
  cases value <;> simp_all [RuntimeAtom.erase, RuntimeValue.erase]

theorem locate_prepared (Sig : Nat → MetatheoryFull.EffSig) (c : Config)
    {fuel : Nat} {e : RuntimeExpr} {R τ m ε} {focus : Focus}
    (typing : MetatheoryFull.HasType Sig MetatheoryFull.Ctx.empty R e.erase τ m ε)
    (included : ∀ id mode, R id = some mode → c.live id = some mode)
    (nonvalue : e.asValue = none) (found : locate fuel e = .ok focus) :
    ∃ plan, prepare c.ledger focus.redex = .ok plan := by
  induction fuel generalizing e R τ m ε focus with
  | zero => simp [locate] at found
  | succ fuel ih =>
      cases e with
      | atom a =>
          obtain ⟨value, rfl⟩ := typed_atom_value Sig typing
          simp [RuntimeExpr.asValue] at nonvalue
      | pair a b =>
          cases typing with
          | pair _ _ split _ left right _ _ _ =>
              obtain ⟨gl, gr⟩ := MetatheoryFull.split_empty split
              rw [gl] at left
              rw [gr] at right
              obtain ⟨x, rfl⟩ := typed_atom_value Sig left
              obtain ⟨y, rfl⟩ := typed_atom_value Sig right
              simp [RuntimeExpr.asValue] at nonvalue
      | letE initializer body =>
          cases hv : initializer.asValue with
          | some value =>
              simp [locate, hv] at found
              subst focus
              exact ⟨⟨.substitute value body, .silent⟩, by simp [prepare, hv]⟩
          | none =>
              cases hi : locate fuel initializer with
              | error error =>
                  simp only [locate, hv] at found
                  rw [hi] at found
                  change Except.error error = Except.ok focus at found
                  contradiction
              | ok inner =>
                  simp only [locate, hv] at found
                  rw [hi] at found
                  change Except.ok (Focus.mk (.letC inner.context body) inner.redex) = Except.ok focus at found
                  cases found
                  cases typing with
                  | letE split splitR initTy _ _ _ _ =>
                      obtain ⟨gi, _⟩ := MetatheoryFull.split_empty split
                      rw [gi] at initTy
                      obtain ⟨plan, hp⟩ := ih (focus := inner) initTy
                        (MetatheoryFull.sub_left splitR included) hv hi
                      exact ⟨plan, hp⟩
      | unpack a body =>
          cases typing with
          | letPair _ split _ valueTy _ =>
              obtain ⟨ga, _⟩ := MetatheoryFull.split_empty split
              rw [ga] at valueTy
              obtain ⟨left, right, rfl⟩ := typed_atom_pair Sig valueTy
              simp [locate] at found
              subst focus
              exact ⟨_, rfl⟩
      | ifz a zero nonzero =>
          cases typing with
          | ifz _ split _ conditionTy _ _ =>
              obtain ⟨ga, _⟩ := MetatheoryFull.split_empty split
              rw [ga] at conditionTy
              obtain ⟨n, rfl⟩ := typed_atom_nat Sig conditionTy
              simp [locate] at found
              subst focus
              exact ⟨_, rfl⟩
      | mint site =>
          simp [locate] at found
          subst focus
          exact ⟨_, rfl⟩
      | consume site a =>
          cases typing with
          | consume _ valueTy =>
              obtain ⟨id, rfl, owned⟩ := typed_atom_handle Sig valueTy
              obtain ⟨entry, lookup, alive⟩ := (live_linear_iff c id).1 (included id .linear owned)
              simp [locate] at found
              subst focus
              exact ⟨⟨.unit, .consume site id⟩, by simp [prepare, lookup, alive]⟩

theorem step_noInvariant (Sig : Nat → MetatheoryFull.EffSig) {l : Limits} {count : Nat}
    {c : Config} {R τ m ε}
    (typing : MetatheoryFull.HasType Sig MetatheoryFull.Ctx.empty R c.expr.erase τ m ε)
    (included : ∀ id mode, R id = some mode → c.live id = some mode)
    (error : InvariantError) : step l count c ≠ .invariant error := by
  intro bad
  cases hv : c.expr.asValue with
  | some value => simp [step, hv] at bad
  | none =>
      cases hf : locate l.runtimeDepth c.expr with
      | error error => simp [step, hv, hf] at bad
      | ok focus =>
          obtain ⟨plan, hp⟩ := locate_prepared Sig c typing included hv hf
          cases hs : plan.replacement.preflight l focus.context with
          | error error => simp [step, hv, hf, hp, hs] at bad
          | ok stats =>
              cases ha : plan.action.preflight l c.ledger.size count with
              | error error => simp [step, hv, hf, hp, hs, ha] at bad
              | ok okValue => simp [step, hv, hf, hp, hs, ha] at bad

theorem typed_step_classification (Sig : Nat → MetatheoryFull.EffSig)
    {l : Limits} {count : Nat} {c : Config} {R τ m ε}
    (typing : MetatheoryFull.HasType Sig MetatheoryFull.Ctx.empty R c.expr.erase τ m ε)
    (included : ∀ id mode, R id = some mode → c.live id = some mode) :
    (∃ value, step l count c = .terminal value) ∨
    (∃ next action event, step l count c = .next next action event) ∨
    (∃ reason, step l count c = .limited reason) := by
  cases h : step l count c with
  | terminal value => exact Or.inl ⟨value, rfl⟩
  | next next action event => exact Or.inr (Or.inl ⟨next, action, event, rfl⟩)
  | limited reason => exact Or.inr (Or.inr ⟨reason, rfl⟩)
  | invariant error => exact False.elim (step_noInvariant Sig typing included error h)

theorem runMachine_noInvariant (Sig : Nat → MetatheoryFull.EffSig) (l : Limits)
    (fuel : Nat) (c : Config) (events : List ResourceEvent)
    (actions : List MetatheoryFull.Act) {τ m ε}
    (typing : MetatheoryFull.WT Sig c.erase τ m ε) (error : InvariantError) :
    (runMachine l fuel c events actions).status ≠ .invariantFailure error := by
  induction fuel generalizing c events actions with
  | zero => cases hv : c.expr.asValue <;> simp [runMachine, hv]
  | succ fuel ih =>
      cases hv : c.expr.asValue with
      | some value => simp [runMachine, hv]
      | none =>
          cases hs : step l events.length c with
          | terminal value => simp [runMachine, hv, hs]
          | limited reason => simp [runMachine, hv, hs]
          | invariant reason =>
              exact False.elim (step_noInvariant Sig typing.1 (fun _ _ h => h) reason hs)
          | next next action event =>
              have nextTyping := MetatheoryFull.preservation (step_sound Sig hs) typing
              cases event with
              | none => simpa [runMachine, hv, hs] using ih next events (action :: actions) nextTyping
              | some ev => simpa [runMachine, hv, hs] using ih next (ev :: events) (action :: actions) nextTyping

end Vesper.ResourceModel42.EvaluatorProofs
