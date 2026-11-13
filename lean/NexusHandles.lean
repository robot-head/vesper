/-!
# Vesper Nexus WIT handle table (design §30.4, §30.5)

A model of the generational WIT resource table of `crates/vesper-nexus`
(`WitResourceTable` there). Slots carry a generation, an owner tenant, the
Component Model lend count and a payload. A guest handle is a pair
`(idx, gen)` of natural numbers; the guest may present any pair at all.

What is proved, about this model only:

* `lookup_ok_sound` (non-forgery): a lookup by tenant `t` of any integer
  pair succeeds only on a live slot at that index whose generation equals
  the handle's and whose owner is `t`. `beginBorrow_ok_sound`,
  `endBorrow_ok_sound`, `dropOwn_ok_sound` and `transfer_ok_sound` say the
  same for the mutating operations.
* `wit_handle_tenant_isolated` (§30.4 theorem 1): a live slot owned by tenant
  A never resolves for tenant B ≠ A, whatever generation is presented, and
  with the matching generation the error is `unauthorizedTenant`.
* `wit_drop_own_rejects_active_borrows` (§30.4 theorem 2) and
  `dropOwn_ok_no_borrows`, `transfer_ok_no_borrows`: an `own` with an
  outstanding lend cannot be dropped or transferred (borrow-before-drop).
* `step_gen_mono` / `run_gen_mono`: no operation, and no finite run of
  operations, lowers any slot's generation; `stale_stays_stale` /
  `run_stale_stays_stale`: a handle whose generation is below its slot's
  never resolves again for any tenant; `dropOwn_ok_stales` and
  `transfer_ok_stales`: dropping or transferring an `own` below the
  generation ceiling makes the old handle stale forever.

This is not a theorem about the Rust code. The Rust crate's tests replay
random operation sequences through `step` below (via
`lean/NexusHandles/Fixture.lean`) and compare every result.
-/

namespace Vesper.NexusHandles

def modelId : String := "vesper.nexus.wit-handles-model/1"
def modelClaim : String :=
  "model of the generational WIT handle table; not a proof about the Rust implementation"

/-- `u32::MAX`, the Rust lend-count ceiling. -/
def borrowMax : Nat := 4294967295

structure Slot where
  gen : Nat
  owner : Nat
  borrows : Nat
  live : Bool
  payload : Nat
  deriving Repr, DecidableEq

structure Handle where
  idx : Nat
  gen : Nat
  deriving Repr, DecidableEq

