import Std

/-!
# Frames and borrowed operation arguments in the Lean extraction

The effectful extractor (`src/lean.rs`) translates a lexical handler to
`HandlerCPS (S × W) (O × W) m`, where `S` is the handler's own state and `W` is
the *frame*: the bindings outside the handler that its body may write.

* Before every action, the handled body stores the current frame in the CPS
  state (`putFrame`), so a clause that aborts, and the return clause, answer
  with the values the evaluator's shared slots hold at that point.
* An operation with an `inout` parameter returns the parameter's value
  together with its result. A clause resumes with the updated value.
* A direct perform site passes an optional lens from the frame to each
  borrowed (`in`/`inout`) argument's place. After a resumption returns, the
  clause reloads the argument through the lens from the resumed frame, and on
  completion writes it back with `patch`.

This file states and proves the algebraic facts that encoding relies on. The
definitions mirror the generated prelude; `tests/lean.rs` checks its type and
Monad denotation with Lean before loading this model's instances. These lemmas concern the
encoding only. They do not prove that extraction preserves the operational
semantics of Vesper, and `partial def` translations do not establish
termination. No axioms or `sorry` are used.
-/

namespace Vesper.EffectPlaces

abbrev HandlerCPS (S O : Type) (m : Type → Type) (A : Type) := (A → S → m O) → S → m O
instance [Monad m] : Monad (HandlerCPS S O m) where
  pure a := fun k s => k a s
  bind action next := fun k s => action (fun a s' => next a k s') s

section Monad
variable {S O A B C : Type} {m : Type → Type} [Monad m]

theorem pure_bind (a : A) (f : A → HandlerCPS S O m B) :
    (pure a >>= f) = f a := rfl

theorem bind_pure (x : HandlerCPS S O m A) : (x >>= pure) = x := rfl

theorem bind_assoc (x : HandlerCPS S O m A) (f : A → HandlerCPS S O m B)
    (g : B → HandlerCPS S O m C) : (x >>= f >>= g) = (x >>= fun a => f a >>= g) := rfl
end Monad

/-- The synchronisation the extractor emits before each action of a body. -/
def putFrame {S W O : Type} {m : Type → Type} (w : W) : HandlerCPS (S × W) O m Unit :=
  fun k s => k () (s.1, w)

/-- An aborting clause discards its continuation and answers with the frame in
its state, as the generated clause does when it never resumes. -/
def abortWith {S W O A : Type} {m : Type → Type} [Monad m] (o : O) :
    HandlerCPS (S × W) (O × W) m A :=
  fun _ s => pure (o, s.2)

theorem abort_observes_last_sync {S W O A : Type} {m : Type → Type} [Monad m]
    (w : W) (o : O) (k : A → S × W → m (O × W)) (s : S × W) :
    (putFrame w >>= fun _ => (abortWith o : HandlerCPS (S × W) (O × W) m A)) k s = pure (o, w) :=
  rfl

