-- Appendix A model from the refreshed §41 design, repaired for Lean 4.24.
-- This is the source's small one-binding calculus, not the production compiler.
-- tLet forces an empty residual frame; the final theorem counts resources.
-- Definitions/statements and the approved compKind ≤ overrideMode premise stay intact.
namespace Vesper.Metatheory
inductive Mode : Type where
  | copy   : Mode
  | affine : Mode
  | linear : Mode
  deriving DecidableEq, Repr
namespace Mode
@[simp] def rank : Mode -> Nat
  | .copy   => 0
  | .affine => 1
  | .linear => 2
@[simp] def Le (m1 m2 : Mode) : Prop := m1.rank <= m2.rank
instance (m1 m2 : Mode) : Decidable (Le m1 m2) :=
  inferInstanceAs (Decidable (m1.rank <= m2.rank))
@[simp] def join (m1 m2 : Mode) : Mode :=
  match m1, m2 with
  | .linear, _ | _, .linear => .linear
  | .affine, _ | _, .affine => .affine
  | .copy, .copy            => .copy
@[simp] def meet (m1 m2 : Mode) : Mode :=
  match m1, m2 with
  | .copy, _ | _, .copy     => .copy
  | .affine, _ | _, .affine => .affine
  | .linear, .linear        => .linear
@[simp] def isDiscardable (m : Mode) : Bool :=
  match m with
  | .copy | .affine => true
  | .linear         => false
@[simp] def allowsCount (m : Mode) (n : Nat) : Prop :=
  match m with
  | .copy   => True
  | .affine => n <= 1
  | .linear => n = 1
theorem le_refl (m : Mode) : Le m m := Nat.le_refl _
theorem le_trans {a b c : Mode} (hab : Le a b) (hbc : Le b c) : Le a c :=
  Nat.le_trans hab hbc
theorem le_antisymm {a b : Mode} (hab : Le a b) (hba : Le b a) : a = b := by
  cases a <;> cases b <;> simp [Le] at hab hba <;> rfl
theorem le_join_left (a b : Mode) : Le a (join a b) := by
  cases a <;> cases b <;> decide
theorem meet_le_left (a b : Mode) : Le (meet a b) a := by
  cases a <;> cases b <;> decide
theorem meet_le_right (a b : Mode) : Le (meet a b) b := by
  cases a <;> cases b <;> decide
theorem mode_submode_allows_count {m1 m2 : Mode} {n : Nat}
    (h_sub : Le m1 m2) (h_use : m2.allowsCount n) :
    m1.allowsCount n := by
  cases m1 <;> cases m2 <;> simp [Le, allowsCount] at h_sub h_use |- <;> omega
theorem linear_not_discardable : Mode.linear.isDiscardable = false := rfl
theorem discardable_iff_le_affine (m : Mode) :
    m.isDiscardable = true <-> Le m .affine := by
  cases m <;> decide
end Mode
structure AdtFieldSpec where
  compKind     : Mode
  overrideMode : Mode := .linear
  deriving DecidableEq, Repr
@[simp] def FieldModalityWf (f : AdtFieldSpec) : Prop :=
  Mode.Le f.compKind f.overrideMode
@[simp] def adtKind (mDecl : Mode) (fields : List AdtFieldSpec) : Mode :=
  fields.foldl (fun acc f => Mode.join acc (Mode.meet f.compKind f.overrideMode)) mDecl
theorem wf_field_meet_eq_comp (f : AdtFieldSpec) (hwf : FieldModalityWf f) :
    Mode.meet f.compKind f.overrideMode = f.compKind := by
  rcases f with ⟨k, m⟩
  cases k <;> cases m <;> simp [FieldModalityWf, Mode.Le] at hwf <;> rfl
theorem foldl_join_ge_init (init : Mode) (fs : List AdtFieldSpec) :
    Mode.Le init (fs.foldl (fun acc f => Mode.join acc (Mode.meet f.compKind f.overrideMode)) init) := by
  induction fs generalizing init with
  | nil => exact Mode.le_refl init
  | cons hd tl ih =>
    have h1 := Mode.le_join_left init (Mode.meet hd.compKind hd.overrideMode)
    exact Mode.le_trans h1 (ih _)
