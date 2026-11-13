/-!
# Vesper Resonance: SPSC ring slot safety and delay compensation (design §32.4)

Model identifier: `vesper.resonance.spsc-pdc-model/1`.

Two small models, each proved in full. Neither is a theorem about Rust code:
`crates/vesper-resonance` uses the `rtrb` ring (whose index protocol is the
one modelled here) and its own PDC compiler (whose latency equations are the
ones modelled here); Rust tests check the same properties on concrete runs.

## 1. SPSC index protocol

A ring of `cap > 0` slots with monotone positions `read ≤ write ≤ read + cap`.
Position `p` lives in slot `p % cap`. The producer writes slot `write % cap`
when `write < read + cap`, then publishes `write + 1`; the consumer reads slot
`read % cap` when `read < write`, then publishes `read + 1`. Each side's
operation is split into a start (choosing its slot from the indices it has
seen) and an end (publishing), and the two sides interleave arbitrarily.

What is proved:

* `spsc_ring_occupancy_bounded` (the §32.4 statement): readable plus
  writable slots equal `cap`, and readable never exceeds `cap`.
* `slot_distinct`: two positions less than `cap` apart use different slots.
* `producer_slot_not_readable`: the slot the producer may write is not the
  slot of any readable position `[read, write)`.
* `step_preserves` / `reachable_inv`: every interleaving of producer and
  consumer start/end steps from the empty ring preserves the invariant.
* `spsc_no_shared_slot`: in every reachable state, if the producer and the
  consumer are both inside their slot access, their slots differ. So the two
  threads never access the same slot concurrently *in this model*.

## 2. Plugin delay compensation

Nodes are naturals; `edge s t` is a forward edge; `lat` is intrinsic latency.
The compiler sets `arrival t = max` of `out s` over the inputs of `t`,
`out t = arrival t + lat t`, and inserts `comp s t = arrival t - out s` on each
edge. A path's latency is the sum, over its edges `u → v`, of
`comp u v + lat v`.

What is proved:

* `pdc_equalizes_parallel_branch_latency` (the §32.4 statement).
* `le_maxList`: every input's latency is at most the computed arrival, so
  the inserted delay is a natural-number difference that does not truncate.
* `compiled_is_compensated`: the compiler's equations give a compensated DAG.
* `path_latency`: every path from `s` to `t` has latency `out t - out s`.
* `pdc_paths_equal`: all paths between two nodes have equal total latency
  after compensation (exact phase alignment).

Feedback edges carry an explicit author-chosen delay and are outside the
compensated forward DAG; they are not part of this model.
-/

namespace Vesper.ResonanceSpsc

def modelId : String := "vesper.resonance.spsc-pdc-model/1"
def modelClaim : String :=
  "SPSC index-protocol and PDC latency models; not a proof about the Rust implementation or rtrb"

/-! ## SPSC ring -/

theorem spsc_ring_occupancy_bounded
    (cap writePos readPos : Nat)
    (_h_order : readPos ≤ writePos)
    (h_full : writePos - readPos ≤ cap) :
    let readable := writePos - readPos
    let writable := cap - readable
    readable + writable = cap ∧ readable ≤ cap := by
  omega

/-- Two positions strictly less than `cap` apart occupy different slots. -/
theorem slot_distinct (cap a b : Nat) (hab : a < b) (hlt : b < a + cap) :
    a % cap ≠ b % cap := by
  intro h
  have hz : (b - a) % cap = 0 := Nat.sub_mod_eq_zero_of_mod_eq h.symm
  have hsmall : b - a < cap := by omega
  rw [Nat.mod_eq_of_lt hsmall] at hz
  omega

structure Ring where
  cap : Nat
  read : Nat
  write : Nat

def Ring.Inv (r : Ring) : Prop :=
  0 < r.cap ∧ r.read ≤ r.write ∧ r.write ≤ r.read + r.cap

theorem producer_slot_not_readable (r : Ring) (_hinv : r.Inv)
    (hpush : r.write < r.read + r.cap) (i : Nat) (hlo : r.read ≤ i) (hhi : i < r.write) :
    r.write % r.cap ≠ i % r.cap := by
  intro h
  exact slot_distinct r.cap i r.write hhi (by omega) h.symm

