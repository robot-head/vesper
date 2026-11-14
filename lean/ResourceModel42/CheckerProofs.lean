import ResourceModel42.Checker

/-! Constructive finite-checker soundness in the existing Full calculus.
No caller typing premise is accepted. Every named theorem is public/audited. -/
namespace Vesper.ResourceModel42.CheckerProofs

theorem sourceAtom_isAtom (a : SourceAtom) : MetatheoryFull.IsAtom a.erase := by
  cases a with
  | unit _ => exact .val .unitV
  | nat _ n => exact .val (.natV n)
  | var _ i => exact .var i

theorem split_right_all (context : MetatheoryFull.Ctx) :
    MetatheoryFull.Split context MetatheoryFull.Ctx.empty context := by
  intro i
  cases h : context i with
  | none => exact .none
  | some binding => exact .right binding

theorem frame_empty_row (Sig : Nat → MetatheoryFull.EffSig)
    (context : MetatheoryFull.Ctx) (resources : MetatheoryFull.RCtx) :
    MetatheoryFull.FrameOK context resources (MetatheoryFull.res Sig []) := by
  exact ⟨fun _ _ mode _ => MetatheoryFull.Mode.le_linear mode,
    fun _ mode _ => MetatheoryFull.Mode.le_linear mode.toMode⟩

def demand (uses : List Bool) (index : Nat) : Bool := (uses[index]?).getD false

/-- The whole finite declared context, not only its minimal demand mask:
unused copy/affine entries may remain, but every mode is the type's kind. -/
def ContextFits (context : List TypeInfo) (uses : List Bool) (full : MetatheoryFull.Ctx) : Prop :=
  ∀ i, match context[i]? with
    | none => full i = none
    | some info =>
      if demand uses i then full i = some (info.ty.erase, info.ty.kind)
      else if info.ty.kind = .linear then full i = none
      else full i = none ∨ full i = some (info.ty.erase, info.ty.kind)

theorem contextFits_demand {context uses full i info}
    (h : ContextFits context uses full) (hc : context[i]? = some info)
    (hu : demand uses i = true) : full i = some (info.ty.erase, info.ty.kind) := by
  simpa [ContextFits, hc, hu] using h i

theorem contextFits_absent {context uses full i}
    (h : ContextFits context uses full) (hc : context[i]? = none) : full i = none := by
  simpa [ContextFits, hc] using h i

theorem mode_discardable_of_ne_linear (mode : Mode) (h : mode ≠ .linear) :
    MetatheoryFull.Mode.Le mode .affine := by
  cases mode <;> simp_all [MetatheoryFull.Mode.Le, MetatheoryFull.Mode.rank]


theorem demand_zero (uses : Usage) : demand uses 0 = uses.headD false := by
  cases uses <;> rfl

theorem demand_succ (uses : Usage) (i : Nat) : demand uses (i+1) = demand uses.tail i := by
  cases uses <;> rfl

theorem zeroUsage_demand (context : Context) (i : Nat) : demand (zeroUsage context) i = false := by
  induction context generalizing i with
  | nil => rfl
  | cons info rest ih =>
    cases i with
    | zero => rfl
    | succ i => exact ih i

theorem singletonUsage_demand (context : Context) (index i : Nat) :
    demand (singletonUsage context index) i = decide (i = index ∧ i < context.length) := by
  induction context generalizing index i with
  | nil => simp [singletonUsage, demand]
  | cons info rest ih =>
    cases index with
    | zero =>
      cases i with
      | zero => simp [singletonUsage, demand]
      | succ i => simp [singletonUsage, demand_succ, zeroUsage_demand]
    | succ index =>
      cases i with
      | zero => simp [singletonUsage, demand]
      | succ i => simpa [singletonUsage, demand_succ] using ih index i

theorem usageUnion_demand {context : Context} (left right : Usage) {i : Nat} {info : TypeInfo}
    (hc : context[i]? = some info) :
    demand (usageUnion context left right) i = (demand left i || demand right i) := by
  induction context generalizing left right i with
  | nil => cases hc
  | cons head rest ih =>
    cases i with
    | zero => simp [usageUnion, demand_zero]
    | succ i =>
      change rest[i]? = some info at hc
      simpa only [usageUnion, demand_succ, List.tail_cons] using ih left.tail right.tail hc

