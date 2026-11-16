import TypedPlaceRefinements
import Metatheory.Polymorphism

/-!
A representation and exchange bridge between the actual mutable-place and
polymorphic calculi. Initialized place values embed with the same usage kind
and exact handle ownership; affine buffers use an affine unit-pair wrapper.
Mutable field replacement corresponds to the polymorphic `swap` reduction,
which returns the displaced owner. This is not a merger of the two languages:
heap-stored closures, captured borrows, allocation and source elaboration are
not supplied by this bridge.
-/
namespace Vesper.PlacePolymorphismBridge
open Vesper.Metatheory

abbrev PVal := Places.Val
abbrev PTy := Places.Ty
abbrev Tm := Polymorphism.Expr
abbrev TTy := Polymorphism.Ty

def encodeMode : Places.Mode → Polymorphism.Mode
  | .copy => .copy | .affine => .affine | .linear => .linear

def encodeType : PTy → TTy
  | .nat => .nat
  | .buf => .pair .affine .unit .copy .unit .copy
  | .hdl => .hdl
  | .pair a b => .pair .copy (encodeType a) (encodeMode a.kind) (encodeType b) (encodeMode b.kind)

/-- `undef` is a sentinel only; every theorem using a value requires initialization. -/
def encodeValue : PVal → Tm
  | .nat n => .natV n
  | .buf => .pair .unitV .unitV
  | .hdl id => .hdlV id
  | .pair a b => .pair (encodeValue a) (encodeValue b)
  | .undef => .unitV

def Unique (value : PVal) : Prop := ∀ id, value.cnt id ≤ 1

def resources (value : PVal) : Polymorphism.RCtx :=
  fun id => if value.cnt id = 0 then none else some .hdl

theorem encode_join (a b : Places.Mode) :
    encodeMode (Places.Mode.join a b) = Polymorphism.Mode.join (encodeMode a) (encodeMode b) := by
  cases a <;> cases b <;> decide

theorem encode_kind (ty : PTy) : (encodeType ty).kind = encodeMode ty.kind := by
  cases ty with
  | nat => rfl
  | buf => rfl
  | hdl => rfl
  | pair a b =>
    simp only [encodeType, Polymorphism.Ty.kind, Places.Ty.kind, encode_join]
    cases a.kind <;> cases b.kind <;> rfl

theorem unique_pair_parts {a b : PVal} (unique : Unique (.pair a b)) : Unique a ∧ Unique b := by
  constructor <;> intro id <;> have bound := unique id <;>
    simp only [Places.Val.cnt] at bound <;> omega

theorem resource_split {a b : PVal} (unique : Unique (.pair a b)) :
    Polymorphism.SplitR (resources (.pair a b)) (resources a) (resources b) := by
  intro id
  have bound := unique id
  simp only [Places.Val.cnt] at bound
  by_cases empty : a.cnt id = 0
  · simp [resources, Places.Val.cnt, Polymorphism.SRE, empty]
  · have rightEmpty : b.cnt id = 0 := by omega
    simp [resources, Places.Val.cnt, Polymorphism.SRE, rightEmpty]

theorem encoded_value_typed (Sig : Nat → Polymorphism.EffSig) {value : PVal} {ty : PTy}
    (typed : Places.VT value ty) (unique : Unique value) :
    Polymorphism.VT Sig (resources value) (encodeValue value) (encodeType ty) := by
  induction value generalizing ty with
  | nat n =>
    cases ty <;> simp only [Places.VT] at typed
    exact .natV n (fun _ => rfl)
  | buf =>
    cases ty <;> simp only [Places.VT] at typed
    exact .pair (Polymorphism.splitR_empty_left _)
      (.unitV (fun _ => rfl)) (.unitV (fun _ => rfl))
      (Polymorphism.Mode.le_refl _) (Polymorphism.Mode.le_refl _)
  | hdl id =>
    cases ty <;> simp only [Places.VT] at typed
    apply Polymorphism.VT.hdlV
    intro other
    simp [resources, Places.Val.cnt, Polymorphism.RCtx.single, eq_comm]
  | pair a b left right =>
    cases ty <;> simp only [Places.VT] at typed
    obtain ⟨uniqueA, uniqueB⟩ := unique_pair_parts unique
    exact .pair (resource_split unique) (left typed.1 uniqueA) (right typed.2 uniqueB)
      (by rw [encode_kind]; exact Polymorphism.Mode.le_refl _)
      (by rw [encode_kind]; exact Polymorphism.Mode.le_refl _)
  | undef => cases ty <;> cases typed