/-- Concurrent state: the ring plus each side's in-flight slot access.
`prod = some p` means the producer is writing position `p`; `cons = some c`
means the consumer is reading position `c`. -/
structure State where
  ring : Ring
  prod : Option Nat
  cons : Option Nat

def State.Inv (s : State) : Prop :=
  s.ring.Inv ∧
  (∀ p, s.prod = some p → p = s.ring.write ∧ s.ring.write < s.ring.read + s.ring.cap) ∧
  (∀ c, s.cons = some c → c = s.ring.read ∧ s.ring.read < s.ring.write)

/-- Interleaved producer/consumer steps. -/
inductive Step : State → State → Prop
  | prodStart (r : Ring) (c : Option Nat) (h : r.write < r.read + r.cap) :
      Step ⟨r, none, c⟩ ⟨r, some r.write, c⟩
  | prodEnd (r : Ring) (p : Nat) (c : Option Nat) :
      Step ⟨r, some p, c⟩ ⟨{ r with write := r.write + 1 }, none, c⟩
  | consStart (r : Ring) (p : Option Nat) (h : r.read < r.write) :
      Step ⟨r, p, none⟩ ⟨r, p, some r.read⟩
  | consEnd (r : Ring) (p : Option Nat) (c : Nat) :
      Step ⟨r, p, some c⟩ ⟨{ r with read := r.read + 1 }, p, none⟩

def init (cap : Nat) : State := ⟨⟨cap, 0, 0⟩, none, none⟩

inductive Reachable (cap : Nat) : State → Prop
  | init : Reachable cap (init cap)
  | step {s t : State} : Reachable cap s → Step s t → Reachable cap t

theorem step_preserves {s t : State} (hs : s.Inv) (st : Step s t) : t.Inv := by
  cases st with
  | prodStart r c h =>
    obtain ⟨hr, _, hc⟩ := hs
    dsimp only at *
    refine ⟨hr, ?_, hc⟩
    intro p hp'
    simp only [Option.some.injEq] at hp'
    exact ⟨hp'.symm, h⟩
  | prodEnd r p c =>
    obtain ⟨⟨hcap, hrw, _⟩, hp, hc⟩ := hs
    dsimp only at *
    have hpush := (hp p rfl).2
    refine ⟨⟨hcap, ?_, ?_⟩, ?_, ?_⟩
    · show r.read ≤ r.write + 1
      omega
    · show r.write + 1 ≤ r.read + r.cap
      omega
    · intro p' hp'
      exact absurd hp' (by simp)
    · intro c' hc'
      have hcc := hc c' hc'
      exact ⟨hcc.1, show r.read < r.write + 1 by omega⟩
  | consStart r p h =>
    obtain ⟨hr, hp, _⟩ := hs
    dsimp only at *
    refine ⟨hr, hp, ?_⟩
    intro c hc'
    simp only [Option.some.injEq] at hc'
    exact ⟨hc'.symm, h⟩
  | consEnd r p c =>
    obtain ⟨⟨hcap, _, hwc⟩, hp, hc⟩ := hs
    dsimp only at *
    have hpop := (hc c rfl).2
    refine ⟨⟨hcap, ?_, ?_⟩, ?_, ?_⟩
    · show r.read + 1 ≤ r.write
      omega
    · show r.write ≤ r.read + 1 + r.cap
      omega
    · intro p' hp'
      have hpp := hp p' hp'
      exact ⟨hpp.1, show r.write < r.read + 1 + r.cap by omega⟩
    · intro c' hc'
      exact absurd hc' (by simp)

theorem reachable_inv {cap : Nat} (hcap : 0 < cap) {s : State} (h : Reachable cap s) : s.Inv := by
  induction h with
  | init =>
    refine ⟨⟨hcap, Nat.le_refl 0, Nat.zero_le _⟩, ?_, ?_⟩ <;> intro x hx <;> simp [init] at hx
  | step _ st ih => exact step_preserves ih st

/-- Slot safety: the producer and consumer never access the same slot at the
same time, in any interleaving. -/
theorem spsc_no_shared_slot {cap : Nat} (hcap : 0 < cap) {s : State} (h : Reachable cap s)
    (p c : Nat) (hp : s.prod = some p) (hc : s.cons = some c) :
    p % s.ring.cap ≠ c % s.ring.cap := by
  obtain ⟨_, hP, hC⟩ := reachable_inv hcap h
  obtain ⟨rfl, hpush⟩ := hP p hp
  obtain ⟨rfl, hpop⟩ := hC c hc
  intro heq
  exact slot_distinct s.ring.cap s.ring.read s.ring.write hpop hpush heq.symm