def NoncopyDisjoint (context : Context) (left right : Usage) : Prop :=
  ∀ i info, context[i]? = some info → info.ty.kind ≠ .copy →
    ¬ (demand left i = true ∧ demand right i = true)

theorem firstOverlap_none_disjoint {context : Context} {left right : Usage} {base : Nat}
    (h : firstOverlap context left right base = none) : NoncopyDisjoint context left right := by
  induction context generalizing left right base with
  | nil => intro i info hi; cases hi
  | cons head rest ih =>
    intro i info hi hk hlr
    cases i with
    | zero =>
      have he : head = info := Option.some.inj hi
      subst info
      cases left <;> cases right <;> simp_all [firstOverlap, demand]
    | succ i =>
      have ht : firstOverlap rest left.tail right.tail (base+1) = none := by
        simp only [firstOverlap] at h
        split at h
        · contradiction
        · exact h
      have hi' : rest[i]? = some info := hi
      exact ih ht i info hi' hk ⟨by simpa [demand_succ] using hlr.1,
        by simpa [demand_succ] using hlr.2⟩

theorem mergeUsage_disjoint {context : Context} {left right : Usage} {site : Site} {uses : Usage}
    (h : mergeUsage context left right site = .ok uses) : NoncopyDisjoint context left right := by
  cases hm : firstOverlap context left right 0 with
  | none => exact firstOverlap_none_disjoint hm
  | some index => simp [mergeUsage, hm, failCheck] at h

theorem mergeUsage_union {context : Context} {left right : Usage} {site : Site} {uses : Usage}
    (h : mergeUsage context left right site = .ok uses) : uses = usageUnion context left right := by
  cases hm : firstOverlap context left right 0 with
  | none => simpa [mergeUsage, hm] using h.symm
  | some index => simp [mergeUsage, hm, failCheck] at h

/-- These deterministic contexts partition noncopy bindings; spare affine
bindings go left, while an available copy binding is duplicated. -/
def splitLeft (context : Context) (right : Usage) (full : MetatheoryFull.Ctx) : MetatheoryFull.Ctx :=
  fun i => match context[i]? with
    | none => none
    | some info => if info.ty.kind = .copy then full i else if demand right i then none else full i

def splitRight (context : Context) (right : Usage) (full : MetatheoryFull.Ctx) : MetatheoryFull.Ctx :=
  fun i => match context[i]? with
    | none => none
    | some info => if info.ty.kind = .copy then full i else if demand right i then full i else none

/-- A single declared local, with its actual kind and optional discardable
extras. This pointwise form makes constructive context splitting explicit. -/
def EntryFits (info : TypeInfo) (used : Bool)
    (entry : Option (MetatheoryFull.Ty × Mode)) : Prop :=
  if used then entry = some (info.ty.erase, info.ty.kind)
  else if info.ty.kind = .linear then entry = none
  else entry = none ∨ entry = some (info.ty.erase, info.ty.kind)

def entryLeft (info : TypeInfo) (right : Bool)
    (entry : Option (MetatheoryFull.Ty × Mode)) : Option (MetatheoryFull.Ty × Mode) :=
  if info.ty.kind = .copy then entry else if right then none else entry

def entryRight (info : TypeInfo) (right : Bool)
    (entry : Option (MetatheoryFull.Ty × Mode)) : Option (MetatheoryFull.Ty × Mode) :=
  if info.ty.kind = .copy then entry else if right then entry else none

theorem entryFits_split (info : TypeInfo) (left right : Bool)
    (entry : Option (MetatheoryFull.Ty × Mode))
    (hf : EntryFits info (left || right) entry)
    (hd : info.ty.kind = .copy ∨ ¬ (left = true ∧ right = true)) :
    MetatheoryFull.SplitE entry (entryLeft info right entry) (entryRight info right entry) ∧
      EntryFits info left (entryLeft info right entry) ∧
      EntryFits info right (entryRight info right entry) := by
  cases hk : info.ty.kind <;> cases left <;> cases right <;>
    try simp_all [EntryFits, entryLeft, entryRight]
  all_goals try (rcases hf with rfl | rfl)
  all_goals try simp_all [EntryFits, entryLeft, entryRight]
  all_goals first
    | exact MetatheoryFull.SplitE.none
    | exact MetatheoryFull.SplitE.left _
    | exact MetatheoryFull.SplitE.right _
    | exact MetatheoryFull.SplitE.copy _

