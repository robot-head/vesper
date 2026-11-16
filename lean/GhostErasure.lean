import MetatheoryAppendix

/-!
A conservative annotated extension of the Appendix A operational core.
Ghost arithmetic is pure and total. Checked closed ghost computations may
mint and consume ghost-only linear resources in a separate allocation supply.
This model does not cover captured ghost tokens, dependent types, clocked
recursion, or a production compiler ghost pass (none exists).
The unsafe constructors have semantics but no runtime typing derivation.
-/
namespace Vesper.GhostErasure
open Vesper.Metatheory

inductive GhostTerm where
  | value (n : Nat)
  | variable
  | add (a b : GhostTerm)

def evalGhost (ghost : Nat) : GhostTerm → Nat
  | .value n => n
  | .variable => ghost
  | .add a b => evalGhost ghost a + evalGhost ghost b

def GhostProofWellTyped (proof : Expr) : Prop :=
  ∃ eff, HasType none proof .copy eff ∧ ¬ ResumptionMode.affine ∈ eff

-- Ghost-only linear resources are checked before erasure. A proof cannot leak
-- them or abort; closed_exact_once also guarantees that this premise is usable.
theorem ghost_proof_terminates (well : GhostProofWellTyped proof) :
    ∃ minted consumed v next,
      EvalDet none proof 0 minted consumed (.val v) next ∧
      (∀ resource, resource ∈ minted → consumed.count resource = 1) := by
  obtain ⟨eff, typed, pure⟩ := well
  obtain ⟨minted, consumed, out, next, ev, _, _, _, exactlyOnce⟩ := closed_exact_once typed 0
  cases out with
  | val v => exact ⟨minted, consumed, v, next, ev, exactlyOnce⟩
  | aborted => exact False.elim (pure (aborted_evaluation_has_affine_effect typed ev))

inductive Annotated where
  | core (e : Expr)
  | ghostLet (value : GhostTerm) (body : Annotated)
  | ghostProof (proof : Expr) (body : Annotated)
  | seq (first second : Annotated)
  | consume (value : Annotated)
  -- Counterexample syntax: neither has a runtime typing rule.
  | observeGhost
  | ghostAbort (body : Annotated)

def erase : Annotated → Expr
  | .core e => e
  | .ghostLet _ body | .ghostProof _ body => erase body
  | .seq a b => .letSeq (erase a) (erase b)
  | .consume value => .consumeLin (erase value)
  | .observeGhost => .litNat 0
  | .ghostAbort body => erase body

inductive RuntimeTyped : Option Mode → Annotated → Mode → EffectRow → Prop where
  | core : HasType gamma e m eff → RuntimeTyped gamma (.core e) m eff
  | ghostLet (g : GhostTerm) : RuntimeTyped gamma body m eff →
      RuntimeTyped gamma (.ghostLet g body) m eff
  | ghostProof : GhostProofWellTyped proof → RuntimeTyped gamma body m eff →
      RuntimeTyped gamma (.ghostProof proof body) m eff
  | seq : RuntimeTyped gamma first m1 eff1 → RuntimeTyped (some m1) second m2 eff2 →
      RuntimeTyped gamma (.seq first second) m2 (eff1 ++ eff2)
  | consume : RuntimeTyped gamma value .linear eff →
      RuntimeTyped gamma (.consume value) .copy (eff ++ [.linear])

