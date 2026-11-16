-- Refinement havoc for scalar stores and aliased integer heap fields.
-- This does not model allocation, linear payloads, or production checker refinement.
namespace Vesper.StrongUpdate

abbrev Store := Nat → Int
abbrev Predicate := Store → Prop

def write (s : Store) (x : Nat) (value : Int) : Store :=
  fun y => if y = x then value else s y

@[simp] theorem write_same (s : Store) (x : Nat) (v : Int) :
    write s x v x = v := by simp [write]

@[simp] theorem write_other (s : Store) (x y : Nat) (v : Int) (h : y ≠ x) :
    write s x v y = s y := by simp [write, h]

@[simp] theorem write_restore (s : Store) (x : Nat) (v : Int) :
    write (write s x v) x (s x) = s := by
  funext y
  by_cases h : y = x <;> simp [write, h]

-- P includes both path facts and the refinements of dependent sibling bindings.
-- Existentially hide the old root, rather than reusing its pre-state equalities.
def havoc (x : Nat) (P : Predicate) : Predicate :=
  fun after => ∃ old, P (write after x old)

theorem mutation_preserves_havoc (P : Predicate) (s : Store) (x : Nat) (v : Int)
    (before : P s) : havoc x P (write s x v) := by
  exact ⟨s x, by simpa using before⟩

-- A frame predicate is semantically independent of writes to x.
def Independent (x : Nat) (P : Predicate) : Prop :=
  ∀ s v, P (write s x v) ↔ P s

theorem havoc_preserves_independent_fact {x : Nat} {P : Predicate}
    (independent : Independent x P) (s : Store) : havoc x P s ↔ P s := by
  constructor
  · intro ⟨old, h⟩
    exact (independent s old).mp h
  · intro h
    exact ⟨s x, (independent s (s x)).mpr h⟩

theorem untouched_variable_fact {x y : Nat} (h : y ≠ x) (value : Int) :
    Independent x (fun s => s y = value) := by
  intro s v
  simp [write, h]

inductive Command where
  | assign (x : Nat) (value : Int)
  | seq (first second : Command)

inductive Step : Command → Store → Store → Prop where
  | assign (s x v) : Step (.assign x v) s (write s x v)
  | seq {a b s mid final} : Step a s mid → Step b mid final → Step (.seq a b) s final

-- A derivation system whose assignment rule computes a fresh postcondition.
inductive Hoare : Predicate → Command → Predicate → Prop where
  | assign (P x v) :
      Hoare P (.assign x v) (fun s => havoc x P s ∧ s x = v)
  | seq {P Q R a b} : Hoare P a Q → Hoare Q b R → Hoare P (.seq a b) R
  | consequence {P P' Q Q' c} :
      (∀ s, P' s → P s) → Hoare P c Q → (∀ s, Q s → Q' s) → Hoare P' c Q'

theorem hoare_sound {P Q c} (derivation : Hoare P c Q) :
    ∀ s after, P s → Step c s after → Q after := by
  induction derivation with
  | assign P x v =>
    intro s after hp step
    cases step
    exact ⟨mutation_preserves_havoc P s x v hp, write_same s x v⟩
  | seq _ _ ihA ihB =>
    intro s after hp step
    cases step with
    | seq a b => exact ihB _ _ (ihA _ _ hp a) b
  | consequence pre _ post ih =>
    intro s after hp step
    exact post after (ih s after (pre s hp) step)

-- The old rule would retain x = 0 together with the new x = 1.
theorem stale_fact_is_false_after_assignment :
    ¬ (write (fun _ => 0) 0 1) 0 = 0 := by simp

-- The corrected rule has a concrete reachable, consistent post-state.
theorem updated_postcondition_is_inhabited :
    ∃ s, havoc 0 (fun before => before 0 = 0) s ∧ s 0 = 1 := by
  exact ⟨write (fun _ => 0) 0 1,
    mutation_preserves_havoc _ _ _ _ rfl, write_same _ _ _⟩

end Vesper.StrongUpdate

