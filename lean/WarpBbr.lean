/-!
# Vesper Warp BBRv3: EDT pacing, cwnd clamp and send-guard model

Model identifier: `vesper.warp.bbr.pacing-model/1`.

This file transcribes the pure integer functions of
`crates/vesper-warp-bbr/src/pacing.rs` into Lean over `Nat`, with the
64-bit saturation made explicit:

* `pacingDelay bytes rate`  ↔ `pacing_delay_ns`   (`ceil(bytes·10⁹ / rate)`,
  `u64::MAX` for a zero rate, clamped to `u64::MAX`);
* `edtStep prev now delay`  ↔ `edt_step`          (`launch = max(now, prev)`,
  `next = launch +sat delay`);
* `targetCwnd cwnd mss hi`  ↔ `clamp_target_cwnd` (§35.5 `target_cwnd_bytes`);
* `sendAllowed inflight cwnd` ↔ `send_allowed`    (`inflight < cwnd`).

It proves, about these Lean definitions:

* `bbrv3_edt_launch_monotonic` (§35.4 theorem 2): the launch time is at
  least both `now` and the previous next-departure time, and the new
  next-departure time is at least the launch and the previous one, including
  at saturation;
* `edtRun_ascending`: over any sequence of `(now, delay)` inputs (arbitrary
  interleavings, `now` not necessarily increasing), launch times never
  decrease;
* `pacingDelay_rounding`: for a nonzero rate and a `u32` packet size, the
  delay is the exact ceiling: `bytes·10⁹ ≤ delay·rate < bytes·10⁹ + rate`;
* `targetCwnd_bounds`: the target window lies in
  `[4·mss, max(hi, 4·mss)]`;
* `send_respects_cwnd` and `sendTrace_bound`: a send that the guard allows
  leaves `inflight < cwnd + maxPkt`, on every send transition of any trace
  of sends, releases and arbitrary window changes. `global_bound_fails`
  shows the bound is about send transitions only: after a window cut,
  in-flight data can exceed `cwnd + maxPkt`, and `cut_blocks_sends` shows
  the guard then refuses every send until in-flight data falls below the
  new window.

It is **not** a proof about the Rust code, the BBRv3 state machine (how the
window and pacing rate are chosen), rate sampling, or real pacing hardware.
The Rust test `crates/vesper-warp-bbr/tests/lean_model.rs` compares these
definitions with the Rust functions on concrete inputs (a differential test,
not a refinement proof).
-/

namespace Vesper.WarpBbr

def modelId : String := "vesper.warp.bbr.pacing-model/1"
def modelClaim : String :=
  "Lean transcription of pacing.rs; proofs cover the transcription, not Rust"

/-- `u64::MAX`. -/
def U64MAX : Nat := 2 ^ 64 - 1

def nanosPerSec : Nat := 1000000000

/-- `u64::saturating_add` on in-range operands. -/
def satAdd (a b : Nat) : Nat := min (a + b) U64MAX

/-- Ceiling division (`u128::div_ceil`). -/
def ceilDiv (n r : Nat) : Nat := (n + r - 1) / r

def pacingDelay (bytes rate : Nat) : Nat :=
  if rate = 0 then U64MAX else min (ceilDiv (bytes * nanosPerSec) rate) U64MAX

def edtStep (prev now delay : Nat) : Nat × Nat :=
  let launch := max now prev
  (launch, satAdd launch delay)

def targetCwnd (cwnd mss hi : Nat) : Nat :=
  min (max cwnd (4 * mss)) (max hi (4 * mss))

def sendAllowed (inflight cwnd : Nat) : Bool := decide (inflight < cwnd)

/-! ## EDT monotonicity (§35.4 theorem 2) -/

theorem bbrv3_edt_launch_monotonic (prev now delay : Nat)
    (hprev : prev ≤ U64MAX) (hnow : now ≤ U64MAX) :
    now ≤ (edtStep prev now delay).1 ∧
    prev ≤ (edtStep prev now delay).1 ∧
    (edtStep prev now delay).1 ≤ (edtStep prev now delay).2 ∧
    prev ≤ (edtStep prev now delay).2 ∧
    (edtStep prev now delay).1 ≤ U64MAX ∧
    (edtStep prev now delay).2 ≤ U64MAX := by
  simp only [edtStep, satAdd]
  omega

/-- Launch times for a sequence of `(now, delay)` requests. -/
def edtRun (prev : Nat) : List (Nat × Nat) → List Nat
  | [] => []
  | (now, delay) :: rest =>
    let s := edtStep prev now delay
    s.1 :: edtRun s.2 rest