theorem encoded_value_is_value {value : PVal} {ty : PTy} (typed : Places.VT value ty) :
    Polymorphism.IsVal (encodeValue value) := by
  induction value generalizing ty with
  | nat n => exact .natV n
  | buf => exact .pair .unitV .unitV
  | hdl id => exact .hdlV id
  | pair a b left right =>
    cases ty <;> simp only [Places.VT] at typed
    exact .pair (left typed.1) (right typed.2)
  | undef => cases ty <;> cases typed

theorem encoded_fresh {value : PVal} {next : Nat} (fresh : value.allLt next) :
    Polymorphism.Fresh (resources value) next := by
  intro id after
  simp [resources, Places.cnt_allLt id fresh after]

theorem encoded_configuration_typed (Sig : Nat → Polymorphism.EffSig)
    {value : PVal} {ty : PTy} {next : Nat}
    (typed : Places.VT value ty) (unique : Unique value) (fresh : value.allLt next) :
    Polymorphism.WT Sig ⟨encodeValue value, resources value, next⟩
      (encodeType ty) (encodeType ty).kind [] := by
  exact ⟨Polymorphism.val_ty (encoded_value_typed Sig typed unique)
    Polymorphism.discard_empty Polymorphism.discardR_empty
    (Polymorphism.splitR_empty_right _) (Polymorphism.Mode.le_refl _), encoded_fresh fresh⟩

theorem encoded_type_at : ∀ {path : Places.Path} {root field : PTy},
    root.at path = some field → (encodeType root).at path = some (encodeType field)
  | [], root, field, valid => by
      have same : root = field := by simpa using valid
      subst field
      exact Polymorphism.Ty.at_nil _
  | head :: tail, root, field, valid => by
      cases head <;> cases root <;> simp [Places.Ty.at] at valid
      all_goals simpa [encodeType, Polymorphism.Ty.at] using encoded_type_at valid

theorem encoded_at : ∀ {path : Places.Path} {value : PVal} {root field : PTy},
    Places.VT value root → root.at path = some field →
    Polymorphism.vAt (encodeValue value) path = some (encodeValue (value.at path))
  | [], _, _, _, _, _ => by simp [Polymorphism.vAt]
  | head :: tail, value, root, field, typed, valid => by
      cases head <;> cases root <;> simp [Places.Ty.at] at valid <;>
        cases value <;> simp [Places.VT] at typed
      · simpa [encodeValue, Polymorphism.vAt, Places.Val.at] using encoded_at typed.1 valid
      · simpa [encodeValue, Polymorphism.vAt, Places.Val.at] using encoded_at typed.2 valid

theorem encoded_set : ∀ {path : Places.Path} {value : PVal} {root field : PTy},
    Places.VT value root → root.at path = some field → ∀ incoming,
    Polymorphism.vSet (encodeValue value) path (encodeValue incoming) =
      some (encodeValue (value.set path incoming))
  | [], _, _, _, _, _, _ => by simp [Polymorphism.vSet]
  | head :: tail, value, root, field, typed, valid, incoming => by
      cases head <;> cases root <;> simp [Places.Ty.at] at valid <;>
        cases value <;> simp [Places.VT] at typed
      · simp [encodeValue, Polymorphism.vSet, Places.Val.set, encoded_set typed.1 valid incoming]
      · simp [encodeValue, Polymorphism.vSet, Places.Val.set, encoded_set typed.2 valid incoming]

theorem exchange_typing (Sig : Nat → Polymorphism.EffSig)
    {value incoming : PVal} {root field : PTy} {path : Places.Path}
    (typed : Places.VT value root) (incomingTyped : Places.VT incoming field)
    (valid : root.at path = some field) (unique : Unique (.pair value incoming)) :
    Polymorphism.HasType Sig Polymorphism.Ctx.empty (resources (.pair value incoming))
      (.swap (encodeValue value) path (encodeValue incoming))
      (Polymorphism.leafRoot (encodeType field) (encodeType root)) .linear [] := by
  obtain ⟨uniqueRoot, uniqueIncoming⟩ := unique_pair_parts unique
  have rootValue := encoded_value_typed Sig typed uniqueRoot
  have incomingValue := encoded_value_typed Sig incomingTyped uniqueIncoming
  have rootTerm : Polymorphism.HasType Sig Polymorphism.Ctx.empty (resources value)
      (encodeValue value) (encodeType root) (encodeType root).kind [] :=
    Polymorphism.val_ty rootValue Polymorphism.discard_empty Polymorphism.discardR_empty
      (Polymorphism.splitR_empty_right _) (Polymorphism.Mode.le_refl _)
  have incomingTerm : Polymorphism.HasType Sig Polymorphism.Ctx.empty (resources incoming)
      (encodeValue incoming) (encodeType field) .linear [] :=
    Polymorphism.val_ty incomingValue Polymorphism.discard_empty Polymorphism.discardR_empty
      (Polymorphism.splitR_empty_right _) (Polymorphism.Mode.le_linear _)
  exact .swap (.val (Polymorphism.vt_isVal rootValue)) (.val (Polymorphism.vt_isVal incomingValue))
    (Polymorphism.split_left_all _) (resource_split unique)
    rootTerm (encoded_type_at valid) incomingTerm (Polymorphism.Mode.le_refl _)