#print axioms Vesper.StrongUpdate.hoare_sound
#print axioms Vesper.StrongUpdate.havoc_preserves_independent_fact
#print axioms Vesper.StrongUpdate.untouched_variable_fact
#print axioms Vesper.StrongUpdate.stale_fact_is_false_after_assignment
#print axioms Vesper.StrongUpdate.updated_postcondition_is_inhabited

/-!
Heap-field extension. Several source names may resolve to the same object.
Predicates range over the entire heap, including refinements of sibling names;
havoc is performed at the resolved location, not at one syntactic name.
This models integer metadata fields, not pointer allocation or ownership of
linear payloads. The place calculus separately checks permission to mutate.
-/
namespace Vesper.StrongUpdate.Heap

abbrev Location := Nat × Nat
abbrev Heap := Location → Int
abbrev Names := Nat → Nat
abbrev Assertion := Names → Heap → Prop

def resolve (names : Names) (root field : Nat) : Location := (names root, field)
def read (names : Names) (heap : Heap) (root field : Nat) : Int :=
  heap (resolve names root field)
def update (heap : Heap) (loc : Location) (value : Int) : Heap :=
  fun address => if address = loc then value else heap address

@[simp] theorem update_same (heap : Heap) (loc : Location) (v : Int) :
    update heap loc v loc = v := by simp [update]

@[simp] theorem update_other (heap : Heap) (loc address : Location) (v : Int) (h : address ≠ loc) :
    update heap loc v address = heap address := by simp [update, h]

@[simp] theorem update_restore (heap : Heap) (loc : Location) (v : Int) :
    update (update heap loc v) loc (heap loc) = heap := by
  funext address
  by_cases h : address = loc <;> simp [update, h]

@[simp] theorem update_twice (heap : Heap) (loc : Location) (v w : Int) :
    update (update heap loc v) loc w = update heap loc w := by
  funext address
  by_cases h : address = loc <;> simp [update, h]

inductive Expr where
  | lit (value : Int)
  | field (root index : Nat)
  | add (left right : Expr)

def eval (names : Names) (heap : Heap) : Expr → Int
  | .lit v => v
  | .field root index => read names heap root index
  | .add a b => eval names heap a + eval names heap b

/-- Rename the changed location everywhere in the combined precondition and
in the RHS expression. The old integer is logical data, not a copied owner. -/
def Post (P : Assertion) (root field : Nat) (rhs : Expr) : Assertion :=
  fun names after => ∃ old,
    let loc := resolve names root field
    let before := update after loc old
    P names before ∧ after loc = eval names before rhs

theorem assignment_post (P : Assertion) (names : Names) (heap : Heap)
    (root field : Nat) (rhs : Expr) (pre : P names heap) :
    Post P root field rhs names
      (update heap (resolve names root field) (eval names heap rhs)) := by
  refine ⟨heap (resolve names root field), ?_⟩
  simp only [update_restore, update_same]
  exact ⟨pre, True.intro⟩

/-- The computed postcondition is exact: it describes precisely the reachable
post-states, not an inconsistent collection of old and new equalities. -/
theorem post_iff_reachable (P : Assertion) (names : Names) (after : Heap)
    (root field : Nat) (rhs : Expr) :
    Post P root field rhs names after ↔
      ∃ before, P names before ∧
        update before (resolve names root field) (eval names before rhs) = after := by
  constructor
  · intro ⟨old, pre, result⟩
    refine ⟨update after (resolve names root field) old, pre, ?_⟩
    rw [← result, update_twice]
    funext address
    by_cases h : address = resolve names root field
    · simp [update, h]
    · simp [update, h]
  · intro ⟨before, pre, result⟩
    rw [← result]
    exact assignment_post P names before root field rhs pre

inductive Command where
  | assign (root field : Nat) (rhs : Expr)
  | seq (first second : Command)
  | branch (condition : Expr) (zero nonzero : Command)