def Ascending : List Nat → Prop
  | [] => True
  | [_] => True
  | a :: b :: rest => a ≤ b ∧ Ascending (b :: rest)

theorem ascending_cons (a : Nat) (t : List Nat)
    (hle : ∀ y ∈ t, a ≤ y) (ht : Ascending t) : Ascending (a :: t) := by
  cases t with
  | nil => trivial
  | cons b rest => exact ⟨hle b (List.mem_cons_self ..), ht⟩

theorem edtRun_spec (xs : List (Nat × Nat)) :
    ∀ prev, prev ≤ U64MAX → (∀ x ∈ xs, x.1 ≤ U64MAX) →
      (∀ l ∈ edtRun prev xs, prev ≤ l ∧ l ≤ U64MAX) ∧ Ascending (edtRun prev xs) := by
  induction xs with
  | nil => intro prev _ _; exact ⟨(fun _ h => nomatch h), by simp [edtRun, Ascending]⟩
  | cons x rest ih =>
    intro prev hprev hall
    obtain ⟨now, delay⟩ := x
    have hnow : now ≤ U64MAX := hall (now, delay) (List.mem_cons_self ..)
    have hrest : ∀ y ∈ rest, y.1 ≤ U64MAX := fun y hy => hall y (List.mem_cons_of_mem _ hy)
    have hstep := bbrv3_edt_launch_monotonic prev now delay hprev hnow
    obtain ⟨_, hl, hln, _, hlmax, hnmax⟩ := hstep
    obtain ⟨ihmem, ihasc⟩ := ih _ hnmax hrest
    simp only [edtRun]
    refine ⟨?_, ?_⟩
    · intro l hmem
      cases hmem with
      | head => exact ⟨hl, hlmax⟩
      | tail _ h =>
        have := ihmem l h
        omega
    · apply ascending_cons _ _ _ ihasc
      intro y hy
      have := ihmem y hy
      omega

/-- Arbitrary interleavings: launches never regress. -/
theorem edtRun_ascending (prev : Nat) (xs : List (Nat × Nat))
    (hprev : prev ≤ U64MAX) (hall : ∀ x ∈ xs, x.1 ≤ U64MAX) :
    Ascending (edtRun prev xs) :=
  (edtRun_spec xs prev hprev hall).2

/-! ## Pacing delay rounding -/

theorem ceilDiv_bounds (n r : Nat) (hr : 0 < r) :
    n ≤ ceilDiv n r * r ∧ ceilDiv n r * r < n + r := by
  have hdm := Nat.div_add_mod (n + r - 1) r
  have hmod := Nat.mod_lt (n + r - 1) hr
  simp only [ceilDiv]
  rw [Nat.mul_comm]
  generalize r * ((n + r - 1) / r) = t at *
  omega

theorem ceilDiv_le (n r : Nat) (hr : 0 < r) : ceilDiv n r ≤ n := by
  have ⟨_, hlt⟩ := ceilDiv_bounds n r hr
  apply Nat.le_of_not_lt
  intro hgt
  have h1 : (n + 1) * r ≤ ceilDiv n r * r := Nat.mul_le_mul_right r hgt
  have h2 : n ≤ n * r := Nat.le_mul_of_pos_right n hr
  rw [Nat.add_mul, Nat.one_mul] at h1
  omega

theorem pacingDelay_rounding (bytes rate : Nat) (hr : 0 < rate)
    (hb : bytes ≤ 2 ^ 32 - 1) :
    bytes * nanosPerSec ≤ pacingDelay bytes rate * rate ∧
    pacingDelay bytes rate * rate < bytes * nanosPerSec + rate := by
  have hne : rate ≠ 0 := Nat.pos_iff_ne_zero.mp hr
  have hcap : (2 ^ 32 - 1) * nanosPerSec ≤ U64MAX := by decide
  have hbn : bytes * nanosPerSec ≤ (2 ^ 32 - 1) * nanosPerSec := Nat.mul_le_mul_right _ hb
  have hle := ceilDiv_le (bytes * nanosPerSec) rate hr
  have hmin : min (ceilDiv (bytes * nanosPerSec) rate) U64MAX =
      ceilDiv (bytes * nanosPerSec) rate := Nat.min_eq_left (by omega)
  simp only [pacingDelay, hne, if_false, hmin]
  exact ceilDiv_bounds _ _ hr

theorem pacingDelay_zero_rate (bytes : Nat) : pacingDelay bytes 0 = U64MAX := by
  simp [pacingDelay]

/-! ## Target cwnd clamp (§35.5) -/

theorem targetCwnd_bounds (cwnd mss hi : Nat) :
    4 * mss ≤ targetCwnd cwnd mss hi ∧ targetCwnd cwnd mss hi ≤ max hi (4 * mss) := by
  simp only [targetCwnd]
  omega