theorem exchange_head (Sig : Nat → Polymorphism.EffSig)
    {value incoming : PVal} {root field : PTy} {path : Places.Path}
    (typed : Places.VT value root) (incomingTyped : Places.VT incoming field)
    (valid : root.at path = some field) (live : Polymorphism.RCtx) (next : Nat) :
    Polymorphism.Head Sig (.swap (encodeValue value) path (encodeValue incoming)) live next .silent
      (.pair (encodeValue (value.at path)) (encodeValue (value.set path incoming))) live next :=
  .swap (encoded_value_is_value typed) (encoded_value_is_value incomingTyped)
    (encoded_at typed valid) (encoded_set typed valid incoming)

theorem exchange_step (Sig : Nat → Polymorphism.EffSig)
    {value incoming : PVal} {root field : PTy} {path : Places.Path}
    (typed : Places.VT value root) (incomingTyped : Places.VT incoming field)
    (valid : root.at path = some field) (live : Polymorphism.RCtx) (next : Nat) :
    Polymorphism.Step Sig
      ⟨.swap (encodeValue value) path (encodeValue incoming), live, next⟩ .silent
      ⟨.pair (encodeValue (value.at path)) (encodeValue (value.set path incoming)), live, next⟩ :=
  .ctx .hole (exchange_head Sig typed incomingTyped valid live next)

/-- The actual polymorphic preservation theorem applies to the encoded update. -/
theorem exchange_preserves_polymorphic_typing (Sig : Nat → Polymorphism.EffSig)
    {value incoming : PVal} {root field : PTy} {path : Places.Path} {next : Nat}
    (typed : Places.VT value root) (incomingTyped : Places.VT incoming field)
    (valid : root.at path = some field) (unique : Unique (.pair value incoming))
    (fresh : (Places.Val.pair value incoming).allLt next) :
    Polymorphism.WT Sig
      ⟨.pair (encodeValue (value.at path)) (encodeValue (value.set path incoming)),
        resources (.pair value incoming), next⟩
      (Polymorphism.leafRoot (encodeType field) (encodeType root)) .linear [] :=
  Polymorphism.preservation (exchange_step Sig typed incomingTyped valid _ next)
    ⟨exchange_typing Sig typed incomingTyped valid unique, encoded_fresh fresh⟩

theorem exchange_preserves_each_handle {value incoming : PVal} {root field : PTy}
    {path : Places.Path} (typed : Places.VT value root) (valid : root.at path = some field)
    (id : Nat) :
    (value.at path).cnt id + (value.set path incoming).cnt id = value.cnt id + incoming.cnt id := by
  have balance := Places.cnt_set id incoming (Places.vt_valid typed valid)
  omega

/-- This is the frame setter used by the mutable-place transition rules. -/
theorem frame_write_correspondence (Sig : Nat → Polymorphism.EffSig)
    (frame : Places.Frame) (localIndex : Nat) (path : Places.Path) {root field : PTy}
    (valid : root.at path = some field) (before : TypedPlaceRefinements.TypedValue root)
    (incoming : TypedPlaceRefinements.TypedValue field) (atLocal : frame.get localIndex = before.val)
    (live : Polymorphism.RCtx) (next : Nat) :
    Polymorphism.Head Sig (.swap (encodeValue before.val) path (encodeValue incoming.val))
      live next .silent
      (.pair (encodeValue (before.val.at path))
        (encodeValue ((frame.wr localIndex path incoming.val).get localIndex))) live next := by
  rw [TypedPlaceRefinements.frame_write_matches_lens frame localIndex path valid before incoming atLocal]
  exact exchange_head Sig before.property incoming.property valid live next

theorem duplicate_handle_rejected (id : Nat) : ¬ Unique (.pair (.hdl id) (.hdl id)) := by
  intro unique
  have impossible := unique id
  simp [Places.Val.cnt] at impossible

theorem uninitialized_value_rejected (ty : PTy) : ¬ Places.VT .undef ty :=
  Places.vt_not_undef

