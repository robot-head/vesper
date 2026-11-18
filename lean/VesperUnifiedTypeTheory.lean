-- VesperUnifiedTypeTheory.lean (Appendix A.3)
-- Limited algebraic models of 4-axis ModeVec, Capsule admission, LIFO order, ghost-list erasure,
-- [T-OutSplitSlice], refinement SemVer variance, graded arena bump-check elision, and Quot.sound.
--
-- No `sorry`, no `admit`, and no custom axioms are used.

namespace Vesper.UnifiedTypeTheory

-- ==================== 1. 4-AXIS MODAL LATTICE & CAPSULE ====================
inductive UsageMode   where | copy | affine | linear       deriving DecidableEq, Repr
inductive Portability where | portable | pinned            deriving DecidableEq, Repr
inductive Contention  where | uncontended | contended      deriving DecidableEq, Repr
inductive OrderAxis   where | unordered | lifo             deriving DecidableEq, Repr

structure ModeVec where
  usage : UsageMode
  port  : Portability
  cont  : Contention
  ord   : OrderAxis
  deriving DecidableEq, Repr

@[simp] def UsageMode.rank : UsageMode → Nat | .copy => 0 | .affine => 1 | .linear => 2
@[simp] def Portability.rank : Portability → Nat | .portable => 0 | .pinned => 1
@[simp] def Contention.rank : Contention → Nat | .uncontended => 0 | .contended => 1
@[simp] def OrderAxis.rank : OrderAxis → Nat | .unordered => 0 | .lifo => 1

@[simp] def ModeVec.Le (m1 m2 : ModeVec) : Prop :=
  m1.usage.rank ≤ m2.usage.rank ∧
  m1.port.rank  ≤ m2.port.rank  ∧
  m1.cont.rank  ≤ m2.cont.rank  ∧
  m1.ord.rank   ≤ m2.ord.rank

theorem modevec_le_trans {a b c : ModeVec} (h1 : ModeVec.Le a b) (h2 : ModeVec.Le b c) :
    ModeVec.Le a c := by
  rcases h1 with ⟨u1, p1, c1, o1⟩
  rcases h2 with ⟨u2, p2, c2, o2⟩
  exact ⟨Nat.le_trans u1 u2, Nat.le_trans p1 p2, Nat.le_trans c1 c2, Nat.le_trans o1 o2⟩

-- These are explicit premises of the abstract model, not a heap analysis.
-- The production checker must establish them from the reachable graph.
structure CapsuleEvidence where
  uniquelyOwned : Bool
  externalAliases : Nat
  freeOuterCaps : Nat
  threadAffineHandles : Nat
  deriving DecidableEq, Repr

def capsuleSealSafe (e : CapsuleEvidence) (m : ModeVec) : Option ModeVec :=
  if e.uniquelyOwned = true ∧ e.externalAliases = 0 ∧ e.freeOuterCaps = 0 ∧
      e.threadAffineHandles = 0 ∧ m.cont = .uncontended ∧ m.ord = .unordered then
    some { m with port := .portable }
  else none

theorem capsule_seal_produces_portable_uncontended
    {e : CapsuleEvidence} {m sealed : ModeVec}
    (h : capsuleSealSafe e m = some sealed) :
    e.uniquelyOwned = true ∧ e.externalAliases = 0 ∧ e.freeOuterCaps = 0 ∧
      e.threadAffineHandles = 0 ∧ sealed.port = .portable ∧
      sealed.cont = .uncontended ∧ sealed.ord = .unordered ∧ sealed.usage = m.usage := by
  unfold capsuleSealSafe at h
  split at h
  next hc =>
    cases h
    exact ⟨hc.1, hc.2.1, hc.2.2.1, hc.2.2.2.1, rfl,
      hc.2.2.2.2.1, hc.2.2.2.2.2, rfl⟩
  next => cases h

theorem capsule_rejects_thread_affinity (m : ModeVec) :
    capsuleSealSafe ⟨true, 0, 0, 1⟩ m = none := by simp [capsuleSealSafe]

theorem capsule_rejects_outer_capability (m : ModeVec) :
    capsuleSealSafe ⟨true, 0, 1, 0⟩ m = none := by simp [capsuleSealSafe]

theorem capsule_rejects_external_alias (m : ModeVec) :
    capsuleSealSafe ⟨true, 1, 0, 0⟩ m = none := by simp [capsuleSealSafe]

theorem capsule_rejects_shared_owner (m : ModeVec) :
    capsuleSealSafe ⟨false, 0, 0, 0⟩ m = none := by simp [capsuleSealSafe]

