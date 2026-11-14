/-!
# Vesper Chorus: session duality with recursion and branch merging
(design §25.4–§25.5, wave seven lane 16)

Core Lean 4 only; no `sorry`, no `admit`, no new axioms.

This extends `ChorusQuorum/SessionDuality.lean` (no recursion, plain
merge) with the two features the Rust projection in
`crates/vesper-chorus/src/project.rs` has:

* **Recursion**: `loop x g` and `var x` (named binders, as in the Rust
  `GlobalChor::Rec`/`Var`). A loop whose body has no interaction of the
  pair and whose only free variable is its own projects to `stop` (the
  Rust rule for a role absent from a loop that can only repeat or end); a
  body view that is just `var x` projects to `stop`; a body view that does
  not mention `x` drops the binder; otherwise the view is `loop x t`.
* **Branch merging**: a choice between other roles merges the pair's
  branch views with `merge`. Two receptions (`recv`/`off`) from the peer
  merge into one `off` with the union of their labels, merging the
  continuations of shared labels: the Rust receive-branch merge
  (`project.rs`, `merge`, "Receptions from the same peer: union of
  labels"). A binary view also needs the dual rule: the peer that learns
  the choice (from a third role, whose message the pair view erases) shows
  differing sends, which merge into one `sel`. Identical heads (`loop`
  with the same binder, `var`, `stop`) merge their continuations; anything
  else is undefined (`none`).

What is proved, about this model only:

* `dual_dual`: duality is an involution.
* `merge_dual`: merging commutes with duality,
  `merge (dual x) (dual y) = (merge x y).map dual`; the receive merge on
  one side is the send merge on the other.
* `proj_dual`: for any global type and distinct roles `a`, `b`,
  `proj b a g = (proj a b g).map dual`: `b`'s view of `a` is defined
  exactly when `a`'s view of `b` is, and is its dual.
  `proj_defined_symm` and `sessionDual_of_proj` follow.
* By `decide`, on modelled choreographies: a recursive Raft heartbeat loop
  (`raft_heartbeat_loop_session_dual`); a three-role commit where the
  client's view of the replica is the receive merge
  (`commit_notify_client_view`, `commit_notify_session_dual`); the same in
  a loop (`streaming_loop_session_dual`); a loop the pair is absent from
  (`absent_loop_projects_to_stop`); and an unmergeable send/receive
  conflict (`send_receive_conflict_is_undefined`).

These are theorems about the Lean model, not about the Rust code. The
Rust side is checked by `crates/vesper-chorus/tests/lean.rs`, which
projects the same choreographies with the Rust projection and checks the
receive merge and `are_dual`. Differences that remain: binders are
compared by name (Rust also merges loops up to renaming), payload types
are not modelled, and branch order in a merged list is first-seen order
(Rust sorts labels).
-/

namespace Vesper.ChorusQuorum.RecursiveDuality

def modelId : String := "vesper.chorus.recursive-duality/1"

mutual
/-- Multiparty global types with recursion. -/
inductive G where
  | msg (p q label : Nat) (k : G)
  | choice (p q : Nat) (bs : Bs)
  | act (r a : Nat) (k : G)
  | loop (x : Nat) (body : G)
  | var (x : Nat)
  | done
/-- Labelled branches of a choice. -/
inductive Bs where
  | nil
  | cons (label : Nat) (g : G) (rest : Bs)
end

mutual
/-- Binary session types with recursion. -/
inductive L where
  | send (label : Nat) (k : L)
  | recv (label : Nat) (k : L)
  | sel (bs : Ls)
  | off (bs : Ls)
  | loop (x : Nat) (body : L)
  | var (x : Nat)
  | stop
deriving DecidableEq
inductive Ls where
  | nil
  | cons (label : Nat) (k : L) (rest : Ls)
deriving DecidableEq
end

/-! ## Duality -/

mutual
def dual : L → L
  | .send l k => .recv l (dual k)
  | .recv l k => .send l (dual k)
  | .sel bs => .off (dualBs bs)
  | .off bs => .sel (dualBs bs)
  | .loop x b => .loop x (dual b)
  | .var x => .var x
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
  | .loop x b => by simp only [dual, dual_dual b]
  | .var x => by simp only [dual]
  | .stop => by simp only [dual]
theorem dualBs_dualBs : ∀ bs : Ls, dualBs (dualBs bs) = bs
  | .nil => by simp only [dualBs]
  | .cons l k r => by simp only [dualBs, dual_dual k, dualBs_dualBs r]
end

/-! ## Free variables -/

mutual
/-- `x` occurs free. -/
def mentions (x : Nat) : L → Bool
  | .send _ k => mentions x k
  | .recv _ k => mentions x k
  | .sel bs => mentionsBs x bs
  | .off bs => mentionsBs x bs
  | .loop y b => y != x && mentions x b
  | .var y => y == x
  | .stop => false
def mentionsBs (x : Nat) : Ls → Bool
  | .nil => false
  | .cons _ k r => mentions x k || mentionsBs x r
end

mutual
theorem mentions_dual (x : Nat) : ∀ s : L, mentions x (dual s) = mentions x s
  | .send _ k => by simp only [dual, mentions, mentions_dual x k]
  | .recv _ k => by simp only [dual, mentions, mentions_dual x k]
  | .sel bs => by simp only [dual, mentions, mentionsBs_dual x bs]
  | .off bs => by simp only [dual, mentions, mentionsBs_dual x bs]
  | .loop y b => by simp only [dual, mentions, mentions_dual x b]
  | .var y => by simp only [dual, mentions]
  | .stop => by simp only [dual, mentions]
theorem mentionsBs_dual (x : Nat) : ∀ bs : Ls, mentionsBs x (dualBs bs) = mentionsBs x bs
  | .nil => by simp only [dualBs, mentionsBs]
  | .cons _ k r => by simp only [dualBs, mentionsBs, mentions_dual x k, mentionsBs_dual x r]
end

/-! ## Branch lists -/

def lookupLs (l : Nat) : Ls → Option L
  | .nil => none
  | .cons l' k r => if l' = l then some k else lookupLs l r

def eraseLs (l : Nat) : Ls → Ls
  | .nil => .nil
  | .cons l' k r => if l' = l then eraseLs l r else .cons l' k (eraseLs l r)

def hasLabel (l : Nat) : Ls → Bool
  | .nil => false
  | .cons l' _ r => l' == l || hasLabel l r

/-- The branches of `ys` whose label is not in `xs`. -/
def diffLs (xs : Ls) : Ls → Ls
  | .nil => .nil
  | .cons l k r => if hasLabel l xs then diffLs xs r else .cons l k (diffLs xs r)

def appendLs : Ls → Ls → Ls
  | .nil, ys => ys
  | .cons l k r, ys => .cons l k (appendLs r ys)

theorem lookupLs_dual (l : Nat) : ∀ ys : Ls, lookupLs l (dualBs ys) = (lookupLs l ys).map dual
  | .nil => by simp only [dualBs, lookupLs, Option.map_none]
  | .cons l' k r => by
    simp only [dualBs, lookupLs]
    by_cases h : l' = l
    · simp only [if_pos h, Option.map_some]
    · simp only [if_neg h]; exact lookupLs_dual l r

theorem eraseLs_dual (l : Nat) : ∀ ys : Ls, eraseLs l (dualBs ys) = dualBs (eraseLs l ys)
  | .nil => by simp only [dualBs, eraseLs]
  | .cons l' k r => by
    simp only [dualBs, eraseLs]
    by_cases h : l' = l
    · simp only [if_pos h]; exact eraseLs_dual l r
    · simp only [if_neg h, dualBs, eraseLs_dual l r]

theorem hasLabel_dual (l : Nat) : ∀ xs : Ls, hasLabel l (dualBs xs) = hasLabel l xs
  | .nil => by simp only [dualBs, hasLabel]
  | .cons l' _ r => by simp only [dualBs, hasLabel, hasLabel_dual l r]

theorem diffLs_dual (xs : Ls) : ∀ ys : Ls,
    diffLs (dualBs xs) (dualBs ys) = dualBs (diffLs xs ys)
  | .nil => by simp only [dualBs, diffLs]
  | .cons l k r => by
    simp only [dualBs, diffLs, hasLabel_dual]
    by_cases h : hasLabel l xs = true
    · simp only [if_pos h]; exact diffLs_dual xs r
    · simp only [if_neg h, dualBs, diffLs_dual xs r]

theorem appendLs_dual : ∀ xs ys : Ls,
    appendLs (dualBs xs) (dualBs ys) = dualBs (appendLs xs ys)
  | .nil, ys => by simp only [dualBs, appendLs]
  | .cons l k r, ys => by simp only [dualBs, appendLs, appendLs_dual r ys]

/-! ## Merging -/

/-- A reception from the peer as a branch list. -/
def asOff : L → Option Ls
  | .recv l k => some (.cons l k .nil)
  | .off bs => some bs
  | _ => none

/-- A send to the peer as a branch list. -/
def asSel : L → Option Ls
  | .send l k => some (.cons l k .nil)
  | .sel bs => some bs
  | _ => none

/-- Canonical form: a one-branch offer is a receive (as in Rust's
`LocalType::choice`). -/
def mkOff : Ls → L
  | .cons l k .nil => .recv l k
  | bs => .off bs

def mkSel : Ls → L
  | .cons l k .nil => .send l k
  | bs => .sel bs

theorem asOff_dual (y : L) : asOff (dual y) = (asSel y).map dualBs := by
  cases y <;> simp [dual, asOff, asSel, dualBs]

theorem asSel_dual (y : L) : asSel (dual y) = (asOff y).map dualBs := by
  cases y <;> simp [dual, asOff, asSel, dualBs]

theorem mkOff_dual (bs : Ls) : dual (mkOff bs) = mkSel (dualBs bs) := by
  match bs with
  | .nil => rfl
  | .cons l k .nil => rfl
  | .cons l k (.cons l' k' r) => rfl

theorem mkSel_dual (bs : Ls) : dual (mkSel bs) = mkOff (dualBs bs) := by
  match bs with
  | .nil => rfl
  | .cons l k .nil => rfl
  | .cons l k (.cons l' k' r) => rfl

mutual
/-- Merge two branch views of a choice the pair does not take part in. -/
def merge : L → L → Option L
  | .recv l k, y =>
    match asOff y with
    | none => none
    | some ys =>
      (match lookupLs l ys with
       | none => some k
       | some k' => merge k k').map (fun m => mkOff (.cons l m (eraseLs l ys)))
  | .send l k, y =>
    match asSel y with
    | none => none
    | some ys =>
      (match lookupLs l ys with
       | none => some k
       | some k' => merge k k').map (fun m => mkSel (.cons l m (eraseLs l ys)))
  | .off bs, y =>
    match asOff y with
    | none => none
    | some ys => (mergeLeft bs ys).map (fun m => mkOff (appendLs m (diffLs bs ys)))
  | .sel bs, y =>
    match asSel y with
    | none => none
    | some ys => (mergeLeft bs ys).map (fun m => mkSel (appendLs m (diffLs bs ys)))
  | .loop x b, .loop x' b' => if x = x' then (merge b b').map (.loop x) else none
  | .loop _ _, _ => none
  | .var x, .var x' => if x = x' then some (.var x) else none
  | .var _, _ => none
  | .stop, .stop => some .stop
  | .stop, _ => none