/-- The same exchange can reduce under the calculus's let/handler contexts. -/
theorem contextual_exchange_step (Sig : Nat → Polymorphism.EffSig)
    (context : Polymorphism.EC) {value incoming : PVal} {root field : PTy} {path : Places.Path}
    (typed : Places.VT value root) (incomingTyped : Places.VT incoming field)
    (valid : root.at path = some field) (live : Polymorphism.RCtx) (next : Nat) :
    Polymorphism.Step Sig
      ⟨Polymorphism.plug context (.swap (encodeValue value) path (encodeValue incoming)), live, next⟩ .silent
      ⟨Polymorphism.plug context
        (.pair (encodeValue (value.at path)) (encodeValue (value.set path incoming))), live, next⟩ :=
  .ctx context (exchange_head Sig typed incomingTyped valid live next)

theorem contextual_exchange_preserves (Sig : Nat → Polymorphism.EffSig)
    (context : Polymorphism.EC) {value incoming : PVal} {root field : PTy} {path : Places.Path}
    (typed : Places.VT value root) (incomingTyped : Places.VT incoming field)
    (valid : root.at path = some field) {live : Polymorphism.RCtx} {next : Nat}
    {result : TTy} {mode : Polymorphism.Mode} {effects : Polymorphism.Row}
    (well : Polymorphism.WT Sig
      ⟨Polymorphism.plug context (.swap (encodeValue value) path (encodeValue incoming)), live, next⟩
      result mode effects) :
    Polymorphism.WT Sig
      ⟨Polymorphism.plug context
        (.pair (encodeValue (value.at path)) (encodeValue (value.set path incoming))), live, next⟩
      result mode effects :=
  Polymorphism.preservation (contextual_exchange_step Sig context typed incomingTyped valid live next) well

/-- A linear field is exchanged for another handle without losing either id. -/
theorem two_handle_exchange (Sig : Nat → Polymorphism.EffSig) :
    Polymorphism.WT Sig
      ⟨.pair (.hdlV 0) (.pair (.hdlV 1) (.natV 7)),
        resources (.pair (.pair (.hdl 0) (.nat 7)) (.hdl 1)), 2⟩
      (Polymorphism.leafRoot .hdl (encodeType (.pair .hdl .nat))) .linear [] := by
  have unique : Unique (.pair (.pair (.hdl 0) (.nat 7)) (.hdl 1)) := by
    intro id
    simp only [Places.Val.cnt]
    split <;> split <;> simp_all <;> omega
  exact exchange_preserves_polymorphic_typing Sig (root := .pair .hdl .nat) (field := .hdl)
    (path := [false]) ⟨trivial, trivial⟩ trivial rfl unique (by simp [Places.Val.allLt])

end Vesper.PlacePolymorphismBridge

#print axioms Vesper.PlacePolymorphismBridge.encode_join
#print axioms Vesper.PlacePolymorphismBridge.encode_kind
#print axioms Vesper.PlacePolymorphismBridge.unique_pair_parts
#print axioms Vesper.PlacePolymorphismBridge.resource_split
#print axioms Vesper.PlacePolymorphismBridge.encoded_value_typed
#print axioms Vesper.PlacePolymorphismBridge.encoded_value_is_value
#print axioms Vesper.PlacePolymorphismBridge.encoded_fresh
#print axioms Vesper.PlacePolymorphismBridge.encoded_configuration_typed

#print axioms Vesper.PlacePolymorphismBridge.encoded_type_at
#print axioms Vesper.PlacePolymorphismBridge.encoded_at
#print axioms Vesper.PlacePolymorphismBridge.encoded_set
#print axioms Vesper.PlacePolymorphismBridge.exchange_typing
#print axioms Vesper.PlacePolymorphismBridge.exchange_head
#print axioms Vesper.PlacePolymorphismBridge.exchange_step
#print axioms Vesper.PlacePolymorphismBridge.exchange_preserves_polymorphic_typing
#print axioms Vesper.PlacePolymorphismBridge.exchange_preserves_each_handle
#print axioms Vesper.PlacePolymorphismBridge.frame_write_correspondence
#print axioms Vesper.PlacePolymorphismBridge.duplicate_handle_rejected
#print axioms Vesper.PlacePolymorphismBridge.uninitialized_value_rejected

#print axioms Vesper.PlacePolymorphismBridge.contextual_exchange_step
#print axioms Vesper.PlacePolymorphismBridge.contextual_exchange_preserves
#print axioms Vesper.PlacePolymorphismBridge.two_handle_exchange