theorem contextFits_entry {context uses full i info}
    (h : ContextFits context uses full) (hc : context[i]? = some info) :
    EntryFits info (demand uses i) (full i) := by
  simpa [ContextFits, EntryFits, hc] using h i

theorem contextFits_split {context : Context} {left right : Usage} {full : MetatheoryFull.Ctx}
    (hf : ContextFits context (usageUnion context left right) full)
    (hd : NoncopyDisjoint context left right) :
    MetatheoryFull.Split full (splitLeft context right full) (splitRight context right full) ∧
      ContextFits context left (splitLeft context right full) ∧
      ContextFits context right (splitRight context right full) := by
  have localSplit (i : Nat) (info : TypeInfo) (hc : context[i]? = some info) := by
    have hi := contextFits_entry hf hc
    rw [usageUnion_demand left right hc] at hi
    have hdis : info.ty.kind = .copy ∨ ¬ (demand left i = true ∧ demand right i = true) := by
      by_cases hk : info.ty.kind = .copy
      · exact Or.inl hk
      · exact Or.inr (hd i info hc hk)
    exact entryFits_split info (demand left i) (demand right i) (full i) hi hdis
  constructor
  · intro i
    cases hc : context[i]? with
    | none =>
      have hn := contextFits_absent hf hc
      simpa [splitLeft, splitRight, hc, hn] using MetatheoryFull.SplitE.none
    | some info =>
      simpa [splitLeft, splitRight, entryLeft, entryRight, hc] using (localSplit i info hc).1
  · constructor
    · intro i
      cases hc : context[i]? with
      | none => simp [splitLeft, hc]
      | some info =>
        simpa [EntryFits, splitLeft, entryLeft, hc] using (localSplit i info hc).2.1
    · intro i
      cases hc : context[i]? with
      | none => simp [splitRight, hc]
      | some info =>
        simpa [EntryFits, splitRight, entryRight, hc] using (localSplit i info hc).2.2

theorem contextFits_full_binding {context uses full i t mode}
    (hf : ContextFits context uses full) (hv : full i = some (t, mode)) :
    ∃ info, context[i]? = some info ∧ t = info.ty.erase ∧ mode = info.ty.kind := by
  cases hc : context[i]? with
  | none => rw [contextFits_absent hf hc] at hv; contradiction
  | some info =>
    have hi := contextFits_entry hf hc
    unfold EntryFits at hi
    split at hi
    · rw [hv] at hi; cases hi; exact ⟨info,rfl,rfl,rfl⟩
    · split at hi
      · rw [hv] at hi; contradiction
      · rcases hi with hn | he
        · rw [hv] at hn; contradiction
        · rw [hv] at he; cases he; exact ⟨info,rfl,rfl,rfl⟩

theorem contextFits_unused_binding {context uses full i t mode}
    (hf : ContextFits context uses full) (hv : full i = some (t, mode))
    (hu : demand uses i = false) : MetatheoryFull.Mode.Le mode .affine := by
  obtain ⟨info,hc,rfl,rfl⟩ := contextFits_full_binding hf hv
  have hi := contextFits_entry hf hc
  have hk : info.ty.kind ≠ .linear := by
    intro hk
    simp [EntryFits,hu,hk,hv] at hi
  exact mode_discardable_of_ne_linear _ hk

theorem contextFits_zero_discard {context full}
    (hf : ContextFits context (zeroUsage context) full) : MetatheoryFull.Discard full := by
  intro i t mode hv
  exact contextFits_unused_binding hf hv (zeroUsage_demand context i)

theorem contextFits_singleton_lookup {context full index info}
    (hf : ContextFits context (singletonUsage context index) full)
    (hc : context[index]? = some info) :
    full index = some (info.ty.erase,info.ty.kind) := by
  have hlt := (List.getElem?_eq_some_iff.mp hc).1
  apply contextFits_demand hf hc
  simp [singletonUsage_demand, hlt]

theorem contextFits_singleton_rest {context full index}
    (hf : ContextFits context (singletonUsage context index) full) :
    ∀ i t mode, i ≠ index → full i = some (t,mode) → MetatheoryFull.Mode.Le mode .affine := by
  intro i t mode hne hv
  apply contextFits_unused_binding hf hv
  simp [singletonUsage_demand,hne]

theorem checkBinderUse_linear {info used site index}
    (h : checkBinderUse info used site index = .ok ())
    (hk : info.ty.kind = .linear) : used = true := by
  cases used <;> simp_all [checkBinderUse,failCheck]