inductive Eval : Nat → Option Val → Annotated → Nat → List ResId → List ResId → Outcome → Nat → Prop where
  | core : EvalDet env e n minted consumed out next →
      Eval ghost env (.core e) n minted consumed out next
  | ghostLet : Eval (evalGhost ghost g) env body n minted consumed out next →
      Eval ghost env (.ghostLet g body) n minted consumed out next
  | ghostProof : EvalDet none proof 0 ghostMinted ghostConsumed (.val v) ghostNext →
      Eval ghost env body n minted consumed out next →
      Eval ghost env (.ghostProof proof body) n minted consumed out next
  | seqVal : Eval ghost env a n minted1 consumed1 (.val v) next1 →
      Eval ghost (some v) b next1 minted2 consumed2 out next2 →
      Eval ghost env (.seq a b) n (minted1 ++ minted2) (consumed1 ++ consumed2) out next2
  | seqAbort : Eval ghost env a n minted consumed .aborted next →
      Eval ghost env (.seq a b) n minted consumed .aborted next
  | consumeVal : Eval ghost env a n minted consumed (.val (.linHandle r)) next →
      Eval ghost env (.consume a) n minted (consumed ++ [r]) (.val .unit) next
  | consumeAbort : Eval ghost env a n minted consumed .aborted next →
      Eval ghost env (.consume a) n minted consumed .aborted next
  | observeGhost : Eval ghost env .observeGhost n [] [] (.val (.nat ghost)) n
  | ghostAbort : Eval ghost env (.ghostAbort body) n [] [] .aborted n

theorem erasure_preserves_typing (typed : RuntimeTyped gamma source m eff) :
    HasType gamma (erase source) m eff := by
  induction typed with
  | core h => exact h
  | ghostLet _ _ ih => exact ih
  | ghostProof _ _ ih => exact ih
  | consume _ ih => exact .tConsume ih
  | seq _ _ ih1 ih2 =>
    have split : ∀ g, ModeSplit g g none := by
      intro g
      cases g with
      | none => exact .none
      | some m => exact .leftOnly m
    exact .tLet (split _) ih1 (by intro _ h; cases h) rfl ih2

-- Both directions preserve the *entire* result and minted/consumed trace.
theorem erasure_equivalence (typed : RuntimeTyped gamma source m eff) :
    ∀ ghost env n minted consumed out next,
      Eval ghost env source n minted consumed out next ↔
      EvalDet env (erase source) n minted consumed out next := by
  induction typed with
  | core _ =>
    intro ghost env n minted consumed out next
    constructor
    · intro h; cases h with | core ev => exact ev
    · exact Eval.core
  | ghostLet g _ ih =>
    intro ghost env n minted consumed out next
    constructor
    · intro h; cases h with
      | ghostLet ev => exact (ih _ _ _ _ _ _ _).mp ev
    · intro h
      exact .ghostLet ((ih _ _ _ _ _ _ _).mpr h)
  | ghostProof well _ ih =>
    intro ghost env n minted consumed out next
    constructor
    · intro h; cases h with
      | ghostProof _ body => exact (ih _ _ _ _ _ _ _).mp body
    · intro h
      obtain ⟨gm, gc, v, gn, proof, _⟩ := ghost_proof_terminates well
      exact .ghostProof proof ((ih _ _ _ _ _ _ _).mpr h)
  | consume _ ih =>
    intro ghost env n minted consumed out next
    constructor
    · intro h; cases h with
      | consumeVal ev => exact .evConsumeVal ((ih _ _ _ _ _ _ _).mp ev)
      | consumeAbort ev => exact .evConsumeAbort ((ih _ _ _ _ _ _ _).mp ev)
    · intro h; cases h with
      | evConsumeVal ev => exact .consumeVal ((ih _ _ _ _ _ _ _).mpr ev)
      | evConsumeAbort ev => exact .consumeAbort ((ih _ _ _ _ _ _ _).mpr ev)
  | seq _ _ ih1 ih2 =>
    intro ghost env n minted consumed out next
    constructor
    · intro h; cases h with
      | seqVal a b => exact .evLetVal ((ih1 _ _ _ _ _ _ _).mp a) ((ih2 _ _ _ _ _ _ _).mp b)
      | seqAbort a => exact .evLetAbort ((ih1 _ _ _ _ _ _ _).mp a)
    · intro h; cases h with
      | evLetVal a b => exact .seqVal ((ih1 _ _ _ _ _ _ _).mpr a) ((ih2 _ _ _ _ _ _ _).mpr b)
      | evLetAbort a => exact .seqAbort ((ih1 _ _ _ _ _ _ _).mpr a)

