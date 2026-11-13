/-!
# Vesper Chorus session duality (design §25.4, wave six lane 17)

Core Lean 4 only; no `sorry`, no `admit`, no new axioms.

The model: multiparty global types `G` without recursion (messages,
n-ary labelled choices, local actions, end; roles and labels are `Nat`),
binary session types `L` (send, receive, n-ary select and offer, end),
`dual`, and the binary projection `proj a b g`: role `a`'s view of its
interactions with role `b`, where choices between other roles must project
identically in every branch (`mergeAll`, the plain-merge rule) or the
projection is undefined (`none`). Local actions are erased.

What is proved, about this model only:

* `dual_dual`: duality is an involution.
* `proj_dual`: for any global type and any two distinct roles `a` and `b`,
  if `a`'s view of `b` is defined then `b`'s view of `a` is defined and is
  its exact dual. `proj_defined_symm`: one view is defined iff the other
  is. No well-formedness side condition is needed beyond `a ≠ b`.
* `raft_replicate_slot_session_dual` (the §25.4 statement): the Leader and
  Follower projections of the modelled `raft_replicate_slot` choreography
  (Leader → Follower AppendEntries; Follower persists; Follower → Leader
  {Accept, Reject}, the `APPEND_CHOREOGRAPHY` of `crates/vesper-chorus`) are
  duals, by `decide`. `raft_vote_session_dual` does the same for the
  RequestVote exchange and `fan_out_slot_session_dual` for a three-follower
  fan-out slot, for every leader/follower pair.

This is a theorem about the Lean model, not about the Rust projection in
`crates/vesper-chorus/src/project.rs`, which is checked by tests (including
`are_dual` on the same choreographies). The model has no recursion and
merges only identical branch views; the Rust projection also handles `rec`
and merges differing receive branches.
-/

namespace Vesper.ChorusQuorum.SessionDuality

def modelId : String := "vesper.chorus.session-duality/1"

mutual
/-- Multiparty global types. -/
inductive G where
  | msg (p q label : Nat) (k : G)
  | choice (p q : Nat) (bs : Bs)
  | act (r a : Nat) (k : G)
  | done
/-- Labelled branches of a choice. -/
inductive Bs where
  | nil
  | cons (label : Nat) (g : G) (rest : Bs)
end

mutual
/-- Binary session types. -/
inductive L where
  | send (label : Nat) (k : L)
  | recv (label : Nat) (k : L)
  | sel (bs : Ls)
  | off (bs : Ls)
  | stop
deriving DecidableEq
inductive Ls where
  | nil
  | cons (label : Nat) (k : L) (rest : Ls)
deriving DecidableEq
end

mutual
def dual : L → L
  | .send l k => .recv l (dual k)
  | .recv l k => .send l (dual k)
  | .sel bs => .off (dualBs bs)
  | .off bs => .sel (dualBs bs)
  | .stop => .stop
def dualBs : Ls → Ls
  | .nil => .nil
  | .cons l k r => .cons l (dual k) (dualBs r)
end

mutual
theorem dual_dual : ∀ s : L, dual (dual s) = s
  | .send l k => by simp only [dual, dual_dual k]
  | .recv l k => by simp only [dual, dual_dual k]
  | .sel bs => by simp only [dual, dualBs_dualBs bs]
  | .off bs => by simp only [dual, dualBs_dualBs bs]
  | .stop => by simp only [dual]
theorem dualBs_dualBs : ∀ bs : Ls, dualBs (dualBs bs) = bs
  | .nil => by simp only [dualBs]
  | .cons l k r => by simp only [dualBs, dual_dual k, dualBs_dualBs r]
end

mutual
/-- Role `a`'s view of its interactions with role `b`. -/
def proj (a b : Nat) : G → Option L
  | .msg p q l k =>
    if p = a ∧ q = b then (proj a b k).map (L.send l)
    else if p = b ∧ q = a then (proj a b k).map (L.recv l)
    else proj a b k
  | .choice p q bs =>
    if p = a ∧ q = b then (projBs a b bs).map L.sel
    else if p = b ∧ q = a then (projBs a b bs).map L.off
    else mergeAll a b bs
  | .act _ _ k => proj a b k
  | .done => some .stop
/-- The views of every branch, when `a` and `b` are the choosing pair. -/
def projBs (a b : Nat) : Bs → Option Ls
  | .nil => some .nil
  | .cons l g r =>
    match proj a b g, projBs a b r with
    | some k, some ks => some (.cons l k ks)
    | _, _ => none
/-- A choice `a` and `b` do not take part in: every branch must give the
same view (an empty choice has none). -/
def mergeAll (a b : Nat) : Bs → Option L
  | .nil => none
  | .cons _ g r =>
    match r with
    | .nil => proj a b g
    | .cons _ _ _ =>
      match proj a b g, mergeAll a b r with
      | some x, some y => if x = y then some x else none
      | _, _ => none