/-- Each branch of `xs`, merged with the branch of `ys` with its label. -/
def mergeLeft : Ls → Ls → Option Ls
  | .nil, _ => some .nil
  | .cons l k r, ys =>
    match (match lookupLs l ys with
           | none => some k
           | some k' => merge k k'), mergeLeft r ys with
    | some m, some rest => some (.cons l m rest)
    | _, _ => none
end

/-! ### Merging commutes with duality -/

/-- The branch step shared by the send and receive rules. -/
def step (k : L) (o : Option L) (m : L → L → Option L) : Option L :=
  match o with
  | none => some k
  | some k' => m k k'

theorem merge_recv (l k y) : merge (.recv l k) y =
    match asOff y with
    | none => none
    | some ys => (step k (lookupLs l ys) merge).map (fun m => mkOff (.cons l m (eraseLs l ys))) := by
  conv => lhs; rw [merge]
  cases asOff y <;> rfl

theorem merge_send (l k y) : merge (.send l k) y =
    match asSel y with
    | none => none
    | some ys => (step k (lookupLs l ys) merge).map (fun m => mkSel (.cons l m (eraseLs l ys))) := by
  conv => lhs; rw [merge]
  cases asSel y <;> rfl

theorem merge_off (bs y) : merge (.off bs) y =
    match asOff y with
    | none => none
    | some ys => (mergeLeft bs ys).map (fun m => mkOff (appendLs m (diffLs bs ys))) := by
  conv => lhs; rw [merge]