structure Table where
  slots : List Slot
  /-- Free slot indices; the head is the most recently freed (a stack, like
  Rust's `Vec::pop`). -/
  free : List Nat
  cap : Nat
  maxGen : Nat
  deriving Repr

inductive Err where
  | invalidOrStale
  | unauthorizedTenant
  | activeBorrows
  | noActiveBorrow
  | borrowOverflow
  | tableFull
  | generationExhausted
  deriving Repr, DecidableEq

def Err.name : Err → String
  | .invalidOrStale => "invalidOrStale"
  | .unauthorizedTenant => "unauthorizedTenant"
  | .activeBorrows => "activeBorrows"
  | .noActiveBorrow => "noActiveBorrow"
  | .borrowOverflow => "borrowOverflow"
  | .tableFull => "tableFull"
  | .generationExhausted => "generationExhausted"

def Table.init (cap maxGen : Nat) : Table :=
  { slots := [], free := [], cap := cap, maxGen := max maxGen 1 }

/-- A live slot at `h.idx` with generation `h.gen`, ignoring the owner. -/
def liveSlot (T : Table) (h : Handle) : Except Err Slot :=
  match T.slots[h.idx]? with
  | some s => if s.live ∧ s.gen = h.gen then .ok s else .error .invalidOrStale
  | none => .error .invalidOrStale

/-- `owned_slot_mut`: live, matching generation, owned by `t`. -/
def ownedSlot (T : Table) (t : Nat) (h : Handle) : Except Err Slot :=
  match liveSlot T h with
  | .ok s => if s.owner = t then .ok s else .error .unauthorizedTenant
  | .error e => .error e

def lookup (T : Table) (t : Nat) (h : Handle) : Except Err Nat :=
  match ownedSlot T t h with
  | .ok s => .ok s.payload
  | .error e => .error e

def Table.update (T : Table) (i : Nat) (s : Slot) : Table :=
  { T with slots := T.slots.set i s }

def insert (T : Table) (t v : Nat) : Except Err (Table × Handle) :=
  match T.free with
  | i :: rest =>
    match T.slots[i]? with
    | some s =>
      .ok ({ T with slots := T.slots.set i { s with owner := t, borrows := 0, live := true, payload := v },
                    free := rest }, ⟨i, s.gen⟩)
    | none => .error .tableFull
  | [] =>
    if T.slots.length < T.cap then
      .ok ({ T with slots := T.slots ++ [{ gen := 1, owner := t, borrows := 0, live := true, payload := v }] },
           ⟨T.slots.length, 1⟩)
    else .error .tableFull

def beginBorrow (T : Table) (t : Nat) (h : Handle) : Except Err Table :=
  match ownedSlot T t h with
  | .ok s =>
    if s.borrows < borrowMax then .ok (T.update h.idx { s with borrows := s.borrows + 1 })
    else .error .borrowOverflow
  | .error e => .error e

def endBorrow (T : Table) (t : Nat) (h : Handle) : Except Err Table :=
  match ownedSlot T t h with
  | .ok s =>
    if s.borrows = 0 then .error .noActiveBorrow
    else .ok (T.update h.idx { s with borrows := s.borrows - 1 })
  | .error e => .error e

/-- Release slot `i` (already taken): bump the generation and free it, or
retire it at the generation ceiling. -/
def release (T : Table) (i : Nat) (s : Slot) : Table :=
  if s.gen < T.maxGen then
    { T with slots := T.slots.set i { s with live := false, borrows := 0, gen := s.gen + 1 },
             free := i :: T.free }
  else
    { T with slots := T.slots.set i { s with live := false, borrows := 0 } }

def dropOwn (T : Table) (t : Nat) (h : Handle) : Except Err (Table × Nat) :=
  match ownedSlot T t h with
  | .ok s =>
    if s.borrows > 0 then .error .activeBorrows
    else .ok (release T h.idx s, s.payload)
  | .error e => .error e

def transfer (T : Table) (t : Nat) (h : Handle) (to : Nat) : Except Err (Table × Handle) :=
  match ownedSlot T t h with
  | .ok s =>
    if s.borrows > 0 then .error .activeBorrows
    else if s.gen < T.maxGen then
      .ok (T.update h.idx { s with gen := s.gen + 1, owner := to }, ⟨h.idx, s.gen + 1⟩)
    else .error .generationExhausted
  | .error e => .error e

/-- Teardown of tenant `t`: release its live slots in ascending index order. -/
def reclaimFrom (t : Nat) : Nat → List Slot → Table × Nat → Table × Nat
  | _, [], acc => acc
  | i, s :: rest, (T, n) =>
    let acc := if s.live ∧ s.owner = t then (release T i s, n + 1) else (T, n)
    reclaimFrom t (i + 1) rest acc

def reclaim (T : Table) (t : Nat) : Table × Nat :=
  reclaimFrom t 0 T.slots (T, 0)

/-- The operations replayed by the differential test. -/
inductive Op where
  | insert (t v : Nat)
  | lookup (t : Nat) (h : Handle)
  | borrow (t : Nat) (h : Handle)
  | unborrow (t : Nat) (h : Handle)
  | drop (t : Nat) (h : Handle)
  | transfer (t : Nat) (h : Handle) (to : Nat)
  | reclaim (t : Nat)

inductive Out where
  | handle (h : Handle)
  | value (v : Nat)
  | unit
  | count (n : Nat)
  | err (e : Err)

def step (T : Table) : Op → Table × Out
  | .insert t v => match insert T t v with
    | .ok (T', h) => (T', .handle h)
    | .error e => (T, .err e)
  | .lookup t h => match lookup T t h with
    | .ok v => (T, .value v)
    | .error e => (T, .err e)
  | .borrow t h => match beginBorrow T t h with
    | .ok T' => (T', .unit)
    | .error e => (T, .err e)
  | .unborrow t h => match endBorrow T t h with
    | .ok T' => (T', .unit)
    | .error e => (T, .err e)
  | .drop t h => match dropOwn T t h with
    | .ok (T', v) => (T', .value v)
    | .error e => (T, .err e)
  | .transfer t h to => match transfer T t h to with
    | .ok (T', h') => (T', .handle h')
    | .error e => (T, .err e)
  | .reclaim t => let (T', n) := reclaim T t; (T', .count n)

