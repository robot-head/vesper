import Std

/-!
Handwritten exact-integer model, checked by Lean 4.24. This is not an extractor
or a proof of Rust implementation, bounded arithmetic, compiler or persistence.
The bilinear identity applies pointwise to join contributions; finite support,
row semantics and transactional machine execution are validated by Rust tests.
-/
namespace Vesper.FluxDbsp

theorem bilinear_join_exact (a b da db : Int) :
    a * b + (da * b + a * db + da * db) = (a + da) * (b + db) := by
  simp [Int.add_mul, Int.mul_add, Int.add_assoc, Int.add_left_comm, Int.add_comm]

theorem retraction_cancels (w : Int) : w + (-w) = 0 := by omega

theorem integrate_delta (previous current : Int) :
    previous + (current - previous) = current := by omega

theorem frontier_chain_monotonic (t0 t1 t2 : Int)
    (h01 : t0 ≤ t1) (h12 : t1 ≤ t2) : t0 ≤ t2 := by omega

/-- A published bound respects every live hold, not only a chosen ticket. -/
def respects (watermark : Int) (holds : List Int) : Prop :=
  ∀ t ∈ holds, watermark ≤ t

theorem advance_preserves_live_bound (watermark old next : Int) (rest : List Int)
    (h : respects watermark (old :: rest)) (advance : old ≤ next) :
    respects watermark (next :: rest) := by
  intro t ht
  simp only [List.mem_cons] at ht
  rcases ht with rfl | ht
  · have h_old := h old (by simp)
    omega
  · exact h t (by simp [ht])

theorem retirement_preserves_live_bound (watermark old : Int) (rest : List Int)
    (h : respects watermark (old :: rest)) : respects watermark rest := by
  intro t ht
  exact h t (by simp [ht])

#print axioms bilinear_join_exact
#print axioms retraction_cancels
#print axioms integrate_delta
#print axioms frontier_chain_monotonic
#print axioms advance_preserves_live_bound
#print axioms retirement_preserves_live_bound
/-!
## Durable checkpoint, replay and partitioned execution (wave six)

A model of a deterministic incremental executor as a step function
`step : S → I → S × O`. `run` folds it over an input log and collects outputs.
A checkpoint is an encoding of the state after a prefix; restore is a partial
decoder that accepts every honest encoding (`dec (enc s) = some s`) and is
transactional: a rejected image leaves the live state unchanged. These are
statements about this model only, not about the Rust executor, its byte
format, the content-addressed store, SimPulse or the cluster protocol.
-/

/-- Fold a deterministic step over an input log, collecting outputs. -/
def run {S I O : Type} (step : S → I → S × O) : S → List I → S × List O
  | s, [] => (s, [])
  | s, i :: is =>
    let r := run step (step s i).1 is
    (r.1, (step s i).2 :: r.2)

theorem run_append {S I O : Type} (step : S → I → S × O) (s : S) (xs ys : List I) :
    run step s (xs ++ ys) =
      ((run step (run step s xs).1 ys).1,
       (run step s xs).2 ++ (run step (run step s xs).1 ys).2) := by
  induction xs generalizing s with
  | nil => simp [run]
  | cons x xs ih => simp [run, ih]

/-- Transactional restore: a rejected image keeps the live state. -/
def restore {S C : Type} (dec : C → Option S) (image : C) (live : S) : S :=
  (dec image).getD live

theorem restore_rejected_keeps_live {S C : Type} (dec : C → Option S) (image : C) (live : S)
    (h : dec image = none) : restore dec image live = live := by
  simp [restore, h]

/-- Restoring the checkpoint taken after the first `k` inputs and replaying the
remaining inputs reaches the uninterrupted final state, and the outputs
emitted before the checkpoint followed by the replayed outputs are exactly the
uninterrupted outputs (exactly-once output across the crash). -/
theorem restore_replay_eq_uninterrupted {S I O C : Type}
    (step : S → I → S × O) (enc : S → C) (dec : C → Option S)
    (honest : ∀ s, dec (enc s) = some s) (s0 crashed : S) (xs : List I) (k : Nat) :
    let ck := enc (run step s0 (xs.take k)).1
    let resumed := run step (restore dec ck crashed) (xs.drop k)
    resumed.1 = (run step s0 xs).1 ∧
      (run step s0 (xs.take k)).2 ++ resumed.2 = (run step s0 xs).2 := by
  have h := run_append step s0 (xs.take k) (xs.drop k)
  rw [List.take_append_drop] at h
  simp only [restore, honest, Option.getD_some]
  constructor
  · rw [h]
  · rw [h]