theorem adt_kind_never_relaxes_linear_component
    (mDecl : Mode) (fields : List AdtFieldSpec)
    (h_wf : forall f, f ∈ fields -> FieldModalityWf f)
    (fLin : AdtFieldSpec) (h_mem : fLin ∈ fields) (h_lin : fLin.compKind = .linear) :
    adtKind mDecl fields = .linear := by
  unfold adtKind
  induction fields generalizing mDecl with
  | nil => cases h_mem
  | cons hd tl ih =>
    simp only [List.mem_cons] at h_mem
    rcases h_mem with rfl | h_in_tl
    · have h_hd_wf : FieldModalityWf fLin := h_wf fLin (List.mem_cons_self)
      have h_meet : Mode.meet fLin.compKind fLin.overrideMode = .linear := by
        rw [wf_field_meet_eq_comp fLin h_hd_wf, h_lin]
      simp only [List.foldl_cons, h_meet]
      have h_join : Mode.join mDecl .linear = .linear := by cases mDecl <;> rfl
      rw [h_join]
      have h_ge := foldl_join_ge_init .linear tl
      exact Mode.le_antisymm
        (by cases tl.foldl (fun acc f => Mode.join acc (Mode.meet f.compKind f.overrideMode)) .linear <;> decide)
        h_ge
    · apply ih (Mode.join mDecl (Mode.meet hd.compKind hd.overrideMode))
      · intro f hf
        exact h_wf f (List.mem_cons_of_mem hd hf)
      · exact h_in_tl
inductive ResumptionMode : Type where
  | affine : ResumptionMode
  | linear : ResumptionMode
  deriving DecidableEq, Repr
@[simp] def ResumptionMode.toMode : ResumptionMode -> Mode
  | .affine => .affine
  | .linear => .linear
abbrev EffectRow := List ResumptionMode
@[simp] def EffectRow.res : EffectRow -> Mode
  | []      => .linear
  | q :: qs => Mode.meet q.toMode (EffectRow.res qs)
@[simp] def FrameSafe (liveModes : List Mode) (eff1 : EffectRow) : Prop :=
  forall m, m ∈ liveModes -> Mode.Le m eff1.res
theorem res_le_affine_of_mem_affine {eff : EffectRow} (h : .affine ∈ eff) :
    Mode.Le eff.res .affine := by
  induction eff with
  | nil => cases h
  | cons hd tl ih =>
    simp only [List.mem_cons] at h
    rcases h with rfl | h_tl
    · exact Mode.meet_le_left .affine (EffectRow.res tl)
    · exact Mode.le_trans (Mode.meet_le_right hd.toMode (EffectRow.res tl)) (ih h_tl)
theorem abort_soundness_preserves_linear
    {liveModes : List Mode} {eff1 : EffectRow}
    (h_frame : FrameSafe liveModes eff1)
    (h_abortable : .affine ∈ eff1) :
    forall m, m ∈ liveModes -> m.isDiscardable = true := by
  intro m hm
  rw [Mode.discardable_iff_le_affine]
  exact Mode.le_trans (h_frame m hm) (res_le_affine_of_mem_affine h_abortable)
theorem uncancellable_io_allows_live_linear
    (liveModes : List Mode) (eff1 : EffectRow)
    (h_all_linear : forall q, q ∈ eff1 -> q = .linear) :
    FrameSafe liveModes eff1 := by
  have h_res : eff1.res = .linear := by
    induction eff1 with
    | nil => rfl
    | cons hd tl ih =>
      have h_hd : hd = .linear := h_all_linear hd (List.mem_cons_self)
      subst h_hd
      have h_tl : (EffectRow.res tl) = .linear := ih (fun q hq => h_all_linear q (List.mem_cons_of_mem _ hq))
      simp [EffectRow.res, h_tl]
  intro m _
  rw [h_res]
  cases m <;> decide
theorem unconsumed_txn_across_question_rejected :
    ¬ FrameSafe [.linear] [.affine] := by
  intro h
  have h_lin := h .linear (List.mem_singleton.mpr rfl)
  simp [EffectRow.res, Mode.Le] at h_lin
abbrev LocalId  := Nat
abbrev FieldIdx := Nat
abbrev LeafPath := List FieldIdx
@[simp] def PathPrefix : LeafPath -> LeafPath -> Prop
  | [], _             => True
  | _ :: _, []        => False
  | f1 :: t1, f2 :: t2 => f1 = f2 /\ PathPrefix t1 t2
structure ProjSelector where
  localVar : LocalId
  branches : List LeafPath
  deriving DecidableEq, Repr
@[simp] def ProjSelector.covers (sel : ProjSelector) (loc : LocalId) (leaf : LeafPath) : Prop :=
  sel.localVar = loc /\ exists b, b ∈ sel.branches /\ PathPrefix b leaf