inductive Exec (names : Names) : Command → Heap → Heap → Prop where
  | assign (heap root field rhs) :
      Exec names (.assign root field rhs) heap
        (update heap (resolve names root field) (eval names heap rhs))
  | seq : Exec names a before mid → Exec names b mid after →
      Exec names (.seq a b) before after
  | zero : eval names before testExpr = 0 → Exec names a before after →
      Exec names (.branch testExpr a b) before after
  | nonzero : eval names before testExpr ≠ 0 → Exec names b before after →
      Exec names (.branch testExpr a b) before after

/-- Every command in this finite heap language evaluates; safety is not
conditional on assuming that evaluation succeeds. -/
theorem execution_exists (names : Names) (command : Command) :
    ∀ before, ∃ after, Exec names command before after := by
  induction command with
  | assign root field rhs =>
    intro before
    exact ⟨_, .assign before root field rhs⟩
  | seq a b first second =>
    intro before
    obtain ⟨mid, ha⟩ := first before
    obtain ⟨after, hb⟩ := second mid
    exact ⟨after, .seq ha hb⟩
  | branch condition a b zero nonzero =>
    intro before
    by_cases h : eval names before condition = 0
    · obtain ⟨after, steps⟩ := zero before
      exact ⟨after, .zero h steps⟩
    · obtain ⟨after, steps⟩ := nonzero before
      exact ⟨after, .nonzero h steps⟩