def run (T : Table) : List Op → Table
  | [] => T
  | o :: os => run (step T o).1 os

/-! ## Non-forgery -/

theorem liveSlot_ok {T : Table} {h : Handle} {s : Slot} (hs : liveSlot T h = .ok s) :
    T.slots[h.idx]? = some s ∧ s.live = true ∧ s.gen = h.gen := by
  unfold liveSlot at hs
  split at hs
  · rename_i s' hget
    split at hs
    · rename_i hc; cases hs; exact ⟨hget, hc.1, hc.2⟩
    · cases hs
  · cases hs

theorem ownedSlot_ok {T : Table} {t : Nat} {h : Handle} {s : Slot}
    (hs : ownedSlot T t h = .ok s) :
    T.slots[h.idx]? = some s ∧ s.live = true ∧ s.gen = h.gen ∧ s.owner = t := by
  unfold ownedSlot at hs
  split at hs
  · rename_i s' hl
    split at hs
    · rename_i ho; cases hs; have := liveSlot_ok hl; exact ⟨this.1, this.2.1, this.2.2, ho⟩
    · cases hs
  · cases hs

/-- Non-forgery: any integer pair resolves only to the caller's own live
slot with the same generation. -/
theorem lookup_ok_sound {T : Table} {t v : Nat} {h : Handle} (hl : lookup T t h = .ok v) :
    ∃ s, T.slots[h.idx]? = some s ∧ s.live = true ∧ s.gen = h.gen ∧ s.owner = t ∧ s.payload = v := by
  unfold lookup at hl
  split at hl
  · rename_i s hs; cases hl; exact ⟨s, ownedSlot_ok hs |>.1, (ownedSlot_ok hs).2.1,
      (ownedSlot_ok hs).2.2.1, (ownedSlot_ok hs).2.2.2, rfl⟩
  · cases hl