theorem contextFits_extend {context uses full info site index}
    (hf : ContextFits context uses.tail full)
    (hb : checkBinderUse info (uses.headD false) site index = .ok ()) :
    ContextFits (info :: context) uses (full.ext (info.ty.erase,info.ty.kind)) := by
  intro i
  cases i with
  | zero =>
    change EntryFits info (demand uses 0) (some (info.ty.erase,info.ty.kind))
    rw [demand_zero]
    unfold EntryFits
    split
    · rfl
    · split
      · have hu := checkBinderUse_linear hb (by assumption)
        contradiction
      · exact Or.inr rfl
  | succ i => simpa [demand_succ,MetatheoryFull.Ctx.ext] using hf i

theorem usage_tail_tail (uses : Usage) : uses.tail.tail = uses.drop 2 := by
  cases uses with
  | nil => rfl
  | cons head tail => cases tail <;> rfl

theorem firstDemandMismatch_none_equal {mode : Mode} {context : Context}
    {left right : Usage} {base : Nat}
    (h : firstDemandMismatch mode context left right base = none)
    {i : Nat} {info : TypeInfo} (hc : context[i]? = some info)
    (hk : info.ty.kind = mode) : demand left i = demand right i := by
  induction context generalizing left right base i with
  | nil => cases hc
  | cons head rest ih =>
    cases i with
    | zero =>
      have he : head = info := Option.some.inj hc
      subst info
      by_cases heq : demand left 0 = demand right 0
      · exact heq
      · have hdiff : left.headD false ≠ right.headD false := by simpa [demand_zero] using heq
        change (if head.ty.kind = mode ∧ left.headD false ≠ right.headD false
          then some base else firstDemandMismatch mode rest left.tail right.tail (base+1)) = none at h
        rw [if_pos ⟨hk,hdiff⟩] at h
        contradiction
    | succ i =>
      have ht : firstDemandMismatch mode rest left.tail right.tail (base+1) = none := by
        simp only [firstDemandMismatch] at h
        split at h
        · contradiction
        · exact h
      simpa only [demand_succ] using ih ht hc

theorem mergeBranchUsage_properties {context : Context} {left right uses : Usage} {site : Site}
    (h : mergeBranchUsage context left right site = .ok uses) :
    uses = usageUnion context left right ∧
      ∀ i info, context[i]? = some info → info.ty.kind ≠ .copy →
        demand left i = demand right i := by
  cases hl : firstDemandMismatch .linear context left right 0 with
  | some index => simp [mergeBranchUsage,hl,failCheck] at h
  | none =>
    cases ha : firstDemandMismatch .affine context left right 0 with
    | some index => simp [mergeBranchUsage,hl,ha] at h
    | none =>
      have he : uses = usageUnion context left right := by simpa [mergeBranchUsage,hl,ha] using h.symm
      refine ⟨he,?_⟩
      intro i info hc hk
      cases hm : info.ty.kind with
      | copy => contradiction
      | affine => exact firstDemandMismatch_none_equal ha hc hm
      | linear => exact firstDemandMismatch_none_equal hl hc hm

theorem entryFits_branches (info : TypeInfo) (left right : Bool)
    (entry : Option (MetatheoryFull.Ty × Mode))
    (hf : EntryFits info (left || right) entry)
    (he : info.ty.kind = .copy ∨ left = right) :
    EntryFits info left entry ∧ EntryFits info right entry := by
  cases hk : info.ty.kind <;> cases left <;> cases right <;> simp_all [EntryFits]

theorem contextFits_branches {context : Context} {left right uses : Usage}
    {site : Site} {full : MetatheoryFull.Ctx}
    (hf : ContextFits context uses full)
    (hm : mergeBranchUsage context left right site = .ok uses) :
    ContextFits context left full ∧ ContextFits context right full := by
  obtain ⟨rfl,he⟩ := mergeBranchUsage_properties hm
  have localFit (i : Nat) (info : TypeInfo) (hc : context[i]? = some info) := by
    have hi := contextFits_entry hf hc
    rw [usageUnion_demand left right hc] at hi
    have hsame : info.ty.kind = .copy ∨ demand left i = demand right i := by
      by_cases hk : info.ty.kind = .copy
      · exact Or.inl hk
      · exact Or.inr (he i info hc hk)
    exact entryFits_branches info (demand left i) (demand right i) (full i) hi hsame
  constructor
  · intro i
    cases hc : context[i]? with
    | none => simpa [hc] using contextFits_absent hf hc
    | some info => simpa [EntryFits,hc] using (localFit i info hc).1
  · intro i
    cases hc : context[i]? with
    | none => simpa [hc] using contextFits_absent hf hc
    | some info => simpa [EntryFits,hc] using (localFit i info hc).2