theorem ghost_noninterference (typed : RuntimeTyped gamma source m eff)
    (g1 g2 : Nat) (env : Option Val) (n : Nat) (minted consumed : List ResId)
    (out : Outcome) (next : Nat) :
    Eval g1 env source n minted consumed out next ↔
    Eval g2 env source n minted consumed out next :=
  (erasure_equivalence typed g1 env n minted consumed out next).trans
    (erasure_equivalence typed g2 env n minted consumed out next).symm

theorem annotated_closed_exact_once (typed : RuntimeTyped none source .copy eff)
    (ghost n : Nat) :
    ∃ minted consumed out next,
      Eval ghost none source n minted consumed out next ∧
      (∀ v, out = .val v → ValTyped v .copy) ∧
      minted.Nodup ∧
      (∀ resource, minted.count resource = consumed.count resource) ∧
      (∀ resource, resource ∈ minted → consumed.count resource = 1) := by
  obtain ⟨minted, consumed, out, next, ev, rest⟩ :=
    closed_exact_once (erasure_preserves_typing typed) n
  exact ⟨minted, consumed, out, next,
    (erasure_equivalence typed ghost none n minted consumed out next).mpr ev, rest⟩

theorem runtime_observation_rejected : ¬ RuntimeTyped gamma .observeGhost m eff := by
  intro h; cases h

theorem ghost_effect_rejected : ¬ RuntimeTyped gamma (.ghostAbort body) m eff := by
  intro h; cases h

-- Erasing an effectful ghost expression would change a terminating result.
theorem erased_abort_changes_behavior :
    Eval 0 none (.ghostAbort (.core (.litNat 42))) 0 [] [] .aborted 0 ∧
    EvalDet none (erase (.ghostAbort (.core (.litNat 42)))) 0 [] [] (.val (.nat 42)) 0 :=
  ⟨.ghostAbort, .evLit none 42 0⟩

-- This is a typed example with runtime allocation and consumption surrounding
-- ghost computation; it is not merely erasure of a list of ghost bindings.
def exampleProgram : Annotated :=
  .seq (.core .mintLinear)
    (.ghostLet (.add .variable (.value 1)) (.consume (.core .var)))

theorem example_program_typed : RuntimeTyped none exampleProgram .copy [.linear, .linear] :=
  .seq (.core (.tMint (gamma := none) trivial)) (.ghostLet _ (.consume (.core .tVar)))

theorem ghost_resource_leak_rejected :
    ¬ GhostProofWellTyped (.letSeq .mintLinear (.litNat 0)) := by
  intro ⟨eff, typed, _⟩
  exact mint_then_leak_rejected eff typed

theorem ghost_failure_rejected : ¬ GhostProofWellTyped .raiseFail := by
  intro ⟨eff, typed, pure⟩
  cases typed
  exact pure (by simp)

theorem resourceful_ghost_proof_accepted : GhostProofWellTyped mintConsume :=
  ⟨[.linear, .linear], mint_consume_has_type, by simp⟩

end Vesper.GhostErasure

#print axioms Vesper.GhostErasure.erasure_preserves_typing
#print axioms Vesper.GhostErasure.erasure_equivalence
#print axioms Vesper.GhostErasure.ghost_noninterference
#print axioms Vesper.GhostErasure.annotated_closed_exact_once
#print axioms Vesper.GhostErasure.runtime_observation_rejected
#print axioms Vesper.GhostErasure.ghost_effect_rejected
#print axioms Vesper.GhostErasure.erased_abort_changes_behavior
#print axioms Vesper.GhostErasure.example_program_typed

#print axioms Vesper.GhostErasure.ghost_proof_terminates
#print axioms Vesper.GhostErasure.ghost_resource_leak_rejected
#print axioms Vesper.GhostErasure.ghost_failure_rejected
#print axioms Vesper.GhostErasure.resourceful_ghost_proof_accepted