theorem beginBorrow_ok_sound {T T' : Table} {t : Nat} {h : Handle}
    (hb : beginBorrow T t h = .ok T') :
    ∃ s, T.slots[h.idx]? = some s ∧ s.live = true ∧ s.gen = h.gen ∧ s.owner = t := by
  unfold beginBorrow at hb
  split at hb
  · rename_i s hs; exact ⟨s, ownedSlot_ok hs⟩
  · cases hb

theorem endBorrow_ok_sound {T T' : Table} {t : Nat} {h : Handle}
    (hb : endBorrow T t h = .ok T') :
    ∃ s, T.slots[h.idx]? = some s ∧ s.live = true ∧ s.gen = h.gen ∧ s.owner = t ∧ s.borrows > 0 := by
  unfold endBorrow at hb
  split at hb
  · rename_i s hs
    split at hb
    · cases hb
    · rename_i hz; exact ⟨s, (ownedSlot_ok hs).1, (ownedSlot_ok hs).2.1, (ownedSlot_ok hs).2.2.1,
        (ownedSlot_ok hs).2.2.2, Nat.pos_of_ne_zero hz⟩
  · cases hb

/-! ## Borrow-before-drop -/

theorem dropOwn_ok_sound {T T' : Table} {t v : Nat} {h : Handle}
    (hd : dropOwn T t h = .ok (T', v)) :
    ∃ s, T.slots[h.idx]? = some s ∧ s.live = true ∧ s.gen = h.gen ∧ s.owner = t ∧
      s.borrows = 0 ∧ T' = release T h.idx s := by
  unfold dropOwn at hd
  split at hd
  · rename_i s hs
    split at hd
    · cases hd
    · rename_i hb; cases hd
      exact ⟨s, (ownedSlot_ok hs).1, (ownedSlot_ok hs).2.1, (ownedSlot_ok hs).2.2.1,
        (ownedSlot_ok hs).2.2.2, by omega, rfl⟩
  · cases hd

theorem dropOwn_ok_no_borrows {T T' : Table} {t v : Nat} {h : Handle}
    (hd : dropOwn T t h = .ok (T', v)) :
    ∃ s, T.slots[h.idx]? = some s ∧ s.borrows = 0 := by
  obtain ⟨s, hs, -, -, -, hb, -⟩ := dropOwn_ok_sound hd
  exact ⟨s, hs, hb⟩

/-- §30.4 theorem 2: dropping an `own` with an outstanding lend is rejected. -/
theorem wit_drop_own_rejects_active_borrows {T : Table} {t : Nat} {h : Handle} {s : Slot}
    (hs : T.slots[h.idx]? = some s) (h_live : s.live = true) (h_gen : s.gen = h.gen)
    (h_owner : s.owner = t) (h_borrowed : s.borrows > 0) :
    dropOwn T t h = .error .activeBorrows := by
  simp [dropOwn, ownedSlot, liveSlot, hs, h_live, h_gen, h_owner, h_borrowed]

theorem transfer_ok_sound {T T' : Table} {t to : Nat} {h h' : Handle}
    (ht : transfer T t h to = .ok (T', h')) :
    ∃ s, T.slots[h.idx]? = some s ∧ s.live = true ∧ s.gen = h.gen ∧ s.owner = t ∧
      s.borrows = 0 ∧ s.gen < T.maxGen ∧ h' = ⟨h.idx, s.gen + 1⟩ ∧
      T' = T.update h.idx { s with gen := s.gen + 1, owner := to } := by
  unfold transfer at ht
  split at ht
  · rename_i s hs
    split at ht
    · cases ht
    · rename_i hb
      split at ht
      · rename_i hg; cases ht
        exact ⟨s, (ownedSlot_ok hs).1, (ownedSlot_ok hs).2.1, (ownedSlot_ok hs).2.2.1,
          (ownedSlot_ok hs).2.2.2, by omega, hg, rfl, rfl⟩
      · cases ht
  · cases ht

theorem transfer_ok_no_borrows {T T' : Table} {t to : Nat} {h h' : Handle}
    (ht : transfer T t h to = .ok (T', h')) :
    ∃ s, T.slots[h.idx]? = some s ∧ s.borrows = 0 := by
  obtain ⟨s, hs, -, -, -, hb, -⟩ := transfer_ok_sound ht
  exact ⟨s, hs, hb⟩

/-! ## Tenant isolation -/

/-- §30.4 theorem 1: a live slot owned by tenant A never resolves for B. -/
theorem wit_handle_tenant_isolated {T : Table} {h : Handle} {s : Slot} {tenantA tenantB : Nat}
    (hs : T.slots[h.idx]? = some s) (h_owner : s.owner = tenantA) (h_diff : tenantA ≠ tenantB) :
    (∀ v, lookup T tenantB h ≠ .ok v) ∧
    (s.live = true → s.gen = h.gen → lookup T tenantB h = .error .unauthorizedTenant) := by
  constructor
  · intro v hl
    obtain ⟨s', hs', -, -, ho, -⟩ := lookup_ok_sound hl
    rw [hs] at hs'; cases hs'
    exact h_diff (h_owner.symm.trans ho)
  · intro hl hg
    have hne : s.owner ≠ tenantB := by rw [h_owner]; exact h_diff
    simp [lookup, ownedSlot, liveSlot, hs, hl, hg, hne]

/-! ## Generations never decrease, so stale handles stay stale -/

/-- Slot `i`'s generation, or 0 past the end. -/
def genAt (T : Table) (i : Nat) : Nat :=
  match T.slots[i]? with
  | some s => s.gen
  | none => 0

def GenLe (T T' : Table) : Prop := ∀ i, genAt T i ≤ genAt T' i

theorem GenLe.refl (T : Table) : GenLe T T := fun _ => Nat.le_refl _

theorem GenLe.trans {A B C : Table} (h1 : GenLe A B) (h2 : GenLe B C) : GenLe A C :=
  fun i => Nat.le_trans (h1 i) (h2 i)

theorem genLe_set {T : Table} {i : Nat} {s s' : Slot} (hs : T.slots[i]? = some s)
    (hg : s.gen ≤ s'.gen) (free : List Nat) :
    GenLe T { T with slots := T.slots.set i s', free := free } := by
  intro j
  unfold genAt
  by_cases hij : i = j
  · subst hij
    have hlt : i < T.slots.length := by
      have := List.getElem?_eq_some_iff.mp hs; exact this.1
    simp [List.getElem?_set_self hlt, hs, hg]
  · simp [List.getElem?_set_ne hij]

theorem genLe_update {T : Table} {i : Nat} {s s' : Slot} (hs : T.slots[i]? = some s)
    (hg : s.gen ≤ s'.gen) : GenLe T (T.update i s') :=
  genLe_set (free := T.free) hs hg

theorem genLe_release {T : Table} {i : Nat} {s : Slot} (hs : T.slots[i]? = some s) :
    GenLe T (release T i s) := by
  unfold release
  split
  · exact genLe_set hs (by simp) _
  · exact genLe_set hs (by simp) _

theorem genLe_append (T : Table) (s : Slot) :
    GenLe T { T with slots := T.slots ++ [s] } := by
  intro j
  unfold genAt
  by_cases hj : j < T.slots.length
  · simp [List.getElem?_append_left hj]
  · have : T.slots[j]? = none := List.getElem?_eq_none (by omega)
    simp [this]

theorem release_slots_len (T : Table) (i : Nat) (s : Slot) :
    (release T i s).cap = T.cap ∧ (release T i s).maxGen = T.maxGen := by
  unfold release; split <;> exact ⟨rfl, rfl⟩

theorem reclaimFrom_gen (t : Nat) :
    ∀ (i : Nat) (rest : List Slot) (T : Table) (n : Nat),
      (∀ k (hk : k < rest.length), T.slots[i + k]? = some rest[k]) →
      GenLe T (reclaimFrom t i rest (T, n)).1 ∧
      (reclaimFrom t i rest (T, n)).1.maxGen = T.maxGen := by
  intro i rest
  induction rest generalizing i with
  | nil => intro T n _; exact ⟨GenLe.refl T, rfl⟩
  | cons s rest ih =>
    intro T n hrest
    have h0 : T.slots[i]? = some s := by simpa using hrest 0 (by simp)
    simp only [reclaimFrom]
    split
    · -- released slot i; later slots are untouched by the set at i
      have hrest' : ∀ k (hk : k < rest.length),
          (release T i s).slots[i + 1 + k]? = some rest[k] := by
        intro k hk
        have := hrest (k + 1) (by simp; omega)
        unfold release
        split
        · simp only
          rw [List.getElem?_set_ne (by omega)]
          simpa [Nat.add_assoc, Nat.add_comm 1 k] using this
        · simp only
          rw [List.getElem?_set_ne (by omega)]
          simpa [Nat.add_assoc, Nat.add_comm 1 k] using this
      obtain ⟨hle, hmax⟩ := ih (i + 1) (release T i s) (n + 1) hrest'
      exact ⟨GenLe.trans (genLe_release h0) hle, hmax.trans (release_slots_len T i s).2⟩
    · have hrest' : ∀ k (hk : k < rest.length), T.slots[i + 1 + k]? = some rest[k] := by
        intro k hk
        have := hrest (k + 1) (by simp; omega)
        simpa [Nat.add_assoc, Nat.add_comm 1 k] using this
      exact ih (i + 1) T n hrest'

theorem reclaim_gen (T : Table) (t : Nat) : GenLe T (reclaim T t).1 := by
  unfold reclaim
  exact (reclaimFrom_gen t 0 T.slots T 0 (fun k hk => by simp [List.getElem?_eq_getElem hk])).1

theorem step_gen_mono (T : Table) (o : Op) : GenLe T (step T o).1 := by
  cases o with
  | insert t v =>
    simp only [step]
    split
    · rename_i T' h hi
      unfold insert at hi
      split at hi
      · rename_i i rest hfree
        split at hi
        · rename_i s hs; cases hi; exact genLe_set hs (by simp) _
        · cases hi
      · split at hi
        · cases hi; exact genLe_append T _
        · cases hi
    · exact GenLe.refl T
  | lookup t h => simp only [step]; split <;> exact GenLe.refl T
  | borrow t h =>
    simp only [step]
    split
    · rename_i T' hb
      unfold beginBorrow at hb
      split at hb
      · rename_i s hs
        split at hb
        · cases hb; exact genLe_update (ownedSlot_ok hs).1 (by simp)
        · cases hb
      · cases hb
    · exact GenLe.refl T
  | unborrow t h =>
    simp only [step]
    split
    · rename_i T' hb
      unfold endBorrow at hb
      split at hb
      · rename_i s hs
        split at hb
        · cases hb
        · cases hb; exact genLe_update (ownedSlot_ok hs).1 (by simp)
      · cases hb
    · exact GenLe.refl T
  | drop t h =>
    simp only [step]
    split
    · rename_i T' v hd
      obtain ⟨s, hs, -, -, -, -, rfl⟩ := dropOwn_ok_sound hd
      exact genLe_release hs
    · exact GenLe.refl T
  | transfer t h to =>
    simp only [step]
    split
    · rename_i T' h' ht
      obtain ⟨s, hs, -, -, -, -, -, -, rfl⟩ := transfer_ok_sound ht
      exact genLe_update hs (by simp)
    · exact GenLe.refl T
  | reclaim t => exact reclaim_gen T t

theorem run_gen_mono (T : Table) (os : List Op) : GenLe T (run T os) := by
  induction os generalizing T with
  | nil => exact GenLe.refl T
  | cons o os ih => exact GenLe.trans (step_gen_mono T o) (ih _)

theorem lookup_fails_of_gen_lt {T : Table} {t : Nat} {h : Handle}
    (hlt : h.gen < genAt T h.idx) : ∀ v, lookup T t h ≠ .ok v := by
  intro v hl
  obtain ⟨s, hs, -, hg, -⟩ := lookup_ok_sound hl
  unfold genAt at hlt
  simp [hs] at hlt
  omega

/-- A stale handle (generation below its slot's) stays stale after any
operation, for every tenant. -/
theorem stale_stays_stale {T : Table} {h : Handle} (o : Op)
    (hlt : h.gen < genAt T h.idx) :
    h.gen < genAt (step T o).1 h.idx ∧ ∀ t v, lookup (step T o).1 t h ≠ .ok v := by
  have := Nat.lt_of_lt_of_le hlt (step_gen_mono T o h.idx)
  exact ⟨this, fun _ => lookup_fails_of_gen_lt this⟩

theorem run_stale_stays_stale {T : Table} {h : Handle} (os : List Op)
    (hlt : h.gen < genAt T h.idx) : ∀ t v, lookup (run T os) t h ≠ .ok v :=
  fun _ => lookup_fails_of_gen_lt (Nat.lt_of_lt_of_le hlt (run_gen_mono T os h.idx))

/-- Dropping an `own` below the generation ceiling stales its handle. -/
theorem dropOwn_ok_stales {T T' : Table} {t v : Nat} {h : Handle}
    (hd : dropOwn T t h = .ok (T', v)) (hmax : h.gen < T.maxGen) :
    h.gen < genAt T' h.idx := by
  obtain ⟨s, hs, -, hg, -, -, rfl⟩ := dropOwn_ok_sound hd
  have hlt : h.idx < T.slots.length := (List.getElem?_eq_some_iff.mp hs).1
  unfold release genAt
  rw [if_pos (by omega)]
  simp [List.getElem?_set_self hlt]
  omega

/-- Transferring an `own` stales the old handle; the new one is current. -/
theorem transfer_ok_stales {T T' : Table} {t to : Nat} {h h' : Handle}
    (ht : transfer T t h to = .ok (T', h')) :
    h.gen < genAt T' h.idx ∧ h'.idx = h.idx ∧ h'.gen = genAt T' h.idx := by
  obtain ⟨s, hs, -, hg, -, -, -, rfl, rfl⟩ := transfer_ok_sound ht
  have hlt : h.idx < T.slots.length := (List.getElem?_eq_some_iff.mp hs).1
  unfold genAt Table.update
  simp [List.getElem?_set_self hlt]
  omega

/-! ## Rendering for the differential fixture -/

def Out.render : Out → String
  | .handle h => s!"ok {h.idx} {h.gen}"
  | .value v => s!"ok {v}"
  | .unit => "ok"
  | .count n => s!"ok {n}"
  | .err e => s!"err {e.name}"

end Vesper.NexusHandles

#print axioms Vesper.NexusHandles.lookup_ok_sound
#print axioms Vesper.NexusHandles.beginBorrow_ok_sound
#print axioms Vesper.NexusHandles.endBorrow_ok_sound
#print axioms Vesper.NexusHandles.dropOwn_ok_sound
#print axioms Vesper.NexusHandles.dropOwn_ok_no_borrows
#print axioms Vesper.NexusHandles.wit_drop_own_rejects_active_borrows
#print axioms Vesper.NexusHandles.transfer_ok_sound
#print axioms Vesper.NexusHandles.transfer_ok_no_borrows
#print axioms Vesper.NexusHandles.wit_handle_tenant_isolated
#print axioms Vesper.NexusHandles.step_gen_mono
#print axioms Vesper.NexusHandles.run_gen_mono
#print axioms Vesper.NexusHandles.stale_stays_stale
#print axioms Vesper.NexusHandles.run_stale_stays_stale
#print axioms Vesper.NexusHandles.dropOwn_ok_stales
#print axioms Vesper.NexusHandles.transfer_ok_stales
