/-!
# Vesper Warp UMEM frame conservation (design §35.4, theorem 1)

A count model of where AF_XDP UMEM frames are. Each of the eleven locations
of `crates/vesper-warp-umem` (`Location` there, `Loc` here) holds a natural
number of frames, and each modelled transition (`TransitionKind` there,
`Trans` here, same names and order) moves exactly one frame from one
location to another, failing when the source is empty.

What is proved, about this model only:

* `apply_preserves_total` / `run_preserves_total`: every transition, and
  every finite run of transitions, preserves the total number of frames.
* `umem_step_conserves_total_frames` (§35.4): one modelled receive step
  (`quicStep`: harvest, then recycle on any failure or turn around and submit
  a reply) preserves `fill + rx + tx`, leaves the application-held counts
  unchanged and preserves the total.
* `quicStep_failure_replenishes_fill`: on every malformed-packet path
  (malformed descriptor, header split failure, CID miss) the fill ring gains
  exactly the frame the rx ring lost.
* `quicStep_reply_submits`: the reply path moves the frame to the tx ring.
* `quicStep_progress`: a step never gets stuck while the rx ring is nonempty.

This is not a theorem about the Rust code. The Rust crate checks its real
rings and owners against the same counts at run time (`UmemAuditor`), and a
differential test replays recorded Rust transitions through `apply` below
(via `lean/WarpUmem/Fixture.lean`) and compares the counts.
-/

namespace Vesper.WarpUmem

def modelId : String := "vesper.warp.umem.conservation-model/1"
def modelClaim : String :=
  "count model of UMEM frame locations; not a proof about the Rust implementation"

structure Census where
  free : Nat
  fill : Nat
  nicRx : Nat
  rx : Nat
  appRx : Nat
  appTx : Nat
  tx : Nat
  nicTx : Nat
  comp : Nat
  inFlight : Nat
  leaked : Nat
  deriving Repr, DecidableEq

def Census.total (c : Census) : Nat :=
  c.free + c.fill + c.nicRx + c.rx + c.appRx + c.appTx + c.tx + c.nicTx + c.comp + c.inFlight
    + c.leaked

/-- Every frame starts on the free list. -/
def Census.init (frames : Nat) : Census :=
  ⟨frames, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0⟩

inductive Loc where
  | free | fill | nicRx | rx | appRx | appTx | tx | nicTx | comp | inFlight | leaked
  deriving Repr, DecidableEq

def Census.get (c : Census) : Loc → Nat
  | .free => c.free
  | .fill => c.fill
  | .nicRx => c.nicRx
  | .rx => c.rx
  | .appRx => c.appRx
  | .appTx => c.appTx
  | .tx => c.tx
  | .nicTx => c.nicTx
  | .comp => c.comp
  | .inFlight => c.inFlight
  | .leaked => c.leaked

def Census.set (c : Census) : Loc → Nat → Census
  | .free, n => { c with free := n }
  | .fill, n => { c with fill := n }
  | .nicRx, n => { c with nicRx := n }
  | .rx, n => { c with rx := n }
  | .appRx, n => { c with appRx := n }
  | .appTx, n => { c with appTx := n }
  | .tx, n => { c with tx := n }
  | .nicTx, n => { c with nicTx := n }
  | .comp, n => { c with comp := n }
  | .inFlight, n => { c with inFlight := n }
  | .leaked, n => { c with leaked := n }

inductive Trans where
  | nicPullFill | nicDeliver | harvest | harvestReject | recycle | rejectToFree | recycleToFree
  | turnAround | allocTx | abandonTx | submit | nicConsumeTx | nicComplete | completeLeased
  | completeRetired | completeLeaked | retire | replenish | leakRx | leakTx | leakInFlight
  deriving Repr, DecidableEq

def Trans.src : Trans → Loc
  | .nicPullFill => .fill
  | .nicDeliver => .nicRx
  | .harvest => .rx
  | .harvestReject => .rx
  | .recycle => .appRx
  | .rejectToFree => .rx
  | .recycleToFree => .appRx
  | .turnAround => .appRx
  | .allocTx => .free
  | .abandonTx => .appTx
  | .submit => .appTx
  | .nicConsumeTx => .tx
  | .nicComplete => .nicTx
  | .completeLeased => .comp
  | .completeRetired => .comp
  | .completeLeaked => .comp
  | .retire => .inFlight
  | .replenish => .free
  | .leakRx => .appRx
  | .leakTx => .appTx
  | .leakInFlight => .inFlight

