/-
Vesper Crucible (design §29.2): a model of sleep-set schedule exploration in
which every Mazurkiewicz trace class of maximal executions is visited.

Model. An abstract deterministic transition system: states `σ`, actions `α`,
a partial transition function `step : σ → α → Option σ`, a finite list `acts`
containing every action that is ever enabled, and a symmetric independence
relation `indep : α → α → Bool`. Its one semantic assumption, `Commutes`, is
the diamond property of independent actions: if `a` then `b` can run from
`s`, then `b` then `a` can run from `s` and reach the same state.

Two words are Mazurkiewicz-equivalent (`Equiv`) when one is obtained from the
other by repeatedly swapping adjacent independent actions. `explore fuel s
sleep` is the classic sleep-set depth-first search (Godefroid 1996), which is
the pruning core of Crucible's DPOR: at each state it tries the enabled
actions in `acts` order, skips those in the sleep set, gives each child the
sleeping actions independent of the action taken, and puts each explored
action to sleep for its later siblings. `fuel` bounds the depth, like
`ExploreConfig::max_depth`.

What is proved, about this model only:
* `equiv_refl`, `equiv_symm`, `equiv_trans`: `Equiv` is an equivalence.
* `equiv_length`, `run_equiv`: equivalent words have the same length, and an
  execution stays an execution reaching the same state when reordered within
  its class (this uses `Commutes`).
* `explore_sound`: every explored word is an execution to a maximal state
  (no enabled action).
* `explore_complete`: if `w` is a maximal execution from `s` no longer than
  the fuel, and no action in the sleep set can be moved to the front of `w`,
  then some explored word is equivalent to `w`.
* `every_trace_class_visited`: hence, from the initial state with an empty
  sleep set, every Mazurkiewicz trace class of maximal executions within the
  depth bound is visited by at least one explored execution.

Not modelled, and not proved: FG05 persistent (backtracking) sets, which
Crucible uses on top of sleep sets to avoid starting most branches at all;
preemption bounding; state caching; timers that are disabled by deliveries
(Crucible makes them dependent with everything, so `Commutes` is only needed
for pairs it calls independent); and the agreement of Crucible's declared
footprints with the steps they describe. This is not a theorem about the Rust
code in crates/vesper-crucible: `tests/lean_model.rs` compares the model's
explored final states with the Rust explorers on concrete programs, which is
a differential test, not a refinement proof.
-/

namespace CrucibleDpor

def modelId : String := "vesper.crucible.dpor-model/1"
def modelClaim : String :=
  "Lean model of sleep-set exploration; every trace class is visited; proofs cover the model, not Rust"

section Model

variable {σ α : Type}
variable (step : σ → α → Option σ) (acts : List α) (indep : α → α → Bool)

/-- Run a word; `none` if some action is not enabled when it is reached. -/
def run : σ → List α → Option σ
  | s, [] => some s
  | s, a :: w =>
    match step s a with
    | some s' => run s' w
    | none => none

/-- One swap of adjacent independent actions. -/
inductive Swap : List α → List α → Prop
  | intro (u v : List α) (a b : α) :
      indep a b = true → Swap (u ++ a :: b :: v) (u ++ b :: a :: v)

/-- Mazurkiewicz trace equivalence: the reflexive-transitive closure of
`Swap` (symmetric because `indep` is). -/
inductive Equiv : List α → List α → Prop
  | refl (w : List α) : Equiv w w
  | tail {x y z : List α} : Equiv x y → Swap indep y z → Equiv x z

/-- The diamond property of independent actions. -/
def Commutes : Prop :=
  ∀ (s s1 s2 : σ) (a b : α), indep a b = true → step s a = some s1 → step s1 b = some s2 →
    ∃ s1', step s b = some s1' ∧ step s1' a = some s2

/-- `q` can be moved to the front of `w` within its trace class. -/
def CanStart (w : List α) (q : α) : Prop :=
  ∃ w', Equiv indep w (q :: w')

section Traces

variable {step acts indep}

theorem equiv_refl (w : List α) : Equiv indep w w := Equiv.refl w

theorem equiv_trans {x y z : List α} (h1 : Equiv indep x y) (h2 : Equiv indep y z) :
    Equiv indep x z := by
  induction h2 with
  | refl => exact h1
  | tail _ hs ih => exact Equiv.tail ih hs

