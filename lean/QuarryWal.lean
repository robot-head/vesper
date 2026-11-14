/-!
# Vesper Quarry: WAL redo/undo recovery and pin conservation (design §24.4)

Model identifier: `vesper.quarry.wal-model/1`.

Small models, each proved in full. None of them is a theorem about Rust code:
`crates/vesper-quarry/src/store` implements a page-LSN WAL, deferred-update
(no-steal) transactions and a pinned frame pool, and its crate tests check the
same properties on concrete runs (crash enumeration at every device operation
and WAL frame boundary, isolation anomalies, counted pins).

## 1. Physical redo with page LSNs

A disk maps page ids to `(lsn, value)`. A log record carries an LSN, a page and
the page's new contents (the store logs page images and byte diffs; a diff is
modelled by the image it produces). Forward execution applies records in
order. Redo applies a record only when the page's LSN is older.

The WAL rule (a page is written only after the log through its LSN is
durable) means that each disk page equals that page after *some* prefix of the
log; different pages may be at different prefixes. That is `WalConsistent`.

* `redo_recovers_history`: for a log with strictly increasing positive LSNs,
  redo from any WAL-consistent disk yields exactly the state of forward
  execution of the whole log from blank pages.
* `redo_idempotent`: redoing a recovered disk changes nothing, so a crash
  during recovery recovers the same state.

## 2. Commit groups and a torn tail

The log is a sequence of commit groups (records then a commit marker) and
possibly an incomplete trailing group that a crash cut off. Recovery redoes
only complete groups.

* `complete_groups`: the records recovery keeps are exactly the records of the
  complete groups, whatever the torn tail holds.
* `recovery_ignores_torn_tail`: recovery of such a log from a disk consistent
  with the complete groups is the forward state of the complete groups.

## 3. Undo of losers under strict record locks

A key-value table, and a history of writes `(txn, key, before, after)` whose
before-images are the values at the time (`Consistent`). Under strict record
locking, once a loser (a transaction that never committed) writes a key, no
other transaction writes that key later (`Strict`).

* `undo_restores_winners`: undoing the losers' writes in reverse order, after
  repeating history, gives the state of the winners' writes alone.
* `no_steal_needs_no_undo`: when the log holds no loser writes (the store is
  no-steal: uncommitted writes never reach the log or the disk), undo is the
  identity, which is why the store's recovery is redo-only.

## 4. Pin conservation (§24.4)

A reference buffer pool counts pins per frame. A scan pins each page, checks
its CRC, reads it and unpins it; on a CRC error it unpins and returns the
error.

* `scan_restores_pool`: on every path (Ok, or Err at any page) the pool after
  the scan has the same pin counts as before.
* `scan_conserves_page_pins`: the §24.4 statement, the pinned-frame count over
  any frame list is invariant.
-/

namespace Vesper.QuarryWal

def modelId : String := "vesper.quarry.wal-model/1"

/-! ## 1. Physical redo -/

structure Rec where
  lsn : Nat
  page : Nat
  val : Nat

structure Page where
  lsn : Nat
  val : Nat

abbrev Disk := Nat → Page

def blank : Disk := fun _ => ⟨0, 0⟩

def apply (d : Disk) (r : Rec) : Disk :=
  fun q => if q = r.page then ⟨r.lsn, r.val⟩ else d q

def history (d : Disk) (log : List Rec) : Disk := log.foldl apply d

def redoStep (d : Disk) (r : Rec) : Disk :=
  if (d r.page).lsn < r.lsn then apply d r else d

def redo (d : Disk) (log : List Rec) : Disk := log.foldl redoStep d

def Ordered (log : List Rec) : Prop := log.Pairwise (fun a b => a.lsn < b.lsn)

def WalConsistent (log : List Rec) (d : Disk) : Prop :=
  ∀ p, ∃ k, d p = history blank (log.take k) p

