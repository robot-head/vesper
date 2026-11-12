/-
  Vesper Silicon (§26.4): W1C register-write safety and physical-frame
  conservation, proved about small models.

  What is proved, and about what:

  * `W1C`: a register is `Fin n → Bool`. The modelled write rule for a
    write-1-to-clear register is `w1cWrite hw w i = hw i && !w i`.
    `w1c_preserves_unwritten` says every bit not set in the written mask keeps
    its value; `w1c_single_ack` specialises it to acknowledging one bit;
    `rmw_clears_all_pending` shows the hazard the checker rejects: writing back
    the value just read clears every pending bit.
  * `Frames`: the physical-memory state is a list of `(pfn, location)` pairs.
    Each operation (`alloc`, `map`, `unmap`, `shootdown`, `dmaSubmit`,
    `dmaComplete`, `free`) only changes the location of one frame, and only
    along the transition table. `step_preserves_pfns` shows the multiset of
    frames is unchanged by any step (so no frame is created, lost or
    duplicated), `count_partition` shows the five location counts sum to the
    total, and `no_reuse_without_shootdown` shows no step moves a frame from
    `mapped` straight to `free` or into `held` without passing through
    `pending` and a shootdown.

  These are theorems about these models only. They are not proofs about the
  Rust code in crates/vesper-silicon, whose behaviour is checked by its tests.
-/

namespace Vesper.Silicon

/-! ## W1C register write rule -/

def w1cWrite {n : Nat} (hw w : Fin n → Bool) : Fin n → Bool :=
  fun i => hw i && !w i

theorem w1c_preserves_unwritten {n : Nat} (hw w : Fin n → Bool) (i : Fin n)
    (h : w i = false) : w1cWrite hw w i = hw i := by
  simp [w1cWrite, h]

theorem w1c_clears_written {n : Nat} (hw w : Fin n → Bool) (i : Fin n)
    (h : w i = true) : w1cWrite hw w i = false := by
  simp [w1cWrite, h]

/-- One-hot mask with only bit `k` set. -/
def oneHot {n : Nat} (k : Fin n) : Fin n → Bool := fun i => decide (i = k)

/-- §26.3 `write_clear_mask(1 << k)`: every sibling pending bit survives. -/
theorem w1c_single_ack {n : Nat} (hw : Fin n → Bool) (k i : Fin n) (h : i ≠ k) :
    w1cWrite hw (oneHot k) i = hw i := by
  apply w1c_preserves_unwritten
  simp [oneHot, h]

/-- Read-modify-write on a W1C register writes back the pending bits,
    clearing all of them: the hazard `silicon.mmio.rmw_of_write_clear_register`. -/
theorem rmw_clears_all_pending {n : Nat} (hw extra : Fin n → Bool) (i : Fin n) :
    w1cWrite hw (fun j => hw j || extra j) i = false := by
  cases h : hw i <;> simp [w1cWrite, h]

/-! ## Physical frame conservation -/

inductive Loc where
  | free
  | held
  | mapped
  | pending
  | dma
  deriving DecidableEq, Repr

abbrev State := List (Nat × Loc)

inductive Op where
  | alloc (pfn : Nat)
  | free (pfn : Nat)
  | map (pfn : Nat)
  | unmap (pfn : Nat)
  | shootdown (pfn : Nat)
  | dmaSubmit (pfn : Nat)
  | dmaComplete (pfn : Nat)
  deriving Repr

/-- The only allowed location transitions, per operation. -/
def Op.pfn : Op → Nat
  | .alloc p | .free p | .map p | .unmap p | .shootdown p | .dmaSubmit p | .dmaComplete p => p

def Op.src : Op → Loc
  | .alloc _ => .free
  | .free _ => .held
  | .map _ => .held
  | .unmap _ => .mapped
  | .shootdown _ => .pending
  | .dmaSubmit _ => .held
  | .dmaComplete _ => .dma

def Op.dst : Op → Loc
  | .alloc _ => .held
  | .free _ => .free
  | .map _ => .mapped
  | .unmap _ => .pending
  | .shootdown _ => .held
  | .dmaSubmit _ => .dma
  | .dmaComplete _ => .held

/-- Apply `op` to every entry: the matching frame moves if it is at the
    operation's source location; everything else is unchanged. -/
def moveEntry (op : Op) (e : Nat × Loc) : Nat × Loc :=
  if e.1 = op.pfn ∧ e.2 = op.src then (e.1, op.dst) else e