theorem merge_sel (bs y) : merge (.sel bs) y =
    match asSel y with
    | none => none
    | some ys => (mergeLeft bs ys).map (fun m => mkSel (appendLs m (diffLs bs ys))) := by
  conv => lhs; rw [merge]

theorem mergeLeft_cons (l k r ys) : mergeLeft (.cons l k r) ys =
    match step k (lookupLs l ys) merge, mergeLeft r ys with
    | some m, some rest => some (.cons l m rest)
    | _, _ => none := by
  conv => lhs; rw [mergeLeft]
  cases lookupLs l ys <;> rfl

/-- One branch step commutes with duality, given that merging the
continuation does. -/
theorem step_dual (k : L) (o : Option L)
    (ih : ∀ k', merge (dual k) (dual k') = (merge k k').map dual) :
    step (dual k) (o.map dual) merge = (step k o merge).map dual := by
  cases o with
  | none => rfl
  | some k' => exact ih k'

mutual
theorem merge_dual : ∀ x y : L, merge (dual x) (dual y) = (merge x y).map dual
  | .recv l k, y => by
    show merge (.send l (dual k)) (dual y) = _
    rw [merge_send, merge_recv, asSel_dual]
    cases asOff y with
    | none => rfl
    | some ys =>
      simp only [Option.map_some]
      rw [lookupLs_dual, step_dual k _ (merge_dual k), eraseLs_dual]
      cases step k (lookupLs l ys) merge with
      | none => rfl
      | some m => simp only [Option.map_some, mkOff_dual, dualBs]
  | .send l k, y => by
    show merge (.recv l (dual k)) (dual y) = _
    rw [merge_recv, merge_send, asOff_dual]
    cases asSel y with
    | none => rfl
    | some ys =>
      simp only [Option.map_some]
      rw [lookupLs_dual, step_dual k _ (merge_dual k), eraseLs_dual]
      cases step k (lookupLs l ys) merge with
      | none => rfl
      | some m => simp only [Option.map_some, mkSel_dual, dualBs]
  | .off bs, y => by
    show merge (.sel (dualBs bs)) (dual y) = _
    rw [merge_sel, merge_off, asSel_dual]
    cases asOff y with
    | none => rfl
    | some ys =>
      simp only [Option.map_some]
      rw [mergeLeft_dual bs ys, diffLs_dual]
      cases mergeLeft bs ys with
      | none => rfl
      | some m => simp only [Option.map_some, mkOff_dual, appendLs_dual]
  | .sel bs, y => by
    show merge (.off (dualBs bs)) (dual y) = _
    rw [merge_off, merge_sel, asOff_dual]
    cases asSel y with
    | none => rfl
    | some ys =>
      simp only [Option.map_some]
      rw [mergeLeft_dual bs ys, diffLs_dual]
      cases mergeLeft bs ys with
      | none => rfl
      | some m => simp only [Option.map_some, mkSel_dual, appendLs_dual]
  | .loop x b, y => by
    cases y with
    | loop x' b' =>
      show merge (.loop x (dual b)) (.loop x' (dual b')) = _
      conv => lhs; rw [merge]
      conv => rhs; rw [merge]
      by_cases h : x = x'
      · rw [if_pos h, if_pos h, merge_dual b b']
        cases merge b b' <;> rfl
      · rw [if_neg h, if_neg h]; rfl
    | send _ _ => rfl
    | recv _ _ => rfl
    | sel _ => rfl
    | off _ => rfl
    | var _ => rfl
    | stop => rfl
  | .var x, y => by
    cases y with
    | var x' =>
      show merge (.var x) (.var x') = _
      by_cases h : x = x'
      · simp [merge, h, dual]
      · simp [merge, h]
    | _ => rfl
  | .stop, y => by
    cases y <;> rfl
theorem mergeLeft_dual : ∀ xs ys : Ls,
    mergeLeft (dualBs xs) (dualBs ys) = (mergeLeft xs ys).map dualBs
  | .nil, ys => rfl
  | .cons l k r, ys => by
    show mergeLeft (.cons l (dual k) (dualBs r)) (dualBs ys) = _
    rw [mergeLeft_cons, mergeLeft_cons, lookupLs_dual, step_dual k _ (merge_dual k),
      mergeLeft_dual r ys]
    cases step k (lookupLs l ys) merge <;> cases mergeLeft r ys <;> rfl
end


/-! ## Projection with recursion -/

mutual
/-- Every free variable of the global type is in `bound`. -/
def closedIn (bound : List Nat) : G → Bool
  | .msg _ _ _ k => closedIn bound k
  | .choice _ _ bs => closedInBs bound bs
  | .act _ _ k => closedIn bound k
  | .loop x g => closedIn (x :: bound) g
  | .var x => bound.contains x
  | .done => true
def closedInBs (bound : List Nat) : Bs → Bool
  | .nil => true
  | .cons _ g r => closedIn bound g && closedInBs bound r
end

mutual
/-- Some message or choice between `a` and `b`, in either direction. -/
def touches (a b : Nat) : G → Bool
  | .msg p q _ k => (p == a && q == b) || (p == b && q == a) || touches a b k
  | .choice p q bs => (p == a && q == b) || (p == b && q == a) || touchesBs a b bs
  | .act _ _ k => touches a b k
  | .loop _ g => touches a b g
  | .var _ => false
  | .done => false
def touchesBs (a b : Nat) : Bs → Bool
  | .nil => false
  | .cons _ g r => touches a b g || touchesBs a b r
end

/-- The view of `loop x` given the view `t` of its body (the Rust rule:
`continue x` alone ends, an unused binder is dropped). -/
def loopView (x : Nat) (t : L) : L :=
  if t = .var x then .stop else if mentions x t then .loop x t else t

theorem dual_eq_var (t : L) (x : Nat) : (dual t = .var x) ↔ (t = .var x) := by
  constructor
  · intro h
    have := congrArg dual h
    rw [dual_dual] at this
    exact this
  · intro h; subst h; rfl

theorem loopView_dual (x : Nat) (t : L) : dual (loopView x t) = loopView x (dual t) := by
  unfold loopView
  by_cases h : t = .var x
  · have h' : dual t = .var x := (dual_eq_var t x).mpr h
    rw [if_pos h, if_pos h']; rfl
  · have h' : ¬ dual t = .var x := fun e => h ((dual_eq_var t x).mp e)
    rw [if_neg h, if_neg h', mentions_dual]
    by_cases m : mentions x t = true
    · rw [if_pos m, if_pos m]; rfl
    · rw [if_neg m, if_neg m]

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
  | .loop x g =>
    if touches (min a b) (max a b) g = false ∧ closedIn [x] g = true then some .stop
    else (proj a b g).map (loopView x)
  | .var x => some (.var x)
  | .done => some .stop
/-- The views of every branch, when `a` and `b` are the choosing pair. -/
def projBs (a b : Nat) : Bs → Option Ls
  | .nil => some .nil
  | .cons l g r =>
    match proj a b g, projBs a b r with
    | some k, some ks => some (.cons l k ks)
    | _, _ => none
/-- A choice `a` and `b` do not take part in: the branch views merge (an
empty choice has none). -/
def mergeAll (a b : Nat) : Bs → Option L
  | .nil => none
  | .cons _ g r =>
    match r with
    | .nil => proj a b g
    | .cons _ _ _ =>
      match proj a b g, mergeAll a b r with
      | some x, some y => merge x y
      | _, _ => none
end

theorem mergeAll_cons_cons (a b l l' : Nat) (g g' : G) (r' : Bs) :
    mergeAll a b (.cons l g (.cons l' g' r')) =
      match proj a b g, mergeAll a b (.cons l' g' r') with
      | some x, some y => merge x y
      | _, _ => none := by
  conv => lhs; rw [mergeAll]

theorem map_send_dual (l : Nat) (o : Option L) :
    (o.map dual).map (L.recv l) = (o.map (L.send l)).map dual := by
  cases o <;> rfl

theorem map_recv_dual (l : Nat) (o : Option L) :
    (o.map dual).map (L.send l) = (o.map (L.recv l)).map dual := by
  cases o <;> rfl

mutual
theorem proj_dual (a b : Nat) (hab : a ≠ b) :
    ∀ g : G, proj b a g = (proj a b g).map dual
  | .msg p q l k => by
    conv => lhs; rw [proj]
    conv => rhs; rw [proj]
    rw [proj_dual a b hab k]
    by_cases h1 : p = a ∧ q = b
    · have h2 : ¬(p = b ∧ q = a) := fun h2 => hab (h1.1.symm.trans h2.1)
      simp only [if_neg h2, if_pos h1, map_send_dual]
    · by_cases h2 : p = b ∧ q = a
      · simp only [if_neg h1, if_pos h2, map_recv_dual]
      · simp only [if_neg h1, if_neg h2]
  | .choice p q bs => by
    conv => lhs; rw [proj]
    conv => rhs; rw [proj]
    rw [projBs_dual a b hab bs, mergeAll_dual a b hab bs]
    by_cases h1 : p = a ∧ q = b
    · have h2 : ¬(p = b ∧ q = a) := fun h2 => hab (h1.1.symm.trans h2.1)
      simp only [if_neg h2, if_pos h1]
      cases projBs a b bs <;> rfl
    · by_cases h2 : p = b ∧ q = a
      · simp only [if_neg h1, if_pos h2]
        cases projBs a b bs <;> rfl
      · simp only [if_neg h1, if_neg h2]
  | .act _ _ k => by
    conv => lhs; rw [proj]
    conv => rhs; rw [proj]
    exact proj_dual a b hab k
  | .loop x g => by
    conv => lhs; rw [proj]
    conv => rhs; rw [proj]
    rw [Nat.min_comm b a, Nat.max_comm b a, proj_dual a b hab g]
    by_cases c : touches (min a b) (max a b) g = false ∧ closedIn [x] g = true
    · rw [if_pos c, if_pos c]; rfl
    · rw [if_neg c, if_neg c]
      cases proj a b g with
      | none => rfl
      | some t => simp only [Option.map_some, loopView_dual]
  | .var x => by
    conv => lhs; rw [proj]
    conv => rhs; rw [proj]
    rfl
  | .done => by
    conv => lhs; rw [proj]
    conv => rhs; rw [proj]
    rfl
theorem projBs_dual (a b : Nat) (hab : a ≠ b) :
    ∀ bs : Bs, projBs b a bs = (projBs a b bs).map dualBs
  | .nil => by
    conv => lhs; rw [projBs]
    conv => rhs; rw [projBs]
    rfl
  | .cons l g r => by
    conv => lhs; rw [projBs]
    conv => rhs; rw [projBs]
    rw [proj_dual a b hab g, projBs_dual a b hab r]
    cases proj a b g <;> cases projBs a b r <;> rfl
theorem mergeAll_dual (a b : Nat) (hab : a ≠ b) :
    ∀ bs : Bs, mergeAll b a bs = (mergeAll a b bs).map dual
  | .nil => by
    conv => lhs; rw [mergeAll]
    conv => rhs; rw [mergeAll]
    rfl
  | .cons _ g .nil => by
    conv => lhs; rw [mergeAll]
    conv => rhs; rw [mergeAll]
    exact proj_dual a b hab g
  | .cons l g (.cons l' g' r') => by
    rw [mergeAll_cons_cons, mergeAll_cons_cons, proj_dual a b hab g,
      mergeAll_dual a b hab (.cons l' g' r')]
    cases proj a b g with
    | none => rfl
    | some x =>
      cases mergeAll a b (.cons l' g' r') with
      | none => rfl
      | some y => exact merge_dual x y
end

theorem proj_defined_symm (a b : Nat) (hab : a ≠ b) (g : G) :
    (proj a b g).isSome = (proj b a g).isSome := by
  rw [proj_dual a b hab g]
  cases proj a b g <;> rfl

/-! ## The §25.4 statement for recursive and merged choreographies -/

/-- Two optional views are defined and dual. -/
def SessionDual : Option L → Option L → Bool
  | some x, some y => decide (dual x = y)
  | _, _ => false

theorem sessionDual_of_proj (a b : Nat) (hab : a ≠ b) (g : G)
    (h : (proj a b g).isSome) : SessionDual (proj a b g) (proj b a g) = true := by
  rw [proj_dual a b hab g]
  cases hp : proj a b g with
  | none => rw [hp] at h; simp at h
  | some s => simp [SessionDual]

-- Roles and labels (any distinct numbers would do).
def leader : Nat := 0
def follower : Nat := 1
def coordinator : Nat := 0
def replica : Nat := 1
def client : Nat := 2
def appendEntries : Nat := 10
def accept : Nat := 11
def stopLabel : Nat := 12
def commit : Nat := 30
def abort : Nat := 31
def committed : Nat := 32
def aborted : Nat := 33
def more : Nat := 34
def item : Nat := 35
def fin : Nat := 36
def ping : Nat := 37

/-- `rec Loop { Leader -> Follower { AppendEntries { Follower -> Leader:
Accept; continue Loop } Stop { } } }`: a heartbeat loop. -/
def raftHeartbeatLoop : G :=
  .loop 0 (.choice leader follower
    (.cons appendEntries (.msg follower leader accept (.var 0)) (.cons stopLabel .done .nil)))

theorem raft_heartbeat_loop_projection :
    proj leader follower raftHeartbeatLoop =
      some (.loop 0 (.sel (.cons appendEntries (.recv accept (.var 0))
        (.cons stopLabel .stop .nil)))) := by
  decide

theorem raft_heartbeat_loop_session_dual :
    SessionDual (proj leader follower raftHeartbeatLoop)
                (proj follower leader raftHeartbeatLoop) = true := by
  decide

/-- The coordinator decides and tells the replica, which tells the
client: the client's view of the replica merges two receptions. -/
def commitNotify : G :=
  .choice coordinator replica
    (.cons commit (.msg replica client committed .done)
      (.cons abort (.msg replica client aborted .done) .nil))

theorem commit_notify_client_view :
    proj client replica commitNotify =
      some (.off (.cons committed .stop (.cons aborted .stop .nil))) := by
  decide

theorem commit_notify_session_dual :
    SessionDual (proj client replica commitNotify) (proj replica client commitNotify) = true ∧
    proj replica client commitNotify =
      some (.sel (.cons committed .stop (.cons aborted .stop .nil))) := by
  decide

/-- The same inside a loop: the replica streams items to the client until
the coordinator says it is finished. -/
def streamingLoop : G :=
  .loop 0 (.choice coordinator replica
    (.cons more (.msg replica client item (.var 0))
      (.cons fin (.msg replica client fin .done) .nil)))

theorem streaming_loop_session_dual :
    SessionDual (proj client replica streamingLoop) (proj replica client streamingLoop) = true ∧
    proj client replica streamingLoop =
      some (.loop 0 (.off (.cons item (.var 0) (.cons fin .stop .nil)))) := by
  decide

/-- A loop between the coordinator and the replica that the client takes
no part in projects to `stop` for the client and the replica's view of it
(the branch views `var 0` and `stop` would not merge). -/
def absentLoop : G :=
  .loop 0 (.choice coordinator replica (.cons ping (.var 0) (.cons stopLabel .done .nil)))

theorem absent_loop_projects_to_stop :
    proj client replica absentLoop = some .stop ∧ proj replica client absentLoop = some .stop ∧
    merge (.var 0) .stop = none := by
  decide

/-- The replica sends in one branch and receives in the other: no merge. -/
def conflict : G :=
  .choice coordinator replica
    (.cons commit (.msg replica client committed .done)
      (.cons abort (.msg client replica aborted .done) .nil))

theorem send_receive_conflict_is_undefined :
    proj replica client conflict = none ∧ proj client replica conflict = none ∧
    SessionDual (proj replica client conflict) (proj client replica conflict) = false := by
  decide

/-! ## Axiom audit (checked from Rust: only the standard axioms) -/

#print axioms dual_dual
#print axioms merge_dual
#print axioms loopView_dual
#print axioms proj_dual
#print axioms proj_defined_symm
#print axioms sessionDual_of_proj
#print axioms raft_heartbeat_loop_projection
#print axioms raft_heartbeat_loop_session_dual
#print axioms commit_notify_client_view
#print axioms commit_notify_session_dual
#print axioms streaming_loop_session_dual
#print axioms absent_loop_projects_to_stop
#print axioms send_receive_conflict_is_undefined

end Vesper.ChorusQuorum.RecursiveDuality