theorem later_sync_wins {S W O : Type} {m : Type → Type} [Monad m]
    (w w' : W) (k : Unit → S × W → m O) (s : S × W) :
    (putFrame w >>= fun _ => putFrame w') k s = (putFrame w' : HandlerCPS (S × W) O m Unit) k s :=
  rfl

theorem sync_keeps_user_state {S W O : Type} {m : Type → Type}
    (w : W) (k : Unit → S × W → m O) (s : S × W) :
    (putFrame w : HandlerCPS (S × W) O m Unit) k s = k () (s.1, w) :=
  rfl

/-- The generated lens representation: a getter and a setter on the frame. -/
abbrev Lens (W X : Type) := (W → X) × (W → X → W)

def Lawful {W X : Type} (l : Lens W X) : Prop :=
  (∀ w x, l.1 (l.2 w x) = x) ∧ (∀ w, l.2 w (l.1 w) = w) ∧ (∀ w x y, l.2 (l.2 w x) y = l.2 w y)

/-- Frame components are nested pairs. -/
def first {A B : Type} : Lens (A × B) A := (Prod.fst, fun w a => (a, w.2))
def second {A B : Type} : Lens (A × B) B := (Prod.snd, fun w b => (w.1, b))
def whole {A : Type} : Lens A A := (id, fun _ a => a)

/-- A place path below a frame component, such as `p.x` for a struct `p`. -/
def compose {W A X : Type} (outer : Lens W A) (inner : Lens A X) : Lens W X :=
  (fun w => inner.1 (outer.1 w), fun w x => outer.2 w (inner.2 (outer.1 w) x))

theorem first_lawful {A B : Type} : Lawful (first : Lens (A × B) A) :=
  ⟨fun _ _ => rfl, fun _ => rfl, fun _ _ _ => rfl⟩

theorem second_lawful {A B : Type} : Lawful (second : Lens (A × B) B) :=
  ⟨fun _ _ => rfl, fun _ => rfl, fun _ _ _ => rfl⟩

theorem whole_lawful {A : Type} : Lawful (whole : Lens A A) :=
  ⟨fun _ _ => rfl, fun _ => rfl, fun _ _ _ => rfl⟩

theorem compose_lawful {W A X : Type} {outer : Lens W A} {inner : Lens A X}
    (ho : Lawful outer) (hi : Lawful inner) : Lawful (compose outer inner) := by
  obtain ⟨og, os, oo⟩ := ho
  obtain ⟨ig, is, ii⟩ := hi
  refine ⟨?_, ?_, ?_⟩
  · intro w x; dsimp only [compose]; rw [og, ig]
  · intro w; dsimp only [compose]; rw [is, os]
  · intro w x y; dsimp only [compose]; rw [og, ii, oo]

/-- Disjoint frame components are preserved by a write through the other. -/
theorem first_frame (w : A × B) (b : B) : first.1 (second.2 w b) = first.1 w := rfl
theorem second_frame (w : A × B) (a : A) : second.1 (first.2 w a) = second.1 w := rfl

/-- The write-back at the end of a clause. `none` is used where the borrowed
place is not part of the frame: a value expression, a place the body never
writes, or a place local to the computation. -/
def patch {W X : Type} : Option (Lens W X) → W → X → W
  | some l, frame, x => l.2 frame x
  | none, frame, _ => frame

theorem patch_reload {W X : Type} (l : Lens W X) (h : Lawful l) (frame : W) :
    patch (some l) frame (l.1 frame) = frame :=
  h.2.1 frame

theorem patch_then_reload {W X : Type} (l : Lens W X) (h : Lawful l) (frame : W) (x : X) :
    l.1 (patch (some l) frame x) = x :=
  h.1 frame x

/-- Two borrowed projections of one root (`inout a.{x}`, `inout b.{y}`) are
written back in sequence; neither clobbers the other. -/
theorem disjoint_patches (w : A × B) (a : A) (b : B) :
    patch (some second) (patch (some first) w a) b = (a, b) := rfl

/-- A concrete check of the encoding against the evaluator's observable
behaviour for `change(x, k) => { x += 1; let r = k(20); x * 1000 + r }`
performed with an outer binding `x = 1` that the body increments by 100
after the operation returns. -/
def clause (lens : Option (Lens Int Int)) (x : Int)
    (resume : Int × Int → Unit × Int → Id (Int × Int)) (st : Unit × Int) : Id (Int × Int) := do
  let mut frame := st.2
  let mut x := x
  x := x + 1
  let r ← resume (20, x) ((), st.2)
  frame := r.2
  match lens with
  | some (get, _) => x := get frame
  | none => pure ()
  let o := x * 1000 + r.1
  pure (o, patch lens frame x)

def body (lens : Option (Lens Int Int)) : HandlerCPS (Unit × Int) (Int × Int) Id Int := do
  let mut x := 1
  putFrame (S := Unit) x
  let t ← (show HandlerCPS (Unit × Int) (Int × Int) Id (Int × Int) from clause lens x)
  x := t.2
  x := x + 100
  putFrame (S := Unit) x
  pure t.1

def run (lens : Option (Lens Int Int)) : Int × Int :=
  body lens (fun r s => pure (r, s.2)) ((), 1)

/-- With the frame lens the clause observes the body's later write (102) and
its own write after resumption persists. Without it the clause would read a
stale snapshot (2); the extractor rejects that case rather than emitting it. -/
theorem lens_observes_resumed_writes : run (some whole) = (102020, 102) := by decide
theorem snapshot_would_differ : run none = (2020, 102) := by decide

end Vesper.EffectPlaces