/-- Any number of crashes, each recovering from the same committed checkpoint
(whatever the live state was), still ends in the uninterrupted state. -/
theorem repeated_recovery_eq_uninterrupted {S I O C : Type}
    (step : S → I → S × O) (enc : S → C) (dec : C → Option S)
    (honest : ∀ s, dec (enc s) = some s) (s0 : S) (xs : List I) (k : Nat)
    (crashes : List S) :
    let ck := enc (run step s0 (xs.take k)).1
    let recovered := crashes.foldl (fun _ crashed => restore dec ck crashed) (restore dec ck s0)
    (run step recovered (xs.drop k)).1 = (run step s0 xs).1 := by
  have h := run_append step s0 (xs.take k) (xs.drop k)
  rw [List.take_append_drop] at h
  have fold : ∀ (l : List S),
      l.foldl (fun _ crashed => restore dec (enc (run step s0 (xs.take k)).1) crashed)
        (run step s0 (xs.take k)).1 = (run step s0 (xs.take k)).1 := by
    intro l
    induction l with
    | nil => rfl
    | cons c cs ih => simpa [List.foldl, restore, honest] using ih
  simp only [restore, honest, Option.getD_some] at fold ⊢
  rw [fold, h]

/-- Z-sets as finitely supported functions are modelled as total functions;
`zsum` adds a list of worker-local Z-sets pointwise. -/
def zsum {α : Type} : List (α → Int) → α → Int
  | [] => fun _ => 0
  | f :: fs => fun a => f a + zsum fs a

/-- A linear (additive, zero-preserving) operator commutes with the sum of
worker partitions: summing worker outputs equals the single-worker output. -/
theorem linear_distributes_over_partitions {α β : Type}
    (L : (α → Int) → β → Int)
    (additive : ∀ f g, L (fun a => f a + g a) = fun b => L f b + L g b)
    (zero : L (fun _ => 0) = fun _ => 0) (parts : List (α → Int)) :
    L (zsum parts) = zsum (parts.map L) := by
  induction parts with
  | nil => simpa [zsum] using zero
  | cons f fs ih =>
    show L (fun a => f a + zsum fs a) = fun b => L f b + zsum (fs.map L) b
    rw [additive, ih]

/-- Co-partitioned join, pointwise on one key: if every row with key `k`
lives on exactly one worker `w` of a duplicate-free worker list, the sum of
per-worker join contributions for `k` equals the join of the summed inputs. -/
theorem copartitioned_join_key {W : Type} [DecidableEq W] (ws : List W) (nodup : ws.Nodup)
    (w : W) (hw : w ∈ ws) (a b : W → Int)
    (onlyA : ∀ v, v ≠ w → a v = 0) (onlyB : ∀ v, v ≠ w → b v = 0) :
    ws.foldr (fun v acc => a v * b v + acc) 0 =
      (ws.foldr (fun v acc => a v + acc) 0) * (ws.foldr (fun v acc => b v + acc) 0) := by
  have single : ∀ (f : W → Int), (∀ v, v ≠ w → f v = 0) →
      ws.foldr (fun v acc => f v + acc) 0 = f w := by
    intro f hf
    have key : ∀ (l : List W), l.Nodup →
        l.foldr (fun v acc => f v + acc) 0 = if w ∈ l then f w else 0 := by
      intro l hl
      induction l with
      | nil => simp
      | cons x xs ih =>
        have hx : x ∉ xs := (List.nodup_cons.mp hl).1
        have ih := ih (List.nodup_cons.mp hl).2
        simp only [List.foldr, ih, List.mem_cons]
        by_cases hxw : x = w
        · subst hxw; simp [hx]
        · have : f x = 0 := hf x hxw
          by_cases hw : w ∈ xs
          · simp [this, hw]
          · simp [this, hw, Ne.symm hxw]
    rw [key ws nodup]
    simp [hw]
  rw [single a onlyA, single b onlyB, single (fun v => a v * b v)]
  intro v hv
  simp [onlyA v hv]

#print axioms run_append
#print axioms restore_rejected_keeps_live
#print axioms restore_replay_eq_uninterrupted
#print axioms repeated_recovery_eq_uninterrupted
#print axioms linear_distributes_over_partitions
#print axioms copartitioned_join_key
end Vesper.FluxDbsp