/-! ## Plugin delay compensation -/

theorem pdc_equalizes_parallel_branch_latency
    (branchLat maxLat : Nat) (h_le : branchLat ≤ maxLat) :
    branchLat + (maxLat - branchLat) = maxLat := by
  omega

def maxList (l : List Nat) : Nat := l.foldl max 0

theorem le_foldl_max (l : List Nat) : ∀ (init x : Nat), x ∈ l ∨ x ≤ init → x ≤ l.foldl max init := by
  induction l with
  | nil => intro init x h; simp at h; simpa using h
  | cons y ys ih =>
    intro init x h
    simp only [List.foldl]
    apply ih
    rcases h with h | h
    · simp at h
      rcases h with rfl | h
      · exact Or.inr (Nat.le_max_right _ _)
      · exact Or.inl h
    · exact Or.inr (Nat.le_trans h (Nat.le_max_left _ _))

theorem le_maxList (l : List Nat) (x : Nat) (h : x ∈ l) : x ≤ maxList l :=
  le_foldl_max l 0 x (Or.inl h)

structure Dag where
  lat : Nat → Nat
  out : Nat → Nat
  arrival : Nat → Nat
  edge : Nat → Nat → Prop

def Dag.comp (d : Dag) (s t : Nat) : Nat := d.arrival t - d.out s

structure Compensated (d : Dag) : Prop where
  out_eq : ∀ t, d.out t = d.arrival t + d.lat t
  arrival_ge : ∀ s t, d.edge s t → d.out s ≤ d.arrival t

/-- The compiler's equations (arrival is the max input latency) give a
compensated DAG. -/
theorem compiled_is_compensated (inputs : Nat → List Nat) (lat out : Nat → Nat)
    (h_out : ∀ t, out t = maxList ((inputs t).map out) + lat t) :
    Compensated ⟨lat, out, fun t => maxList ((inputs t).map out), fun s t => s ∈ inputs t⟩ := by
  refine ⟨h_out, ?_⟩
  intro s t hst
  exact le_maxList _ _ (List.mem_map.mpr ⟨s, hst, rfl⟩)

/-- `Path d s t k`: a forward path from `s` to `t` with compensated latency `k`. -/
inductive Path (d : Dag) : Nat → Nat → Nat → Prop
  | refl (s : Nat) : Path d s s 0
  | step {s u v k : Nat} : Path d s u k → d.edge u v → Path d s v (k + d.comp u v + d.lat v)

theorem path_latency {d : Dag} (h : Compensated d) {s t k : Nat} (p : Path d s t k) :
    d.out s + k = d.out t := by
  induction p with
  | refl => simp
  | @step u v k _ e ih =>
    have hge := h.arrival_ge u v e
    have hout := h.out_eq v
    unfold Dag.comp
    omega

/-- Exact phase alignment: all compensated paths between two nodes carry the
same total latency. -/
theorem pdc_paths_equal {d : Dag} (h : Compensated d) {s t k₁ k₂ : Nat}
    (p₁ : Path d s t k₁) (p₂ : Path d s t k₂) : k₁ = k₂ := by
  have h₁ := path_latency h p₁
  have h₂ := path_latency h p₂
  omega

end Vesper.ResonanceSpsc

#print axioms Vesper.ResonanceSpsc.spsc_ring_occupancy_bounded
#print axioms Vesper.ResonanceSpsc.slot_distinct
#print axioms Vesper.ResonanceSpsc.producer_slot_not_readable
#print axioms Vesper.ResonanceSpsc.step_preserves
#print axioms Vesper.ResonanceSpsc.reachable_inv
#print axioms Vesper.ResonanceSpsc.spsc_no_shared_slot
#print axioms Vesper.ResonanceSpsc.pdc_equalizes_parallel_branch_latency
#print axioms Vesper.ResonanceSpsc.le_maxList
#print axioms Vesper.ResonanceSpsc.compiled_is_compensated
#print axioms Vesper.ResonanceSpsc.path_latency
#print axioms Vesper.ResonanceSpsc.pdc_paths_equal