/-- Forward execution seen from page `p`. -/
def pageStep (p : Nat) (pg : Page) (r : Rec) : Page :=
  if r.page = p then ⟨r.lsn, r.val⟩ else pg

/-- Redo seen from page `p`. -/
def redoPage (p : Nat) (pg : Page) (r : Rec) : Page :=
  if r.page = p ∧ pg.lsn < r.lsn then ⟨r.lsn, r.val⟩ else pg

theorem history_at (p : Nat) :
    ∀ (l : List Rec) (d : Disk), l.foldl apply d p = l.foldl (pageStep p) (d p)
  | [], _ => rfl
  | r :: l, d => by
    have h : apply d r p = pageStep p (d p) r := by
      rcases r with ⟨lsn, page, val⟩
      by_cases hp : page = p
      · subst hp; simp [apply, pageStep]
      · have hp' : p ≠ page := fun e => hp e.symm
        simp [apply, pageStep, hp, hp']
    simp only [List.foldl_cons]
    rw [history_at p l, h]

theorem redo_at (p : Nat) :
    ∀ (l : List Rec) (d : Disk), l.foldl redoStep d p = l.foldl (redoPage p) (d p)
  | [], _ => rfl
  | r :: l, d => by
    have h : redoStep d r p = redoPage p (d p) r := by
      rcases r with ⟨lsn, page, val⟩
      by_cases hp : page = p
      · subst hp
        by_cases hl : (d page).lsn < lsn <;> simp [redoStep, redoPage, apply, hl]
      · have hp' : p ≠ page := fun e => hp e.symm
        by_cases hl : (d page).lsn < lsn <;> simp [redoStep, redoPage, apply, hl, hp, hp']
    simp only [List.foldl_cons]
    rw [redo_at p l, h]

theorem redo_skips (p : Nat) :
    ∀ (a : List Rec) (pg : Page), (∀ r ∈ a, r.page = p → r.lsn ≤ pg.lsn) →
      a.foldl (redoPage p) pg = pg
  | [], _, _ => rfl
  | r :: a, pg, h => by
    have hr : redoPage p pg r = pg := by
      unfold redoPage
      rw [if_neg]
      intro ⟨hp, hl⟩
      have := h r (by simp) hp
      omega
    rw [List.foldl_cons, hr]
    exact redo_skips p a pg (fun r' hm => h r' (by simp [hm]))

theorem forward_mono (p : Nat) :
    ∀ (b : List Rec) (pg : Page), Ordered b → (∀ r ∈ b, r.page = p → pg.lsn ≤ r.lsn) →
      pg.lsn ≤ (b.foldl (pageStep p) pg).lsn
  | [], _, _, _ => Nat.le_refl _
  | r :: b, pg, ho, h => by
    rw [Ordered, List.pairwise_cons] at ho
    rw [List.foldl_cons]
    by_cases hp : r.page = p
    · have h1 := h r (by simp) hp
      have h2 := forward_mono p b ⟨r.lsn, r.val⟩ ho.2
        (fun r' hm _ => Nat.le_of_lt (ho.1 r' hm))
      simp only [pageStep, hp, if_true]
      exact Nat.le_trans h1 h2
    · simp only [pageStep, hp, if_false]
      exact forward_mono p b pg ho.2 (fun r' hm => h r' (by simp [hm]))

theorem forward_bounds (p : Nat) :
    ∀ (a : List Rec) (pg : Page), Ordered a →
      ∀ r ∈ a, r.page = p → r.lsn ≤ (a.foldl (pageStep p) pg).lsn
  | [], _, _, _, hm, _ => by simp at hm
  | x :: a, pg, ho, r, hm, hp => by
    have ho' := ho
    rw [Ordered, List.pairwise_cons] at ho'
    rw [List.foldl_cons]
    rcases List.mem_cons.mp hm with he | hm'
    · subst he
      have e : pageStep p pg r = ⟨r.lsn, r.val⟩ := by simp [pageStep, hp]
      rw [e]
      exact forward_mono p a ⟨r.lsn, r.val⟩ ho'.2 (fun r' hm _ => Nat.le_of_lt (ho'.1 r' hm))
    · exact forward_bounds p a _ ho'.2 r hm' hp