theorem swap_symm (hsym : ∀ a b, indep a b = indep b a) {x y : List α}
    (h : Swap indep x y) : Swap indep y x := by
  cases h with
  | intro u v a b hab =>
    exact Swap.intro u v b a (by rw [hsym]; exact hab)

theorem equiv_symm (hsym : ∀ a b, indep a b = indep b a) {x y : List α}
    (h : Equiv indep x y) : Equiv indep y x := by
  induction h with
  | refl => exact Equiv.refl _
  | tail _ hs ih =>
    exact equiv_trans (Equiv.tail (Equiv.refl _) (swap_symm hsym hs)) ih

theorem swap_cons (c : α) {x y : List α} (h : Swap indep x y) :
    Swap indep (c :: x) (c :: y) := by
  cases h with
  | intro u v a b hab => exact Swap.intro (c :: u) v a b hab

theorem equiv_cons (c : α) {x y : List α} (h : Equiv indep x y) :
    Equiv indep (c :: x) (c :: y) := by
  induction h with
  | refl => exact Equiv.refl _
  | tail _ hs ih => exact Equiv.tail ih (swap_cons c hs)

theorem swap_length {x y : List α} (h : Swap indep x y) : x.length = y.length := by
  cases h with
  | intro u v a b _ => simp

theorem equiv_length {x y : List α} (h : Equiv indep x y) : x.length = y.length := by
  induction h with
  | refl => rfl
  | tail _ hs ih => exact ih.trans (swap_length hs)

theorem run_cons {s t : σ} {a : α} {w : List α} (h : run step s (a :: w) = some t) :
    ∃ s', step s a = some s' ∧ run step s' w = some t := by
  unfold run at h
  split at h
  · exact ⟨_, by assumption, h⟩
  · cases h

theorem run_swap (hcomm : Commutes step indep) {x y : List α} (h : Swap indep x y) :
    ∀ {s t : σ}, run step s x = some t → run step s y = some t := by
  cases h with
  | intro u v a b hab =>
    induction u with
    | nil =>
      intro s t hx
      obtain ⟨s1, h1, hx⟩ := run_cons hx
      obtain ⟨s2, h2, hx⟩ := run_cons hx
      obtain ⟨s1', h1', h2'⟩ := hcomm s s1 s2 a b hab h1 h2
      show run step s (b :: a :: v) = some t
      simp only [run, h1', h2']
      exact hx
    | cons c u ih =>
      intro s t hx
      obtain ⟨s1, h1, hx⟩ := run_cons hx
      show run step s (c :: (u ++ b :: a :: v)) = some t
      simp only [run, h1]
      exact ih hx

theorem run_equiv (hcomm : Commutes step indep) {x y : List α} (h : Equiv indep x y) :
    ∀ {s t : σ}, run step s x = some t → run step s y = some t := by
  induction h with
  | refl => exact fun hx => hx
  | tail _ hs ih => exact fun hx => run_swap hcomm hs (ih hx)