theorem except_bind_success {ε α β : Type} {first : Except ε α}
    {next : α → Except ε β} {result : β}
    (h : first.bind next = .ok result) :
    ∃ value, first = .ok value ∧ next value = .ok result := by
  cases first with
  | error error => cases h
  | ok value => exact ⟨value,rfl,h⟩

theorem liftType_success {site : Site} {first : Except TypeError TypeInfo} {info : TypeInfo}
    (h : liftType site first = .ok info) : first = .ok info := by
  cases first with
  | ok value => simpa [liftType] using h
  | error error => cases error <;> simp [liftType,failCheck] at h

theorem inferAtom_success_sound {limits : Limits} {context : Context}
    {atom : SourceAtom} {info : InferInfo}
    (h : inferAtom limits context atom = .ok info)
    {full : MetatheoryFull.Ctx} (hf : ContextFits context info.uses full)
    (Sig : Nat → MetatheoryFull.EffSig) :
    MetatheoryFull.HasType Sig full MetatheoryFull.RCtx.empty atom.erase
      info.result.ty.erase info.result.ty.kind [] := by
  cases atom with
  | unit site =>
    simp only [inferAtom] at h
    obtain ⟨u,_,hrest⟩ := except_bind_success h
    obtain ⟨value,hvalue,hrest⟩ := except_bind_success hrest
    cases hrest
    have ht := TypeInfo.ofTy_ty (liftType_success hvalue)
    rw [ht]
    exact .unitV (contextFits_zero_discard hf) MetatheoryFull.discardR_empty
  | nat site n =>
    simp only [inferAtom] at h
    obtain ⟨u,_,hrest⟩ := except_bind_success h
    split at hrest
    · simp [failCheck] at hrest
    · obtain ⟨value,hvalue,hrest⟩ := except_bind_success hrest
      cases hrest
      have ht := TypeInfo.ofTy_ty (liftType_success hvalue)
      rw [ht]
      exact .natV n (contextFits_zero_discard hf) MetatheoryFull.discardR_empty
  | var site index =>
    simp only [inferAtom] at h
    obtain ⟨u,_,hrest⟩ := except_bind_success h
    cases hc : context[index]? with
    | none => simp [hc,failCheck] at hrest
    | some value =>
      simp only [hc] at hrest
      obtain ⟨u,_,hrest⟩ := except_bind_success hrest
      cases hrest
      exact .var (contextFits_singleton_lookup hf hc) (contextFits_singleton_rest hf)
        MetatheoryFull.discardR_empty (MetatheoryFull.Mode.le_refl _)