inductive Hoare : Assertion → Command → Assertion → Prop where
  | assign (P root field rhs) : Hoare P (.assign root field rhs) (Post P root field rhs)
  | seq : Hoare P a Q → Hoare Q b R → Hoare P (.seq a b) R
  | branch : Hoare (fun names heap => P names heap ∧ eval names heap testExpr = 0) a Q →
      Hoare (fun names heap => P names heap ∧ eval names heap testExpr ≠ 0) b Q →
      Hoare P (.branch testExpr a b) Q
  | consequence : (∀ names heap, P' names heap → P names heap) → Hoare P c Q →
      (∀ names heap, Q names heap → Q' names heap) → Hoare P' c Q'

theorem hoare_sound (typed : Hoare P command Q) :
    ∀ names before after, P names before → Exec names command before after → Q names after := by
  induction typed with
  | assign P root field rhs =>
    intro names before after pre steps
    cases steps
    exact assignment_post P names before root field rhs pre
  | seq _ _ first second =>
    intro names before after pre steps
    cases steps with
    | seq a b => exact second names _ after (first names before _ pre a) b
  | branch _ _ zero nonzero =>
    intro names before after pre steps
    cases steps with
    | zero condition branch => exact zero names before after ⟨pre, condition⟩ branch
    | nonzero condition branch => exact nonzero names before after ⟨pre, condition⟩ branch
  | consequence pre _ post ih =>
    intro names before after h steps
    exact post names after (ih names before after (pre names before h) steps)

theorem total_correctness (typed : Hoare P command Q) (names : Names)
    (before : Heap) (pre : P names before) :
    ∃ after, Exec names command before after ∧ Q names after := by
  obtain ⟨after, steps⟩ := execution_exists names command before
  exact ⟨after, steps, hoare_sound typed names before after pre steps⟩

/-- A different source name is not a frame when it aliases the mutated object. -/
theorem aliases_observe_update (names : Names) (heap : Heap) (a b field : Nat) (v : Int)
    (alias : names a = names b) :
    read names (update heap (resolve names a field) v) b field = v := by
  simp [read, resolve, update, alias]

/-- Physical disjointness, not syntactic inequality of root names, justifies
retaining a field fact. This includes another field of the same object. -/
theorem disjoint_field_preserved (names : Names) (heap : Heap) (a f b g : Nat) (v : Int)
    (disjoint : resolve names b g ≠ resolve names a f) :
    read names (update heap (resolve names a f) v) b g = read names heap b g := by
  exact update_other heap _ _ v disjoint

/-- Both a path fact and a dependent sibling refinement are renamed together.
The old alias value is zero, while all current aliases observe the increment. -/
theorem aliased_dependent_update_consistent :
    ∃ after, Post (fun names heap => read names heap 0 0 = 0 ∧
        read names heap 1 0 = read names heap 0 0) 0 0
        (.add (.field 1 0) (.lit 1)) (fun _ => 7) after ∧
      read (fun _ => 7) after 0 0 = 1 ∧ read (fun _ => 7) after 1 0 = 1 := by
  let names : Names := fun _ => 7
  let before : Heap := fun _ => 0
  refine ⟨update before (resolve names 0 0) 1, ?_, ?_, ?_⟩
  · exact assignment_post _ names before 0 0 (.add (.field 1 0) (.lit 1)) ⟨rfl, rfl⟩
  · rfl
  · rfl

theorem stale_alias_fact_rejected :
    ¬ read (fun _ => 7) (update (fun _ => 0) (7, 0) 1) 1 0 = 0 := by decide

end Vesper.StrongUpdate.Heap

#print axioms Vesper.StrongUpdate.Heap.assignment_post
#print axioms Vesper.StrongUpdate.Heap.post_iff_reachable
#print axioms Vesper.StrongUpdate.Heap.hoare_sound
#print axioms Vesper.StrongUpdate.Heap.aliases_observe_update
#print axioms Vesper.StrongUpdate.Heap.disjoint_field_preserved
#print axioms Vesper.StrongUpdate.Heap.aliased_dependent_update_consistent
#print axioms Vesper.StrongUpdate.Heap.stale_alias_fact_rejected

#print axioms Vesper.StrongUpdate.write_same

#print axioms Vesper.StrongUpdate.write_other

#print axioms Vesper.StrongUpdate.write_restore

#print axioms Vesper.StrongUpdate.mutation_preserves_havoc

#print axioms Vesper.StrongUpdate.Heap.update_same

#print axioms Vesper.StrongUpdate.Heap.update_other

#print axioms Vesper.StrongUpdate.Heap.update_restore

#print axioms Vesper.StrongUpdate.Heap.update_twice

#print axioms Vesper.StrongUpdate.Heap.execution_exists
#print axioms Vesper.StrongUpdate.Heap.total_correctness

namespace Vesper.StrongUpdate.Heap

/-- Two heaps agree on every location not writable by concurrent interference. -/
def AgreesOutside (mutable : Location → Prop) (before after : Heap) : Prop :=
  ∀ location, ¬ mutable location → before location = after location

theorem agreesOutside_refl (mutable : Location → Prop) (heap : Heap) :
    AgreesOutside mutable heap heap := fun _ _ => rfl

theorem agreesOutside_trans (first : AgreesOutside mutable a b)
    (second : AgreesOutside mutable b c) : AgreesOutside mutable a c :=
  fun location outside => (first location outside).trans (second location outside)

/-- Semantically retain only facts valid under every allowed mutable-state
replacement. No conjunction/disjunction factorization is assumed. -/
def Stabilize (mutable : Location → Prop) (P : Assertion) : Assertion :=
  fun names heap => ∀ replacement, AgreesOutside mutable heap replacement → P names replacement

/-- A shared invariant must still hold in the current state. -/
theorem stabilize_implies_original (safe : Stabilize mutable P names heap) : P names heap :=
  safe heap (agreesOutside_refl mutable heap)

/-- The retained fact survives every interference step, including one accessed
through a different source alias. -/
theorem stabilize_preserved (safe : Stabilize mutable P names before)
    (interference : AgreesOutside mutable before after) : Stabilize mutable P names after := by
  intro replacement agrees
  exact safe replacement (agreesOutside_trans interference agrees)

def Stable (mutable : Location → Prop) (P : Assertion) : Prop :=
  ∀ names before after, P names before → AgreesOutside mutable before after → P names after

theorem stabilize_is_stable (mutable : Location → Prop) (P : Assertion) :
    Stable mutable (Stabilize mutable P) := by
  intro names before after safe interference
  exact stabilize_preserved safe interference

/-- Greatest stable strengthening of P: any other invariant Q that is stable
and implies P also implies Stabilize P. This characterizes the filtering rule. -/
theorem stabilize_greatest (stable : Stable mutable Q)
    (implies : ∀ names heap, Q names heap → P names heap) :
    ∀ names heap, Q names heap → Stabilize mutable P names heap := by
  intro names heap h replacement agrees
  exact implies names replacement (stable names heap replacement h agrees)

theorem immutable_field_fact_survives (names : Names) (heap : Heap) (root field : Nat)
    (mutable : Location → Prop) (value : Int)
    (outside : ¬ mutable (resolve names root field))
    (known : read names heap root field = value) :
    Stabilize mutable (fun names heap => read names heap root field = value) names heap := by
  intro replacement agrees
  exact (agrees _ outside).symm.trans known

/-- A currently true disjunction cannot simply retain its mutable disjunct.
The first field is mutable, the second immutable; both names alias object 7. -/
theorem disjunction_does_not_survive_interference :
    let names : Names := fun _ => 7
    let heap : Heap := fun location => if location = (7, 0) then 1 else 0
    let P : Assertion := fun names heap =>
      read names heap 0 1 = 1 ∨ read names heap 1 0 > 0
    P names heap ∧ ¬ Stabilize (fun location => location = (7, 0)) P names heap := by
  dsimp
  constructor
  · exact Or.inr (by decide)
  · intro safe
    have agrees : AgreesOutside (fun location => location = (7, 0))
        (fun location => if location = (7, 0) then 1 else 0) (fun _ => 0) := by
      intro location outside
      simp [outside]
    have impossible := safe (fun _ => 0) agrees
    simp [read] at impossible

end Vesper.StrongUpdate.Heap

#print axioms Vesper.StrongUpdate.Heap.agreesOutside_refl
#print axioms Vesper.StrongUpdate.Heap.agreesOutside_trans
#print axioms Vesper.StrongUpdate.Heap.stabilize_implies_original
#print axioms Vesper.StrongUpdate.Heap.stabilize_preserved
#print axioms Vesper.StrongUpdate.Heap.stabilize_is_stable
#print axioms Vesper.StrongUpdate.Heap.stabilize_greatest
#print axioms Vesper.StrongUpdate.Heap.immutable_field_fact_survives
#print axioms Vesper.StrongUpdate.Heap.disjunction_does_not_survive_interference

namespace Vesper.StrongUpdate.Heap

/-- A sound automatic weakening hides all prior mutable contents. Unlike
Stabilize, which is an obligation for retaining a predicate unchanged, this
closure follows from any pre-state assertion. -/
def ForgetMutable (mutable : Location → Prop) (P : Assertion) : Assertion :=
  fun names heap => ∃ before, AgreesOutside mutable before heap ∧ P names before

theorem forget_mutable_admits_prestate (known : P names heap) :
    ForgetMutable mutable P names heap :=
  ⟨heap, agreesOutside_refl mutable heap, known⟩

theorem forget_mutable_stable : Stable mutable (ForgetMutable mutable P) := by
  intro names before after known interference
  obtain ⟨snapshot, agrees, pre⟩ := known
  exact ⟨snapshot, agreesOutside_trans agrees interference, pre⟩

/-- Strongest stable consequence of P: every other sound stable weakening is
implied by ForgetMutable P. Thus retaining facts cannot manufacture stronger
information from a merely true pre-state disjunction. -/
theorem forget_mutable_least (stable : Stable mutable Q)
    (implies : ∀ names heap, P names heap → Q names heap) :
    ∀ names heap, ForgetMutable mutable P names heap → Q names heap := by
  intro names heap known
  obtain ⟨snapshot, agrees, pre⟩ := known
  exact stable names snapshot heap (implies names snapshot pre) agrees

/-- Sharing followed by arbitrary allowed interference preserves the weakened
combined context, without assuming that the original predicate was stable. -/
theorem sharing_preserves_weakened_context (known : P names before)
    (interference : AgreesOutside mutable before after) :
    ForgetMutable mutable P names after := ⟨before, interference, known⟩

end Vesper.StrongUpdate.Heap

#print axioms Vesper.StrongUpdate.Heap.forget_mutable_admits_prestate
#print axioms Vesper.StrongUpdate.Heap.forget_mutable_stable
#print axioms Vesper.StrongUpdate.Heap.forget_mutable_least
#print axioms Vesper.StrongUpdate.Heap.sharing_preserves_weakened_context