/-! ## Send guard: the in-flight bound holds on send transitions -/

theorem send_respects_cwnd (inflight cwnd bytes maxPkt : Nat)
    (hguard : sendAllowed inflight cwnd = true) (hb : bytes ≤ maxPkt) :
    inflight + bytes < cwnd + maxPkt := by
  simp only [sendAllowed, decide_eq_true_eq] at hguard
  omega

structure Flight where
  inflight : Nat
  cwnd : Nat

inductive Ev where
  /-- Try to send a packet of this many bytes. -/
  | send (bytes : Nat)
  /-- Acknowledgement or loss removes bytes from flight. -/
  | release (bytes : Nat)
  /-- The controller sets an arbitrary window (growth or cut). -/
  | setCwnd (cwnd : Nat)

/-- One transition. A send the guard refuses (or an oversize packet) leaves
the state unchanged. -/
def stepFlight (maxPkt : Nat) (s : Flight) : Ev → Flight
  | .send b =>
    if sendAllowed s.inflight s.cwnd = true ∧ b ≤ maxPkt then
      { s with inflight := s.inflight + b }
    else s
  | .release b => { s with inflight := s.inflight - b }
  | .setCwnd c => { s with cwnd := c }

/-- Whether event `e` from state `s` is a send the guard accepts. -/
def isAcceptedSend (maxPkt : Nat) (s : Flight) : Ev → Prop
  | .send b => sendAllowed s.inflight s.cwnd = true ∧ b ≤ maxPkt
  | _ => False

/-- On every accepted send of any trace, the post-state satisfies
`inflight < cwnd + maxPkt`. -/
theorem sendTrace_bound (maxPkt : Nat) (s : Flight) (e : Ev)
    (hacc : isAcceptedSend maxPkt s e) :
    (stepFlight maxPkt s e).inflight < (stepFlight maxPkt s e).cwnd + maxPkt := by
  cases e with
  | send b =>
    simp only [isAcceptedSend] at hacc
    simp only [stepFlight, hacc, and_self, if_true]
    exact send_respects_cwnd _ _ _ _ hacc.1 hacc.2
  | release _ => exact absurd hacc id
  | setCwnd _ => exact absurd hacc id

/-- After a window cut the guard refuses every send until
`inflight < cwnd` again. -/
theorem cut_blocks_sends (maxPkt : Nat) (s : Flight) (c b : Nat) (hcut : c ≤ s.inflight) :
    stepFlight maxPkt (stepFlight maxPkt s (.setCwnd c)) (.send b) =
      stepFlight maxPkt s (.setCwnd c) := by
  have : sendAllowed s.inflight c = false := by
    simp only [sendAllowed, decide_eq_false_iff_not]
    omega
  simp [stepFlight, this]

/-- There is no global `inflight ≤ cwnd + maxPkt` invariant: a window cut
breaks it. -/
theorem global_bound_fails :
    ¬ ∀ (maxPkt : Nat) (s : Flight) (e : Ev),
      (stepFlight maxPkt s e).inflight ≤ (stepFlight maxPkt s e).cwnd + maxPkt := by
  intro h
  have := h 1 ⟨10, 10⟩ (.setCwnd 1)
  simp [stepFlight] at this

/-! ## Executable fixture entry points -/

/-- One differential case; see `WarpBbr/Fixture.lean` for the line format. -/
def runCase : List Nat → Option String
  | [0, bytes, rate] => some s!"{pacingDelay bytes rate}"
  | [1, prev, now, delay] =>
    let s := edtStep prev now delay
    some s!"{s.1} {s.2}"
  | [2, cwnd, mss, hi] => some s!"{targetCwnd cwnd mss hi}"
  | [3, inflight, cwnd] => some s!"{sendAllowed inflight cwnd}"
  | _ => none

end Vesper.WarpBbr

#print axioms Vesper.WarpBbr.bbrv3_edt_launch_monotonic
#print axioms Vesper.WarpBbr.edtRun_ascending
#print axioms Vesper.WarpBbr.ceilDiv_bounds
#print axioms Vesper.WarpBbr.pacingDelay_rounding
#print axioms Vesper.WarpBbr.pacingDelay_zero_rate
#print axioms Vesper.WarpBbr.targetCwnd_bounds
#print axioms Vesper.WarpBbr.send_respects_cwnd
#print axioms Vesper.WarpBbr.sendTrace_bound
#print axioms Vesper.WarpBbr.cut_blocks_sends
#print axioms Vesper.WarpBbr.global_bound_fails
