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

end Vesper.ResourceModel42.EvaluatorProofs