theorem forward_source (p : Nat) :
    ∀ (b : List Rec) (pg : Page),
      (b.foldl (pageStep p) pg).lsn = pg.lsn ∨ ∃ r ∈ b, (b.foldl (pageStep p) pg).lsn = r.lsn
  | [], _ => Or.inl rfl
  | r :: b, pg => by
    rw [List.foldl_cons]
    rcases forward_source p b (pageStep p pg r) with h | ⟨r', hm, h⟩
    · by_cases hp : r.page = p
      · have e : pageStep p pg r = ⟨r.lsn, r.val⟩ := by simp [pageStep, hp]
        rw [e] at h ⊢
        exact Or.inr ⟨r, by simp, h⟩
      · have e : pageStep p pg r = pg := by simp [pageStep, hp]
        rw [e] at h ⊢
        exact Or.inl h
    · exact Or.inr ⟨r', by simp [hm], h⟩

theorem redo_applies (p : Nat) :
    ∀ (b : List Rec) (pg : Page), Ordered b → (∀ r ∈ b, r.page = p → pg.lsn < r.lsn) →
      b.foldl (redoPage p) pg = b.foldl (pageStep p) pg
  | [], _, _, _ => rfl
  | r :: b, pg, ho, h => by
    rw [Ordered, List.pairwise_cons] at ho
    simp only [List.foldl_cons]
    by_cases hp : r.page = p
    · have h1 := h r (by simp) hp
      have e1 : redoPage p pg r = ⟨r.lsn, r.val⟩ := by simp [redoPage, hp, h1]
      have e2 : pageStep p pg r = ⟨r.lsn, r.val⟩ := by simp [pageStep, hp]
      rw [e1, e2]
      exact redo_applies p b _ ho.2 (fun r' hm _ => ho.1 r' hm)
    · have e1 : redoPage p pg r = pg := by simp [redoPage, hp]
      have e2 : pageStep p pg r = pg := by simp [pageStep, hp]
      rw [e1, e2]
      exact redo_applies p b pg ho.2 (fun r' hm => h r' (by simp [hm]))

theorem redo_page_split (p : Nat) (a b : List Rec) (ho : Ordered (a ++ b))
    (hpos : ∀ r ∈ a ++ b, 0 < r.lsn) :
    (a ++ b).foldl (redoPage p) (a.foldl (pageStep p) ⟨0, 0⟩) =
      (a ++ b).foldl (pageStep p) ⟨0, 0⟩ := by
  have ho' := ho
  rw [Ordered, List.pairwise_append] at ho'
  obtain ⟨hoa, hob, hab⟩ := ho'
  rw [List.foldl_append, List.foldl_append]
  rw [redo_skips p a _ (forward_bounds p a ⟨0, 0⟩ hoa)]
  apply redo_applies p b _ hob
  intro r hm hp
  rcases forward_source p a ⟨0, 0⟩ with h | ⟨x, hx, h⟩
  · rw [h]
    exact hpos r (List.mem_append.mpr (Or.inr hm))
  · rw [h]
    exact hab x hx r hm

theorem redo_recovers_history (log : List Rec) (ho : Ordered log)
    (hpos : ∀ r ∈ log, 0 < r.lsn) (d : Disk) (hd : WalConsistent log d) :
    redo d log = history blank log := by
  funext p
  obtain ⟨k, hk⟩ := hd p
  unfold redo history
  rw [redo_at, hk, history, history_at, history_at]
  have := redo_page_split p (log.take k) (log.drop k)
  rw [List.take_append_drop] at this
  exact this ho hpos

