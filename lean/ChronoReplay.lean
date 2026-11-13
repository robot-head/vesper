/-!
# Vesper Chrono replay determinism and lineage completeness (design §31.4)

Models of three Chrono claims, with what is proved about each model:

* Replay determinism over interaction trees. `Prog α` is a program that
  either returns or asks the environment one effect question (`op`) and
  continues with the answer. `recordRun env pos p` runs `p` against a live
  environment and records the tape of `(op, answer)` pairs; `replay tape p`
  runs `p` answering only from the tape, checking each recorded `op`.
  `chrono_replay_reproduces_live_execution`: replaying the recorded tape
  yields the live result, consumes exactly the tape (whatever follows it is
  returned untouched) and re-records the identical event stream.
* Step-back from a snapshot. `runSteps` folds a partial step function over a
  record list. `replay_from_snapshot`: running a prefix, then the suffix from
  the resulting state (a snapshot image), equals running the whole list.
* Lineage completeness. `checkLog` accepts a hop log only if every hop names
  a minted obligation that has not reached its terminal hop and nothing is
  minted twice. `lineage_creation_precedes`: in an accepted log, every
  non-mint hop (move, projection, handoff, discharge, leak) of obligation `i`
  is preceded by `mint i`. `lineage_complete`: every discharged or leaked
  obligation has a creation record.
* Reverse watchpoint. `lastBefore xs t` is the last element of `xs` below
  `t`. `reverse_watch_finds_latest_mutation`: for a strictly increasing
  write list it is a write before `t` and no write before `t` is later.

* Activation checkpoints. `checkpointAt step s0 es e` is the state after
  the first `e` events (the image a checkpoint at epoch `e` holds).
  `checkpoint_restore_replays_to_live`: restoring it and replaying the
  suffix `es.drop e` equals the live run over all of `es`.
  `reexecuted_checkpoint_replays_to_live`: a state rebuilt by re-executing a
  journal equal to the recorded prefix is that checkpoint, so it too replays
  to the live run. `restoreAt` models a byte-image restore that must land at
  the captured base: `restore_rejects_relocation` and
  `restore_at_captured_base`.

These are theorems about these models only, not about the Rust crate
`crates/vesper-chrono`. Its tests replay a real SimPulse recording and check
the same properties at run time, and `tests/lean_model.rs` compares
`checkLog` with the Rust `ObligationLineage` on generated logs through
`lean/ChronoReplay/Fixture.lean`.
-/

namespace Vesper.ChronoReplay

def modelId : String := "vesper.chrono.replay-model/1"
def modelClaim : String :=
  "interaction-tree replay, snapshot fold and lineage log models; not a proof about the Rust implementation"

/-! ## Replay determinism -/

inductive Prog (α : Type) where
  | pure : α → Prog α
  | ask : Nat → (Nat → Prog α) → Prog α

/-- One recorded effect: the question asked and the answer received. -/
abbrev Tape := List (Nat × Nat)

/-- Run against a live environment indexed by position and question. -/
def recordRun {α : Type} (env : Nat → Nat → Nat) : Nat → Prog α → α × Tape
  | _, .pure a => (a, [])
  | pos, .ask op k =>
    let answer := env pos op
    let (out, tape) := recordRun env (pos + 1) (k answer)
    (out, (op, answer) :: tape)