def step (op : Op) (s : State) : State := s.map (moveEntry op)

theorem moveEntry_fst (op : Op) (e : Nat × Loc) : (moveEntry op e).1 = e.1 := by
  unfold moveEntry
  split <;> rfl

/-- No step creates, loses or duplicates a frame. -/
theorem step_preserves_pfns (op : Op) (s : State) :
    (step op s).map Prod.fst = s.map Prod.fst := by
  induction s with
  | nil => rfl
  | cons e s ih =>
    simp only [step, List.map_cons] at ih ⊢
    rw [moveEntry_fst, ih]

theorem steps_preserve_pfns (ops : List Op) (s : State) :
    (ops.foldl (fun st op => step op st) s).map Prod.fst = s.map Prod.fst := by
  induction ops generalizing s with
  | nil => rfl
  | cons op ops ih =>
    simp only [List.foldl_cons]
    rw [ih, step_preserves_pfns]

/-- A duplicate-free set of frames stays duplicate-free. -/
theorem step_preserves_nodup (op : Op) (s : State)
    (h : (s.map Prod.fst).Nodup) : ((step op s).map Prod.fst).Nodup := by
  rw [step_preserves_pfns]
  exact h

def count (l : Loc) (s : State) : Nat := (s.filter (fun e => decide (e.2 = l))).length

/-- `total_frames = free + held + mapped + pending + dma`. -/
theorem count_partition (s : State) :
    count .free s + count .held s + count .mapped s + count .pending s + count .dma s
      = s.length := by
  induction s with
  | nil => rfl
  | cons e s ih =>
    obtain ⟨p, l⟩ := e
    cases l <;> simp [count] at ih ⊢ <;> omega

theorem step_length (op : Op) (s : State) : (step op s).length = s.length := by
  simp [step]

/-- Unmapping and recycling (§26.4 statement): totals are conserved by any step. -/
theorem step_conserves_total (op : Op) (s : State) :
    count .free (step op s) + count .held (step op s) + count .mapped (step op s)
        + count .pending (step op s) + count .dma (step op s)
      = count .free s + count .held s + count .mapped s + count .pending s + count .dma s := by
  rw [count_partition, count_partition, step_length]

/-- No single step takes a mapped frame anywhere but `pending`. -/
theorem mapped_only_to_pending (op : Op) (e : Nat × Loc) (h : e.2 = .mapped) :
    (moveEntry op e).2 = .mapped ∨ (moveEntry op e).2 = .pending := by
  unfold moveEntry
  split
  · rename_i hm
    obtain ⟨_, hs⟩ := hm
    rw [h] at hs
    cases op <;> simp [Op.src] at hs <;> simp [Op.dst]
  · exact Or.inl h

/-- A frame only leaves `pending` through `shootdown`. -/
theorem pending_exit_is_shootdown (op : Op) (e : Nat × Loc) (h : e.2 = .pending)
    (h' : (moveEntry op e).2 ≠ .pending) : ∃ p, op = .shootdown p := by
  unfold moveEntry at h'
  split at h'
  · rename_i hm
    obtain ⟨_, hs⟩ := hm
    rw [h] at hs
    cases op with
    | shootdown p => exact ⟨p, rfl⟩
    | _ => simp [Op.src] at hs
  · exact absurd h h'

/-- No step takes a frame from `mapped` directly to `free` or `held`:
    reuse requires the pending shootdown. -/
theorem no_reuse_without_shootdown (op : Op) (e : Nat × Loc) (h : e.2 = .mapped) :
    (moveEntry op e).2 ≠ .free ∧ (moveEntry op e).2 ≠ .held := by
  rcases mapped_only_to_pending op e h with h1 | h1 <;> rw [h1] <;> simp

/-! Executable sanity check of the model on a concrete trace. -/

def demo : State := [(1, .free), (2, .free), (3, .free)]

def demoOps : List Op :=
  [.alloc 1, .map 1, .alloc 2, .dmaSubmit 2, .unmap 1, .shootdown 1, .free 1, .dmaComplete 2]

def demoFinal : State := demoOps.foldl (fun st op => step op st) demo

example : demoFinal = [(1, .free), (2, .held), (3, .free)] := by decide

/-- Shooting down before unmapping does nothing: the frame stays mapped. -/
example : step (.shootdown 1) [(1, .mapped)] = [(1, .mapped)] := by decide

end Vesper.Silicon