theorem redo_idempotent (log : List Rec) (ho : Ordered log)
    (hpos : ∀ r ∈ log, 0 < r.lsn) (d : Disk) (hd : WalConsistent log d) :
    redo (redo d log) log = redo d log := by
  have hr := redo_recovers_history log ho hpos d hd
  have hc : WalConsistent log (redo d log) := by
    intro p
    exact ⟨log.length, by rw [hr, List.take_length]⟩
  rw [redo_recovers_history log ho hpos _ hc, hr]

/-! ## 2. Commit groups and a torn tail -/

inductive Entry where
  | upd (r : Rec)
  | commit

def frame (g : List Rec) : List Entry := g.map Entry.upd ++ [Entry.commit]

/-- Records of complete groups; `pend` holds the current group's records. -/
def complete : List Entry → List Rec → List Rec
  | [], _ => []
  | .upd r :: es, pend => complete es (pend ++ [r])
  | .commit :: es, pend => pend ++ complete es []

theorem complete_frame (es : List Entry) :
    ∀ (g pend : List Rec), complete (frame g ++ es) pend = pend ++ g ++ complete es []
  | [], pend => by simp [frame, complete]
  | r :: g, pend => by
    have := complete_frame es g (pend ++ [r])
    simp only [frame, List.map_cons, List.cons_append] at this ⊢
    simp only [complete]
    rw [this]
    simp

theorem complete_torn :
    ∀ (tail pend : List Rec), complete (tail.map Entry.upd) pend = []
  | [], _ => rfl
  | r :: tail, pend => by
    simp only [List.map_cons, complete]
    exact complete_torn tail _

theorem complete_groups (tail : List Rec) :
    ∀ gs : List (List Rec),
      complete ((gs.map frame).flatten ++ tail.map Entry.upd) [] = gs.flatten
  | [] => by simp [complete_torn]
  | g :: gs => by
    simp only [List.map_cons, List.flatten_cons, List.append_assoc]
    rw [complete_frame, complete_groups tail gs]
    simp

def recover (d : Disk) (log : List Entry) : Disk := redo d (complete log [])

theorem recovery_ignores_torn_tail (gs : List (List Rec)) (tail : List Rec)
    (ho : Ordered gs.flatten) (hpos : ∀ r ∈ gs.flatten, 0 < r.lsn) (d : Disk)
    (hd : WalConsistent gs.flatten d) :
    recover d ((gs.map frame).flatten ++ tail.map Entry.upd) = history blank gs.flatten := by
  unfold recover
  rw [complete_groups]
  exact redo_recovers_history _ ho hpos d hd

/-! ## 3. Undo of losers -/

structure Write where
  txn : Nat
  key : Nat
  before : Nat
  after : Nat

abbrev Table := Nat → Nat

def set (s : Table) (k v : Nat) : Table := fun q => if q = k then v else s q

def forward (s : Table) (log : List Write) : Table :=
  log.foldl (fun s w => set s w.key w.after) s

def undoStep (loser : Nat → Bool) (s : Table) (w : Write) : Table :=
  if loser w.txn then set s w.key w.before else s

def undo (loser : Nat → Bool) (s : Table) (log : List Write) : Table :=
  log.reverse.foldl (undoStep loser) s

def Consistent : Table → List Write → Prop
  | _, [] => True
  | s, w :: rest => w.before = s w.key ∧ Consistent (set s w.key w.after) rest

def Strict (loser : Nat → Bool) (log : List Write) : Prop :=
  log.Pairwise (fun a b => a.key = b.key → loser a.txn = true → b.txn = a.txn)

theorem set_set (s : Table) (k v u : Nat) : set (set s k v) k u = set s k u := by
  funext q; unfold set; by_cases h : q = k <;> simp [h]

theorem set_self (s : Table) (k : Nat) : set s k (s k) = s := by
  funext q; unfold set; by_cases h : q = k <;> simp [h]