/-- Run answering from the tape only. Returns the result, the events it
re-recorded and the unconsumed rest; fails if the program asks a different
question than the one recorded or the tape runs out. -/
def replay {α : Type} : Tape → Prog α → Option (α × Tape × Tape)
  | rest, .pure a => some (a, [], rest)
  | [], .ask _ _ => none
  | (op', answer) :: rest, .ask op k =>
    if op' = op then
      match replay rest (k answer) with
      | some (out, trace, rest') => some (out, (op, answer) :: trace, rest')
      | none => none
    else none

theorem replay_recorded_prefix {α : Type} (env : Nat → Nat → Nat) (p : Prog α) :
    ∀ (pos : Nat) (rest : Tape),
      replay ((recordRun env pos p).2 ++ rest) p
        = some ((recordRun env pos p).1, (recordRun env pos p).2, rest) := by
  induction p with
  | pure a => intro pos rest; simp [recordRun, replay]
  | ask op k ih =>
    intro pos rest
    simp only [recordRun, List.cons_append, replay]
    rw [ih (env pos op) (pos + 1) rest]
    simp

/-- §31.4 theorem 1 for the model. -/
theorem chrono_replay_reproduces_live_execution {α : Type}
    (env : Nat → Nat → Nat) (p : Prog α) :
    let live := recordRun env 0 p
    replay live.2 p = some (live.1, live.2, []) := by
  have h := replay_recorded_prefix env p 0 []
  simpa using h

/-- The tape determines the result: two live environments that produced the
same tape produced the same result, because replay consults no environment. -/
theorem replay_ignores_environment {α : Type} (env env' : Nat → Nat → Nat) (p : Prog α)
    (htape : (recordRun env' 0 p).2 = (recordRun env 0 p).2) :
    (recordRun env' 0 p).1 = (recordRun env 0 p).1 := by
  have h1 := chrono_replay_reproduces_live_execution env p
  have h2 := chrono_replay_reproduces_live_execution env' p
  simp only at h1 h2
  rw [htape] at h2
  rw [h1] at h2
  injection h2 with h2
  injection h2 with h2
  exact h2.symm

/-! ## Step-back from a snapshot -/

def runSteps {S E : Type} (step : S → E → Option S) : S → List E → Option S
  | s, [] => some s
  | s, e :: es =>
    match step s e with
    | some s' => runSteps step s' es
    | none => none

theorem replay_from_snapshot {S E : Type} (step : S → E → Option S) :
    ∀ (s : S) (pre post : List E),
      runSteps step s (pre ++ post) = (runSteps step s pre).bind (fun snap => runSteps step snap post) := by
  intro s pre
  induction pre generalizing s with
  | nil => intro post; simp [runSteps]
  | cons e es ih =>
    intro post
    simp only [List.cons_append, runSteps]
    cases step s e with
    | none => rfl
    | some s' => exact ih s' post

/-! ## Lineage completeness -/

inductive Hop where
  | mint : Nat → Hop
  | move : Nat → Hop
  | proj : Nat → Hop
  | handoff : Nat → Hop
  | discharge : Nat → Hop
  | leak : Nat → Hop
  deriving Repr, DecidableEq

def Hop.id : Hop → Nat
  | .mint i | .move i | .proj i | .handoff i | .discharge i | .leak i => i

def Hop.isMint : Hop → Bool
  | .mint _ => true
  | _ => false

def Hop.isTerminal : Hop → Bool
  | .discharge _ | .leak _ => true
  | _ => false

structure St where
  minted : List Nat
  done : List Nat

def step (s : St) (h : Hop) : Option St :=
  if h.isMint then
    if h.id ∈ s.minted then none else some ⟨h.id :: s.minted, s.done⟩
  else if h.id ∈ s.minted ∧ h.id ∉ s.done then
    if h.isTerminal then some ⟨s.minted, h.id :: s.done⟩ else some s
  else none

def checkLog : St → List Hop → Option St := runSteps step

theorem step_nonmint_minted {s s' : St} {h : Hop} (hs : step s h = some s')
    (hn : h.isMint = false) : s'.minted = s.minted := by
  unfold step at hs
  rw [hn] at hs
  simp only [Bool.false_eq_true, if_false] at hs
  split at hs
  · split at hs <;> (cases hs; rfl)
  · cases hs

theorem step_minted {s s' : St} {h : Hop} (hs : step s h = some s') :
    ∀ i, i ∈ s'.minted → i ∈ s.minted ∨ h = .mint i := by
  intro i hi
  cases hm : h.isMint
  · rw [step_nonmint_minted hs hm] at hi
    exact Or.inl hi
  · cases h with
    | mint a =>
      unfold step at hs
      simp only [Hop.isMint, Hop.id, if_true] at hs
      split at hs
      · cases hs
      · cases hs
        simp only [List.mem_cons] at hi
        rcases hi with rfl | h'
        · exact Or.inr rfl
        · exact Or.inl h'
    | _ => simp [Hop.isMint] at hm

theorem check_minted :
    ∀ (log : List Hop) (s s' : St), checkLog s log = some s' →
      ∀ i, i ∈ s'.minted → i ∈ s.minted ∨ Hop.mint i ∈ log := by
  intro log
  induction log with
  | nil => intro s s' h i hi; simp [checkLog, runSteps] at h; subst h; exact Or.inl hi
  | cons h hs ih =>
    intro s s' hc i hi
    simp only [checkLog, runSteps] at hc
    cases hstep : step s h with
    | none => rw [hstep] at hc; cases hc
    | some t =>
      rw [hstep] at hc
      rcases ih t s' hc i hi with ht | hm
      · rcases step_minted hstep i ht with hs0 | heq
        · exact Or.inl hs0
        · exact Or.inr (by simp [heq])
      · exact Or.inr (by simp [hm])

theorem step_nonmint_requires_minted {s s' : St} {h : Hop} (hs : step s h = some s')
    (hn : h.isMint = false) : h.id ∈ s.minted := by
  unfold step at hs
  rw [hn] at hs
  simp only [Bool.false_eq_true, if_false] at hs
  split at hs
  · rename_i hc; exact hc.1
  · cases hs

/-- Every non-mint hop of obligation `i` in an accepted log is preceded by `mint i`. -/
theorem lineage_creation_precedes (pre post : List Hop) (h : Hop) (st : St)
    (hc : checkLog ⟨[], []⟩ (pre ++ h :: post) = some st) (hn : h.isMint = false) :
    Hop.mint h.id ∈ pre := by
  simp only [checkLog] at hc
  rw [replay_from_snapshot] at hc
  cases hpre : runSteps step ⟨[], []⟩ pre with
  | none => rw [hpre] at hc; cases hc
  | some s =>
    rw [hpre] at hc
    simp only [Option.bind_some, runSteps] at hc
    cases hstep : step s h with
    | none => rw [hstep] at hc; cases hc
    | some t =>
      have hmem := step_nonmint_requires_minted hstep hn
      rcases check_minted pre ⟨[], []⟩ s hpre h.id hmem with h0 | hm
      · simp at h0
      · exact hm

/-- Every discharged or leaked obligation has a creation record. -/
theorem lineage_complete (log : List Hop) (st : St) (i : Nat)
    (hc : checkLog ⟨[], []⟩ log = some st)
    (ht : Hop.discharge i ∈ log ∨ Hop.leak i ∈ log) : Hop.mint i ∈ log := by
  rcases ht with hd | hl
  · obtain ⟨pre, post, rfl⟩ := List.append_of_mem hd
    have := lineage_creation_precedes pre post (.discharge i) st hc rfl
    simp [Hop.id] at this
    exact List.mem_append_left _ this
  · obtain ⟨pre, post, rfl⟩ := List.append_of_mem hl
    have := lineage_creation_precedes pre post (.leak i) st hc rfl
    simp [Hop.id] at this
    exact List.mem_append_left _ this

/-! ## Reverse watchpoint -/

def lastBefore (xs : List Nat) (t : Nat) : Option Nat :=
  (xs.filter (fun w => decide (w < t))).getLast?

theorem lastBefore_spec (xs : List Nat) (t h : Nat) (hs : xs.Pairwise (· < ·))
    (hl : lastBefore xs t = some h) :
    h ∈ xs ∧ h < t ∧ ∀ w ∈ xs, w < t → w ≤ h := by
  unfold lastBefore at hl
  have hmem : h ∈ xs.filter (fun w => decide (w < t)) := List.mem_of_getLast? hl
  have hsf : (xs.filter (fun w => decide (w < t))).Pairwise (· < ·) := hs.filter _
  refine ⟨(List.mem_filter.mp hmem).1, by simpa using (List.mem_filter.mp hmem).2, ?_⟩
  intro w hw hwt
  have hwf : w ∈ xs.filter (fun w => decide (w < t)) := List.mem_filter.mpr ⟨hw, by simpa using hwt⟩
  obtain ⟨init, hinit⟩ := List.getLast?_eq_some_iff.mp hl
  rw [hinit] at hwf hsf
  rcases List.mem_append.mp hwf with hwi | hwl
  · have := List.pairwise_append.mp hsf
    exact Nat.le_of_lt (this.2.2 w hwi h (by simp))
  · simp at hwl; omega

/-- §31.4 theorem 2 for the model. -/
theorem reverse_watch_finds_latest_mutation (xs : List Nat) (t h : Nat)
    (hs : xs.Pairwise (· < ·)) (hl : lastBefore xs t = some h) :
    h < t ∧ ∀ w ∈ xs, h < w → t ≤ w := by
  obtain ⟨_, hlt, hmax⟩ := lastBefore_spec xs t h hs hl
  refine ⟨hlt, fun w hw hgt => ?_⟩
  by_cases hwt : w < t
  · have := hmax w hw hwt; omega
  · omega

/-! ## Activation checkpoints -/

/-- The state a checkpoint at epoch `e` holds: the first `e` events applied. -/
def checkpointAt {S E : Type} (step : S → E → Option S) (s0 : S) (es : List E) (e : Nat) :
    Option S :=
  runSteps step s0 (es.take e)

/-- Restoring the checkpoint at epoch `e` and replaying the suffix equals the
live run. -/
theorem checkpoint_restore_replays_to_live {S E : Type} (step : S → E → Option S)
    (s0 : S) (es : List E) (e : Nat) (c : S) (hc : checkpointAt step s0 es e = some c) :
    runSteps step c (es.drop e) = runSteps step s0 es := by
  have h := replay_from_snapshot step s0 (es.take e) (es.drop e)
  rw [List.take_append_drop] at h
  unfold checkpointAt at hc
  rw [h, hc]
  rfl

/-- A state rebuilt by re-executing a journal that equals the recorded prefix
replays the suffix to the live run. -/
theorem reexecuted_checkpoint_replays_to_live {S E : Type} (step : S → E → Option S)
    (s0 : S) (es journal : List E) (e : Nat) (r : S)
    (hj : journal = es.take e) (hr : runSteps step s0 journal = some r) :
    runSteps step r (es.drop e) = runSteps step s0 es := by
  subst hj
  exact checkpoint_restore_replays_to_live step s0 es e r hr

/-- A byte image of an activation captured at `base`. -/
structure Image where
  base : Nat
  words : List Nat
  deriving DecidableEq

/-- A byte-copy restore succeeds only at the captured base address. -/
def restoreAt (img : Image) (base : Nat) : Option Image :=
  if base = img.base then some img else none

theorem restore_rejects_relocation (img : Image) (base : Nat) (h : base ≠ img.base) :
    restoreAt img base = none := by
  simp [restoreAt, h]

theorem restore_at_captured_base (img : Image) : restoreAt img img.base = some img := by
  simp [restoreAt]

end Vesper.ChronoReplay

#print axioms Vesper.ChronoReplay.chrono_replay_reproduces_live_execution
#print axioms Vesper.ChronoReplay.replay_ignores_environment
#print axioms Vesper.ChronoReplay.replay_from_snapshot
#print axioms Vesper.ChronoReplay.lineage_creation_precedes
#print axioms Vesper.ChronoReplay.lineage_complete
#print axioms Vesper.ChronoReplay.reverse_watch_finds_latest_mutation
#print axioms Vesper.ChronoReplay.checkpoint_restore_replays_to_live
#print axioms Vesper.ChronoReplay.reexecuted_checkpoint_replays_to_live
#print axioms Vesper.ChronoReplay.restore_rejects_relocation
#print axioms Vesper.ChronoReplay.restore_at_captured_base