theorem capsule_rejects_ordered_owner (e : CapsuleEvidence) (m : ModeVec)
    (h : m.ord = .lifo) : capsuleSealSafe e m = none := by simp [capsuleSealSafe, h]

-- Context splitting concerns permission to access a place, not just its value's mode.
inductive Access where | own | shr | mut | out deriving DecidableEq, Repr
structure PlaceBinding where
  access : Access
  usage : UsageMode
  order : OrderAxis
  deriving DecidableEq, Repr

def Contractible (b : PlaceBinding) : Prop :=
  b.access = .shr ∨ (b.access = .own ∧ b.usage = .copy ∧ b.order = .unordered)

inductive PlaceSplit : Option PlaceBinding → Option PlaceBinding → Option PlaceBinding → Prop where
  | empty : PlaceSplit none none none
  | left (b) : PlaceSplit (some b) (some b) none
  | right (b) : PlaceSplit (some b) none (some b)
  | both (b) (h : Contractible b) : PlaceSplit (some b) (some b) (some b)

-- Any derivation that supplies the binding to both branches must justify contraction.
theorem duplicated_place_is_contractible {b left right : PlaceBinding}
    (h : PlaceSplit (some b) (some left) (some right)) : Contractible b := by
  cases h with
  | both _ hc => exact hc

theorem exclusive_place_never_duplicated {b left right : PlaceBinding}
    (exclusive : b.access = .mut ∨ b.access = .out) :
    ¬ PlaceSplit (some b) (some left) (some right) := by
  intro h
  have hc := duplicated_place_is_contractible h
  rcases exclusive with hm | ho <;> simp [Contractible, *] at hc

theorem ordered_owner_never_duplicated {b left right : PlaceBinding}
    (owned : b.access = .own) (ordered : b.order = .lifo) :
    ¬ PlaceSplit (some b) (some left) (some right) := by
  intro split
  have hc := duplicated_place_is_contractible split
  simp [Contractible, owned, ordered] at hc

theorem shared_linear_view_can_split :
    PlaceSplit (some ⟨.shr, .linear, .lifo⟩) (some ⟨.shr, .linear, .lifo⟩) (some ⟨.shr, .linear, .lifo⟩) :=
  .both _ (Or.inl rfl)

-- ==================== 2. LIFO ORDERED EXCHANGE SAFETY ======================
abbrev GuardId := Nat

def popLifo (stack : List GuardId) (target : GuardId) : Option (List GuardId) :=
  match stack with
  | []          => none
  | top :: rest => if top = target then some rest else none

theorem lifo_rejects_out_of_order_irq_restore
    (outer inner : GuardId) (rest : List GuardId) (h_neq : inner ≠ outer) :
    popLifo (inner :: outer :: rest) outer = none := by
  unfold popLifo
  exact ite_eq_right h_neq

-- ==================== 3. @0 GHOST LINEARITY ERASURE ========================
inductive Presence where | ghost0 | runtime1 deriving DecidableEq, Repr

structure GradedBinding where
  pres      : Presence
  valOrPerm : Nat
  deriving DecidableEq, Repr

@[simp] def eraseGhostBindings : List GradedBinding → List Nat
  | [] => []
  | b :: rest =>
      match b.pres with
      | .ghost0 => eraseGhostBindings rest
      | .runtime1 => b.valOrPerm :: eraseGhostBindings rest

theorem ghost_linear_token_zero_runtime_footprint
    (permId : Nat) (ctx : List GradedBinding) :
    eraseGhostBindings ({ pres := .ghost0, valOrPerm := permId } :: ctx) =
    eraseGhostBindings ctx := rfl

-- ==================== 4. `out` DESTINATION PARALLEL SLICE SPLIT ============
theorem out_split_slice_disjoint_and_exhaustive
    (mid len idx : Nat) (_h_mid : mid ≤ len) :
    (¬ (idx < mid ∧ (mid ≤ idx ∧ idx < len))) ∧
    (idx < len → (idx < mid ∨ (mid ≤ idx ∧ idx < len))) := by
  constructor
  · intro ⟨h_lt, h_ge, _⟩
    omega
  · intro h_lt
    omega

-- ==================== 5. LIQUID REFINEMENTS & SEMVER VARIANCE ==============
theorem semver_refinement_variance_sound
    {Val : Type}
    (P_old P_new Q_old Q_new : Val → Prop)
    (h_pre_contra : ∀ x, P_old x → P_new x)
    (h_post_cov   : ∀ y, Q_new y → Q_old y)
    (h_impl_new   : ∀ x, P_new x → ∃ y, Q_new y) :
    ∀ x, P_old x → ∃ y, Q_old y := by
  intro x h_po
  obtain ⟨y, h_qn⟩ := h_impl_new x (h_pre_contra x h_po)
  exact ⟨y, h_post_cov y h_qn⟩