theorem set_comm (s : Table) (j k u v : Nat) (h : j ≠ k) :
    set (set s k v) j u = set (set s j u) k v := by
  funext q; unfold set
  by_cases hj : q = j
  · subst hj
    simp [h]
  · simp [hj]

theorem consistent_append (a b : List Write) :
    ∀ s, Consistent s (a ++ b) ↔ Consistent s a ∧ Consistent (forward s a) b := by
  induction a with
  | nil => intro s; simp [Consistent, forward]
  | cons w a ih =>
    intro s
    simp only [List.cons_append, Consistent]
    rw [ih]
    simp only [forward, List.foldl_cons]
    exact and_assoc.symm

theorem undo_commutes (loser : Nat → Bool) (k v : Nat) :
    ∀ (m : List Write) (s : Table), (∀ a ∈ m, a.key = k → loser a.txn = false) →
      m.foldl (undoStep loser) (set s k v) = set (m.foldl (undoStep loser) s) k v
  | [], _, _ => rfl
  | a :: m, s, h => by
    have step : undoStep loser (set s k v) a = set (undoStep loser s a) k v := by
      unfold undoStep
      cases hl : loser a.txn
      · simp
      · have hk : a.key ≠ k := fun e => by
          have := h a (by simp) e
          rw [hl] at this
          exact Bool.noConfusion this
        simp only [if_true]
        exact set_comm s a.key k a.before v hk
    simp only [List.foldl_cons]
    rw [step]
    exact undo_commutes loser k v m _ (fun a' hm => h a' (by simp [hm]))

theorem undo_restores_winners (loser : Nat → Bool) (s0 : Table) (log : List Write)
    (hc : Consistent s0 log) (hs : Strict loser log) :
    undo loser (forward s0 log) log = forward s0 (log.filter (fun w => !loser w.txn)) := by
  obtain ⟨m, rfl⟩ : ∃ m : List Write, log = m.reverse := ⟨log.reverse, by simp⟩
  induction m with
  | nil => rfl
  | cons w m ih =>
    rw [List.reverse_cons] at hc hs ⊢
    generalize m.reverse = pre at ih hc hs ⊢
    rw [consistent_append] at hc
    rw [Strict, List.pairwise_append] at hs
    obtain ⟨hcp, hcw⟩ := hc
    obtain ⟨hsp, _, hcross⟩ := hs
    have hb : w.before = forward s0 pre w.key := hcw.1
    have hfw : forward s0 (pre ++ [w]) = set (forward s0 pre) w.key w.after := by
      simp [forward, List.foldl_append]
    have hu : undo loser (forward s0 (pre ++ [w])) (pre ++ [w]) =
        pre.reverse.foldl (undoStep loser) (undoStep loser (forward s0 (pre ++ [w])) w) := by
      simp [undo, List.reverse_append]
    rw [hu, hfw]
    cases hl : loser w.txn
    · -- a winner: its write survives, and no loser in `pre` wrote its key
      have hfilt : (pre ++ [w]).filter (fun w => !loser w.txn) =
          pre.filter (fun w => !loser w.txn) ++ [w] := by
        simp [List.filter_append, hl]
      have hnone : ∀ a ∈ pre.reverse, a.key = w.key → loser a.txn = false := by
        intro a hm hk
        cases ha : loser a.txn
        · rfl
        · have := hcross a (List.mem_reverse.mp hm) w (by simp) hk ha
          rw [this, ha] at hl
          exact Bool.noConfusion hl
      have hstep : undoStep loser (set (forward s0 pre) w.key w.after) w =
          set (forward s0 pre) w.key w.after := by simp [undoStep, hl]
      rw [hstep, undo_commutes loser w.key w.after _ _ hnone, hfilt]
      have := ih hcp hsp
      unfold undo at this
      rw [this]
      simp [forward, List.foldl_append]
    · -- a loser: undo restores its before-image, the value before it
      have hfilt : (pre ++ [w]).filter (fun w => !loser w.txn) =
          pre.filter (fun w => !loser w.txn) := by
        simp [List.filter_append, hl]
      have hstep : undoStep loser (set (forward s0 pre) w.key w.after) w = forward s0 pre := by
        simp only [undoStep, hl, if_true]
        rw [set_set, hb, set_self]
      rw [hstep, hfilt]
      exact ih hcp hsp

