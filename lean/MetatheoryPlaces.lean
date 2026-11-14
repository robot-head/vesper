/-
Vesper mechanized metatheory, wave eight (issue #455): mutable places, the
Place Capability Graph, second-class borrows and Zero-Pin (design §3-§5 and
the Zero-Pin corollary of §6, Appendix A).

The earlier fragments (`lean/MetatheoryFull.lean`, `lean/Metatheory/**`) are
substitution calculi whose places are functional values. This file is a
separate calculus, close to the Stage 3 Place IR checked by the Place
Capability Graph (PCG):

* a frame holds a store of mutable owned locals; a place is a root (an owned
  local or a borrow slot) followed by a field-tree path;
* borrows are second class: `shr`/`mut` borrows exist only as callee
  parameters created by [T-CallProj] for the duration of a call; there is no
  reference type, no operation stores a borrow in a value, and a spawned
  fiber (the closure/fiber entry, [T-Abs]) may receive only owned (`sink`)
  arguments;
* the static state is a PCG: for each owned local a capability tree over its
  field tree (`full`, `moved`, or split into the two fields), and for each
  borrow slot its mode and type. Statements are typed flow-sensitively,
  `G ⊢ s ⊣ G'`, and the two arms of a branch must join in the same PCG;
* the machine is a pool of fibers, each a stack of frames. A borrow slot
  holds a fiber-relative location (frame position, local, path). A fiber
  suspends at `yield` (a linear effect, `res = linear`) and is resumed,
  possibly after being relocated to a new base address.

Proved (all theorems about this Lean model, not about the Rust compiler):
type safety (preservation and progress), borrow soundness on every reachable
state (no two live overlapping borrows unless they form a reborrow chain
whose older end is `mut`; every slot points into a strictly older live frame
of its own fiber, at an initialized place of its type), total linear
exhaustion (every minted handle is consumed exactly once by the end of a
terminating run), and the Zero-Pin corollary (no suspended stack contains a
pointer into itself or into another fiber, values contain no pointers, so
relocating a suspended fiber by copying it to a new base preserves typing and
commutes with every step).
-/
set_option linter.unusedSimpArgs false

namespace Vesper.Metatheory.Places

/-! ## Modes and types -/

inductive Mode : Type where
  | copy | affine | linear
  deriving DecidableEq, Repr

namespace Mode
@[simp] def rank : Mode → Nat
  | copy => 0 | affine => 1 | linear => 2
def Le (a b : Mode) : Prop := a.rank ≤ b.rank
instance (a b : Mode) : Decidable (Le a b) := inferInstanceAs (Decidable (a.rank ≤ b.rank))
def join (a b : Mode) : Mode := if a.rank ≤ b.rank then b else a

theorem le_refl (a : Mode) : Le a a := Nat.le_refl _
theorem le_trans {a b c : Mode} : Le a b → Le b c → Le a c := Nat.le_trans
theorem le_join_left (a b : Mode) : Le a (join a b) := by
  cases a <;> cases b <;> decide
theorem le_join_right (a b : Mode) : Le b (join a b) := by
  cases a <;> cases b <;> decide
theorem join_le {a b c : Mode} : Le a c → Le b c → Le (join a b) c := by
  cases a <;> cases b <;> cases c <;> decide
theorem join_eq_copy {a b : Mode} : join a b = copy ↔ a = copy ∧ b = copy := by
  cases a <;> cases b <;> decide
end Mode

/-- Types: `nat` (copy), `buf` (an affine buffer, dropped by synchronous
cleanup), `hdl` (a linear obligation such as `WriteTxn`) and two-field
structs, whose field tree is the place structure. -/
inductive Ty : Type where
  | nat
  | buf
  | hdl
  | pair (a b : Ty)
  deriving DecidableEq, Repr

/-- [K-Prim] and [K-Adt] (without field-modality overrides). -/
def Ty.kind : Ty → Mode
  | .nat => .copy
  | .buf => .affine
  | .hdl => .linear
  | .pair a b => Mode.join a.kind b.kind

/-- Field-tree paths: `false` selects the first field, `true` the second. -/
abbrev Path := List Bool

def Ty.at : Ty → Path → Option Ty
  | t, [] => some t
  | .pair a _, false :: p => a.at p
  | .pair _ b, true :: p => b.at p
  | _, _ :: _ => none

/-- `Pre p q`: `p` is a prefix of `q` (the place `q` lies inside `p`). -/
def Pre : Path → Path → Prop
  | [], _ => True
  | _ :: _, [] => False
  | a :: p, b :: q => a = b ∧ Pre p q

/-- Field-tree overlap of two paths of the same root. -/
def Ovl (p q : Path) : Prop := Pre p q ∨ Pre q p

/-! ## Values -/

/-- Runtime values. There is no pointer: borrows are second class and never
values. `undef` is a moved-out (uninitialized) place. -/
inductive Val : Type where
  | nat (n : Nat)
  | buf
  | hdl (r : Nat)
  | pair (a b : Val)
  | undef
  deriving DecidableEq, Repr

def Val.at : Val → Path → Val
  | v, [] => v
  | .pair a _, false :: p => a.at p
  | .pair _ b, true :: p => b.at p
  | _, _ :: _ => .undef

/-- Write `w` at path `p`; an `undef` struct is expanded field by field. -/
def Val.set : Val → Path → Val → Val
  | _, [], w => w
  | .pair a b, false :: p, w => .pair (a.set p w) b
  | .pair a b, true :: p, w => .pair a (b.set p w)
  | .undef, false :: p, w => .pair (Val.undef.set p w) .undef
  | .undef, true :: p, w => .pair .undef (Val.undef.set p w)
  | v, _ :: _, _ => v

/-- A fully initialized value of a type. -/
def VT : Val → Ty → Prop
  | .nat _, .nat => True
  | .buf, .buf => True
  | .hdl _, .hdl => True
  | .pair a b, .pair s t => VT a s ∧ VT b t
  | _, _ => False

/-- The number of occurrences of handle `r` in a value. -/
def Val.cnt (r : Nat) : Val → Nat
  | .hdl r' => if r' = r then 1 else 0
  | .pair a b => a.cnt r + b.cnt r
  | _ => 0

/-- Every handle in the value is below `n` (fresh-id discipline). -/
def Val.allLt (n : Nat) : Val → Prop
  | .hdl r => r < n
  | .pair a b => a.allLt n ∧ b.allLt n
  | _ => True

/-! ## The Place Capability Graph of an owned local -/

/-- A capability tree over a local's field tree: `full t` (owned and
initialized: the PCG's exclusive capability), `moved t` (no capability), or
a struct split into its two field capabilities. -/
inductive Sh : Type where
  | full (t : Ty)
  | moved (t : Ty)
  | pair (a b : Sh)
  deriving DecidableEq, Repr

def Sh.ty : Sh → Ty
  | .full t => t
  | .moved t => t
  | .pair a b => .pair a.ty b.ty

/-- The type of a fully initialized capability tree. -/
def Sh.full? : Sh → Option Ty
  | .full t => some t
  | .moved _ => none
  | .pair a b =>
    match a.full?, b.full? with
    | some s, some t => some (.pair s t)
    | _, _ => none

/-- discard: every initialized leaf is at most affine (copy or affine). -/
def Sh.dropOK : Sh → Prop
  | .full t => Mode.Le t.kind .affine
  | .moved _ => True
  | .pair a b => a.dropOK ∧ b.dropOK

def Sh.at : Sh → Path → Option Sh
  | s, [] => some s
  | .full (.pair a _), false :: p => (Sh.full a).at p
  | .full (.pair _ b), true :: p => (Sh.full b).at p
  | .moved (.pair a _), false :: p => (Sh.moved a).at p
  | .moved (.pair _ b), true :: p => (Sh.moved b).at p
  | .pair a _, false :: p => a.at p
  | .pair _ b, true :: p => b.at p
  | _, _ :: _ => none

/-- Replace the capability at a path, splitting `full`/`moved` structs. -/
def Sh.set : Sh → Path → Sh → Sh
  | _, [], n => n
  | .full (.pair a b), false :: p, n => .pair ((Sh.full a).set p n) (.full b)
  | .full (.pair a b), true :: p, n => .pair (.full a) ((Sh.full b).set p n)
  | .moved (.pair a b), false :: p, n => .pair ((Sh.moved a).set p n) (.moved b)
  | .moved (.pair a b), true :: p, n => .pair (.moved a) ((Sh.moved b).set p n)
  | .pair a b, false :: p, n => .pair (a.set p n) b
  | .pair a b, true :: p, n => .pair a (b.set p n)
  | s, _ :: _, _ => s

/-- A value agrees with a capability tree. -/
def VS : Val → Sh → Prop
  | v, .full t => VT v t
  | v, .moved _ => v = .undef
  | .pair a b, .pair s t => VS a s ∧ VS b t
  | _, .pair _ _ => False

/-! ## Value and capability-tree lemmas -/

/-- The path exists in the value, or runs into a moved-out (`undef`) part. -/
def ValidP : Val → Path → Prop
  | _, [] => True
  | .pair a _, false :: p => ValidP a p
  | .pair _ b, true :: p => ValidP b p
  | .undef, _ :: _ => True
  | _, _ :: _ => False


@[simp] theorem Val.at_nil (v : Val) : v.at [] = v := by cases v <;> rfl
@[simp] theorem Val.set_nil (v w : Val) : v.set [] w = w := by cases v <;> rfl
@[simp] theorem Sh.at_nil (s : Sh) : s.at [] = some s := by
  cases s with
  | full t => cases t <;> rfl
  | moved t => cases t <;> rfl
  | pair => rfl
@[simp] theorem Sh.set_nil (s n : Sh) : s.set [] n = n := by
  cases s with
  | full t => cases t <;> rfl
  | moved t => cases t <;> rfl
  | pair => rfl
@[simp] theorem VS_full (v : Val) (t : Ty) : VS v (.full t) = VT v t := by cases v <;> rfl
@[simp] theorem VS_moved (v : Val) (t : Ty) : VS v (.moved t) = (v = .undef) := by
  cases v <;> rfl
@[simp] theorem Ty.at_nil (t : Ty) : t.at [] = some t := by cases t <;> rfl
@[simp] theorem ValidP_nil (v : Val) : ValidP v [] := by cases v <;> trivial

theorem vt_unique : ∀ {v : Val} {s t : Ty}, VT v s → VT v t → s = t
  | .nat _, .nat, .nat, _, _ => rfl
  | .buf, .buf, .buf, _, _ => rfl
  | .hdl _, .hdl, .hdl, _, _ => rfl
  | .pair _ _, .pair _ _, .pair _ _, ⟨h1, h2⟩, ⟨h3, h4⟩ => by
    rw [vt_unique h1 h3, vt_unique h2 h4]

theorem vt_not_undef : ∀ {t : Ty}, ¬ VT .undef t := by
  intro t h; cases t <;> simp [VT] at h

theorem vs_full : ∀ {s : Sh} {v : Val} {t : Ty}, VS v s → s.full? = some t → VT v t
  | .full _, _, _, h, e => by simp [Sh.full?] at e; subst e; simpa using h
  | .moved _, _, _, _, e => by simp [Sh.full?] at e
  | .pair a b, v, t, h, e => by
    cases v <;> simp [VS] at h
    rename_i va vb
    simp only [Sh.full?] at e
    split at e
    · rename_i s1 t1 h1 h2
      simp at e; subst e
      exact ⟨vs_full h.1 h1, vs_full h.2 h2⟩
    · simp at e

theorem vt_at : ∀ {p : Path} {v : Val} {t u : Ty}, VT v t → t.at p = some u → VT (v.at p) u
  | [], _, _, _, h, e => by simp [Ty.at] at e; subst e; simpa [Val.at] using h
  | b :: p, v, t, u, h, e => by
    cases b <;> cases t <;> simp [Ty.at] at e <;> cases v <;> simp [VT] at h
    · simpa [Val.at] using vt_at h.1 e
    · simpa [Val.at] using vt_at h.2 e

theorem vt_valid : ∀ {p : Path} {v : Val} {t u : Ty}, VT v t → t.at p = some u → ValidP v p
  | [], _, _, _, _, _ => by simp [ValidP]
  | b :: p, v, t, u, h, e => by
    cases b <;> cases t <;> simp [Ty.at] at e <;> cases v <;> simp [VT] at h
    · simpa [ValidP] using vt_valid h.1 e
    · simpa [ValidP] using vt_valid h.2 e

theorem at_append : ∀ (p q : Path) (v : Val), v.at (p ++ q) = (v.at p).at q
  | [], _, _ => by simp
  | b :: p, q, v => by
    cases v <;> cases b <;> simp [Val.at, at_append p q] <;> cases q <;> simp [Val.at]

theorem ty_at_append : ∀ (p q : Path) (t : Ty), t.at (p ++ q) = (t.at p).bind (·.at q)
  | [], _, _ => by simp [Ty.at]
  | b :: p, q, t => by
    cases t <;> cases b <;> simp [Ty.at, ty_at_append p q]

theorem valid_of_vt_at : ∀ {p : Path} {v : Val} {t : Ty}, VT (v.at p) t → ValidP v p
  | [], _, _, _ => by simp [ValidP]
  | b :: p, v, t, h => by
    cases v <;> cases b <;> simp [Val.at, ValidP] at h ⊢ <;>
      first | exact vt_not_undef h | exact valid_of_vt_at h

theorem at_set_same : ∀ {p : Path} {v : Val} (w : Val), ValidP v p → (v.set p w).at p = w
  | [], _, _, _ => by simp
  | b :: p, v, w, h => by
    cases v <;> cases b <;> simp [ValidP, Val.set, Val.at] at h ⊢
    all_goals first
      | exact at_set_same w h
      | exact at_set_same (v := .undef) w (by cases p <;> simp [ValidP])

theorem undef_at : ∀ (p : Path), Val.undef.at p = .undef
  | [] => rfl
  | _ :: _ => rfl

theorem at_set_disj : ∀ {p q : Path} (v w : Val), ¬ Ovl p q → (v.set p w).at q = v.at q
  | [], q, _, _, h => absurd (Or.inl trivial) h
  | _ :: _, [], _, _, h => absurd (Or.inr trivial) h
  | a :: p, b :: q, v, w, h => by
    have h' : a ≠ b ∨ ¬ Ovl p q := by
      by_cases e : a = b
      · subst e; right; intro o; apply h; cases o with
        | inl o => exact Or.inl ⟨rfl, o⟩
        | inr o => exact Or.inr ⟨rfl, o⟩
      · exact Or.inl e
    cases v <;> cases a <;> cases b <;> simp [Val.set, Val.at, undef_at] at h' ⊢
    all_goals first
      | exact at_set_disj _ _ h'
      | (rw [at_set_disj _ _ h', undef_at])

theorem at_set_inside : ∀ (q r : Path) (v w : Val), ValidP v q →
    (v.set (q ++ r) w).at q = (v.at q).set r w
  | [], _, _, _, _ => by simp
  | b :: q, r, v, w, h => by
    cases v <;> cases b <;> simp [ValidP, Val.set, Val.at] at h ⊢
    all_goals first
      | exact at_set_inside q r _ w h
      | (rw [at_set_inside q r _ w (by cases q <;> simp [ValidP]), undef_at])

theorem cnt_set : ∀ (r : Nat) {p : Path} {v : Val} (w : Val), ValidP v p →
    (v.set p w).cnt r + (v.at p).cnt r = v.cnt r + w.cnt r
  | _, [], v, w, _ => by simp [Val.set, Val.at]; omega
  | r, b :: p, v, w, h => by
    have hu : ValidP .undef p := by cases p <;> simp [ValidP]
    have := cnt_set r (v := .undef) w hu
    rw [undef_at] at this; simp only [Val.cnt] at this
    cases v <;> cases b <;> (try simp [ValidP] at h) <;> simp [Val.set, Val.at, Val.cnt]
    all_goals first
      | (have := cnt_set r w h; omega)
      | omega

theorem allLt_at : ∀ {n : Nat} (p : Path) {v : Val}, v.allLt n → (v.at p).allLt n
  | _, [], _, h => by simpa using h
  | _, b :: p, v, h => by
    cases v <;> cases b <;> simp [Val.at, Val.allLt] at h ⊢
    all_goals first | exact allLt_at p h.1 | exact allLt_at p h.2

theorem allLt_set : ∀ {n : Nat} (p : Path) {v w : Val}, v.allLt n → w.allLt n →
    (v.set p w).allLt n
  | _, [], _, _, _, hw => by simpa using hw
  | n, b :: p, v, w, hv, hw => by
    have hu : Val.undef.allLt n := trivial
    cases v <;> cases b <;> (try simp [Val.allLt] at hv) <;> simp [Val.set, Val.allLt]
    all_goals first
      | exact ⟨allLt_set p hv.1 hw, hv.2⟩
      | exact ⟨hv.1, allLt_set p hv.2 hw⟩
      | exact allLt_set p hu hw
      | exact ⟨allLt_set p hu hw, hu⟩
      | exact ⟨hu, allLt_set p hu hw⟩
      | omega

theorem allLt_mono : ∀ {n m : Nat} {v : Val}, v.allLt n → n ≤ m → v.allLt m
  | _, _, .hdl _, h, hm => by simp [Val.allLt] at h ⊢; omega
  | _, _, .pair _ _, h, hm => ⟨allLt_mono h.1 hm, allLt_mono h.2 hm⟩
  | _, _, .nat _, _, _ => trivial
  | _, _, .buf, _, _ => trivial
  | _, _, .undef, _, _ => trivial

theorem cnt_allLt : ∀ {n : Nat} {v : Val} (r : Nat), v.allLt n → n ≤ r → v.cnt r = 0
  | _, .hdl r', r, h, hr => by simp [Val.allLt] at h; simp [Val.cnt]; omega
  | _, .pair _ _, r, h, hr => by simp [Val.cnt, cnt_allLt r h.1 hr, cnt_allLt r h.2 hr]
  | _, .nat _, _, _, _ => rfl
  | _, .buf, _, _, _ => rfl
  | _, .undef, _, _, _ => rfl

/-- A value of a droppable (copy or affine) type holds no linear handle. -/
theorem cnt_droppable : ∀ {v : Val} {t : Ty} (r : Nat), VT v t → Mode.Le t.kind .affine →
    v.cnt r = 0
  | .nat _, .nat, _, _, _ => rfl
  | .buf, .buf, _, _, _ => rfl
  | .hdl _, .hdl, _, _, hk => by simp [Ty.kind, Mode.Le] at hk
  | .pair _ _, .pair s t, r, ⟨h1, h2⟩, hk => by
    simp only [Ty.kind] at hk
    simp [Val.cnt, cnt_droppable r h1 (Mode.le_trans (Mode.le_join_left _ _) hk),
      cnt_droppable r h2 (Mode.le_trans (Mode.le_join_right _ _) hk)]

theorem cnt_vs_drop : ∀ {s : Sh} {v : Val} (r : Nat), VS v s → s.dropOK → v.cnt r = 0
  | .full _, _, r, h, hd => cnt_droppable r (by simpa using h) hd
  | .moved _, _, _, h, _ => by simp [VS] at h; subst h; rfl
  | .pair _ _, v, r, h, hd => by
    cases v <;> simp [VS] at h
    simp [Val.cnt, cnt_vs_drop r h.1 hd.1, cnt_vs_drop r h.2 hd.2]

theorem vt_replace : ∀ {q : Path} {v w : Val} {t u : Ty}, VT v t → VT (v.at q) u → VT w u →
    VT (v.set q w) t
  | [], v, w, t, u, h, hq, hw => by
    simp [Val.at] at hq; simp [Val.set]; rw [vt_unique h hq]; exact hw
  | b :: q, v, w, t, u, h, hq, hw => by
    cases v <;> cases t <;> simp [VT] at h
    all_goals cases b <;> simp [Val.at, Val.set, VT] at hq ⊢
    all_goals first
      | exact absurd hq vt_not_undef
      | exact ⟨vt_replace h.1 hq hw, h.2⟩
      | exact ⟨h.1, vt_replace h.2 hq hw⟩

theorem vs_replace : ∀ {q : Path} {s : Sh} {v w : Val} {t : Ty}, VS v s → VT (v.at q) t →
    VT w t → VS (v.set q w) s
  | q, .full _, _, _, _, h, hq, hw => by simpa using vt_replace (by simpa using h) hq hw
  | q, .moved _, v, _, _, h, hq, _ => by
    simp [VS] at h; subst h; rw [undef_at] at hq; exact absurd hq vt_not_undef
  | [], .pair a b, v, w, t, h, hq, hw => by
    cases v <;> simp [VS] at h
    rename_i va vb
    simp [Val.at] at hq
    cases t <;> simp [VT] at hq
    cases w <;> simp [VT] at hw
    simp [Val.set, VS]
    exact ⟨by simpa using vs_replace (q := []) h.1 (by simpa using hq.1) hw.1,
      by simpa using vs_replace (q := []) h.2 (by simpa using hq.2) hw.2⟩
  | c :: q, .pair a b, v, w, t, h, hq, hw => by
    cases v <;> simp [VS] at h
    cases c <;> simp [Val.at, Val.set, VS] at hq ⊢
    · exact ⟨vs_replace h.1 hq hw, h.2⟩
    · exact ⟨h.1, vs_replace h.2 hq hw⟩

theorem vs_at : ∀ {p : Path} {s s' : Sh} {v : Val}, VS v s → s.at p = some s' → VS (v.at p) s'
  | [], s, s', v, h, e => by simp [Sh.at] at e; subst e; simpa using h
  | b :: p, s, s', v, h, e => by
    match s, b with
    | .full (.pair t1 t2), false =>
      cases v <;> simp [VS, VT] at h
      simp [Sh.at] at e
      simpa [Val.at] using vs_at (p := p) (s := .full t1) (by simpa using h.1) e
    | .full (.pair t1 t2), true =>
      cases v <;> simp [VS, VT] at h
      simp [Sh.at] at e
      simpa [Val.at] using vs_at (p := p) (s := .full t2) (by simpa using h.2) e
    | .moved (.pair t1 t2), false =>
      simp [VS] at h; subst h; simp [Val.at, Sh.at] at e ⊢
      have := vs_at (v := .undef) (s := .moved t1) rfl e; rwa [undef_at] at this
    | .moved (.pair t1 t2), true =>
      simp [VS] at h; subst h; simp [Val.at, Sh.at] at e ⊢
      have := vs_at (v := .undef) (s := .moved t2) rfl e; rwa [undef_at] at this
    | .pair s1 s2, false =>
      cases v <;> simp [VS] at h
      simp [Sh.at] at e; simpa [Val.at] using vs_at h.1 e
    | .pair s1 s2, true =>
      cases v <;> simp [VS] at h
      simp [Sh.at] at e; simpa [Val.at] using vs_at h.2 e
    | .full .nat, _ => simp [Sh.at] at e
    | .full .buf, _ => simp [Sh.at] at e
    | .full .hdl, _ => simp [Sh.at] at e
    | .moved .nat, _ => simp [Sh.at] at e
    | .moved .buf, _ => simp [Sh.at] at e
    | .moved .hdl, _ => simp [Sh.at] at e

theorem vs_valid : ∀ {p : Path} {s s' : Sh} {v : Val}, VS v s → s.at p = some s' → ValidP v p
  | [], _, _, _, _, _ => by simp [ValidP]
  | b :: p, s, s', v, h, e => by
    match s, b with
    | .full (.pair t1 t2), false =>
      cases v <;> simp [VS, VT] at h
      simp [Sh.at] at e
      simpa [ValidP] using vs_valid (s := .full t1) (by simpa using h.1) e
    | .full (.pair t1 t2), true =>
      cases v <;> simp [VS, VT] at h
      simp [Sh.at] at e
      simpa [ValidP] using vs_valid (s := .full t2) (by simpa using h.2) e
    | .moved (.pair _ _), _ => simp [VS] at h; subst h; simp [ValidP]
    | .pair s1 s2, false =>
      cases v <;> simp [VS] at h
      simp [Sh.at] at e; simpa [ValidP] using vs_valid h.1 e
    | .pair s1 s2, true =>
      cases v <;> simp [VS] at h
      simp [Sh.at] at e; simpa [ValidP] using vs_valid h.2 e
    | .full .nat, _ => simp [Sh.at] at e
    | .full .buf, _ => simp [Sh.at] at e
    | .full .hdl, _ => simp [Sh.at] at e
    | .moved .nat, _ => simp [Sh.at] at e
    | .moved .buf, _ => simp [Sh.at] at e
    | .moved .hdl, _ => simp [Sh.at] at e

theorem vs_set : ∀ {p : Path} {s s0 n : Sh} {v w : Val}, VS v s → s.at p = some s0 → VS w n →
    VS (v.set p w) (s.set p n)
  | [], _, _, _, _, _, _, _, hw => by simpa [Val.set, Sh.set] using hw
  | b :: p, s, s0, n, v, w, h, e, hw => by
    match s, b with
    | .full (.pair t1 t2), false =>
      cases v <;> simp [VS, VT] at h
      simp [Sh.at] at e
      simp [Val.set, Sh.set, VS]
      exact ⟨vs_set (s := .full t1) (by simpa using h.1) e hw, by simpa using h.2⟩
    | .full (.pair t1 t2), true =>
      cases v <;> simp [VS, VT] at h
      simp [Sh.at] at e
      simp [Val.set, Sh.set, VS]
      exact ⟨by simpa using h.1, vs_set (s := .full t2) (by simpa using h.2) e hw⟩
    | .moved (.pair t1 t2), false =>
      simp [VS] at h; subst h; simp [Sh.at] at e
      simp [Val.set, Sh.set, VS]
      exact vs_set (s := .moved t1) (v := .undef) rfl e hw
    | .moved (.pair t1 t2), true =>
      simp [VS] at h; subst h; simp [Sh.at] at e
      simp [Val.set, Sh.set, VS]
      exact vs_set (s := .moved t2) (v := .undef) rfl e hw
    | .pair s1 s2, false =>
      cases v <;> simp [VS] at h
      simp [Sh.at] at e; simp [Val.set, Sh.set, VS]; exact ⟨vs_set h.1 e hw, h.2⟩
    | .pair s1 s2, true =>
      cases v <;> simp [VS] at h
      simp [Sh.at] at e; simp [Val.set, Sh.set, VS]; exact ⟨h.1, vs_set h.2 e hw⟩
    | .full .nat, _ => simp [Sh.at] at e
    | .full .buf, _ => simp [Sh.at] at e
    | .full .hdl, _ => simp [Sh.at] at e
    | .moved .nat, _ => simp [Sh.at] at e
    | .moved .buf, _ => simp [Sh.at] at e
    | .moved .hdl, _ => simp [Sh.at] at e

/-! ## Syntax: places, operands, statements, functions -/

/-- A place root: an owned local of the current frame, or one of its
second-class borrow slots (a callee parameter of mode `in`/`inout`). -/
inductive Root : Type where
  | own (x : Nat)
  | bor (b : Nat)
  deriving DecidableEq, Repr

/-- A place `root.π`. -/
structure Place where
  root : Root
  path : Path
  deriving DecidableEq, Repr

/-- Borrow modes: `shr` (an `in` parameter) and `mut` (an `inout` parameter). -/
inductive Mu : Type where
  | shr | mut
  deriving DecidableEq, Repr

/-- Field-tree disjointness of two places of the same frame, `p ⊥ q`. -/
def Place.Disj (p q : Place) : Prop := p.root ≠ q.root ∨ ¬ Ovl p.path q.path

/-- Operands. `copy` reads a copy-kind value through any root (an owned
local or a borrow, [T-Var]); `move` takes the value out of an owned place. -/
inductive Opnd : Type where
  | lit (n : Nat)
  | newBuf
  | mint
  | copy (p : Place)
  | move (p : Place)
  deriving DecidableEq, Repr

/-- Place IR statements. `call f ss bs` passes the places `ss` as `sink`
arguments and `bs` as `in`/`inout` arguments (the callee's declaration fixes
each mode). `spawn f ss` starts a fiber; `yield` performs a linear suspending
effect (`Async`/`Storage`, `res = linear`). -/
inductive Stmt : Type where
  | assign (p : Place) (o : Opnd)
  | swap (p q : Place)
  | consume (o : Opnd)
  | drop (o : Opnd)
  | ifz (o : Opnd) (b1 b2 : List Stmt)
  | call (f : Nat) (ss bs : List Place)
  | spawn (f : Nat) (ss : List Place)
  | yield

/-- A function: `sink` parameters (owned locals `0 ..`), borrow parameters
(slots), further owned locals (initially moved) and a body. Functions return
through their `inout` parameters. -/
structure Fn where
  sinks : List Ty
  bors : List (Mu × Ty)
  locals : List Ty
  body : List Stmt

abbrev Prog := List Fn

/-! ## The Place Capability Graph and flow-sensitive typing -/

/-- The PCG of a frame: a capability tree per owned local, and the mode and
type of each borrow slot. -/
structure Pcg where
  own : List Sh
  slots : List (Mu × Ty)

/-- The type of a fully initialized place, through any root. -/
def Pcg.read (G : Pcg) : Place → Option Ty
  | ⟨.own x, p⟩ =>
    match G.own[x]? with
    | some s => match s.at p with
      | some s' => s'.full?
      | none => none
    | none => none
  | ⟨.bor b, p⟩ =>
    match G.slots[b]? with
    | some (_, t) => t.at p
    | none => none

def Pcg.setOwn (G : Pcg) (x : Nat) (p : Path) (n : Sh) : Pcg :=
  match G.own[x]? with
  | some s => { G with own := G.own.set x (s.set p n) }
  | none => G

/-- Places that may be written: owned places and places under a `mut` slot. -/
def Pcg.Writable (G : Pcg) : Place → Prop
  | ⟨.own _, _⟩ => True
  | ⟨.bor b, _⟩ => ∃ t, G.slots[b]? = some (.mut, t)

/-- discard(Γ) at a frame's exit: every owned capability tree is droppable. -/
def Done (G : Pcg) : Prop := ∀ s ∈ G.own, s.dropOK

/-- Operand typing `G ⊢ o : t ⊣ G'`. -/
inductive TyO : Pcg → Opnd → Ty → Pcg → Prop where
  | lit (G : Pcg) (n : Nat) : TyO G (.lit n) .nat G
  | newBuf (G : Pcg) : TyO G .newBuf .buf G
  | mint (G : Pcg) : TyO G .mint .hdl G
  | copy {G : Pcg} {p : Place} {t : Ty} : G.read p = some t → t.kind = .copy →
      TyO G (.copy p) t G
  | move {G : Pcg} {x : Nat} {p : Path} {t : Ty} : G.read ⟨.own x, p⟩ = some t →
      TyO G (.move ⟨.own x, p⟩) t (G.setOwn x p (.moved t))

/-- Assignment targets: an owned place whose current capability tree has the
type and is droppable (the old value is discarded), or a place under a `mut`
slot holding a droppable type. -/
inductive TyAsg : Pcg → Place → Ty → Pcg → Prop where
  | own {G : Pcg} {x : Nat} {p : Path} {s s0 : Sh} {t : Ty} :
      G.own[x]? = some s → s.at p = some s0 → s0.ty = t → s0.dropOK →
      TyAsg G ⟨.own x, p⟩ t (G.setOwn x p (.full t))
  | bor {G : Pcg} {b : Nat} {p : Path} {t0 t : Ty} :
      G.slots[b]? = some (.mut, t0) → t0.at p = some t → Mode.Le t.kind .affine →
      TyAsg G ⟨.bor b, p⟩ t G

/-- `sink` arguments: owned, fully initialized places of the parameter types. -/
def SinksOK (G : Pcg) : List Place → List Ty → Prop
  | [], [] => True
  | p :: ps, t :: ts => (∃ x, p.root = .own x) ∧ G.read p = some t ∧ SinksOK G ps ts
  | _, _ => False

/-- Borrow arguments: initialized places of the parameter types; an `inout`
argument must be writable (a reborrow `ν ≥ μ`: `in` reborrows anything,
`inout` only owned places and `mut` slots). -/
def BorsOK (G : Pcg) : List Place → List (Mu × Ty) → Prop
  | [], [] => True
  | p :: ps, (μ, t) :: ts => G.read p = some t ∧ (μ = .mut → G.Writable p) ∧ BorsOK G ps ts
  | _, _ => False

/-- Move the `sink` arguments out of the caller's PCG. -/
def moveSinks (G : Pcg) : List Place → List Ty → Pcg
  | ⟨.own x, p⟩ :: ps, t :: ts => moveSinks (G.setOwn x p (.moved t)) ps ts
  | _, _ => G

/-- All arguments with a flag that is true for `in` (shared) arguments. -/
def argList (ss bs : List Place) (ms : List (Mu × Ty)) : List (Bool × Place) :=
  ss.map (fun p => (false, p)) ++ (bs.zip ms).map (fun a => (a.2.1 == .shr, a.1))

/-- [T-CallProj] alias-XOR-mutability: `∀ i ≠ j, p_i ⊥ p_j ∨ μ_i = μ_j = in`. -/
def Compat (a b : Bool × Place) : Prop := (a.1 = true ∧ b.1 = true) ∨ a.2.Disj b.2

def AllCompat : List (Bool × Place) → Prop
  | [] => True
  | a :: r => (∀ b ∈ r, Compat a b) ∧ AllCompat r

mutual
/-- Flow-sensitive statement typing `G ⊢ s ⊣ G'` over the PCG. -/
inductive TyS (P : Prog) : Pcg → Stmt → Pcg → Prop where
  | assign {G G1 G2 : Pcg} {p : Place} {o : Opnd} {t : Ty} :
      TyO G o t G1 → TyAsg G1 p t G2 → TyS P G (.assign p o) G2
  | swap {G : Pcg} {p : Place} {x : Nat} {q : Path} {t : Ty} :
      G.Writable p → G.read p = some t → G.read ⟨.own x, q⟩ = some t →
      p.Disj ⟨.own x, q⟩ → TyS P G (.swap p ⟨.own x, q⟩) G
  | consume {G G1 : Pcg} {o : Opnd} : TyO G o .hdl G1 → TyS P G (.consume o) G1
  | drop {G G1 : Pcg} {o : Opnd} {t : Ty} : TyO G o t G1 → Mode.Le t.kind .affine →
      TyS P G (.drop o) G1
  /-- [T-Match]/PCG join: both arms start from the same PCG and must end in
  the same PCG. -/
  | ifz {G G1 G2 : Pcg} {o : Opnd} {b1 b2 : List Stmt} :
      TyO G o .nat G1 → TyB P G1 b1 G2 → TyB P G1 b2 G2 → TyS P G (.ifz o b1 b2) G2
  /-- [T-CallProj]. -/
  | call {G : Pcg} {f : Nat} {F : Fn} {ss bs : List Place} :
      P[f]? = some F → SinksOK G ss F.sinks → BorsOK G bs F.bors →
      AllCompat (argList ss bs F.bors) →
      TyS P G (.call f ss bs) (moveSinks G ss F.sinks)
  /-- A fiber entry captures only owned values: the [T-Abs] premise "no
  shr/mut borrows in Γ". -/
  | spawn {G : Pcg} {f : Nat} {F : Fn} {ss : List Place} :
      P[f]? = some F → F.bors = [] → SinksOK G ss F.sinks → AllCompat (argList ss [] []) →
      TyS P G (.spawn f ss) (moveSinks G ss F.sinks)
  /-- A linear suspending effect: `res = linear`, so the Resumption Frame
  Condition admits every live binding, linear ones included. -/
  | yield {G : Pcg} : TyS P G .yield G

inductive TyB (P : Prog) : Pcg → List Stmt → Pcg → Prop where
  | nil (G : Pcg) : TyB P G [] G
  | cons {G G1 G2 : Pcg} {s : Stmt} {b : List Stmt} :
      TyS P G s G1 → TyB P G1 b G2 → TyB P G (s :: b) G2
end

def Fn.init (F : Fn) : Pcg := ⟨F.sinks.map .full ++ F.locals.map .moved, F.bors⟩

/-- A function body is typed from its initial PCG and ends with every owned
local discardable (linear ones consumed or moved away). -/
def FnOK (P : Prog) (F : Fn) : Prop := ∃ G, TyB P F.init F.body G ∧ Done G

def ProgOK (P : Prog) : Prop := ∀ F ∈ P, FnOK P F

/-! ## The machine: frames, fibers and suspended stacks -/

/-- A borrow slot at run time: a fiber-relative location (the position of
the owning frame in the fiber's stack, the owned local and the path). -/
structure Loc where
  mu : Mu
  pos : Nat
  x : Nat
  path : Path
  deriving DecidableEq, Repr

structure Frame where
  own : List Val
  slots : List Loc
  code : List Stmt

instance : Inhabited Frame := ⟨⟨[], [], []⟩⟩

/-- A fiber: its base address, its stack (bottom first; a frame's position
is its index) and whether it is suspended at a `yield`. -/
structure Fiber where
  base : Nat
  stack : List Frame
  susp : Bool

structure Store where
  live : Nat → Bool
  next : Nat

structure Cfg where
  fibers : List Fiber
  st : Store

def upd (f : Nat → Bool) (r : Nat) (b : Bool) : Nat → Bool := fun r' => if r' = r then b else f r'

def Frame.get (f : Frame) (x : Nat) : Val := f.own[x]?.getD .undef

def Frame.wr (f : Frame) (x : Nat) (p : Path) (w : Val) : Frame :=
  { f with own := f.own.set x ((f.get x).set p w) }

/-- The value of local `x` of the frame at position `pos`. -/
def stGet (st : List Frame) (pos x : Nat) : Val := (st[pos]?.getD default).get x

/-- Read a place of the top frame `fr` above the older frames `pre`. -/
def rd (pre : List Frame) (fr : Frame) : Place → Val
  | ⟨.own x, p⟩ => (fr.get x).at p
  | ⟨.bor b, p⟩ =>
    match fr.slots[b]? with
    | some l => (stGet pre l.pos l.x).at (l.path ++ p)
    | none => .undef

/-- Write a place of the top frame: an owned place changes `fr`, a place
under a slot changes the owning older frame. -/
def wr (pre : List Frame) (fr : Frame) : Place → Val → List Frame × Frame
  | ⟨.own x, p⟩, w => (pre, fr.wr x p w)
  | ⟨.bor b, p⟩, w =>
    match fr.slots[b]? with
    | some l => (pre.set l.pos ((pre[l.pos]?.getD default).wr l.x (l.path ++ p) w), fr)
    | none => (pre, fr)

def evalO (pre : List Frame) (fr : Frame) (S : Store) : Opnd → Val × Frame × Store
  | .lit n => (.nat n, fr, S)
  | .newBuf => (.buf, fr, S)
  | .mint => (.hdl S.next, fr, ⟨upd S.live S.next true, S.next + 1⟩)
  | .copy p => (rd pre fr p, fr, S)
  | .move ⟨.own x, p⟩ => ((fr.get x).at p, fr.wr x p .undef, S)
  | .move ⟨.bor _, _⟩ => (.undef, fr, S)

/-- Move the `sink` arguments out of the caller frame. -/
def clearSinks (fr : Frame) : List Place → Frame
  | ⟨.own x, p⟩ :: ps => clearSinks (fr.wr x p .undef) ps
  | _ => fr

/-- A borrow argument becomes a callee slot: an owned place of the caller at
position `top`, or a reborrow of a caller slot, flattened so every slot names
an owned local directly. -/
def resolve (top : Nat) (fr : Frame) (a : Place × (Mu × Ty)) : Loc :=
  match a.1.root with
  | .own x => ⟨a.2.1, top, x, a.1.path⟩
  | .bor b =>
    match fr.slots[b]? with
    | some l => ⟨a.2.1, l.pos, l.x, l.path ++ a.1.path⟩
    | none => ⟨a.2.1, 0, 0, []⟩

/-- The callee frame of `call f ss bs` (or the root frame of a spawned fiber). -/
def calleeFrame (pre : List Frame) (fr : Frame) (F : Fn) (ss bs : List Place) : Frame :=
  ⟨ss.map (rd pre fr) ++ F.locals.map (fun _ => .undef),
   (bs.zip F.bors).map (resolve pre.length fr), F.body⟩

/-- One step of a running fiber. The last component lists spawned fibers. -/
inductive FStep (P : Prog) : Fiber → Store → Fiber → Store → List Fiber → Prop where
  | assign {f : Fiber} {pre : List Frame} {fr : Frame} {p : Place} {o : Opnd}
      {rest : List Stmt} {S S1 : Store} {v : Val} {fr1 : Frame} :
      f.susp = false → f.stack = pre ++ [fr] → fr.code = .assign p o :: rest →
      evalO pre fr S o = (v, fr1, S1) →
      FStep P f S { f with stack := (wr pre { fr1 with code := rest } p v).1 ++
        [(wr pre { fr1 with code := rest } p v).2] } S1 []
  | swap {f : Fiber} {pre : List Frame} {fr : Frame} {p q : Place} {rest : List Stmt}
      {S : Store} :
      f.susp = false → f.stack = pre ++ [fr] → fr.code = .swap p q :: rest →
      FStep P f S { f with stack :=
        let r1 := wr pre { fr with code := rest } p (rd pre fr q)
        let r2 := wr r1.1 r1.2 q (rd pre fr p)
        r2.1 ++ [r2.2] } S []
  | consume {f : Fiber} {pre : List Frame} {fr fr1 : Frame} {o : Opnd} {rest : List Stmt}
      {S S1 : Store} {r : Nat} :
      f.susp = false → f.stack = pre ++ [fr] → fr.code = .consume o :: rest →
      evalO pre fr S o = (.hdl r, fr1, S1) → S1.live r = true →
      FStep P f S { f with stack := pre ++ [{ fr1 with code := rest }] }
        ⟨upd S1.live r false, S1.next⟩ []
  | drop {f : Fiber} {pre : List Frame} {fr fr1 : Frame} {o : Opnd} {rest : List Stmt}
      {S S1 : Store} {v : Val} :
      f.susp = false → f.stack = pre ++ [fr] → fr.code = .drop o :: rest →
      evalO pre fr S o = (v, fr1, S1) →
      FStep P f S { f with stack := pre ++ [{ fr1 with code := rest }] } S1 []
  | ifz {f : Fiber} {pre : List Frame} {fr fr1 : Frame} {o : Opnd} {b1 b2 rest : List Stmt}
      {S S1 : Store} {n : Nat} :
      f.susp = false → f.stack = pre ++ [fr] → fr.code = .ifz o b1 b2 :: rest →
      evalO pre fr S o = (.nat n, fr1, S1) →
      FStep P f S { f with stack :=
        pre ++ [{ fr1 with code := (if n = 0 then b1 else b2) ++ rest }] } S1 []
  | call {f : Fiber} {pre : List Frame} {fr : Frame} {g : Nat} {F : Fn}
      {ss bs : List Place} {rest : List Stmt} {S : Store} :
      f.susp = false → f.stack = pre ++ [fr] → fr.code = .call g ss bs :: rest →
      P[g]? = some F →
      FStep P f S { f with stack :=
        pre ++ [{ clearSinks fr ss with code := rest }, calleeFrame pre fr F ss bs] } S []
  | ret {f : Fiber} {pre : List Frame} {fr : Frame} {S : Store} :
      f.susp = false → f.stack = pre ++ [fr] → fr.code = [] →
      FStep P f S { f with stack := pre } S []
  | spawn {f : Fiber} {pre : List Frame} {fr : Frame} {g : Nat} {F : Fn}
      {ss : List Place} {rest : List Stmt} {S : Store} (b : Nat) :
      f.susp = false → f.stack = pre ++ [fr] → fr.code = .spawn g ss :: rest →
      P[g]? = some F →
      FStep P f S { f with stack := pre ++ [{ clearSinks fr ss with code := rest }] } S
        [⟨b, [calleeFrame pre fr F ss []], false⟩]
  | yield {f : Fiber} {pre : List Frame} {fr : Frame} {rest : List Stmt} {S : Store} :
      f.susp = false → f.stack = pre ++ [fr] → fr.code = .yield :: rest →
      FStep P f S { f with stack := pre ++ [{ fr with code := rest }], susp := true } S []

/-- Relocate a fiber to a new base address by copying it: no local and no
slot is rewritten. -/
def Fiber.reloc (f : Fiber) (b : Nat) : Fiber := { f with base := b }

/-- Machine steps: any running fiber steps; a suspended fiber is resumed,
possibly relocated to a new base (fiber pooling). -/
inductive Step (P : Prog) : Cfg → Cfg → Prop where
  | run {c : Cfg} {i : Nat} {f f' : Fiber} {S' : Store} {new : List Fiber} :
      c.fibers[i]? = some f → FStep P f c.st f' S' new →
      Step P c ⟨c.fibers.set i f' ++ new, S'⟩
  | resume {c : Cfg} {i : Nat} {f : Fiber} (b : Nat) :
      c.fibers[i]? = some f → f.susp = true →
      Step P c ⟨c.fibers.set i { f.reloc b with susp := false }, c.st⟩

inductive Steps (P : Prog) : Cfg → Cfg → Prop where
  | refl (c : Cfg) : Steps P c c
  | step {c c' c'' : Cfg} : Step P c c' → Steps P c' c'' → Steps P c c''

/-! ## Invariants -/

/-- Each owned local's value agrees with its capability tree. -/
def VSL : List Val → List Sh → Prop
  | [], [] => True
  | v :: vs, s :: ss => VS v s ∧ VSL vs ss
  | _, _ => False

/-- A slot points into a strictly older frame (`pos < i`) of the same stack,
at an initialized place of the slot's type. (`VT` of an absent frame or
local fails, so the target exists: no borrow outlives its place.) -/
def SlotOK (st : List Frame) (i : Nat) (l : Loc) (mt : Mu × Ty) : Prop :=
  l.mu = mt.1 ∧ l.pos < i ∧ VT ((stGet st l.pos l.x).at l.path) mt.2

def SlotsOK (st : List Frame) (i : Nat) : List Loc → List (Mu × Ty) → Prop
  | [], [] => True
  | l :: ls, m :: ms => SlotOK st i l m ∧ SlotsOK st i ls ms
  | _, _ => False

/-- The frame at position `i` of `st`: its values agree with its PCG, its
slots are sound, and its remaining code is typed from its PCG to a
discardable exit state. -/
structure FrameOK (P : Prog) (st : List Frame) (i : Nat) (fr : Frame) (G : Pcg) : Prop where
  vals : VSL fr.own G.own
  slots : SlotsOK st i fr.slots G.slots
  code : ∃ G', TyB P G fr.code G' ∧ Done G'

def slotAt (st : List Frame) (i a : Nat) : Option Loc := (st[i]?).bind (fun f => f.slots[a]?)

/-- Two slots name overlapping places. -/
def LOvl (l m : Loc) : Prop := l.pos = m.pos ∧ l.x = m.x ∧ Ovl l.path m.path

/-- Borrow soundness (alias-XOR-mutability on the whole stack): two distinct
slots that overlap, one of them `mut`, lie in different frames, the older
one is `mut` and contains the younger one (a reborrow chain). In particular
two `mut` slots of one frame never overlap and a `shr` slot never overlaps a
live `mut` borrow except the one it was reborrowed from. -/
def BorrowSound (st : List Frame) : Prop :=
  ∀ i a j b la lb, slotAt st i a = some la → slotAt st j b = some lb → (i ≠ j ∨ a ≠ b) →
    LOvl la lb → (la.mu = .mut ∨ lb.mu = .mut) →
    (i < j ∧ la.mu = .mut ∧ Pre la.path lb.path) ∨ (j < i ∧ lb.mu = .mut ∧ Pre lb.path la.path)

def PreOK (P : Prog) (st : List Frame) : Prop :=
  ∀ i fr, st[i]? = some fr → ∃ G, FrameOK P st i fr G

structure StackOK (P : Prog) (st : List Frame) : Prop where
  frames : PreOK P st
  sound : BorrowSound st

def cntVals (r : Nat) : List Val → Nat
  | [] => 0
  | v :: vs => v.cnt r + cntVals r vs

def cntFrames (r : Nat) : List Frame → Nat
  | [] => 0
  | f :: fs => cntVals r f.own + cntFrames r fs

def cntFibers (r : Nat) : List Fiber → Nat
  | [] => 0
  | f :: fs => cntFrames r f.stack + cntFibers r fs

/-- Every handle held anywhere is below `n`. -/
def FreshSt (n : Nat) (st : List Frame) : Prop := ∀ fr ∈ st, ∀ v ∈ fr.own, v.allLt n

def liveN (S : Store) (r : Nat) : Nat := if S.live r then 1 else 0

/-- The configuration invariant: a well-typed program, every fiber's stack
typed and borrow-sound, and the store agreeing exactly with the handles held
in owned locals (each live handle held exactly once, no dead one held). -/
structure CfgOK (P : Prog) (c : Cfg) : Prop where
  prog : ProgOK P
  stacks : ∀ f ∈ c.fibers, StackOK P f.stack
  count : ∀ r, cntFibers r c.fibers = liveN c.st r
  fresh : ∀ r, c.st.live r = true → r < c.st.next
  freshv : ∀ f ∈ c.fibers, FreshSt c.st.next f.stack

/-! ## List and frame lemmas -/

theorem vsl_len : ∀ {vs : List Val} {ss : List Sh}, VSL vs ss → vs.length = ss.length
  | [], [], _ => rfl
  | _ :: _, _ :: _, h => by simp [vsl_len h.2]
  | [], _ :: _, h => h.elim
  | _ :: _, [], h => h.elim

theorem vsl_get : ∀ {vs : List Val} {ss : List Sh} {x : Nat} {s : Sh}, VSL vs ss →
    ss[x]? = some s → ∃ v, vs[x]? = some v ∧ VS v s
  | [], [], _, _, _, e => by simp at e
  | v :: vs, s' :: ss, 0, s, h, e => by simp at e; subst e; exact ⟨v, rfl, h.1⟩
  | _ :: vs, _ :: ss, x + 1, s, h, e => by simpa using vsl_get h.2 (by simpa using e)
  | [], _ :: _, _, _, h, _ => h.elim
  | _ :: _, [], _, _, h, _ => h.elim

theorem vsl_set : ∀ {vs : List Val} {ss : List Sh} (x : Nat) {w : Val} {n : Sh}, VSL vs ss →
    VS w n → VSL (vs.set x w) (ss.set x n)
  | [], [], _, _, _, _, _ => trivial
  | _ :: _, _ :: _, 0, _, _, h, hw => ⟨hw, h.2⟩
  | _ :: _, _ :: _, x + 1, _, _, h, hw => ⟨h.1, vsl_set x h.2 hw⟩
  | [], _ :: _, _, _, _, h, _ => h.elim
  | _ :: _, [], _, _, _, h, _ => h.elim

theorem vsl_set_same : ∀ {vs : List Val} {ss : List Sh} (x : Nat) {w : Val} {s : Sh}, VSL vs ss →
    ss[x]? = some s → VS w s → VSL (vs.set x w) ss
  | [], [], _, _, _, _, _, _ => trivial
  | _ :: _, _ :: _, 0, _, _, h, e, hw => by simp at e; subst e; exact ⟨hw, h.2⟩
  | _ :: _, _ :: _, x + 1, _, _, h, e, hw => ⟨h.1, vsl_set_same x h.2 (by simpa using e) hw⟩
  | [], _ :: _, _, _, _, h, _, _ => h.elim
  | _ :: _, [], _, _, _, h, _, _ => h.elim

theorem frame_get_of {fr : Frame} {x : Nat} {v : Val} (h : fr.own[x]? = some v) : fr.get x = v := by
  simp [Frame.get, h]

theorem slotsOK_len : ∀ {st : List Frame} {i : Nat} {ls : List Loc} {ms : List (Mu × Ty)},
    SlotsOK st i ls ms → ls.length = ms.length
  | _, _, [], [], _ => rfl
  | _, _, _ :: _, _ :: _, h => by simp [slotsOK_len h.2]
  | _, _, [], _ :: _, h => h.elim
  | _, _, _ :: _, [], h => h.elim

theorem slotsOK_get : ∀ {st : List Frame} {i : Nat} {ls : List Loc} {ms : List (Mu × Ty)} {b : Nat}
    {m : Mu × Ty}, SlotsOK st i ls ms → ms[b]? = some m → ∃ l, ls[b]? = some l ∧ SlotOK st i l m
  | _, _, [], [], _, _, _, e => by simp at e
  | _, _, l :: _, _ :: _, 0, _, h, e => by simp at e; subst e; exact ⟨l, rfl, h.1⟩
  | _, _, _ :: _, _ :: _, b + 1, _, h, e => by simpa using slotsOK_get h.2 (by simpa using e)
  | _, _, [], _ :: _, _, _, h, _ => h.elim
  | _, _, _ :: _, [], _, _, h, _ => h.elim

theorem slotsOK_get' : ∀ {st : List Frame} {i : Nat} {ls : List Loc} {ms : List (Mu × Ty)} {b : Nat}
    {l : Loc}, SlotsOK st i ls ms → ls[b]? = some l → ∃ m, ms[b]? = some m ∧ SlotOK st i l m
  | _, _, [], [], _, _, _, e => by simp at e
  | _, _, _ :: _, m :: _, 0, _, h, e => by simp at e; subst e; exact ⟨m, rfl, h.1⟩
  | _, _, _ :: _, _ :: _, b + 1, _, h, e => by simpa using slotsOK_get' h.2 (by simpa using e)
  | _, _, [], _ :: _, _, _, h, _ => h.elim
  | _, _, _ :: _, [], _, _, h, _ => h.elim

theorem slotsOK_mono : ∀ {st st' : List Frame} {i : Nat} {ls : List Loc} {ms : List (Mu × Ty)},
    (∀ l m, SlotOK st i l m → SlotOK st' i l m) → SlotsOK st i ls ms → SlotsOK st' i ls ms
  | _, _, _, [], [], _, _ => trivial
  | _, _, _, _ :: _, _ :: _, f, h => ⟨f _ _ h.1, slotsOK_mono f h.2⟩
  | _, _, _, [], _ :: _, _, h => h.elim
  | _, _, _, _ :: _, [], _, h => h.elim

theorem stGet_congr {st st' : List Frame} {pos x : Nat} (h : st[pos]? = st'[pos]?) :
    stGet st pos x = stGet st' pos x := by
  simp [stGet, h]

/-- Slots only look below their own frame. -/
theorem slotOK_congr {st st' : List Frame} {i : Nat} {l : Loc} {m : Mu × Ty}
    (h : ∀ p, p < i → st[p]? = st'[p]?) (hs : SlotOK st i l m) : SlotOK st' i l m := by
  obtain ⟨h1, h2, h3⟩ := hs
  exact ⟨h1, h2, by rwa [← stGet_congr (h _ h2)]⟩

theorem frameOK_congr {P : Prog} {st st' : List Frame} {i : Nat} {fr : Frame} {G : Pcg}
    (h : ∀ p, p < i → st[p]? = st'[p]?) (hf : FrameOK P st i fr G) : FrameOK P st' i fr G :=
  ⟨hf.vals, slotsOK_mono (fun _ _ => slotOK_congr h) hf.slots, hf.code⟩

theorem getElem_opt_append_lt {α : Type} {l : List α} {x : α} {p : Nat} (hp : p < l.length) :
    (l ++ [x])[p]? = l[p]? := List.getElem?_append_left hp

theorem getElem_opt_append_len {α : Type} (l : List α) (x : α) : (l ++ [x])[l.length]? = some x := by
  simp

/-- `StackOK` of a non-empty stack, split into its older frames and its top. -/
theorem preOK_snoc {P : Prog} {pre : List Frame} {fr : Frame} :
    PreOK P (pre ++ [fr]) ↔ PreOK P pre ∧ ∃ G, FrameOK P pre pre.length fr G := by
  constructor
  · intro h
    refine ⟨fun i f e => ?_, ?_⟩
    · have hi : i < pre.length := by
        rcases Nat.lt_or_ge i pre.length with hi | hi
        · exact hi
        · simp [List.getElem?_eq_none hi] at e
      obtain ⟨G, hG⟩ := h i f (by rw [getElem_opt_append_lt hi]; exact e)
      exact ⟨G, frameOK_congr (fun p hp => getElem_opt_append_lt (Nat.lt_trans hp hi)) hG⟩
    · obtain ⟨G, hG⟩ := h pre.length fr (getElem_opt_append_len pre fr)
      exact ⟨G, frameOK_congr (fun p hp => getElem_opt_append_lt hp) hG⟩
  · rintro ⟨h1, G, hG⟩ i f e
    rcases Nat.lt_or_ge i pre.length with hi | hi
    · rw [getElem_opt_append_lt hi] at e
      obtain ⟨G', hG'⟩ := h1 i f e
      exact ⟨G', frameOK_congr (fun p hp => (getElem_opt_append_lt (Nat.lt_trans hp hi)).symm) hG'⟩
    · rw [List.getElem?_append_right hi] at e
      have : i - pre.length = 0 := by
        cases hh : i - pre.length with
        | zero => rfl
        | succ k => rw [hh] at e; simp at e
      rw [this] at e; simp at e; subst e
      have hi' : i = pre.length := by omega
      subst hi'
      exact ⟨G, frameOK_congr (fun p hp => (getElem_opt_append_lt hp).symm) hG⟩

theorem bs_congr {st st' : List Frame}
    (h : ∀ i a, slotAt st' i a = slotAt st i a) (hb : BorrowSound st) : BorrowSound st' := by
  intro i a j b la lb ha hb' hne ho hm
  exact hb i a j b la lb (by rw [← h]; exact ha) (by rw [← h]; exact hb') hne ho hm

/-! ## Paths -/

theorem pre_refl : ∀ (p : Path), Pre p p
  | [] => trivial
  | _ :: p => ⟨rfl, pre_refl p⟩

theorem pre_append_right : ∀ (p r : Path), Pre p (p ++ r)
  | [], _ => trivial
  | _ :: p, r => ⟨rfl, pre_append_right p r⟩

theorem pre_exists : ∀ {p q : Path}, Pre p q → ∃ r, q = p ++ r
  | [], q, _ => ⟨q, rfl⟩
  | _ :: _, [], h => h.elim
  | a :: p, b :: q, h => by
    obtain ⟨e, h⟩ := h
    obtain ⟨r, hr⟩ := pre_exists h
    exact ⟨r, by simp [e, hr]⟩

theorem pre_trans : ∀ {a b c : Path}, Pre a b → Pre b c → Pre a c
  | [], _, _, _, _ => trivial
  | _ :: _, [], _, h, _ => h.elim
  | _ :: _, _ :: _, [], _, h => h.elim
  | _ :: _, _ :: _, _ :: _, ⟨e1, h1⟩, ⟨e2, h2⟩ => ⟨e1.trans e2, pre_trans h1 h2⟩

/-- Two prefixes of one path are comparable. -/
theorem pre_total : ∀ {a b c : Path}, Pre a c → Pre b c → Ovl a b
  | [], _, _, _, _ => Or.inl trivial
  | _ :: _, [], _, _, _ => Or.inr trivial
  | _ :: _, _ :: _, [], h, _ => h.elim
  | x :: a, y :: b, z :: c, ⟨e1, h1⟩, ⟨e2, h2⟩ => by
    subst e1; subst e2
    rcases pre_total h1 h2 with h | h
    · exact Or.inl ⟨rfl, h⟩
    · exact Or.inr ⟨rfl, h⟩

theorem ovl_append {a b : Path} (c : Path) (h : Ovl a (b ++ c)) : Ovl a b := by
  rcases h with h | h
  · exact pre_total h (pre_append_right b c)
  · exact Or.inr (pre_trans (pre_append_right b c) h)

theorem ovl_symm {a b : Path} : Ovl a b → Ovl b a := Or.symm

/-- Disjoint paths stay disjoint when both are extended under a common prefix. -/
theorem not_ovl_prefix : ∀ (r : Path) {p q : Path}, ¬ Ovl p q → ¬ Ovl (r ++ p) (r ++ q)
  | [], _, _, h => by simpa using h
  | x :: r, p, q, h => by
    intro o; apply not_ovl_prefix r h
    rcases o with o | o
    · exact Or.inl o.2
    · exact Or.inr o.2

/-- A write inside a slot's region, or disjoint from it, keeps the slot typed. -/
theorem vt_after_write {v w : Val} {mp q : Path} {tm t : Ty} (hm : VT (v.at mp) tm)
    (hsafe : ¬ Ovl mp q ∨ Pre mp q) (hq : VT (v.at q) t) (hw : VT w t) :
    VT ((v.set q w).at mp) tm := by
  rcases hsafe with hd | hp
  · rw [at_set_disj _ _ (fun o => hd (ovl_symm o))]; exact hm
  · obtain ⟨r, rfl⟩ := pre_exists hp
    rw [at_set_inside _ _ _ _ (valid_of_vt_at hm)]
    rw [at_append] at hq
    exact vt_replace hm hq hw

/-! ## Writes into the stack -/

theorem frame_wr_slots (f : Frame) (x : Nat) (q : Path) (w : Val) : (f.wr x q w).slots = f.slots := rfl
theorem frame_wr_code (f : Frame) (x : Nat) (q : Path) (w : Val) : (f.wr x q w).code = f.code := rfl

theorem frame_wr_get (f : Frame) (x y : Nat) (q : Path) (w : Val) :
    (f.wr x q w).get y = if x = y ∧ x < f.own.length then (f.get x).set q w else f.get y := by
  simp only [Frame.wr, Frame.get, List.getElem?_set]
  by_cases h : x = y
  · subst h; by_cases hx : x < f.own.length
    · simp [hx]
    · simp [hx, List.getElem?_eq_none (Nat.le_of_not_lt hx)]
  · simp [h]

theorem get_lt {f : Frame} {x : Nat} {q : Path} {t : Ty} (h : VT ((f.get x).at q) t) :
    x < f.own.length := by
  rcases Nat.lt_or_ge x f.own.length with hx | hx
  · exact hx
  · simp [Frame.get, List.getElem?_eq_none hx, undef_at] at h; exact absurd h vt_not_undef

theorem stGet_lt {st : List Frame} {pos x : Nat} {q : Path} {t : Ty}
    (h : VT ((stGet st pos x).at q) t) : pos < st.length := by
  rcases Nat.lt_or_ge pos st.length with hx | hx
  · exact hx
  · simp [stGet, List.getElem?_eq_none hx, default, Frame.get, undef_at] at h
    exact absurd h vt_not_undef

theorem stGet_set_wr (st : List Frame) (pos x : Nat) (q : Path) (w : Val) (p y : Nat)
    (hpos : pos < st.length) (hx : x < (st[pos]?.getD default).own.length) :
    stGet (st.set pos ((st[pos]?.getD default).wr x q w)) p y =
      if pos = p ∧ x = y then (stGet st pos x).set q w else stGet st p y := by
  have hx' : x < st[pos].own.length := by simpa [List.getElem?_eq_getElem hpos] using hx
  simp only [stGet, List.getElem?_set]
  by_cases hp : pos = p
  · subst hp
    simp [hpos, frame_wr_get]
    by_cases hxy : x = y
    · subst hxy; simp [hx']
    · simp [hxy]
  · simp [hp]

theorem slotAt_set_wr (st : List Frame) (pos x : Nat) (q : Path) (w : Val) (i a : Nat) :
    slotAt (st.set pos ((st[pos]?.getD default).wr x q w)) i a = slotAt st i a := by
  simp only [slotAt, List.getElem?_set]
  by_cases hp : pos = i
  · subst hp
    by_cases hl : pos < st.length
    · simp [hl, frame_wr_slots, List.getElem?_eq_getElem hl]
    · simp [hl, List.getElem?_eq_none (Nat.le_of_not_lt hl)]
  · simp [hp]

theorem vsl_wr_vt {f : Frame} {Gs : List Sh} {x : Nat} {q : Path} {t : Ty} {w : Val}
    (h : VSL f.own Gs) (hq : VT ((f.get x).at q) t) (hw : VT w t) : VSL (f.wr x q w).own Gs := by
  have hx := get_lt hq
  have hlen := vsl_len h
  obtain ⟨s, hs⟩ : ∃ s, Gs[x]? = some s := ⟨Gs[x]'(by omega), by simp [List.getElem?_eq_getElem (by omega : x < Gs.length)]⟩
  obtain ⟨v, hv, hvs⟩ := vsl_get h hs
  have : f.get x = v := frame_get_of hv
  rw [this] at hq
  exact vsl_set_same x h hs (by rw [this]; exact vs_replace hvs hq hw)

theorem slotsOK_mono_idx {st st' : List Frame} {i : Nat} :
    ∀ {ls : List Loc} {ms : List (Mu × Ty)},
    (∀ (a : Nat) (l : Loc) (m : Mu × Ty), ls[a]? = some l → SlotOK st i l m → SlotOK st' i l m) →
    SlotsOK st i ls ms → SlotsOK st' i ls ms
  | [], [], _, _ => trivial
  | _ :: _, _ :: _, f, h => ⟨f 0 _ _ rfl h.1, slotsOK_mono_idx (fun a l m e => f (a + 1) l m e) h.2⟩
  | [], _ :: _, _, h => h.elim
  | _ :: _, [], _, h => h.elim

/-- A same-type write at an owned place `(pos, x, q)` of a stack keeps every
frame typed at its own PCG, provided every slot overlapping the written place
contains it. -/
theorem write_frame {P : Prog} {st : List Frame} {pos x : Nat} {q : Path} {t : Ty} {w : Val}
    (hold : VT ((stGet st pos x).at q) t) (hw : VT w t)
    (hsafe : ∀ i a m, slotAt st i a = some m → m.pos = pos → m.x = x → Ovl m.path q →
      Pre m.path q)
    {j : Nat} {f : Frame} {G : Pcg} (ej : st[j]? = some f) (hG : FrameOK P st j f G) :
    ∃ f', (st.set pos ((st[pos]?.getD default).wr x q w))[j]? = some f' ∧
      FrameOK P (st.set pos ((st[pos]?.getD default).wr x q w)) j f' G ∧
      f'.slots = f.slots ∧ f'.code = f.code := by
  have hpos := stGet_lt hold
  obtain ⟨g, hg⟩ : ∃ g, st[pos]? = some g := ⟨_, List.getElem?_eq_getElem hpos⟩
  have hd : st[pos]?.getD default = g := by simp [hg]
  have hold' : VT ((g.get x).at q) t := by simpa [stGet, hd] using hold
  have hx : x < (st[pos]?.getD default).own.length := by rw [hd]; exact get_lt hold'
  have slot_ok : ∀ a m mt, slotAt st j a = some m → SlotOK st j m mt →
      SlotOK (st.set pos ((st[pos]?.getD default).wr x q w)) j m mt := by
    intro a m mt hsa ⟨h1, h2, h3⟩
    refine ⟨h1, h2, ?_⟩
    rw [stGet_set_wr st pos x q w _ _ hpos hx]
    split
    · rename_i hh
      obtain ⟨e1, e2⟩ := hh
      subst e1; subst e2
      have : ¬ Ovl m.path q ∨ Pre m.path q := by
        by_cases o : Ovl m.path q
        · exact Or.inr (hsafe j a m hsa rfl rfl o)
        · exact Or.inl o
      exact vt_after_write h3 this hold hw
    · exact h3
  by_cases hj : pos = j
  · subst hj
    rw [hg] at ej; cases ej
    refine ⟨f.wr x q w, by rw [List.getElem?_set_self hpos, hd], ?_, rfl, rfl⟩
    refine ⟨vsl_wr_vt hG.vals hold' hw, ?_, hG.code⟩
    refine slotsOK_mono_idx (fun a l m e hs => slot_ok a l m ?_ hs) hG.slots
    simpa [slotAt, hg, frame_wr_slots] using e
  · refine ⟨f, by rw [List.getElem?_set_ne hj]; exact ej, ?_, rfl, rfl⟩
    refine ⟨hG.vals, ?_, hG.code⟩
    exact slotsOK_mono_idx (fun a l m e hs => slot_ok a l m (by simp [slotAt, ej, e]) hs) hG.slots

theorem write_stack {P : Prog} {st : List Frame} {pos x : Nat} {q : Path} {t : Ty} {w : Val}
    (h : PreOK P st) (hold : VT ((stGet st pos x).at q) t) (hw : VT w t)
    (hsafe : ∀ i a m, slotAt st i a = some m → m.pos = pos → m.x = x → Ovl m.path q →
      Pre m.path q) :
    PreOK P (st.set pos ((st[pos]?.getD default).wr x q w)) := by
  intro j fj ej
  have hj : j < st.length := by
    have := List.getElem?_eq_some_iff.mp ej |>.1; simpa using this
  obtain ⟨f, hf⟩ : ∃ f, st[j]? = some f := ⟨_, List.getElem?_eq_getElem hj⟩
  obtain ⟨G, hG⟩ := h j f hf
  obtain ⟨f', e', hf', _⟩ := write_frame hold hw hsafe hf hG
  rw [ej] at e'; cases e'
  exact ⟨G, hf'⟩

/-! ## The top frame -/

theorem slotAt_snoc_top {pre : List Frame} {fr fr' : Frame} (hs : fr'.slots = fr.slots) (i a : Nat) :
    slotAt (pre ++ [fr']) i a = slotAt (pre ++ [fr]) i a := by
  simp only [slotAt]
  rcases Nat.lt_or_ge i pre.length with hi | hi
  · rw [getElem_opt_append_lt hi, getElem_opt_append_lt hi]
  · rw [List.getElem?_append_right hi, List.getElem?_append_right hi]
    cases h : i - pre.length with
    | zero => simp [hs]
    | succ k => simp

theorem stack_top {P : Prog} {pre : List Frame} {fr fr' : Frame} {G : Pcg} (hpre : PreOK P pre)
    (hf : FrameOK P pre pre.length fr' G) (hb : BorrowSound (pre ++ [fr]))
    (hs : fr'.slots = fr.slots) : StackOK P (pre ++ [fr']) :=
  ⟨preOK_snoc.mpr ⟨hpre, G, hf⟩, bs_congr (slotAt_snoc_top hs) hb⟩

theorem stack_inv {P : Prog} {pre : List Frame} {fr : Frame} (h : StackOK P (pre ++ [fr])) :
    PreOK P pre ∧ (∃ G, FrameOK P pre pre.length fr G) ∧ BorrowSound (pre ++ [fr]) :=
  ⟨(preOK_snoc.mp h.frames).1, (preOK_snoc.mp h.frames).2, h.sound⟩

theorem read_own {fr : Frame} {G : Pcg} {x : Nat} {p : Path} {t : Ty} (hv : VSL fr.own G.own)
    (h : G.read ⟨.own x, p⟩ = some t) :
    VT ((fr.get x).at p) t ∧ ValidP (fr.get x) p ∧
      ∃ s s', G.own[x]? = some s ∧ s.at p = some s' ∧ s'.full? = some t := by
  simp only [Pcg.read] at h
  split at h
  · rename_i s hs
    split at h
    · rename_i s' hs'
      obtain ⟨v, hv1, hv2⟩ := vsl_get hv hs
      rw [frame_get_of hv1]
      exact ⟨vs_full (vs_at hv2 hs') h, vs_valid hv2 hs', s, s', hs, hs', h⟩
    · simp at h
  · simp at h

theorem read_bor {pre : List Frame} {fr : Frame} {G : Pcg} {b : Nat} {p : Path} {t : Ty}
    (hs : SlotsOK pre pre.length fr.slots G.slots) (h : G.read ⟨.bor b, p⟩ = some t) :
    ∃ l t0, fr.slots[b]? = some l ∧ G.slots[b]? = some (l.mu, t0) ∧ l.pos < pre.length ∧
      VT ((stGet pre l.pos l.x).at l.path) t0 ∧ t0.at p = some t := by
  simp only [Pcg.read] at h
  split at h
  · rename_i μ t0 hb
    obtain ⟨l, hl, h1, h2, h3⟩ := slotsOK_get hs hb
    exact ⟨l, t0, hl, by rw [hb, h1], h2, h3, h⟩
  · simp at h

theorem rd_ok {pre : List Frame} {fr : Frame} {G : Pcg} {p : Place} {t : Ty}
    (hv : VSL fr.own G.own) (hs : SlotsOK pre pre.length fr.slots G.slots)
    (h : G.read p = some t) : VT (rd pre fr p) t := by
  rcases p with ⟨_ | b, p⟩
  · exact (read_own hv h).1
  · obtain ⟨l, t0, hl, _, _, h3, h4⟩ := read_bor hs h
    simp only [rd, hl, at_append]
    exact vt_at h3 h4

theorem vsl_setOwn {fr : Frame} {G : Pcg} {x : Nat} {p : Path} {s s0 n : Sh} {w : Val}
    (hv : VSL fr.own G.own) (hs : G.own[x]? = some s) (hs0 : s.at p = some s0) (hw : VS w n) :
    VSL (fr.wr x p w).own (G.setOwn x p n).own := by
  obtain ⟨v, hv1, hv2⟩ := vsl_get hv hs
  simp only [Frame.wr, Pcg.setOwn, hs, frame_get_of hv1]
  exact vsl_set x hv (vs_set hv2 hs0 hw)

theorem setOwn_slots (G : Pcg) (x : Nat) (p : Path) (n : Sh) : (G.setOwn x p n).slots = G.slots := by
  simp only [Pcg.setOwn]; split <;> rfl

/-! ## Counting handles -/

theorem cntVals_set (r : Nat) : ∀ {vs : List Val} {x : Nat} (w : Val), (hx : x < vs.length) →
    cntVals r (vs.set x w) + (vs[x]'hx).cnt r = cntVals r vs + w.cnt r
  | [], _, _, hx => by simp at hx
  | v :: vs, 0, w, _ => by simp [cntVals]; omega
  | v :: vs, x + 1, w, hx => by
    have := cntVals_set r (vs := vs) (x := x) w (by simpa using hx)
    simp [cntVals]; omega

theorem cnt_frame_wr (r : Nat) {f : Frame} {x : Nat} {p : Path} (w : Val) (hx : x < f.own.length)
    (hp : ValidP (f.get x) p) :
    cntVals r (f.wr x p w).own + ((f.get x).at p).cnt r = cntVals r f.own + w.cnt r := by
  have e : f.get x = f.own[x]'hx := by simp [Frame.get, List.getElem?_eq_getElem hx]
  have h1 := cntVals_set r (vs := f.own) ((f.get x).set p w) hx
  have h2 := cnt_set r w hp
  rw [← e] at h1
  simp only [Frame.wr]
  omega

theorem cntFrames_set (r : Nat) : ∀ {fs : List Frame} {i : Nat} (g : Frame), (hi : i < fs.length) →
    cntFrames r (fs.set i g) + cntVals r (fs[i]'hi).own = cntFrames r fs + cntVals r g.own
  | [], _, _, hi => by simp at hi
  | f :: fs, 0, g, _ => by simp [cntFrames]; omega
  | f :: fs, i + 1, g, hi => by
    have := cntFrames_set r (fs := fs) (i := i) g (by simpa using hi)
    simp [cntFrames]; omega

theorem cntFrames_append (r : Nat) : ∀ (a b : List Frame),
    cntFrames r (a ++ b) = cntFrames r a + cntFrames r b
  | [], _ => by simp [cntFrames]
  | f :: a, b => by simp [cntFrames, cntFrames_append r a b]; omega

theorem cntFrames_snoc (r : Nat) (a : List Frame) (f : Frame) :
    cntFrames r (a ++ [f]) = cntFrames r a + cntVals r f.own := by
  simp [cntFrames_append, cntFrames]

theorem cntFibers_append (r : Nat) : ∀ (a b : List Fiber),
    cntFibers r (a ++ b) = cntFibers r a + cntFibers r b
  | [], _ => by simp [cntFibers]
  | f :: a, b => by simp [cntFibers, cntFibers_append r a b]; omega

theorem cntFibers_set (r : Nat) : ∀ {fs : List Fiber} {i : Nat} (g : Fiber), (hi : i < fs.length) →
    cntFibers r (fs.set i g) + cntFrames r (fs[i]'hi).stack = cntFibers r fs + cntFrames r g.stack
  | [], _, _, hi => by simp at hi
  | f :: fs, 0, g, _ => by simp [cntFibers]; omega
  | f :: fs, i + 1, g, hi => by
    have := cntFibers_set r (fs := fs) (i := i) g (by simpa using hi)
    simp [cntFibers]; omega

theorem cntVals_append (r : Nat) : ∀ (a b : List Val), cntVals r (a ++ b) = cntVals r a + cntVals r b
  | [], _ => by simp [cntVals]
  | v :: a, b => by simp [cntVals, cntVals_append r a b]; omega

theorem cntVals_undefs (r : Nat) : ∀ (ts : List Ty), cntVals r (ts.map (fun _ => Val.undef)) = 0
  | [] => rfl
  | _ :: ts => by simp [cntVals, Val.cnt, cntVals_undefs r ts]

theorem cntVals_drop (r : Nat) : ∀ {vs : List Val} {ss : List Sh}, VSL vs ss → (∀ s ∈ ss, s.dropOK) →
    cntVals r vs = 0
  | [], [], _, _ => rfl
  | _ :: _, _ :: _, h, hd => by
    simp [cntVals, cnt_vs_drop r h.1 (hd _ (by simp)),
      cntVals_drop r h.2 (fun s hs => hd s (by simp [hs]))]
  | [], _ :: _, h, _ => h.elim
  | _ :: _, [], h, _ => h.elim

/-! ## Operands -/

theorem allLt_droppable : ∀ {v : Val} {t : Ty} (n : Nat), VT v t → Mode.Le t.kind .affine →
    v.allLt n
  | .nat _, .nat, _, _, _ => trivial
  | .buf, .buf, _, _, _ => trivial
  | .hdl _, .hdl, _, _, hk => by simp [Ty.kind, Mode.Le] at hk
  | .pair _ _, .pair s t, n, ⟨h1, h2⟩, hk => by
    simp only [Ty.kind] at hk
    exact ⟨allLt_droppable n h1 (Mode.le_trans (Mode.le_join_left _ _) hk),
      allLt_droppable n h2 (Mode.le_trans (Mode.le_join_right _ _) hk)⟩

theorem get_allLt {f : Frame} {n x : Nat} (h : ∀ v ∈ f.own, v.allLt n) : (f.get x).allLt n := by
  simp only [Frame.get]
  cases e : f.own[x]? with
  | none => trivial
  | some v => exact h v (List.mem_of_getElem? e)

theorem wr_allLt {f : Frame} {n x : Nat} {p : Path} {w : Val} (h : ∀ v ∈ f.own, v.allLt n)
    (hw : w.allLt n) : ∀ v ∈ (f.wr x p w).own, v.allLt n := by
  intro v hv
  rcases List.mem_or_eq_of_mem_set hv with hv | hv
  · exact h v hv
  · subst hv; exact allLt_set p (get_allLt h) hw

/-- Typed operand evaluation: the value has the type, the frame agrees with
the new PCG, and handles are conserved (a `mint` adds one fresh live handle). -/
theorem evalO_ok {pre : List Frame} {fr fr1 : Frame} {G G1 : Pcg} {o : Opnd} {t : Ty}
    {S S1 : Store} {v : Val}
    (hv : VSL fr.own G.own) (hs : SlotsOK pre pre.length fr.slots G.slots)
    (ht : TyO G o t G1) (he : evalO pre fr S o = (v, fr1, S1)) (hS : S.live S.next = false) :
    VT v t ∧ VSL fr1.own G1.own ∧ fr1.slots = fr.slots ∧ fr1.code = fr.code ∧
      G1.slots = G.slots ∧
      (∀ r, cntVals r fr1.own + v.cnt r + liveN S r = cntVals r fr.own + liveN S1 r) ∧
      ((∀ w ∈ fr.own, w.allLt S.next) → (∀ w ∈ fr1.own, w.allLt S1.next) ∧ v.allLt S1.next) ∧
      S.next ≤ S1.next ∧ ((∀ r, S.live r = true → r < S.next) → ∀ r, S1.live r = true → r < S1.next) := by
  cases ht with
  | lit n =>
    simp [evalO] at he; obtain ⟨rfl, rfl, rfl⟩ := he
    refine ⟨trivial, hv, rfl, rfl, rfl, fun r => by simp [Val.cnt], fun h => ⟨h, trivial⟩,
      Nat.le_refl _, id⟩
  | newBuf =>
    simp [evalO] at he; obtain ⟨rfl, rfl, rfl⟩ := he
    refine ⟨trivial, hv, rfl, rfl, rfl, fun r => by simp [Val.cnt], fun h => ⟨h, trivial⟩,
      Nat.le_refl _, id⟩
  | mint =>
    simp [evalO] at he; obtain ⟨rfl, rfl, rfl⟩ := he
    refine ⟨trivial, hv, rfl, rfl, rfl, fun r => ?_, fun h => ⟨fun w hw => allLt_mono (h w hw)
      (Nat.le_succ _), by simp [Val.allLt]⟩, Nat.le_succ _, fun h r hr => ?_⟩
    · by_cases e : S.next = r
      · subst e; simp [Val.cnt, liveN, upd, hS]
      · simp [Val.cnt, liveN, upd, e, Ne.symm e]
    · show r < S.next + 1
      simp only [upd] at hr
      by_cases e : r = S.next
      · omega
      · simp [e] at hr; have := h r hr; omega
  | copy hr hk =>
    simp [evalO] at he; obtain ⟨rfl, rfl, rfl⟩ := he
    have hvt := rd_ok hv hs hr
    refine ⟨hvt, hv, rfl, rfl, rfl, fun r => ?_, fun h => ⟨h, ?_⟩, Nat.le_refl _, id⟩
    · rw [cnt_droppable r hvt (by rw [hk]; decide)]; simp
    · exact allLt_droppable _ hvt (by rw [hk]; decide)
  | @move x p t hr =>
    simp [evalO] at he; obtain ⟨rfl, rfl, rfl⟩ := he
    obtain ⟨hvt, hvp, s, s', hs1, hs2, _⟩ := read_own hv hr
    have hx := get_lt hvt
    refine ⟨hvt, vsl_setOwn hv hs1 hs2 (by simp), rfl, rfl, setOwn_slots _ _ _ _, fun r => ?_,
      fun h => ⟨wr_allLt h trivial, allLt_at _ (get_allLt h)⟩, Nat.le_refl _, id⟩
    have := cnt_frame_wr r Val.undef hx hvp
    simp [Val.cnt] at this; omega

/-! ## Writes by the running frame -/

theorem fresh_set {n pos : Nat} {st : List Frame} {g : Frame} (h : FreshSt n st)
    (hg : ∀ v ∈ g.own, v.allLt n) : FreshSt n (st.set pos g) := by
  intro f hf
  rcases List.mem_or_eq_of_mem_set hf with hf | hf
  · exact h f hf
  · subst hf; exact hg

theorem fresh_snoc {n : Nat} {pre : List Frame} {fr : Frame} :
    FreshSt n (pre ++ [fr]) ↔ FreshSt n pre ∧ ∀ v ∈ fr.own, v.allLt n := by
  constructor
  · intro h; exact ⟨fun f hf => h f (by simp [hf]), h fr (by simp)⟩
  · rintro ⟨h1, h2⟩ f hf
    simp at hf; rcases hf with hf | hf
    · exact h1 f hf
    · subst hf; exact h2

theorem slotAt_lt {st : List Frame} {i a : Nat} {m : Loc} (h : slotAt st i a = some m) :
    i < st.length := by
  simp only [slotAt] at h
  rcases Nat.lt_or_ge i st.length with hi | hi
  · exact hi
  · simp [List.getElem?_eq_none hi] at h

/-- A write through a `mut` slot of the running frame, at a place of the
slot's type: the stack stays typed and borrow-sound and handles are
conserved. -/
theorem slot_write_ok {P : Prog} {pre : List Frame} {fr : Frame} {G : Pcg} {b : Nat} {p : Path}
    {t : Ty} {w : Val} (hst : StackOK P (pre ++ [fr])) (hf : FrameOK P pre pre.length fr G)
    (hW : G.Writable ⟨.bor b, p⟩) (hr : G.read ⟨.bor b, p⟩ = some t) (hvt : VT w t) :
    (wr pre fr ⟨.bor b, p⟩ w).2 = fr ∧
      StackOK P ((wr pre fr ⟨.bor b, p⟩ w).1 ++ [fr]) ∧
      FrameOK P (wr pre fr ⟨.bor b, p⟩ w).1 pre.length fr G ∧
      (wr pre fr ⟨.bor b, p⟩ w).1.length = pre.length ∧
      (∀ r, cntFrames r ((wr pre fr ⟨.bor b, p⟩ w).1 ++ [fr]) + (rd pre fr ⟨.bor b, p⟩).cnt r =
        cntFrames r (pre ++ [fr]) + w.cnt r) ∧
      (∀ n, FreshSt n (pre ++ [fr]) → w.allLt n →
        FreshSt n ((wr pre fr ⟨.bor b, p⟩ w).1 ++ [fr])) := by
  obtain ⟨l, t0, hl, hg, hpos, hvt0, hat⟩ := read_bor hf.slots hr
  obtain ⟨t1, ht1⟩ := hW
  rw [hg] at ht1
  have hmut : l.mu = .mut := by simp at ht1; exact ht1.1
  let st := pre ++ [fr]
  let q := l.path ++ p
  have hsetst : st.set l.pos ((st[l.pos]?.getD default).wr l.x q w) =
      pre.set l.pos ((pre[l.pos]?.getD default).wr l.x q w) ++ [fr] := by
    simp only [st, getElem_opt_append_lt hpos]
    exact List.set_append_left _ _ hpos
  have hwr : wr pre fr ⟨.bor b, p⟩ w = (pre.set l.pos ((pre[l.pos]?.getD default).wr l.x q w), fr) := by
    simp [wr, hl, q]
  have hget : stGet st l.pos l.x = stGet pre l.pos l.x := stGet_congr (getElem_opt_append_lt hpos)
  have hold : VT ((stGet st l.pos l.x).at q) t := by
    rw [hget]; simp only [q, at_append]; exact vt_at hvt0 hat
  have hlslot : slotAt st pre.length b = some l := by simp [slotAt, st, hl]
  have hsafe : ∀ i a m, slotAt st i a = some m → m.pos = l.pos → m.x = l.x → Ovl m.path q →
      Pre m.path q := by
    intro i a m hm e1 e2 o
    by_cases hia : i = pre.length ∧ a = b
    · obtain ⟨rfl, rfl⟩ := hia
      rw [hlslot] at hm; cases hm; exact pre_append_right _ _
    · have hne : pre.length ≠ i ∨ b ≠ a := by
        by_cases h1 : i = pre.length
        · exact Or.inr (fun h2 => hia ⟨h1, h2.symm⟩)
        · exact Or.inl (Ne.symm h1)
      have hov : LOvl l m := ⟨e1.symm, e2.symm, ovl_symm (ovl_append p o)⟩
      have hil := slotAt_lt hm
      simp [st] at hil
      rcases hst.sound _ _ _ _ _ _ hlslot hm hne hov (Or.inl hmut) with ⟨h1, _⟩ | ⟨_, _, h3⟩
      · omega
      · exact pre_trans h3 (pre_append_right _ _)
  have hpre' := write_stack hst.frames hold hvt hsafe
  have hlt : pre.length < st.length := by simp [st]
  obtain ⟨f', e', hf', _, _⟩ := write_frame hold hvt hsafe (getElem_opt_append_len pre fr)
    (frameOK_congr (fun p hp => (getElem_opt_append_lt hp).symm) hf)
  have hlen : (pre.set l.pos ((pre[l.pos]?.getD default).wr l.x q w)).length = pre.length := by simp
  rw [hsetst] at e' hpre' hf'
  have e'' := getElem_opt_append_len (pre.set l.pos ((pre[l.pos]?.getD default).wr l.x q w)) fr
  rw [hlen, e'] at e''; cases e''
  rw [hwr]
  refine ⟨rfl, ⟨hpre', ?_⟩, ?_, hlen, ?_, ?_⟩
  · rw [← hsetst]; exact bs_congr (slotAt_set_wr _ _ _ _ _) hst.sound
  · exact frameOK_congr (fun p hp => by rw [getElem_opt_append_lt (by rw [hlen]; exact hp)]) hf'
  · intro r
    obtain ⟨g, hgp⟩ : ∃ g, pre[l.pos]? = some g := ⟨_, List.getElem?_eq_getElem hpos⟩
    have hd : pre[l.pos]?.getD default = g := by simp [hgp]
    have hold' : VT ((g.get l.x).at q) t := by
      have := hold; rw [hget] at this; simpa [stGet, hd] using this
    have h1 := cntFrames_set r (fs := pre) (i := l.pos) (g.wr l.x q w) hpos
    have h2 := cnt_frame_wr r w (get_lt hold') (valid_of_vt_at hold')
    have e3 : pre[l.pos]'hpos = g := by simp [List.getElem?_eq_getElem hpos] at hgp; exact hgp
    rw [e3] at h1
    have e4 : rd pre fr ⟨.bor b, p⟩ = (g.get l.x).at q := by
      simp [rd, hl, stGet, hd, q]
    rw [hd, cntFrames_snoc, cntFrames_snoc, e4]
    dsimp only
    omega
  · intro n hfr hwn
    have ⟨h1, h2⟩ := fresh_snoc.mp hfr
    refine fresh_snoc.mpr ⟨fresh_set h1 ?_, h2⟩
    cases e : pre[l.pos]? with
    | none => intro v hv; simp [default] at hv; simp [Frame.wr, Frame.get] at hv
    | some g =>
      simp only [Option.getD_some]
      exact wr_allLt (h1 g (List.mem_of_getElem? e)) hwn

/-- A same-type write at an owned place of the running frame. -/
theorem own_write_ok {P : Prog} {pre : List Frame} {fr : Frame} {G : Pcg} {x : Nat} {p : Path}
    {t : Ty} {w : Val} (hst : StackOK P (pre ++ [fr])) (hf : FrameOK P pre pre.length fr G)
    (hr : G.read ⟨.own x, p⟩ = some t) (hvt : VT w t) :
    StackOK P (pre ++ [fr.wr x p w]) ∧ FrameOK P pre pre.length (fr.wr x p w) G ∧
      (∀ r, cntVals r (fr.wr x p w).own + (rd pre fr ⟨.own x, p⟩).cnt r =
        cntVals r fr.own + w.cnt r) := by
  obtain ⟨hv, hvp, _⟩ := read_own hf.vals hr
  have hf' : FrameOK P pre pre.length (fr.wr x p w) G :=
    ⟨vsl_wr_vt hf.vals hv hvt, hf.slots, hf.code⟩
  refine ⟨stack_top (stack_inv hst).1 hf' hst.sound rfl, hf', fun r => ?_⟩
  · exact cnt_frame_wr r w (get_lt hv) hvp

/-! ## Preservation, statement by statement -/

/-- What a fiber step preserves: the new stack (and any spawned fiber) is
typed and borrow-sound, handles are conserved against the store, and the
fresh-id discipline holds. -/
structure Post (P : Prog) (S : Store) (st : List Frame) (S' : Store) (st' : List Frame)
    (new : List Fiber) : Prop where
  stack : StackOK P st'
  spawned : ∀ g ∈ new, StackOK P g.stack
  cnt : ∀ r, cntFrames r st' + cntFibers r new + liveN S r = cntFrames r st + liveN S' r
  fresh : FreshSt S.next st → (∀ r, S.live r = true → r < S.next) →
    FreshSt S'.next st' ∧ (∀ g ∈ new, FreshSt S'.next g.stack) ∧
      ∀ r, S'.live r = true → r < S'.next
  mono : S.next ≤ S'.next

theorem top_inv {P : Prog} {pre : List Frame} {fr : Frame} {s : Stmt} {rest : List Stmt}
    (hst : StackOK P (pre ++ [fr])) (hc : fr.code = s :: rest) :
    ∃ G G1 G', FrameOK P pre pre.length fr G ∧ TyS P G s G1 ∧ TyB P G1 rest G' ∧ Done G' := by
  obtain ⟨_, ⟨G, hf⟩, _⟩ := stack_inv hst
  obtain ⟨G', hb, hd⟩ := hf.code
  rw [hc] at hb
  cases hb with
  | cons h1 h2 => exact ⟨G, _, G', hf, h1, h2, hd⟩

theorem read_bor_of {G : Pcg} {b : Nat} {p : Path} {μ : Mu} {t0 t : Ty}
    (h1 : G.slots[b]? = some (μ, t0)) (h2 : t0.at p = some t) : G.read ⟨.bor b, p⟩ = some t := by
  simp [Pcg.read, h1, h2]

theorem tyAsg_inv {G G' : Pcg} {p : Place} {t : Ty} (h : TyAsg G p t G') :
    (∃ x path s s0, p = ⟨.own x, path⟩ ∧ G.own[x]? = some s ∧ s.at path = some s0 ∧
      s0.ty = t ∧ s0.dropOK ∧ G' = G.setOwn x path (.full t)) ∨
    (∃ b path t0, p = ⟨.bor b, path⟩ ∧ G.slots[b]? = some (.mut, t0) ∧ t0.at path = some t ∧
      Mode.Le t.kind .affine ∧ G' = G) := by
  cases h with
  | own h1 h2 h3 h4 => exact Or.inl ⟨_, _, _, _, rfl, h1, h2, h3, h4, rfl⟩
  | bor h1 h2 h3 => exact Or.inr ⟨_, _, _, rfl, h1, h2, h3, rfl⟩

theorem assign_ok {P : Prog} {pre : List Frame} {fr fr1 : Frame} {p : Place} {o : Opnd}
    {rest : List Stmt} {S S1 : Store} {v : Val}
    (hst : StackOK P (pre ++ [fr])) (hc : fr.code = .assign p o :: rest)
    (he : evalO pre fr S o = (v, fr1, S1)) (hS : S.live S.next = false) :
    Post P S (pre ++ [fr]) S1 ((wr pre { fr1 with code := rest } p v).1 ++
      [(wr pre { fr1 with code := rest } p v).2]) [] := by
  obtain ⟨G, G2, G', hf, hs, hrest, hdone⟩ := top_inv hst hc
  cases hs with
  | @assign _ Ga _ _ _ t ho ha =>
  obtain ⟨hvt, hv1, hsl1, _, hgs, hcnt, hfr, hmono, hlive⟩ := evalO_ok hf.vals hf.slots ho he hS
  have hsl : SlotsOK pre pre.length ({ fr1 with code := rest } : Frame).slots Ga.slots := by
    simp only [hsl1, hgs]; exact hf.slots
  have hb := (stack_inv hst).2.2
  have hpre := (stack_inv hst).1
  rcases tyAsg_inv ha with ⟨x, path, s, s0, rfl, hs1, hs2, hty, hdrop, rfl⟩ |
    ⟨b, path, t0, rfl, hs1, hs2, hk, hG2⟩
  · simp only [wr]
    have hf' : FrameOK P pre pre.length ({ fr1 with code := rest }.wr x path v)
        (Ga.setOwn x path (.full t)) :=
      ⟨vsl_setOwn hv1 hs1 hs2 (by simpa using hvt), by rw [setOwn_slots]; exact hsl,
        ⟨G', hrest, hdone⟩⟩
    obtain ⟨v0, hv0, hvs0⟩ := vsl_get hv1 hs1
    have hx : x < fr1.own.length := (List.getElem?_eq_some_iff.mp hv0).1
    have hold := vs_at hvs0 hs2
    have hvalid := vs_valid hvs0 hs2
    have hget : fr1.get x = v0 := frame_get_of hv0
    refine ⟨stack_top hpre hf' hb (by rw [frame_wr_slots]; exact hsl1), by simp, fun r => ?_,
      fun h1 h2 => ?_, hmono⟩
    · have := cnt_frame_wr r (f := { fr1 with code := rest }) v (x := x) (p := path) hx
        (by have : ({ fr1 with code := rest } : Frame).get x = v0 := hget
            rw [this]; exact hvalid)
      have h0 : ((({ fr1 with code := rest } : Frame).get x).at path).cnt r = 0 := by
        have : ({ fr1 with code := rest } : Frame).get x = v0 := hget
        rw [this]; exact cnt_vs_drop r hold hdrop
      have hc' := hcnt r
      simp only [wr, cntFrames_snoc, cntFibers] at this ⊢
      have h3 : cntVals r ({ fr1 with code := rest } : Frame).own = cntVals r fr1.own := rfl
      omega
    · obtain ⟨hpre_f, htop_f⟩ := fresh_snoc.mp h1
      obtain ⟨h3, h4⟩ := hfr htop_f
      refine ⟨fresh_snoc.mpr ⟨fun f hf v hv => allLt_mono (hpre_f f hf v hv) hmono, ?_⟩,
        by simp, hlive h2⟩
      exact wr_allLt (f := { fr1 with code := rest }) h3 h4
  · rw [hG2] at hrest
    have hf1 : FrameOK P pre pre.length { fr1 with code := rest } Ga :=
      ⟨hv1, hsl, ⟨G', hrest, hdone⟩⟩
    have hst1 := stack_top hpre hf1 hb (by simp [hsl1])
    obtain ⟨e2, hst2, _, _, hcnt2, hfr2⟩ :=
      slot_write_ok hst1 hf1 ⟨t0, hs1⟩ (read_bor_of hs1 hs2) hvt
    rw [e2]
    refine ⟨hst2, by simp, fun r => ?_, fun h1 h2 => ?_, hmono⟩
    · have h0 : (rd pre { fr1 with code := rest } ⟨.bor b, path⟩).cnt r = 0 :=
        cnt_droppable r (rd_ok hf1.vals hf1.slots (read_bor_of hs1 hs2)) hk
      have := hcnt2 r
      have hc' := hcnt r
      simp only [cntFrames_snoc, cntFibers] at this ⊢
      have h3 : cntVals r ({ fr1 with code := rest } : Frame).own = cntVals r fr1.own := rfl
      omega
    · obtain ⟨hpre_f, htop_f⟩ := fresh_snoc.mp h1
      obtain ⟨h3, h4⟩ := hfr htop_f
      refine ⟨hfr2 _ (fresh_snoc.mpr ⟨fun f hf v hv => allLt_mono (hpre_f f hf v hv) hmono, h3⟩) h4,
        by simp, hlive h2⟩

theorem stGet_allLt {n : Nat} {st : List Frame} (h : FreshSt n st) (pos x : Nat) :
    (stGet st pos x).allLt n := by
  simp only [stGet]
  cases e : st[pos]? with
  | none => simp [default, Frame.get]; trivial
  | some f => exact get_allLt (h f (List.mem_of_getElem? e))

theorem rd_allLt {n : Nat} {pre : List Frame} {fr : Frame} (h : FreshSt n (pre ++ [fr]))
    (p : Place) : (rd pre fr p).allLt n := by
  obtain ⟨h1, h2⟩ := fresh_snoc.mp h
  rcases p with ⟨_ | b, p⟩
  · exact allLt_at _ (get_allLt h2)
  · simp only [rd]
    split
    · exact allLt_at _ (stGet_allLt h1 _ _)
    · trivial

theorem rd_own_wr_disj {pre pre' : List Frame} {fr : Frame} {y x : Nat} {path q : Path} {w : Val}
    (hd : Place.Disj ⟨.own y, path⟩ ⟨.own x, q⟩) :
    rd pre' (fr.wr y path w) ⟨.own x, q⟩ = rd pre fr ⟨.own x, q⟩ := by
  simp only [rd, frame_wr_get]
  split
  · rename_i h
    obtain ⟨rfl, _⟩ := h
    rcases hd with hd | hd
    · exact absurd rfl hd
    · exact at_set_disj _ _ hd
  · rfl

theorem wr_own (pre : List Frame) (fr : Frame) (x : Nat) (p : Path) (w : Val) :
    wr pre fr ⟨.own x, p⟩ w = (pre, fr.wr x p w) := rfl

theorem swap_ok {P : Prog} {pre : List Frame} {fr : Frame} {p q : Place} {rest : List Stmt}
    {S : Store} (hst : StackOK P (pre ++ [fr])) (hc : fr.code = .swap p q :: rest) :
    Post P S (pre ++ [fr]) S
      (let r1 := wr pre { fr with code := rest } p (rd pre fr q)
       let r2 := wr r1.1 r1.2 q (rd pre fr p)
       r2.1 ++ [r2.2]) [] := by
  obtain ⟨G, G1, G', hf, hs, hrest, hdone⟩ := top_inv hst hc
  cases hs with
  | @swap _ _ x qp t hW hrp hrq hdisj =>
  have hf' : FrameOK P pre pre.length { fr with code := rest } G := ⟨hf.vals, hf.slots, G', hrest, hdone⟩
  have hst' := stack_top (stack_inv hst).1 hf' (stack_inv hst).2.2 rfl
  have hvq : VT (rd pre fr ⟨.own x, qp⟩) t := rd_ok hf.vals hf.slots hrq
  have hvp : VT (rd pre fr p) t := rd_ok hf.vals hf.slots hrp
  have hrdq : ∀ pre', rd pre' ({ fr with code := rest } : Frame) ⟨.own x, qp⟩ =
      rd pre fr ⟨.own x, qp⟩ := fun _ => rfl
  rcases p with ⟨y | b, path⟩
  · -- both places are owned by the running frame
    obtain ⟨hst1, hf1, hc1⟩ := own_write_ok hst' hf' hrp hvq
    obtain ⟨hst2, _, hc2⟩ := own_write_ok hst1 hf1 hrq hvp
    have e1 : rd pre ({ fr with code := rest }.wr y path (rd pre fr ⟨.own x, qp⟩)) ⟨.own x, qp⟩ =
        rd pre fr ⟨.own x, qp⟩ := rd_own_wr_disj (pre := pre) hdisj
    simp only [wr]
    refine ⟨hst2, by simp, fun r => ?_, fun h1 h2 => ⟨?_, by simp, h2⟩, Nat.le_refl _⟩
    · have a1 := hc1 r; have a2 := hc2 r
      rw [e1] at a2
      have a3 : rd pre ({ fr with code := rest } : Frame) ⟨.own y, path⟩ = rd pre fr ⟨.own y, path⟩ := rfl
      rw [a3] at a1
      simp only [cntFrames_snoc, cntFibers]
      have : cntVals r ({ fr with code := rest } : Frame).own = cntVals r fr.own := rfl
      omega
    · obtain ⟨h3, h4⟩ := fresh_snoc.mp h1
      refine fresh_snoc.mpr ⟨h3, wr_allLt (wr_allLt h4 (rd_allLt h1 _)) (rd_allLt h1 _)⟩
  · -- the first place is under a `mut` slot, the second is owned
    obtain ⟨e2, hst1, hf1, hlen, hc1, hfr1⟩ := slot_write_ok hst' hf' hW hrp hvq
    rw [← hlen] at hf1
    obtain ⟨hst2, _, hc2⟩ := own_write_ok hst1 hf1 hrq hvp
    simp only [e2, wr_own]
    refine ⟨hst2, by simp, fun r => ?_, fun h1 h2 => ⟨?_, by simp, h2⟩, Nat.le_refl _⟩
    · have a1 := hc1 r; have a2 := hc2 r
      have a3 : rd pre ({ fr with code := rest } : Frame) ⟨.bor b, path⟩ = rd pre fr ⟨.bor b, path⟩ := rfl
      rw [a3] at a1
      rw [hrdq] at a2
      simp only [cntFrames_snoc, cntFibers] at a1 ⊢
      have : cntVals r ({ fr with code := rest } : Frame).own = cntVals r fr.own := rfl
      omega
    · have h5 := hfr1 _ (by
        obtain ⟨h3, h4⟩ := fresh_snoc.mp h1
        exact fresh_snoc.mpr ⟨h3, h4⟩) (rd_allLt h1 _)
      obtain ⟨h3, h4⟩ := fresh_snoc.mp h5
      exact fresh_snoc.mpr ⟨h3, wr_allLt h4 (rd_allLt h1 _)⟩

theorem top_post {P : Prog} {pre : List Frame} {fr fr' : Frame} {G2 : Pcg} {S S' : Store}
    (hst : StackOK P (pre ++ [fr])) (hf' : FrameOK P pre pre.length fr' G2)
    (hs : fr'.slots = fr.slots)
    (hc : ∀ r, cntVals r fr'.own + liveN S r = cntVals r fr.own + liveN S' r)
    (hfr : FreshSt S.next (pre ++ [fr]) → (∀ r, S.live r = true → r < S.next) →
      (∀ v ∈ fr'.own, v.allLt S'.next) ∧ ∀ r, S'.live r = true → r < S'.next)
    (hm : S.next ≤ S'.next) : Post P S (pre ++ [fr]) S' (pre ++ [fr']) [] := by
  refine ⟨stack_top (stack_inv hst).1 hf' (stack_inv hst).2.2 hs, by simp, fun r => ?_,
    fun h1 h2 => ?_, hm⟩
  · have := hc r; simp only [cntFrames_snoc, cntFibers]; omega
  · obtain ⟨a, b⟩ := hfr h1 h2
    exact ⟨fresh_snoc.mpr ⟨fun f hf v hv => allLt_mono ((fresh_snoc.mp h1).1 f hf v hv) hm, a⟩,
      by simp, b⟩

theorem tyB_append {P : Prog} : ∀ {G G1 G2 : Pcg} {b1 b2 : List Stmt},
    TyB P G b1 G1 → TyB P G1 b2 G2 → TyB P G (b1 ++ b2) G2
  | _, _, _, [], _, h1, h2 => by cases h1; exact h2
  | _, _, _, _ :: _, _, h1, h2 => by
    cases h1 with
    | cons a b => exact .cons a (tyB_append b h2)

theorem consume_ok {P : Prog} {pre : List Frame} {fr fr1 : Frame} {o : Opnd} {rest : List Stmt}
    {S S1 : Store} {r : Nat}
    (hst : StackOK P (pre ++ [fr])) (hc : fr.code = .consume o :: rest)
    (he : evalO pre fr S o = (.hdl r, fr1, S1)) (hl : S1.live r = true)
    (hS : S.live S.next = false) :
    Post P S (pre ++ [fr]) ⟨upd S1.live r false, S1.next⟩ (pre ++ [{ fr1 with code := rest }]) [] := by
  obtain ⟨G, G1, G', hf, hs, hrest, hdone⟩ := top_inv hst hc
  cases hs with
  | consume ho =>
  obtain ⟨_, hv1, hsl1, _, hgs, hcnt, hfr, hmono, hlive⟩ := evalO_ok hf.vals hf.slots ho he hS
  refine top_post hst ⟨hv1, by simp only [hsl1, hgs]; exact hf.slots, G', hrest, hdone⟩ hsl1
    (fun r' => ?_) (fun h1 h2 => ?_) hmono
  · have := hcnt r'
    by_cases e : r = r'
    · subst e; simp [liveN, upd, hl, Val.cnt] at this ⊢; omega
    · simp [liveN, upd, Val.cnt, e, Ne.symm e] at this ⊢; omega
  · obtain ⟨a, _⟩ := hfr (fresh_snoc.mp h1).2
    refine ⟨a, fun r' hr' => ?_⟩
    simp only [upd] at hr'
    split at hr'
    · simp at hr'
    · exact hlive h2 r' hr'

theorem drop_ok {P : Prog} {pre : List Frame} {fr fr1 : Frame} {o : Opnd} {rest : List Stmt}
    {S S1 : Store} {v : Val}
    (hst : StackOK P (pre ++ [fr])) (hc : fr.code = .drop o :: rest)
    (he : evalO pre fr S o = (v, fr1, S1)) (hS : S.live S.next = false) :
    Post P S (pre ++ [fr]) S1 (pre ++ [{ fr1 with code := rest }]) [] := by
  obtain ⟨G, G1, G', hf, hs, hrest, hdone⟩ := top_inv hst hc
  cases hs with
  | drop ho hk =>
  obtain ⟨hvt, hv1, hsl1, _, hgs, hcnt, hfr, hmono, hlive⟩ := evalO_ok hf.vals hf.slots ho he hS
  refine top_post hst ⟨hv1, by simp only [hsl1, hgs]; exact hf.slots, G', hrest, hdone⟩ hsl1
    (fun r' => ?_) (fun h1 h2 => ⟨(hfr (fresh_snoc.mp h1).2).1, hlive h2⟩) hmono
  have := hcnt r'
  rw [cnt_droppable r' hvt hk] at this
  simp only at this ⊢; omega

theorem ifz_ok {P : Prog} {pre : List Frame} {fr fr1 : Frame} {o : Opnd} {b1 b2 rest : List Stmt}
    {S S1 : Store} {n : Nat}
    (hst : StackOK P (pre ++ [fr])) (hc : fr.code = .ifz o b1 b2 :: rest)
    (he : evalO pre fr S o = (.nat n, fr1, S1)) (hS : S.live S.next = false) :
    Post P S (pre ++ [fr]) S1 (pre ++ [{ fr1 with code := (if n = 0 then b1 else b2) ++ rest }]) [] := by
  obtain ⟨G, G1, G', hf, hs, hrest, hdone⟩ := top_inv hst hc
  cases hs with
  | @ifz _ Gc _ _ _ _ ho hb1 hb2 =>
  obtain ⟨_, hv1, hsl1, _, hgs, hcnt, hfr, hmono, hlive⟩ := evalO_ok hf.vals hf.slots ho he hS
  have hcode : TyB P Gc ((if n = 0 then b1 else b2) ++ rest) G' := by
    split
    · exact tyB_append hb1 hrest
    · exact tyB_append hb2 hrest
  refine top_post hst ⟨hv1, by simp only [hsl1, hgs]; exact hf.slots, G', hcode, hdone⟩ hsl1
    (fun r' => ?_) (fun h1 h2 => ⟨(hfr (fresh_snoc.mp h1).2).1, hlive h2⟩) hmono
  have := hcnt r'
  simp only [Val.cnt] at this ⊢; omega

theorem yield_ok {P : Prog} {pre : List Frame} {fr : Frame} {rest : List Stmt} {S : Store}
    (hst : StackOK P (pre ++ [fr])) (hc : fr.code = .yield :: rest) :
    Post P S (pre ++ [fr]) S (pre ++ [{ fr with code := rest }]) [] := by
  obtain ⟨G, G1, G', hf, hs, hrest, hdone⟩ := top_inv hst hc
  cases hs with
  | yield =>
  exact top_post hst ⟨hf.vals, hf.slots, G', hrest, hdone⟩ rfl (fun _ => rfl)
    (fun h1 h2 => ⟨(fresh_snoc.mp h1).2, h2⟩) (Nat.le_refl _)

theorem slotAt_snoc_lt {st : List Frame} {f : Frame} {i a : Nat} (hi : i < st.length) :
    slotAt (st ++ [f]) i a = slotAt st i a := by
  simp [slotAt, getElem_opt_append_lt hi]

theorem bs_pop {st : List Frame} {f : Frame} (h : BorrowSound (st ++ [f])) : BorrowSound st := by
  intro i a j b la lb ha hb hne ho hm
  exact h i a j b la lb (by rw [slotAt_snoc_lt (slotAt_lt ha)]; exact ha)
    (by rw [slotAt_snoc_lt (slotAt_lt hb)]; exact hb) hne ho hm

theorem ret_ok {P : Prog} {pre : List Frame} {fr : Frame} {S : Store}
    (hst : StackOK P (pre ++ [fr])) (hc : fr.code = []) :
    Post P S (pre ++ [fr]) S pre [] := by
  obtain ⟨hpre, ⟨G, hf⟩, hb⟩ := stack_inv hst
  obtain ⟨G', hb', hdone⟩ := hf.code
  rw [hc] at hb'
  cases hb'
  have h0 := fun r => cntVals_drop r hf.vals hdone
  refine ⟨⟨hpre, bs_pop hb⟩, by simp, fun r => ?_, fun h1 h2 => ⟨(fresh_snoc.mp h1).1, by simp, h2⟩,
    Nat.le_refl _⟩
  simp [cntFrames_snoc, cntFibers, h0 r]

/-! ## Calls: moving the `sink` arguments -/

theorem sh_at_set_disj : ∀ {p q : Path} (s n : Sh), ¬ Ovl p q → (s.set p n).at q = s.at q
  | [], _, _, _, h => absurd (Or.inl trivial) h
  | _ :: _, [], _, _, h => absurd (Or.inr trivial) h
  | a :: p, b :: q, s, n, h => by
    have h' : a ≠ b ∨ ¬ Ovl p q := by
      by_cases e : a = b
      · subst e; right; intro o; apply h; cases o with
        | inl o => exact Or.inl ⟨rfl, o⟩
        | inr o => exact Or.inr ⟨rfl, o⟩
      · exact Or.inl e
    match s with
    | .full (.pair t1 t2) =>
      cases a <;> cases b <;> simp [Sh.set, Sh.at] at h' ⊢ <;> exact sh_at_set_disj _ _ h'
    | .moved (.pair t1 t2) =>
      cases a <;> cases b <;> simp [Sh.set, Sh.at] at h' ⊢ <;> exact sh_at_set_disj _ _ h'
    | .pair s1 s2 =>
      cases a <;> cases b <;> simp [Sh.set, Sh.at] at h' ⊢ <;> exact sh_at_set_disj _ _ h'
    | .full .nat => simp [Sh.set]
    | .full .buf => simp [Sh.set]
    | .full .hdl => simp [Sh.set]
    | .moved .nat => simp [Sh.set]
    | .moved .buf => simp [Sh.set]
    | .moved .hdl => simp [Sh.set]

theorem read_setOwn_disj {G : Pcg} {x : Nat} {p : Path} {n : Sh} {q : Place}
    (hd : Place.Disj ⟨.own x, p⟩ q) : (G.setOwn x p n).read q = G.read q := by
  unfold Pcg.setOwn
  split
  · rename_i s hs
    rcases q with ⟨y | b, q⟩
    · simp only [Pcg.read, List.getElem?_set]
      by_cases e : x = y
      · subst e
        rcases hd with hd | hd
        · exact absurd rfl hd
        · obtain ⟨hxl, e⟩ := List.getElem?_eq_some_iff.mp hs
          simp [hs, sh_at_set_disj _ _ hd, hxl, e]
      · simp [e]
    · rfl
  · rfl

theorem moveSinks_slots : ∀ (G : Pcg) (ss : List Place) (ts : List Ty),
    (moveSinks G ss ts).slots = G.slots
  | _, [], _ => rfl
  | _, ⟨.own _, _⟩ :: _, [] => rfl
  | _, ⟨.bor _, _⟩ :: _, _ => rfl
  | G, ⟨.own x, p⟩ :: ss, _ :: ts => by
    simp only [moveSinks]; rw [moveSinks_slots]; exact setOwn_slots _ _ _ _

theorem clearSinks_slots : ∀ (fr : Frame) (ss : List Place), (clearSinks fr ss).slots = fr.slots
  | _, [] => rfl
  | _, ⟨.bor _, _⟩ :: _ => rfl
  | fr, ⟨.own x, p⟩ :: ss => by simp only [clearSinks]; rw [clearSinks_slots]; rfl

theorem clearSinks_code : ∀ (fr : Frame) (ss : List Place), (clearSinks fr ss).code = fr.code
  | _, [] => rfl
  | _, ⟨.bor _, _⟩ :: _ => rfl
  | fr, ⟨.own x, p⟩ :: ss => by simp only [clearSinks]; rw [clearSinks_code]; rfl

/-- The `sink` arguments are pairwise disjoint. -/
def SinkDisj : List Place → Prop
  | [] => True
  | p :: ps => (∀ q ∈ ps, p.Disj q) ∧ SinkDisj ps

theorem rd_wr_disj {pre : List Frame} {fr : Frame} {x : Nat} {p : Path} {w : Val} {q : Place}
    (hq : ∃ y, q.root = .own y) (hd : Place.Disj ⟨.own x, p⟩ q) :
    rd pre (fr.wr x p w) q = rd pre fr q := by
  obtain ⟨y, hy⟩ := hq
  rcases q with ⟨r, q⟩; simp at hy; subst hy
  exact rd_own_wr_disj hd

theorem sinksOK_own {G : Pcg} : ∀ {ss : List Place} {ts : List Ty}, SinksOK G ss ts →
    ∀ p ∈ ss, ∃ y, p.root = .own y
  | [], [], _ => by simp
  | _ :: _, _ :: _, h => by
    intro p hp; simp at hp; rcases hp with rfl | hp
    · exact h.1
    · exact sinksOK_own h.2.2 p hp
  | [], _ :: _, h => h.elim
  | _ :: _, [], h => h.elim

theorem sinksOK_setOwn {G : Pcg} {x : Nat} {p : Path} {n : Sh} : ∀ {ss : List Place} {ts : List Ty},
    SinksOK G ss ts → (∀ q ∈ ss, Place.Disj ⟨.own x, p⟩ q) → SinksOK (G.setOwn x p n) ss ts
  | [], [], _, _ => trivial
  | _ :: _, _ :: _, ⟨h1, h2, h3⟩, hd => ⟨h1, by rw [read_setOwn_disj (hd _ (by simp))]; exact h2,
      sinksOK_setOwn h3 (fun q hq => hd q (by simp [hq]))⟩
  | [], _ :: _, h, _ => h.elim
  | _ :: _, [], h, _ => h.elim

/-- Moving the `sink` arguments out of the caller: the caller frame agrees
with `moveSinks`, the moved values have the parameter types, handles are
conserved and every place disjoint from the arguments is untouched. -/
theorem sinks_ok {pre : List Frame} : ∀ {fr : Frame} {G : Pcg} {ss : List Place} {ts : List Ty},
    VSL fr.own G.own → SlotsOK pre pre.length fr.slots G.slots → SinksOK G ss ts → SinkDisj ss →
    VSL (clearSinks fr ss).own (moveSinks G ss ts).own ∧
      VSL (ss.map (rd pre fr)) (ts.map .full) ∧
      (∀ r, cntVals r (clearSinks fr ss).own + cntVals r (ss.map (rd pre fr)) = cntVals r fr.own) ∧
      (∀ q, (∀ p ∈ ss, p.Disj q) → (∃ y, q.root = .own y) → rd pre (clearSinks fr ss) q = rd pre fr q) ∧
      (∀ q, (∀ p ∈ ss, p.Disj q) → (moveSinks G ss ts).read q = G.read q)
  | fr, G, [], [], hv, _, _, _ => by
    refine ⟨hv, trivial, fun r => by simp [cntVals, clearSinks], fun q _ _ => rfl, fun q _ => rfl⟩
  | fr, G, ⟨.own x, p⟩ :: ss, t :: ts, hv, hsl, ⟨_, hr, hss⟩, ⟨hd, hds⟩ => by
    obtain ⟨hvt, hvp, s, s', hs1, hs2, _⟩ := read_own hv hr
    have hv' : VSL (fr.wr x p .undef).own (G.setOwn x p (.moved t)).own :=
      vsl_setOwn hv hs1 hs2 (by simp)
    have hsl' : SlotsOK pre pre.length (fr.wr x p .undef).slots (G.setOwn x p (.moved t)).slots := by
      rw [setOwn_slots]; exact hsl
    have hss' : SinksOK (G.setOwn x p (.moved t)) ss ts := sinksOK_setOwn hss hd
    obtain ⟨A, B, C, D, E⟩ := sinks_ok (fr := fr.wr x p .undef) hv' hsl' hss' hds
    have hmap : ss.map (rd pre (fr.wr x p .undef)) = ss.map (rd pre fr) :=
      List.map_congr_left (fun q hq => rd_wr_disj (sinksOK_own hss q hq) (hd q hq))
    rw [hmap] at B C
    refine ⟨A, ⟨by simpa [rd] using hvt, B⟩, fun r => ?_, fun q hq hown => ?_, fun q hq => ?_⟩
    · have h1 := C r
      have h2 := cnt_frame_wr r (f := fr) Val.undef (get_lt hvt) hvp
      simp only [clearSinks, List.map_cons, cntVals, Val.cnt] at h1 h2 ⊢
      have : rd pre fr ⟨.own x, p⟩ = (fr.get x).at p := rfl
      rw [this]; omega
    · simp only [clearSinks]
      rw [D q (fun p hp => hq p (by simp [hp])) hown]
      exact rd_wr_disj hown (hq _ (by simp))
    · simp only [moveSinks]
      rw [E q (fun p hp => hq p (by simp [hp]))]
      exact read_setOwn_disj (hq _ (by simp))
  | _, _, ⟨.bor _, _⟩ :: _, _ :: _, _, _, h, _ => by
    obtain ⟨⟨y, hy⟩, _⟩ := h; simp at hy
  | _, _, [], _ :: _, _, _, h, _ => h.elim
  | _, _, _ :: _, [], _, _, h, _ => h.elim

/-! ## Calls: argument compatibility -/

theorem disj_symm {p q : Place} (h : p.Disj q) : q.Disj p := by
  rcases h with h | h
  · exact Or.inl (Ne.symm h)
  · exact Or.inr (fun o => h (ovl_symm o))

theorem compat_symm {a b : Bool × Place} (h : Compat a b) : Compat b a := by
  rcases h with ⟨h1, h2⟩ | h
  · exact Or.inl ⟨h2, h1⟩
  · exact Or.inr (disj_symm h)

theorem allCompat_append : ∀ {A B : List (Bool × Place)}, AllCompat (A ++ B) →
    AllCompat A ∧ AllCompat B ∧ ∀ a ∈ A, ∀ b ∈ B, Compat a b
  | [], _, h => ⟨trivial, by simpa using h, by simp⟩
  | a :: A, B, ⟨h1, h2⟩ => by
    obtain ⟨i1, i2, i3⟩ := allCompat_append h2
    refine ⟨⟨fun b hb => h1 b (by simp [hb]), i1⟩, i2, ?_⟩
    intro x hx b hb
    simp at hx; rcases hx with rfl | hx
    · exact h1 b (by simp [hb])
    · exact i3 x hx b hb

theorem allCompat_idx : ∀ {L : List (Bool × Place)} {i j : Nat} {a b : Bool × Place},
    AllCompat L → i ≠ j → L[i]? = some a → L[j]? = some b → Compat a b
  | [], _, _, _, _, _, _, e, _ => by simp at e
  | x :: L, 0, 0, _, _, _, h, _, _ => absurd rfl h
  | x :: L, 0, j + 1, a, b, ⟨h1, _⟩, _, ea, eb => by
    simp at ea; subst ea; exact h1 b (List.mem_of_getElem? (by simpa using eb))
  | x :: L, i + 1, 0, a, b, ⟨h1, _⟩, _, ea, eb => by
    simp at eb; subst eb; exact compat_symm (h1 a (List.mem_of_getElem? (by simpa using ea)))
  | x :: L, i + 1, j + 1, a, b, ⟨_, h2⟩, hne, ea, eb =>
    allCompat_idx h2 (fun e => hne (congrArg (· + 1) e)) (by simpa using ea) (by simpa using eb)

theorem sinkDisj_of : ∀ {ss : List Place}, AllCompat (ss.map (fun p => (false, p))) → SinkDisj ss
  | [], _ => trivial
  | p :: ps, ⟨h1, h2⟩ => by
    refine ⟨fun q hq => ?_, sinkDisj_of h2⟩
    rcases h1 (false, q) (by simp [hq]) with ⟨h, _⟩ | h
    · simp at h
    · exact h

theorem args_split {ss bs : List Place} {ms : List (Mu × Ty)} (hlen : bs.length = ms.length)
    (h : AllCompat (argList ss bs ms)) :
    SinkDisj ss ∧ AllCompat ((bs.zip ms).map (fun a => (a.2.1 == .shr, a.1))) ∧
      ∀ p ∈ ss, ∀ q ∈ bs, p.Disj q := by
  obtain ⟨h1, h2, h3⟩ := allCompat_append h
  refine ⟨sinkDisj_of h1, h2, fun p hp q hq => ?_⟩
  obtain ⟨i, hi⟩ := List.mem_iff_getElem?.mp hq
  have hil : i < ms.length := by rw [← hlen]; exact (List.getElem?_eq_some_iff.mp hi).1
  have hz : (bs.zip ms)[i]? = some (q, ms[i]) := by
    rw [List.getElem?_zip_eq_some]; exact ⟨hi, List.getElem?_eq_getElem hil⟩
  have hm : (ms[i].1 == .shr, q) ∈ (bs.zip ms).map (fun a => (a.2.1 == .shr, a.1)) :=
    List.mem_map.mpr ⟨_, List.mem_of_getElem? hz, rfl⟩
  rcases h3 (false, p) (by simp [argList, hp]) _ hm with ⟨h, _⟩ | h
  · simp at h
  · exact h

/-! ## Calls: the callee's slots -/

theorem borsOK_len {G : Pcg} : ∀ {bs : List Place} {ms : List (Mu × Ty)}, BorsOK G bs ms →
    bs.length = ms.length
  | [], [], _ => rfl
  | _ :: _, _ :: _, h => by simp [borsOK_len h.2.2]
  | [], _ :: _, h => h.elim
  | _ :: _, [], h => h.elim

theorem borsOK_get {G : Pcg} : ∀ {bs : List Place} {ms : List (Mu × Ty)} {a : Nat} {q : Place}
    {μ : Mu} {t : Ty}, BorsOK G bs ms → bs[a]? = some q → ms[a]? = some (μ, t) →
    G.read q = some t ∧ (μ = .mut → G.Writable q)
  | [], [], _, _, _, _, _, e, _ => by simp at e
  | _ :: _, (_, _) :: _, 0, _, _, _, h, e1, e2 => by
    simp at e1 e2; obtain ⟨rfl, rfl⟩ := e2; subst e1; exact ⟨h.1, h.2.1⟩
  | _ :: _, _ :: _, a + 1, _, _, _, h, e1, e2 => borsOK_get h.2.2 (by simpa using e1) (by simpa using e2)
  | [], _ :: _, _, _, _, _, h, _, _ => h.elim
  | _ :: _, [], _, _, _, _, h, _, _ => h.elim

theorem slots_get {n : Nat} {fr : Frame} {bs : List Place} {ms : List (Mu × Ty)} {a : Nat} {la : Loc}
    (h : ((bs.zip ms).map (resolve n fr))[a]? = some la) :
    ∃ q m, bs[a]? = some q ∧ ms[a]? = some m ∧ la = resolve n fr (q, m) := by
  rw [List.getElem?_map] at h
  cases e : (bs.zip ms)[a]? with
  | none => rw [e] at h; simp at h
  | some z =>
    rw [e] at h; simp at h
    obtain ⟨h1, h2⟩ := List.getElem?_zip_eq_some.mp e
    exact ⟨z.1, z.2, h1, h2, h.symm⟩

theorem resolve_mu (n : Nat) (fr : Frame) (a : Place × (Mu × Ty)) : (resolve n fr a).mu = a.2.1 := by
  simp only [resolve]; split
  · rfl
  · split <;> rfl

/-- A borrow argument, disjoint from every `sink` argument, becomes a sound
slot of the callee. -/
theorem arg_slot_ok {pre : List Frame} {fr fr1 : Frame} {G : Pcg} {ss : List Place} {q : Place}
    {μ : Mu} {t : Ty} (hv : VSL fr.own G.own) (hsl : SlotsOK pre pre.length fr.slots G.slots)
    (hr : G.read q = some t)
    (hown : (∃ y, q.root = .own y) → (fr1.get = (clearSinks fr ss).get) ∧
      rd pre (clearSinks fr ss) q = rd pre fr q) :
    SlotOK (pre ++ [fr1]) (pre.length + 1) (resolve pre.length fr (q, (μ, t))) (μ, t) := by
  rcases q with ⟨x | c, path⟩
  · obtain ⟨hg, hrd⟩ := hown ⟨x, rfl⟩
    refine ⟨rfl, Nat.lt_succ_self _, ?_⟩
    have : stGet (pre ++ [fr1]) pre.length x = fr1.get x := by simp [stGet]
    simp only [resolve]; rw [this, hg]
    have h2 : rd pre (clearSinks fr ss) ⟨.own x, path⟩ = ((clearSinks fr ss).get x).at path := rfl
    rw [← h2, hrd]; exact rd_ok hv hsl hr
  · obtain ⟨l, t0, hl, _, hpos, h3, h4⟩ := read_bor hsl hr
    simp only [resolve, hl]
    refine ⟨rfl, Nat.lt_succ_of_lt hpos, ?_⟩
    rw [stGet_congr (getElem_opt_append_lt hpos), at_append]
    exact vt_at h3 h4

theorem callee_slots_ok {pre : List Frame} {fr fr1 : Frame} {G : Pcg} {ss : List Place}
    (hv : VSL fr.own G.own) (hsl : SlotsOK pre pre.length fr.slots G.slots)
    (hg : fr1.get = (clearSinks fr ss).get)
    (hclear : ∀ q, (∀ p ∈ ss, p.Disj q) → (∃ y, q.root = .own y) →
      rd pre (clearSinks fr ss) q = rd pre fr q) :
    ∀ {bs : List Place} {ms : List (Mu × Ty)}, BorsOK G bs ms → (∀ p ∈ ss, ∀ q ∈ bs, p.Disj q) →
    SlotsOK (pre ++ [fr1]) (pre.length + 1) ((bs.zip ms).map (resolve pre.length fr)) ms
  | [], [], _, _ => trivial
  | q :: bs, (μ, t) :: ms, ⟨h1, _, h3⟩, hd => by
    refine ⟨arg_slot_ok hv hsl h1 (fun ho => ⟨hg, hclear q (fun p hp => hd p hp q (by simp)) ho⟩),
      callee_slots_ok hv hsl hg hclear h3 (fun p hp q' hq' => hd p hp q' (by simp [hq']))⟩
  | [], _ :: _, h, _ => h.elim
  | _ :: _, [], h, _ => h.elim

/-! ## Calls: borrow soundness of the new frame -/

theorem lovl_symm {l m : Loc} (h : LOvl l m) : LOvl m l := ⟨h.1.symm, h.2.1.symm, ovl_symm h.2.2⟩

theorem slot_pos_lt {P : Prog} {st : List Frame} (h : StackOK P st) {j b : Nat} {m : Loc}
    (e : slotAt st j b = some m) : m.pos < j := by
  simp only [slotAt] at e
  cases ej : st[j]? with
  | none => rw [ej] at e; simp at e
  | some f =>
    rw [ej] at e; simp at e
    obtain ⟨G, hG⟩ := h.frames j f ej
    obtain ⟨_, _, _, h2, _⟩ := slotsOK_get' hG.slots e
    exact h2

theorem lovl_ext {l m : Loc} {μ : Mu} {p : Path} (h : LOvl ⟨μ, l.pos, l.x, l.path ++ p⟩ m) :
    LOvl l m := ⟨h.1, h.2.1, ovl_symm (ovl_append p (ovl_symm h.2.2))⟩

theorem new_old {P : Prog} {pre : List Frame} {fr : Frame} {G : Pcg} {bs : List Place}
    {ms : List (Mu × Ty)} (hst : StackOK P (pre ++ [fr])) (hf : FrameOK P pre pre.length fr G)
    (hb : BorsOK G bs ms) {a j b : Nat} {la lb : Loc}
    (ha : ((bs.zip ms).map (resolve pre.length fr))[a]? = some la)
    (hj : j ≤ pre.length) (hlb : slotAt (pre ++ [fr]) j b = some lb)
    (ho : LOvl la lb) (hm : la.mu = .mut ∨ lb.mu = .mut) :
    lb.mu = .mut ∧ Pre lb.path la.path := by
  obtain ⟨q, ⟨μ, t⟩, hq, hmt, rfl⟩ := slots_get ha
  obtain ⟨hr, hw⟩ := borsOK_get hb hq hmt
  have hpos := slot_pos_lt hst hlb
  rcases q with ⟨x | c, path⟩
  · simp only [resolve] at ho; have := ho.1; simp at this; omega
  · obtain ⟨l, t0, hl, hg, _, _, _⟩ := read_bor hf.slots hr
    simp only [resolve, hl] at ho hm ⊢
    have hlo : LOvl l lb := lovl_ext ho
    have hmut : μ = .mut → l.mu = .mut := by
      intro e; obtain ⟨t1, ht1⟩ := hw e; rw [hg] at ht1; simp at ht1; exact ht1.1
    have hls : slotAt (pre ++ [fr]) pre.length c = some l := by simp [slotAt, hl]
    by_cases e : j = pre.length ∧ b = c
    · obtain ⟨rfl, rfl⟩ := e
      rw [hls] at hlb; cases hlb
      refine ⟨?_, pre_append_right _ _⟩
      rcases hm with hm | hm
      · exact hmut hm
      · exact hm
    · have hne : pre.length ≠ j ∨ c ≠ b := by
        by_cases h1 : j = pre.length
        · exact Or.inr (fun h2 => e ⟨h1, h2.symm⟩)
        · exact Or.inl (Ne.symm h1)
      have hm' : l.mu = .mut ∨ lb.mu = .mut := by
        rcases hm with hm | hm
        · exact Or.inl (hmut hm)
        · exact Or.inr hm
      rcases hst.sound _ _ _ _ _ _ hls hlb hne hlo hm' with ⟨h1, _⟩ | ⟨_, h2, h3⟩
      · omega
      · exact ⟨h2, pre_trans h3 (pre_append_right _ _)⟩

theorem new_new {P : Prog} {pre : List Frame} {fr : Frame} {G : Pcg} {bs : List Place}
    {ms : List (Mu × Ty)} (hst : StackOK P (pre ++ [fr])) (hf : FrameOK P pre pre.length fr G)
    (hb : BorsOK G bs ms) (hc : AllCompat ((bs.zip ms).map (fun a => (a.2.1 == .shr, a.1))))
    {a b : Nat} {la lb : Loc} (hab : a ≠ b)
    (ha : ((bs.zip ms).map (resolve pre.length fr))[a]? = some la)
    (hb' : ((bs.zip ms).map (resolve pre.length fr))[b]? = some lb)
    (ho : LOvl la lb) (hm : la.mu = .mut ∨ lb.mu = .mut) : False := by
  obtain ⟨q1, ⟨μ1, t1⟩, hq1, hm1, rfl⟩ := slots_get ha
  obtain ⟨q2, ⟨μ2, t2⟩, hq2, hm2, rfl⟩ := slots_get hb'
  obtain ⟨hr1, hw1⟩ := borsOK_get hb hq1 hm1
  obtain ⟨hr2, hw2⟩ := borsOK_get hb hq2 hm2
  rw [resolve_mu, resolve_mu] at hm
  have hz1 : (bs.zip ms)[a]? = some (q1, (μ1, t1)) := List.getElem?_zip_eq_some.mpr ⟨hq1, hm1⟩
  have hz2 : (bs.zip ms)[b]? = some (q2, (μ2, t2)) := List.getElem?_zip_eq_some.mpr ⟨hq2, hm2⟩
  have hcomp := allCompat_idx hc hab (by rw [List.getElem?_map, hz1]; rfl)
    (by rw [List.getElem?_map, hz2]; rfl)
  have hdisj : q1.Disj q2 := by
    rcases hcomp with ⟨e1, e2⟩ | h
    · simp at e1 e2; rcases hm with hm | hm <;> simp_all
    · exact h
  have mutl : ∀ {c : Nat} {l : Loc} {t0 : Ty} {μ : Mu} {path : Path},
      fr.slots[c]? = some l → G.slots[c]? = some (l.mu, t0) →
      (μ = .mut → G.Writable ⟨.bor c, path⟩) → μ = .mut → l.mu = .mut := by
    intro c l t0 μ path _ hg hw e
    obtain ⟨t3, ht3⟩ := hw e; rw [hg] at ht3; simp at ht3; exact ht3.1
  rcases q1 with ⟨x1 | c1, p1⟩ <;> rcases q2 with ⟨x2 | c2, p2⟩
  · simp only [resolve] at ho
    rcases hdisj with h | h
    · exact h (by simp at ho ⊢; exact ho.2.1)
    · exact h ho.2.2
  · obtain ⟨l, _, hl, _, hpos, _, _⟩ := read_bor hf.slots hr2
    simp only [resolve, hl] at ho; have := ho.1; simp at this; omega
  · obtain ⟨l, _, hl, _, hpos, _, _⟩ := read_bor hf.slots hr1
    simp only [resolve, hl] at ho; have := ho.1; simp at this; omega
  · obtain ⟨l1, u1, hl1, hg1, _, _, _⟩ := read_bor hf.slots hr1
    obtain ⟨l2, u2, hl2, hg2, _, _, _⟩ := read_bor hf.slots hr2
    simp only [resolve, hl1, hl2] at ho
    by_cases e : c1 = c2
    · subst e
      rw [hl1] at hl2; cases hl2
      rcases hdisj with h | h
      · exact h rfl
      · exact not_ovl_prefix l1.path h ho.2.2
    · have hlo : LOvl l1 l2 := lovl_ext (lovl_symm (lovl_ext (lovl_symm ho)))
      have hm' : l1.mu = .mut ∨ l2.mu = .mut := by
        rcases hm with hm | hm
        · exact Or.inl (mutl hl1 hg1 hw1 hm)
        · exact Or.inr (mutl hl2 hg2 hw2 hm)
      rcases hst.sound _ _ _ _ _ _ (by simp [slotAt, hl1] : slotAt (pre ++ [fr]) pre.length c1 = some l1)
        (by simp [slotAt, hl2] : slotAt (pre ++ [fr]) pre.length c2 = some l2) (Or.inr e) hlo hm'
        with ⟨h, _⟩ | ⟨h, _⟩ <;> omega

theorem slotAt_push {pre : List Frame} {fr fr1 callee : Frame} (hs : fr1.slots = fr.slots)
    {i a : Nat} (hi : i ≤ pre.length) :
    slotAt (pre ++ [fr1, callee]) i a = slotAt (pre ++ [fr]) i a := by
  have e : pre ++ [fr1, callee] = (pre ++ [fr1]) ++ [callee] := by simp
  rw [e, slotAt_snoc_lt (by simp; omega)]
  exact slotAt_snoc_top hs i a

theorem slotAt_push_top {pre : List Frame} {fr1 callee : Frame} {a : Nat} :
    slotAt (pre ++ [fr1, callee]) (pre.length + 1) a = callee.slots[a]? := by
  simp [slotAt]

theorem call_bs {P : Prog} {pre : List Frame} {fr fr1 callee : Frame} {G : Pcg} {bs : List Place}
    {ms : List (Mu × Ty)} (hst : StackOK P (pre ++ [fr])) (hf : FrameOK P pre pre.length fr G)
    (hs1 : fr1.slots = fr.slots) (hcs : callee.slots = (bs.zip ms).map (resolve pre.length fr))
    (hb : BorsOK G bs ms) (hc : AllCompat ((bs.zip ms).map (fun a => (a.2.1 == .shr, a.1)))) :
    BorrowSound (pre ++ [fr1, callee]) := by
  intro i a j b la lb ha hb' hne ho hm
  have hi := slotAt_lt ha
  have hj := slotAt_lt hb'
  simp at hi hj
  rcases Nat.lt_or_ge i (pre.length + 1) with hi' | hi' <;>
    rcases Nat.lt_or_ge j (pre.length + 1) with hj' | hj'
  · rw [slotAt_push hs1 (by omega)] at ha hb'
    exact hst.sound _ _ _ _ _ _ ha hb' hne ho hm
  · have ej : j = pre.length + 1 := by omega
    subst ej
    rw [slotAt_push hs1 (by omega)] at ha
    rw [slotAt_push_top, hcs] at hb'
    obtain ⟨h1, h2⟩ := new_old hst hf hb hb' (by omega) ha (lovl_symm ho) hm.symm
    exact Or.inl ⟨hi', h1, h2⟩
  · have ei : i = pre.length + 1 := by omega
    subst ei
    rw [slotAt_push hs1 (by omega)] at hb'
    rw [slotAt_push_top, hcs] at ha
    obtain ⟨h1, h2⟩ := new_old hst hf hb ha (by omega) hb' ho hm
    exact Or.inr ⟨hj', h1, h2⟩
  · have ei : i = pre.length + 1 := by omega
    have ej : j = pre.length + 1 := by omega
    subst ei; subst ej
    rw [slotAt_push_top, hcs] at ha hb'
    have hab : a ≠ b := by rcases hne with h | h; exact absurd rfl h; exact h
    exact (new_new hst hf hb hc hab ha hb' ho hm).elim

theorem vsl_append : ∀ {a : List Val} {b : List Sh} {c : List Val} {d : List Sh},
    VSL a b → VSL c d → VSL (a ++ c) (b ++ d)
  | [], [], _, _, _, h => h
  | _ :: _, _ :: _, _, _, h1, h2 => ⟨h1.1, vsl_append h1.2 h2⟩
  | [], _ :: _, _, _, h, _ => h.elim
  | _ :: _, [], _, _, h, _ => h.elim

theorem vsl_undefs : ∀ (ts : List Ty), VSL (ts.map (fun _ => Val.undef)) (ts.map .moved)
  | [] => trivial
  | _ :: ts => ⟨by simp, vsl_undefs ts⟩

theorem clearSinks_get : ∀ (fr : Frame) (ss : List Place) (x : Nat),
    ({ clearSinks fr ss with code := (fr.code) } : Frame).get x = (clearSinks fr ss).get x := by
  intros; rfl

theorem clearSinks_allLt {n : Nat} : ∀ {fr : Frame} (ss : List Place), (∀ v ∈ fr.own, v.allLt n) →
    ∀ v ∈ (clearSinks fr ss).own, v.allLt n
  | _, [], h => h
  | _, ⟨.bor _, _⟩ :: _, h => h
  | _, ⟨.own _, _⟩ :: ss, h => clearSinks_allLt ss (wr_allLt h trivial)

theorem callee_allLt {n : Nat} {pre : List Frame} {fr : Frame} {F : Fn} {ss bs : List Place}
    (h : FreshSt n (pre ++ [fr])) : ∀ v ∈ (calleeFrame pre fr F ss bs).own, v.allLt n := by
  intro v hv
  simp [calleeFrame] at hv
  rcases hv with ⟨p, _, rfl⟩ | ⟨_, _, rfl⟩
  · exact rd_allLt h p
  · trivial

theorem call_ok {P : Prog} {pre : List Frame} {fr : Frame} {g : Nat} {F : Fn} {ss bs : List Place}
    {rest : List Stmt} {S : Store} (hP : ProgOK P)
    (hst : StackOK P (pre ++ [fr])) (hc : fr.code = .call g ss bs :: rest) (hF : P[g]? = some F) :
    Post P S (pre ++ [fr]) S
      (pre ++ [{ clearSinks fr ss with code := rest }, calleeFrame pre fr F ss bs]) [] := by
  obtain ⟨G, G1, G', hf, hs, hrest, hdone⟩ := top_inv hst hc
  cases hs with
  | @call _ _ F' _ _ hF' hsk hbo hcomp =>
  rw [hF] at hF'; cases hF'
  obtain ⟨Gf, hbody, hdf⟩ := hP F (List.mem_of_getElem? hF)
  obtain ⟨hsd, hbc, hcross⟩ := args_split (borsOK_len hbo) hcomp
  obtain ⟨A, B, C, D, _⟩ := sinks_ok (pre := pre) hf.vals hf.slots hsk hsd
  let fr1 : Frame := { clearSinks fr ss with code := rest }
  let callee := calleeFrame pre fr F ss bs
  have hf1 : FrameOK P pre pre.length fr1 (moveSinks G ss F.sinks) :=
    ⟨A, by simp only [fr1, clearSinks_slots, moveSinks_slots]; exact hf.slots, G', hrest, hdone⟩
  have hfc : FrameOK P (pre ++ [fr1]) (pre.length + 1) callee F.init := by
    refine ⟨vsl_append B (vsl_undefs _), ?_, Gf, hbody, hdf⟩
    exact callee_slots_ok hf.vals hf.slots (fr1 := fr1) rfl
      (fun q hq ho => D q hq ho) hbo hcross
  have e : pre ++ [fr1, callee] = (pre ++ [fr1]) ++ [callee] := by simp
  refine ⟨⟨?_, call_bs hst hf (clearSinks_slots fr ss) rfl hbo hbc⟩, by simp, fun r => ?_,
    fun h1 h2 => ⟨?_, by simp, h2⟩, Nat.le_refl _⟩
  · rw [e]
    refine preOK_snoc.mpr ⟨preOK_snoc.mpr ⟨(stack_inv hst).1, _, hf1⟩, F.init, ?_⟩
    simpa using hfc
  · have := C r
    have hcal : cntVals r callee.own = cntVals r (ss.map (rd pre fr)) := by
      simp [callee, calleeFrame, cntVals_append, cntVals_undefs]
    rw [e, cntFrames_snoc, cntFrames_snoc, cntFrames_snoc, hcal]
    simp only [cntFibers]
    have : cntVals r fr1.own = cntVals r (clearSinks fr ss).own := rfl
    omega
  · rw [e]
    obtain ⟨h3, h4⟩ := fresh_snoc.mp h1
    exact fresh_snoc.mpr ⟨fresh_snoc.mpr ⟨h3, clearSinks_allLt ss h4⟩, callee_allLt h1⟩

theorem spawn_ok {P : Prog} {pre : List Frame} {fr : Frame} {g : Nat} {F : Fn} {ss : List Place}
    {rest : List Stmt} {S : Store} (b : Nat) (hP : ProgOK P)
    (hst : StackOK P (pre ++ [fr])) (hc : fr.code = .spawn g ss :: rest) (hF : P[g]? = some F) :
    Post P S (pre ++ [fr]) S (pre ++ [{ clearSinks fr ss with code := rest }])
      [⟨b, [calleeFrame pre fr F ss []], false⟩] := by
  obtain ⟨G, G1, G', hf, hs, hrest, hdone⟩ := top_inv hst hc
  cases hs with
  | @spawn _ _ F' _ hF' hnb hsk hcomp =>
  rw [hF] at hF'; cases hF'
  obtain ⟨Gf, hbody, hdf⟩ := hP F (List.mem_of_getElem? hF)
  obtain ⟨hsd, _, _⟩ := args_split (ms := []) rfl hcomp
  obtain ⟨A, B, C, _, _⟩ := sinks_ok (pre := pre) hf.vals hf.slots hsk hsd
  let fr1 : Frame := { clearSinks fr ss with code := rest }
  have hf1 : FrameOK P pre pre.length fr1 (moveSinks G ss F.sinks) :=
    ⟨A, by simp only [fr1, clearSinks_slots, moveSinks_slots]; exact hf.slots, G', hrest, hdone⟩
  have hfc : FrameOK P [] 0 (calleeFrame pre fr F ss []) F.init := by
    refine ⟨vsl_append B (vsl_undefs _), ?_, Gf, hbody, hdf⟩
    simp [calleeFrame, Fn.init, hnb, SlotsOK]
  refine ⟨stack_top (stack_inv hst).1 hf1 (stack_inv hst).2.2 (clearSinks_slots fr ss), ?_,
    fun r => ?_, fun h1 h2 => ⟨?_, ?_, h2⟩, Nat.le_refl _⟩
  · intro f hf'
    simp at hf'; subst hf'
    refine ⟨fun i f e => ?_, fun i a j b' la lb ha _ _ _ _ => ?_⟩
    · rcases i with _ | i
      · simp at e; subst e
        exact ⟨F.init, frameOK_congr (fun p hp => absurd hp (Nat.not_lt_zero _)) hfc⟩
      · simp at e
    · rcases i with _ | i
      · simp [slotAt, calleeFrame] at ha
      · simp [slotAt] at ha
  · have := C r
    simp [cntFrames_snoc, cntFibers, cntFrames, calleeFrame, cntVals_append, cntVals_undefs]
    have : cntVals r fr1.own = cntVals r (clearSinks fr ss).own := rfl
    omega
  · obtain ⟨h3, h4⟩ := fresh_snoc.mp h1
    exact fresh_snoc.mpr ⟨h3, clearSinks_allLt ss h4⟩
  · intro f hf' fr' hfr' v hv
    simp at hf'; subst hf'
    simp at hfr'; subst hfr'
    exact callee_allLt h1 v hv

/-! ## Type safety -/

theorem fstep_ok {P : Prog} {f f' : Fiber} {S S' : Store} {new : List Fiber} (hP : ProgOK P)
    (hst : StackOK P f.stack) (h : FStep P f S f' S' new) (hS : S.live S.next = false) :
    Post P S f.stack S' f'.stack new := by
  cases h with
  | assign _ e hc he => rw [e] at hst ⊢; exact assign_ok hst hc he hS
  | swap _ e hc => rw [e] at hst ⊢; exact swap_ok hst hc
  | consume _ e hc he hl => rw [e] at hst ⊢; exact consume_ok hst hc he hl hS
  | drop _ e hc he => rw [e] at hst ⊢; exact drop_ok hst hc he hS
  | ifz _ e hc he => rw [e] at hst ⊢; exact ifz_ok hst hc he hS
  | call _ e hc hF => rw [e] at hst ⊢; exact call_ok hP hst hc hF
  | ret _ e hc => rw [e] at hst ⊢; exact ret_ok hst hc
  | spawn b _ e hc hF => rw [e] at hst ⊢; exact spawn_ok b hP hst hc hF
  | yield _ e hc => rw [e] at hst ⊢; exact yield_ok hst hc

theorem fresh_mono {n m : Nat} {st : List Frame} (h : FreshSt n st) (hm : n ≤ m) : FreshSt m st :=
  fun f hf v hv => allLt_mono (h f hf v hv) hm

/-- **Preservation** (Theorem 1): every step keeps the configuration typed,
borrow-sound and in exact agreement with the store. -/
theorem preservation {P : Prog} {c c' : Cfg} (hc : CfgOK P c) (h : Step P c c') : CfgOK P c' := by
  cases h with
  | @run i f f' S' new hf hs =>
    have hfm : f ∈ c.fibers := List.mem_of_getElem? hf
    have hS : c.st.live c.st.next = false := by
      cases e : c.st.live c.st.next
      · rfl
      · have := hc.fresh _ e; omega
    have post := fstep_ok hc.prog (hc.stacks f hfm) hs hS
    have hi : i < c.fibers.length := (List.getElem?_eq_some_iff.mp hf).1
    have hfi : c.fibers[i] = f := (List.getElem?_eq_some_iff.mp hf).2
    obtain ⟨hfr1, hfr2, hlive⟩ := post.fresh (hc.freshv f hfm) hc.fresh
    refine ⟨hc.prog, fun g hg => ?_, fun r => ?_, hlive, fun g hg => ?_⟩
    · simp at hg
      rcases hg with hg | hg
      · rcases List.mem_or_eq_of_mem_set hg with hg | rfl
        · exact hc.stacks g hg
        · exact post.stack
      · exact post.spawned g hg
    · have h1 := cntFibers_set r (fs := c.fibers) f' hi
      have h2 := post.cnt r
      have h3 := hc.count r
      rw [hfi] at h1
      simp only [cntFibers_append]
      omega
    · simp at hg
      rcases hg with hg | hg
      · rcases List.mem_or_eq_of_mem_set hg with hg | rfl
        · exact fresh_mono (hc.freshv g hg) post.mono
        · exact hfr1
      · exact hfr2 g hg
  | @resume i f b hf hsusp =>
    have hi : i < c.fibers.length := (List.getElem?_eq_some_iff.mp hf).1
    have hfi : c.fibers[i] = f := (List.getElem?_eq_some_iff.mp hf).2
    have hfm : f ∈ c.fibers := List.mem_of_getElem? hf
    refine ⟨hc.prog, fun g hg => ?_, fun r => ?_, hc.fresh, fun g hg => ?_⟩
    · rcases List.mem_or_eq_of_mem_set hg with hg | rfl
      · exact hc.stacks g hg
      · exact hc.stacks f hfm
    · have h1 := cntFibers_set r (fs := c.fibers) { f.reloc b with susp := false } hi
      rw [hfi] at h1
      have h3 := hc.count r
      simp only [Fiber.reloc] at h1 ⊢
      omega
    · rcases List.mem_or_eq_of_mem_set hg with hg | rfl
      · exact hc.freshv g hg
      · exact hc.freshv f hfm

theorem preservation_steps {P : Prog} {c c' : Cfg} (hc : CfgOK P c) (h : Steps P c c') :
    CfgOK P c' := by
  induction h with
  | refl => exact hc
  | step h1 _ ih => exact ih (preservation hc h1)

theorem cntVals_le_frames (r : Nat) : ∀ {st : List Frame} {fr : Frame}, fr ∈ st →
    cntVals r fr.own ≤ cntFrames r st
  | [], _, h => by simp at h
  | f :: st, fr, h => by
    simp at h; rcases h with rfl | h
    · simp [cntFrames]
    · have := cntVals_le_frames r h; simp [cntFrames]; omega

theorem cntFrames_le_fibers (r : Nat) : ∀ {fs : List Fiber} {f : Fiber}, f ∈ fs →
    cntFrames r f.stack ≤ cntFibers r fs
  | [], _, h => by simp at h
  | g :: fs, f, h => by
    simp at h; rcases h with rfl | h
    · simp [cntFibers]
    · have := cntFrames_le_fibers r h; simp [cntFibers]; omega

theorem vt_hdl {v : Val} (h : VT v .hdl) : ∃ r, v = .hdl r := by
  cases v <;> simp [VT] at h; exact ⟨_, rfl⟩

theorem vt_nat {v : Val} (h : VT v .nat) : ∃ n, v = .nat n := by
  cases v <;> simp [VT] at h; exact ⟨_, rfl⟩

theorem snoc_of_ne_nil {α : Type} {l : List α} (h : l ≠ []) : ∃ pre x, l = pre ++ [x] := by
  rcases List.eq_nil_or_concat l with h' | ⟨pre, x, e⟩
  · exact absurd h' h
  · exact ⟨pre, x, by simp [e]⟩

/-- A running fiber with a non-empty stack can always step: `consume` always
finds a live handle, a branch a number, and a call its callee. -/
theorem fiber_progress {P : Prog} {c : Cfg} {i : Nat} {f : Fiber} (hc : CfgOK P c)
    (hf : c.fibers[i]? = some f) (hne : f.stack ≠ []) : ∃ c', Step P c c' := by
  cases hsusp : f.susp
  case true => exact ⟨_, .resume 0 hf hsusp⟩
  case false =>
  have hfm : f ∈ c.fibers := List.mem_of_getElem? hf
  have hS : c.st.live c.st.next = false := by
    cases e : c.st.live c.st.next
    · rfl
    · have := hc.fresh _ e; omega
  obtain ⟨pre, fr, e⟩ := snoc_of_ne_nil hne
  have hst := hc.stacks f hfm
  rw [e] at hst
  cases hcode : fr.code with
  | nil => exact ⟨_, .run hf (.ret hsusp e hcode)⟩
  | cons s rest =>
    obtain ⟨G, G1, G', hfr, hs, _, _⟩ := top_inv hst hcode
    cases s with
    | assign p o =>
      rcases he : evalO pre fr c.st o with ⟨v, fr1, S1⟩
      exact ⟨_, .run hf (.assign hsusp e hcode he)⟩
    | swap p q => exact ⟨_, .run hf (.swap hsusp e hcode)⟩
    | consume o =>
      cases hs with
      | consume ho =>
      rcases he : evalO pre fr c.st o with ⟨v, fr1, S1⟩
      obtain ⟨hvt, _, _, _, _, hcnt, _⟩ := evalO_ok hfr.vals hfr.slots ho he hS
      obtain ⟨r, rfl⟩ := vt_hdl hvt
      have h1 := hcnt r
      have h2 := cntVals_le_frames r (st := f.stack) (fr := fr) (by rw [e]; simp)
      have h3 := cntFrames_le_fibers r hfm
      have h4 := hc.count r
      have hl : S1.live r = true := by
        have hc1 : (Val.hdl r).cnt r = 1 := by simp [Val.cnt]
        rw [hc1] at h1
        have h6 : 1 ≤ liveN S1 r := by omega
        cases h5 : S1.live r
        · simp [liveN, h5] at h6
        · rfl
      exact ⟨_, .run hf (.consume hsusp e hcode he hl)⟩
    | drop o =>
      rcases he : evalO pre fr c.st o with ⟨v, fr1, S1⟩
      exact ⟨_, .run hf (.drop hsusp e hcode he)⟩
    | ifz o b1 b2 =>
      cases hs with
      | ifz ho _ _ =>
      rcases he : evalO pre fr c.st o with ⟨v, fr1, S1⟩
      obtain ⟨hvt, _⟩ := evalO_ok hfr.vals hfr.slots ho he hS
      obtain ⟨n, rfl⟩ := vt_nat hvt
      exact ⟨_, .run hf (.ifz hsusp e hcode he)⟩
    | call g ss bs =>
      cases hs with
      | call hF _ _ _ => exact ⟨_, .run hf (.call hsusp e hcode hF)⟩
    | spawn g ss =>
      cases hs with
      | spawn hF _ _ _ => exact ⟨_, .run hf (.spawn 0 hsusp e hcode hF)⟩
    | yield => exact ⟨_, .run hf (.yield hsusp e hcode)⟩

/-- **Progress** (Theorem 1): a typed configuration has finished (every
fiber's stack is empty) or can step. -/
theorem progress {P : Prog} {c : Cfg} (hc : CfgOK P c) :
    (∀ f ∈ c.fibers, f.stack = []) ∨ ∃ c', Step P c c' := by
  by_cases h : ∀ f ∈ c.fibers, f.stack = []
  · exact Or.inl h
  · right
    obtain ⟨f, hfm, hne⟩ : ∃ f ∈ c.fibers, f.stack ≠ [] := by
      apply Classical.byContradiction; intro hh; apply h; intro f hf
      apply Classical.byContradiction; intro h'; exact hh ⟨f, hf, h'⟩
    obtain ⟨i, hi⟩ := List.mem_iff_getElem?.mp hfm
    exact fiber_progress hc hi hne

/-- **Type safety**: from a typed configuration, every reachable
configuration is typed and has finished or can step. -/
theorem type_safety {P : Prog} {c c' : Cfg} (hc : CfgOK P c) (h : Steps P c c') :
    CfgOK P c' ∧ ((∀ f ∈ c'.fibers, f.stack = []) ∨ ∃ c'', Step P c' c'') :=
  ⟨preservation_steps hc h, progress (preservation_steps hc h)⟩

/-! ## Theorem 2: total linear exhaustion -/

/-- The initial configuration of a program whose entry function `F` takes no
parameters: one fiber, one frame, an empty store. -/
def initCfg (F : Fn) : Cfg :=
  ⟨[⟨0, [⟨F.locals.map (fun _ => .undef), [], F.body⟩], false⟩], ⟨fun _ => false, 0⟩⟩

theorem init_ok {P : Prog} {F : Fn} (hP : ProgOK P) (hF : F ∈ P) (hs : F.sinks = [])
    (hb : F.bors = []) : CfgOK P (initCfg F) := by
  obtain ⟨G, hbody, hd⟩ := hP F hF
  have hfr : FrameOK P [] 0 ⟨F.locals.map (fun _ => .undef), [], F.body⟩ F.init := by
    refine ⟨?_, ?_, G, hbody, hd⟩
    · simp only [Fn.init, hs, List.map_nil, List.nil_append]; exact vsl_undefs _
    · simp [Fn.init, hb, SlotsOK]
  refine ⟨hP, fun f hf => ?_, fun r => ?_, fun r h => by simp [initCfg] at h, fun f hf => ?_⟩
  · simp [initCfg] at hf; subst hf
    refine ⟨fun i f e => ?_, fun i a j b la lb ha _ _ _ _ => ?_⟩
    · rcases i with _ | i
      · simp at e; subst e
        exact ⟨F.init, frameOK_congr (fun p hp => absurd hp (Nat.not_lt_zero _)) hfr⟩
      · simp at e
    · rcases i with _ | i <;> simp [slotAt] at ha
  · simp [initCfg, cntFibers, cntFrames, cntVals_undefs, liveN]
  · simp [initCfg] at hf; subst hf
    intro fr hfr v hv
    simp at hfr; subst hfr
    simp at hv; obtain ⟨_, _, rfl⟩ := hv; trivial

theorem cntFibers_empty (r : Nat) : ∀ {fs : List Fiber}, (∀ f ∈ fs, f.stack = []) →
    cntFibers r fs = 0
  | [], _ => rfl
  | f :: fs, h => by
    have h1 := cntFibers_empty r (fs := fs) (fun g hg => h g (List.mem_cons_of_mem _ hg))
    simp [cntFibers, h f (by simp), cntFrames, h1]

/-- At every reachable point each live handle is held by exactly one owned
place and no dead handle is held anywhere: a linear obligation is never
duplicated (not even across a suspension) and never silently dropped. -/
theorem linear_held_exactly_once {P : Prog} {F : Fn} {c : Cfg} (hP : ProgOK P) (hF : F ∈ P)
    (hs : F.sinks = []) (hb : F.bors = []) (h : Steps P (initCfg F) c) (r : Nat) :
    cntFibers r c.fibers = if c.st.live r then 1 else 0 :=
  (preservation_steps (init_ok hP hF hs hb) h).count r

/-- **Theorem 2 (Total Linear Exhaustion)** for this calculus: when a run of a
well-typed program finishes (every fiber's stack is empty), every handle
minted during the run has been consumed (exactly once: consuming needs it
live and kills it, and it was held exactly once all along). -/
theorem total_linear_exhaustion {P : Prog} {F : Fn} {c : Cfg} (hP : ProgOK P) (hF : F ∈ P)
    (hs : F.sinks = []) (hb : F.bors = []) (h : Steps P (initCfg F) c)
    (hdone : ∀ f ∈ c.fibers, f.stack = []) : ∀ r, c.st.live r = false := by
  intro r
  have := linear_held_exactly_once hP hF hs hb h r
  rw [cntFibers_empty r hdone] at this
  cases e : c.st.live r
  · rfl
  · simp [e] at this

/-! ## Borrow soundness on every reachable state -/

def Reach (P : Prog) (F : Fn) (c : Cfg) : Prop := Steps P (initCfg F) c

theorem reach_ok {P : Prog} {F : Fn} {c : Cfg} (hP : ProgOK P) (hF : F ∈ P) (hs : F.sinks = [])
    (hb : F.bors = []) (h : Reach P F c) : CfgOK P c :=
  preservation_steps (init_ok hP hF hs hb) h

/-- No borrow outlives its place: every slot of the frame at position `i`
points into a strictly older frame of the same fiber, at an existing owned
local whose place is initialized at the slot's type. -/
theorem no_dangling_borrow {P : Prog} {c : Cfg} (hc : CfgOK P c) {f : Fiber} (hf : f ∈ c.fibers)
    {i a : Nat} {l : Loc} (hl : slotAt f.stack i a = some l) :
    l.pos < i ∧ ∃ fr t, f.stack[l.pos]? = some fr ∧ l.x < fr.own.length ∧
      VT ((fr.get l.x).at l.path) t := by
  have hst := hc.stacks f hf
  simp only [slotAt] at hl
  cases ei : f.stack[i]? with
  | none => rw [ei] at hl; simp at hl
  | some g =>
    rw [ei] at hl; simp at hl
    obtain ⟨G, hG⟩ := hst.frames i g ei
    obtain ⟨⟨_, t⟩, _, _, h2, h3⟩ := slotsOK_get' hG.slots hl
    refine ⟨h2, ?_⟩
    have hpos := stGet_lt h3
    obtain ⟨fr, hfr⟩ : ∃ fr, f.stack[l.pos]? = some fr := ⟨_, List.getElem?_eq_getElem hpos⟩
    have : stGet f.stack l.pos l.x = fr.get l.x := by simp [stGet, hfr]
    rw [this] at h3
    exact ⟨fr, t, hfr, get_lt h3, h3⟩

/-- Alias-XOR-mutability: two distinct live `mut` borrows never overlap,
except along a reborrow chain (the younger lies inside the older, which is
frozen while the younger's frame runs). -/
theorem mut_borrows_exclusive {P : Prog} {c : Cfg} (hc : CfgOK P c) {f : Fiber} (hf : f ∈ c.fibers)
    {i a j b : Nat} {la lb : Loc} (ha : slotAt f.stack i a = some la)
    (hb : slotAt f.stack j b = some lb) (hne : i ≠ j ∨ a ≠ b) (hma : la.mu = .mut)
    (_hmb : lb.mu = .mut) (ho : LOvl la lb) :
    (i < j ∧ Pre la.path lb.path) ∨ (j < i ∧ Pre lb.path la.path) := by
  rcases (hc.stacks f hf).sound i a j b la lb ha hb hne ho (Or.inl hma) with
    ⟨h1, _, h3⟩ | ⟨h1, _, h3⟩
  · exact Or.inl ⟨h1, h3⟩
  · exact Or.inr ⟨h1, h3⟩

/-- Two `mut` borrows held by the same frame (for example two `inout`
parameters) never overlap. -/
theorem same_frame_mut_disjoint {P : Prog} {c : Cfg} (hc : CfgOK P c) {f : Fiber}
    (hf : f ∈ c.fibers) {i a b : Nat} {la lb : Loc} (ha : slotAt f.stack i a = some la)
    (hb : slotAt f.stack i b = some lb) (hne : a ≠ b) (hma : la.mu = .mut) : ¬ LOvl la lb := by
  intro ho
  rcases (hc.stacks f hf).sound i a i b la lb ha hb (Or.inr hne) ho (Or.inl hma) with
    ⟨h, _⟩ | ⟨h, _⟩ <;> omega

/-- A shared borrow never overlaps a live `mut` borrow, except the older
`mut` borrow it was reborrowed from (which is frozen meanwhile). -/
theorem shr_never_aliases_mut {P : Prog} {c : Cfg} (hc : CfgOK P c) {f : Fiber} (hf : f ∈ c.fibers)
    {i a j b : Nat} {la lb : Loc} (ha : slotAt f.stack i a = some la)
    (hb : slotAt f.stack j b = some lb) (hsa : la.mu = .shr) (hmb : lb.mu = .mut)
    (ho : LOvl la lb) : j < i ∧ Pre lb.path la.path := by
  have hne : i ≠ j ∨ a ≠ b := by
    by_cases e : i = j ∧ a = b
    · obtain ⟨rfl, rfl⟩ := e; rw [ha] at hb; cases hb; rw [hsa] at hmb; cases hmb
    · by_cases h1 : i = j
      · exact Or.inr (fun h2 => e ⟨h1, h2⟩)
      · exact Or.inl h1
  rcases (hc.stacks f hf).sound i a j b la lb ha hb hne ho (Or.inr hmb) with
    ⟨_, h2, _⟩ | ⟨h1, _, h3⟩
  · rw [hsa] at h2; cases h2
  · exact ⟨h1, h3⟩

/-- The running (top) frame's owned locals are not borrowed by anyone. -/
theorem running_frame_unaliased {P : Prog} {c : Cfg} (hc : CfgOK P c) {f : Fiber}
    (hf : f ∈ c.fibers) {pre : List Frame} {fr : Frame} (e : f.stack = pre ++ [fr])
    {i a : Nat} {l : Loc} (hl : slotAt f.stack i a = some l) : l.pos < pre.length := by
  have h1 := slot_pos_lt (hc.stacks f hf) hl
  have h2 := slotAt_lt hl
  rw [e] at h2; simp at h2; omega

/-! ## The Zero-Pin corollary -/

/-- **Zero-Pin, part 1**: a suspended fiber's stack contains no pointer into
itself: values have no pointer at all, and every borrow slot of the frame at
position `i` names an owned place of a strictly older frame `pos < i` of the
same fiber (never its own frame, a younger frame, or another fiber). The
stack is therefore position independent: a slot is a fiber-relative frame
index, and copying the whole stack to another base needs no fix-up. -/
theorem zero_pin_no_self_reference {P : Prog} {c : Cfg} (hc : CfgOK P c) {f : Fiber}
    (hf : f ∈ c.fibers) (_hs : f.susp = true) {i a : Nat} {l : Loc}
    (hl : slotAt f.stack i a = some l) : l.pos < i ∧ i < f.stack.length :=
  ⟨(no_dangling_borrow hc hf hl).1, slotAt_lt hl⟩

/-- Relocation never changes what a fiber does: stepping commutes with
moving the fiber to another base. -/
theorem fstep_reloc {P : Prog} {f f' : Fiber} {S S' : Store} {new : List Fiber} (b : Nat)
    (h : FStep P f S f' S' new) : FStep P (f.reloc b) S (f'.reloc b) S' new := by
  cases h with
  | assign h1 h2 h3 h4 => exact .assign h1 h2 h3 h4
  | swap h1 h2 h3 => exact .swap h1 h2 h3
  | consume h1 h2 h3 h4 h5 => exact .consume h1 h2 h3 h4 h5
  | drop h1 h2 h3 h4 => exact .drop h1 h2 h3 h4
  | ifz h1 h2 h3 h4 => exact .ifz h1 h2 h3 h4
  | call h1 h2 h3 h4 => exact .call h1 h2 h3 h4
  | ret h1 h2 h3 => exact .ret h1 h2 h3
  | spawn b' h1 h2 h3 h4 => exact .spawn b' h1 h2 h3 h4
  | yield h1 h2 h3 => exact .yield h1 h2 h3

/-- **Zero-Pin, part 2**: relocating any fiber (in particular a suspended
one, as a fiber pool does) by copying it to a new base keeps the whole
configuration well typed, borrow-sound and in agreement with the store. -/
theorem zero_pin_relocation {P : Prog} {c : Cfg} (hc : CfgOK P c) {i : Nat} {f : Fiber}
    (hf : c.fibers[i]? = some f) (b : Nat) :
    CfgOK P ⟨c.fibers.set i (f.reloc b), c.st⟩ := by
  have hi : i < c.fibers.length := (List.getElem?_eq_some_iff.mp hf).1
  have hfi : c.fibers[i] = f := (List.getElem?_eq_some_iff.mp hf).2
  have hfm : f ∈ c.fibers := List.mem_of_getElem? hf
  refine ⟨hc.prog, fun g hg => ?_, fun r => ?_, hc.fresh, fun g hg => ?_⟩
  · rcases List.mem_or_eq_of_mem_set hg with hg | rfl
    · exact hc.stacks g hg
    · exact hc.stacks f hfm
  · have h1 := cntFibers_set r (fs := c.fibers) (f.reloc b) hi
    rw [hfi] at h1
    have h3 := hc.count r
    simp only [Fiber.reloc] at h1 ⊢
    omega
  · rcases List.mem_or_eq_of_mem_set hg with hg | rfl
    · exact hc.freshv g hg
    · exact hc.freshv f hfm

/-! ## Worked examples -/

namespace Examples

/-- `fn inc(inout n: u64) { n = 7 }` -/
def inc : Fn := ⟨[], [(.mut, .nat)], [], [.assign ⟨.bor 0, []⟩ (.lit 7)]⟩

/-- `fn refresh(inout t: Txn) { let fresh = mint(); swap(t, fresh); consume(fresh) }`:
a linear value inside a borrowed place is exchanged, never moved out. -/
def refresh : Fn := ⟨[], [(.mut, .hdl)], [.hdl],
  [.assign ⟨.own 0, []⟩ .mint, .swap ⟨.bor 0, []⟩ ⟨.own 0, []⟩, .consume (.move ⟨.own 0, []⟩)]⟩

/-- `fn peek(in n: u64) { let m = n }`: reading through a shared borrow. -/
def peek : Fn := ⟨[], [(.shr, .nat)], [.nat], [.assign ⟨.own 0, []⟩ (.copy ⟨.bor 0, []⟩)]⟩

/-- `fn both(inout t: Txn, inout n: u64) { n = 3 }`, called on disjoint fields. -/
def both : Fn := ⟨[], [(.mut, .hdl), (.mut, .nat)], [], [.assign ⟨.bor 1, []⟩ (.lit 3)]⟩

/-- `fn worker(sink t: Txn) { consume(t) }`: a fiber entry takes owned values only. -/
def worker : Fn := ⟨[.hdl], [], [], [.consume (.move ⟨.own 0, []⟩)]⟩

/-- `fn twoN(inout a: u64, inout b: u64) {}` -/
def twoN : Fn := ⟨[], [(.mut, .nat), (.mut, .nat)], [], []⟩

/-- The entry point: a struct `x : (Txn, u64)` built field by field, disjoint
`inout x.{0, 1}` projections, a reborrowing chain of calls, a suspension while
the linear field is live, a spawned fiber that takes a handle by `sink`. -/
def main : Fn := ⟨[], [], [.pair .hdl .nat, .hdl],
  [.assign ⟨.own 0, [false]⟩ .mint,
   .assign ⟨.own 0, [true]⟩ (.lit 1),
   .call 0 [] [⟨.own 0, [true]⟩],
   .call 1 [] [⟨.own 0, [false]⟩],
   .call 2 [] [⟨.own 0, [true]⟩],
   .call 3 [] [⟨.own 0, [false]⟩, ⟨.own 0, [true]⟩],
   .yield,
   .consume (.move ⟨.own 0, [false]⟩),
   .assign ⟨.own 1, []⟩ .mint,
   .spawn 4 [⟨.own 1, []⟩]]⟩

def prog : Prog := [inc, refresh, peek, both, worker, twoN, main]

theorem inc_ok : FnOK prog inc := by
  refine ⟨_, .cons (.assign (.lit _ 7) (.bor (t0 := .nat) rfl rfl (by decide))) (.nil _), ?_⟩
  simp [Done, Fn.init, inc]

theorem refresh_ok : FnOK prog refresh := by
  refine ⟨_, .cons (.assign (.mint _) (.own (s := .moved .hdl) rfl rfl rfl trivial))
    (.cons (.swap (t := .hdl) ⟨.hdl, rfl⟩ rfl rfl (Or.inl (by simp)))
    (.cons (.consume (.move rfl)) (.nil _))), ?_⟩
  simp [Done, Fn.init, refresh, Pcg.setOwn, Sh.set, Sh.dropOK]

theorem peek_ok : FnOK prog peek := by
  refine ⟨_, .cons (.assign (.copy (t := .nat) rfl rfl) (.own (s := .moved .nat) rfl rfl rfl trivial))
    (.nil _), ?_⟩
  simp [Done, Fn.init, peek, Pcg.setOwn, Sh.set, Sh.dropOK, Ty.kind, Mode.Le]

theorem both_ok : FnOK prog both := by
  refine ⟨_, .cons (.assign (.lit _ 3) (.bor (t0 := .nat) rfl rfl (by decide))) (.nil _), ?_⟩
  simp [Done, Fn.init, both]

theorem worker_ok : FnOK prog worker := by
  refine ⟨_, .cons (.consume (.move rfl)) (.nil _), ?_⟩
  simp [Done, Fn.init, worker, Pcg.setOwn, Sh.set, Sh.dropOK]

theorem twoN_ok : FnOK prog twoN := ⟨_, .nil _, by simp [Done, Fn.init, twoN]⟩


theorem main_ok : FnOK prog main := by
  refine Exists.intro ?G ⟨?b, ?d⟩
  case b =>
    refine .cons (.assign (.mint _) (.own (s := .moved (.pair .hdl .nat)) (s0 := .moved .hdl)
      rfl rfl rfl trivial)) ?_
    refine .cons (.assign (.lit _ 1) (.own (s0 := .moved .nat) rfl rfl rfl trivial)) ?_
    refine .cons (.call (F := inc) rfl trivial ⟨rfl, fun _ => trivial, trivial⟩ (by simp [AllCompat, argList, inc])) ?_
    refine .cons (.call (F := refresh) rfl trivial ⟨rfl, fun _ => trivial, trivial⟩
      (by simp [AllCompat, argList, refresh])) ?_
    refine .cons (.call (F := peek) rfl trivial ⟨rfl, fun _ => trivial, trivial⟩
      (by simp [AllCompat, argList, peek])) ?_
    refine .cons (.call (F := both) rfl trivial ⟨rfl, fun _ => trivial, rfl, fun _ => trivial, trivial⟩
      (by simp [AllCompat, argList, both, Compat, Place.Disj, Ovl, Pre])) ?_
    refine .cons .yield ?_
    refine .cons (.consume (.move (t := .hdl) rfl)) ?_
    refine .cons (.assign (.mint _) (.own (s0 := .moved .hdl) rfl rfl rfl trivial)) ?_
    refine .cons (.spawn (F := worker) rfl rfl ⟨⟨1, rfl⟩, rfl, trivial⟩ (by simp [AllCompat, argList])) ?_
    exact .nil _
  case d =>
    simp [Done, Fn.init, main, Pcg.setOwn, Sh.set, Sh.dropOK, moveSinks, worker, Ty.kind, Mode.Le]

theorem prog_ok : ProgOK prog := by
  intro F hF
  simp [prog] at hF
  rcases hF with rfl | rfl | rfl | rfl | rfl | rfl | rfl
  · exact inc_ok
  · exact refresh_ok
  · exact peek_ok
  · exact both_ok
  · exact worker_ok
  · exact twoN_ok
  · exact main_ok

/-- Every run of the example program keeps all the invariants, and when it
finishes, both handles it minted have been consumed. -/
theorem main_exhausts {c : Cfg} (h : Reach prog main c) (hdone : ∀ f ∈ c.fibers, f.stack = []) :
    ∀ r, c.st.live r = false :=
  total_linear_exhaustion prog_ok (by simp [prog]) rfl rfl h hdone

/-! Rejected programs. -/

/-- Overlapping `inout` arguments are rejected ([T-CallProj]). -/
theorem overlapping_inout_rejected (G G' : Pcg) :
    ¬ TyS prog G (.call 5 [] [⟨.own 0, [true]⟩, ⟨.own 0, [true]⟩]) G' := by
  intro h
  cases h with
  | call hF _ _ hc =>
    simp [prog] at hF; subst hF
    simp [AllCompat, argList, twoN, Compat, Place.Disj, Ovl, Pre] at hc

/-- An `inout` argument overlapping a field passed by `sink` is rejected. -/
theorem sink_overlapping_borrow_rejected (G G' : Pcg) :
    ¬ TyS [⟨[.nat], [(.shr, .nat)], [], []⟩] G (.call 0 [⟨.own 0, []⟩] [⟨.own 0, [true]⟩]) G' := by
  intro h
  cases h with
  | call hF _ _ hc =>
    simp at hF; subst hF
    simp [AllCompat, argList, Compat, Place.Disj, Ovl, Pre] at hc

/-- A fiber entry may not take a borrow: second-class borrows never cross
into another fiber's stack ([T-Abs] premise). -/
theorem spawn_with_borrow_rejected (G G' : Pcg) : ¬ TyS prog G (.spawn 0 []) G' := by
  intro h
  cases h with
  | spawn hF hb _ _ => simp [prog] at hF; subst hF; simp [inc] at hb

/-- Nothing can be moved out of a borrow. -/
theorem move_out_of_borrow_rejected (G G' : Pcg) (b : Nat) (p : Path) (t : Ty) :
    ¬ TyO G (.move ⟨.bor b, p⟩) t G' := by
  intro h; cases h

/-- A shared (`in`) borrow cannot be written. -/
theorem write_through_shr_rejected (G G' : Pcg) (t0 : Ty) (h0 : G.slots[0]? = some (.shr, t0)) :
    ¬ TyS prog G (.assign ⟨.bor 0, []⟩ (.lit 1)) G' := by
  intro h
  cases h with
  | assign ho ha =>
    cases ho
    rcases tyAsg_inv ha with ⟨_, _, _, _, e, _⟩ | ⟨_, _, _, e, h1, _⟩
    · cases e
    · cases e; rw [h0] at h1; cases h1

/-- A linear value cannot be overwritten (the old value would be dropped). -/
theorem overwrite_linear_rejected (G G' : Pcg) (h0 : G.own[0]? = some (.full .hdl)) :
    ¬ TyS prog G (.assign ⟨.own 0, []⟩ .mint) G' := by
  intro h
  cases h with
  | assign ho ha =>
    cases ho
    rcases tyAsg_inv ha with ⟨_, _, s, s0, e, h1, h2, _, h4, _⟩ | ⟨_, _, _, e, _⟩
    · cases e; rw [h0] at h1; cases h1; simp at h2; subst h2
      simp [Sh.dropOK, Ty.kind, Mode.Le] at h4
    · cases e

/-- A function that leaves a minted handle in a local is rejected: its exit
PCG is not discardable. -/
theorem leak_rejected : ¬ FnOK prog ⟨[], [], [.hdl], [.assign ⟨.own 0, []⟩ .mint]⟩ := by
  rintro ⟨G, hb, hd⟩
  cases hb with
  | cons hs hn =>
    cases hn
    cases hs with
    | assign ho ha =>
      cases ho
      rcases tyAsg_inv ha with ⟨_, _, s, s0, e, h1, h2, _, _, rfl⟩ | ⟨_, _, _, e, _⟩
      · cases e
        simp [Fn.init] at h1; subst h1; simp at h2; subst h2
        have := hd (.full .hdl) (by simp [Pcg.setOwn, Fn.init, Sh.set])
        simp [Sh.dropOK, Ty.kind, Mode.Le] at this
      · cases e

/-- Branches that disagree on whether a linear value was consumed do not
join (the PCG join of [T-Match]). -/
theorem branch_disagreement_rejected (G2 : Pcg) :
    ¬ TyS prog ⟨[.full .hdl, .full .nat], []⟩
      (.ifz (.copy ⟨.own 1, []⟩) [.consume (.move ⟨.own 0, []⟩)] []) G2 := by
  intro h
  cases h with
  | ifz ho h1 h2 =>
    cases ho
    cases h2
    cases h1 with
    | cons hs hn =>
      cases hn
      cases hs with
      | consume ho' =>
        cases ho'

end Examples

end Vesper.Metatheory.Places


/-! ## Axiom audit: every theorem above, checked by `tests/metatheory_places.rs`. -/

#print axioms Vesper.Metatheory.Places.Mode.le_refl
#print axioms Vesper.Metatheory.Places.Mode.le_trans
#print axioms Vesper.Metatheory.Places.Mode.le_join_left
#print axioms Vesper.Metatheory.Places.Mode.le_join_right
#print axioms Vesper.Metatheory.Places.Mode.join_le
#print axioms Vesper.Metatheory.Places.Mode.join_eq_copy
#print axioms Vesper.Metatheory.Places.vt_unique
#print axioms Vesper.Metatheory.Places.vt_not_undef
#print axioms Vesper.Metatheory.Places.vs_full
#print axioms Vesper.Metatheory.Places.vt_at
#print axioms Vesper.Metatheory.Places.vt_valid
#print axioms Vesper.Metatheory.Places.at_append
#print axioms Vesper.Metatheory.Places.ty_at_append
#print axioms Vesper.Metatheory.Places.valid_of_vt_at
#print axioms Vesper.Metatheory.Places.at_set_same
#print axioms Vesper.Metatheory.Places.undef_at
#print axioms Vesper.Metatheory.Places.at_set_disj
#print axioms Vesper.Metatheory.Places.at_set_inside
#print axioms Vesper.Metatheory.Places.cnt_set
#print axioms Vesper.Metatheory.Places.allLt_at
#print axioms Vesper.Metatheory.Places.allLt_set
#print axioms Vesper.Metatheory.Places.allLt_mono
#print axioms Vesper.Metatheory.Places.cnt_allLt
#print axioms Vesper.Metatheory.Places.cnt_droppable
#print axioms Vesper.Metatheory.Places.cnt_vs_drop
#print axioms Vesper.Metatheory.Places.vt_replace
#print axioms Vesper.Metatheory.Places.vs_replace
#print axioms Vesper.Metatheory.Places.vs_at
#print axioms Vesper.Metatheory.Places.vs_valid
#print axioms Vesper.Metatheory.Places.vs_set
#print axioms Vesper.Metatheory.Places.vsl_len
#print axioms Vesper.Metatheory.Places.vsl_get
#print axioms Vesper.Metatheory.Places.vsl_set
#print axioms Vesper.Metatheory.Places.vsl_set_same
#print axioms Vesper.Metatheory.Places.frame_get_of
#print axioms Vesper.Metatheory.Places.slotsOK_len
#print axioms Vesper.Metatheory.Places.slotsOK_get
#print axioms Vesper.Metatheory.Places.slotsOK_get'
#print axioms Vesper.Metatheory.Places.slotsOK_mono
#print axioms Vesper.Metatheory.Places.stGet_congr
#print axioms Vesper.Metatheory.Places.slotOK_congr
#print axioms Vesper.Metatheory.Places.frameOK_congr
#print axioms Vesper.Metatheory.Places.getElem_opt_append_lt
#print axioms Vesper.Metatheory.Places.getElem_opt_append_len
#print axioms Vesper.Metatheory.Places.preOK_snoc
#print axioms Vesper.Metatheory.Places.bs_congr
#print axioms Vesper.Metatheory.Places.pre_refl
#print axioms Vesper.Metatheory.Places.pre_append_right
#print axioms Vesper.Metatheory.Places.pre_exists
#print axioms Vesper.Metatheory.Places.pre_trans
#print axioms Vesper.Metatheory.Places.pre_total
#print axioms Vesper.Metatheory.Places.ovl_append
#print axioms Vesper.Metatheory.Places.ovl_symm
#print axioms Vesper.Metatheory.Places.not_ovl_prefix
#print axioms Vesper.Metatheory.Places.vt_after_write
#print axioms Vesper.Metatheory.Places.frame_wr_slots
#print axioms Vesper.Metatheory.Places.frame_wr_code
#print axioms Vesper.Metatheory.Places.frame_wr_get
#print axioms Vesper.Metatheory.Places.get_lt
#print axioms Vesper.Metatheory.Places.stGet_lt
#print axioms Vesper.Metatheory.Places.stGet_set_wr
#print axioms Vesper.Metatheory.Places.slotAt_set_wr
#print axioms Vesper.Metatheory.Places.vsl_wr_vt
#print axioms Vesper.Metatheory.Places.slotsOK_mono_idx
#print axioms Vesper.Metatheory.Places.write_frame
#print axioms Vesper.Metatheory.Places.write_stack
#print axioms Vesper.Metatheory.Places.slotAt_snoc_top
#print axioms Vesper.Metatheory.Places.stack_top
#print axioms Vesper.Metatheory.Places.stack_inv
#print axioms Vesper.Metatheory.Places.read_own
#print axioms Vesper.Metatheory.Places.read_bor
#print axioms Vesper.Metatheory.Places.rd_ok
#print axioms Vesper.Metatheory.Places.vsl_setOwn
#print axioms Vesper.Metatheory.Places.setOwn_slots
#print axioms Vesper.Metatheory.Places.cntVals_set
#print axioms Vesper.Metatheory.Places.cnt_frame_wr
#print axioms Vesper.Metatheory.Places.cntFrames_set
#print axioms Vesper.Metatheory.Places.cntFrames_append
#print axioms Vesper.Metatheory.Places.cntFrames_snoc
#print axioms Vesper.Metatheory.Places.cntFibers_append
#print axioms Vesper.Metatheory.Places.cntFibers_set
#print axioms Vesper.Metatheory.Places.cntVals_append
#print axioms Vesper.Metatheory.Places.cntVals_undefs
#print axioms Vesper.Metatheory.Places.cntVals_drop
#print axioms Vesper.Metatheory.Places.allLt_droppable
#print axioms Vesper.Metatheory.Places.get_allLt
#print axioms Vesper.Metatheory.Places.wr_allLt
#print axioms Vesper.Metatheory.Places.evalO_ok
#print axioms Vesper.Metatheory.Places.fresh_set
#print axioms Vesper.Metatheory.Places.fresh_snoc
#print axioms Vesper.Metatheory.Places.slotAt_lt
#print axioms Vesper.Metatheory.Places.slot_write_ok
#print axioms Vesper.Metatheory.Places.own_write_ok
#print axioms Vesper.Metatheory.Places.top_inv
#print axioms Vesper.Metatheory.Places.read_bor_of
#print axioms Vesper.Metatheory.Places.tyAsg_inv
#print axioms Vesper.Metatheory.Places.assign_ok
#print axioms Vesper.Metatheory.Places.stGet_allLt
#print axioms Vesper.Metatheory.Places.rd_allLt
#print axioms Vesper.Metatheory.Places.rd_own_wr_disj
#print axioms Vesper.Metatheory.Places.wr_own
#print axioms Vesper.Metatheory.Places.swap_ok
#print axioms Vesper.Metatheory.Places.top_post
#print axioms Vesper.Metatheory.Places.tyB_append
#print axioms Vesper.Metatheory.Places.consume_ok
#print axioms Vesper.Metatheory.Places.drop_ok
#print axioms Vesper.Metatheory.Places.ifz_ok
#print axioms Vesper.Metatheory.Places.yield_ok
#print axioms Vesper.Metatheory.Places.slotAt_snoc_lt
#print axioms Vesper.Metatheory.Places.bs_pop
#print axioms Vesper.Metatheory.Places.ret_ok
#print axioms Vesper.Metatheory.Places.sh_at_set_disj
#print axioms Vesper.Metatheory.Places.read_setOwn_disj
#print axioms Vesper.Metatheory.Places.moveSinks_slots
#print axioms Vesper.Metatheory.Places.clearSinks_slots
#print axioms Vesper.Metatheory.Places.clearSinks_code
#print axioms Vesper.Metatheory.Places.rd_wr_disj
#print axioms Vesper.Metatheory.Places.sinksOK_own
#print axioms Vesper.Metatheory.Places.sinksOK_setOwn
#print axioms Vesper.Metatheory.Places.sinks_ok
#print axioms Vesper.Metatheory.Places.disj_symm
#print axioms Vesper.Metatheory.Places.compat_symm
#print axioms Vesper.Metatheory.Places.allCompat_append
#print axioms Vesper.Metatheory.Places.allCompat_idx
#print axioms Vesper.Metatheory.Places.sinkDisj_of
#print axioms Vesper.Metatheory.Places.args_split
#print axioms Vesper.Metatheory.Places.borsOK_len
#print axioms Vesper.Metatheory.Places.borsOK_get
#print axioms Vesper.Metatheory.Places.slots_get
#print axioms Vesper.Metatheory.Places.resolve_mu
#print axioms Vesper.Metatheory.Places.arg_slot_ok
#print axioms Vesper.Metatheory.Places.callee_slots_ok
#print axioms Vesper.Metatheory.Places.lovl_symm
#print axioms Vesper.Metatheory.Places.slot_pos_lt
#print axioms Vesper.Metatheory.Places.lovl_ext
#print axioms Vesper.Metatheory.Places.new_old
#print axioms Vesper.Metatheory.Places.new_new
#print axioms Vesper.Metatheory.Places.slotAt_push
#print axioms Vesper.Metatheory.Places.slotAt_push_top
#print axioms Vesper.Metatheory.Places.call_bs
#print axioms Vesper.Metatheory.Places.vsl_append
#print axioms Vesper.Metatheory.Places.vsl_undefs
#print axioms Vesper.Metatheory.Places.clearSinks_get
#print axioms Vesper.Metatheory.Places.clearSinks_allLt
#print axioms Vesper.Metatheory.Places.callee_allLt
#print axioms Vesper.Metatheory.Places.call_ok
#print axioms Vesper.Metatheory.Places.spawn_ok
#print axioms Vesper.Metatheory.Places.fstep_ok
#print axioms Vesper.Metatheory.Places.fresh_mono
#print axioms Vesper.Metatheory.Places.preservation
#print axioms Vesper.Metatheory.Places.preservation_steps
#print axioms Vesper.Metatheory.Places.cntVals_le_frames
#print axioms Vesper.Metatheory.Places.cntFrames_le_fibers
#print axioms Vesper.Metatheory.Places.vt_hdl
#print axioms Vesper.Metatheory.Places.vt_nat
#print axioms Vesper.Metatheory.Places.snoc_of_ne_nil
#print axioms Vesper.Metatheory.Places.fiber_progress
#print axioms Vesper.Metatheory.Places.progress
#print axioms Vesper.Metatheory.Places.type_safety
#print axioms Vesper.Metatheory.Places.init_ok
#print axioms Vesper.Metatheory.Places.cntFibers_empty
#print axioms Vesper.Metatheory.Places.linear_held_exactly_once
#print axioms Vesper.Metatheory.Places.total_linear_exhaustion
#print axioms Vesper.Metatheory.Places.reach_ok
#print axioms Vesper.Metatheory.Places.no_dangling_borrow
#print axioms Vesper.Metatheory.Places.mut_borrows_exclusive
#print axioms Vesper.Metatheory.Places.same_frame_mut_disjoint
#print axioms Vesper.Metatheory.Places.shr_never_aliases_mut
#print axioms Vesper.Metatheory.Places.running_frame_unaliased
#print axioms Vesper.Metatheory.Places.zero_pin_no_self_reference
#print axioms Vesper.Metatheory.Places.fstep_reloc
#print axioms Vesper.Metatheory.Places.zero_pin_relocation
#print axioms Vesper.Metatheory.Places.Examples.inc_ok
#print axioms Vesper.Metatheory.Places.Examples.refresh_ok
#print axioms Vesper.Metatheory.Places.Examples.peek_ok
#print axioms Vesper.Metatheory.Places.Examples.both_ok
#print axioms Vesper.Metatheory.Places.Examples.worker_ok
#print axioms Vesper.Metatheory.Places.Examples.twoN_ok
#print axioms Vesper.Metatheory.Places.Examples.main_ok
#print axioms Vesper.Metatheory.Places.Examples.prog_ok
#print axioms Vesper.Metatheory.Places.Examples.main_exhausts
#print axioms Vesper.Metatheory.Places.Examples.overlapping_inout_rejected
#print axioms Vesper.Metatheory.Places.Examples.sink_overlapping_borrow_rejected
#print axioms Vesper.Metatheory.Places.Examples.spawn_with_borrow_rejected
#print axioms Vesper.Metatheory.Places.Examples.move_out_of_borrow_rejected
#print axioms Vesper.Metatheory.Places.Examples.write_through_shr_rejected
#print axioms Vesper.Metatheory.Places.Examples.overwrite_linear_rejected
#print axioms Vesper.Metatheory.Places.Examples.leak_rejected
#print axioms Vesper.Metatheory.Places.Examples.branch_disagreement_rejected