-- ==================== 6. GRADED ARENA BUMP-CHECK ELISION ===================
theorem graded_region_loop_elides_limit_checks
    (cursor0 initB iters stepB i limit : Nat)
    (h_iter   : i ≤ iters)
    (h_budget : cursor0 + initB + iters * stepB ≤ limit) :
    cursor0 + initB + i * stepB ≤ limit := by
  have h_mul : i * stepB ≤ iters * stepB := Nat.mul_le_mul_right stepB h_iter
  omega

-- ==================== 7. SUBSTRUCTURAL QUOTIENTS (`Quot.sound`) ============
abbrev RawZTile := List Int

def ZWeightEquiv (t1 t2 : RawZTile) : Prop :=
  t1.foldl (· + ·) 0 = t2.foldl (· + ·) 0

abbrev ZSetQuot := Quot ZWeightEquiv

def ZSetQuot.totalMass : ZSetQuot → Int :=
  Quot.lift (fun t => t.foldl (· + ·) 0) (fun _ _ h_eq => h_eq)

theorem uncompacted_zset_eq_compacted :
    (Quot.mk ZWeightEquiv [10, -10, 42] : ZSetQuot) = Quot.mk ZWeightEquiv [42] := by
  apply Quot.sound
  unfold ZWeightEquiv
  decide

-- ==================== 8. SKELETAL SUGAR PASS-MODE TRANSPARENCY =============
inductive SugarPass where
  | sharedIn | mutInout | initOut | moveSink
  deriving DecidableEq, Repr

def passPermitsUse (patHole templateUse : SugarPass) : Bool :=
  match patHole, templateUse with
  | .sharedIn, .sharedIn => true
  | .mutInout, .sharedIn | .mutInout, .mutInout => true
  | .initOut,  .initOut  => true
  | .moveSink, .sharedIn | .moveSink, .mutInout | .moveSink, .moveSink => true
  | _, _ => false

def isMutatingOrConsuming (u : SugarPass) : Bool :=
  match u with
  | .sharedIn => false
  | .mutInout | .initOut | .moveSink => true

theorem sugar_shared_hole_forbids_hidden_mutation
    (u : SugarPass)
    (h_wf : passPermitsUse .sharedIn u = true) :
    isMutatingOrConsuming u = false := by
  cases u <;> first | rfl | cases h_wf

theorem sugar_pass_permits_refl (p : SugarPass) :
    passPermitsUse p p = true := by
  cases p <;> rfl

theorem sugar_out_hole_disjoint_from_reads
    (u : SugarPass)
    (h_wf : passPermitsUse .initOut u = true) :
    u = .initOut := by
  cases u <;> first | rfl | cases h_wf

end Vesper.UnifiedTypeTheory

#print axioms Vesper.UnifiedTypeTheory.modevec_le_trans
#print axioms Vesper.UnifiedTypeTheory.capsule_seal_produces_portable_uncontended
#print axioms Vesper.UnifiedTypeTheory.lifo_rejects_out_of_order_irq_restore
#print axioms Vesper.UnifiedTypeTheory.ghost_linear_token_zero_runtime_footprint
#print axioms Vesper.UnifiedTypeTheory.out_split_slice_disjoint_and_exhaustive
#print axioms Vesper.UnifiedTypeTheory.semver_refinement_variance_sound
#print axioms Vesper.UnifiedTypeTheory.graded_region_loop_elides_limit_checks
#print axioms Vesper.UnifiedTypeTheory.uncompacted_zset_eq_compacted
#print axioms Vesper.UnifiedTypeTheory.sugar_shared_hole_forbids_hidden_mutation
#print axioms Vesper.UnifiedTypeTheory.sugar_pass_permits_refl
#print axioms Vesper.UnifiedTypeTheory.sugar_out_hole_disjoint_from_reads

#print axioms Vesper.UnifiedTypeTheory.capsule_rejects_thread_affinity
#print axioms Vesper.UnifiedTypeTheory.capsule_rejects_outer_capability
#print axioms Vesper.UnifiedTypeTheory.capsule_rejects_external_alias
#print axioms Vesper.UnifiedTypeTheory.capsule_rejects_shared_owner
#print axioms Vesper.UnifiedTypeTheory.capsule_rejects_ordered_owner
#print axioms Vesper.UnifiedTypeTheory.duplicated_place_is_contractible
#print axioms Vesper.UnifiedTypeTheory.exclusive_place_never_duplicated
#print axioms Vesper.UnifiedTypeTheory.shared_linear_view_can_split

#print axioms Vesper.UnifiedTypeTheory.ordered_owner_never_duplicated