theorem no_steal_needs_no_undo (loser : Nat → Bool) (s : Table) (log : List Write)
    (h : ∀ w ∈ log, loser w.txn = false) : undo loser s log = s := by
  unfold undo
  have : ∀ (m : List Write) (s : Table), (∀ w ∈ m, loser w.txn = false) →
      m.foldl (undoStep loser) s = s := by
    intro m
    induction m with
    | nil => intro s _; rfl
    | cons w m ih =>
      intro s hm
      have hw := hm w (by simp)
      simp only [List.foldl_cons, undoStep, hw]
      exact ih s (fun w' hw' => hm w' (by simp [hw']))
  exact this log.reverse s (fun w hm => h w (List.mem_reverse.mp hm))

/-! ## 4. Pin conservation -/

structure Pool where
  pins : Nat → Nat
  corrupt : Nat → Bool
  value : Nat → Nat

def Pool.pin (P : Pool) (p : Nat) : Pool :=
  { P with pins := fun q => if q = p then P.pins q + 1 else P.pins q }

def Pool.unpin (P : Pool) (p : Nat) : Pool :=
  { P with pins := fun q => if q = p then P.pins q - 1 else P.pins q }

def Pool.pinnedFrameCount (P : Pool) (frames : List Nat) : Nat :=
  frames.foldl (fun n f => n + P.pins f) 0

inductive ScanError where
  | crc (page : Nat)

/-- Pin each page, check its CRC, read it, unpin it; on a CRC error unpin and
stop with the error. Collects values at least `min`. -/
def scan (min : Nat) : Pool → List Nat → List Nat → Pool × Except ScanError (List Nat)
  | P, [], acc => (P, .ok acc.reverse)
  | P, p :: ps, acc =>
    let P1 := P.pin p
    if P1.corrupt p then (P1.unpin p, .error (.crc p))
    else
      let v := P1.value p
      scan min (P1.unpin p) ps (if min ≤ v then v :: acc else acc)

theorem unpin_pin (P : Pool) (p : Nat) : (P.pin p).unpin p = P := by
  cases P with
  | mk pins corrupt value =>
    simp only [Pool.pin, Pool.unpin, Pool.mk.injEq, and_true]
    funext q
    by_cases h : q = p <;> simp [h]

theorem scan_restores_pool (min : Nat) :
    ∀ (pages : List Nat) (P : Pool) (acc : List Nat), (scan min P pages acc).1 = P
  | [], _, _ => rfl
  | p :: ps, P, acc => by
    simp only [scan]
    split
    · exact unpin_pin P p
    · rw [scan_restores_pool min ps, unpin_pin]

theorem scan_conserves_page_pins (min : Nat) (P : Pool) (pages frames acc : List Nat) :
    (scan min P pages acc).1.pinnedFrameCount frames = P.pinnedFrameCount frames := by
  rw [scan_restores_pool]

end Vesper.QuarryWal

#print axioms Vesper.QuarryWal.redo_recovers_history
#print axioms Vesper.QuarryWal.redo_idempotent
#print axioms Vesper.QuarryWal.complete_groups
#print axioms Vesper.QuarryWal.recovery_ignores_torn_tail
#print axioms Vesper.QuarryWal.undo_restores_winners
#print axioms Vesper.QuarryWal.no_steal_needs_no_undo
#print axioms Vesper.QuarryWal.scan_restores_pool
#print axioms Vesper.QuarryWal.scan_conserves_page_pins