/-- Actual successful finite inference constructs Full typing in every fitting
context. The caller supplies no typing premise or resource-store proof. -/
theorem infer_success_sound {limits : Limits} {context : Context}
    {expression : SourceExpr} {info : InferInfo}
    (h : infer limits context expression = .ok info)
    {full : MetatheoryFull.Ctx} (hf : ContextFits context info.uses full)
    (Sig : Nat → MetatheoryFull.EffSig) :
    MetatheoryFull.HasType Sig full MetatheoryFull.RCtx.empty expression.erase
      info.result.ty.erase info.result.ty.kind [] := by
  induction expression generalizing context info full with
  | atom atom => exact inferAtom_success_sound h hf Sig
  | mint site =>
    simp only [infer] at h
    obtain ⟨_,_,hrest⟩ := except_bind_success h
    obtain ⟨value,hvalue,hrest⟩ := except_bind_success hrest
    cases hrest
    have ht := TypeInfo.ofTy_ty (liftType_success hvalue)
    rw [ht]
    exact .mint (contextFits_zero_discard hf) MetatheoryFull.discardR_empty
      (MetatheoryFull.Mode.le_refl _)
  | consume site atom =>
    simp only [infer] at h
    obtain ⟨value,hvalue,hrest⟩ := except_bind_success h
    by_cases ht : value.result.ty = .ticket
    · simp only [ht,ne_eq,not_true_eq_false,ite_false] at hrest
      obtain ⟨result,hresult,hrest⟩ := except_bind_success hrest
      cases hrest
      have hv := inferAtom_success_sound hvalue hf Sig
      rw [ht] at hv
      have hr := TypeInfo.ofTy_ty (liftType_success hresult)
      rw [hr]
      exact .consume (sourceAtom_isAtom atom) hv
    · simp [ht,failCheck] at hrest
  | pair site floor left leftMode right rightMode =>
    simp only [infer] at h
    obtain ⟨a,ha,hrest⟩ := except_bind_success h
    obtain ⟨b,hb,hrest⟩ := except_bind_success hrest
    obtain ⟨uses,hu,hrest⟩ := except_bind_success hrest
    obtain ⟨result,hr,hrest⟩ := except_bind_success hrest
    cases hrest
    have hunion := mergeUsage_union hu
    rw [hunion] at hf
    obtain ⟨hs,hfa,hfb⟩ := contextFits_split hf (mergeUsage_disjoint hu)
    have hta := inferAtom_success_sound ha hfa Sig
    have htb := inferAtom_success_sound hb hfb Sig
    have ht := TypeInfo.pair_ty (liftType_success hr)
    have hw := result.wf
    rw [ht] at hw
    rw [ht]
    cases hw with
    | pair _ _ hleft hright =>
      exact .pair (sourceAtom_isAtom left) (sourceAtom_isAtom right) hs
        (MetatheoryFull.splitR_empty_left _) hta htb hleft hright
        (MetatheoryFull.Mode.le_refl _)
  | letE site initializer body ih1 ih2 =>
    simp only [infer] at h
    obtain ⟨value,hvalue,hrest⟩ := except_bind_success h
    obtain ⟨_,_,hrest⟩ := except_bind_success hrest
    obtain ⟨result,hresult,hrest⟩ := except_bind_success hrest
    obtain ⟨u,hbinder,hrest⟩ := except_bind_success hrest
    cases u
    obtain ⟨uses,hu,hrest⟩ := except_bind_success hrest
    cases hrest
    have hunion := mergeUsage_union hu
    rw [hunion] at hf
    obtain ⟨hs,hf1,hf2⟩ := contextFits_split hf (mergeUsage_disjoint hu)
    have ht1 := ih1 hvalue hf1
    have hb := contextFits_extend hf2 hbinder
    have ht2 := ih2 hresult hb
    exact .letE hs (MetatheoryFull.splitR_empty_left _) ht1 ht2
      (frame_empty_row Sig _ _) (MetatheoryFull.rowSub_refl _) (MetatheoryFull.rowSub_refl _)
  | unpack site atom body ih =>
    simp only [infer] at h
    obtain ⟨value,hvalue,hrest⟩ := except_bind_success h
    cases htype : value.result.ty with
    | unit => simp [htype,failCheck] at hrest
    | nat => simp [htype,failCheck] at hrest
    | ticket => simp [htype,failCheck] at hrest
    | pair floor left leftMode right rightMode =>
      simp only [htype] at hrest
      obtain ⟨leftInfo,hleft,hrest⟩ := except_bind_success hrest
      obtain ⟨rightInfo,hright,hrest⟩ := except_bind_success hrest
      obtain ⟨_,_,hrest⟩ := except_bind_success hrest
      obtain ⟨result,hresult,hrest⟩ := except_bind_success hrest
      obtain ⟨u,hleftUse,hrest⟩ := except_bind_success hrest
      cases u
      obtain ⟨u,hrightUse,hrest⟩ := except_bind_success hrest
      cases u
      obtain ⟨uses,hu,hrest⟩ := except_bind_success hrest
      cases hrest
      have hunion := mergeUsage_union hu
      rw [hunion] at hf
      obtain ⟨hs,hf1,hf2⟩ := contextFits_split hf (mergeUsage_disjoint hu)
      have ht1 := inferAtom_success_sound hvalue hf1 Sig
      rw [htype] at ht1
      have hfleft : ContextFits (leftInfo :: context) result.uses.tail
          (MetatheoryFull.Ctx.ext (splitRight context (result.uses.drop 2) full)
            (leftInfo.ty.erase,leftInfo.ty.kind)) :=
        contextFits_extend (by simpa [usage_tail_tail] using hf2) hleftUse
      have hfright := contextFits_extend hfleft hrightUse
      have ht2 := ih hresult hfright
      have hlty := TypeInfo.ofTy_ty (liftType_success hleft)
      have hrty := TypeInfo.ofTy_ty (liftType_success hright)
      rw [hlty,hrty] at ht2
      exact .letPair (sourceAtom_isAtom atom) hs (MetatheoryFull.splitR_empty_left _) ht1 ht2
  | ifz site atom zero nonzero ihz ihn =>
    simp only [infer] at h
    obtain ⟨condition,hcondition,hrest⟩ := except_bind_success h
    by_cases hct : condition.result.ty = .nat
    · simp only [hct,ne_eq,not_true_eq_false,ite_false] at hrest
      obtain ⟨a,ha,hrest⟩ := except_bind_success hrest
      obtain ⟨b,hb,hrest⟩ := except_bind_success hrest
      by_cases hbt : a.result.ty = b.result.ty
      · simp only [hbt,not_true_eq_false,ite_false] at hrest
        obtain ⟨arms,harms,hrest⟩ := except_bind_success hrest
        obtain ⟨uses,hu,hrest⟩ := except_bind_success hrest
        cases hrest
        have hunion := mergeUsage_union hu
        rw [hunion] at hf
        obtain ⟨hs,hf1,hf2⟩ := contextFits_split hf (mergeUsage_disjoint hu)
        have htc := inferAtom_success_sound hcondition hf1 Sig
        rw [hct] at htc
        obtain ⟨hfz,hfn⟩ := contextFits_branches hf2 harms
        have htz := ihz ha hfz
        have htn := ihn hb hfn
        rw [←hbt] at htn
        exact .ifz (sourceAtom_isAtom atom) hs (MetatheoryFull.splitR_empty_left _) htc htz htn
      · simp [hbt,failCheck] at hrest
    · simp [hct,failCheck] at hrest