/-- The step of the completeness argument that needs independence: if `w`
starts with `a` and the rest can start with `q`, and `a`, `q` are
independent, then `w` can start with `q`. -/
theorem front_swap {w w' w'' : List α} {a q : α} (h1 : Equiv indep w (a :: w'))
    (h2 : Equiv indep w' (q :: w'')) (haq : indep a q = true) :
    Equiv indep w (q :: a :: w'') :=
  Equiv.tail (equiv_trans h1 (equiv_cons a h2)) (Swap.intro [] w'' a q haq)

theorem not_canStart_of_disabled (hcomm : Commutes step indep) {s t : σ} {w : List α}
    {a : α} (hw : run step s w = some t) (ha : step s a = none) : ¬ CanStart indep w a := by
  intro ⟨w', hw'⟩
  obtain ⟨_, h, _⟩ := run_cons (run_equiv hcomm hw' hw)
  rw [ha] at h
  cases h

end Traces

variable [DecidableEq α]

/-- No action is enabled. -/
def maximal (s : σ) : Bool :=
  acts.all fun a => (step s a).isNone

/-- One level of the search at `s`: try each action of the list in order. -/
def exploreAt (rec : σ → List α → List (List α)) (s : σ) :
    List α → List α → List (List α)
  | [], _ => []
  | a :: as, sl =>
    match step s a with
    | none => exploreAt rec s as sl
    | some s' =>
      if a ∈ sl then exploreAt rec s as sl
      else
        ((rec s' (sl.filter fun q => indep a q)).map (a :: ·)) ++
          exploreAt rec s as (a :: sl)

/-- Sleep-set depth-first search; returns the explored maximal executions.
A branch whose every enabled action is asleep contributes nothing. -/
def explore : Nat → σ → List α → List (List α)
  | 0, s, _ => if maximal step acts s then [[]] else []
  | n + 1, s, sl =>
    if maximal step acts s then [[]]
    else exploreAt step indep (explore n) s acts sl

variable {step acts indep}

theorem exploreAt_complete
    (hcomm : Commutes step indep) (m : Nat)
    (IH : ∀ (s : σ) (sl w : List α) (t : σ), run step s w = some t →
      maximal step acts t = true → w.length ≤ m → (∀ q ∈ sl, ¬ CanStart indep w q) →
      ∃ v ∈ explore step acts indep m s sl, Equiv indep w v)
    {s t : σ} {w : List α} (hw : run step s w = some t) (ht : maximal step acts t = true)
    (hlen : w.length ≤ m + 1) :
    ∀ (L sl : List α), (∀ q ∈ sl, ¬ CanStart indep w q) → (∃ p ∈ L, CanStart indep w p) →
      ∃ v ∈ exploreAt step indep (explore step acts indep m) s L sl, Equiv indep w v := by
  intro L
  induction L with
  | nil =>
    intro _ _ ⟨_, hp, _⟩
    cases hp
  | cons a as ih =>
    intro sl hsl ⟨p, hp, hcp⟩
    have hrest : p ≠ a → ∃ p ∈ as, CanStart indep w p := fun hne =>
      ⟨p, by
        cases hp with
        | head => exact absurd rfl hne
        | tail _ h => exact h, hcp⟩
    unfold exploreAt
    split
    · rename_i hnone
      apply ih sl hsl
      apply hrest
      intro hpa
      subst hpa
      exact not_canStart_of_disabled hcomm hw hnone hcp
    · rename_i s' hsome
      by_cases ha : a ∈ sl
      · rw [if_pos ha]
        apply ih sl hsl
        apply hrest
        intro hpa
        subst hpa
        exact hsl p ha hcp
      · rw [if_neg ha]
        by_cases hc : CanStart indep w a
        · obtain ⟨w', hw'⟩ := hc
          obtain ⟨s'', h1, hrun⟩ := run_cons (run_equiv hcomm hw' hw)
          rw [hsome] at h1
          cases h1
          have hl : w'.length ≤ m := by
            have := equiv_length hw'
            simp at this
            omega
          have hsl' : ∀ q ∈ sl.filter (fun q => indep a q), ¬ CanStart indep w' q := by
            intro q hq ⟨w'', h2⟩
            simp only [List.mem_filter] at hq
            exact hsl q hq.1 ⟨a :: w'', front_swap hw' h2 hq.2⟩
          obtain ⟨v', hv', he⟩ := IH s' _ w' t hrun ht hl hsl'
          refine ⟨a :: v', ?_, equiv_trans hw' (equiv_cons a he)⟩
          apply List.mem_append_left
          exact List.mem_map.mpr ⟨v', hv', rfl⟩
        · have hsl2 : ∀ q ∈ a :: sl, ¬ CanStart indep w q := by
            intro q hq
            cases hq with
            | head => exact hc
            | tail _ h => exact hsl q h
          obtain ⟨v, hv, he⟩ := ih (a :: sl) hsl2 (hrest (fun hpa => hc (hpa ▸ hcp)))
          exact ⟨v, List.mem_append_right _ hv, he⟩

theorem explore_complete
    (hcomm : Commutes step indep)
    (hacts : ∀ (s s' : σ) (a : α), step s a = some s' → a ∈ acts) :
    ∀ (n : Nat) (s : σ) (sl w : List α) (t : σ), run step s w = some t →
      maximal step acts t = true → w.length ≤ n → (∀ q ∈ sl, ¬ CanStart indep w q) →
      ∃ v ∈ explore step acts indep n s sl, Equiv indep w v := by
  intro n
  induction n with
  | zero =>
    intro s sl w t hw ht hlen _
    match w, hw, hlen with
    | [], hw, _ =>
      simp only [run, Option.some.injEq] at hw
      subst hw
      exact ⟨[], by simp [explore, ht], Equiv.refl _⟩
    | _ :: _, _, hlen => simp at hlen
  | succ m ihm =>
    intro s sl w t hw ht hlen hsl
    match w, hw, hlen, hsl with
    | [], hw, _, _ =>
      simp only [run, Option.some.injEq] at hw
      subst hw
      exact ⟨[], by simp [explore, ht], Equiv.refl _⟩
    | a :: rest, hw, hlen, hsl =>
      obtain ⟨s1, h1, _⟩ := run_cons hw
      have hnm : maximal step acts s = false := by
        unfold maximal
        rw [List.all_eq_false]
        exact ⟨a, hacts s s1 a h1, by simp [h1]⟩
      unfold explore
      rw [if_neg (by simp [hnm])]
      exact exploreAt_complete hcomm m ihm hw ht hlen acts sl hsl
        ⟨a, hacts s s1 a h1, rest, Equiv.refl _⟩

theorem exploreAt_sound (m : Nat)
    (IH : ∀ (s : σ) (sl v : List α), v ∈ explore step acts indep m s sl →
      ∃ t, run step s v = some t ∧ maximal step acts t = true)
    (s : σ) : ∀ (L sl : List α) (v : List α),
      v ∈ exploreAt step indep (explore step acts indep m) s L sl →
      ∃ t, run step s v = some t ∧ maximal step acts t = true := by
  intro L
  induction L with
  | nil => intro _ _ hv; cases hv
  | cons a as ih =>
    intro sl v hv
    unfold exploreAt at hv
    split at hv
    · exact ih sl v hv
    · rename_i s' hsome
      split at hv
      · exact ih sl v hv
      · rcases List.mem_append.mp hv with hv | hv
        · obtain ⟨v', hv', rfl⟩ := List.mem_map.mp hv
          obtain ⟨t, hrun, ht⟩ := IH s' _ v' hv'
          exact ⟨t, by simp only [run, hsome]; exact hrun, ht⟩
        · exact ih _ v hv

theorem explore_sound :
    ∀ (n : Nat) (s : σ) (sl v : List α), v ∈ explore step acts indep n s sl →
      ∃ t, run step s v = some t ∧ maximal step acts t = true := by
  intro n
  induction n with
  | zero =>
    intro s sl v hv
    unfold explore at hv
    split at hv
    · rename_i hm
      simp at hv
      subst hv
      exact ⟨s, rfl, hm⟩
    · cases hv
  | succ m ihm =>
    intro s sl v hv
    unfold explore at hv
    split at hv
    · rename_i hm
      simp at hv
      subst hv
      exact ⟨s, rfl, hm⟩
    · exact exploreAt_sound m ihm s acts sl v hv

/-- The lane's statement: from the initial state with an empty sleep set,
every Mazurkiewicz trace class of maximal executions (within the depth
bound) contains an explored execution. -/
theorem every_trace_class_visited
    (hcomm : Commutes step indep)
    (hacts : ∀ (s s' : σ) (a : α), step s a = some s' → a ∈ acts)
    (n : Nat) (s0 t : σ) (w : List α) (hw : run step s0 w = some t)
    (ht : maximal step acts t = true) (hlen : w.length ≤ n) :
    ∃ v ∈ explore step acts indep n s0 [], Equiv indep w v ∧ run step s0 v = some t :=
  let ⟨v, hv, he⟩ :=
    explore_complete hcomm hacts n s0 [] w t hw ht hlen (fun _ h => by cases h)
  ⟨v, hv, he, run_equiv hcomm he hw⟩

end Model

end CrucibleDpor

#print axioms CrucibleDpor.equiv_symm
#print axioms CrucibleDpor.equiv_trans
#print axioms CrucibleDpor.equiv_length
#print axioms CrucibleDpor.run_equiv
#print axioms CrucibleDpor.explore_sound
#print axioms CrucibleDpor.explore_complete
#print axioms CrucibleDpor.every_trace_class_visited
