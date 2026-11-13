/-!
# Vesper Chorus quorum model (design §25.1 item 5, §25.4)

Core Lean 4 only (`omega`, `simp`, `decide`, induction); no `sorry`, no
`admit`, no new axioms.

What is proved, about the models defined in this file only:

* `raft_election_single_leader_per_term`: the arithmetic statement quoted in
  §25.4 (two majorities of a cluster overlap in at least one node).
* `majority_quorums_intersect` / `exists_common_voter`: for two vote vectors
  of the same length (one Boolean per node), if each has more than half its
  entries set, some node is set in both.
* `castVote_keeps_vote`: the modelled vote rule (a voter grants only if it
  has not voted this term, or re-grants the same candidate) never changes a
  recorded vote.
* `election_safety`: for any ballot vector of one term (each voter records at
  most one candidate, which is what `castVote` maintains), two candidates
  that each hold a strict majority are the same candidate: at most one
  leader per term.
* `half_floor_quorums_can_be_disjoint`: the deliberately wrong `n / 2`
  quorum admits two disjoint quorums for `n = 5`, so the Rust simulation's
  buggy variant is genuinely unsafe.
* `append_yields_leader_prefix` and `follower_append_preserves_matching`:
  under the modelled AppendEntries rule (accept iff `prev = 0` or the
  follower's entry at `prev` has the leader's `prevTerm`; then keep the
  first `prev` entries and append the leader's next `k` entries), if the
  leader's log satisfies log matching with every log in the system, the
  follower's new log is a prefix of the leader's and also satisfies log
  matching with every log.
* `leader_append_preserves_matching`: a leader appending an entry of its own
  term `t` preserves log matching, given that every entry of term `t`
  anywhere agrees with the leader's log up to that entry (the property
  election safety provides, assumed here rather than derived).
