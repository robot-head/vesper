import MetatheoryPlaces
import RefinementPlaces

/-!
Connect snapshot refinements to the actual value writes of the mutable-place
calculus, including linear payloads. A lens is lawful only on initialized,
well-shaped roots and same-typed replacements. The separate resource balance
obligation requires returning/consuming the old payload; logical snapshots
never grant permission to duplicate or silently overwrite a linear owner.
This does not add dependent types to the source checker or cover partial
initialization as an initialized-value lens.
-/
namespace Vesper.TypedPlaceRefinements
open Vesper.Metatheory.Places
open Vesper.EffectPlaces

abbrev TypedValue (ty : Ty) := { value : Val // VT value ty }

theorem set_get_initialized : ∀ {path : Path} {value : Val} {root field : Ty},
    VT value root → root.at path = some field → value.set path (value.at path) = value
  | [], _, _, _, _, _ => by simp
  | head :: tail, value, root, field, typed, existsPath => by
      cases head <;> cases root <;> simp [Ty.at] at existsPath <;>
        cases value <;> simp [VT] at typed
      · simp only [Val.set, Val.at]
        rw [set_get_initialized typed.1 existsPath]
      · simp only [Val.set, Val.at]
        rw [set_get_initialized typed.2 existsPath]

theorem set_set_initialized : ∀ {path : Path} {value : Val} {root field : Ty},
    VT value root → root.at path = some field → ∀ first second,
      (value.set path first).set path second = value.set path second
  | [], _, _, _, _, _, _, _ => by simp
  | head :: tail, value, root, field, typed, existsPath, first, second => by
      cases head <;> cases root <;> simp [Ty.at] at existsPath <;>
        cases value <;> simp [VT] at typed
      · simp only [Val.set]
        rw [set_set_initialized typed.1 existsPath first second]
      · simp only [Val.set]
        rw [set_set_initialized typed.2 existsPath first second]

theorem disjoint_sets_commute : ∀ {p q : Path} (value first second : Val), ¬ Ovl p q →
    (value.set p first).set q second = (value.set q second).set p first
  | [], _, _, _, _, disjoint => absurd (.inl trivial) disjoint
  | _ :: _, [], _, _, _, disjoint => absurd (.inr trivial) disjoint
  | a :: p, b :: q, value, first, second, disjoint => by
      cases a <;> cases b <;> simp [Ovl, Pre] at disjoint <;>
        cases value <;> simp only [Val.set]
      all_goals first
        | rfl
        | exact congrArg (fun v => Val.pair v _) (disjoint_sets_commute (p := p) (q := q) _ first second (by simpa [Ovl] using disjoint))
        | exact congrArg (fun v => Val.pair _ v) (disjoint_sets_commute (p := p) (q := q) _ first second (by simpa [Ovl] using disjoint))

variable {root field : Ty}

def valueLens (path : Path) (valid : root.at path = some field) :
    Lens (TypedValue root) (TypedValue field) :=
  (fun value => ⟨value.val.at path, vt_at value.property valid⟩,
   fun value replacement => ⟨value.val.set path replacement.val,
     vt_replace value.property (vt_at value.property valid) replacement.property⟩)

theorem value_lens_lawful (path : Path) (valid : root.at path = some field) :
    Lawful (valueLens path valid) := by
  refine ⟨?_, ?_, ?_⟩
  · intro value replacement
    apply Subtype.ext
    exact at_set_same replacement.val (vt_valid value.property valid)
  · intro value
    apply Subtype.ext
    exact set_get_initialized value.property valid
  · intro value first second
    apply Subtype.ext
    exact set_set_initialized value.property valid first.val second.val

theorem value_lenses_disjoint (p q : Path)
    (validP : root.at p = some field) (validQ : root.at q = some other)
    (disjoint : ¬ Ovl p q) :
    RefinementPlaces.DisjointLenses (valueLens p validP) (valueLens q validQ) := by
  refine ⟨?_, ?_, ?_⟩
  · intro value incoming
    apply Subtype.ext
    exact at_set_disj value.val incoming.val disjoint
  · intro value incoming
    apply Subtype.ext
    exact at_set_disj value.val incoming.val (fun overlap => disjoint (ovl_symm overlap))
  · intro value first second
    apply Subtype.ext
    exact (disjoint_sets_commute value.val first.val second.val disjoint).symm

def pairedLens (p q : Path) (validP : root.at p = some field) (validQ : root.at q = some field) :
    Lens (TypedValue root) (TypedValue field × TypedValue field) :=
  RefinementPlaces.together (valueLens p validP) (valueLens q validQ)

theorem paired_lens_lawful (p q : Path) (validP : root.at p = some field)
    (validQ : root.at q = some field) (disjoint : ¬ Ovl p q) :
    Lawful (pairedLens p q validP validQ) :=
  RefinementPlaces.together_lawful _ _ (value_lens_lawful p validP)
    (value_lens_lawful q validQ) (value_lenses_disjoint p q validP validQ disjoint)

def swapValues (p q : Path) (validP : root.at p = some field) (validQ : root.at q = some field)
    (before : TypedValue root) : TypedValue root :=
  (pairedLens p q validP validQ).2 before
    ((valueLens q validQ).1 before, (valueLens p validP).1 before)

theorem swap_snapshot (p q : Path) (validP : root.at p = some field)
    (validQ : root.at q = some field) (disjoint : ¬ Ovl p q)
    (P : TypedValue root → Prop) (before : TypedValue root) (pre : P before) :
    RefinementPlaces.Post (pairedLens p q validP validQ)
      (fun value => ((valueLens q validQ).1 value, (valueLens p validP).1 value)) P
      (swapValues p q validP validQ before) :=
  RefinementPlaces.patch_post _ (paired_lens_lawful p q validP validQ disjoint) _ P before pre

theorem swap_preserves_every_identity (p q : Path) (validP : root.at p = some field)
    (validQ : root.at q = some field) (disjoint : ¬ Ovl p q)
    (before : TypedValue root) (resource : Nat) :
    (swapValues p q validP validQ before).val.cnt resource = before.val.cnt resource := by
  have first := cnt_set resource (before.val.at q) (vt_valid before.property validP)
  have middleTyped := vt_replace before.property (vt_at before.property validP)
    (vt_at before.property validQ)
  have second := cnt_set resource (before.val.at p) (vt_valid middleTyped validQ)
  rw [at_set_disj before.val (before.val.at q) disjoint] at second
  change ((before.val.set p (before.val.at q)).set q (before.val.at p)).cnt resource = _
  omega

theorem swap_preserves_freshness (p q : Path) (validP : root.at p = some field)
    (validQ : root.at q = some field) (before : TypedValue root) (fresh : before.val.allLt next) :
    (swapValues p q validP validQ before).val.allLt next :=
  allLt_set q (allLt_set p fresh (allLt_at q fresh)) (allLt_at p fresh)

theorem swap_refinement_and_ownership (p q : Path) (validP : root.at p = some field)
    (validQ : root.at q = some field) (disjoint : ¬ Ovl p q)
    (P : TypedValue root → Prop) (before : TypedValue root) (pre : P before)
    (unique : ∀ resource, before.val.cnt resource ≤ 1) (fresh : before.val.allLt next) :
    RefinementPlaces.Post (pairedLens p q validP validQ)
      (fun value => ((valueLens q validQ).1 value, (valueLens p validP).1 value)) P
      (swapValues p q validP validQ before) ∧
      (∀ resource, (swapValues p q validP validQ before).val.cnt resource ≤ 1) ∧
      (swapValues p q validP validQ before).val.allLt next := by
  refine ⟨swap_snapshot p q validP validQ disjoint P before pre, ?_,
    swap_preserves_freshness p q validP validQ before fresh⟩
  intro resource
  rw [swap_preserves_every_identity p q validP validQ disjoint]
  exact unique resource

theorem typed_write_snapshot (path : Path) (valid : root.at path = some field)
    (rhs : TypedValue root → TypedValue field) (P : TypedValue root → Prop)
    (before : TypedValue root) (pre : P before) :
    RefinementPlaces.Post (valueLens path valid) rhs P
      ((valueLens path valid).2 before (rhs before)) :=
  RefinementPlaces.patch_post _ (value_lens_lawful path valid) rhs P before pre

theorem typed_snapshot_exact (path : Path) (valid : root.at path = some field)
    (rhs : TypedValue root → TypedValue field) (P : TypedValue root → Prop)
    (after : TypedValue root) :
    RefinementPlaces.Post (valueLens path valid) rhs P after ↔
      ∃ before, P before ∧ before.val.set path (rhs before).val = after.val := by
  rw [RefinementPlaces.post_iff_patch _ (value_lens_lawful path valid)]
  constructor
  · rintro ⟨before, pre, written⟩
    exact ⟨before, pre, congrArg Subtype.val written⟩
  · rintro ⟨before, pre, written⟩
    exact ⟨before, pre, Subtype.ext written⟩

-- This is the resource obligation that a purely logical lens law cannot supply.
-- Returning the old field alongside the updated root preserves every identity.
theorem exchange_resource_balance (path : Path) (valid : root.at path = some field)
    (before : TypedValue root) (incoming : TypedValue field) (resource : Nat) :
    ((valueLens path valid).2 before incoming).val.cnt resource +
      ((valueLens path valid).1 before).val.cnt resource =
      before.val.cnt resource + incoming.val.cnt resource :=
  cnt_set resource incoming.val (vt_valid before.property valid)

theorem exchange_preserves_unique_ownership (path : Path) (valid : root.at path = some field)
    (before : TypedValue root) (incoming : TypedValue field)
    (unique : ∀ resource, before.val.cnt resource + incoming.val.cnt resource ≤ 1) :
    ∀ resource, ((valueLens path valid).2 before incoming).val.cnt resource +
      ((valueLens path valid).1 before).val.cnt resource ≤ 1 := by
  intro resource
  rw [exchange_resource_balance]
  exact unique resource

theorem disjoint_projection_preserved (path other : Path) (valid : root.at path = some field)
    (before : TypedValue root) (incoming : TypedValue field) (disjoint : ¬ Ovl path other) :
    (((valueLens path valid).2 before incoming).val).at other = before.val.at other :=
  at_set_disj before.val incoming.val disjoint

-- The frame setter used by FStep.assign and FStep.swap uses precisely this
-- value update, rather than a distinct model of mutation.
theorem frame_write_matches_lens (frame : Frame) (localIndex : Nat) (path : Path)
    (valid : root.at path = some field) (before : TypedValue root) (incoming : TypedValue field)
    (atLocal : frame.get localIndex = before.val) :
    (frame.wr localIndex path incoming.val).get localIndex = ((valueLens path valid).2 before incoming).val := by
  have initialized : VT ((frame.get localIndex).at []) root := by simpa [atLocal] using before.property
  have inBounds := get_lt initialized
  simp [frame_wr_get, inBounds, atLocal, valueLens]

theorem frame_swap_matches_lens (frame : Frame) (localIndex : Nat) (p q : Path)
    (validP : root.at p = some field) (validQ : root.at q = some field)
    (before : TypedValue root) (atLocal : frame.get localIndex = before.val) :
    ((frame.wr localIndex p (before.val.at q)).wr localIndex q (before.val.at p)).get localIndex =
      (swapValues p q validP validQ before).val :=
  frame_write_matches_lens _ localIndex q validQ
    ((valueLens p validP).2 before ((valueLens q validQ).1 before))
    ((valueLens p validP).1 before)
    (frame_write_matches_lens frame localIndex p validP before ((valueLens q validQ).1 before) atLocal)

theorem own_swap_operational_step (program : Prog) (fiber : Fiber) (store : Store)
    (older : List Frame) (frame : Frame) (rest : List Stmt) (localIndex : Nat) (p q : Path)
    (running : fiber.susp = false) (stack : fiber.stack = older ++ [frame])
    (code : frame.code = .swap ⟨.own localIndex, p⟩ ⟨.own localIndex, q⟩ :: rest) :
    FStep program fiber store
      { fiber with stack := older ++ [
        (({ frame with code := rest } : Frame).wr localIndex p ((frame.get localIndex).at q)).wr
          localIndex q ((frame.get localIndex).at p)] } store [] :=
  .swap running stack code

theorem frame_write_refinement_and_resources (frame : Frame) (localIndex : Nat) (path : Path)
    (valid : root.at path = some field) (before : TypedValue root)
    (rhs : TypedValue root → TypedValue field) (P : TypedValue root → Prop)
    (atLocal : frame.get localIndex = before.val) (pre : P before) :
    ∃ after : TypedValue root,
      (frame.wr localIndex path (rhs before).val).get localIndex = after.val ∧
      RefinementPlaces.Post (valueLens path valid) rhs P after ∧
      (∀ resource, after.val.cnt resource + (before.val.at path).cnt resource =
        before.val.cnt resource + (rhs before).val.cnt resource) := by
  refine ⟨(valueLens path valid).2 before (rhs before),
    frame_write_matches_lens frame localIndex path valid before (rhs before) atLocal,
    typed_write_snapshot path valid rhs P before pre, ?_⟩
  intro resource
  exact exchange_resource_balance path valid before (rhs before) resource

def writeStack (stack : List Frame) (position localIndex : Nat) (path : Path) (value : Val) : List Frame :=
  stack.set position ((stack[position]?.getD default).wr localIndex path value)

theorem stack_write_matches_lens (stack : List Frame) (position localIndex : Nat) (path : Path)
    (valid : root.at path = some field) (before : TypedValue root) (incoming : TypedValue field)
    (atRoot : stGet stack position localIndex = before.val) :
    stGet (writeStack stack position localIndex path incoming.val) position localIndex =
      ((valueLens path valid).2 before incoming).val := by
  have initialized : VT ((stGet stack position localIndex).at path) field := by
    rw [atRoot]; exact vt_at before.property valid
  have inStack := stGet_lt initialized
  have inFrame := get_lt (by simpa only [stGet] using initialized)
  simp only [writeStack, stGet_set_wr stack position localIndex path incoming.val
    position localIndex inStack inFrame, and_self, ite_true]
  simp only [valueLens, atRoot]

-- Reuse the existing stack/borrow preservation theorem with its actual
-- overlap premise. The logical update does not waive exclusive-access rules.
theorem stack_write_keeps_borrows (program : Prog) (stack : List Frame)
    (position localIndex : Nat) (path : Path) (valid : root.at path = some field)
    (before : TypedValue root) (incoming : TypedValue field)
    (atRoot : stGet stack position localIndex = before.val) (well : StackOK program stack)
    (safe : ∀ i a location, slotAt stack i a = some location →
      location.pos = position → location.x = localIndex → Ovl location.path path →
        Pre location.path path) :
    StackOK program (writeStack stack position localIndex path incoming.val) := by
  have initialized : VT ((stGet stack position localIndex).at path) field := by
    rw [atRoot]; exact vt_at before.property valid
  exact ⟨write_stack well.frames initialized incoming.property safe,
    bs_congr (slotAt_set_wr stack position localIndex path incoming.val) well.sound⟩

theorem borrowed_alias_observes_write (stack : List Frame) (frame : Frame)
    (slot : Nat) (location : Loc) (suffix : Path)
    (bound : frame.slots[slot]? = some location)
    (valid : root.at (location.path ++ suffix) = some field)
    (before : TypedValue root) (incoming : TypedValue field)
    (atRoot : stGet stack location.pos location.x = before.val) :
    rd (writeStack stack location.pos location.x (location.path ++ suffix) incoming.val)
      frame ⟨.bor slot, suffix⟩ = incoming.val := by
  simp only [rd, bound]
  rw [stack_write_matches_lens stack location.pos location.x _ valid before incoming atRoot]
  exact at_set_same incoming.val (vt_valid before.property valid)

theorem stack_snapshot_and_borrow_soundness (program : Prog) (stack : List Frame)
    (position localIndex : Nat) (path : Path) (valid : root.at path = some field)
    (before : TypedValue root) (rhs : TypedValue root → TypedValue field)
    (P : TypedValue root → Prop) (pre : P before)
    (atRoot : stGet stack position localIndex = before.val) (well : StackOK program stack)
    (safe : ∀ i a location, slotAt stack i a = some location →
      location.pos = position → location.x = localIndex → Ovl location.path path →
        Pre location.path path) :
    ∃ after : TypedValue root,
      stGet (writeStack stack position localIndex path (rhs before).val) position localIndex = after.val ∧
      RefinementPlaces.Post (valueLens path valid) rhs P after ∧
      StackOK program (writeStack stack position localIndex path (rhs before).val) ∧
      (∀ resource, after.val.cnt resource + (before.val.at path).cnt resource =
        before.val.cnt resource + (rhs before).val.cnt resource) := by
  exact ⟨(valueLens path valid).2 before (rhs before),
    stack_write_matches_lens stack position localIndex path valid before (rhs before) atRoot,
    typed_write_snapshot path valid rhs P before pre,
    stack_write_keeps_borrows program stack position localIndex path valid before (rhs before) atRoot well safe,
    exchange_resource_balance path valid before (rhs before)⟩

theorem linear_assignment_rejected (context after : Pcg) (localIndex : Nat)
    (live : context.own[localIndex]? = some (.full .hdl)) :
    ¬ TyAsg context ⟨.own localIndex, []⟩ .hdl after := by
  intro assigned
  cases assigned with
  | own lookup path _ discard =>
      rw [live] at lookup
      cases lookup
      simp only [Sh.at_nil, Option.some.injEq] at path
      subst path
      contradiction

def twoHandles : TypedValue (.pair .hdl .hdl) := ⟨.pair (.hdl 0) (.hdl 1), ⟨trivial, trivial⟩⟩

theorem swap_two_linear_handles :
    (swapValues [false] [true] rfl rfl twoHandles).val = .pair (.hdl 1) (.hdl 0) ∧
      ∀ resource, (swapValues [false] [true] rfl rfl twoHandles).val.cnt resource =
        twoHandles.val.cnt resource :=
  ⟨rfl, swap_preserves_every_identity [false] [true] rfl rfl (by simp [Ovl, Pre]) twoHandles⟩

theorem overwriting_linear_payload_loses_identity :
    (Val.hdl 0).cnt 0 = 1 ∧ ((Val.hdl 0).set [] (.hdl 1)).cnt 0 = 0 := by decide

-- `ValidP` permits expansion of undef for output initialization. That alone
-- does not justify using a total lens: writing back its read changes shape.
theorem uninitialized_path_is_not_a_lawful_lens :
    ValidP .undef [false] ∧
      Val.undef.set [false] (Val.undef.at [false]) ≠ Val.undef := by
  constructor
  · trivial
  · decide

end Vesper.TypedPlaceRefinements

#print axioms Vesper.TypedPlaceRefinements.set_get_initialized
#print axioms Vesper.TypedPlaceRefinements.set_set_initialized
#print axioms Vesper.TypedPlaceRefinements.value_lens_lawful
#print axioms Vesper.TypedPlaceRefinements.typed_write_snapshot
#print axioms Vesper.TypedPlaceRefinements.typed_snapshot_exact
#print axioms Vesper.TypedPlaceRefinements.exchange_resource_balance
#print axioms Vesper.TypedPlaceRefinements.exchange_preserves_unique_ownership
#print axioms Vesper.TypedPlaceRefinements.disjoint_projection_preserved
#print axioms Vesper.TypedPlaceRefinements.frame_write_matches_lens
#print axioms Vesper.TypedPlaceRefinements.frame_write_refinement_and_resources
#print axioms Vesper.TypedPlaceRefinements.overwriting_linear_payload_loses_identity
#print axioms Vesper.TypedPlaceRefinements.uninitialized_path_is_not_a_lawful_lens

#print axioms Vesper.TypedPlaceRefinements.stack_write_matches_lens
#print axioms Vesper.TypedPlaceRefinements.stack_write_keeps_borrows
#print axioms Vesper.TypedPlaceRefinements.borrowed_alias_observes_write
#print axioms Vesper.TypedPlaceRefinements.stack_snapshot_and_borrow_soundness
#print axioms Vesper.TypedPlaceRefinements.linear_assignment_rejected
#print axioms Vesper.TypedPlaceRefinements.disjoint_sets_commute
#print axioms Vesper.TypedPlaceRefinements.value_lenses_disjoint
#print axioms Vesper.TypedPlaceRefinements.paired_lens_lawful
#print axioms Vesper.TypedPlaceRefinements.swap_snapshot
#print axioms Vesper.TypedPlaceRefinements.swap_preserves_every_identity
#print axioms Vesper.TypedPlaceRefinements.swap_preserves_freshness
#print axioms Vesper.TypedPlaceRefinements.swap_refinement_and_ownership
#print axioms Vesper.TypedPlaceRefinements.frame_swap_matches_lens
#print axioms Vesper.TypedPlaceRefinements.own_swap_operational_step
#print axioms Vesper.TypedPlaceRefinements.swap_two_linear_handles
