/-
  Strata regions (design §15.2): a Lean model of the source region rules that
  the Vesper checker implements (`src/regions.rs`, `src/regions/check.rs`).

  Model. A first-order expression language with lexical region blocks.
  Region labels are de Bruijn levels: inside `n` enclosing blocks the live
  labels are `0 .. n-1`, and a block opened there binds label `n`. The store
  is a stack of regions, one per live label, each an append-only list of
  payload values; a handle `loc r a` names payload `a` of region `r`.

  * `region e` pushes an empty region, runs `e`, and pops (frees) it:
    lexical lifetimes.
  * `box r e` appends the value of `e` to region `r`; `get r e` reads a
    handle, and the operation's label must be the handle's region (branding,
    E0792).
  * [T-RegionScope] (E0791): the type of a block's result is well formed
    without the block's own label, so no handle into the freed region can
    leave it.
  * [T-RegionAlloc] (E0401/E0797): `box r` and `get r` need `r` live, and a
    type `box r t` may only name a live label.

  The evaluator is total (fuel) and reports `wrong` for every unsafe access:
  a label that is not live, a handle of another region, a payload that does
  not exist, or an operand of the wrong shape. A use of a handle after its
  region ended reaches one of these, or, worse, silently reads a *later*
  region that reuses the label; `dangling_reads_the_next_region` shows the
  second case for an untypeable program.

  Results (all without `sorry`, `admit` or new axioms):
  * `soundness`: a well-typed expression never evaluates to `wrong`; its
    value has its type in the final store, and the final store extends the
    initial one with the same live regions (every region opened inside was
    freed again, `lexical_lifetimes`).
  * `escape_freedom`: the value of a well-typed region block holds no handle
    into the freed region (every handle label is below the block's label).
  * `escaping_box_rejected`, `cross_region_rejected`: the escape and branding
    rules reject the programs the checker rejects with E0791 and E0792.

  This is a theorem about this model of the rules, not about the Rust
  checker, the evaluator, the JIT or the generated runtime. Not modelled:
  label polymorphism of functions (`eff [r: Region]`), copy/affine/linear
  modes and `take`, `set`, watermarks and rewind (E0795/E0796), and the
  byte bound of a region.
-/
namespace Vesper.StrataRegions

/-- Payload types: natural numbers and handles into region `r`. -/
inductive Ty where
  | nat
  | box (r : Nat) (t : Ty)
  deriving DecidableEq, Repr

/-- `t` names only labels below `n` (the live regions). -/
def Ty.wf (n : Nat) : Ty → Prop
  | .nat => True
  | .box r t => r < n ∧ t.wf n

inductive Expr where
  | lit (k : Nat)
  | var (i : Nat)
  | add (a b : Expr)
  | letE (a b : Expr)
  | region (body : Expr)
  | box (r : Nat) (a : Expr)
  | get (r : Nat) (a : Expr)
  deriving Repr

inductive Val where
  | nat (k : Nat)
  | loc (r a : Nat)
  deriving DecidableEq, Repr

/-- A stack of regions, one per live label; each an append-only payload list. -/
abbrev Store := List (List Val)

def lookup (st : Store) (r a : Nat) : Option Val :=
  (st[r]?).bind (fun region => region[a]?)

inductive Res where
  | ok (v : Val) (st : Store)
  | wrong
  deriving DecidableEq, Repr

/-- Fuel-bounded evaluator: `none` when fuel runs out. -/
def eval : Nat → List Val → Store → Expr → Option Res
  | 0, _, _, _ => none
  | fuel + 1, env, st, e =>
    match e with
    | .lit k => some (.ok (.nat k) st)
    | .var i =>
      match env[i]? with
      | some v => some (.ok v st)
      | none => some .wrong
    | .add a b =>
      match eval fuel env st a with
      | some (.ok (.nat x) st1) =>
        match eval fuel env st1 b with
        | some (.ok (.nat y) st2) => some (.ok (.nat (x + y)) st2)
        | some (.ok _ _) => some .wrong
        | other => other
      | some (.ok _ _) => some .wrong
      | other => other
    | .letE a b =>
      match eval fuel env st a with
      | some (.ok v st1) => eval fuel (v :: env) st1 b
      | other => other
    | .region body =>
      match eval fuel env (st ++ [[]]) body with
      | some (.ok v st1) => some (.ok v (st1.take st.length))
      | other => other
    | .box r a =>
      match eval fuel env st a with
      | some (.ok v st1) =>
        match st1[r]? with
        | some region => some (.ok (.loc r region.length) (st1.set r (region ++ [v])))
        | none => some .wrong
      | other => other
    | .get r a =>
      match eval fuel env st a with
      | some (.ok (.loc r' addr) st1) =>
        if r' = r then
          match lookup st1 r addr with
          | some v => some (.ok v st1)
          | none => some .wrong
        else some .wrong
      | some (.ok _ _) => some .wrong
      | other => other

/-- Typing: `HasTy n Γ e t` with `n` live region labels. -/
inductive HasTy : Nat → List Ty → Expr → Ty → Prop where
  | lit : HasTy n Γ (.lit k) .nat
  | var : Γ[i]? = some t → HasTy n Γ (.var i) t
  | add : HasTy n Γ a .nat → HasTy n Γ b .nat → HasTy n Γ (.add a b) .nat
  | letE : HasTy n Γ a s → HasTy n (s :: Γ) b t → HasTy n Γ (.letE a b) t
  /-- [T-RegionScope]: the result type must not mention label `n`. -/
  | region : HasTy (n + 1) Γ body t → t.wf n → HasTy n Γ (.region body) t
  /-- [T-RegionAlloc]: allocation needs the live capability `r`. -/
  | box : r < n → HasTy n Γ a t → HasTy n Γ (.box r a) (.box r t)
  | get : HasTy n Γ a (.box r t) → HasTy n Γ (.get r a) t

/-- Semantic value typing in a store, by recursion on the type. -/
def ValTy (st : Store) : Val → Ty → Prop
  | .nat _, .nat => True
  | .loc r a, .box r' t => r = r' ∧ ∃ v, lookup st r a = some v ∧ ValTy st v t
  | _, _ => False

def EnvTy (st : Store) (env : List Val) (Γ : List Ty) : Prop :=
  ∀ (i : Nat) (t : Ty), Γ[i]? = some t → ∃ v, env[i]? = some v ∧ ValTy st v t

/-- Every payload of `st` is still there, unchanged, in `st'`. -/
def Sub (st st' : Store) : Prop :=
  ∀ r a v, lookup st r a = some v → lookup st' r a = some v

/-- `st'` extends `st` and has the same live regions. -/
def Ext (st st' : Store) : Prop := st'.length = st.length ∧ Sub st st'

theorem Sub.refl (st : Store) : Sub st st := fun _ _ _ h => h

theorem Sub.trans {a b c : Store} (h1 : Sub a b) (h2 : Sub b c) : Sub a c :=
  fun r x v h => h2 r x v (h1 r x v h)

theorem Ext.refl (st : Store) : Ext st st := ⟨rfl, Sub.refl st⟩

theorem Ext.trans {a b c : Store} (h1 : Ext a b) (h2 : Ext b c) : Ext a c :=
  ⟨h2.1.trans h1.1, Sub.trans h1.2 h2.2⟩

theorem ValTy.mono {st st' : Store} (h : Sub st st') :
    ∀ (t : Ty) (v : Val), ValTy st v t → ValTy st' v t
  | .nat, .nat _, _ => trivial
  | .nat, .loc _ _, hv => by simp [ValTy] at hv
  | .box _ _, .nat _, hv => by simp [ValTy] at hv
  | .box r' t, .loc r a, hv => by
    obtain ⟨hr, w, hl, hw⟩ := hv
    exact ⟨hr, w, h r a w hl, ValTy.mono h t w hw⟩

theorem EnvTy.mono {st st' : Store} (h : Sub st st') {env : List Val} {Γ : List Ty}
    (he : EnvTy st env Γ) : EnvTy st' env Γ := by
  intro i t hi
  obtain ⟨v, hv, ht⟩ := he i t hi
  exact ⟨v, hv, ValTy.mono h t v ht⟩

theorem EnvTy.cons {st : Store} {env : List Val} {Γ : List Ty} {v : Val} {s : Ty}
    (he : EnvTy st env Γ) (hv : ValTy st v s) : EnvTy st (v :: env) (s :: Γ) := by
  intro i t hi
  cases i with
  | zero =>
    simp at hi
    subst hi
    exact ⟨v, rfl, hv⟩
  | succ i =>
    simp at hi
    obtain ⟨w, hw, ht⟩ := he i t hi
    exact ⟨w, by simpa using hw, ht⟩

/-- A handle names a live label. -/
theorem lookup_lt {st : Store} {r a : Nat} {v : Val} (h : lookup st r a = some v) :
    r < st.length := by
  unfold lookup at h
  cases hr : st[r]? with
  | none => simp [hr] at h
  | some _ => exact (List.getElem?_eq_some_iff.mp hr).1

/-- Opening a region keeps every existing payload. -/
theorem sub_push (st : Store) : Sub st (st ++ [[]]) := by
  intro r a v h
  have hr := lookup_lt h
  unfold lookup at *
  rw [List.getElem?_append_left hr]
  exact h

/-- Freeing the top region keeps the payloads of every older region. -/
theorem lookup_take {st : Store} {n r a : Nat} (hr : r < n) :
    lookup (st.take n) r a = lookup st r a := by
  unfold lookup
  rw [List.getElem?_take, if_pos hr]

/-- A value typed by a type over labels below `n` keeps its type when the
regions from `n` up are freed. -/
theorem ValTy.take {st : Store} {n : Nat} :
    ∀ (t : Ty) (v : Val), t.wf n → ValTy st v t → ValTy (st.take n) v t
  | .nat, .nat _, _, _ => trivial
  | .nat, .loc _ _, _, hv => by simp [ValTy] at hv
  | .box _ _, .nat _, _, hv => by simp [ValTy] at hv
  | .box r' t, .loc r a, hwf, hv => by
    obtain ⟨hr, w, hl, hw⟩ := hv
    subst hr
    refine ⟨rfl, w, ?_, ValTy.take t w hwf.2 hw⟩
    rw [lookup_take hwf.1]
    exact hl

/-- Appending to region `r` keeps every payload. -/
theorem sub_append {st : Store} {r : Nat} {region : List Val} (hr : st[r]? = some region)
    (v : Val) : Sub st (st.set r (region ++ [v])) := by
  intro r' a w h
  unfold lookup at *
  by_cases he : r' = r
  · subst he
    rw [List.getElem?_set_self (List.getElem?_eq_some_iff.mp hr).1]
    rw [hr] at h
    simp only [Option.bind_some] at h ⊢
    have ha : a < region.length := (List.getElem?_eq_some_iff.mp h).1
    rw [List.getElem?_append_left ha]
    exact h
  · rw [List.getElem?_set_ne (Ne.symm he)]
    exact h

theorem lookup_appended {st : Store} {r : Nat} {region : List Val}
    (hr : st[r]? = some region) (v : Val) :
    lookup (st.set r (region ++ [v])) r region.length = some v := by
  unfold lookup
  rw [List.getElem?_set_self (List.getElem?_eq_some_iff.mp hr).1]
  simp

/-- **Soundness.** A well-typed expression, run in a store with its `n`
live regions and a well-typed environment, never goes wrong. Its value has
its type, and the final store extends the initial one with the same live
regions. -/
theorem soundness :
    ∀ (fuel : Nat) {n : Nat} {Γ : List Ty} {e : Expr} {t : Ty} {env : List Val}
      {st : Store} {res : Res},
      HasTy n Γ e t → st.length = n → EnvTy st env Γ → eval fuel env st e = some res →
      ∃ v st', res = .ok v st' ∧ Ext st st' ∧ ValTy st' v t := by
  intro fuel
  induction fuel with
  | zero => intro _ _ _ _ _ _ _ _ _ _ h; simp [eval] at h
  | succ fuel ih =>
    intro n Γ e t env st res ht hn he hev
    cases ht with
    | lit =>
      simp [eval] at hev
      subst hev
      exact ⟨_, _, rfl, Ext.refl st, trivial⟩
    | var hi =>
      obtain ⟨v, hv, hvt⟩ := he _ _ hi
      simp [eval, hv] at hev
      subst hev
      exact ⟨v, st, rfl, Ext.refl st, hvt⟩
    | add ha hb =>
      simp only [eval] at hev
      cases h1 : eval fuel env st _ with
      | none => rw [h1] at hev; simp at hev
      | some r1 =>
        obtain ⟨v1, st1, rfl, hx1, hv1⟩ := ih ha hn he h1
        cases v1 with
        | loc _ _ => simp [ValTy] at hv1
        | nat x =>
          rw [h1] at hev
          simp only at hev
          have he1 := EnvTy.mono hx1.2 he
          cases h2 : eval fuel env st1 _ with
          | none => rw [h2] at hev; simp at hev
          | some r2 =>
            obtain ⟨v2, st2, rfl, hx2, hv2⟩ := ih hb (hx1.1.trans hn) he1 h2
            cases v2 with
            | loc _ _ => simp [ValTy] at hv2
            | nat y =>
              rw [h2] at hev
              simp at hev
              subst hev
              exact ⟨_, st2, rfl, Ext.trans hx1 hx2, trivial⟩
    | letE ha hb =>
      simp only [eval] at hev
      cases h1 : eval fuel env st _ with
      | none => rw [h1] at hev; simp at hev
      | some r1 =>
        obtain ⟨v1, st1, rfl, hx1, hv1⟩ := ih ha hn he h1
        rw [h1] at hev
        simp only at hev
        have he1 := EnvTy.cons (EnvTy.mono hx1.2 he) hv1
        obtain ⟨v, st', rfl, hx, hv⟩ := ih hb (hx1.1.trans hn) he1 hev
        exact ⟨v, st', rfl, Ext.trans hx1 hx, hv⟩
    | region hb hwf =>
      simp only [eval] at hev
      cases h1 : eval fuel env (st ++ [[]]) _ with
      | none => rw [h1] at hev; simp at hev
      | some r1 =>
        have hn' : (st ++ [[]]).length = n + 1 := by simp [hn]
        obtain ⟨v1, st1, rfl, hx1, hv1⟩ :=
          ih hb hn' (EnvTy.mono (sub_push st) he) h1
        rw [h1] at hev
        simp at hev
        obtain ⟨rfl, rfl⟩ := hev
        refine ⟨v1, st1.take st.length, rfl, ⟨?_, ?_⟩, ?_⟩
        · simp [hx1.1]
        · intro r a w h
          have hr := lookup_lt h
          rw [lookup_take hr]
          exact hx1.2 r a w (sub_push st r a w h)
        · rw [hn]
          exact ValTy.take _ v1 hwf hv1
    | box hr ha =>
      rename_i r a t
      simp only [eval] at hev
      cases h1 : eval fuel env st _ with
      | none => rw [h1] at hev; simp at hev
      | some r1 =>
        obtain ⟨v1, st1, rfl, hx1, hv1⟩ := ih ha hn he h1
        rw [h1] at hev
        simp only at hev
        have hlt : r < st1.length := by rw [hx1.1, hn]; exact hr
        cases hreg : st1[r]? with
        | none => simp at hreg; omega
        | some region =>
          rw [hreg] at hev
          simp at hev
          subst hev
          have hs := sub_append hreg v1
          refine ⟨_, _, rfl, ⟨?_, Sub.trans hx1.2 hs⟩, ?_⟩
          · simp [hx1.1]
          · exact ⟨rfl, v1, lookup_appended hreg v1, ValTy.mono hs _ v1 hv1⟩
    | get ha =>
      simp only [eval] at hev
      cases h1 : eval fuel env st _ with
      | none => rw [h1] at hev; simp at hev
      | some r1 =>
        obtain ⟨v1, st1, rfl, hx1, hv1⟩ := ih ha hn he h1
        cases v1 with
        | nat _ => simp [ValTy] at hv1
        | loc r a =>
          obtain ⟨rfl, w, hl, hw⟩ := hv1
          rw [h1] at hev
          simp [hl] at hev
          subst hev
          exact ⟨w, st1, rfl, hx1, hw⟩

/-- Lexical lifetimes: running a well-typed program leaves exactly the
regions that were live before; every region opened inside it was freed. -/
theorem lexical_lifetimes {fuel n : Nat} {Γ : List Ty} {e : Expr} {t : Ty} {env : List Val}
    {st st' : Store} {v : Val} (ht : HasTy n Γ e t) (hn : st.length = n)
    (he : EnvTy st env Γ) (hev : eval fuel env st e = some (.ok v st')) :
    st'.length = n := by
  obtain ⟨_, _, h, hx, _⟩ := soundness fuel ht hn he hev
  cases h
  exact hx.1.trans hn

/-- Never goes wrong. -/
theorem never_wrong {fuel n : Nat} {Γ : List Ty} {e : Expr} {t : Ty} {env : List Val}
    {st : Store} (ht : HasTy n Γ e t) (hn : st.length = n) (he : EnvTy st env Γ) :
    eval fuel env st e ≠ some .wrong := by
  intro h
  obtain ⟨_, _, h', _, _⟩ := soundness fuel ht hn he h
  cases h'

/-- Every handle in a value of a type over labels below `n`. -/
def Val.below (n : Nat) : Val → Prop
  | .nat _ => True
  | .loc r _ => r < n

/-- **Escape freedom** ([T-RegionScope]). The value of a well-typed region
block opened with `n` live regions holds no handle into the block's region
(label `n`), which the block freed. -/
theorem escape_freedom {fuel n : Nat} {Γ : List Ty} {body : Expr} {t : Ty} {env : List Val}
    {st st' : Store} {v : Val} (ht : HasTy n Γ (.region body) t) (hn : st.length = n)
    (he : EnvTy st env Γ) (hev : eval fuel env st (.region body) = some (.ok v st')) :
    v.below n := by
  obtain ⟨_, _, h, hx, hv⟩ := soundness fuel ht hn he hev
  cases h
  cases v with
  | nat _ => trivial
  | loc r a =>
    cases t with
    | nat => simp [ValTy] at hv
    | box r' t =>
      obtain ⟨rfl, w, hl, _⟩ := hv
      have := lookup_lt hl
      rw [hx.1, hn] at this
      exact this

/-- The program `let b = region { box 0 1 } in region { let _ = box 0 2 in get 0 b }`
lets a handle escape its block (E0791). -/
def escapingProgram : Expr :=
  .letE (.region (.box 0 (.lit 1)))
    (.region (.letE (.box 0 (.lit 2)) (.get 0 (.var 1))))

/-- Without the escape rule the dangling handle silently reads the payload
of the next region that reuses label 0: a use after free that no run-time
check of this store catches. -/
theorem dangling_reads_the_next_region :
    eval 10 [] [] escapingProgram = some (.ok (.nat 2) []) := by
  decide

/-- The checker rejects it: the first block's result type mentions its own
label. -/
theorem escaping_box_rejected : ∀ t, ¬ HasTy 0 [] escapingProgram t := by
  intro t h
  cases h with
  | letE ha _ =>
    cases ha with
    | region hb hwf =>
      cases hb with
      | box _ _ => exact absurd hwf.1 (Nat.lt_irrefl 0)

/-- Branding (E0792): a box of region 0 is not accepted by an operation of
region 1. -/
theorem cross_region_rejected :
    ∀ t, ¬ HasTy 0 [] (.region (.region (.get 1 (.box 0 (.lit 7))))) t := by
  intro t h
  cases h with
  | region hb _ =>
    cases hb with
    | region hb' _ =>
      cases hb' with
      | get ha => cases ha

/-- An accepted program: nested blocks, the inner one reading a box of the
outer one, evaluate to its value. -/
def nestedProgram : Expr :=
  .region (.letE (.box 0 (.lit 40))
    (.region (.letE (.box 1 (.lit 2)) (.add (.get 0 (.var 1)) (.get 1 (.var 0))))))

theorem nested_typed : HasTy 0 [] nestedProgram .nat := by
  unfold nestedProgram
  refine .region (.letE (.box (by decide) .lit)
    (.region (.letE (.box (by decide) .lit)
      (.add (.get (.var rfl)) (.get (.var rfl)))) trivial)) trivial

theorem nested_runs : eval 20 [] [] nestedProgram = some (.ok (.nat 42) []) := by
  decide

end Vesper.StrataRegions