@[simp] def ProjSelector.Disjoint (s1 s2 : ProjSelector) : Prop :=
  forall loc leaf, ¬ (s1.covers loc leaf /\ s2.covers loc leaf)
@[simp] def ProjSelector.Subset (sNew sOld : ProjSelector) : Prop :=
  forall loc leaf, sNew.covers loc leaf -> sOld.covers loc leaf
theorem semver_narrowing_preserves_disjoint
    {sNew sOld sOther : ProjSelector}
    (h_narrow : sNew.Subset sOld)
    (h_disj   : sOld.Disjoint sOther) :
    sNew.Disjoint sOther := by
  intro loc leaf ⟨h_new, h_other⟩
  exact h_disj loc leaf ⟨h_narrow loc leaf h_new, h_other⟩
inductive PassMode : Type where
  | sharedIn : PassMode
  | inOut    : PassMode
  | sinkOwn  : PassMode
  deriving DecidableEq, Repr
structure CallArg where
  mode : PassMode
  proj : ProjSelector
  deriving DecidableEq, Repr
def CallProjValid (args : List CallArg) : Prop :=
  forall (i j : Fin args.length), i != j ->
    ((args.get i).mode = .sharedIn /\ (args.get j).mode = .sharedIn) \/
    ProjSelector.Disjoint (args.get i).proj (args.get j).proj
theorem call_proj_axm_exclusivity
    {args : List CallArg} (h_valid : CallProjValid args)
    (i j : Fin args.length) (h_neq : i != j)
    (h_mut_or_sink : (args.get i).mode != .sharedIn)
    {loc : LocalId} {leaf : LeafPath}
    (h_cov_i : (args.get i).proj.covers loc leaf) :
    ¬ (args.get j).proj.covers loc leaf := by
  intro h_cov_j
  rcases h_valid i j h_neq with ⟨h_in_i, _⟩ | h_disj
  · exact absurd h_in_i (bne_iff_ne.mp h_mut_or_sink)
  · exact h_disj loc leaf ⟨h_cov_i, h_cov_j⟩
inductive CapState : Type where
  | liveOwned        (m : Mode)
  | consumed
  | poisonedRecovery
  deriving DecidableEq, Repr
def pcgJoinLocal (lhs rhs : CapState) : Option CapState :=
  match lhs, rhs with
  | .poisonedRecovery, _ | _, .poisonedRecovery => some .poisonedRecovery
  | .consumed, .consumed                        => some .consumed
  | .liveOwned m1, .liveOwned m2                =>
    if m1 = m2 then some (.liveOwned m1) else none
  | .liveOwned m, .consumed | .consumed, .liveOwned m =>
    if m.isDiscardable then some .consumed else none
@[simp] def pcgReturnOk : CapState -> Bool
  | .liveOwned .linear => false
  | .liveOwned .copy | .liveOwned .affine | .consumed | .poisonedRecovery => true
theorem pcg_join_preserves_linear_agreement
    {lhs rhs merged : CapState}
    (h_no_poison_l : lhs != .poisonedRecovery)
    (h_no_poison_r : rhs != .poisonedRecovery)
    (h_join : pcgJoinLocal lhs rhs = some merged) :
    (lhs = .liveOwned .linear <-> rhs = .liveOwned .linear) := by
  cases lhs <;> cases rhs <;> simp [pcgJoinLocal, Mode.isDiscardable] at h_no_poison_l h_no_poison_r h_join |-
  case liveOwned.liveOwned m1 m2 =>
    rcases h_join with ⟨rfl, _⟩
    rfl
  case liveOwned.consumed m => cases m <;> simp_all
  case consumed.liveOwned m => cases m <;> simp_all
abbrev ResId := Nat
inductive Val : Type where
  | unit      : Val
  | nat       : Nat -> Val
  | linHandle : ResId -> Val
  | pair      : Val -> Val → Val
  deriving DecidableEq, Repr
@[simp] def Val.linResources : Val -> List ResId
  | .unit        => []
  | .nat _       => []
  | .linHandle r => [r]
  | .pair v1 v2  => v1.linResources ++ v2.linResources
def ValTyped (v : Val) : Mode -> Prop
  | .copy   => v.linResources = []
  | .affine => v.linResources = []
  | .linear => True
