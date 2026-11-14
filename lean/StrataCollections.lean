/-
  Strata collections (§15.5): a model of the SlotArena generation check.

  The model is a total map from slot indices to a generation and an optional
  value. A key names a slot and the generation it was issued at. Lookup
  succeeds only when the key's generation equals the slot's current one;
  removal clears the slot and bumps its generation.

  Proved here, about this Lean model only (not about the Rust code in
  crates/vesper-alloc, whose RegionSlotArena draws generations from an
  arena-wide counter rather than a per-slot one):
  * generations never decrease under any operation sequence;
  * once a key's value is removed, that key never reads a value again, under
    any later sequence of insertions and removals, even when the slot is
    reused (stale handles never alias a live slot);
  * a key issued by a later insertion differs from every stale key;
  * two keys that both read a value from the same slot are equal.
  Core Lean 4.24 only; no Mathlib, no `sorry`, no new axioms.
-/
namespace Vesper.StrataCollections

def modelId : String := "vesper.strata.slot-arena-generations/1"

def upd {β : Type} (f : Nat → β) (s : Nat) (v : β) : Nat → β :=
  fun i => if i = s then v else f i

theorem upd_same {β : Type} (f : Nat → β) (s : Nat) (v : β) : upd f s v s = v := by
  simp [upd]

theorem upd_other {β : Type} (f : Nat → β) (s i : Nat) (v : β) (h : i ≠ s) :
    upd f s v i = f i := by
  simp [upd, h]

structure Key where
  slot : Nat
  gen : Nat
  deriving DecidableEq, Repr

structure Arena (α : Type) where
  gen : Nat → Nat
  val : Nat → Option α

variable {α : Type}

def Arena.empty : Arena α := ⟨fun _ => 0, fun _ => none⟩

def Arena.get (a : Arena α) (k : Key) : Option α :=
  if a.gen k.slot = k.gen then a.val k.slot else none

/-- Insert into a vacant slot, issuing a key at its current generation. -/
def Arena.insert (a : Arena α) (s : Nat) (v : α) : Arena α × Option Key :=
  match a.val s with
  | none => ({ a with val := upd a.val s (some v) }, some ⟨s, a.gen s⟩)
  | some _ => (a, none)

/-- Remove through a valid key: the value leaves and the generation bumps. -/
def Arena.remove (a : Arena α) (k : Key) : Arena α × Option α :=
  match a.get k with
  | some v => (⟨upd a.gen k.slot (a.gen k.slot + 1), upd a.val k.slot none⟩, some v)
  | none => (a, none)

inductive Op (α : Type) where
  | insert (s : Nat) (v : α)
  | remove (k : Key)

def step (a : Arena α) : Op α → Arena α
  | .insert s v => (a.insert s v).1
  | .remove k => (a.remove k).1

def run (a : Arena α) : List (Op α) → Arena α
  | [] => a
  | o :: os => run (step a o) os

theorem get_some_gen {a : Arena α} {k : Key} {v : α} (h : a.get k = some v) :
    a.gen k.slot = k.gen := by
  unfold Arena.get at h
  split at h
  · assumption
  · cases h

theorem step_gen_mono (a : Arena α) (o : Op α) (i : Nat) : a.gen i ≤ (step a o).gen i := by
  cases o with
  | insert s v =>
    simp only [step, Arena.insert]
    split <;> exact Nat.le_refl _
  | remove k =>
    simp only [step, Arena.remove]
    split
    · simp only [upd]
      split
      · subst_vars; exact Nat.le_succ _
      · exact Nat.le_refl _
    · exact Nat.le_refl _

theorem run_gen_mono (a : Arena α) (os : List (Op α)) (i : Nat) :
    a.gen i ≤ (run a os).gen i := by
  induction os generalizing a with
  | nil => exact Nat.le_refl _
  | cons o os ih => exact Nat.le_trans (step_gen_mono a o i) (ih (step a o))

theorem get_stale {a : Arena α} {k : Key} (h : k.gen < a.gen k.slot) : a.get k = none := by
  unfold Arena.get
  split
  · omega
  · rfl

theorem remove_makes_stale {a : Arena α} {k : Key} {v : α} (h : (a.remove k).2 = some v) :
    k.gen < (a.remove k).1.gen k.slot := by
  unfold Arena.remove at *
  split at h
  · rename_i w hw
    simp only [upd_same]
    have := get_some_gen hw
    omega
  · cases h

/-- Stale handles never alias a live slot: after a successful removal the key
    reads nothing under every later operation sequence. -/
theorem stale_key_never_reads (a : Arena α) (k : Key) (v : α) (os : List (Op α))
    (h : (a.remove k).2 = some v) : (run (a.remove k).1 os).get k = none :=
  get_stale (Nat.lt_of_lt_of_le (remove_makes_stale h) (run_gen_mono _ os k.slot))

/-- A newly issued key is never equal to a stale key. -/
theorem fresh_key_differs_from_stale (a : Arena α) (k k' : Key) (s : Nat) (v : α)
    (hs : k.gen < a.gen k.slot) (hi : (a.insert s v).2 = some k') : k' ≠ k := by
  unfold Arena.insert at hi
  split at hi
  · cases hi
    intro heq
    cases k
    simp only [Key.mk.injEq] at heq
    obtain ⟨h1, h2⟩ := heq
    subst h1
    simp only at hs
    omega
  · cases hi

/-- Two keys that both read a value from one slot are the same key. -/
theorem live_keys_unique (a : Arena α) (k k' : Key) (v v' : α)
    (h : a.get k = some v) (h' : a.get k' = some v') (hs : k.slot = k'.slot) : k = k' := by
  have g := get_some_gen h
  have g' := get_some_gen h'
  cases k; cases k'
  simp only at hs g g'
  subst hs
  simp only [Key.mk.injEq, true_and]
  omega

/-- A concrete trace: insert, remove, reuse the slot; the stale key fails. -/
def trace : Option Nat × Option Nat × Option Nat :=
  let a0 : Arena Nat := Arena.empty
  let (a1, k1) := a0.insert 0 10
  let k1 := k1.getD ⟨0, 0⟩
  let (a2, _) := a1.remove k1
  let (a3, k2) := a2.insert 0 20
  let k2 := k2.getD ⟨0, 0⟩
  (a1.get k1, a3.get k1, a3.get k2)

#guard trace == (some 10, none, some 20)

#print axioms Vesper.StrataCollections.step_gen_mono
#print axioms Vesper.StrataCollections.run_gen_mono
#print axioms Vesper.StrataCollections.get_stale
#print axioms Vesper.StrataCollections.remove_makes_stale
#print axioms Vesper.StrataCollections.stale_key_never_reads
#print axioms Vesper.StrataCollections.fresh_key_differs_from_stale
#print axioms Vesper.StrataCollections.live_keys_unique

end Vesper.StrataCollections
