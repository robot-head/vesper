import EffectPlaces
import StrongUpdate

/-!
Connect refinement snapshots to the lawful place lenses used by the effect
extractor. This proves the update contract for the actual `patch` encoding,
including composed field projections. It does not certify extraction of an
arbitrary source program, allocation, or the permission to use a lens.
-/
namespace Vesper.RefinementPlaces
open Vesper.EffectPlaces

/-- The precondition includes every path and dependent-context fact. The
snapshot rewrites all of it, not just the binding used to name the place. -/
def Post (lens : Lens W X) (rhs : W → X) (P : W → Prop) (after : W) : Prop :=
  ∃ old, P (lens.2 after old) ∧ lens.1 after = rhs (lens.2 after old)

theorem patch_post (lens : Lens W X) (lawful : Lawful lens)
    (rhs : W → X) (P : W → Prop) (before : W) (pre : P before) :
    Post lens rhs P (patch (some lens) before (rhs before)) := by
  obtain ⟨getSet, setGet, setSet⟩ := lawful
  refine ⟨lens.1 before, ?_, ?_⟩
  · simpa only [patch, setSet, setGet] using pre
  · simp only [patch, getSet, setSet, setGet]

/-- Exactness rules out admitting an unreachable contradictory postcondition. -/
theorem post_iff_patch (lens : Lens W X) (lawful : Lawful lens)
    (rhs : W → X) (P : W → Prop) (after : W) :
    Post lens rhs P after ↔
      ∃ before, P before ∧ patch (some lens) before (rhs before) = after := by
  constructor
  · intro ⟨old, pre, result⟩
    refine ⟨lens.2 after old, pre, ?_⟩
    dsimp only [patch]
    rw [← result, lawful.2.2, lawful.2.1]
  · intro ⟨before, pre, result⟩
    rw [← result]
    exact patch_post lens lawful rhs P before pre

/-- Composition supports nested field trees using the extractor's lens
composition rather than assuming an opaque source-level mutation rule. -/
theorem composed_patch_post (outer : Lens W A) (inner : Lens A X)
    (outerLaw : Lawful outer) (innerLaw : Lawful inner)
    (rhs : W → X) (P : W → Prop) (before : W) (pre : P before) :
    Post (compose outer inner) rhs P
      (patch (some (compose outer inner)) before (rhs before)) :=
  patch_post _ (compose_lawful outerLaw innerLaw) rhs P before pre

/-- Frame facts must be independent of the semantic write, including aliases. -/
def Independent (lens : Lens W X) (P : W → Prop) : Prop :=
  ∀ before value, P (lens.2 before value) ↔ P before

theorem patch_keeps_independent (lens : Lens W X) (P : W → Prop)
    (frame : Independent lens P) (before : W) (value : X) :
    P (patch (some lens) before value) ↔ P before := frame before value

/-- Two simultaneous writes require independent observations and commuting
setters, not merely different source names. -/
def DisjointLenses (left : Lens W X) (right : Lens W Y) : Prop :=
  (∀ w x, right.1 (left.2 w x) = right.1 w) ∧
  (∀ w y, left.1 (right.2 w y) = left.1 w) ∧
  (∀ w x y, left.2 (right.2 w y) x = right.2 (left.2 w x) y)

def together (left : Lens W X) (right : Lens W Y) : Lens W (X × Y) :=
  (fun w => (left.1 w, right.1 w), fun w values => right.2 (left.2 w values.1) values.2)

theorem together_lawful (left : Lens W X) (right : Lens W Y)
    (leftLaw : Lawful left) (rightLaw : Lawful right) (disjoint : DisjointLenses left right) :
    Lawful (together left right) := by
  obtain ⟨lg, ls, ll⟩ := leftLaw
  obtain ⟨rg, rs, rr⟩ := rightLaw
  obtain ⟨_, lr, commute⟩ := disjoint
  refine ⟨?_, ?_, ?_⟩
  · intro w values
    simp only [together, lr, lg, rg]
  · intro w
    simp only [together, ls, rs]
  · intro w first second
    simp only [together]
    rw [commute, ll, rr]

theorem aliased_pair_is_not_lawful :
    ¬ Lawful (together (whole : Lens Nat Nat) whole) := by
  intro lawful
  have impossible : (2, 2) = (1, 2) := lawful.1 0 (1, 2)
  cases impossible

/-- A heap location gives a lawful lens, even when multiple source names
resolve to that location. -/
def heapField (location : StrongUpdate.Heap.Location) : Lens StrongUpdate.Heap.Heap Int :=
  (fun heap => heap location, fun heap value => StrongUpdate.Heap.update heap location value)

theorem heapField_lawful (location : StrongUpdate.Heap.Location) :
    Lawful (heapField location) := by
  refine ⟨?_, ?_, ?_⟩
  · intro heap value; exact StrongUpdate.Heap.update_same heap location value
  · intro heap; funext address
    by_cases h : address = location <;> simp [heapField, StrongUpdate.Heap.update, h]
  · intro heap first second
    exact StrongUpdate.Heap.update_twice heap location first second

/-- The heap-field assignment postcondition is the same predicate as the
extractor lens update contract, not merely an analogous lemma. -/
theorem heap_post_is_lens_post (P : StrongUpdate.Heap.Assertion)
    (names : StrongUpdate.Heap.Names) (root field : Nat)
    (rhs : StrongUpdate.Heap.Expr) (after : StrongUpdate.Heap.Heap) :
    StrongUpdate.Heap.Post P root field rhs names after =
      Post (heapField (StrongUpdate.Heap.resolve names root field))
        (fun heap => StrongUpdate.Heap.eval names heap rhs) (P names) after := rfl

theorem heap_patch_preserves_refinements (P : StrongUpdate.Heap.Assertion)
    (names : StrongUpdate.Heap.Names) (root field : Nat)
    (rhs : StrongUpdate.Heap.Expr) (before : StrongUpdate.Heap.Heap) (pre : P names before) :
    StrongUpdate.Heap.Post P root field rhs names
      (patch (some (heapField (StrongUpdate.Heap.resolve names root field))) before
        (StrongUpdate.Heap.eval names before rhs)) := by
  rw [heap_post_is_lens_post]
  exact patch_post _ (heapField_lawful _) _ _ before pre

end Vesper.RefinementPlaces

#print axioms Vesper.RefinementPlaces.patch_post
#print axioms Vesper.RefinementPlaces.post_iff_patch
#print axioms Vesper.RefinementPlaces.composed_patch_post
#print axioms Vesper.RefinementPlaces.patch_keeps_independent
#print axioms Vesper.RefinementPlaces.heapField_lawful
#print axioms Vesper.RefinementPlaces.heap_post_is_lens_post
#print axioms Vesper.RefinementPlaces.heap_patch_preserves_refinements
#print axioms Vesper.RefinementPlaces.together_lawful
#print axioms Vesper.RefinementPlaces.aliased_pair_is_not_lawful