end

theorem mergeAll_cons_cons (a b l l' : Nat) (g g' : G) (r' : Bs) :
    mergeAll a b (.cons l g (.cons l' g' r')) =
      match proj a b g, mergeAll a b (.cons l' g' r') with
      | some x, some y => if x = y then some x else none
      | _, _ => none := by
  conv => lhs; rw [mergeAll]

mutual
theorem proj_dual (a b : Nat) (hab : a ≠ b) :
    ∀ (g : G) (s : L), proj a b g = some s → proj b a g = some (dual s)
  | .msg p q l k, s, h => by
    have ih := proj_dual a b hab k
    simp only [proj] at h ⊢
    by_cases h1 : p = a ∧ q = b
    · rw [if_pos h1] at h
      have h2 : ¬(p = b ∧ q = a) := fun h2 => hab (h1.1.symm.trans h2.1)
      rw [if_neg h2, if_pos h1]
      cases hk : proj a b k with
      | none => rw [hk] at h; simp at h
      | some t =>
        rw [hk] at h; simp only [Option.map_some, Option.some.injEq] at h
        subst h; rw [ih t hk]; rfl
    · rw [if_neg h1] at h
      by_cases h2 : p = b ∧ q = a
      · rw [if_pos h2] at h; rw [if_pos h2]
        cases hk : proj a b k with
        | none => rw [hk] at h; simp at h
        | some t =>
          rw [hk] at h; simp only [Option.map_some, Option.some.injEq] at h
          subst h; rw [ih t hk]; rfl
      · rw [if_neg h2] at h
        have h1' : ¬(p = b ∧ q = a) := h2
        have h2' : ¬(p = a ∧ q = b) := h1
        rw [if_neg h1', if_neg h2']
        exact ih s h
  | .choice p q bs, s, h => by
    simp only [proj] at h ⊢
    by_cases h1 : p = a ∧ q = b
    · rw [if_pos h1] at h
      have h2 : ¬(p = b ∧ q = a) := fun h2 => hab (h1.1.symm.trans h2.1)
      rw [if_neg h2, if_pos h1]
      cases hk : projBs a b bs with
      | none => rw [hk] at h; simp at h
      | some t =>
        rw [hk] at h; simp only [Option.map_some, Option.some.injEq] at h
        subst h; rw [projBs_dual a b hab bs t hk]; rfl
    · rw [if_neg h1] at h
      by_cases h2 : p = b ∧ q = a
      · rw [if_pos h2] at h; rw [if_pos h2]
        cases hk : projBs a b bs with
        | none => rw [hk] at h; simp at h
        | some t =>
          rw [hk] at h; simp only [Option.map_some, Option.some.injEq] at h
          subst h; rw [projBs_dual a b hab bs t hk]; rfl
      · rw [if_neg h2] at h
        have h1' : ¬(p = b ∧ q = a) := h2
        have h2' : ¬(p = a ∧ q = b) := h1
        rw [if_neg h1', if_neg h2']
        exact mergeAll_dual a b hab bs s h
  | .act _ _ k, s, h => by
    simp only [proj] at h ⊢
    exact proj_dual a b hab k s h
  | .done, s, h => by
    simp only [proj, Option.some.injEq] at h ⊢
    subst h; rfl
theorem projBs_dual (a b : Nat) (hab : a ≠ b) :
    ∀ (bs : Bs) (ss : Ls), projBs a b bs = some ss → projBs b a bs = some (dualBs ss)
  | .nil, ss, h => by
    simp only [projBs, Option.some.injEq] at h ⊢
    subst h; rfl
  | .cons l g r, ss, h => by
    simp only [projBs] at h ⊢
    cases hg : proj a b g with
    | none => rw [hg] at h; simp at h
    | some k =>
      cases hr : projBs a b r with
      | none => rw [hg, hr] at h; simp at h
      | some ks =>
        rw [hg, hr] at h
        simp only [Option.some.injEq] at h
        subst h
        rw [proj_dual a b hab g k hg, projBs_dual a b hab r ks hr]
        rfl
theorem mergeAll_dual (a b : Nat) (hab : a ≠ b) :
    ∀ (bs : Bs) (s : L), mergeAll a b bs = some s → mergeAll b a bs = some (dual s)
  | .nil, s, h => by simp [mergeAll] at h
  | .cons _ g .nil, s, h => by
    simp only [mergeAll] at h ⊢
    exact proj_dual a b hab g s h
  | .cons l g (.cons l' g' r'), s, h => by
    rw [mergeAll_cons_cons] at h ⊢
    cases hg : proj a b g with
    | none => rw [hg] at h; simp at h
    | some x =>
      cases hr : mergeAll a b (.cons l' g' r') with
      | none => rw [hg, hr] at h; simp at h
      | some y =>
        rw [hg, hr] at h
        simp only at h
        rw [proj_dual a b hab g x hg, mergeAll_dual a b hab (.cons l' g' r') y hr]
        simp only
        by_cases hxy : x = y
        · rw [if_pos hxy] at h
          simp only [Option.some.injEq] at h
          subst h; subst hxy
          simp
        · rw [if_neg hxy] at h; simp at h
end

theorem proj_defined_symm (a b : Nat) (hab : a ≠ b) (g : G) :
    (proj a b g).isSome = (proj b a g).isSome := by
  cases ha : proj a b g with
  | some s => rw [proj_dual a b hab g s ha]; rfl
  | none =>
    cases hb : proj b a g with
    | none => rfl
    | some t =>
      have := proj_dual b a (Ne.symm hab) g t hb
      rw [ha] at this; simp at this

/-! ## The §25.4 statement for the Chorus choreographies -/

/-- Two optional views are defined and dual. -/
def SessionDual : Option L → Option L → Bool
  | some x, some y => decide (dual x = y)
  | _, _ => false

theorem sessionDual_of_proj (a b : Nat) (hab : a ≠ b) (g : G)
    (h : (proj a b g).isSome) : SessionDual (proj a b g) (proj b a g) = true := by
  cases ha : proj a b g with
  | none => rw [ha] at h; simp at h
  | some s => rw [proj_dual a b hab g s ha]; simp [SessionDual]

-- Roles and labels (any distinct numbers would do).
def leader : Nat := 0
def follower : Nat := 1
def appendEntries : Nat := 10
def accept : Nat := 11
def reject : Nat := 12
def persist : Nat := 13
def requestVote : Nat := 20
def grant : Nat := 21
def deny : Nat := 22

/-- `projectRole r chor`: the role's view of its peer in a two-role
choreography. -/
def projectRole (r : Nat) (chor : G) : Option L := proj r (1 - r) chor

/-- `raft_replicate_slot`: Leader → Follower AppendEntries; Follower
persists; Follower → Leader {Accept, Reject}. -/
def raftReplicateSlotChor : G :=
  .msg leader follower appendEntries
    (.act follower persist
      (.choice follower leader (.cons accept .done (.cons reject .done .nil))))

/-- RequestVote: Candidate (0) → Voter (1); Voter → Candidate {Grant, Deny}. -/
def raftVoteChor : G :=
  .msg 0 1 requestVote (.choice 1 0 (.cons grant .done (.cons deny .done .nil)))

theorem raft_replicate_slot_session_dual :
    SessionDual (projectRole leader raftReplicateSlotChor)
                (projectRole follower raftReplicateSlotChor) = true := by
  decide

theorem raft_replicate_slot_projection :
    projectRole leader raftReplicateSlotChor =
      some (.send appendEntries (.off (.cons accept .stop (.cons reject .stop .nil)))) := by
  decide

theorem raft_vote_session_dual :
    SessionDual (projectRole 0 raftVoteChor) (projectRole 1 raftVoteChor) = true := by
  decide

/-- One fan-out slot: Leader (0) appends to F1, F2, F3 (1..3), each persists
and acks. -/
def fanOutSlotChor : G :=
  .msg 0 1 appendEntries (.msg 0 2 appendEntries (.msg 0 3 appendEntries
    (.act 1 persist (.act 2 persist (.act 3 persist
      (.msg 1 0 accept (.msg 2 0 accept (.msg 3 0 accept .done))))))))

theorem fan_out_slot_session_dual :
    ∀ f ∈ [1, 2, 3], SessionDual (proj 0 f fanOutSlotChor) (proj f 0 fanOutSlotChor) = true := by
  decide

/-- A choice the pair does not take part in must not change their views:
here F1 tells only the Leader its choice, and the Leader's two branches
treat F2 differently, so F2 cannot know what to expect. Both the
Leader–F2 and the F2–Leader views are undefined, and they are not
reported dual. -/
def unmergeableChor : G :=
  .choice 1 0 (.cons accept (.msg 0 2 appendEntries .done) (.cons reject .done .nil))

theorem unmergeable_view_is_undefined :
    proj 2 0 unmergeableChor = none ∧ proj 0 2 unmergeableChor = none ∧
    SessionDual (proj 0 2 unmergeableChor) (proj 2 0 unmergeableChor) = false := by
  decide

/-! ## Axiom audit (checked from Rust: only the standard axioms) -/

#print axioms dual_dual
#print axioms proj_dual
#print axioms proj_defined_symm
#print axioms sessionDual_of_proj
#print axioms raft_replicate_slot_session_dual
#print axioms raft_vote_session_dual
#print axioms fan_out_slot_session_dual

end Vesper.ChorusQuorum.SessionDuality