def Trans.dst : Trans → Loc
  | .nicPullFill => .nicRx
  | .nicDeliver => .rx
  | .harvest => .appRx
  | .harvestReject => .fill
  | .recycle => .fill
  | .rejectToFree => .free
  | .recycleToFree => .free
  | .turnAround => .appTx
  | .allocTx => .appTx
  | .abandonTx => .free
  | .submit => .tx
  | .nicConsumeTx => .nicTx
  | .nicComplete => .comp
  | .completeLeased => .inFlight
  | .completeRetired => .free
  | .completeLeaked => .leaked
  | .retire => .free
  | .replenish => .fill
  | .leakRx => .leaked
  | .leakTx => .leaked
  | .leakInFlight => .leaked

/-- Move one frame from `t.src` to `t.dst`; `none` if the source is empty. -/
def apply (c : Census) (t : Trans) : Option Census :=
  if c.get t.src = 0 then none
  else
    let c1 := c.set t.src (c.get t.src - 1)
    some (c1.set t.dst (c1.get t.dst + 1))

def run (c : Census) : List Trans → Option Census
  | [] => some c
  | t :: ts => (apply c t).bind (fun c' => run c' ts)

theorem total_set (c : Census) (l : Loc) (n : Nat) :
    (c.set l n).total + c.get l = c.total + n := by
  cases l <;> simp only [Census.set, Census.get, Census.total] <;> omega

theorem get_set_same (c : Census) (l : Loc) (n : Nat) : (c.set l n).get l = n := by
  cases l <;> rfl

theorem apply_preserves_total {c c' : Census} {t : Trans} (h : apply c t = some c') :
    c'.total = c.total := by
  unfold apply at h
  split at h
  · contradiction
  · rename_i hne
    injection h with h
    subst h
    have h1 := total_set c t.src (c.get t.src - 1)
    have h2 := total_set (c.set t.src (c.get t.src - 1)) t.dst
      ((c.set t.src (c.get t.src - 1)).get t.dst + 1)
    omega