/-- The opaque wrapper's stored equality is sufficient for genuine typing;
there is no caller-supplied Full.HasType assumption. -/
theorem checkedProgram_success_sound (program : CheckedProgram)
    {full : MetatheoryFull.Ctx}
    (hf : ContextFits program.program.inputInfo.toList program.uses full)
    (Sig : Nat → MetatheoryFull.EffSig) :
    MetatheoryFull.HasType Sig full MetatheoryFull.RCtx.empty program.program.raw.body.erase
      program.resultType.erase program.resultType.kind [] :=
  infer_success_sound program.check_ok hf Sig

def declaredContext (context : Context) : MetatheoryFull.Ctx :=
  fun i => (context[i]?).map (fun info => (info.ty.erase,info.ty.kind))

theorem contextFits_declared_copy (context : Context) (uses : Usage)
    (hc : ∀ info ∈ context, info.ty.kind = .copy) :
    ContextFits context uses (declaredContext context) := by
  intro i
  cases hi : context[i]? with
  | none => simp [declaredContext,hi]
  | some info =>
    have hk := hc info (List.mem_of_getElem? hi)
    cases demand uses i <;> simp [declaredContext,hi,hk]

/-- All initial locals are independently validated copy types, so the entry
corollary needs neither a typing premise nor a caller context-mask proof. -/
theorem checkedProgram_declared_sound (program : CheckedProgram)
    (Sig : Nat → MetatheoryFull.EffSig) :
    MetatheoryFull.HasType Sig (declaredContext program.program.inputInfo.toList)
      MetatheoryFull.RCtx.empty program.program.raw.body.erase
      program.resultType.erase program.resultType.kind [] :=
  checkedProgram_success_sound program
    (contextFits_declared_copy _ _ program.program.inputs_copy) Sig

theorem zeroUsage_length (context : Context) : (zeroUsage context).length = context.length := by
  induction context <;> simp_all [zeroUsage]

theorem singletonUsage_length (context : Context) (index : Nat) :
    (singletonUsage context index).length = context.length := by
  induction context generalizing index with
  | nil => rfl
  | cons head rest ih => cases index <;> simp [singletonUsage,zeroUsage_length,ih]

theorem usageUnion_length (context : Context) (left right : Usage) :
    (usageUnion context left right).length = context.length := by
  induction context generalizing left right <;> simp_all [usageUnion]

theorem mergeUsage_length {context : Context} {left right uses : Usage} {site : Site}
    (h : mergeUsage context left right site = .ok uses) : uses.length = context.length := by
  rw [mergeUsage_union h]
  exact usageUnion_length _ _ _