* `system_log_matching` and `system_entries_owned` (wave six): in every
  reachable state of a modelled cluster whose steps are leader appends (by
  a node holding a strict majority of that term's ballots) and follower
  appends under the rule above, every pair of logs satisfies log matching,
  and every entry of term `t` was copied from the unique majority holder of
  `t`. This combines `election_safety` with the append lemmas; the one
  assumption is that a leader's log still extends what it appended in its
  term when it appends again (see the section).

The session-duality model is in `ChorusQuorum/SessionDuality.lean`.

Logs are lists of entry terms. This is a model of the protocol rules, not a
theorem about the Rust code in `crates/vesper-chorus`, which is checked by
seeded simulation instead. The model's append rule truncates to `prev`
before appending; the Rust follower truncates only on a conflicting entry.
-/

namespace Vesper.ChorusQuorum

def modelId : String := "vesper.chorus.quorum-model/1"

/-! ## Quorum arithmetic -/

theorem raft_election_single_leader_per_term
    (clusterNodes votesA votesB overlap : Nat)
    (h_majA : 2 * votesA > clusterNodes)
    (h_majB : 2 * votesB > clusterNodes)
    (h_incl : votesA + votesB ≤ clusterNodes + overlap) :
    overlap ≥ 1 := by
  omega

def majority (n : Nat) : Nat := n / 2 + 1

theorem majority_is_majority (n : Nat) : 2 * majority n > n := by
  unfold majority; omega

theorem two_majorities_overlap (n a b : Nat)
    (ha : a ≥ majority n) (hb : b ≥ majority n) : a + b > n := by
  unfold majority at ha hb; omega

/-! ## Vote vectors -/

def count : List Bool → Nat
  | [] => 0
  | b :: bs => (if b then 1 else 0) + count bs

def both : List Bool → List Bool → List Bool
  | a :: as, b :: bs => (a && b) :: both as bs
  | _, _ => []

theorem count_le_length : ∀ xs : List Bool, count xs ≤ xs.length
  | [] => by simp [count]
  | b :: bs => by
    have := count_le_length bs
    cases b <;> simp [count] <;> omega

theorem inclusion_exclusion :
    ∀ xs ys : List Bool, xs.length = ys.length →
      count xs + count ys ≤ xs.length + count (both xs ys)
  | [], [], _ => by simp [count, both]
  | [], _ :: _, h => by simp at h
  | _ :: _, [], h => by simp at h
  | a :: as, b :: bs, h => by
    have ih := inclusion_exclusion as bs (by simpa using h)
    cases a <;> cases b <;> simp [count, both] <;> omega

theorem majority_quorums_intersect (xs ys : List Bool)
    (hl : xs.length = ys.length)
    (ha : 2 * count xs > xs.length) (hb : 2 * count ys > xs.length) :
    count (both xs ys) ≥ 1 := by
  have := inclusion_exclusion xs ys hl
  omega

theorem exists_common_voter :
    ∀ xs ys : List Bool, count (both xs ys) ≥ 1 →
      ∃ i : Nat, xs[i]? = some true ∧ ys[i]? = some true
  | [], _, h => by simp [both, count] at h
  | _ :: _, [], h => by simp [both, count] at h
  | a :: as, b :: bs, h => by
    by_cases hab : (a && b) = true
    · refine ⟨0, ?_⟩
      simp only [Bool.and_eq_true] at hab
      simp [hab.1, hab.2]
    · have hab' : (a && b) = false := by simpa using hab
      have : count (both as bs) ≥ 1 := by
        simp [both, count, hab'] at h; exact h
      obtain ⟨i, hi⟩ := exists_common_voter as bs this
      exact ⟨i + 1, by simpa using hi⟩

/-- The wrong `n / 2` quorum: two disjoint quorums of size 2 in a 5-node
cluster. -/
theorem half_floor_quorums_can_be_disjoint :
    count [true, true, false, false, false] = 5 / 2 ∧
    count [false, false, true, true, false] = 5 / 2 ∧
    count (both [true, true, false, false, false] [false, false, true, true, false]) = 0 := by
  decide

/-! ## Election safety for the modelled vote rule -/

/-- One term's ballots: entry `i` is the candidate voter `i` granted, if any. -/
abbrev Ballots := List (Option Nat)

/-- The vote rule: grant `c` only if the voter has not voted, or already
voted for `c`. Returns the new ballots and whether the vote was granted. -/
def castVote (bs : Ballots) (voter c : Nat) : Ballots × Bool :=
  match bs[voter]? with
  | some none => (bs.set voter (some c), true)
  | some (some d) => (bs, d == c)
  | none => (bs, false)

theorem castVote_keeps_vote (bs : Ballots) (voter c i d : Nat)
    (h : bs[i]? = some (some d)) : (castVote bs voter c).1[i]? = some (some d) := by
  unfold castVote
  split
  · rename_i hv
    simp only [List.getElem?_set]
    by_cases hiv : voter = i
    · subst hiv; rw [hv] at h; simp at h
    · simp [hiv, h]
  · exact h
  · exact h

def votesFor (c : Nat) (bs : Ballots) : List Bool := bs.map (fun b => b == some c)

theorem votesFor_length (c : Nat) (bs : Ballots) : (votesFor c bs).length = bs.length := by
  simp [votesFor]

theorem election_safety (bs : Ballots) (c₁ c₂ : Nat)
    (h₁ : 2 * count (votesFor c₁ bs) > bs.length)
    (h₂ : 2 * count (votesFor c₂ bs) > bs.length) : c₁ = c₂ := by
  have hl : (votesFor c₁ bs).length = (votesFor c₂ bs).length := by
    simp [votesFor_length]
  have hint := majority_quorums_intersect (votesFor c₁ bs) (votesFor c₂ bs) hl
    (by rw [votesFor_length]; exact h₁) (by rw [votesFor_length]; exact h₂)
  obtain ⟨i, hi₁, hi₂⟩ := exists_common_voter _ _ hint
  simp only [votesFor, List.getElem?_map] at hi₁ hi₂
  cases hb : bs[i]? with
  | none => simp [hb] at hi₁
  | some b =>
    simp only [hb, Option.map_some, Option.some.injEq, beq_iff_eq] at hi₁ hi₂
    rw [hi₁] at hi₂
    simpa using hi₂

/-! ## Log matching for the modelled append rule -/

abbrev Log := List Nat

/-- Two logs that hold an entry with the same index and term agree on every
entry up to it. -/
def Matching (a b : Log) : Prop :=
  ∀ i t, a[i]? = some t → b[i]? = some t → a.take (i + 1) = b.take (i + 1)

theorem matching_symm {a b : Log} (h : Matching a b) : Matching b a := by
  intro i t hb ha
  exact (h i t ha hb).symm

theorem matching_take {a b : Log} (m : Nat) (h : Matching a b) : Matching (a.take m) b := by
  intro i t ha hb
  rw [List.getElem?_take] at ha
  split at ha
  · rename_i him
    have := h i t ha hb
    rw [List.take_take, Nat.min_eq_left (by omega), this]
  · simp at ha

/-- The modelled follower rule. -/
def appendEntries (f leader : Log) (prev prevTerm k : Nat) : Option Log :=
  if prev = 0 ∨ f[prev - 1]? = some prevTerm then
    some (f.take prev ++ (leader.drop prev).take k)
  else none

theorem append_yields_leader_prefix (f leader : Log) (prev k : Nat) (f' : Log)
    (hm : Matching leader f)
    (hprev : prev = 0 ∨ leader[prev - 1]? = some (leader[prev - 1]?.getD 0))
    (hlen : prev ≤ leader.length)
    (h : appendEntries f leader prev (leader[prev - 1]?.getD 0) k = some f') :
    f' = leader.take (prev + k) := by
  unfold appendEntries at h
  split at h
  · rename_i hc
    simp only [Option.some.injEq] at h
    subst h
    rw [List.take_add]
    congr 1
    by_cases h0 : prev = 0
    · subst h0; simp
    · rcases hc with h0' | hf
      · exact absurd h0' h0
      · rcases hprev with h0'' | hl
        · exact absurd h0'' h0
        · have := hm (prev - 1) _ hl hf
          have e : prev - 1 + 1 = prev := by omega
          rw [e] at this
          exact this.symm
  · simp at h

theorem follower_append_preserves_matching (sys : List Log) (f leader : Log)
    (prev k : Nat) (f' : Log)
    (hm : ∀ x ∈ sys, Matching leader x) (hf : f ∈ sys)
    (hlen : prev ≤ leader.length)
    (h : appendEntries f leader prev (leader[prev - 1]?.getD 0) k = some f') :
    f' = leader.take (prev + k) ∧ ∀ x ∈ sys, Matching f' x ∧ Matching x f' := by
  have hprev : prev = 0 ∨ leader[prev - 1]? = some (leader[prev - 1]?.getD 0) := by
    by_cases h0 : prev = 0
    · exact Or.inl h0
    · right
      have : prev - 1 < leader.length := by omega
      simp [List.getElem?_eq_getElem this]
  have e := append_yields_leader_prefix f leader prev k f' (hm f hf) hprev hlen h
  refine ⟨e, fun x hx => ?_⟩
  have := matching_take (prev + k) (hm x hx)
  rw [← e] at this
  exact ⟨this, matching_symm this⟩

/-- Every entry of term `t` in `x` agrees with the leader's log up to it. -/
def OwnsTerm (t : Nat) (leader x : Log) : Prop :=
  ∀ i, x[i]? = some t → x.take (i + 1) = leader.take (i + 1)

theorem leader_append_preserves_matching (leader x : Log) (t : Nat)
    (hm : Matching leader x) (own : OwnsTerm t leader x) :
    Matching (leader ++ [t]) x := by
  intro i u ha hb
  by_cases hi : i < leader.length
  · rw [List.getElem?_append_left hi] at ha
    have := hm i u ha hb
    rw [List.take_append_of_le_length (by omega), this]
  · rw [List.getElem?_append_right (by omega)] at ha
    have hi' : i = leader.length := by
      cases hlt : i - leader.length with
      | zero => omega
      | succ n => simp [hlt] at ha
    subst hi'
    simp at ha
    subst ha
    have hx := own leader.length hb
    have hxl : (x.take (leader.length + 1)).length = leader.length + 1 := by
      have : leader.length < x.length := by
        rcases Nat.lt_or_ge leader.length x.length with h | h
        · exact h
        · rw [List.getElem?_eq_none h] at hb; simp at hb
      simp; omega
    rw [hx] at hxl
    simp at hxl

/-! ## Whole-system log matching (wave six lane 17)

The append lemmas above each preserve log matching under a hypothesis;
here they are combined with election safety into an invariant of a whole
modelled cluster. A state holds every node's log and, per term `t` and
node `c`, a ghost log: what `c`'s log was right after it last appended an
entry of term `t` as leader (`[]` if it never did). `ballots t` is the
final ballot vector of term `t` (`castVote` never changes a recorded vote).

Steps:
* `leaderAppend t c`: `c` holds a strict majority in `ballots t`, and its
  log still extends its own ghost for `t` (a leader of `t` never loses its
  own entries of `t` while it leads: it only truncates after accepting an
  append of a higher term, after which it no longer leads `t`; this is an
  assumption of the model). `c` appends an entry of term `t`.
* `followerAppend f l prev k`: the modelled AppendEntries rule
  (`appendEntries`) applied by node `f` to node `l`'s log, for any `l`.

`system_log_matching`: in every reachable state every pair of logs
satisfies `Matching`. `system_entries_owned`: every entry of term `t`
anywhere agrees with the ghost log of a node that holds a majority of
term `t`, and by `election_safety` that node is unique. Again this is a
theorem about the model, not about the Rust simulation.
-/

structure Sys where
  logs : Nat → Log
  ghost : Nat → Nat → Log

def Leads (ballots : Nat → Ballots) (t c : Nat) : Prop :=
  2 * count (votesFor c (ballots t)) > (ballots t).length

def setLog (s : Sys) (c : Nat) (l : Log) : Nat → Log :=
  fun j => if j = c then l else s.logs j

inductive Step (ballots : Nat → Ballots) : Sys → Sys → Prop
  | leaderAppend (s : Sys) (t c : Nat)
      (hlead : Leads ballots t c)
      (hpre : (s.logs c).take (s.ghost t c).length = s.ghost t c) :
      Step ballots s
        { logs := setLog s c (s.logs c ++ [t]),
          ghost := fun t' c' => if t' = t ∧ c' = c then s.logs c ++ [t] else s.ghost t' c' }
  | followerAppend (s : Sys) (f l prev k : Nat) (f' : Log)
      (hlen : prev ≤ (s.logs l).length)
      (h : appendEntries (s.logs f) (s.logs l) prev ((s.logs l)[prev - 1]?.getD 0) k = some f') :
      Step ballots s { s with logs := setLog s f f' }

def initSys : Sys := { logs := fun _ => [], ghost := fun _ _ => [] }

inductive Reach (ballots : Nat → Ballots) : Sys → Prop
  | init : Reach ballots initSys
  | step (s s' : Sys) : Reach ballots s → Step ballots s s' → Reach ballots s'

/-- Every entry of term `t` agrees, up to it, with the ghost log of a node
that leads `t`. -/
def Owned (ballots : Nat → Ballots) (s : Sys) : Prop :=
  ∀ j i t, (s.logs j)[i]? = some t →
    ∃ c, Leads ballots t c ∧ i < (s.ghost t c).length ∧
      (s.logs j).take (i + 1) = (s.ghost t c).take (i + 1)

def AllMatching (s : Sys) : Prop := ∀ a b, Matching (s.logs a) (s.logs b)

theorem matching_refl (a : Log) : Matching a a := fun _ _ _ _ => rfl

theorem leads_unique {ballots : Nat → Ballots} {t c c' : Nat}
    (h : Leads ballots t c) (h' : Leads ballots t c') : c = c' :=
  election_safety (ballots t) c c' h h'

theorem take_succ_of_lt {l : Log} {i : Nat} (h : i < l.length) (t : Nat) :
    (l ++ [t]).take (i + 1) = l.take (i + 1) :=
  List.take_append_of_le_length (by omega)

/-- The ghost of a leading node is a prefix of its log, so the two agree
below the ghost's length. -/
theorem ghost_take {g l : Log} {i : Nat} (hpre : l.take g.length = g) (hi : i < g.length) :
    g.take (i + 1) = l.take (i + 1) := by
  conv => lhs; rw [← hpre]
  rw [List.take_take, Nat.min_eq_left (by omega)]

theorem ghost_len_le {g l : Log} (hpre : l.take g.length = g) : g.length ≤ l.length := by
  have := congrArg List.length hpre
  simp at this
  omega

theorem owned_leaderAppend (ballots : Nat → Ballots) (s : Sys) (t c : Nat)
    (hlead : Leads ballots t c)
    (hpre : (s.logs c).take (s.ghost t c).length = s.ghost t c)
    (ho : Owned ballots s) :
    Owned ballots
      { logs := setLog s c (s.logs c ++ [t]),
        ghost := fun t' c' => if t' = t ∧ c' = c then s.logs c ++ [t] else s.ghost t' c' } := by
  intro j i u hj
  simp only [setLog] at hj ⊢
  -- The witness for an entry of term `u` already in some log.
  have old : ∀ x : Log, x[i]? = some u →
      (∃ c', Leads ballots u c' ∧ i < (s.ghost u c').length ∧
        x.take (i + 1) = (s.ghost u c').take (i + 1)) →
      ∃ c', Leads ballots u c' ∧
        i < (if u = t ∧ c' = c then s.logs c ++ [t] else s.ghost u c').length ∧
        x.take (i + 1) = (if u = t ∧ c' = c then s.logs c ++ [t] else s.ghost u c').take (i + 1) := by
    intro x _ ⟨c', hl, hi, htake⟩
    refine ⟨c', hl, ?_⟩
    by_cases hc : u = t ∧ c' = c
    · rw [if_pos hc]
      obtain ⟨hu, hcc⟩ := hc
      subst hu; subst hcc
      have hle := ghost_len_le hpre
      refine ⟨by simp; omega, ?_⟩
      rw [htake, ghost_take hpre hi, take_succ_of_lt (by omega)]
    · rw [if_neg hc]; exact ⟨hi, htake⟩
  by_cases hjc : j = c
  · rw [if_pos hjc] at hj ⊢
    by_cases hi : i < (s.logs c).length
    · rw [List.getElem?_append_left hi] at hj
      rw [take_succ_of_lt hi]
      exact old _ hj (ho c i u hj)
    · -- The new entry, at the end.
      rw [List.getElem?_append_right (by omega)] at hj
      have hi' : i = (s.logs c).length := by
        cases hlt : i - (s.logs c).length with
        | zero => omega
        | succ n => simp [hlt] at hj
      subst hi'
      simp at hj
      subst hj
      exact ⟨c, hlead, by simp, by simp⟩
  · rw [if_neg hjc] at hj ⊢
    exact old _ hj (ho j i u hj)

theorem matching_leaderAppend (ballots : Nat → Ballots) (s : Sys) (t c : Nat)
    (hlead : Leads ballots t c)
    (hpre : (s.logs c).take (s.ghost t c).length = s.ghost t c)
    (ho : Owned ballots s) (hm : AllMatching s) :
    AllMatching { s with logs := setLog s c (s.logs c ++ [t]) } := by
  -- The leader owns term `t` against every log (election safety).
  have own : ∀ j, OwnsTerm t (s.logs c) (s.logs j) := by
    intro j i hj
    obtain ⟨c', hl, hi, htake⟩ := ho j i t hj
    have := leads_unique hl hlead
    subst this
    rw [htake, ghost_take hpre hi]
  have step : ∀ j, Matching (s.logs c ++ [t]) (s.logs j) := fun j =>
    leader_append_preserves_matching (s.logs c) (s.logs j) t (hm c j) (own j)
  intro a b
  simp only [setLog]
  by_cases ha : a = c <;> by_cases hb : b = c
  · rw [if_pos ha, if_pos hb]; exact matching_refl _
  · rw [if_pos ha, if_neg hb]; exact step b
  · rw [if_neg ha, if_pos hb]; exact matching_symm (step a)
  · rw [if_neg ha, if_neg hb]; exact hm a b

theorem prefix_entry {l : Log} {m i t : Nat} (h : (l.take m)[i]? = some t) :
    i < m ∧ l[i]? = some t := by
  rw [List.getElem?_take] at h
  split at h
  · exact ⟨by assumption, h⟩
  · simp at h

theorem followerAppend_shape (s : Sys) (f l prev k : Nat) (f' : Log)
    (hm : AllMatching s) (hlen : prev ≤ (s.logs l).length)
    (h : appendEntries (s.logs f) (s.logs l) prev ((s.logs l)[prev - 1]?.getD 0) k = some f') :
    f' = (s.logs l).take (prev + k) := by
  have hprev : prev = 0 ∨ (s.logs l)[prev - 1]? = some ((s.logs l)[prev - 1]?.getD 0) := by
    by_cases h0 : prev = 0
    · exact Or.inl h0
    · right
      have : prev - 1 < (s.logs l).length := by omega
      simp [List.getElem?_eq_getElem this]
  exact append_yields_leader_prefix (s.logs f) (s.logs l) prev k f' (hm l f) hprev hlen h

theorem owned_followerAppend (ballots : Nat → Ballots) (s : Sys) (f l prev k : Nat) (f' : Log)
    (hf' : f' = (s.logs l).take (prev + k)) (ho : Owned ballots s) :
    Owned ballots { s with logs := setLog s f f' } := by
  intro j i u hj
  simp only [setLog] at hj ⊢
  by_cases hjf : j = f
  · rw [if_pos hjf] at hj ⊢
    subst hf'
    obtain ⟨hik, hl⟩ := prefix_entry hj
    obtain ⟨c, hlead, hi, htake⟩ := ho l i u hl
    refine ⟨c, hlead, hi, ?_⟩
    rw [List.take_take, Nat.min_eq_left (by omega), htake]
  · rw [if_neg hjf] at hj ⊢
    exact ho j i u hj

theorem matching_followerAppend (s : Sys) (f l prev k : Nat) (f' : Log)
    (hf' : f' = (s.logs l).take (prev + k)) (hm : AllMatching s) :
    AllMatching { s with logs := setLog s f f' } := by
  have step : ∀ j, Matching f' (s.logs j) := fun j => by
    rw [hf']; exact matching_take (prev + k) (hm l j)
  intro a b
  simp only [setLog]
  by_cases ha : a = f <;> by_cases hb : b = f
  · rw [if_pos ha, if_pos hb]; exact matching_refl _
  · rw [if_pos ha, if_neg hb]; exact step b
  · rw [if_neg ha, if_pos hb]; exact matching_symm (step a)
  · rw [if_neg ha, if_neg hb]; exact hm a b

theorem system_invariant (ballots : Nat → Ballots) (s : Sys) (h : Reach ballots s) :
    AllMatching s ∧ Owned ballots s := by
  induction h with
  | init =>
    refine ⟨fun _ _ => matching_refl [], ?_⟩
    intro j i t hj
    simp [initSys] at hj
  | step s s' _ hstep ih =>
    obtain ⟨hm, ho⟩ := ih
    cases hstep with
    | leaderAppend t c hlead hpre =>
      exact ⟨matching_leaderAppend ballots s t c hlead hpre ho hm,
        owned_leaderAppend ballots s t c hlead hpre ho⟩
    | followerAppend f l prev k f' hlen happ =>
      have hf' := followerAppend_shape s f l prev k f' hm hlen happ
      exact ⟨matching_followerAppend s f l prev k f' hf' hm,
        owned_followerAppend ballots s f l prev k f' hf' ho⟩

/-- Whole-system log matching: in every reachable state of the modelled
cluster, any two logs that hold an entry with the same index and term agree
on every entry up to it. -/
theorem system_log_matching (ballots : Nat → Ballots) (s : Sys) (h : Reach ballots s)
    (a b : Nat) : Matching (s.logs a) (s.logs b) :=
  (system_invariant ballots s h).1 a b

/-- Every entry of term `t` anywhere was copied from the unique leader of
`t`: the node holding a majority of `ballots t`. -/
theorem system_entries_owned (ballots : Nat → Ballots) (s : Sys) (h : Reach ballots s)
    (j i t : Nat) (hj : (s.logs j)[i]? = some t) :
    ∃ c, Leads ballots t c ∧ (∀ c', Leads ballots t c' → c' = c) ∧
      (s.logs j).take (i + 1) = (s.ghost t c).take (i + 1) := by
  obtain ⟨c, hl, _, htake⟩ := (system_invariant ballots s h).2 j i t hj
  exact ⟨c, hl, fun c' h' => leads_unique h' hl, htake⟩

/-! ## Executable fixture checked from Rust -/

/-- Majority sizes for 0..64 nodes; `crates/vesper-chorus/tests/lean.rs`
compares this line with `QuorumRule::Majority`. -/
def majorityTable : List Nat := (List.range 65).map majority

#eval majorityTable

/-! ## Axiom audit (checked from Rust: only the standard axioms) -/

#print axioms raft_election_single_leader_per_term
#print axioms majority_quorums_intersect
#print axioms exists_common_voter
#print axioms half_floor_quorums_can_be_disjoint
#print axioms castVote_keeps_vote
#print axioms election_safety
#print axioms append_yields_leader_prefix
#print axioms follower_append_preserves_matching
#print axioms leader_append_preserves_matching
#print axioms system_log_matching
#print axioms system_entries_owned

end Vesper.ChorusQuorum