theorem run_preserves_total (ts : List Trans) :
    ∀ {c c' : Census}, run c ts = some c' → c'.total = c.total := by
  induction ts with
  | nil =>
    intro c c' h
    simp only [run, Option.some.injEq] at h
    rw [h]
  | cons t ts ih =>
    intro c c' h
    simp only [run] at h
    cases ha : apply c t with
    | none => rw [ha] at h; contradiction
    | some c1 =>
      rw [ha] at h
      have := ih h
      rw [this, apply_preserves_total ha]

theorem apply_none_iff (c : Census) (t : Trans) : apply c t = none ↔ c.get t.src = 0 := by
  unfold apply
  split <;> simp_all

/-! ## The §35.3 receive step -/

/-- How one harvested packet ends, as in `poll_and_step_quic_rx_one`. -/
inductive RxOutcome where
  /-- The descriptor itself is malformed; the driver recycles the chunk. -/
  | malformedDescriptor
  | headerSplitFailed
  | cidMiss
  | reply
  deriving Repr, DecidableEq

def quicStep (c : Census) : RxOutcome → Option Census
  | .malformedDescriptor => apply c .harvestReject
  | .headerSplitFailed => (apply c .harvest).bind (apply · .recycle)
  | .cidMiss => (apply c .harvest).bind (apply · .recycle)
  | .reply => ((apply c .harvest).bind (apply · .turnAround)).bind (apply · .submit)

/-- Closed form of `quicStep`, from which the theorems follow. -/
theorem quicStep_eq (c : Census) (o : RxOutcome) (hrx : c.rx ≠ 0) :
    quicStep c o =
      some (match o with
        | .reply => { c with rx := c.rx - 1, tx := c.tx + 1 }
        | _ => { c with rx := c.rx - 1, fill := c.fill + 1 }) := by
  cases o <;>
    simp [quicStep, apply, Trans.src, Trans.dst, Census.get, Census.set, hrx, Option.bind]

theorem quicStep_none_of_empty (c : Census) (o : RxOutcome) (hrx : c.rx = 0) :
    quicStep c o = none := by
  cases o <;> simp [quicStep, apply, Trans.src, Census.get, hrx, Option.bind]

theorem quicStep_rx_pos {c c' : Census} {o : RxOutcome} (h : quicStep c o = some c') :
    c.rx ≠ 0 := by
  intro hrx
  rw [quicStep_none_of_empty c o hrx] at h
  contradiction

/-- §35.4 `umem_step_conserves_total_frames`. -/
theorem umem_step_conserves_total_frames {c c' : Census} {o : RxOutcome}
    (h : quicStep c o = some c') :
    c'.fill + c'.rx + c'.tx = c.fill + c.rx + c.tx ∧ c'.appRx = c.appRx ∧ c'.appTx = c.appTx ∧
      c'.total = c.total := by
  have hrx := quicStep_rx_pos h
  rw [quicStep_eq c o hrx] at h
  injection h with h
  subst h
  cases o <;> dsimp only [Census.total] <;> omega

theorem quicStep_failure_replenishes_fill {c c' : Census} {o : RxOutcome} (hfail : o ≠ .reply)
    (h : quicStep c o = some c') : c'.fill = c.fill + 1 ∧ c'.rx + 1 = c.rx ∧ c'.tx = c.tx := by
  have hrx := quicStep_rx_pos h
  rw [quicStep_eq c o hrx] at h
  injection h with h
  subst h
  cases o <;> first | exact absurd rfl hfail | (dsimp only; omega)

theorem quicStep_reply_submits {c c' : Census} (h : quicStep c .reply = some c') :
    c'.tx = c.tx + 1 ∧ c'.rx + 1 = c.rx ∧ c'.fill = c.fill := by
  have hrx := quicStep_rx_pos h
  rw [quicStep_eq c .reply hrx] at h
  injection h with h
  subst h
  dsimp only
  omega

theorem quicStep_progress (c : Census) (o : RxOutcome) (hrx : c.rx ≠ 0) :
    (quicStep c o).isSome := by
  rw [quicStep_eq c o hrx]
  rfl

/-! ## Executable fixture support (used by `lean/WarpUmem/Fixture.lean`) -/

def Trans.ofName : String → Option Trans
  | "nicPullFill" => some .nicPullFill
  | "nicDeliver" => some .nicDeliver
  | "harvest" => some .harvest
  | "harvestReject" => some .harvestReject
  | "recycle" => some .recycle
  | "rejectToFree" => some .rejectToFree
  | "recycleToFree" => some .recycleToFree
  | "turnAround" => some .turnAround
  | "allocTx" => some .allocTx
  | "abandonTx" => some .abandonTx
  | "submit" => some .submit
  | "nicConsumeTx" => some .nicConsumeTx
  | "nicComplete" => some .nicComplete
  | "completeLeased" => some .completeLeased
  | "completeRetired" => some .completeRetired
  | "completeLeaked" => some .completeLeaked
  | "retire" => some .retire
  | "replenish" => some .replenish
  | "leakRx" => some .leakRx
  | "leakTx" => some .leakTx
  | "leakInFlight" => some .leakInFlight
  | _ => none

/-- Counts in `Location` order, space separated. -/
def Census.render (c : Census) : String :=
  " ".intercalate ([c.free, c.fill, c.nicRx, c.rx, c.appRx, c.appTx, c.tx, c.nicTx, c.comp,
    c.inFlight, c.leaked].map toString)

end Vesper.WarpUmem

#print axioms Vesper.WarpUmem.total_set
#print axioms Vesper.WarpUmem.get_set_same
#print axioms Vesper.WarpUmem.apply_preserves_total
#print axioms Vesper.WarpUmem.run_preserves_total
#print axioms Vesper.WarpUmem.apply_none_iff
#print axioms Vesper.WarpUmem.quicStep_eq
#print axioms Vesper.WarpUmem.quicStep_none_of_empty
#print axioms Vesper.WarpUmem.umem_step_conserves_total_frames
#print axioms Vesper.WarpUmem.quicStep_failure_replenishes_fill
#print axioms Vesper.WarpUmem.quicStep_reply_submits
#print axioms Vesper.WarpUmem.quicStep_progress