theorem inferAtom_success_length {limits context atom info}
    (h : inferAtom limits context atom = .ok info) : info.uses.length = context.length := by
  cases atom with
  | unit site =>
    simp only [inferAtom] at h
    obtain ⟨_,_,hrest⟩ := except_bind_success h
    obtain ⟨_,_,hrest⟩ := except_bind_success hrest
    cases hrest
    exact zeroUsage_length _
  | nat site n =>
    simp only [inferAtom] at h
    obtain ⟨_,_,hrest⟩ := except_bind_success h
    split at hrest
    · simp [failCheck] at hrest
    · obtain ⟨_,_,hrest⟩ := except_bind_success hrest
      cases hrest
      exact zeroUsage_length _
  | var site index =>
    simp only [inferAtom] at h
    obtain ⟨_,_,hrest⟩ := except_bind_success h
    cases hc : context[index]? with
    | none => simp [hc,failCheck] at hrest
    | some value =>
      simp only [hc] at hrest
      obtain ⟨_,_,hrest⟩ := except_bind_success hrest
      cases hrest
      exact singletonUsage_length _ _

theorem infer_success_length {limits context expression info}
    (h : infer limits context expression = .ok info) : info.uses.length = context.length := by
  cases expression with
  | atom atom => exact inferAtom_success_length h
  | mint site =>
    simp only [infer] at h
    obtain ⟨_,_,hrest⟩ := except_bind_success h
    obtain ⟨_,_,hrest⟩ := except_bind_success hrest
    cases hrest
    exact zeroUsage_length _
  | consume site atom =>
    simp only [infer] at h
    obtain ⟨value,hvalue,hrest⟩ := except_bind_success h
    split at hrest
    · simp [failCheck] at hrest
    · obtain ⟨_,_,hrest⟩ := except_bind_success hrest
      cases hrest
      exact inferAtom_success_length (info := value) hvalue
  | pair site floor left ma right mb =>
    simp only [infer] at h
    obtain ⟨_,_,hrest⟩ := except_bind_success h
    obtain ⟨_,_,hrest⟩ := except_bind_success hrest
    obtain ⟨uses,hu,hrest⟩ := except_bind_success hrest
    obtain ⟨_,_,hrest⟩ := except_bind_success hrest
    cases hrest
    exact mergeUsage_length hu
  | letE site initializer body =>
    simp only [infer] at h
    obtain ⟨_,_,hrest⟩ := except_bind_success h
    obtain ⟨_,_,hrest⟩ := except_bind_success hrest
    obtain ⟨_,_,hrest⟩ := except_bind_success hrest
    obtain ⟨_,_,hrest⟩ := except_bind_success hrest
    obtain ⟨uses,hu,hrest⟩ := except_bind_success hrest
    cases hrest
    exact mergeUsage_length hu
  | unpack site atom body =>
    simp only [infer] at h
    obtain ⟨value,_,hrest⟩ := except_bind_success h
    cases ht : value.result.ty with
    | unit => simp [ht,failCheck] at hrest
    | nat => simp [ht,failCheck] at hrest
    | ticket => simp [ht,failCheck] at hrest
    | pair floor left ma right mb =>
      simp only [ht] at hrest
      obtain ⟨_,_,hrest⟩ := except_bind_success hrest
      obtain ⟨_,_,hrest⟩ := except_bind_success hrest
      obtain ⟨_,_,hrest⟩ := except_bind_success hrest
      obtain ⟨_,_,hrest⟩ := except_bind_success hrest
      obtain ⟨_,_,hrest⟩ := except_bind_success hrest
      obtain ⟨_,_,hrest⟩ := except_bind_success hrest
      obtain ⟨uses,hu,hrest⟩ := except_bind_success hrest
      cases hrest
      exact mergeUsage_length hu
  | ifz site atom zero nonzero =>
    simp only [infer] at h
    obtain ⟨_,_,hrest⟩ := except_bind_success h
    split at hrest
    · simp [failCheck] at hrest
    · obtain ⟨_,_,hrest⟩ := except_bind_success hrest
      obtain ⟨_,_,hrest⟩ := except_bind_success hrest
      split at hrest
      · simp [failCheck] at hrest
      · obtain ⟨_,_,hrest⟩ := except_bind_success hrest
        obtain ⟨uses,hu,hrest⟩ := except_bind_success hrest
        cases hrest
        exact mergeUsage_length hu

end Vesper.ResourceModel42.CheckerProofs