inductive ModeSplit : Option Mode -> Option Mode -> Option Mode -> Prop where
  | none      : ModeSplit none none none
  | copyBoth  : ModeSplit (some .copy) (some .copy) (some .copy)
  | leftOnly  (m : Mode) : ModeSplit (some m) (some m) none
  | rightOnly (m : Mode) : ModeSplit (some m) none (some m)
@[simp] def OptEnvDiscardable : Option Mode -> Prop
  | none   => True
  | some m => Mode.Le m .affine
inductive Expr : Type where
  | var        : Expr
  | litNat     : Nat -> Expr
  | mintLinear : Expr
  | consumeLin : Expr -> Expr
  | raiseFail  : Expr
  | letSeq     : Expr -> Expr -> Expr
inductive HasType : Option Mode -> Expr -> Mode -> EffectRow -> Prop where
  | tVar {m : Mode} :
      HasType (some m) .var m []
  | tLit {gamma : Option Mode} (n : Nat) (h_disc : OptEnvDiscardable gamma) :
      HasType gamma (.litNat n) .copy []
  | tMint {gamma : Option Mode} (h_disc : OptEnvDiscardable gamma) :
      HasType gamma .mintLinear .linear [.linear]
  | tConsume {gamma : Option Mode} {e : Expr} {eff : EffectRow}
      (h_e : HasType gamma e .linear eff) :
      HasType gamma (.consumeLin e) .copy (eff ++ [.linear])
  | tFail {gamma : Option Mode} (h_disc : OptEnvDiscardable gamma) :
      HasType gamma .raiseFail .copy [.affine]
  | tLet {gamma gamma1 gamma2 : Option Mode} {e1 e2 : Expr} {m1 m2 : Mode} {eff1 eff2 : EffectRow}
      (h_split  : ModeSplit gamma gamma1 gamma2)
      (h_e1     : HasType gamma1 e1 m1 eff1)
      (h_frame  : forall m_z, gamma2 = some m_z -> Mode.Le m_z eff1.res)
      (h_z_none : gamma2 = none)
      (h_e2     : HasType (some m1) e2 m2 eff2) :
      HasType gamma (.letSeq e1 e2) m2 (eff1 ++ eff2)
inductive Outcome : Type where
  | val     : Val -> Outcome
  | aborted : Outcome
  deriving DecidableEq, Repr
inductive EvalDet : Option Val -> Expr -> Nat -> List ResId -> List ResId -> Outcome -> Nat -> Prop where
  | evVar (v : Val) (n : Nat) :
      EvalDet (some v) .var n [] [] (.val v) n
  | evLit (env : Option Val) (k n : Nat) :
      EvalDet env (.litNat k) n [] [] (.val (.nat k)) n
  | evMint (env : Option Val) (n : Nat) :
      EvalDet env .mintLinear n [n] [] (.val (.linHandle n)) (n + 1)
  | evConsumeVal {env : Option Val} {e : Expr} {n n' : Nat} {m c : List ResId} {r : ResId}
      (h : EvalDet env e n m c (.val (.linHandle r)) n') :
      EvalDet env (.consumeLin e) n m (c ++ [r]) (.val .unit) n'
  | evConsumeAbort {env : Option Val} {e : Expr} {n n' : Nat} {m c : List ResId}
      (h : EvalDet env e n m c .aborted n') :
      EvalDet env (.consumeLin e) n m c .aborted n'
  | evFail (env : Option Val) (n : Nat) :
      EvalDet env .raiseFail n [] [] .aborted n
  | evLetVal {env : Option Val} {e1 e2 : Expr} {n n1 n2 : Nat}
      {m1 c1 m2 c2 : List ResId} {v1 : Val} {out2 : Outcome}
      (h1 : EvalDet env e1 n m1 c1 (.val v1) n1)
      (h2 : EvalDet (some v1) e2 n1 m2 c2 out2 n2) :
      EvalDet env (.letSeq e1 e2) n (m1 ++ m2) (c1 ++ c2) out2 n2
  | evLetAbort {env : Option Val} {e1 e2 : Expr} {n n1 : Nat} {m1 c1 : List ResId}
      (h1 : EvalDet env e1 n m1 c1 .aborted n1) :
      EvalDet env (.letSeq e1 e2) n m1 c1 .aborted n1
def EnvTyped : Option Val -> Option Mode -> Prop
  | none, none     => True
  | some v, some m => ValTyped v m
  | _, _           => False
@[simp] def EnvResources : Option Val -> List ResId
  | none   => []
  | some v => v.linResources
@[simp] def OutcomeResources : Outcome -> List ResId
  | .val v   => v.linResources
  | .aborted => []
theorem discardable_env_has_no_linear_resources
    {env : Option Val} {gamma : Option Mode}
    (h_ty : EnvTyped env gamma) (h_disc : OptEnvDiscardable gamma) :
    EnvResources env = [] := by
  cases env <;> cases gamma <;> simp [EnvTyped, OptEnvDiscardable, Mode.Le] at h_ty h_disc |-
  rename_i v m
  cases m <;> simp at h_disc <;> exact h_ty
theorem vesper_linear_conservation_soundness
    {env : Option Val} {gamma : Option Mode} {e : Expr} {m : Mode} {eff : EffectRow}
    {n n' : Nat} {minted consumed : List ResId} {out : Outcome}
    (h_type : HasType gamma e m eff)
    (h_env  : EnvTyped env gamma)
    (h_eval : EvalDet env e n minted consumed out n') :
    (forall v, out = .val v -> ValTyped v m) /\
    (EnvResources env).length + minted.length =
      consumed.length + (OutcomeResources out).length := by
  induction h_type generalizing env n n' minted consumed out with
  | tVar =>
    cases h_eval with
    | evVar v _ =>
      refine ⟨?_, by simp⟩
      intro v' hv
      cases hv
      exact h_env
  | tLit k h_disc =>
    cases h_eval with
    | evLit _ _ _ =>
      have h_empty := discardable_env_has_no_linear_resources h_env h_disc
      constructor
      · simp [ValTyped]
      · rw [h_empty]
        simp
  | tMint h_disc =>
    cases h_eval with
    | evMint _ _ =>
      have h_empty := discardable_env_has_no_linear_resources h_env h_disc
      constructor
      · simp [ValTyped]
      · rw [h_empty]
        simp
  | tConsume _ ih =>
    cases h_eval with
    | evConsumeVal h_sub =>
      obtain ⟨_, h_len⟩ := ih h_env h_sub
      simp [ValTyped] at h_len |-
      omega
    | evConsumeAbort h_sub =>
      obtain ⟨_, h_len⟩ := ih h_env h_sub
      constructor
      · intro v h
        cases h
      · exact h_len
  | tFail h_disc =>
    cases h_eval with
    | evFail _ _ =>
      have h_empty := discardable_env_has_no_linear_resources h_env h_disc
      constructor
      · intro v h
        cases h
      · rw [h_empty]
        simp
  | tLet h_split _ _ h_z_none _ ih1 ih2 =>
    subst h_z_none
    have split_empty : ∀ {a b : Option Mode}, ModeSplit a b none → a = b := by
      intro a b h
      cases h <;> rfl
    have h_eq_gamma := split_empty h_split
    subst h_eq_gamma
    cases h_eval with
    | evLetVal h_ev1 h_ev2 =>
      obtain ⟨h_v1_ty, h_len1⟩ := ih1 h_env h_ev1
      obtain ⟨h_out2_ty, h_len2⟩ := ih2 (env := some _) (h_v1_ty _ rfl) h_ev2
      refine ⟨h_out2_ty, ?_⟩
      simp [EnvResources, OutcomeResources] at h_len1 h_len2 |-
      omega
    | evLetAbort h_ev1 =>
      obtain ⟨_, h_len1⟩ := ih1 h_env h_ev1
      constructor
      · intro v h
        cases h
      · exact h_len1
theorem vesper_total_linear_exhaustion
    {e : Expr} {eff : EffectRow} {n n' : Nat}
    {minted consumed : List ResId} {out : Outcome}
    (h_closed_ty : HasType none e .copy eff)
    (h_eval      : EvalDet none e n minted consumed out n') :
    minted.length = consumed.length := by
  obtain ⟨h_val_ty, h_cons⟩ :=
    vesper_linear_conservation_soundness (env := none) h_closed_ty trivial h_eval
  cases out with
  | aborted =>
    simp [EnvResources, OutcomeResources] at h_cons
    exact h_cons
  | val v =>
    have h_no_res : v.linResources = [] := h_val_ty v rfl
    simp [EnvResources, OutcomeResources, h_no_res] at h_cons
    exact h_cons

-- Concrete applicability checks. These extend the examples only; none of the
-- Appendix definitions or theorem statements above are weakened.
def mintConsume : Expr := .letSeq .mintLinear (.consumeLin .var)

theorem mint_consume_has_type :
    HasType none mintConsume .copy [.linear, .linear] := by
  exact HasType.tLet (gamma1 := none) (gamma2 := none)
    ModeSplit.none (HasType.tMint trivial)
    (by intro m h; cases h) rfl (HasType.tConsume HasType.tVar)

theorem mint_consume_evaluates (n : Nat) :
    EvalDet none mintConsume n [n] [n] (.val .unit) (n + 1) := by
  exact EvalDet.evLetVal (EvalDet.evMint none n)
    (EvalDet.evConsumeVal (EvalDet.evVar (.linHandle n) (n + 1)))

theorem mint_consume_exhaustion (n : Nat) :
    ([n] : List ResId).length = ([n] : List ResId).length := by
  exact vesper_total_linear_exhaustion mint_consume_has_type (mint_consume_evaluates n)

-- Operational derivations alone allow a leak/abort; the corresponding typing
-- derivations are rejected. This also separates evaluation from progress.
theorem mint_then_leak_evaluates (n : Nat) :
    EvalDet none (.letSeq .mintLinear (.litNat 0)) n [n] [] (.val (.nat 0)) (n + 1) := by
  exact EvalDet.evLetVal (EvalDet.evMint none n)
    (EvalDet.evLit (some (.linHandle n)) 0 (n + 1))

theorem mint_then_leak_rejected (eff : EffectRow) :
    ¬ HasType none (.letSeq .mintLinear (.litNat 0)) .copy eff := by
  intro h
  cases h with
  | tLet _ h_mint _ _ h_body =>
    cases h_mint
    cases h_body with
    | tLit _ h_disc => simp [OptEnvDiscardable, Mode.Le] at h_disc

theorem mint_then_abort_evaluates (n : Nat) :
    EvalDet none (.letSeq .mintLinear .raiseFail) n [n] [] .aborted (n + 1) := by
  exact EvalDet.evLetVal (EvalDet.evMint none n)
    (EvalDet.evFail (some (.linHandle n)) (n + 1))

theorem mint_then_abort_rejected (eff : EffectRow) :
    ¬ HasType none (.letSeq .mintLinear .raiseFail) .copy eff := by
  intro h
  cases h with
  | tLet _ h_mint _ _ h_body =>
    cases h_mint
    cases h_body with
    | tFail h_disc => simp [OptEnvDiscardable, Mode.Le] at h_disc

end Vesper.Metatheory

-- Kernel axiom audit: every named source theorem and applicability example.
#print axioms Vesper.Metatheory.Mode.le_refl
#print axioms Vesper.Metatheory.Mode.le_trans
#print axioms Vesper.Metatheory.Mode.le_antisymm
#print axioms Vesper.Metatheory.Mode.le_join_left
#print axioms Vesper.Metatheory.Mode.meet_le_left
#print axioms Vesper.Metatheory.Mode.meet_le_right
#print axioms Vesper.Metatheory.Mode.mode_submode_allows_count
#print axioms Vesper.Metatheory.Mode.linear_not_discardable
#print axioms Vesper.Metatheory.Mode.discardable_iff_le_affine
#print axioms Vesper.Metatheory.wf_field_meet_eq_comp
#print axioms Vesper.Metatheory.foldl_join_ge_init
#print axioms Vesper.Metatheory.adt_kind_never_relaxes_linear_component
#print axioms Vesper.Metatheory.res_le_affine_of_mem_affine
#print axioms Vesper.Metatheory.abort_soundness_preserves_linear
#print axioms Vesper.Metatheory.uncancellable_io_allows_live_linear
#print axioms Vesper.Metatheory.unconsumed_txn_across_question_rejected
#print axioms Vesper.Metatheory.semver_narrowing_preserves_disjoint
#print axioms Vesper.Metatheory.call_proj_axm_exclusivity
#print axioms Vesper.Metatheory.pcg_join_preserves_linear_agreement
#print axioms Vesper.Metatheory.discardable_env_has_no_linear_resources
#print axioms Vesper.Metatheory.vesper_linear_conservation_soundness
#print axioms Vesper.Metatheory.vesper_total_linear_exhaustion
#print axioms Vesper.Metatheory.mint_consume_has_type
#print axioms Vesper.Metatheory.mint_consume_evaluates
#print axioms Vesper.Metatheory.mint_consume_exhaustion
#print axioms Vesper.Metatheory.mint_then_leak_evaluates
#print axioms Vesper.Metatheory.mint_then_leak_rejected
#print axioms Vesper.Metatheory.mint_then_abort_evaluates
#print axioms Vesper.Metatheory.mint_then_abort_rejected
