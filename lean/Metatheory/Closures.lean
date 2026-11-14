/-
Vesper mechanized metatheory, wave seven (issue #338): functions, closures,
handler state, several operations per effect and effect-row variables.

This file extends the fragment of `lean/MetatheoryFull.lean` (wave six, which
it does not import or change) with:
* first-class functions: closures `clos env body` typed by [T-Abs], whose
  captured environment `env : τ_z @ m_z` must satisfy the capture premise
  `m_z ⊑ m_fn`, so that a closure capturing a linear value is linear;
  application `app f a` ([T-App]);
* stateful deep handlers `handle l : E^q state s = e_init in e_body with
  { return(x, s) => e_ret, op_i(y, s, k) => e_i }` ([T-Handle]) whose
  continuation takes the operation result and the next state,
  `k : (σ_i, τ_s) -[ε]@q-> τ_out`, so linear state threads through clauses;
* several operations per effect (`perform l.op_o(a)`, one clause per op);
* effect-row variables `ρ` in rows (Δ), with a row-instantiation theorem:
  code that is well typed with row variables stays well typed under every
  instantiation of them;
* a kinded resource store: each live store id records whether it is a linear
  handle or a continuation of mode q, so Theorem 2 can be stated per kind (a
  handle is never released by `resume`, a continuation never by `consume`).

All statements are theorems about this Lean model, not about the Rust
compiler or checker.
-/
namespace Vesper.Metatheory.Closures

/-! ## Modes -/

inductive Mode : Type where
  | copy | affine | linear
  deriving DecidableEq, Repr

namespace Mode
@[simp] def rank : Mode → Nat
  | copy => 0 | affine => 1 | linear => 2
def Le (a b : Mode) : Prop := a.rank ≤ b.rank
instance (a b : Mode) : Decidable (Le a b) := inferInstanceAs (Decidable (a.rank ≤ b.rank))
def join (a b : Mode) : Mode := if a.rank ≤ b.rank then b else a
def meet (a b : Mode) : Mode := if a.rank ≤ b.rank then a else b

theorem le_refl (a : Mode) : Le a a := Nat.le_refl _
theorem le_trans {a b c : Mode} : Le a b → Le b c → Le a c := Nat.le_trans
theorem le_antisymm {a b : Mode} : Le a b → Le b a → a = b := by
  cases a <;> cases b <;> simp [Le]
theorem copy_le (a : Mode) : Le copy a := by cases a <;> decide
theorem le_linear (a : Mode) : Le a linear := by cases a <;> decide
theorem le_join_left (a b : Mode) : Le a (join a b) := by
  cases a <;> cases b <;> decide
theorem le_join_right (a b : Mode) : Le b (join a b) := by
  cases a <;> cases b <;> decide
theorem join_le {a b c : Mode} : Le a c → Le b c → Le (join a b) c := by
  cases a <;> cases b <;> cases c <;> decide
theorem meet_le_left (a b : Mode) : Le (meet a b) a := by
  cases a <;> cases b <;> decide
theorem meet_of_le {a b : Mode} : Le a b → meet a b = a := by
  cases a <;> cases b <;> decide
theorem le_copy {a : Mode} : Le a copy → a = copy := by
  cases a <;> decide
end Mode

/-- Resumption guarantees: never copy. -/
inductive RMode : Type where
  | affine | linear
  deriving DecidableEq, Repr

namespace RMode
@[simp] def toMode : RMode → Mode
  | affine => .affine | linear => .linear
def Le (a b : RMode) : Prop := Mode.Le a.toMode b.toMode
theorem le_refl (a : RMode) : Le a a := Mode.le_refl _
theorem le_trans {a b c : RMode} : Le a b → Le b c → Le a c := Mode.le_trans
theorem affine_le (a : RMode) : Le .affine a := by cases a <;> simp [Le, Mode.Le]
theorem not_le_copy (q : RMode) : ¬ Mode.Le q.toMode .copy := by
  cases q <;> decide
theorem le_affine_iff (q : RMode) : Mode.Le q.toMode .affine ↔ q = .affine := by
  cases q <;> decide
theorem affine_le_mode (q : RMode) : Mode.Le .affine q.toMode := by
  cases q <;> decide
end RMode

/-- Store entries: a live linear handle, or a live continuation of mode q.
The kind is recorded when the id is allocated and never changes. -/
inductive Ent : Type where
  | hdl
  | kont (q : RMode)
  deriving DecidableEq, Repr

/-- The resumption/linearity mode of an entry: handles are linear. -/
def Ent.q : Ent → RMode
  | .hdl => .linear
  | .kont q => q

/-! ## Types, modal kinds and effect rows -/

inductive Label : Type where
  | abort
  | user (n : Nat)
  /-- an effect-row variable ρ (Δ ⊢ ρ : Row) -/
  | rvar (n : Nat)
  deriving DecidableEq, Repr

abbrev Row := List Label

inductive Ty : Type where
  | unit
  | nat
  /-- a linear resource handle, standing for `WriteTxn`, `NonceTicket`, ... -/
  | hdl
  /-- a two-field struct `T { f1 : t1 @@ m1, f2 : t2 @@ m2 }` with nominal floor `md` -/
  | pair (md : Mode) (t1 : Ty) (m1 : Mode) (t2 : Ty) (m2 : Mode)
  /-- a one-shot continuation `(arg, state @ ms) -[row]@q-> ans @ am` -/
  | kont (arg st : Ty) (ms : Mode) (ans : Ty) (am : Mode) (row : Row) (q : RMode)
  /-- a function `arg -[row]@mf-> ans @ am` -/
  | arrow (arg ans : Ty) (am : Mode) (row : Row) (mf : Mode)

/-- [K-Prim], [K-Adt] (`m_decl ⊔ ⊔_i (k_i ⊓ m_i)`) and [K-Fn]. -/
def Ty.kind : Ty → Mode
  | .unit => .copy
  | .nat => .copy
  | .hdl => .linear
  | .pair md t1 m1 t2 m2 => Mode.join md (Mode.join (Mode.meet t1.kind m1) (Mode.meet t2.kind m2))
  | .kont _ _ _ _ _ _ q => q.toMode
  | .arrow _ _ _ _ mf => mf

/-- Field-tree paths: `false` selects the first field, `true` the second. -/
abbrev Path := List Bool

def Ty.at : Ty → Path → Option Ty
  | t, [] => some t
  | .pair _ t1 _ _ _, false :: p => t1.at p
  | .pair _ _ _ t2 _, true :: p => t2.at p
  | _, _ :: _ => none

/-- Field-tree disjointness `p ⊥ q`: the paths diverge before either ends. -/
def PathDisj : Path → Path → Prop
  | x :: p, y :: q => if x = y then PathDisj p q else True
  | _, _ => False

/-- The result type of `get`/`swap`: the leaf, then the restored root. -/
def leafRoot (σ τ : Ty) : Ty := .pair .copy σ .linear τ .linear

/-- An effect `E` with resumption guarantee `q` and operations
`op_o : arg o -> res o` for `o < nops`. -/
structure EffSig where
  q : RMode
  nops : Nat
  arg : Nat → Ty
  res : Nat → Ty

section Sig
variable (Sig : Nat → EffSig)

/-- The resumption guarantee of a row label. A row variable may be
instantiated by any row, so it counts as affine (the bottom). -/
def qOf : Label → RMode
  | .abort => .affine
  | .user l => (Sig l).q
  | .rvar _ => .affine

/-- `res(ε) = ⊓_i q_i`, with `res(∅) = linear`. -/
def res (ε : Row) : RMode :=
  if ε.any (fun l => qOf Sig l == .affine) then .affine else .linear

theorem res_le {ε : Row} {l : Label} (h : l ∈ ε) : RMode.Le (res Sig ε) (qOf Sig l) := by
  unfold res
  cases hq : qOf Sig l
  · have : ε.any (fun l => qOf Sig l == .affine) = true :=
      List.any_eq_true.mpr ⟨l, h, by simp [hq]⟩
    simp [this, RMode.Le, Mode.Le]
  · split <;> simp [RMode.Le, Mode.Le]

theorem res_mono {ε1 ε : Row} (h : ∀ l, l ∈ ε1 → l ∈ ε) : RMode.Le (res Sig ε) (res Sig ε1) := by
  unfold res
  by_cases h1 : ε1.any (fun l => qOf Sig l == .affine) = true
  · obtain ⟨l, hl, hq⟩ := List.any_eq_true.mp h1
    have : ε.any (fun l => qOf Sig l == .affine) = true := List.any_eq_true.mpr ⟨l, h l hl, hq⟩
    rw [if_pos this, if_pos h1]; exact RMode.le_refl _
  · simp only [h1]; split <;> simp [RMode.Le, Mode.Le]

end Sig

/-! ## Syntax -/

inductive ArithOp : Type where
  | add | sub | mul
  deriving DecidableEq, Repr

def u64Bound : Nat := 2 ^ 64

/-- `vesper.arith.checked/1` on u64: exact result in range (operands and result), otherwise abort. -/
def ArithOp.eval : ArithOp → Nat → Nat → Option Nat
  | .add, a, b => if a + b < u64Bound then some (a + b) else none
  | .sub, a, b => if b ≤ a ∧ a - b < u64Bound then some (a - b) else none
  | .mul, a, b => if a * b < u64Bound then some (a * b) else none

/-- Values are a syntactic subset (`IsVal`). Operands of primitives are atoms
(variables or values): the calculus is in A-normal form. -/
inductive Expr : Type where
  | var (i : Nat)
  | unitV
  | natV (n : Nat)
  | hdlV (r : Nat)
  /-- a captured continuation with store id `r`; `body` binds the operation
  result as `var 1` and the next handler state as `var 0` -/
  | kontV (r : Nat) (body : Expr)
  /-- a closure `λx. body` with captured environment `env` (an atom; several
  captures are a struct); `body` binds `env` as `var 1` and `x` as `var 0` -/
  | clos (env body : Expr)
  | pair (a b : Expr)
  /-- `let x = e1 in e2` (binds 0 in `e2`) -/
  | letE (e1 e2 : Expr)
  /-- `match a { T(x, y) => e }` (binds `x` as 1 and `y` as 0) -/
  | letPair (a e : Expr)
  | ifz (a e1 e2 : Expr)
  | mint
  | consume (a : Expr)
  | arith (op : ArithOp) (a b : Expr)
  | abort
  /-- `perform l.op_o(a)` -/
  | perform (l o : Nat) (a : Expr)
  /-- `handle l state s in body with { return(x, s) => ret, op_o(y, s, k) => cls[o] }`;
  `ret` binds `x` as 1 and `s` as 0; each clause binds `y` as 2, `s` as 1 and
  `k` as 0; `cls` is a clause chain built from `ccons`/`cnil` -/
  | handle (l : Nat) (s body ret cls : Expr)
  | cnil
  | ccons (c rest : Expr)
  /-- `resume(k, a, s)`: resume with the operation result and the next state -/
  | resume (k a s : Expr)
  | app (f a : Expr)
  | get (a : Expr) (p : Path)
  | swap (a : Expr) (p : Path) (b : Expr)
  | swap2 (a : Expr) (p q : Path)

inductive IsVal : Expr → Prop where
  | unitV : IsVal .unitV
  | natV (n : Nat) : IsVal (.natV n)
  | hdlV (r : Nat) : IsVal (.hdlV r)
  | kontV (r : Nat) (b : Expr) : IsVal (.kontV r b)
  | clos {v : Expr} (b : Expr) : IsVal v → IsVal (.clos v b)
  | pair {a b : Expr} : IsVal a → IsVal b → IsVal (.pair a b)

inductive IsAtom : Expr → Prop where
  | var (i : Nat) : IsAtom (.var i)
  | val {v : Expr} : IsVal v → IsAtom v

/-- The `o`-th clause of a clause chain. -/
def nthC : Expr → Nat → Option Expr
  | .ccons c _, 0 => some c
  | .ccons _ cs, n + 1 => nthC cs n
  | _, _ => none

/-- Substitution of a closed value for `var k`, lowering the variables above
`k`. Values are closed (continuation and closure bodies are typed in their own
contexts), so substitution does not descend into `kontV` or closure bodies. -/
def subst (k : Nat) (v : Expr) : Expr → Expr
  | .var i => if i = k then v else if k < i then .var (i - 1) else .var i
  | .unitV => .unitV
  | .natV n => .natV n
  | .hdlV r => .hdlV r
  | .kontV r b => .kontV r b
  | .clos a b => .clos (subst k v a) b
  | .pair a b => .pair (subst k v a) (subst k v b)
  | .letE e1 e2 => .letE (subst k v e1) (subst (k + 1) v e2)
  | .letPair a e => .letPair (subst k v a) (subst (k + 2) v e)
  | .ifz a e1 e2 => .ifz (subst k v a) (subst k v e1) (subst k v e2)
  | .mint => .mint
  | .consume a => .consume (subst k v a)
  | .arith op a b => .arith op (subst k v a) (subst k v b)
  | .abort => .abort
  | .perform l o a => .perform l o (subst k v a)
  | .handle l s b r cs => .handle l (subst k v s) (subst k v b) (subst (k + 2) v r) (subst k v cs)
  | .cnil => .cnil
  | .ccons c cs => .ccons (subst (k + 3) v c) (subst k v cs)
  | .resume a b c => .resume (subst k v a) (subst k v b) (subst k v c)
  | .app f a => .app (subst k v f) (subst k v a)
  | .get a p => .get (subst k v a) p
  | .swap a p b => .swap (subst k v a) p (subst k v b)
  | .swap2 a p q => .swap2 (subst k v a) p q

theorem nthC_val {v : Expr} (hv : IsVal v) (o : Nat) : nthC v o = none := by
  cases hv <;> cases o <;> rfl

theorem nthC_subst {v : Expr} (hv : IsVal v) (k : Nat) (cs : Expr) :
    ∀ o, nthC (subst k v cs) o = (nthC cs o).map (subst (k + 3) v) := by
  induction cs generalizing k with
  | var i =>
    intro o; simp only [subst]
    split
    · rw [nthC_val hv]; cases o <;> rfl
    · split <;> cases o <;> rfl
  | ccons c cs _ ih =>
    intro o; cases o with
    | zero => rfl
    | succ o => exact ih k o
  | _ => intro o; cases o <;> rfl

/-! ## Contexts -/

/-- Γ: de Bruijn index ↦ (type, binding mode). -/
abbrev Ctx := Nat → Option (Ty × Mode)
/-- Resource contexts / stores: id ↦ kind and mode of a live handle or continuation. -/
abbrev RCtx := Nat → Option Ent

def Ctx.empty : Ctx := fun _ => none
def RCtx.empty : RCtx := fun _ => none
def RCtx.single (r : Nat) (q : Ent) : RCtx := fun x => if x = r then some q else none

def Ctx.ext (Γ : Ctx) (b : Ty × Mode) : Ctx
  | 0 => some b
  | i + 1 => Γ i

def Ctx.del (Γ : Ctx) (k : Nat) : Ctx := fun i => if i < k then Γ i else Γ (i + 1)

/-- A one-binding context. -/
def Ctx.one (b : Ty × Mode) : Ctx := Ctx.empty.ext b
/-- A two-binding context: `b1` is `var 1`, `b0` is `var 0`. -/
def Ctx.two (b1 b0 : Ty × Mode) : Ctx := (Ctx.one b1).ext b0

/-- Γ = Γ1 ⊛ Γ2: copy bindings contract into both sides, others go to one. -/
inductive SplitE : Option (Ty × Mode) → Option (Ty × Mode) → Option (Ty × Mode) → Prop where
  | none : SplitE none none none
  | left (b) : SplitE (some b) (some b) none
  | right (b) : SplitE (some b) none (some b)
  | copy (t : Ty) : SplitE (some (t, .copy)) (some (t, .copy)) (some (t, .copy))

def Split (Γ Γ1 Γ2 : Ctx) : Prop := ∀ i, SplitE (Γ i) (Γ1 i) (Γ2 i)

/-- Resources never contract. -/
def SRE (x y z : Option Ent) : Prop := (y = none ∧ x = z) ∨ (z = none ∧ x = y)

def SplitR (R R1 R2 : RCtx) : Prop := ∀ r, SRE (R r) (R1 r) (R2 r)

/-- discard(Γ): every binding is at most affine. -/
def Discard (Γ : Ctx) : Prop := ∀ i t m, Γ i = some (t, m) → Mode.Le m .affine
def DiscardR (R : RCtx) : Prop := ∀ r e, R r = some e → e.q = .affine
def AllCopy (Γ : Ctx) : Prop := ∀ i t m, Γ i = some (t, m) → m = .copy

/-- The Resumption Frame Condition: every binding/resource of the residual
frame is at most `res(ε1)`. -/
def FrameOK (Γ : Ctx) (R : RCtx) (q : RMode) : Prop :=
  (∀ i t m, Γ i = some (t, m) → Mode.Le m q.toMode) ∧ (∀ r e, R r = some e → RMode.Le e.q q)

def RowSub (ε1 ε : Row) : Prop := ∀ l, l ∈ ε1 → l ∈ ε

/-! ## Typing: Δ; Σ; Γ ⊢ e : τ @ m | ε (with the resource context R) -/

section Typing
variable (Sig : Nat → EffSig)

/-- The context of handler clause `o` of effect `l`: `y`, `s`, `k`. -/
def clauseCtx (Γh : Ctx) (l o : Nat) (τs : Ty) (ms : Mode) (τ : Ty) (m : Mode) (εc : Row) : Ctx :=
  ((Γh.ext ((Sig l).arg o, ((Sig l).arg o).kind)).ext (τs, ms)).ext
    (.kont ((Sig l).res o) τs ms τ m εc (Sig l).q, (Sig l).q.toMode)

inductive HasType : Ctx → RCtx → Expr → Ty → Mode → Row → Prop where
  /-- [T-Var] with [T-SubMode]/[T-Cross] folded in: a variable is usable at any
  mode above its type's kind; the rest of Γ is discarded. -/
  | var {Γ R i τ mx m ε} :
      Γ i = some (τ, mx) →
      (∀ j t m', j ≠ i → Γ j = some (t, m') → Mode.Le m' .affine) →
      DiscardR R → Mode.Le τ.kind m →
      HasType Γ R (.var i) τ m ε
  | unitV {Γ R m ε} : Discard Γ → DiscardR R → HasType Γ R .unitV .unit m ε
  | natV {Γ R m ε} (n : Nat) : Discard Γ → DiscardR R → HasType Γ R (.natV n) .nat m ε
  | hdlV {Γ R Rd m ε} (r : Nat) :
      Discard Γ → SplitR R (RCtx.single r .hdl) Rd → DiscardR Rd →
      Mode.Le .linear m → HasType Γ R (.hdlV r) .hdl m ε
  | kontV {Γ R R0 Rd Rb m ε σ τs ms τ am ε' q} (r : Nat) (body : Expr) :
      Discard Γ → SplitR R R0 Rd → DiscardR Rd →
      SplitR R0 (RCtx.single r (.kont q)) Rb →
      HasType (Ctx.two (σ, σ.kind) (τs, ms)) Rb body τ am ε' →
      (∀ r' e, Rb r' = some e → RMode.Le e.q q) →
      Mode.Le q.toMode m →
      HasType Γ R (.kontV r body) (.kont σ τs ms τ am ε' q) m ε
  /-- [T-Abs]: the captured environment `env : τz @ mz` satisfies the capture
  premise `mz ⊑ m_fn`; the body sees only the environment and the argument. -/
  | clos {Γ R a body τz mz σ τ am εf mf m ε} :
      IsAtom a → HasType Γ R a τz mz ε → Mode.Le mz mf →
      HasType (Ctx.two (τz, mz) (σ, σ.kind)) RCtx.empty body τ am εf →
      Mode.Le mf m →
      HasType Γ R (.clos a body) (.arrow σ τ am εf mf) m ε
  /-- struct construction; requires field-modality well-formedness `k_i ⊑ m_i` -/
  | pair {Γ Γ1 Γ2 R R1 R2 a b md t1 m1 t2 m2 m ε} :
      IsAtom a → IsAtom b → Split Γ Γ1 Γ2 → SplitR R R1 R2 →
      HasType Γ1 R1 a t1 t1.kind ε → HasType Γ2 R2 b t2 t2.kind ε →
      Mode.Le t1.kind m1 → Mode.Le t2.kind m2 →
      Mode.Le (Ty.pair md t1 m1 t2 m2).kind m →
      HasType Γ R (.pair a b) (.pair md t1 m1 t2 m2) m ε
  /-- [T-Let] with the Resumption Frame Condition over Γ2 and its resources -/
  | letE {Γ Γ1 Γ2 R R1 R2 e1 e2 τ1 m1 ε1 τ2 m2 ε2 ε} :
      Split Γ Γ1 Γ2 → SplitR R R1 R2 →
      HasType Γ1 R1 e1 τ1 m1 ε1 →
      HasType (Γ2.ext (τ1, m1)) R2 e2 τ2 m2 ε2 →
      FrameOK Γ2 R2 (res Sig ε1) →
      RowSub ε1 ε → RowSub ε2 ε →
      HasType Γ R (.letE e1 e2) τ2 m2 ε
  /-- [T-Match] for a struct: fields bind at their (crossed) kinds -/
  | letPair {Γ Γ1 Γ2 R R1 R2 a e md t1 m1 t2 m2 m0 τ m ε} :
      IsAtom a → Split Γ Γ1 Γ2 → SplitR R R1 R2 →
      HasType Γ1 R1 a (.pair md t1 m1 t2 m2) m0 ε →
      HasType ((Γ2.ext (t1, t1.kind)).ext (t2, t2.kind)) R2 e τ m ε →
      HasType Γ R (.letPair a e) τ m ε
  /-- uniform arm consumption: both arms get the same residual context -/
  | ifz {Γ Γ1 Γ2 R R1 R2 a e1 e2 τ m ε} :
      IsAtom a → Split Γ Γ1 Γ2 → SplitR R R1 R2 →
      HasType Γ1 R1 a .nat .copy ε →
      HasType Γ2 R2 e1 τ m ε → HasType Γ2 R2 e2 τ m ε →
      HasType Γ R (.ifz a e1 e2) τ m ε
  | mint {Γ R m ε} : Discard Γ → DiscardR R → Mode.Le .linear m → HasType Γ R .mint .hdl m ε
  | consume {Γ R a m ε} : IsAtom a → HasType Γ R a .hdl .linear ε →
      HasType Γ R (.consume a) .unit m ε
  /-- checked arithmetic may abort, so its row contains the affine `abort` -/
  | arith {Γ Γ1 Γ2 R R1 R2 op a b m ε} :
      IsAtom a → IsAtom b → Split Γ Γ1 Γ2 → SplitR R R1 R2 →
      HasType Γ1 R1 a .nat .copy ε → HasType Γ2 R2 b .nat .copy ε →
      Label.abort ∈ ε → HasType Γ R (.arith op a b) .nat m ε
  /-- Closed literal arithmetic whose evaluation succeeds is pure. -/
  | arithLiteral {Γ R op a b c m ε} :
      Discard Γ → DiscardR R → a < u64Bound → b < u64Bound → op.eval a b = some c →
      HasType Γ R (.arith op (.natV a) (.natV b)) .nat m ε
  | abort {Γ R τ m ε} : Discard Γ → DiscardR R → Label.abort ∈ ε → HasType Γ R .abort τ m ε
  /-- [T-Perform] for operation `o` of effect `l` -/
  | perform {Γ R l o a m ε} : o < (Sig l).nops → IsAtom a → HasType Γ R a ((Sig l).arg o) .linear ε →
      Label.user l ∈ ε → Mode.Le ((Sig l).res o).kind m →
      HasType Γ R (.perform l o a) ((Sig l).res o) m ε
  /-- [T-Handle] with state threading: `Γ = Γs ⊛ Γb ⊛ Γh` with a copy-only
  ambient context Γh, no resources in the clauses (linear resources reach them
  only through the state), a continuation `k` of mode q that takes the
  operation result and the next state, one clause per operation. The state is
  part of the body's frame: `ms ⊑ res(εc)` (it is dropped if an effect that
  escapes the handler drops the body). -/
  | handle {Γ Γs Γ' Γb Γh R Rs Rb l s body ret cls τs ms τb mb εb τ m εc ε} :
      IsAtom s → Split Γ Γs Γ' → Split Γ' Γb Γh → AllCopy Γh → SplitR R Rs Rb →
      HasType Γs Rs s τs ms εc → Mode.Le ms (res Sig εc).toMode →
      HasType Γb Rb body τb mb εb → RowSub εb (.user l :: εc) →
      HasType ((Γh.ext (τb, mb)).ext (τs, ms)) RCtx.empty ret τ m εc →
      (∀ o, o < (Sig l).nops → (nthC cls o).isSome) →
      (∀ o c, nthC cls o = some c → HasType (clauseCtx Sig Γh l o τs ms τ m εc) RCtx.empty c τ m εc) →
      RowSub εc ε →
      HasType Γ R (.handle l s body ret cls) τ m ε
  | resume {Γ Γ1 Γ2 Γ3 Γ4 R R1 R2 R3 R4 k a s σ τs ms τ m ε' q ε} :
      IsAtom k → IsAtom a → IsAtom s → Split Γ Γ1 Γ2 → Split Γ2 Γ3 Γ4 →
      SplitR R R1 R2 → SplitR R2 R3 R4 →
      HasType Γ1 R1 k (.kont σ τs ms τ m ε' q) q.toMode ε →
      HasType Γ3 R3 a σ .linear ε → HasType Γ4 R4 s τs ms ε → RowSub ε' ε →
      HasType Γ R (.resume k a s) τ m ε
  /-- [T-App]: a function of mode `mf` is used at `mf` (so a linear closure
  is called at most once) -/
  | app {Γ Γ1 Γ2 R R1 R2 f a σ τ am εf mf ε} :
      IsAtom f → IsAtom a → Split Γ Γ1 Γ2 → SplitR R R1 R2 →
      HasType Γ1 R1 f (.arrow σ τ am εf mf) mf ε → HasType Γ2 R2 a σ .linear ε →
      RowSub εf ε → HasType Γ R (.app f a) τ am ε
  /-- an `in` projection of a copy leaf; the root is restored -/
  | get {Γ R a τ ma p σ m ε} : IsAtom a → HasType Γ R a τ ma ε →
      τ.at p = some σ → σ.kind = .copy → Mode.Le (leafRoot σ τ).kind m →
      HasType Γ R (.get a p) (leafRoot σ τ) m ε
  /-- an `inout` projection: replace the leaf, return the old leaf and the root -/
  | swap {Γ Γ1 Γ2 R R1 R2 a b τ ma p σ m ε} : IsAtom a → IsAtom b →
      Split Γ Γ1 Γ2 → SplitR R R1 R2 →
      HasType Γ1 R1 a τ ma ε → τ.at p = some σ → HasType Γ2 R2 b σ .linear ε →
      Mode.Le (leafRoot σ τ).kind m →
      HasType Γ R (.swap a p b) (leafRoot σ τ) m ε
  /-- two `inout` projections of one root: [T-CallProj] demands `p ⊥ q` -/
  | swap2 {Γ R a τ ma p q σ m ε} : IsAtom a → HasType Γ R a τ ma ε →
      τ.at p = some σ → τ.at q = some σ → PathDisj p q → Mode.Le τ.kind m →
      HasType Γ R (.swap2 a p q) τ m ε

end Typing

/-! ## Context algebra -/

def RCtx.union (R1 R2 : RCtx) : RCtx := fun r =>
  match R1 r with
  | some q => some q
  | none => R2 r

def RCtx.set (R : RCtx) (r : Nat) (o : Option Ent) : RCtx := fun x => if x = r then o else R x

def REmpty (R : RCtx) : Prop := ∀ r, R r = none

/-- Resources of a context are at most mode `m`. -/
def RLeM (R : RCtx) (m : Mode) : Prop := ∀ r e, R r = some e → Mode.Le e.q.toMode m

theorem splitR_comm {R R1 R2 : RCtx} (h : SplitR R R1 R2) : SplitR R R2 R1 := by
  intro r; have := h r; unfold SRE at *; grind

theorem splitR_empty_left (R : RCtx) : SplitR R RCtx.empty R := fun _ => Or.inl ⟨rfl, rfl⟩
theorem splitR_empty_right (R : RCtx) : SplitR R R RCtx.empty := fun _ => Or.inr ⟨rfl, rfl⟩

theorem splitR_of_empty_left {R R1 R2 : RCtx} (h : SplitR R R1 R2) (h1 : REmpty R1) : ∀ r, R r = R2 r := by
  intro r; have := h r; have := h1 r; unfold SRE at *; grind

theorem splitR_of_empty_right {R R1 R2 : RCtx} (h : SplitR R R1 R2) (h2 : REmpty R2) : ∀ r, R r = R1 r := by
  intro r; have := h r; have := h2 r; unfold SRE at *; grind

theorem splitR_congr {R R' R1 R2 : RCtx} (h : SplitR R R1 R2) (he : ∀ r, R' r = R r) : SplitR R' R1 R2 := by
  intro r; rw [he r]; exact h r

/-- Reassociation: `R = (A ⊎ B) ⊎ C` gives `R = A ⊎ (B ⊎ C)`. -/
theorem splitR_assoc {R R1 R2 A B : RCtx} (h : SplitR R R1 R2) (h1 : SplitR R1 A B) :
    SplitR R A (RCtx.union B R2) ∧ SplitR (RCtx.union B R2) B R2 := by
  constructor <;> intro r <;> have := h r <;> have := h1 r <;> unfold SRE RCtx.union at * <;>
    grind

/-- Reassociation: `R = A ⊎ (B ⊎ C)` gives `R = (A ⊎ B) ⊎ C`. -/
theorem splitR_assoc' {R R1 R2 A B : RCtx} (h : SplitR R R1 R2) (h2 : SplitR R2 A B) :
    SplitR R (RCtx.union R1 A) B ∧ SplitR (RCtx.union R1 A) R1 A := by
  constructor <;> intro r <;> have := h r <;> have := h2 r <;> unfold SRE RCtx.union at * <;>
    grind

/-- Moving `B` across: `R = (A ⊎ B) ⊎ C` gives `R = (A ⊎ C) ⊎ B`. -/
theorem splitR_swap {R R1 R2 A B : RCtx} (h : SplitR R R1 R2) (h1 : SplitR R1 A B) :
    SplitR R (RCtx.union A R2) B ∧ SplitR (RCtx.union A R2) A R2 := by
  constructor <;> intro r <;> have := h r <;> have := h1 r <;> unfold SRE RCtx.union at * <;>
    grind

theorem discardR_split {R R1 R2 : RCtx} (h : SplitR R R1 R2) (hd : DiscardR R) :
    DiscardR R1 ∧ DiscardR R2 := by
  constructor <;> intro r e hr <;> have := h r <;> have := hd r e <;> unfold SRE at * <;> grind

theorem discardR_join {R R1 R2 : RCtx} (h : SplitR R R1 R2) (h1 : DiscardR R1) (h2 : DiscardR R2) :
    DiscardR R := by
  intro r e hr; have := h r; have := h1 r e; have := h2 r e; unfold SRE at *; grind

theorem discardR_empty : DiscardR RCtx.empty := fun _ _ h => by cases h

theorem rleM_split {R R1 R2 : RCtx} {m : Mode} (h : SplitR R R1 R2) (h1 : RLeM R1 m) (h2 : RLeM R2 m) :
    RLeM R m := by
  intro r q hr; have := h r; unfold SRE at *
  rcases this with ⟨_, e⟩ | ⟨_, e⟩
  · exact h2 r q (e ▸ hr)
  · exact h1 r q (e ▸ hr)

theorem rleM_part {R R1 R2 : RCtx} {m : Mode} (h : SplitR R R1 R2) (hm : RLeM R m) :
    RLeM R1 m ∧ RLeM R2 m := by
  constructor <;> intro r q hr <;> have := h r <;> unfold SRE at * <;>
    apply hm r q <;> grind

theorem rleM_mono {R : RCtx} {m m' : Mode} (h : RLeM R m) (hm : Mode.Le m m') : RLeM R m' :=
  fun r q hr => Mode.le_trans (h r q hr) hm

theorem rleM_copy {R : RCtx} (h : RLeM R .copy) : REmpty R := by
  intro r; cases hr : R r with
  | none => rfl
  | some q => exact absurd (h r q hr) (RMode.not_le_copy _)

theorem rleM_affine_discard {R : RCtx} (h : RLeM R .affine) : DiscardR R := by
  intro r e hr; exact (RMode.le_affine_iff _).mp (h r e hr)

theorem discardR_rleM {R : RCtx} (h : DiscardR R) (q : RMode) : RLeM R q.toMode := by
  intro r e hr; rw [h r e hr]; exact RMode.affine_le_mode q

theorem empty_rleM {R : RCtx} (h : REmpty R) (m : Mode) : RLeM R m := by
  intro r q hr; rw [h r] at hr; cases hr

theorem empty_discardR {R : RCtx} (h : REmpty R) : DiscardR R := by
  intro r e hr; rw [h r] at hr; cases hr

/-! Variable contexts. -/

theorem splitE_left {x y z : Option (Ty × Mode)} {b} (h : SplitE x y z) (hy : y = some b) : x = some b := by
  cases h <;> simp_all

theorem splitE_right {x y z : Option (Ty × Mode)} {b} (h : SplitE x y z) (hz : z = some b) : x = some b := by
  cases h <;> simp_all

theorem split_empty {Γ1 Γ2 : Ctx} (h : Split Ctx.empty Γ1 Γ2) : Γ1 = Ctx.empty ∧ Γ2 = Ctx.empty := by
  constructor <;> funext i <;> have := h i
  · cases h1 : Γ1 i with
    | none => rfl
    | some b => have := splitE_left this h1; simp [Ctx.empty] at this
  · cases h2 : Γ2 i with
    | none => rfl
    | some b => have := splitE_right this h2; simp [Ctx.empty] at this

theorem split_left_all (Γ : Ctx) : Split Γ Γ Ctx.empty := by
  intro i; cases h : Γ i with
  | none => exact SplitE.none
  | some b => exact SplitE.left b

theorem split_right_all (Γ : Ctx) : Split Γ Ctx.empty Γ := by
  intro i; cases h : Γ i with
  | none => exact SplitE.none
  | some b => exact SplitE.right b

theorem discard_empty : Discard Ctx.empty := fun _ _ _ h => by cases h
theorem allCopy_empty : AllCopy Ctx.empty := fun _ _ _ h => by cases h

theorem ext_del (Γ : Ctx) (b : Ty × Mode) (k : Nat) : (Γ.ext b).del (k + 1) = (Γ.del k).ext b := by
  funext i; cases i with
  | zero => simp [Ctx.del, Ctx.ext]
  | succ i =>
    simp only [Ctx.del, Ctx.ext]
    by_cases hi : i < k
    · simp [hi]
    · simp [hi]

theorem split_del {Γ Γ1 Γ2 : Ctx} (h : Split Γ Γ1 Γ2) (k : Nat) : Split (Γ.del k) (Γ1.del k) (Γ2.del k) := by
  intro i; simp only [Ctx.del]; split <;> exact h _

theorem discard_del {Γ : Ctx} (h : Discard Γ) (k : Nat) : Discard (Γ.del k) := by
  intro i t m hi; simp only [Ctx.del] at hi; split at hi <;> exact h _ _ _ hi

theorem allCopy_del {Γ : Ctx} (h : AllCopy Γ) (k : Nat) : AllCopy (Γ.del k) := by
  intro i t m hi; simp only [Ctx.del] at hi; split at hi <;> exact h _ _ _ hi

theorem discard_split {Γ Γ1 Γ2 : Ctx} (h : Split Γ Γ1 Γ2) (hd : Discard Γ) : Discard Γ1 ∧ Discard Γ2 := by
  constructor <;> intro i t m hi
  · exact hd i t m (splitE_left (h i) hi)
  · exact hd i t m (splitE_right (h i) hi)

theorem split4 {R R1 R2 A B C D : RCtx} (h : SplitR R R1 R2) (h1 : SplitR R1 A B) (h2 : SplitR R2 C D) :
    SplitR R (RCtx.union A C) (RCtx.union B D) ∧ SplitR (RCtx.union A C) A C ∧ SplitR (RCtx.union B D) B D := by
  refine ⟨?_, ?_, ?_⟩ <;> intro r <;> have := h r <;> have := h1 r <;> have := h2 r <;>
    unfold SRE RCtx.union at * <;> grind

theorem splitR_congr_left {R A A' B : RCtx} (h : SplitR R A B) (he : ∀ r, A r = A' r) : SplitR R A' B := by
  intro r; rw [← he r]; exact h r

theorem splitR_self_empty (R : RCtx) {E : RCtx} (he : REmpty E) : SplitR R R E :=
  fun r => Or.inr ⟨he r, rfl⟩

theorem rempty_eq {R : RCtx} (h : REmpty R) : R = RCtx.empty := funext h

theorem splitR_of_empty {R' R Rv : RCtx} (h : SplitR R' R Rv) (he : REmpty Rv) : R' = R := by
  funext r; exact splitR_of_empty_right h he r

theorem del_eq (Γ : Ctx) (k j : Nat) : Γ.del k j = Γ (if j < k then j else j + 1) := by
  unfold Ctx.del; split <;> rfl

theorem splitE_none {y z : Option (Ty × Mode)} (h : SplitE none y z) : y = none ∧ z = none := by
  generalize hx : (none : Option (Ty × Mode)) = x at h
  cases h <;> simp_all

theorem splitE_some {b : Ty × Mode} {y z : Option (Ty × Mode)} (h : SplitE (some b) y z) :
    (y = some b ∧ z = none) ∨ (y = none ∧ z = some b) ∨ (y = some b ∧ z = some b ∧ b.2 = .copy) := by
  generalize hx : some b = x at h
  cases h <;> simp_all

theorem split3_empty {Γ1 Γ2 Γ3 Γ4 : Ctx} (h : Split Ctx.empty Γ1 Γ2) (h' : Split Γ2 Γ3 Γ4) :
    Γ1 = Ctx.empty ∧ Γ2 = Ctx.empty ∧ Γ3 = Ctx.empty ∧ Γ4 = Ctx.empty := by
  obtain ⟨g1, g2⟩ := split_empty h; subst g1; subst g2
  obtain ⟨g3, g4⟩ := split_empty h'
  exact ⟨rfl, rfl, g3, g4⟩

/-! ## Closed value typing -/

section Values
variable {Sig : Nat → EffSig}

/-- Exact typing of closed values: `R` is precisely the resources inside. -/
inductive VT (Sig : Nat → EffSig) : RCtx → Expr → Ty → Prop where
  | unitV {R} : REmpty R → VT Sig R .unitV .unit
  | natV {R} (n : Nat) : REmpty R → VT Sig R (.natV n) .nat
  | hdlV {R} (r : Nat) : (∀ x, R x = RCtx.single r .hdl x) → VT Sig R (.hdlV r) .hdl
  | kontV {R Rb σ τs ms τ am ε' q} (r : Nat) (body : Expr) :
      SplitR R (RCtx.single r (.kont q)) Rb →
      HasType Sig (Ctx.two (σ, σ.kind) (τs, ms)) Rb body τ am ε' →
      RLeM Rb q.toMode →
      VT Sig R (.kontV r body) (.kont σ τs ms τ am ε' q)
  | clos {R v body τz mz σ τ am εf mf} :
      VT Sig R v τz → Mode.Le τz.kind mz → Mode.Le mz mf →
      HasType Sig (Ctx.two (τz, mz) (σ, σ.kind)) RCtx.empty body τ am εf →
      VT Sig R (.clos v body) (.arrow σ τ am εf mf)
  | pair {R R1 R2 a b md t1 m1 t2 m2} :
      SplitR R R1 R2 → VT Sig R1 a t1 → VT Sig R2 b t2 →
      Mode.Le t1.kind m1 → Mode.Le t2.kind m2 →
      VT Sig R (.pair a b) (.pair md t1 m1 t2 m2)

theorem vt_isVal {R v τ} (h : VT Sig R v τ) : IsVal v := by
  induction h with
  | unitV => exact .unitV
  | natV n => exact .natV n
  | hdlV r => exact .hdlV r
  | kontV r b => exact .kontV r b
  | clos _ _ _ _ ih => exact .clos _ ih
  | pair _ _ _ _ _ ih1 ih2 => exact .pair ih1 ih2

/-- Resources of a value are bounded by its type's kind. This is where the
field-modality premise `k_i ⊑ m_i` of [K-Adt] and the capture premise
`m_z ⊑ m_fn` of [T-Abs] are used. -/
theorem vt_rleM {R v τ} (h : VT Sig R v τ) : RLeM R τ.kind := by
  induction h with
  | unitV he => exact empty_rleM he _
  | natV _ he => exact empty_rleM he _
  | hdlV r he =>
    intro x q hx; rw [he x] at hx; unfold RCtx.single at hx; split at hx
    · cases hx; exact Mode.le_refl _
    · cases hx
  | kontV r b hs _ hb =>
    intro x q hx; have := hs x; unfold SRE RCtx.single at this; simp only [Ty.kind]
    split at this
    · rcases this with ⟨h1, _⟩ | ⟨_, h2⟩
      · cases h1
      · rw [hx] at h2; cases h2; exact Mode.le_refl _
    · rcases this with ⟨_, h2⟩ | ⟨_, h2⟩
      · exact hb x q (h2 ▸ hx)
      · rw [hx] at h2; cases h2
  | clos _ hk hle _ ih =>
    simp only [Ty.kind]; exact rleM_mono ih (Mode.le_trans hk hle)
  | @pair R R1 R2 a b md t1 m1 t2 m2 hs _ _ hk1 hk2 ih1 ih2 =>
    simp only [Ty.kind]
    rw [Mode.meet_of_le hk1, Mode.meet_of_le hk2]
    apply rleM_split hs
    · exact rleM_mono ih1 (Mode.le_trans (Mode.le_join_left _ _) (Mode.le_join_right _ _))
    · exact rleM_mono ih2 (Mode.le_trans (Mode.le_join_right _ _) (Mode.le_join_right _ _))

/-- Any value typing splits into its exact resources and discarded ones. -/
theorem val_inv {Γ R v τ m ε} (h : HasType Sig Γ R v τ m ε) (hv : IsVal v) :
    ∃ Rv Rd, SplitR R Rv Rd ∧ DiscardR Rd ∧ VT Sig Rv v τ ∧ Mode.Le τ.kind m := by
  induction h with
  | var => cases hv
  | unitV _ hR => exact ⟨RCtx.empty, _, splitR_empty_left _, hR, .unitV (fun _ => rfl), Mode.copy_le _⟩
  | natV n _ hR => exact ⟨RCtx.empty, _, splitR_empty_left _, hR, .natV n (fun _ => rfl), Mode.copy_le _⟩
  | hdlV r _ hs hd hm => exact ⟨_, _, hs, hd, .hdlV r (fun _ => rfl), hm⟩
  | kontV r body _ hs hd hs0 hb hle hm =>
    exact ⟨_, _, hs, hd, .kontV r body hs0 hb (fun r' q' h => hle r' q' h), hm⟩
  | clos _ _ hle hb hm ih =>
    cases hv with
    | clos _ hva =>
      obtain ⟨Rv, Rd, hs, hd, hvt, hk⟩ := ih hva
      exact ⟨Rv, Rd, hs, hd, .clos hvt hk hle hb, hm⟩
  | pair _ _ _ hsr _ _ hk1 hk2 hk ih1 ih2 =>
    cases hv with
    | pair hva hvb =>
      obtain ⟨Rv1, Rd1, hs1, hd1, hvt1, _⟩ := ih1 hva
      obtain ⟨Rv2, Rd2, hs2, hd2, hvt2, _⟩ := ih2 hvb
      obtain ⟨hA, hB, hC⟩ := split4 hsr hs1 hs2
      exact ⟨_, _, hA, discardR_join hC hd1 hd2, .pair hB hvt1 hvt2 hk1 hk2, hk⟩
  | _ => cases hv

theorem val_ty {Rv v τ} (h : VT Sig Rv v τ) :
    ∀ {Γ R Rd m ε}, Discard Γ → DiscardR Rd → SplitR R Rv Rd → Mode.Le τ.kind m →
      HasType Sig Γ R v τ m ε := by
  induction h with
  | unitV he =>
    intro Γ R Rd m ε hΓ hd hs _
    exact .unitV hΓ (discardR_join hs (empty_discardR he) hd)
  | natV n he =>
    intro Γ R Rd m ε hΓ hd hs _
    exact .natV n hΓ (discardR_join hs (empty_discardR he) hd)
  | hdlV r he =>
    intro Γ R Rd m ε hΓ hd hs hm
    exact .hdlV r hΓ (splitR_congr_left hs he) hd hm
  | kontV r b hs0 hb hle =>
    intro Γ R Rd m ε hΓ hd hs hm
    exact .kontV r b hΓ hs hd hs0 hb hle hm
  | clos hv hk hle hb ih =>
    intro Γ R Rd m ε hΓ hd hs hm
    exact .clos (.val (vt_isVal hv)) (ih hΓ hd hs hk) hle hb hm
  | pair hs12 hv1 hv2 hk1 hk2 ih1 ih2 =>
    intro Γ R Rd m ε hΓ hd hs hm
    obtain ⟨hA, hB⟩ := splitR_swap hs hs12
    exact .pair (.val (vt_isVal hv1)) (.val (vt_isVal hv2)) (split_left_all Γ) hA
      (ih1 hΓ hd hB (Mode.le_refl _))
      (ih2 discard_empty discardR_empty (splitR_empty_right _) (Mode.le_refl _))
      hk1 hk2 hm

theorem subst_val {w : Expr} (hw : IsVal w) (k : Nat) (v : Expr) : subst k v w = w := by
  induction hw with
  | pair _ _ ih1 ih2 => simp [subst, ih1, ih2]
  | clos _ _ ih => simp [subst, ih]
  | _ => rfl

end Values

/-! ## Substitution -/

section Subst
variable {Sig : Nat → EffSig}

/-- The value `v` (with resources `Rv`) may replace `var k` of Γ. -/
def SubstOK (Sig : Nat → EffSig) (Γ : Ctx) (k : Nat) (v : Expr) (Rv : RCtx) : Prop :=
  IsVal v ∧ ((Γ k = none ∧ REmpty Rv) ∨ ∃ σ mx, Γ k = some (σ, mx) ∧ VT Sig Rv v σ ∧ RLeM Rv mx)

theorem substOK_ext {Γ k v Rv} (h : SubstOK Sig Γ k v Rv) (b : Ty × Mode) :
    SubstOK Sig (Γ.ext b) (k + 1) v Rv := h

theorem substOK_discard {Γ k v Rv} (h : SubstOK Sig Γ k v Rv)
    (hd : ∀ t m, Γ k = some (t, m) → Mode.Le m .affine) : DiscardR Rv := by
  rcases h with ⟨_, ⟨_, he⟩ | ⟨σ, mx, hk, _, hle⟩⟩
  · exact empty_discardR he
  · exact rleM_affine_discard (rleM_mono hle (hd σ mx hk))

theorem substOK_split {Γ Γ1 Γ2 : Ctx} {k v Rv R R' R1 R2}
    (hs : Split Γ Γ1 Γ2) (hok : SubstOK Sig Γ k v Rv) (hR' : SplitR R' R Rv) (hR : SplitR R R1 R2) :
    ∃ Ra Rb R1' R2', SubstOK Sig Γ1 k v Ra ∧ SubstOK Sig Γ2 k v Rb ∧
      SplitR R1' R1 Ra ∧ SplitR R2' R2 Rb ∧ SplitR R' R1' R2' := by
  obtain ⟨hv, hok⟩ := hok
  have hk := hs k
  rcases hok with ⟨hn, he⟩ | ⟨σ, mx, hsome, hvt, hle⟩
  · rw [hn] at hk
    obtain ⟨h1, h2⟩ := splitE_none hk
    have := splitR_of_empty hR' he; subst this
    exact ⟨Rv, Rv, _, _, ⟨hv, Or.inl ⟨h1, he⟩⟩, ⟨hv, Or.inl ⟨h2, he⟩⟩,
      splitR_self_empty _ he, splitR_self_empty _ he, hR⟩
  · rw [hsome] at hk
    rcases splitE_some hk with ⟨h1, h2⟩ | ⟨h1, h2⟩ | ⟨h1, h2, hc⟩
    · obtain ⟨hA, hB⟩ := splitR_swap hR' hR
      exact ⟨Rv, RCtx.empty, _, R2, ⟨hv, Or.inr ⟨σ, mx, h1, hvt, hle⟩⟩,
        ⟨hv, Or.inl ⟨h2, fun _ => rfl⟩⟩, hB, splitR_empty_right _, hA⟩
    · obtain ⟨hA, hB⟩ := splitR_assoc hR' hR
      exact ⟨RCtx.empty, Rv, R1, _, ⟨hv, Or.inl ⟨h1, fun _ => rfl⟩⟩,
        ⟨hv, Or.inr ⟨σ, mx, h2, hvt, hle⟩⟩, splitR_empty_right _, hB, hA⟩
    · simp only at hc; subst hc
      have he := rleM_copy hle
      have := splitR_of_empty hR' he; subst this
      exact ⟨Rv, Rv, _, _, ⟨hv, Or.inr ⟨σ, _, h1, hvt, hle⟩⟩, ⟨hv, Or.inr ⟨σ, _, h2, hvt, hle⟩⟩,
        splitR_self_empty _ he, splitR_self_empty _ he, hR⟩

theorem substOK_handle {Γ Γb Γh : Ctx} {k v Rv}
    (hs : Split Γ Γb Γh) (hc : AllCopy Γh) (hok : SubstOK Sig Γ k v Rv) :
    SubstOK Sig Γb k v Rv ∧ SubstOK Sig Γh k v RCtx.empty := by
  obtain ⟨hv, hok⟩ := hok
  have hk := hs k
  rcases hok with ⟨hn, he⟩ | ⟨σ, mx, hsome, hvt, hle⟩
  · rw [hn] at hk
    obtain ⟨h1, h2⟩ := splitE_none hk
    exact ⟨⟨hv, Or.inl ⟨h1, he⟩⟩, ⟨hv, Or.inl ⟨h2, fun _ => rfl⟩⟩⟩
  · rw [hsome] at hk
    rcases splitE_some hk with ⟨h1, h2⟩ | ⟨h1, h2⟩ | ⟨h1, h2, hc'⟩
    · exact ⟨⟨hv, Or.inr ⟨σ, mx, h1, hvt, hle⟩⟩, ⟨hv, Or.inl ⟨h2, fun _ => rfl⟩⟩⟩
    · have := hc k σ mx h2; subst this
      have he := rleM_copy hle
      have := rempty_eq he; subst this
      exact ⟨⟨hv, Or.inl ⟨h1, he⟩⟩, ⟨hv, Or.inr ⟨σ, _, h2, hvt, hle⟩⟩⟩
    · simp only at hc'; subst hc'
      have he := rleM_copy hle
      have := rempty_eq he; subst this
      exact ⟨⟨hv, Or.inr ⟨σ, _, h1, hvt, hle⟩⟩, ⟨hv, Or.inr ⟨σ, _, h2, hvt, hle⟩⟩⟩

theorem substOK_frame {Γ2 : Ctx} {R2 R2' Rb k v q}
    (hf : FrameOK Γ2 R2 q) (hok : SubstOK Sig Γ2 k v Rb) (hs : SplitR R2' R2 Rb) :
    FrameOK (Γ2.del k) R2' q := by
  obtain ⟨hfg, hfr⟩ := hf
  refine ⟨fun i t m hi => ?_, fun r q' hr => ?_⟩
  · rw [del_eq] at hi; exact hfg _ t m hi
  · have := hs r; unfold SRE at this
    rcases this with ⟨_, e⟩ | ⟨_, e⟩
    · rcases hok with ⟨_, ⟨_, he⟩ | ⟨σ, mx, hk, _, hle⟩⟩
      · rw [he r] at e; rw [e] at hr; cases hr
      · exact Mode.le_trans (hle r q' (e ▸ hr)) (hfg k σ mx hk)
    · exact hfr r q' (e ▸ hr)

theorem isAtom_subst {a : Expr} (ha : IsAtom a) {k v} (hv : IsVal v) : IsAtom (subst k v a) := by
  cases ha with
  | var i =>
    simp only [subst]; split
    · exact .val hv
    · split <;> exact .var _
  | val hw => rw [subst_val hw]; exact .val hw

theorem discard_rest {Γ : Ctx} {Rv R R' : RCtx} {k v} (hΓ : Discard Γ) (hok : SubstOK Sig Γ k v Rv)
    (hR : DiscardR R) (hs : SplitR R' R Rv) : DiscardR R' :=
  discardR_join hs hR (substOK_discard hok (fun t m h => hΓ k t m h))

theorem clauseCtx_del (Γh : Ctx) (l o : Nat) (τs : Ty) (ms : Mode) (τ : Ty) (m : Mode) (εc : Row) (k : Nat) :
    (clauseCtx Sig Γh l o τs ms τ m εc).del (k + 3) = clauseCtx Sig (Γh.del k) l o τs ms τ m εc := by
  simp only [clauseCtx]; rw [ext_del, ext_del, ext_del]

theorem nthC_subst_some {v : Expr} (hv : IsVal v) {k cls o c'} (h : nthC (subst k v cls) o = some c') :
    ∃ c, nthC cls o = some c ∧ c' = subst (k + 3) v c := by
  rw [nthC_subst hv] at h
  cases hc : nthC cls o with
  | none => rw [hc] at h; cases h
  | some c => rw [hc] at h; cases h; exact ⟨c, rfl, rfl⟩

theorem nthC_subst_isSome {v : Expr} (hv : IsVal v) {k cls o} (h : (nthC cls o).isSome) :
    (nthC (subst k v cls) o).isSome := by
  rw [nthC_subst hv]; cases hc : nthC cls o with
  | none => rw [hc] at h; cases h
  | some c => rfl

theorem subst_ty {Γ R e τ m ε} (h : HasType Sig Γ R e τ m ε) :
    ∀ {k v Rv R'}, SubstOK Sig Γ k v Rv → SplitR R' R Rv →
      HasType Sig (Γ.del k) R' (subst k v e) τ m ε := by
  induction h with
  | @var Γ R i τ mx m ε hi hj hR hk =>
    intro k v Rv R' hok hs
    simp only [subst]
    by_cases hik : i = k
    · subst hik
      rw [if_pos rfl]
      obtain ⟨hv, ⟨hn, _⟩ | ⟨σ, mx', hsome, hvt, _⟩⟩ := hok
      · rw [hn] at hi; cases hi
      · rw [hsome] at hi; cases hi
        refine val_ty hvt ?_ hR (splitR_comm hs) hk
        intro j t m' hj'; rw [del_eq] at hj'
        exact hj _ t m' (by split <;> omega) hj'
    · rw [if_neg hik]
      have hdR : DiscardR R' := discardR_join hs hR
        (substOK_discard hok (fun t m h => hj k t m (Ne.symm hik) h))
      split
      · refine .var (mx := mx) ?_ ?_ hdR hk
        · rw [del_eq, if_neg (by omega), Nat.sub_add_cancel (by omega)]; exact hi
        · intro j t m' hne hj'; rw [del_eq] at hj'
          exact hj _ t m' (by split <;> omega) hj'
      · refine .var (mx := mx) ?_ ?_ hdR hk
        · rw [del_eq, if_pos (by omega)]; exact hi
        · intro j t m' hne hj'; rw [del_eq] at hj'
          exact hj _ t m' (by split <;> omega) hj'
  | unitV hΓ hR =>
    intro k v Rv R' hok hs
    exact .unitV (discard_del hΓ k) (discard_rest hΓ hok hR hs)
  | natV n hΓ hR =>
    intro k v Rv R' hok hs
    exact .natV n (discard_del hΓ k) (discard_rest hΓ hok hR hs)
  | arithLiteral hΓ hR ha hb hev =>
    intro k v Rv R' hok hs
    exact .arithLiteral (discard_del hΓ k) (discard_rest hΓ hok hR hs) ha hb hev
  | mint hΓ hR hm =>
    intro k v Rv R' hok hs
    exact .mint (discard_del hΓ k) (discard_rest hΓ hok hR hs) hm
  | abort hΓ hR hab =>
    intro k v Rv R' hok hs
    exact .abort (discard_del hΓ k) (discard_rest hΓ hok hR hs) hab
  | hdlV r hΓ hsR hd hm =>
    intro k v Rv R' hok hs
    obtain ⟨hA, hB⟩ := splitR_assoc hs hsR
    exact .hdlV r (discard_del hΓ k) hA
      (discardR_join hB hd (substOK_discard hok (fun t m h => hΓ k t m h))) hm
  | kontV r body hΓ hsR hd hs0 hb hle hm =>
    intro k v Rv R' hok hs
    obtain ⟨hA, hB⟩ := splitR_assoc hs hsR
    exact .kontV r body (discard_del hΓ k) hA
      (discardR_join hB hd (substOK_discard hok (fun t m h => hΓ k t m h))) hs0 hb hle hm
  | clos ha _ hle hb hm ih =>
    intro k v Rv R' hok hs
    exact .clos (isAtom_subst ha hok.1) (ih hok hs) hle hb hm
  | pair ha hb hsΓ hsR _ _ hk1 hk2 hk ih1 ih2 =>
    intro k v Rv R' hok hs
    obtain ⟨Ra, Rb, R1', R2', hoka, hokb, h1, h2, h12⟩ := substOK_split hsΓ hok hs hsR
    exact .pair (isAtom_subst ha hok.1) (isAtom_subst hb hok.1) (split_del hsΓ k) h12
      (ih1 hoka h1) (ih2 hokb h2) hk1 hk2 hk
  | letE hsΓ hsR _ _ hf h1s h2s ih1 ih2 =>
    intro k v Rv R' hok hs
    obtain ⟨Ra, Rb, R1', R2', hoka, hokb, h1, h2, h12⟩ := substOK_split hsΓ hok hs hsR
    have ih2' := ih2 (substOK_ext hokb _) h2
    rw [ext_del] at ih2'
    exact .letE (split_del hsΓ k) h12 (ih1 hoka h1) ih2' (substOK_frame hf hokb h2) h1s h2s
  | letPair ha hsΓ hsR _ _ iha ihe =>
    intro k v Rv R' hok hs
    obtain ⟨Ra, Rb, R1', R2', hoka, hokb, h1, h2, h12⟩ := substOK_split hsΓ hok hs hsR
    have ihe' := ihe (substOK_ext (substOK_ext hokb _) _) h2
    rw [ext_del, ext_del] at ihe'
    exact .letPair (isAtom_subst ha hok.1) (split_del hsΓ k) h12 (iha hoka h1) ihe'
  | ifz ha hsΓ hsR _ _ _ iha ih1 ih2 =>
    intro k v Rv R' hok hs
    obtain ⟨Ra, Rb, R1', R2', hoka, hokb, h1, h2, h12⟩ := substOK_split hsΓ hok hs hsR
    exact .ifz (isAtom_subst ha hok.1) (split_del hsΓ k) h12 (iha hoka h1) (ih1 hokb h2) (ih2 hokb h2)
  | consume ha _ ih =>
    intro k v Rv R' hok hs
    exact .consume (isAtom_subst ha hok.1) (ih hok hs)
  | arith ha hb hsΓ hsR _ _ hab iha ihb =>
    intro k v Rv R' hok hs
    obtain ⟨Ra, Rb, R1', R2', hoka, hokb, h1, h2, h12⟩ := substOK_split hsΓ hok hs hsR
    exact .arith (isAtom_subst ha hok.1) (isAtom_subst hb hok.1) (split_del hsΓ k) h12
      (iha hoka h1) (ihb hokb h2) hab
  | perform ho ha _ hl hm ih =>
    intro k v Rv R' hok hs
    exact .perform ho (isAtom_subst ha hok.1) (ih hok hs) hl hm
  | handle hsa hsΓ hsΓ' hc hsR _ hms _ hsub _ hall _ hc' ihs ihb ihr ihc =>
    intro k v Rv R' hok hs
    obtain ⟨Ra, Rb, R1', R2', hoka, hokb, h1, h2, h12⟩ := substOK_split hsΓ hok hs hsR
    obtain ⟨hokb', hokh⟩ := substOK_handle hsΓ' hc hokb
    have ihr' := ihr (substOK_ext (substOK_ext hokh _) _) (splitR_empty_left _)
    rw [ext_del, ext_del] at ihr'
    refine .handle (isAtom_subst hsa hok.1) (split_del hsΓ k) (split_del hsΓ' k) (allCopy_del hc k) h12
      (ihs hoka h1) hms (ihb hokb' h2) hsub ihr' ?_ ?_ hc'
    · intro o ho; exact nthC_subst_isSome hok.1 (hall o ho)
    · intro o c' hc''
      obtain ⟨c, hc0, rfl⟩ := nthC_subst_some hok.1 hc''
      have := ihc o c hc0 (substOK_ext (substOK_ext (substOK_ext hokh _) _) _) (splitR_empty_left _)
      rw [clauseCtx_del] at this
      exact this
  | resume hk ha hsa hsΓ hsΓ' hsR hsR' _ _ _ hsub ihk iha ihs =>
    intro k v Rv R' hok hs
    obtain ⟨Ra, Rb, R1', R2', hoka, hokb, h1, h2, h12⟩ := substOK_split hsΓ hok hs hsR
    obtain ⟨Rc, Rd, R3', R4', hokc, hokd, h3, h4, h34⟩ := substOK_split hsΓ' hokb h2 hsR'
    exact .resume (isAtom_subst hk hok.1) (isAtom_subst ha hok.1) (isAtom_subst hsa hok.1)
      (split_del hsΓ k) (split_del hsΓ' k) h12 h34 (ihk hoka h1) (iha hokc h3) (ihs hokd h4) hsub
  | app hf ha hsΓ hsR _ _ hsub ihf iha =>
    intro k v Rv R' hok hs
    obtain ⟨Ra, Rb, R1', R2', hoka, hokb, h1, h2, h12⟩ := substOK_split hsΓ hok hs hsR
    exact .app (isAtom_subst hf hok.1) (isAtom_subst ha hok.1) (split_del hsΓ k) h12
      (ihf hoka h1) (iha hokb h2) hsub
  | get ha _ hp hc hm ih =>
    intro k v Rv R' hok hs
    exact .get (isAtom_subst ha hok.1) (ih hok hs) hp hc hm
  | swap ha hb hsΓ hsR _ hp _ hm iha ihb =>
    intro k v Rv R' hok hs
    obtain ⟨Ra, Rb, R1', R2', hoka, hokb, h1, h2, h12⟩ := substOK_split hsΓ hok hs hsR
    exact .swap (isAtom_subst ha hok.1) (isAtom_subst hb hok.1) (split_del hsΓ k) h12
      (iha hoka h1) hp (ihb hokb h2) hm
  | swap2 ha _ hp hq hd hm ih =>
    intro k v Rv R' hok hs
    exact .swap2 (isAtom_subst ha hok.1) (ih hok hs) hp hq hd hm

end Subst

/-! ## Weakening by discardable resources and by larger rows -/

section Weaken
variable {Sig : Nat → EffSig}

theorem weakenR {Γ R e τ m ε} (h : HasType Sig Γ R e τ m ε) :
    ∀ {Rd R'}, DiscardR Rd → SplitR R' R Rd → HasType Sig Γ R' e τ m ε := by
  induction h with
  | var hi hj hR hk => intro Rd R' hd hs; exact .var hi hj (discardR_join hs hR hd) hk
  | unitV hΓ hR => intro Rd R' hd hs; exact .unitV hΓ (discardR_join hs hR hd)
  | natV n hΓ hR => intro Rd R' hd hs; exact .natV n hΓ (discardR_join hs hR hd)
  | arithLiteral hΓ hR ha hb hev =>
    intro Rd R' hd hs; exact .arithLiteral hΓ (discardR_join hs hR hd) ha hb hev
  | mint hΓ hR hm => intro Rd R' hd hs; exact .mint hΓ (discardR_join hs hR hd) hm
  | abort hΓ hR hab => intro Rd R' hd hs; exact .abort hΓ (discardR_join hs hR hd) hab
  | hdlV r hΓ hsR hd0 hm =>
    intro Rd R' hd hs
    obtain ⟨hA, hB⟩ := splitR_assoc hs hsR
    exact .hdlV r hΓ hA (discardR_join hB hd0 hd) hm
  | kontV r body hΓ hsR hd0 hs0 hb hle hm =>
    intro Rd R' hd hs
    obtain ⟨hA, hB⟩ := splitR_assoc hs hsR
    exact .kontV r body hΓ hA (discardR_join hB hd0 hd) hs0 hb hle hm
  | clos ha _ hle hb hm ih => intro Rd R' hd hs; exact .clos ha (ih hd hs) hle hb hm
  | pair ha hb hsΓ hsR _ h2 hk1 hk2 hk ih1 _ =>
    intro Rd R' hd hs
    obtain ⟨hA, hB⟩ := splitR_swap hs hsR
    exact .pair ha hb hsΓ hA (ih1 hd hB) h2 hk1 hk2 hk
  | letE hsΓ hsR _ h2 hf h1s h2s ih1 _ =>
    intro Rd R' hd hs
    obtain ⟨hA, hB⟩ := splitR_swap hs hsR
    exact .letE hsΓ hA (ih1 hd hB) h2 hf h1s h2s
  | letPair ha hsΓ hsR _ he iha _ =>
    intro Rd R' hd hs
    obtain ⟨hA, hB⟩ := splitR_swap hs hsR
    exact .letPair ha hsΓ hA (iha hd hB) he
  | ifz ha hsΓ hsR _ h1 h2 iha _ _ =>
    intro Rd R' hd hs
    obtain ⟨hA, hB⟩ := splitR_swap hs hsR
    exact .ifz ha hsΓ hA (iha hd hB) h1 h2
  | consume ha _ ih => intro Rd R' hd hs; exact .consume ha (ih hd hs)
  | arith ha hb hsΓ hsR _ h2 hab iha _ =>
    intro Rd R' hd hs
    obtain ⟨hA, hB⟩ := splitR_swap hs hsR
    exact .arith ha hb hsΓ hA (iha hd hB) h2 hab
  | perform ho ha _ hl hm ih => intro Rd R' hd hs; exact .perform ho ha (ih hd hs) hl hm
  | handle hsa hsΓ hsΓ' hc hsR hst hms _ hsub hr hall hcl hc' _ ihb _ _ =>
    intro Rd R' hd hs
    obtain ⟨hA, hB⟩ := splitR_assoc hs hsR
    exact .handle hsa hsΓ hsΓ' hc hA hst hms (ihb hd hB) hsub hr hall hcl hc'
  | resume hk ha hsa hsΓ hsΓ' hsR hsR' _ h2 h3 hsub ihk _ _ =>
    intro Rd R' hd hs
    obtain ⟨hA, hB⟩ := splitR_swap hs hsR
    exact .resume hk ha hsa hsΓ hsΓ' hA hsR' (ihk hd hB) h2 h3 hsub
  | app hf ha hsΓ hsR _ h2 hsub ihf _ =>
    intro Rd R' hd hs
    obtain ⟨hA, hB⟩ := splitR_swap hs hsR
    exact .app hf ha hsΓ hA (ihf hd hB) h2 hsub
  | get ha _ hp hc hm ih => intro Rd R' hd hs; exact .get ha (ih hd hs) hp hc hm
  | swap ha hb hsΓ hsR _ hp h2 hm iha _ =>
    intro Rd R' hd hs
    obtain ⟨hA, hB⟩ := splitR_swap hs hsR
    exact .swap ha hb hsΓ hA (iha hd hB) hp h2 hm
  | swap2 ha _ hp hq hdj hm ih => intro Rd R' hd hs; exact .swap2 ha (ih hd hs) hp hq hdj hm

theorem rowSub_trans {a b c : Row} (h1 : RowSub a b) (h2 : RowSub b c) : RowSub a c :=
  fun l h => h2 l (h1 l h)

theorem rowSub_refl (a : Row) : RowSub a a := fun _ h => h

theorem weakenRow {Γ R e τ m ε} (h : HasType Sig Γ R e τ m ε) :
    ∀ {ε'}, RowSub ε ε' → HasType Sig Γ R e τ m ε' := by
  induction h with
  | var hi hj hR hk => intro ε' _; exact .var hi hj hR hk
  | unitV hΓ hR => intro ε' _; exact .unitV hΓ hR
  | natV n hΓ hR => intro ε' _; exact .natV n hΓ hR
  | arithLiteral hΓ hR ha hb hev => intro ε' _; exact .arithLiteral hΓ hR ha hb hev
  | mint hΓ hR hm => intro ε' _; exact .mint hΓ hR hm
  | abort hΓ hR hab => intro ε' hε; exact .abort hΓ hR (hε _ hab)
  | hdlV r hΓ hsR hd0 hm => intro ε' _; exact .hdlV r hΓ hsR hd0 hm
  | kontV r body hΓ hsR hd0 hs0 hb hle hm => intro ε' _; exact .kontV r body hΓ hsR hd0 hs0 hb hle hm
  | clos ha _ hle hb hm ih => intro ε' hε; exact .clos ha (ih hε) hle hb hm
  | pair ha hb hsΓ hsR _ _ hk1 hk2 hk ih1 ih2 =>
    intro ε' hε; exact .pair ha hb hsΓ hsR (ih1 hε) (ih2 hε) hk1 hk2 hk
  | letE hsΓ hsR h1 h2 hf h1s h2s _ _ =>
    intro ε' hε; exact .letE hsΓ hsR h1 h2 hf (rowSub_trans h1s hε) (rowSub_trans h2s hε)
  | letPair ha hsΓ hsR _ _ iha ihe => intro ε' hε; exact .letPair ha hsΓ hsR (iha hε) (ihe hε)
  | ifz ha hsΓ hsR _ _ _ iha ih1 ih2 => intro ε' hε; exact .ifz ha hsΓ hsR (iha hε) (ih1 hε) (ih2 hε)
  | consume ha _ ih => intro ε' hε; exact .consume ha (ih hε)
  | arith ha hb hsΓ hsR _ _ hab iha ihb =>
    intro ε' hε; exact .arith ha hb hsΓ hsR (iha hε) (ihb hε) (hε _ hab)
  | perform ho ha _ hl hm ih => intro ε' hε; exact .perform ho ha (ih hε) (hε _ hl) hm
  | handle hsa hsΓ hsΓ' hc hsR hst hms hb hsub hr hall hcl hc' _ _ _ _ =>
    intro ε' hε; exact .handle hsa hsΓ hsΓ' hc hsR hst hms hb hsub hr hall hcl (rowSub_trans hc' hε)
  | resume hk ha hsa hsΓ hsΓ' hsR hsR' _ _ _ hsub ihk iha ihs =>
    intro ε' hε
    exact .resume hk ha hsa hsΓ hsΓ' hsR hsR' (ihk hε) (iha hε) (ihs hε) (rowSub_trans hsub hε)
  | app hf ha hsΓ hsR _ _ hsub ihf iha =>
    intro ε' hε; exact .app hf ha hsΓ hsR (ihf hε) (iha hε) (rowSub_trans hsub hε)
  | get ha _ hp hc hm ih => intro ε' hε; exact .get ha (ih hε) hp hc hm
  | swap ha hb hsΓ hsR _ hp _ hm iha ihb => intro ε' hε; exact .swap ha hb hsΓ hsR (iha hε) hp (ihb hε) hm
  | swap2 ha _ hp hq hdj hm ih => intro ε' hε; exact .swap2 ha (ih hε) hp hq hdj hm

end Weaken

/-! ## Field-tree places on values -/

def vAt : Expr → Path → Option Expr
  | v, [] => some v
  | v, b :: p =>
    match v, b with
    | .pair a _, false => vAt a p
    | .pair _ c, true => vAt c p
    | _, _ => none

def vSet : Expr → Path → Expr → Option Expr
  | _, [], w => some w
  | v, b :: p, w =>
    match v, b with
    | .pair a c, false => (vSet a p w).map (fun a' => .pair a' c)
    | .pair a c, true => (vSet c p w).map (fun c' => .pair a c')
    | _, _ => none

/-- Exchange the leaf of `u` at `p` with the leaf of `w` at `q`. -/
def xchg (u : Expr) (p : Path) (w : Expr) (q : Path) : Option (Expr × Expr) :=
  match vAt u p, vAt w q with
  | some lu, some lw =>
    match vSet u p lw, vSet w q lu with
    | some u', some w' => some (u', w')
    | _, _ => none
  | _, _ => none

/-- Two simultaneous `inout` projections of one root exchanging their leaves. -/
def swap2V : Expr → Path → Path → Option Expr
  | .pair a c, x :: p, y :: q =>
    if x = y then
      (if x then (swap2V c p q).map (fun c' => .pair a c')
       else (swap2V a p q).map (fun a' => .pair a' c))
    else if x then (xchg c p a q).map (fun r => .pair r.2 r.1)
    else (xchg a p c q).map (fun r => .pair r.1 r.2)
  | _, _, _ => none

section Places
variable {Sig : Nat → EffSig}

theorem splitR_rot {R X Y A B : RCtx} (h : SplitR R X Y) (h1 : SplitR Y A B) :
    SplitR R A (RCtx.union X B) ∧ SplitR (RCtx.union X B) X B := by
  constructor <;> intro r <;> have := h r <;> have := h1 r <;> unfold SRE RCtx.union at * <;> grind

theorem vt_at {p : Path} : ∀ {R v τ σ}, VT Sig R v τ → τ.at p = some σ →
    ∃ w Rw Rr, vAt v p = some w ∧ VT Sig Rw w σ ∧ SplitR R Rw Rr ∧
      ∀ u Ru R', VT Sig Ru u σ → SplitR R' Rr Ru → ∃ v', vSet v p u = some v' ∧ VT Sig R' v' τ := by
  induction p with
  | nil =>
    intro R v τ σ hv hp
    simp only [Ty.at] at hp; cases hp
    refine ⟨v, R, RCtx.empty, by simp [vAt], hv, splitR_empty_right _, ?_⟩
    intro u Ru R' hu hs
    have : R' = Ru := funext (splitR_of_empty_left hs (fun _ => rfl))
    subst this; exact ⟨u, by simp [vSet], hu⟩
  | cons b p ih =>
    intro R v τ σ hv hp
    cases hv with
    | unitV => simp [Ty.at] at hp
    | natV => simp [Ty.at] at hp
    | hdlV => simp [Ty.at] at hp
    | kontV => simp [Ty.at] at hp
    | clos => simp [Ty.at] at hp
    | pair hs h1 h2 hk1 hk2 =>
      cases b with
      | false =>
        simp only [Ty.at] at hp
        obtain ⟨w, Rw, Rr, ha, hw, hsw, hset⟩ := ih h1 hp
        obtain ⟨hA, hB⟩ := splitR_assoc hs hsw
        refine ⟨w, Rw, _, ha, hw, hA, ?_⟩
        intro u Ru R' hu hs'
        obtain ⟨hC, hD⟩ := splitR_swap hs' hB
        obtain ⟨a', ha', hva'⟩ := hset u Ru _ hu hD
        exact ⟨_, by simp [vSet, ha'], .pair hC hva' h2 hk1 hk2⟩
      | true =>
        simp only [Ty.at] at hp
        obtain ⟨w, Rw, Rr, ha, hw, hsw, hset⟩ := ih h2 hp
        obtain ⟨hA, hB⟩ := splitR_rot hs hsw
        refine ⟨w, Rw, _, ha, hw, hA, ?_⟩
        intro u Ru R' hu hs'
        obtain ⟨hC, hD⟩ := splitR_assoc hs' hB
        obtain ⟨b', hb', hvb'⟩ := hset u Ru _ hu hD
        exact ⟨_, by simp [vSet, hb'], .pair hC h1 hvb' hk1 hk2⟩

theorem vt_xchg {u w : Expr} {p q : Path} {R Ru Rw t t' σ}
    (hu : VT Sig Ru u t) (hw : VT Sig Rw w t') (hp : t.at p = some σ) (hq : t'.at q = some σ)
    (hR : SplitR R Ru Rw) :
    ∃ u' w' Ru' Rw', xchg u p w q = some (u', w') ∧ VT Sig Ru' u' t ∧ VT Sig Rw' w' t' ∧
      SplitR R Ru' Rw' := by
  obtain ⟨lu, Ra, Rb, hau, hlu, hsu, hsetu⟩ := vt_at hu hp
  obtain ⟨lw, Rc, Rd, haw, hlw, hsw, hsetw⟩ := vt_at hw hq
  have hdisj : ∀ r, SRE (R r) (Ru r) (Rw r) := hR
  obtain ⟨u', hu', hvu'⟩ := hsetu lw Rc (RCtx.union Rb Rc) hlw (by
    intro r; have := hdisj r; have := hsu r; have := hsw r; unfold SRE RCtx.union at *; grind)
  obtain ⟨w', hw', hvw'⟩ := hsetw lu Ra (RCtx.union Rd Ra) hlu (by
    intro r; have := hdisj r; have := hsu r; have := hsw r; unfold SRE RCtx.union at *; grind)
  refine ⟨u', w', _, _, by simp [xchg, hau, haw, hu', hw'], hvu', hvw', ?_⟩
  intro r
  have := hdisj r; have := hsu r; have := hsw r
  unfold SRE RCtx.union at *; grind

theorem vt_swap2 {p : Path} : ∀ {q R v τ σ}, VT Sig R v τ → τ.at p = some σ → τ.at q = some σ →
    PathDisj p q → ∃ v', swap2V v p q = some v' ∧ VT Sig R v' τ := by
  induction p with
  | nil => intro q R v τ σ _ _ _ hd; simp [PathDisj] at hd
  | cons x p ih =>
    intro q R v τ σ hv hp hq hd
    cases q with
    | nil => simp [PathDisj] at hd
    | cons y q =>
      cases hv with
      | unitV => simp [Ty.at] at hp
      | natV => simp [Ty.at] at hp
      | hdlV => simp [Ty.at] at hp
      | kontV => simp [Ty.at] at hp
      | clos => simp [Ty.at] at hp
      | pair hs h1 h2 hk1 hk2 =>
        by_cases hxy : x = y
        · subst hxy
          simp only [PathDisj] at hd
          cases x with
          | false =>
            simp only [Ty.at] at hp hq
            obtain ⟨a', ha', hva'⟩ := ih h1 hp hq hd
            exact ⟨_, by simp [swap2V, ha'], .pair hs hva' h2 hk1 hk2⟩
          | true =>
            simp only [Ty.at] at hp hq
            obtain ⟨c', hc', hvc'⟩ := ih h2 hp hq hd
            exact ⟨_, by simp [swap2V, hc'], .pair hs h1 hvc' hk1 hk2⟩
        · cases x <;> cases y
          · exact absurd rfl hxy
          · simp only [Ty.at] at hp hq
            obtain ⟨u', w', Ru', Rw', hx, hvu, hvw, hsplit⟩ := vt_xchg h1 h2 hp hq hs
            exact ⟨_, by simp [swap2V, hx], .pair hsplit hvu hvw hk1 hk2⟩
          · simp only [Ty.at] at hp hq
            obtain ⟨u', w', Ru', Rw', hx, hvu, hvw, hsplit⟩ := vt_xchg h2 h1 hp hq (splitR_comm hs)
            exact ⟨_, by simp [swap2V, hx], .pair (splitR_comm hsplit) hvw hvu hk1 hk2⟩
          · exact absurd rfl hxy

theorem bind_map_l (o : Option Expr) (d b : Expr) (q : Path) :
    ((o.map fun c' => Expr.pair c' d).bind fun v' => vSet v' (false :: q) b) =
      (o.bind fun c' => vSet c' q b).map fun c' => Expr.pair c' d := by
  cases o <;> simp [vSet]

theorem bind_map_r (o : Option Expr) (c b : Expr) (q : Path) :
    ((o.map fun d' => Expr.pair c d').bind fun v' => vSet v' (true :: q) b) =
      (o.bind fun d' => vSet d' q b).map fun d' => Expr.pair c d' := by
  cases o <;> simp [vSet]

/-- Writes through disjoint places commute: the semantic content of the
[T-CallProj] premise `p_i ⊥ p_j`. -/
theorem vSet_comm_disjoint {p : Path} : ∀ {q : Path} {v a b : Expr}, PathDisj p q →
    ((vSet v p a).bind fun v' => vSet v' q b) = ((vSet v q b).bind fun v' => vSet v' p a) := by
  induction p with
  | nil => intro q v a b hd; simp [PathDisj] at hd
  | cons x p ih =>
    intro q v a b hd
    cases q with
    | nil => simp [PathDisj] at hd
    | cons y q =>
      cases v with
      | pair c d =>
        by_cases hxy : x = y
        · subst hxy
          simp only [PathDisj] at hd
          cases x with
          | false =>
            show ((vSet c p a).map fun c' => Expr.pair c' d).bind (fun v' => vSet v' (false :: q) b) =
              ((vSet c q b).map fun c' => Expr.pair c' d).bind (fun v' => vSet v' (false :: p) a)
            rw [bind_map_l, bind_map_l, ih hd]
          | true =>
            show ((vSet d p a).map fun d' => Expr.pair c d').bind (fun v' => vSet v' (true :: q) b) =
              ((vSet d q b).map fun d' => Expr.pair c d').bind (fun v' => vSet v' (true :: p) a)
            rw [bind_map_r, bind_map_r, ih hd]
        · cases x <;> cases y
          · exact absurd rfl hxy
          · cases h1 : vSet c p a <;> cases h2 : vSet d q b <;> simp [vSet, h1, h2]
          · cases h1 : vSet d p a <;> cases h2 : vSet c q b <;> simp [vSet, h1, h2]
          · exact absurd rfl hxy
      | _ => simp [vSet]

/-- Overlapping places do not commute, which is why [T-CallProj] rejects them. -/
theorem vSet_overlap_not_comm :
    ((vSet (.pair (.pair (.natV 1) (.natV 2)) (.natV 3)) [false] (.natV 9)).bind
        fun v' => vSet v' [false, true] (.natV 7)) ≠
      ((vSet (.pair (.pair (.natV 1) (.natV 2)) (.natV 3)) [false, true] (.natV 7)).bind
        fun v' => vSet v' [false] (.natV 9)) := by
  simp [vSet]

theorem overlap_not_disj : ¬ PathDisj [false] [false, true] := by
  simp [PathDisj]

end Places

/-! ## Evaluation contexts and decomposition -/

inductive EC : Type where
  | hole
  | letC (E : EC) (e2 : Expr)
  | handleC (l : Nat) (s : Expr) (E : EC) (ret cls : Expr)

def plug : EC → Expr → Expr
  | .hole, e => e
  | .letC E e2, e => .letE (plug E e) e2
  | .handleC l s E r cs, e => .handle l s (plug E e) r cs

/-- `E` has no handler for `lab` between its hole and its root. -/
def EC.Free (lab : Label) : EC → Prop
  | .hole => True
  | .letC E _ => E.Free lab
  | .handleC l _ E _ _ => lab ≠ .user l ∧ E.Free lab

section Decompose
variable {Sig : Nat → EffSig}

theorem frame_rleM {Γ R q} (h : FrameOK Γ R q) : RLeM R q.toMode := h.2

theorem atom_closed {R a τ m ε} (ha : IsAtom a) (h : HasType Sig Ctx.empty R a τ m ε) : IsVal a := by
  cases ha with
  | var i => cases h with | var hi => simp [Ctx.empty] at hi
  | val hv => exact hv

/-- The resources of a handler's state are bounded by every label of the
clause row: they are part of the body's frame. -/
theorem state_rleM {Rs s τs ms εc lab} (hsa : IsAtom s) (hst : HasType Sig Ctx.empty Rs s τs ms εc)
    (hms : Mode.Le ms (res Sig εc).toMode) (hl : lab ∈ εc) : RLeM Rs (qOf Sig lab).toMode := by
  obtain ⟨Rv, Rd, hs, hd, hvt, hk⟩ := val_inv hst (atom_closed hsa hst)
  refine rleM_split hs ?_ (discardR_rleM hd _)
  exact rleM_mono (vt_rleM hvt) (Mode.le_trans hk (Mode.le_trans hms (res_le Sig hl)))

/-- Inversion of a closed term in an evaluation context: the hole's typing,
plugging any other term of the same type, and the frame condition along `E`. -/
theorem plug_inv (E : EC) : ∀ {R e τ m ε}, HasType Sig Ctx.empty R (plug E e) τ m ε →
    ∃ R1 R2 τh mh εh, SplitR R R1 R2 ∧ HasType Sig Ctx.empty R1 e τh mh εh ∧
      (∀ Γ' R1' R' e', HasType Sig Γ' R1' e' τh mh εh → SplitR R' R1' R2 →
        HasType Sig Γ' R' (plug E e') τ m ε) ∧
      (∀ lab, E.Free lab → lab ∈ εh → lab ∈ ε ∧ RLeM R2 (qOf Sig lab).toMode) := by
  induction E with
  | hole =>
    intro R e τ m ε h
    refine ⟨R, RCtx.empty, τ, m, ε, splitR_empty_right _, h, ?_, ?_⟩
    · intro Γ' R1' R' e' h' hs
      have : R' = R1' := funext (splitR_of_empty_right hs (fun _ => rfl))
      subst this; exact h'
    · intro lab _ hl
      exact ⟨hl, empty_rleM (fun _ => rfl) _⟩
  | letC E e2 ih =>
    intro R e τ m ε h
    cases h with
    | letE hsΓ hsR h1 h2 hf h1s h2s =>
      obtain ⟨hΓ1, hΓ2⟩ := split_empty hsΓ
      subst hΓ1; subst hΓ2
      obtain ⟨R1, R2, τh, mh, εh, hs, hh, hrepl, hfr⟩ := ih h1
      obtain ⟨hA, hB⟩ := splitR_assoc hsR hs
      refine ⟨R1, _, τh, mh, εh, hA, hh, ?_, ?_⟩
      · intro Γ' R1' R' e' h' hs'
        obtain ⟨hC, hD⟩ := splitR_assoc' hs' hB
        exact .letE (split_left_all Γ') hC (hrepl Γ' R1' _ e' h' hD) h2 hf h1s h2s
      · intro lab hfree hl
        obtain ⟨hl1, hle⟩ := hfr lab hfree hl
        refine ⟨h1s lab hl1, rleM_split hB hle ?_⟩
        exact rleM_mono (frame_rleM hf) (res_le Sig hl1)
  | handleC l s E r cs ih =>
    intro R e τ m ε h
    cases h with
    | handle hsa hsΓ hsΓ' hc hsR hst hms hb hsub hr hall hcl hc' =>
      obtain ⟨g1, g2, g3, g4⟩ := split3_empty hsΓ hsΓ'
      subst g1; subst g2; subst g3; subst g4
      obtain ⟨R1, R2, τh, mh, εh, hs, hh, hrepl, hfr⟩ := ih hb
      obtain ⟨hA, hB⟩ := splitR_assoc (splitR_comm hsR) hs
      refine ⟨R1, _, τh, mh, εh, hA, hh, ?_, ?_⟩
      · intro Γ' R1' R' e' h' hs'
        obtain ⟨hC, hD⟩ := splitR_assoc' hs' hB
        exact .handle hsa (split_right_all Γ') (split_left_all Γ') allCopy_empty (splitR_comm hC) hst hms
          (hrepl Γ' R1' _ e' h' hD) hsub hr hall hcl hc'
      · intro lab hfree hl
        obtain ⟨hne, hfree'⟩ := hfree
        obtain ⟨hl1, hle⟩ := hfr lab hfree' hl
        have := hsub lab hl1
        simp only [List.mem_cons] at this
        rcases this with h | h
        · exact absurd h hne
        · exact ⟨hc' lab h, rleM_split hB hle (state_rleM hsa hst hms h)⟩

end Decompose

/-! ## Small-step semantics over configurations -/

structure Conf where
  e : Expr
  live : RCtx
  next : Nat

inductive Act : Type where
  | silent
  | mint (r : Nat)
  | consume (r : Nat)
  | capture (r : Nat) (q : RMode)
  | resume (r : Nat)
  deriving DecidableEq, Repr

/-- The continuation captured by `perform l.op(v)` in `E` under the handler
`handle l state _ … with ret, cls`: the deep handler is reinstalled around
`E[var 1]`, with the next state `var 0`. -/
def kontBody (l : Nat) (E : EC) (ret cls : Expr) : Expr :=
  .handle l (.var 0) (plug E (.var 1)) ret cls

section Semantics
variable (Sig : Nat → EffSig)

inductive Head : Expr → RCtx → Nat → Act → Expr → RCtx → Nat → Prop where
  | letV {v e2 L n} : IsVal v → Head (.letE v e2) L n .silent (subst 0 v e2) L n
  | letAbort {e2 L n} : Head (.letE .abort e2) L n .silent .abort L n
  | letPairV {a b e L n} : IsVal a → IsVal b →
      Head (.letPair (.pair a b) e) L n .silent (subst 0 a (subst 0 b e)) L n
  | ifzZ {e1 e2 L n} : Head (.ifz (.natV 0) e1 e2) L n .silent e1 L n
  | ifzS {k e1 e2 L n} : Head (.ifz (.natV (k + 1)) e1 e2) L n .silent e2 L n
  | mint {L n} : Head .mint L n (.mint n) (.hdlV n) (L.set n (some .hdl)) (n + 1)
  /-- consuming requires a live *handle* -/
  | consume {r L n} : L r = some .hdl →
      Head (.consume (.hdlV r)) L n (.consume r) .unitV (L.set r none) n
  | arithOk {op a b c L n} : op.eval a b = some c →
      Head (.arith op (.natV a) (.natV b)) L n .silent (.natV c) L n
  | arithAbort {op a b L n} : op.eval a b = none →
      Head (.arith op (.natV a) (.natV b)) L n .silent .abort L n
  | handleRet {l s v ret cls L n} : IsVal s → IsVal v →
      Head (.handle l s v ret cls) L n .silent (subst 0 v (subst 0 s ret)) L n
  | handleAbort {l s ret cls L n} : Head (.handle l s .abort ret cls) L n .silent .abort L n
  /-- capture the delimited continuation up to the innermost `l` handler and
  run the clause of operation `o` with the argument, the current state and
  the continuation -/
  | capture {l o E v s ret cls c L n} : E.Free (.user l) → IsVal v → IsVal s → nthC cls o = some c →
      Head (.handle l s (plug E (.perform l o v)) ret cls) L n (.capture n (Sig l).q)
        (subst 0 v (subst 0 s (subst 0 (.kontV n (kontBody l E ret cls)) c)))
        (L.set n (some (.kont (Sig l).q))) (n + 1)
  /-- one-shot resumption: the id must be a live *continuation*, and dies -/
  | resume {r body w s q L n} : L r = some (.kont q) → IsVal w → IsVal s →
      Head (.resume (.kontV r body) w s) L n (.resume r) (subst 0 w (subst 0 s body)) (L.set r none) n
  /-- β for closures: the argument, then the environment -/
  | app {v body w L n} : IsVal v → IsVal w →
      Head (.app (.clos v body) w) L n .silent (subst 0 v (subst 0 w body)) L n
  | get {v p w L n} : IsVal v → vAt v p = some w → Head (.get v p) L n .silent (.pair w v) L n
  | swap {v p b w v' L n} : IsVal v → IsVal b → vAt v p = some w → vSet v p b = some v' →
      Head (.swap v p b) L n .silent (.pair w v') L n
  | swap2 {v p q v' L n} : IsVal v → swap2V v p q = some v' →
      Head (.swap2 v p q) L n .silent v' L n

inductive Step : Conf → Act → Conf → Prop where
  | ctx {e L n a e' L' n'} (E : EC) : Head Sig e L n a e' L' n' →
      Step ⟨plug E e, L, n⟩ a ⟨plug E e', L', n'⟩

inductive Steps : Conf → List Act → Conf → Prop where
  | refl (c : Conf) : Steps c [] c
  | cons {c a c1 acts c'} : Step Sig c a c1 → Steps c1 acts c' → Steps c (a :: acts) c'

end Semantics

/-- Store ids at or above `n` are unallocated. -/
def Fresh (L : RCtx) (n : Nat) : Prop := ∀ r, n ≤ r → L r = none

/-- Well-typed configuration: the whole store types the closed term. -/
def WT (Sig : Nat → EffSig) (c : Conf) (τ : Ty) (m : Mode) (ε : Row) : Prop :=
  HasType Sig Ctx.empty c.live c.e τ m ε ∧ Fresh c.live c.next

/-! ## Preservation (Theorem 1, first half) -/

section Preservation
variable {Sig : Nat → EffSig}

theorem ext_del_zero (Γ : Ctx) (b : Ty × Mode) : (Γ.ext b).del 0 = Γ := by
  funext i; simp [Ctx.del, Ctx.ext]

theorem two_del_zero (b1 b0 : Ty × Mode) : (Ctx.two b1 b0).del 0 = Ctx.one b1 := ext_del_zero _ _

theorem one_del_zero (b : Ty × Mode) : (Ctx.one b).del 0 = Ctx.empty := ext_del_zero _ _

theorem regroup {R A B X Y : RCtx} (h : SplitR R A B) (h1 : SplitR A X Y) :
    SplitR (RCtx.union B X) B X ∧ SplitR R (RCtx.union B X) Y := by
  constructor <;> intro r <;> have := h r <;> have := h1 r <;> unfold SRE RCtx.union at * <;> grind

theorem nat_discard {R n τ m ε} (h : HasType Sig Ctx.empty R (.natV n) τ m ε) : DiscardR R := by
  obtain ⟨Rv, Rd, hs, hd, hvt, _⟩ := val_inv h (.natV n)
  cases hvt with
  | natV _ he => exact discardR_join hs (empty_discardR he) hd

theorem splitR_left_of_empty {R E : RCtx} (he : REmpty E) : SplitR R E R :=
  fun r => Or.inl ⟨he r, rfl⟩

/-- `SubstOK` for a closed value at its exact resources, bound at a mode above its kind. -/
theorem substOK_val {Γ : Ctx} {Rv v τ mx} (hvt : VT Sig Rv v τ) (hk : Mode.Le τ.kind mx)
    (hΓ : Γ 0 = some (τ, mx)) : SubstOK Sig Γ 0 v Rv :=
  ⟨vt_isVal hvt, Or.inr ⟨τ, mx, hΓ, hvt, rleM_mono (vt_rleM hvt) hk⟩⟩

/-- The binding at `var 0` only, of a two-binding context. -/
def Ctx.only0 (b : Ty × Mode) : Ctx := Ctx.one b
/-- The binding at `var 1` only. -/
def Ctx.only1 (b : Ty × Mode) : Ctx := fun i => if i = 1 then some b else none

theorem split_two (b1 b0 : Ty × Mode) : Split (Ctx.two b1 b0) (Ctx.only0 b0) (Ctx.only1 b1) := by
  intro i
  match i with
  | 0 => exact .left _
  | 1 => exact .right _
  | i + 2 => exact .none

theorem var_only0 {b : Ty × Mode} {m ε} (hk : Mode.Le b.1.kind m) :
    HasType Sig (Ctx.only0 b) RCtx.empty (.var 0) b.1 m ε :=
  .var (mx := b.2) rfl (fun j t m' hj h => by
      cases j with
      | zero => exact absurd rfl hj
      | succ j => simp [Ctx.only0, Ctx.one, Ctx.ext, Ctx.empty] at h)
    discardR_empty hk

theorem var_only1 {b : Ty × Mode} {m ε} (hk : Mode.Le b.1.kind m) :
    HasType Sig (Ctx.only1 b) RCtx.empty (.var 1) b.1 m ε :=
  .var (mx := b.2) (by simp [Ctx.only1]) (fun j t m' hj h => by
      simp only [Ctx.only1] at h; split at h
      · exact absurd (by assumption) hj
      · cases h)
    discardR_empty hk

/-- Resource bookkeeping of a capture step, in a clean context for `grind`. -/
theorem capture_splits {R1 Rs Rb Rsv Rsd Rp RE Rvv Rvd : RCtx} {n : Nat} (X : Ent) (hn : R1 n = none)
    (h1 : SplitR R1 Rs Rb) (h2 : SplitR Rs Rsv Rsd) (h3 : SplitR Rb Rp RE) (h4 : SplitR Rp Rvv Rvd) :
    SplitR (RCtx.union (RCtx.single n X) RE) (RCtx.single n X) RE ∧
    SplitR (RCtx.union (RCtx.union (RCtx.single n X) RE) Rsv)
      (RCtx.union (RCtx.single n X) RE) Rsv ∧
    SplitR (RCtx.union (RCtx.union (RCtx.union (RCtx.single n X) RE) Rsv) Rvv)
      (RCtx.union (RCtx.union (RCtx.single n X) RE) Rsv) Rvv ∧
    SplitR (R1.set n (some X))
      (RCtx.union (RCtx.union (RCtx.union (RCtx.single n X) RE) Rsv) Rvv) (RCtx.union Rsd Rvd) ∧
    SplitR (RCtx.union Rsd Rvd) Rsd Rvd := by
  refine ⟨?_, ?_, ?_, ?_, ?_⟩ <;> intro r <;> have := h1 r <;> have := h2 r <;> have := h3 r <;>
    have := h4 r <;> simp only [SRE, RCtx.union, RCtx.single, RCtx.set] at * <;> by_cases hr : r = n <;>
    (try subst hr) <;> (try simp_all) <;> grind

/-- Removing a live single entry from a split store. -/
theorem split_remove {L R1 R2 Q : RCtx} {r : Nat} {X : Ent} (hs : SplitR L R1 R2)
    (h : SplitR R1 (RCtx.single r X) Q) : SplitR (L.set r none) Q R2 := by
  intro x; have := hs x; have := h x
  simp only [SRE, RCtx.single, RCtx.set] at *; by_cases hx : x = r
  · subst hx; simp_all
  · simp only [hx, if_false] at *; grind

/-- Resource bookkeeping of a resume step. -/
theorem resume_splits {R1 Rk1 R2' R3 R4 Rk Rdk Rbody Rwv Rwd Rsv Rsd : RCtx} {r : Nat} {X : Ent}
    (h1 : SplitR R1 Rk1 R2') (h2 : SplitR R2' R3 R4) (h3 : SplitR Rk1 Rk Rdk)
    (h4 : SplitR Rk (RCtx.single r X) Rbody) (h5 : SplitR R3 Rwv Rwd) (h6 : SplitR R4 Rsv Rsd) :
    ∃ Q V D, SplitR R1 (RCtx.single r X) Q ∧ SplitR (RCtx.union Rbody Rsv) Rbody Rsv ∧
      SplitR V (RCtx.union Rbody Rsv) Rwv ∧ SplitR Q V D ∧
      (DiscardR Rdk → DiscardR Rwd → DiscardR Rsd → DiscardR D) := by
  obtain ⟨hA, hB⟩ := splitR_assoc h3 h4
  obtain ⟨hC, hD⟩ := splitR_assoc h1 hA
  obtain ⟨hE, hF, hG⟩ := split4 (splitR_comm h2) h6 h5
  obtain ⟨hH, hI, hJ⟩ := split4 hD hB hE
  obtain ⟨hK, hL⟩ := splitR_assoc' hI hF
  exact ⟨_, _, _, hC, hL, hK, hH, fun d1 d2 d3 => discardR_join hJ d1 (discardR_join hG d3 d2)⟩

theorem head_pres {e L n a e' L' n'} (hh : Head Sig e L n a e' L' n') :
    ∀ {R1 R2 τ m ε}, HasType Sig Ctx.empty R1 e τ m ε → SplitR L R1 R2 → Fresh L n →
      ∃ R1', HasType Sig Ctx.empty R1' e' τ m ε ∧ SplitR L' R1' R2 ∧ Fresh L' n' := by
  cases hh with
  | @letV v e2 _ _ hv =>
    intro R1 R2 τ m ε h hs hf
    cases h with
    | @letE _ Γ1 Γ2 _ Ra Rb _ _ τ1 m1 ε1 _ _ ε2 _ hsΓ hsR h1 h2 hfr h1s h2s =>
      obtain ⟨hΓ1, hΓ2⟩ := split_empty hsΓ
      subst hΓ1; subst hΓ2
      obtain ⟨Rv, Rd, hsv, hd, hvt, hk⟩ := val_inv h1 hv
      obtain ⟨hA, hB⟩ := regroup hsR hsv
      have h2' := subst_ty h2 (k := 0) (substOK_val hvt hk rfl) hA
      rw [ext_del_zero] at h2'
      exact ⟨R1, weakenRow (weakenR h2' hd hB) h2s, hs, hf⟩
  | letAbort =>
    intro R1 R2 τ m ε h hs hf
    cases h with
    | @letE _ Γ1 Γ2 _ Ra Rb _ _ τ1 m1 ε1 _ _ ε2 _ hsΓ hsR h1 h2 hfr h1s h2s =>
      obtain ⟨hΓ1, hΓ2⟩ := split_empty hsΓ
      subst hΓ1; subst hΓ2
      cases h1 with
      | abort _ hdA hab =>
        have hle := rleM_mono (frame_rleM hfr) (res_le Sig hab)
        refine ⟨R1, .abort discard_empty (discardR_join hsR hdA (rleM_affine_discard hle)) (h1s _ hab),
          hs, hf⟩
  | @letPairV va vb body _ _ hva hvb =>
    intro R1 R2 τ m ε h hs hf
    cases h with
    | @letPair _ Γ1 Γ2 _ Ra Rb _ _ md t1 m1 t2 m2 m0 _ _ _ _ hsΓ hsR h1 h2 =>
      obtain ⟨hΓ1, hΓ2⟩ := split_empty hsΓ
      subst hΓ1; subst hΓ2
      obtain ⟨Rv, Rd, hsv, hd, hvt, _⟩ := val_inv h1 (.pair hva hvb)
      cases hvt with
      | @pair _ Rva Rvb _ _ _ _ _ _ _ hsab hvta hvtb _ _ =>
        have hsplit1 : SplitR (RCtx.union Rb Rvb) Rb Rvb := by
          intro r; have := hsR r; have := hsv r; have := hsab r
          unfold SRE RCtx.union at *; grind
        have hsplit2 : SplitR (RCtx.union (RCtx.union Rb Rvb) Rva) (RCtx.union Rb Rvb) Rva := by
          intro r; have := hsR r; have := hsv r; have := hsab r
          unfold SRE RCtx.union at *; grind
        have hsplit3 : SplitR R1 (RCtx.union (RCtx.union Rb Rvb) Rva) Rd := by
          intro r; have := hsR r; have := hsv r; have := hsab r
          unfold SRE RCtx.union at *; grind
        have e1 := subst_ty h2 (k := 0) ⟨hvb, Or.inr ⟨t2, t2.kind, rfl, hvtb, vt_rleM hvtb⟩⟩ hsplit1
        rw [ext_del_zero] at e1
        have e2 := subst_ty e1 (k := 0) ⟨hva, Or.inr ⟨t1, t1.kind, rfl, hvta, vt_rleM hvta⟩⟩ hsplit2
        rw [ext_del_zero] at e2
        exact ⟨R1, weakenR e2 hd hsplit3, hs, hf⟩
  | ifzZ =>
    intro R1 R2 τ m ε h hs hf
    cases h with
    | ifz _ hsΓ hsR h1 h2 _ =>
      obtain ⟨hΓ1, hΓ2⟩ := split_empty hsΓ
      subst hΓ1; subst hΓ2
      exact ⟨R1, weakenR h2 (nat_discard h1) (splitR_comm hsR), hs, hf⟩
  | ifzS =>
    intro R1 R2 τ m ε h hs hf
    cases h with
    | ifz _ hsΓ hsR h1 _ h3 =>
      obtain ⟨hΓ1, hΓ2⟩ := split_empty hsΓ
      subst hΓ1; subst hΓ2
      exact ⟨R1, weakenR h3 (nat_discard h1) (splitR_comm hsR), hs, hf⟩
  | mint =>
    intro R1 R2 τ m ε h hs hf
    cases h with
    | mint _ hd hm =>
      have hLn := hf n (Nat.le_refl _)
      have h12 := hs n; unfold SRE at h12
      refine ⟨R1.set n (some .hdl), .hdlV n discard_empty ?_ hd hm, ?_, ?_⟩
      · intro r; have := hs r; unfold SRE RCtx.set RCtx.single at *; grind
      · intro r; have := hs r; unfold SRE RCtx.set at *; grind
      · intro r hr; unfold RCtx.set; rw [if_neg (by omega)]; exact hf r (by omega)
  | @consume r _ _ hL =>
    intro R1 R2 τ m ε h hs hf
    cases h with
    | consume _ h1 =>
      obtain ⟨Rv, Rd, hsv, hd, hvt, _⟩ := val_inv h1 (.hdlV r)
      cases hvt with
      | hdlV _ he =>
        refine ⟨R1.set r none, .unitV discard_empty ?_, ?_, ?_⟩
        · intro x e hx; have := hsv x; have := he x; have := hd x e
          unfold SRE RCtx.set RCtx.single at *; grind
        · intro x; have := hs x; have := hsv x; have := he x
          unfold SRE RCtx.set RCtx.single at *; grind
        · intro x hx; unfold RCtx.set; split
          · rfl
          · exact hf x hx
  | arithOk =>
    intro R1 R2 τ m ε h hs hf
    cases h with
    | arith _ _ hsΓ hsR h1 h2 _ =>
      obtain ⟨hΓ1, hΓ2⟩ := split_empty hsΓ
      subst hΓ1; subst hΓ2
      exact ⟨R1, .natV _ discard_empty (discardR_join hsR (nat_discard h1) (nat_discard h2)), hs, hf⟩
    | arithLiteral _ hd _ _ _ => exact ⟨R1, .natV _ discard_empty hd, hs, hf⟩
  | arithAbort hev =>
    intro R1 R2 τ m ε h hs hf
    cases h with
    | arith _ _ hsΓ hsR h1 h2 hab =>
      obtain ⟨hΓ1, hΓ2⟩ := split_empty hsΓ
      subst hΓ1; subst hΓ2
      exact ⟨R1, .abort discard_empty (discardR_join hsR (nat_discard h1) (nat_discard h2)) hab, hs, hf⟩
    | arithLiteral _ _ _ _ hsafe => rw [hev] at hsafe; contradiction
  | @handleRet l s v ret cls _ _ hsv hv =>
    intro R1 R2 τ m ε h hs hf
    cases h with
    | @handle _ Γs Γ' Γb Γh _ Rs Rb _ _ _ _ _ τs ms τb mb εb _ _ εc _ hsa hsΓ hsΓ' _ hsR hst hms hb
        hsub hr _ _ hc' =>
      obtain ⟨g1, g2, g3, g4⟩ := split3_empty hsΓ hsΓ'
      subst g1; subst g2; subst g3; subst g4
      obtain ⟨Rsv, Rsd, hss, hsd, hvs, hks⟩ := val_inv hst hsv
      obtain ⟨Rbv, Rbd, hsb, hbd, hvb, hkb⟩ := val_inv hb hv
      obtain ⟨hA, hB, hC⟩ := split4 hsR hss hsb
      have e1 := subst_ty hr (k := 0) (substOK_val hvs hks rfl) (splitR_empty_left Rsv)
      rw [ext_del_zero] at e1
      have e2 := subst_ty e1 (k := 0) (substOK_val hvb hkb rfl) hB
      rw [ext_del_zero] at e2
      exact ⟨R1, weakenRow (weakenR e2 (discardR_join hC hsd hbd) hA) hc', hs, hf⟩
  | handleAbort =>
    intro R1 R2 τ m ε h hs hf
    cases h with
    | handle hsa hsΓ hsΓ' _ hsR hst hms hb hsub _ _ _ hc' =>
      obtain ⟨g1, g2, g3, g4⟩ := split3_empty hsΓ hsΓ'
      subst g1; subst g2; subst g3; subst g4
      cases hb with
      | abort _ hd hab =>
        have := hsub _ hab
        simp only [List.mem_cons, reduceCtorEq, false_or] at this
        have hds := rleM_affine_discard (state_rleM hsa hst hms this)
        exact ⟨R1, .abort discard_empty (discardR_join hsR hds hd) (hc' _ this), hs, hf⟩
  | @capture l o E v s ret cls c _ _ hfree hv hsv hc =>
    intro R1 R2 τ m ε h hs hf
    cases h with
    | @handle _ Γs Γ' Γb Γh _ Rs Rb _ _ _ _ _ τs ms τb mb εb _ _ εc _ hsa hsΓ hsΓ' _ hsR hst hms hb
        hsub hr hall hcl hc' =>
      obtain ⟨g1, g2, g3, g4⟩ := split3_empty hsΓ hsΓ'
      subst g1; subst g2; subst g3; subst g4
      obtain ⟨Rp, RE, τh, mh, εh, hsp, hperf, hrepl, hfr⟩ := plug_inv E hb
      cases hperf with
      | perform _ _ hva hl hm =>
        obtain ⟨_, hle⟩ := hfr _ hfree hl
        have hLn := hf n (Nat.le_refl _)
        obtain ⟨Rsv, Rsd, hss, hsd, hvs, hks⟩ := val_inv hst hsv
        obtain ⟨Rvv, Rvd, hsvv, hvd, hvt, _⟩ := val_inv hva hv
        -- the continuation's body: the deep handler around `E[var 1]` with state `var 0`
        have hbody : HasType Sig (Ctx.two ((Sig l).res o, ((Sig l).res o).kind) (τs, ms)) RE
            (kontBody l E ret cls) τ m εc :=
          .handle (.var 0) (split_two _ _) (split_left_all _) allCopy_empty (splitR_empty_left _)
            (var_only0 hks) hms
            (hrepl _ RCtx.empty RE _ (var_only1 hm) (splitR_empty_left _)) hsub hr hall hcl
            (rowSub_refl _)
        have hR1n : R1 n = none := by
          have := hs n; unfold SRE at this; grind
        obtain ⟨hRE, hsplit1, hsplit2, hsplit3, hsplit4⟩ :=
          capture_splits (.kont (Sig l).q) hR1n hsR hss hsp hsvv
        have hK : VT Sig (RCtx.union (RCtx.single n (.kont (Sig l).q)) RE)
            (.kontV n (kontBody l E ret cls))
            (.kont ((Sig l).res o) τs ms τ m εc (Sig l).q) := .kontV n _ hRE hbody hle
        have e1 := subst_ty (hcl o c hc) (k := 0) (substOK_val hK (Mode.le_refl _) rfl)
          (splitR_empty_left _)
        simp only [clauseCtx] at e1
        rw [ext_del_zero] at e1
        have e2 := subst_ty e1 (k := 0) (substOK_val hvs hks rfl) hsplit1
        rw [ext_del_zero] at e2
        have e3 := subst_ty e2 (k := 0) (substOK_val hvt (Mode.le_refl _) rfl) hsplit2
        rw [ext_del_zero] at e3
        refine ⟨R1.set n (some (.kont (Sig l).q)),
          weakenRow (weakenR e3 (discardR_join hsplit4 hsd hvd) hsplit3) hc', ?_, ?_⟩
        · intro r; have := hs r; unfold SRE RCtx.set at *; have := hf r; grind
        · intro r hr; unfold RCtx.set; rw [if_neg (by omega)]; exact hf r (by omega)
  | @resume r body w s q _ _ hL hw hsv =>
    intro R1 R2 τ m ε h hs hf
    cases h with
    | resume _ _ _ hsΓ hsΓ' hsR hsR' hk ha hst hsub =>
      obtain ⟨g1, g2, g3, g4⟩ := split3_empty hsΓ hsΓ'
      subst g1; subst g2; subst g3; subst g4
      obtain ⟨Rk, Rdk, hsk, hdk, hvk, _⟩ := val_inv hk (.kontV r body)
      cases hvk with
      | kontV _ _ hs0 hbody _ =>
        obtain ⟨Rwv, Rwd, hsw, hdw, hvw, _⟩ := val_inv ha hw
        obtain ⟨Rsv, Rsd, hss, hsd, hvs, hks⟩ := val_inv hst hsv
        obtain ⟨Q, V, D, hQ, hA, hB, hC, hD⟩ := resume_splits hsR hsR' hsk hs0 hsw hss
        have e1 := subst_ty hbody (k := 0) (substOK_val hvs hks rfl) hA
        rw [two_del_zero] at e1
        have e2 := subst_ty e1 (k := 0) (substOK_val hvw (Mode.le_refl _) rfl) hB
        rw [one_del_zero] at e2
        refine ⟨Q, weakenRow (weakenR e2 (hD hdk hdw hsd) hC) hsub, split_remove hs hQ, ?_⟩
        intro x hx; unfold RCtx.set; split
        · rfl
        · exact hf x hx
  | @app v body w _ _ hv hw =>
    intro R1 R2 τ m ε h hs hf
    cases h with
    | app _ _ hsΓ hsR hfun ha hsub =>
      obtain ⟨hΓ1, hΓ2⟩ := split_empty hsΓ
      subst hΓ1; subst hΓ2
      obtain ⟨Rf, Rfd, hsf, hfd, hvf, _⟩ := val_inv hfun (.clos _ hv)
      cases hvf with
      | clos hvenv hkz _ hbody =>
        obtain ⟨Rwv, Rwd, hsw, hwd, hvw, _⟩ := val_inv ha hw
        obtain ⟨hA, hB, hC⟩ := split4 (splitR_comm hsR) hsw hsf
        have e1 := subst_ty hbody (k := 0) (substOK_val hvw (Mode.le_refl _) rfl) (splitR_empty_left Rwv)
        rw [two_del_zero] at e1
        have e2 := subst_ty e1 (k := 0) (substOK_val hvenv hkz rfl) hB
        rw [one_del_zero] at e2
        exact ⟨R1, weakenRow (weakenR e2 (discardR_join hC hwd hfd) hA) hsub, hs, hf⟩
  | @get v p w _ _ hv hat =>
    intro R1 R2 τ m ε h hs hf
    cases h with
    | get _ h1 hp hc hm =>
      obtain ⟨Rv, Rd, hsv, hd, hvt, _⟩ := val_inv h1 hv
      obtain ⟨w', Rw, Rr, hat', hw, _, _⟩ := vt_at hvt hp
      rw [hat] at hat'; cases hat'
      have he : REmpty Rw := rleM_copy (hc ▸ vt_rleM hw)
      have hpair : VT Sig Rv (.pair w v) (leafRoot _ _) :=
        .pair (splitR_left_of_empty he) hw hvt (Mode.le_linear _) (Mode.le_linear _)
      exact ⟨R1, val_ty hpair discard_empty hd hsv hm, hs, hf⟩
  | @swap v p b w v' _ _ hv hb hat hset =>
    intro R1 R2 τ m ε h hs hf
    cases h with
    | swap _ _ hsΓ hsR h1 hp h2 hm =>
      obtain ⟨hΓ1, hΓ2⟩ := split_empty hsΓ
      subst hΓ1; subst hΓ2
      obtain ⟨Rv, Rdv, hsv, hdv, hvt, _⟩ := val_inv h1 hv
      obtain ⟨Rbv, Rdb, hsb, hdb, hvb, _⟩ := val_inv h2 hb
      obtain ⟨w', Rw, Rr, hat', hw, hsw, hsetf⟩ := vt_at hvt hp
      rw [hat] at hat'; cases hat'
      have hs1 : SplitR (RCtx.union Rr Rbv) Rr Rbv := by
        intro x; have := hsR x; have := hsv x; have := hsb x; have := hsw x
        unfold SRE RCtx.union at *; grind
      obtain ⟨v'', hset', hv''⟩ := hsetf b Rbv _ hvb hs1
      rw [hset] at hset'; cases hset'
      have hs2 : SplitR (RCtx.union Rw (RCtx.union Rr Rbv)) Rw (RCtx.union Rr Rbv) := by
        intro x; have := hsR x; have := hsv x; have := hsb x; have := hsw x
        unfold SRE RCtx.union at *; grind
      have hs3 : SplitR R1 (RCtx.union Rw (RCtx.union Rr Rbv)) (RCtx.union Rdv Rdb) := by
        intro x; have h1 := hsR x; have h2 := hsv x; have h3 := hsb x; have h4 := hsw x
        clear hs1 hs2 hsetf; unfold SRE RCtx.union at *; grind (splits := 40)
      have hs4 : SplitR (RCtx.union Rdv Rdb) Rdv Rdb := by
        intro x; have := hsR x; have := hsv x; have := hsb x; have := hsw x
        unfold SRE RCtx.union at *; grind
      have hpair : VT Sig (RCtx.union Rw (RCtx.union Rr Rbv)) (.pair w v') (leafRoot _ _) :=
        .pair hs2 hw hv'' (Mode.le_linear _) (Mode.le_linear _)
      exact ⟨R1, val_ty hpair discard_empty (discardR_join hs4 hdv hdb) hs3 hm, hs, hf⟩
  | @swap2 v p q v' _ _ hv hsw =>
    intro R1 R2 τ m ε h hs hf
    cases h with
    | swap2 _ h1 hp hq hd hm =>
      obtain ⟨Rv, Rd, hsv, hdd, hvt, _⟩ := val_inv h1 hv
      obtain ⟨v'', hsw', hv''⟩ := vt_swap2 hvt hp hq hd
      rw [hsw] at hsw'; cases hsw'
      exact ⟨R1, val_ty hv'' discard_empty hdd hsv hm, hs, hf⟩

/-- **Preservation** (types, modes and effect rows). -/
theorem preservation {c a c' τ m ε} (hs : Step Sig c a c') (hwt : WT Sig c τ m ε) : WT Sig c' τ m ε := by
  cases hs with
  | ctx E hh =>
    obtain ⟨ht, hf⟩ := hwt
    obtain ⟨R1, R2, τh, mh, εh, hsp, hhole, hrepl, _⟩ := plug_inv E ht
    obtain ⟨R1', ht', hs', hf'⟩ := head_pres hh hhole hsp hf
    exact ⟨hrepl _ _ _ _ ht' hs', hf'⟩

end Preservation

/-! ## Progress (Theorem 1, second half) -/

section Progress
variable {Sig : Nat → EffSig}

theorem sub_left {R R1 R2 L : RCtx} (hs : SplitR R R1 R2) (hL : ∀ r q, R r = some q → L r = some q) :
    ∀ r q, R1 r = some q → L r = some q := by
  intro r q h; apply hL; have := hs r; unfold SRE at this; grind

theorem sub_right {R R1 R2 L : RCtx} (hs : SplitR R R1 R2) (hL : ∀ r q, R r = some q → L r = some q) :
    ∀ r q, R2 r = some q → L r = some q := by
  intro r q h; apply hL; have := hs r; unfold SRE at this; grind

theorem canon_nat {R v m ε} (h : HasType Sig Ctx.empty R v .nat m ε) (hv : IsVal v) : ∃ k, v = .natV k := by
  obtain ⟨_, _, _, _, hvt, _⟩ := val_inv h hv
  cases hvt with | natV k _ => exact ⟨k, rfl⟩

theorem canon_hdl {R v m ε} (h : HasType Sig Ctx.empty R v .hdl m ε) (hv : IsVal v) :
    ∃ r, v = .hdlV r ∧ R r = some .hdl := by
  obtain ⟨_, _, hs, _, hvt, _⟩ := val_inv h hv
  cases hvt with
  | hdlV r he =>
    refine ⟨r, rfl, ?_⟩
    have := hs r; have := he r; unfold SRE RCtx.single at *; grind

theorem canon_kont {R v σ τs ms τ am ε' q m ε} (h : HasType Sig Ctx.empty R v (.kont σ τs ms τ am ε' q) m ε)
    (hv : IsVal v) : ∃ r body, v = .kontV r body ∧ R r = some (.kont q) := by
  obtain ⟨_, _, hs, _, hvt, _⟩ := val_inv h hv
  cases hvt with
  | kontV r body hs0 _ _ =>
    refine ⟨r, body, rfl, ?_⟩
    have := hs r; have := hs0 r; unfold SRE RCtx.single at *; grind

theorem canon_arrow {R v σ τ am εf mf m ε} (h : HasType Sig Ctx.empty R v (.arrow σ τ am εf mf) m ε)
    (hv : IsVal v) : ∃ w body, v = .clos w body ∧ IsVal w := by
  obtain ⟨_, _, _, _, hvt, _⟩ := val_inv h hv
  cases hvt with
  | clos hw _ _ _ => exact ⟨_, _, rfl, vt_isVal hw⟩

theorem canon_pair {R v md t1 m1 t2 m2 m ε} (h : HasType Sig Ctx.empty R v (.pair md t1 m1 t2 m2) m ε)
    (hv : IsVal v) : ∃ a b, v = .pair a b ∧ IsVal a ∧ IsVal b := by
  cases hv with
  | pair ha hb => exact ⟨_, _, rfl, ha, hb⟩
  | _ =>
    obtain ⟨_, _, _, _, hvt, _⟩ := val_inv h (by constructor <;> assumption)
    cases hvt

theorem vt_set_exists {p : Path} : ∀ {R v τ σ}, VT Sig R v τ → τ.at p = some σ →
    ∀ u, ∃ v', vSet v p u = some v' := by
  induction p with
  | nil => intro _ _ _ _ _ _ u; exact ⟨u, by simp [vSet]⟩
  | cons b p ih =>
    intro R v τ σ hv hp u
    cases hv with
    | @pair _ _ _ a c _ _ _ _ _ _ h1 h2 _ _ =>
      cases b with
      | false =>
        obtain ⟨a', ha'⟩ := ih h1 hp u
        exact ⟨.pair a' c, by simp [vSet, ha']⟩
      | true =>
        obtain ⟨b', hb'⟩ := ih h2 hp u
        exact ⟨.pair a b', by simp [vSet, hb']⟩
    | _ => simp [Ty.at] at hp

/-- A closed well-typed term is a value, an abort, an unhandled perform of an
operation of an effect in its row, or it takes a head step in some evaluation
context. -/
theorem progress_gen {Γ R e τ m ε} (h : HasType Sig Γ R e τ m ε) : Γ = Ctx.empty →
    ∀ {L : RCtx} {n : Nat}, (∀ r q, R r = some q → L r = some q) →
      IsVal e ∨ e = .abort ∨
      (∃ E l o v, E.Free (.user l) ∧ IsVal v ∧ o < (Sig l).nops ∧ e = plug E (.perform l o v) ∧
        Label.user l ∈ ε) ∨
      ∃ E e0 a e0' L' n', e = plug E e0 ∧ Head Sig e0 L n a e0' L' n' := by
  induction h with
  | var hi => intro hΓ; subst hΓ; simp [Ctx.empty] at hi
  | unitV => intro _ _ _ _; exact Or.inl .unitV
  | natV k => intro _ _ _ _; exact Or.inl (.natV k)
  | hdlV r => intro _ _ _ _; exact Or.inl (.hdlV r)
  | kontV r body => intro _ _ _ _; exact Or.inl (.kontV r body)
  | clos ha h1 =>
    intro hΓ L n _; subst hΓ
    exact Or.inl (.clos _ (atom_closed ha h1))
  | pair ha hb hsΓ _ h1 h2 =>
    intro hΓ L n _; subst hΓ
    obtain ⟨e1, e2⟩ := split_empty hsΓ; subst e1; subst e2
    exact Or.inl (.pair (atom_closed ha h1) (atom_closed hb h2))
  | @letE _ _ _ _ _ _ e1 e2 _ _ _ _ _ _ _ hsΓ hsR _ _ _ h1s _ ih1 _ =>
    intro hΓ L n hL; subst hΓ
    obtain ⟨g1, g2⟩ := split_empty hsΓ; subst g1; subst g2
    rcases ih1 rfl (sub_left hsR hL) (n := n) with hv | hab | ⟨E, l, o, v, hfree, hv, ho, he, hl⟩ |
        ⟨E, e0, a, e0', L', n', he, hh⟩
    · exact Or.inr (Or.inr (Or.inr ⟨.hole, _, _, _, _, _, rfl, .letV hv⟩))
    · subst hab; exact Or.inr (Or.inr (Or.inr ⟨.hole, _, _, _, _, _, rfl, .letAbort⟩))
    · exact Or.inr (Or.inr (Or.inl ⟨.letC E e2, l, o, v, hfree, hv, ho, by simp [plug, he], h1s _ hl⟩))
    · exact Or.inr (Or.inr (Or.inr ⟨.letC E e2, e0, a, e0', L', n', by simp [plug, he], hh⟩))
  | letPair ha hsΓ _ h1 _ =>
    intro hΓ L n _; subst hΓ
    obtain ⟨g1, g2⟩ := split_empty hsΓ; subst g1; subst g2
    obtain ⟨x, y, rfl, hx, hy⟩ := canon_pair h1 (atom_closed ha h1)
    exact Or.inr (Or.inr (Or.inr ⟨.hole, _, _, _, _, _, rfl, .letPairV hx hy⟩))
  | ifz ha hsΓ _ h1 =>
    intro hΓ L n _; subst hΓ
    obtain ⟨g1, g2⟩ := split_empty hsΓ; subst g1; subst g2
    obtain ⟨k, rfl⟩ := canon_nat h1 (atom_closed ha h1)
    cases k with
    | zero => exact Or.inr (Or.inr (Or.inr ⟨.hole, _, _, _, _, _, rfl, .ifzZ⟩))
    | succ k => exact Or.inr (Or.inr (Or.inr ⟨.hole, _, _, _, _, _, rfl, .ifzS⟩))
  | mint => intro _ L n _; exact Or.inr (Or.inr (Or.inr ⟨.hole, _, _, _, _, _, rfl, .mint⟩))
  | consume ha h1 =>
    intro hΓ L n hL; subst hΓ
    obtain ⟨r, rfl, hr⟩ := canon_hdl h1 (atom_closed ha h1)
    exact Or.inr (Or.inr (Or.inr ⟨.hole, _, _, _, _, _, rfl, .consume (hL r _ hr)⟩))
  | @arith _ _ _ _ _ _ op _ _ _ _ ha hb hsΓ _ h1 h2 =>
    intro hΓ L n _; subst hΓ
    obtain ⟨g1, g2⟩ := split_empty hsΓ; subst g1; subst g2
    obtain ⟨x, rfl⟩ := canon_nat h1 (atom_closed ha h1)
    obtain ⟨y, rfl⟩ := canon_nat h2 (atom_closed hb h2)
    cases hev : op.eval x y with
    | none => exact Or.inr (Or.inr (Or.inr ⟨.hole, _, _, _, _, _, rfl, .arithAbort hev⟩))
    | some z => exact Or.inr (Or.inr (Or.inr ⟨.hole, _, _, _, _, _, rfl, .arithOk hev⟩))
  | arithLiteral _ _ _ _ hev =>
    intro _ L n _
    exact Or.inr (Or.inr (Or.inr ⟨.hole, _, _, _, _, _, rfl, .arithOk hev⟩))
  | abort => intro _ _ _ _; exact Or.inr (Or.inl rfl)
  | @perform _ _ l o _ _ _ ho ha h1 hl =>
    intro hΓ L n _; subst hΓ
    exact Or.inr (Or.inr (Or.inl ⟨.hole, l, o, _, trivial, atom_closed ha h1, ho, rfl, hl⟩))
  | @handle _ _ _ _ _ _ _ _ l s _ ret cls _ _ _ _ _ _ _ _ _ hsa hsΓ hsΓ' _ hsR hst _ _ hsub _ hall _ hc' _ ihb =>
    intro hΓ L n hL; subst hΓ
    obtain ⟨g1, g2, g3, g4⟩ := split3_empty hsΓ hsΓ'
    subst g1; subst g2; subst g3; subst g4
    have hsv := atom_closed hsa hst
    rcases ihb rfl (sub_right hsR hL) (n := n) with hv | hab | ⟨E, l', o, v, hfree, hv, ho, he, hl⟩ |
        ⟨E, e0, a, e0', L', n', he, hh⟩
    · exact Or.inr (Or.inr (Or.inr ⟨.hole, _, _, _, _, _, rfl, .handleRet hsv hv⟩))
    · subst hab; exact Or.inr (Or.inr (Or.inr ⟨.hole, _, _, _, _, _, rfl, .handleAbort⟩))
    · subst he
      by_cases hll : l' = l
      · subst hll
        obtain ⟨c, hc⟩ := Option.isSome_iff_exists.mp (hall o ho)
        exact Or.inr (Or.inr (Or.inr ⟨.hole, _, _, _, _, _, rfl, .capture hfree hv hsv hc⟩))
      · have hne : Label.user l' ≠ Label.user l := fun h => hll (Label.user.inj h)
        have hmem := hsub _ hl
        simp only [List.mem_cons] at hmem
        rcases hmem with h | h
        · exact absurd h hne
        · exact Or.inr (Or.inr (Or.inl ⟨.handleC l s E ret cls, l', o, v, ⟨hne, hfree⟩, hv, ho, rfl,
            hc' _ h⟩))
    · exact Or.inr (Or.inr (Or.inr ⟨.handleC l s E ret cls, e0, a, e0', L', n', by simp [plug, he], hh⟩))
  | resume hk ha hsa hsΓ hsΓ' hsR _ h1 h2 h3 =>
    intro hΓ L n hL; subst hΓ
    obtain ⟨g1, g2, g3, g4⟩ := split3_empty hsΓ hsΓ'
    subst g1; subst g2; subst g3; subst g4
    obtain ⟨r, body, rfl, hr⟩ := canon_kont h1 (atom_closed hk h1)
    exact Or.inr (Or.inr (Or.inr ⟨.hole, _, _, _, _, _, rfl,
      .resume (sub_left hsR hL r _ hr) (atom_closed ha h2) (atom_closed hsa h3)⟩))
  | app hf ha hsΓ _ h1 h2 =>
    intro hΓ L n _; subst hΓ
    obtain ⟨g1, g2⟩ := split_empty hsΓ; subst g1; subst g2
    obtain ⟨w, body, rfl, hw⟩ := canon_arrow h1 (atom_closed hf h1)
    exact Or.inr (Or.inr (Or.inr ⟨.hole, _, _, _, _, _, rfl, .app hw (atom_closed ha h2)⟩))
  | get ha h1 hp =>
    intro hΓ L n _; subst hΓ
    have hv := atom_closed ha h1
    obtain ⟨_, _, _, _, hvt, _⟩ := val_inv h1 hv
    obtain ⟨w, _, _, hat, _⟩ := vt_at hvt hp
    exact Or.inr (Or.inr (Or.inr ⟨.hole, _, _, _, _, _, rfl, .get hv hat⟩))
  | swap ha hb hsΓ _ h1 hp h2 =>
    intro hΓ L n _; subst hΓ
    obtain ⟨g1, g2⟩ := split_empty hsΓ; subst g1; subst g2
    have hv := atom_closed ha h1
    have hbv := atom_closed hb h2
    obtain ⟨_, _, _, _, hvt, _⟩ := val_inv h1 hv
    obtain ⟨w, _, _, hat, _⟩ := vt_at hvt hp
    obtain ⟨v', hset⟩ := vt_set_exists hvt hp _
    exact Or.inr (Or.inr (Or.inr ⟨.hole, _, _, _, _, _, rfl, .swap hv hbv hat hset⟩))
  | swap2 ha h1 hp hq hd =>
    intro hΓ L n _; subst hΓ
    have hv := atom_closed ha h1
    obtain ⟨_, _, _, _, hvt, _⟩ := val_inv h1 hv
    obtain ⟨v', hsw, _⟩ := vt_swap2 hvt hp hq hd
    exact Or.inr (Or.inr (Or.inr ⟨.hole, _, _, _, _, _, rfl, .swap2 hv hsw⟩))

/-- **Progress**: a well-typed closed configuration whose row has no
unhandled user effects is a value, an abort, or steps. -/
theorem progress {c τ m ε} (hwt : WT Sig c τ m ε) (hε : ∀ l, Label.user l ∉ ε) :
    IsVal c.e ∨ c.e = .abort ∨ ∃ a c', Step Sig c a c' := by
  obtain ⟨e, L, n⟩ := c
  obtain ⟨ht, _⟩ := hwt
  rcases progress_gen ht rfl (L := L) (n := n) (fun _ _ h => h) with hv | hab | ⟨_, l, _, _, _, _, _, _, hl⟩ |
      ⟨E, e0, a, e0', L', n', he, hh⟩
  · exact Or.inl hv
  · exact Or.inr (Or.inl hab)
  · exact absurd hl (hε l)
  · simp only at he; subst he
    exact Or.inr (Or.inr ⟨a, _, .ctx E hh⟩)

/-- **Type safety** (Theorem 1): along any run, configurations stay well typed
and are never stuck. -/
theorem type_safety {c acts c' τ m ε} (hs : Steps Sig c acts c') (hwt : WT Sig c τ m ε)
    (hε : ∀ l, Label.user l ∉ ε) :
    WT Sig c' τ m ε ∧ (IsVal c'.e ∨ c'.e = .abort ∨ ∃ a c'', Step Sig c' a c'') := by
  induction hs with
  | refl c => exact ⟨hwt, progress hwt hε⟩
  | cons hstep _ ih => exact ih (preservation hstep hwt)

end Progress

section Exhaustion
variable {Sig : Nat → EffSig}

/-- `a` releases store id `r`: consuming a handle or resuming a continuation. -/
def isRel (r : Nat) : Act → Bool
  | .consume x => x == r
  | .resume x => x == r
  | _ => false

def relCount (r : Nat) (acts : List Act) : Nat := acts.countP (isRel r)

/-- `r` was allocated and is no longer live; it can never be live again. -/
def Dead (r : Nat) (c : Conf) : Prop := r < c.next ∧ c.live r = none

theorem head_next {e L n a e' L' n'} (hh : Head Sig e L n a e' L' n') : n ≤ n' := by
  cases hh <;> omega

theorem head_fresh {e L n a e' L' n'} (hh : Head Sig e L n a e' L' n') (hf : Fresh L n) : Fresh L' n' := by
  cases hh <;> first
    | exact hf
    | (intro x hx; unfold RCtx.set; rw [if_neg (by omega)]; exact hf x (by omega))
    | (intro x hx; unfold RCtx.set; split
       · rfl
       · exact hf x hx)

theorem head_dead {e L n a e' L' n'} (hh : Head Sig e L n a e' L' n') {r : Nat}
    (hr : r < n) (hl : L r = none) : L' r = none := by
  cases hh <;> first
    | exact hl
    | (unfold RCtx.set; rw [if_neg (by omega)]; exact hl)
    | (unfold RCtx.set; split
       · rfl
       · exact hl)

theorem head_release {e L n a e' L' n'} (hh : Head Sig e L n a e' L' n') {r : Nat}
    (ha : isRel r a = true) : L r ≠ none ∧ L' r = none := by
  cases hh <;> simp [isRel] at ha
  all_goals (subst ha; simp_all [RCtx.set])

theorem head_keep {e L n a e' L' n'} (hh : Head Sig e L n a e' L' n') (hf : Fresh L n) {r : Nat} {q}
    (hl : L r = some q) (ha : isRel r a = false) : L' r = some q := by
  have hrn : r < n := by
    apply Classical.byContradiction; intro h
    rw [hf r (by omega)] at hl; cases hl
  cases hh <;> simp [isRel] at ha
  all_goals first
    | exact hl
    | (unfold RCtx.set; rw [if_neg (by omega)]; exact hl)
    | (unfold RCtx.set; rw [if_neg (fun h => ha h.symm)]; exact hl)

theorem head_alloc {e L n a e' L' n'} (hh : Head Sig e L n a e' L' n') {r : Nat} {q : RMode}
    (ha : a = .mint r ∨ a = .capture r q) :
    (a = .capture r q ∧ L' r = some (.kont q)) ∨ (a = .mint r ∧ L' r = some .hdl) := by
  cases hh <;> simp at ha
  · right; subst ha; simp [RCtx.set]
  · left; obtain ⟨h1, h2⟩ := ha; subst h1; subst h2; simp [RCtx.set]

theorem step_parts {c a c'} (hs : Step Sig c a c') :
    ∃ e L n e' L' n', c = ⟨e, L, n⟩ ∧ c' = ⟨e', L', n'⟩ ∧ ∃ e0 e0', Head Sig e0 L n a e0' L' n' := by
  cases hs with
  | ctx E hh => exact ⟨_, _, _, _, _, _, rfl, rfl, _, _, hh⟩

theorem step_fresh {c a c'} (hs : Step Sig c a c') (hf : Fresh c.live c.next) : Fresh c'.live c'.next := by
  obtain ⟨_, _, _, _, _, _, rfl, rfl, _, _, hh⟩ := step_parts hs
  exact head_fresh hh hf

theorem step_dead {c a c' r} (hs : Step Sig c a c') (hd : Dead r c) : Dead r c' := by
  obtain ⟨_, _, _, _, _, _, rfl, rfl, _, _, hh⟩ := step_parts hs
  exact ⟨Nat.lt_of_lt_of_le hd.1 (head_next hh), head_dead hh hd.1 hd.2⟩

theorem steps_fresh {c acts c'} (hs : Steps Sig c acts c') (hf : Fresh c.live c.next) :
    Fresh c'.live c'.next := by
  induction hs with
  | refl => exact hf
  | cons h1 _ ih => exact ih (step_fresh h1 hf)

theorem steps_dead {c acts c' r} (hs : Steps Sig c acts c') (hd : Dead r c) : Dead r c' := by
  induction hs with
  | refl => exact hd
  | cons h1 _ ih => exact ih (step_dead h1 hd)

/-- **One-shot release**: along any run from a configuration with a fresh
allocator, each store id is released (a handle consumed or a continuation
resumed) at most once, and never once it is dead. -/
theorem release_at_most_once {c acts c'} (hs : Steps Sig c acts c') (hf : Fresh c.live c.next) (r : Nat) :
    relCount r acts ≤ 1 ∧ (Dead r c → relCount r acts = 0) ∧ (0 < relCount r acts → Dead r c') := by
  induction hs with
  | refl => simp [relCount]
  | @cons c a c1 acts c' h1 hrest ih =>
    have ih := ih (step_fresh h1 hf)
    obtain ⟨e, L, n, e', L', n', rfl, rfl, _, _, hh⟩ := step_parts h1
    simp only at hf
    by_cases ha : isRel r a = true
    · obtain ⟨hlive, hdead⟩ := head_release hh ha
      have hrn : r < n := by
        apply Classical.byContradiction; intro h
        exact hlive (hf r (by omega))
      have hd1 : Dead r ⟨e', L', n'⟩ := ⟨Nat.lt_of_lt_of_le hrn (head_next hh), hdead⟩
      have h0 := ih.2.1 hd1
      refine ⟨?_, ?_, ?_⟩
      · simp [relCount, ha] at h0 ⊢; omega
      · intro hd; exact absurd hd.2 hlive
      · intro _; exact steps_dead hrest hd1
    · have ha' : isRel r a = false := by simpa using ha
      have hc : relCount r (a :: acts) = relCount r acts := by simp [relCount, ha']
      rw [hc]
      refine ⟨ih.1, fun hd => ih.2.1 (step_dead h1 hd), ih.2.2⟩

theorem count_resume_le_rel (r : Nat) (acts : List Act) : acts.count (.resume r) ≤ relCount r acts := by
  induction acts with
  | nil => simp
  | cons a acts ih =>
    simp only [List.count_cons, relCount, List.countP_cons] at ih ⊢
    cases a <;> simp [isRel] <;> (try split) <;> (try split) <;> omega

/-- **One-shot resumption** (Theorem 2, the continuation half): every
continuation is resumed at most once along any run. -/
theorem one_shot_resumption {c acts c'} (hs : Steps Sig c acts c') (hf : Fresh c.live c.next) (r : Nat) :
    acts.count (.resume r) ≤ 1 :=
  Nat.le_trans (count_resume_le_rel r acts) (release_at_most_once hs hf r).1

theorem steps_keep {c acts c' r q} (hs : Steps Sig c acts c') (hf : Fresh c.live c.next)
    (hl : c.live r = some q) (h0 : relCount r acts = 0) : c'.live r = some q := by
  induction hs with
  | refl => exact hl
  | @cons c a c1 acts c' h1 _ ih =>
    have hnot : isRel r a = false := by
      cases h : isRel r a
      · rfl
      · simp [relCount, h] at h0
    have h0' : relCount r acts = 0 := by simpa [relCount, hnot] using h0
    obtain ⟨e, L, n, e', L', n', rfl, rfl, _, _, hh⟩ := step_parts h1
    exact ih (step_fresh h1 hf) (head_keep hh hf hl hnot) h0'

/-- A linear obligation allocated along a run and never released is still live. -/
theorem alloc_live {c acts c' r} (hs : Steps Sig c acts c') (hf : Fresh c.live c.next)
    (ha : Act.mint r ∈ acts ∨ Act.capture r .linear ∈ acts) (h0 : relCount r acts = 0) :
    ∃ e, c'.live r = some e ∧ e.q = .linear := by
  induction hs with
  | refl => simp at ha
  | @cons c a c1 acts c' h1 hrest ih =>
    have hnot : isRel r a = false := by
      cases h : isRel r a
      · rfl
      · simp [relCount, h] at h0
    have h0' : relCount r acts = 0 := by simpa [relCount, hnot] using h0
    have hf1 := step_fresh h1 hf
    simp only [List.mem_cons] at ha
    by_cases hnow : a = .mint r ∨ a = .capture r .linear
    · obtain ⟨e, L, n, e', L', n', rfl, rfl, _, _, hh⟩ := step_parts h1
      rcases head_alloc hh (q := .linear) hnow with ⟨_, hl⟩ | ⟨_, hl⟩
      · exact ⟨_, steps_keep hrest hf1 hl h0', rfl⟩
      · exact ⟨_, steps_keep hrest hf1 hl h0', rfl⟩
    · have : Act.mint r ∈ acts ∨ Act.capture r .linear ∈ acts := by
        rcases ha with (h | h) | (h | h)
        · exact absurd (Or.inl h.symm) hnow
        · exact Or.inl h
        · exact absurd (Or.inr h.symm) hnow
        · exact Or.inr h
      exact ih hf1 this h0'

/-- A terminated configuration (a value at mode copy, or an abort) holds no
live linear handle or linear continuation. -/
theorem terminal_no_linear {c τ m ε} (hwt : WT Sig c τ m ε)
    (hfin : (IsVal c.e ∧ m = .copy) ∨ c.e = .abort) : DiscardR c.live := by
  obtain ⟨ht, _⟩ := hwt
  rcases hfin with ⟨hv, rfl⟩ | hab
  · obtain ⟨Rv, Rd, hs, hd, hvt, hk⟩ := val_inv ht hv
    have he := rleM_copy (rleM_mono (vt_rleM hvt) hk)
    exact discardR_join hs (empty_discardR he) hd
  · obtain ⟨e, L, n⟩ := c
    simp only at hab; subst hab
    cases ht with
    | abort _ hd _ => exact hd

/-- **Theorem 2 (Total Linear Exhaustion)** for this fragment. In a closed
program typed at mode copy with no unhandled user effects (aborts allowed),
every run that terminates — in a value or in a checked-arithmetic abort —
releases every linear obligation it allocated exactly once: every minted
handle and every captured linear continuation is consumed or resumed exactly
once, so nothing leaks on an early error edge, and (by `one_shot_resumption`)
no continuation is resumed twice. -/
theorem total_linear_exhaustion {e τ ε acts c'}
    (ht : HasType Sig Ctx.empty RCtx.empty e τ .copy ε) (hε : ∀ l, Label.user l ∉ ε)
    (hs : Steps Sig ⟨e, RCtx.empty, 0⟩ acts c') (hfin : IsVal c'.e ∨ c'.e = .abort) :
    ∀ r, (Act.mint r ∈ acts ∨ Act.capture r .linear ∈ acts) → relCount r acts = 1 := by
  intro r hr
  have hf0 : Fresh RCtx.empty 0 := fun _ _ => rfl
  have hwt' := (type_safety hs ⟨ht, hf0⟩ hε).1
  have hdisc := terminal_no_linear hwt' (by rcases hfin with h | h; exact Or.inl ⟨h, rfl⟩; exact Or.inr h)
  have hle := (release_at_most_once hs hf0 r).1
  cases h : relCount r acts with
  | zero =>
    obtain ⟨e, he, hq⟩ := alloc_live hs hf0 hr h
    rw [hdisc r e he] at hq; cases hq
  | succ k => omega

end Exhaustion

/-! ## Theorem 2 per kind: handles are released only by `consume`,
continuations only by `resume` -/

section PerKind
variable {Sig : Nat → EffSig}

theorem head_consume_hdl {e L n e' L' n' r} (hh : Head Sig e L n (.consume r) e' L' n') :
    L r = some .hdl := by
  cases hh with
  | consume h => exact h

theorem head_resume_kont {e L n e' L' n' r} (hh : Head Sig e L n (.resume r) e' L' n') :
    ∃ q, L r = some (.kont q) := by
  cases hh with
  | resume h _ _ => exact ⟨_, h⟩

/-- Allocation acts name ids at or above the allocator of the run's start. -/
theorem steps_alloc_ge {c acts c' r} (hs : Steps Sig c acts c') {q : RMode}
    (ha : Act.mint r ∈ acts ∨ Act.capture r q ∈ acts) : c.next ≤ r := by
  induction hs with
  | refl => simp at ha
  | @cons c a c1 acts c' h1 _ ih =>
    obtain ⟨e, L, n, e', L', n', rfl, rfl, _, _, hh⟩ := step_parts h1
    simp only [List.mem_cons] at ha
    rcases ha with (h | h) | (h | h)
    · subst h; cases hh; exact Nat.le_refl _
    · exact Nat.le_trans (head_next hh) (ih (Or.inl h))
    · subst h; cases hh; exact Nat.le_refl _
    · exact Nat.le_trans (head_next hh) (ih (Or.inr h))

theorem count_consume_le_rel (r : Nat) (acts : List Act) : acts.count (.consume r) ≤ relCount r acts := by
  induction acts with
  | nil => simp
  | cons a acts ih =>
    simp only [List.count_cons, relCount, List.countP_cons] at ih ⊢
    cases a <;> simp [isRel] <;> (try split) <;> (try split) <;> omega

theorem relCount_eq (r : Nat) (acts : List Act) :
    relCount r acts = acts.count (.consume r) + acts.count (.resume r) := by
  induction acts with
  | nil => simp [relCount]
  | cons a acts ih =>
    simp only [relCount, List.countP_cons, List.count_cons] at ih ⊢
    rw [ih]; cases a <;> simp [isRel] <;> (try split) <;> (try split) <;> omega

/-- Once dead, an id is never released again along a run. -/
theorem dead_no_release {c acts c' r} (hs : Steps Sig c acts c') (hf : Fresh c.live c.next)
    (hd : Dead r c) : Act.resume r ∉ acts ∧ Act.consume r ∉ acts := by
  have h0 := (release_at_most_once hs hf r).2.1 hd
  constructor
  · intro hm
    have := count_resume_le_rel r acts
    have := List.count_pos_iff.mpr hm; omega
  · intro hm
    have := count_consume_le_rel r acts
    have := List.count_pos_iff.mpr hm; omega

/-- A live handle is never resumed along any run. -/
theorem live_hdl_never_resumed {c acts c' r} (hs : Steps Sig c acts c') (hf : Fresh c.live c.next)
    (hl : c.live r = some .hdl) : Act.resume r ∉ acts := by
  induction hs with
  | refl => simp
  | @cons c a c1 acts c' h1 hrest ih =>
    have hf1 := step_fresh h1 hf
    obtain ⟨e, L, n, e', L', n', rfl, rfl, _, _, hh⟩ := step_parts h1
    simp only at hl hf hf1
    intro hm
    simp only [List.mem_cons] at hm
    by_cases hrel : isRel r a = true
    · obtain ⟨hlive, hdead⟩ := head_release hh hrel
      have hrn : r < n := by
        apply Classical.byContradiction; intro h
        exact hlive (hf r (by omega))
      rcases hm with h | h
      · subst h; obtain ⟨q, hq⟩ := head_resume_kont hh; rw [hl] at hq; cases hq
      · exact (dead_no_release hrest hf1 ⟨Nat.lt_of_lt_of_le hrn (head_next hh), hdead⟩).1 h
    · have hrel' : isRel r a = false := by simpa using hrel
      rcases hm with h | h
      · subst h; simp [isRel] at hrel'
      · exact ih hf1 (head_keep hh hf hl hrel') h

/-- A live continuation is never consumed along any run. -/
theorem live_kont_never_consumed {c acts c' r q} (hs : Steps Sig c acts c') (hf : Fresh c.live c.next)
    (hl : c.live r = some (.kont q)) : Act.consume r ∉ acts := by
  induction hs with
  | refl => simp
  | @cons c a c1 acts c' h1 hrest ih =>
    have hf1 := step_fresh h1 hf
    obtain ⟨e, L, n, e', L', n', rfl, rfl, _, _, hh⟩ := step_parts h1
    simp only at hl hf hf1
    intro hm
    simp only [List.mem_cons] at hm
    by_cases hrel : isRel r a = true
    · obtain ⟨hlive, hdead⟩ := head_release hh hrel
      have hrn : r < n := by
        apply Classical.byContradiction; intro h
        exact hlive (hf r (by omega))
      rcases hm with h | h
      · subst h; have hq := head_consume_hdl hh; rw [hl] at hq; cases hq
      · exact (dead_no_release hrest hf1 ⟨Nat.lt_of_lt_of_le hrn (head_next hh), hdead⟩).2 h
    · have hrel' : isRel r a = false := by simpa using hrel
      rcases hm with h | h
      · subst h; simp [isRel] at hrel'
      · exact ih hf1 (head_keep hh hf hl hrel') h

/-- **Theorem 2, handle kind**: a minted handle is never released by
`resume`, along any run from a fresh store. -/
theorem minted_never_resumed {c acts c' r} (hs : Steps Sig c acts c') (hf : Fresh c.live c.next)
    (hm : Act.mint r ∈ acts) : Act.resume r ∉ acts := by
  induction hs with
  | refl => simp at hm
  | @cons c a c1 acts c' h1 hrest ih =>
    have hf1 := step_fresh h1 hf
    intro hres
    simp only [List.mem_cons] at hm hres
    by_cases hnow : a = .mint r
    · subst hnow
      obtain ⟨e, L, n, e', L', n', rfl, rfl, _, _, hh⟩ := step_parts h1
      have hl : L' r = some .hdl := by
        rcases head_alloc hh (q := .linear) (Or.inl rfl) with ⟨h, _⟩ | ⟨_, h⟩
        · cases h
        · exact h
      rcases hres with h | h
      · cases h
      · exact live_hdl_never_resumed hrest hf1 hl h
    · rcases hm with h | hm
      · exact hnow h.symm
      · rcases hres with h | h
        · subst h
          obtain ⟨e, L, n, e', L', n', rfl, rfl, _, _, hh⟩ := step_parts h1
          obtain ⟨q, hq⟩ := head_resume_kont hh
          have hrn : r < n := by
            apply Classical.byContradiction; intro h
            have := hf r (by simp only; omega); simp only at this; rw [this] at hq; cases hq
          have h3 : n' ≤ r := steps_alloc_ge hrest (q := .linear) (Or.inl hm)
          have := head_next hh
          omega
        · exact ih hf1 hm h

/-- **Theorem 2, continuation kind**: a captured continuation is never
released by `consume`, along any run from a fresh store. -/
theorem captured_never_consumed {c acts c' r q} (hs : Steps Sig c acts c') (hf : Fresh c.live c.next)
    (hm : Act.capture r q ∈ acts) : Act.consume r ∉ acts := by
  induction hs with
  | refl => simp at hm
  | @cons c a c1 acts c' h1 hrest ih =>
    have hf1 := step_fresh h1 hf
    intro hcon
    simp only [List.mem_cons] at hm hcon
    by_cases hnow : a = .capture r q
    · subst hnow
      obtain ⟨e, L, n, e', L', n', rfl, rfl, _, _, hh⟩ := step_parts h1
      have hl : L' r = some (.kont q) := by
        rcases head_alloc hh (q := q) (Or.inr rfl) with ⟨_, h⟩ | ⟨h, _⟩
        · exact h
        · cases h
      rcases hcon with h | h
      · cases h
      · exact live_kont_never_consumed hrest hf1 hl h
    · rcases hm with h | hm
      · exact hnow h.symm
      · rcases hcon with h | h
        · subst h
          obtain ⟨e, L, n, e', L', n', rfl, rfl, _, _, hh⟩ := step_parts h1
          have hq := head_consume_hdl hh
          have hrn : r < n := by
            apply Classical.byContradiction; intro h
            have := hf r (by simp only; omega); simp only at this; rw [this] at hq; cases hq
          have h3 : n' ≤ r := steps_alloc_ge hrest (q := q) (Or.inr hm)
          have := head_next hh
          omega
        · exact ih hf1 hm h

/-- **Theorem 2 per kind (Total Linear Exhaustion)**. In a closed program
typed at mode copy with no unhandled user effects (aborts allowed), every run
that terminates in a value or an abort consumes every minted handle exactly
once and never resumes it, and resumes every captured linear continuation
exactly once and never consumes it. -/
theorem total_linear_exhaustion_per_kind {e τ ε acts c'}
    (ht : HasType Sig Ctx.empty RCtx.empty e τ .copy ε) (hε : ∀ l, Label.user l ∉ ε)
    (hs : Steps Sig ⟨e, RCtx.empty, 0⟩ acts c') (hfin : IsVal c'.e ∨ c'.e = .abort) :
    (∀ r, Act.mint r ∈ acts → acts.count (.consume r) = 1 ∧ acts.count (.resume r) = 0) ∧
    (∀ r, Act.capture r .linear ∈ acts → acts.count (.resume r) = 1 ∧ acts.count (.consume r) = 0) := by
  have hf0 : Fresh RCtx.empty 0 := fun _ _ => rfl
  constructor
  · intro r hm
    have h1 := total_linear_exhaustion ht hε hs hfin r (Or.inl hm)
    have h2 : acts.count (.resume r) = 0 := List.count_eq_zero.mpr (minted_never_resumed hs hf0 hm)
    rw [relCount_eq] at h1; omega
  · intro r hm
    have h1 := total_linear_exhaustion ht hε hs hfin r (Or.inr hm)
    have h2 : acts.count (.consume r) = 0 := List.count_eq_zero.mpr (captured_never_consumed hs hf0 hm)
    rw [relCount_eq] at h1; omega

end PerKind

/-! ## Effect-row variables: instantiation

Rows may contain row variables `ρ` (`Label.rvar`). A row variable is abstract
in the typing judgment: no operation performs it and `res` counts it as
affine. An instantiation `θ` replaces every row variable by a row. The
theorem `inst_ty` shows that typing is stable under every instantiation, so
row-polymorphic code (typed once with ρ in Δ) may be used at every row. -/

section RowVars
variable {Sig : Nat → EffSig}

def Label.inst (θ : Nat → Row) : Label → Row
  | .rvar n => θ n
  | l => [l]

def Row.inst (θ : Nat → Row) (ε : Row) : Row := ε.flatMap (Label.inst θ)

def Ty.inst (θ : Nat → Row) : Ty → Ty
  | .unit => .unit
  | .nat => .nat
  | .hdl => .hdl
  | .pair md t1 m1 t2 m2 => .pair md (t1.inst θ) m1 (t2.inst θ) m2
  | .kont σ st ms τ am row q => .kont (σ.inst θ) (st.inst θ) ms (τ.inst θ) am (Row.inst θ row) q
  | .arrow σ τ am row mf => .arrow (σ.inst θ) (τ.inst θ) am (Row.inst θ row) mf

def Ctx.inst (θ : Nat → Row) (Γ : Ctx) : Ctx := fun i => (Γ i).map (fun b => (b.1.inst θ, b.2))

/-- The effect signature mentions no row variable that `θ` changes. -/
def SigInv (Sig : Nat → EffSig) (θ : Nat → Row) : Prop :=
  ∀ l o, ((Sig l).arg o).inst θ = (Sig l).arg o ∧ ((Sig l).res o).inst θ = (Sig l).res o

theorem kind_inst (θ : Nat → Row) (t : Ty) : (t.inst θ).kind = t.kind := by
  induction t with
  | pair md t1 m1 t2 m2 ih1 ih2 => simp only [Ty.inst, Ty.kind, ih1, ih2]
  | _ => rfl

theorem at_inst (θ : Nat → Row) (p : Path) : ∀ t : Ty, (t.inst θ).at p = (t.at p).map (Ty.inst θ) := by
  induction p with
  | nil => intro t; cases t <;> rfl
  | cons b p ih =>
    intro t
    cases t with
    | pair md t1 m1 t2 m2 => cases b <;> simp only [Ty.inst, Ty.at, ih]
    | _ => rfl

theorem mem_inst_of {θ : Nat → Row} {ε : Row} {l : Label} (h : l ∈ ε) (hl : ∀ n, l ≠ .rvar n) :
    l ∈ Row.inst θ ε := by
  simp only [Row.inst, List.mem_flatMap]
  refine ⟨l, h, ?_⟩
  cases l with
  | rvar n => exact absurd rfl (hl n)
  | _ => simp [Label.inst]

theorem rowSub_inst {θ : Nat → Row} {a b : Row} (h : RowSub a b) : RowSub (Row.inst θ a) (Row.inst θ b) := by
  intro x hx
  simp only [Row.inst, List.mem_flatMap] at hx ⊢
  obtain ⟨l, hl, hx⟩ := hx
  exact ⟨l, h l hl, hx⟩

theorem inst_cons_user (θ : Nat → Row) (l : Nat) (ε : Row) :
    Row.inst θ (.user l :: ε) = .user l :: Row.inst θ ε := by
  simp [Row.inst, Label.inst]

/-- Instantiating row variables can only raise `res`. -/
theorem res_inst (θ : Nat → Row) (ε : Row) : RMode.Le (res Sig ε) (res Sig (Row.inst θ ε)) := by
  cases hr : res Sig ε with
  | affine => exact RMode.affine_le _
  | linear =>
    have hno : ∀ l, l ∈ ε → qOf Sig l ≠ .affine := by
      intro l hl hq
      have : ε.any (fun l => qOf Sig l == .affine) = true := List.any_eq_true.mpr ⟨l, hl, by simp [hq]⟩
      simp [res, this] at hr
    have : (Row.inst θ ε).any (fun l => qOf Sig l == .affine) = false := by
      apply Bool.eq_false_iff.mpr
      intro h
      obtain ⟨x, hx, hq⟩ := List.any_eq_true.mp h
      simp only [Row.inst, List.mem_flatMap] at hx
      obtain ⟨l, hl, hx⟩ := hx
      cases l with
      | rvar n => exact hno _ hl rfl
      | abort => exact hno _ hl rfl
      | user u =>
        simp [Label.inst] at hx; subst hx
        exact hno _ hl (by simpa using hq)
    simp [res, this]; exact RMode.le_refl _

theorem inst_ext (θ : Nat → Row) (Γ : Ctx) (b : Ty × Mode) :
    (Γ.ext b).inst θ = (Γ.inst θ).ext (b.1.inst θ, b.2) := by
  funext i; cases i <;> rfl

theorem inst_empty (θ : Nat → Row) : Ctx.empty.inst θ = Ctx.empty := rfl

theorem inst_two (θ : Nat → Row) (b1 b0 : Ty × Mode) :
    (Ctx.two b1 b0).inst θ = Ctx.two (b1.1.inst θ, b1.2) (b0.1.inst θ, b0.2) := by
  simp only [Ctx.two, Ctx.one, inst_ext, inst_empty]

theorem splitE_inst {θ : Nat → Row} {x y z : Option (Ty × Mode)} (h : SplitE x y z) :
    SplitE (x.map (fun b => (b.1.inst θ, b.2))) (y.map (fun b => (b.1.inst θ, b.2)))
      (z.map (fun b => (b.1.inst θ, b.2))) := by
  cases h with
  | none => exact .none
  | left b => exact .left _
  | right b => exact .right _
  | copy t => exact .copy _

theorem split_inst {θ : Nat → Row} {Γ Γ1 Γ2 : Ctx} (h : Split Γ Γ1 Γ2) :
    Split (Γ.inst θ) (Γ1.inst θ) (Γ2.inst θ) := fun i => splitE_inst (h i)

theorem discard_inst {θ : Nat → Row} {Γ : Ctx} (h : Discard Γ) : Discard (Γ.inst θ) := by
  intro i t m hi; simp only [Ctx.inst] at hi
  cases hg : Γ i with
  | none => rw [hg] at hi; cases hi
  | some b => rw [hg] at hi; cases hi; exact h i b.1 b.2 hg

theorem allCopy_inst {θ : Nat → Row} {Γ : Ctx} (h : AllCopy Γ) : AllCopy (Γ.inst θ) := by
  intro i t m hi; simp only [Ctx.inst] at hi
  cases hg : Γ i with
  | none => rw [hg] at hi; cases hi
  | some b => rw [hg] at hi; cases hi; exact h i b.1 b.2 hg

theorem frameOK_inst {θ : Nat → Row} {Γ : Ctx} {R : RCtx} {ε : Row} (h : FrameOK Γ R (res Sig ε)) :
    FrameOK (Γ.inst θ) R (res Sig (Row.inst θ ε)) := by
  obtain ⟨hg, hr⟩ := h
  refine ⟨fun i t m hi => ?_, fun r e he => RMode.le_trans (hr r e he) (res_inst θ ε)⟩
  simp only [Ctx.inst] at hi
  cases hgi : Γ i with
  | none => rw [hgi] at hi; cases hi
  | some b => rw [hgi] at hi; cases hi; exact Mode.le_trans (hg i b.1 b.2 hgi) (res_inst θ ε)

theorem clauseCtx_inst {θ : Nat → Row} (hSig : SigInv Sig θ) (Γh : Ctx) (l o : Nat) (τs : Ty) (ms : Mode)
    (τ : Ty) (m : Mode) (εc : Row) :
    (clauseCtx Sig Γh l o τs ms τ m εc).inst θ =
      clauseCtx Sig (Γh.inst θ) l o (τs.inst θ) ms (τ.inst θ) m (Row.inst θ εc) := by
  simp only [clauseCtx, inst_ext, Ty.inst, (hSig l o).1, (hSig l o).2]

/-- **Row instantiation**: typing is stable under every instantiation of the
effect-row variables (for an effect signature they do not occur in). -/
theorem inst_ty {θ : Nat → Row} (hSig : SigInv Sig θ) {Γ R e τ m ε} (h : HasType Sig Γ R e τ m ε) :
    HasType Sig (Γ.inst θ) R e (τ.inst θ) m (Row.inst θ ε) := by
  induction h with
  | @var Γ R i τ mx m ε hi hj hR hk =>
    refine .var (mx := mx) (by simp [Ctx.inst, hi]) ?_ hR (by rw [kind_inst]; exact hk)
    intro j t m' hne hj'
    simp only [Ctx.inst] at hj'
    cases hg : Γ j with
    | none => rw [hg] at hj'; cases hj'
    | some b => rw [hg] at hj'; cases hj'; exact hj j b.1 b.2 hne hg
  | unitV hΓ hR => exact .unitV (discard_inst hΓ) hR
  | natV n hΓ hR => exact .natV n (discard_inst hΓ) hR
  | hdlV r hΓ hsR hd hm => exact .hdlV r (discard_inst hΓ) hsR hd hm
  | kontV r body hΓ hsR hd hs0 _ hle hm ihb =>
    rw [inst_two] at ihb
    simp only at ihb
    rw [← kind_inst θ] at ihb
    exact .kontV r body (discard_inst hΓ) hsR hd hs0 ihb hle hm
  | clos ha _ hle _ hm iha ihb =>
    rw [inst_two] at ihb
    simp only at ihb
    rw [← kind_inst θ] at ihb
    exact .clos ha iha hle ihb hm
  | @pair _ _ _ _ _ _ _ _ md t1 m1 t2 m2 _ _ ha hb hsΓ hsR _ _ hk1 hk2 hk ih1 ih2 =>
    refine .pair ha hb (split_inst hsΓ) hsR ?_ ?_ ?_ ?_ ?_
    · rw [kind_inst]; exact ih1
    · rw [kind_inst]; exact ih2
    · rw [kind_inst]; exact hk1
    · rw [kind_inst]; exact hk2
    · have := kind_inst θ (Ty.pair md t1 m1 t2 m2); simp only [Ty.inst] at this; rw [this]; exact hk
  | letE hsΓ hsR _ _ hf h1s h2s ih1 ih2 =>
    rw [inst_ext] at ih2
    exact .letE (split_inst hsΓ) hsR ih1 ih2 (frameOK_inst hf) (rowSub_inst h1s) (rowSub_inst h2s)
  | @letPair _ _ _ _ _ _ _ _ _ t1 _ t2 _ _ _ _ _ ha hsΓ hsR _ _ iha ihe =>
    rw [inst_ext, inst_ext] at ihe
    refine .letPair ha (split_inst hsΓ) hsR iha ?_
    rw [kind_inst, kind_inst]; exact ihe
  | ifz ha hsΓ hsR _ _ _ iha ih1 ih2 => exact .ifz ha (split_inst hsΓ) hsR iha ih1 ih2
  | mint hΓ hR hm => exact .mint (discard_inst hΓ) hR hm
  | consume ha _ ih => exact .consume ha ih
  | arith ha hb hsΓ hsR _ _ hab iha ihb =>
    exact .arith ha hb (split_inst hsΓ) hsR iha ihb (mem_inst_of hab (fun _ h => by cases h))
  | arithLiteral hΓ hR ha hb hev => exact .arithLiteral (discard_inst hΓ) hR ha hb hev
  | abort hΓ hR hab => exact .abort (discard_inst hΓ) hR (mem_inst_of hab (fun _ h => by cases h))
  | @perform Γ R l o a m ε ho ha _ hl hm ih =>
    rw [(hSig l o).1] at ih
    rw [(hSig l o).2]
    exact .perform ho ha ih (mem_inst_of hl (fun _ h => by cases h)) hm
  | handle hsa hsΓ hsΓ' hc hsR _ hms _ hsub _ hall _ hc' ihs ihb ihr ihc =>
    rw [inst_ext, inst_ext] at ihr
    refine .handle hsa (split_inst hsΓ) (split_inst hsΓ') (allCopy_inst hc) hsR ihs
      (Mode.le_trans hms (res_inst θ _)) ihb ?_ ihr hall ?_ (rowSub_inst hc')
    · rw [← inst_cons_user]; exact rowSub_inst hsub
    · intro o c hc0
      have := ihc o c hc0
      rw [clauseCtx_inst hSig] at this
      exact this
  | resume hk ha hsa hsΓ hsΓ' hsR hsR' _ _ _ hsub ihk iha ihs =>
    exact .resume hk ha hsa (split_inst hsΓ) (split_inst hsΓ') hsR hsR' ihk iha ihs (rowSub_inst hsub)
  | app hf ha hsΓ hsR _ _ hsub ihf iha =>
    exact .app hf ha (split_inst hsΓ) hsR ihf iha (rowSub_inst hsub)
  | @get _ _ _ τ _ _ σ _ _ ha _ hp hc hm ih =>
    refine .get ha ih (by rw [at_inst, hp]; rfl) (by rw [kind_inst]; exact hc) ?_
    rw [show leafRoot (σ.inst θ) (τ.inst θ) = (leafRoot σ τ).inst θ from rfl, kind_inst]; exact hm
  | @swap _ _ _ _ _ _ _ _ τ _ _ σ _ _ ha hb hsΓ hsR _ hp _ hm iha ihb =>
    refine .swap ha hb (split_inst hsΓ) hsR iha (by rw [at_inst, hp]; rfl) ihb ?_
    rw [show leafRoot (σ.inst θ) (τ.inst θ) = (leafRoot σ τ).inst θ from rfl, kind_inst]; exact hm
  | swap2 ha _ hp hq hd hm ih =>
    exact .swap2 ha ih (by rw [at_inst, hp]; rfl) (by rw [at_inst, hq]; rfl) hd (by rw [kind_inst]; exact hm)

/-- A closed program typed with row variables is safe at every
instantiation whose row has no unhandled user effect. -/
theorem row_poly_safety {θ : Nat → Row} (hSig : SigInv Sig θ) {e τ m ε acts c'}
    (ht : HasType Sig Ctx.empty RCtx.empty e τ m ε) (hε : ∀ l, Label.user l ∉ Row.inst θ ε)
    (hs : Steps Sig ⟨e, RCtx.empty, 0⟩ acts c') :
    WT Sig c' (τ.inst θ) m (Row.inst θ ε) ∧ (IsVal c'.e ∨ c'.e = .abort ∨ ∃ a c'', Step Sig c' a c'') :=
  type_safety hs ⟨inst_ty hSig ht, fun _ _ => rfl⟩ hε

end RowVars

/-! ## Towards the Zero-Pin corollary: captured continuations are self-contained

The spec's corollary is about machine stacks: a suspended fiber stack holds
no first-class pointer into itself, so it can be moved without `Pin`. This
calculus has no addresses and no second-class borrows in Γ, so the corollary
cannot be stated literally (see the report). What it does show is the
syntactic core: the body of every continuation a well-typed program captures
mentions no variable except its two parameters (the operation result and the
next state). It refers to the rest of the program only through store ids,
which are location-independent names, so nothing in it depends on where the
suspended frames live. -/

section ZeroPin
variable {Sig : Nat → EffSig}

/-- `Below n e`: every free variable of `e` is below `n` (clause chains are
checked through `nthC`, as the typing rule checks them). -/
inductive Below : Nat → Expr → Prop where
  | var {n i} : i < n → Below n (.var i)
  | unitV {n} : Below n .unitV
  | natV {n k} : Below n (.natV k)
  | hdlV {n r} : Below n (.hdlV r)
  | kontV {n r body} : Below 2 body → Below n (.kontV r body)
  | clos {n a body} : Below n a → Below 2 body → Below n (.clos a body)
  | pair {n a b} : Below n a → Below n b → Below n (.pair a b)
  | letE {n e1 e2} : Below n e1 → Below (n + 1) e2 → Below n (.letE e1 e2)
  | letPair {n a e} : Below n a → Below (n + 2) e → Below n (.letPair a e)
  | ifz {n a e1 e2} : Below n a → Below n e1 → Below n e2 → Below n (.ifz a e1 e2)
  | mint {n} : Below n .mint
  | consume {n a} : Below n a → Below n (.consume a)
  | arith {n op a b} : Below n a → Below n b → Below n (.arith op a b)
  | abort {n} : Below n .abort
  | perform {n l o a} : Below n a → Below n (.perform l o a)
  | handle {n l s b r cls} : Below n s → Below n b → Below (n + 2) r →
      (∀ o c, nthC cls o = some c → Below (n + 3) c) → Below n (.handle l s b r cls)
  | resume {n k a s} : Below n k → Below n a → Below n s → Below n (.resume k a s)
  | app {n f a} : Below n f → Below n a → Below n (.app f a)
  | get {n a p} : Below n a → Below n (.get a p)
  | swap {n a p b} : Below n a → Below n b → Below n (.swap a p b)
  | swap2 {n a p q} : Below n a → Below n (.swap2 a p q)

/-- Γ has no binding at or above `n`. -/
def CtxBelow (Γ : Ctx) (n : Nat) : Prop := ∀ i, n ≤ i → Γ i = none

theorem ctxBelow_split_left {Γ Γ1 Γ2 : Ctx} {n} (hs : Split Γ Γ1 Γ2) (h : CtxBelow Γ n) : CtxBelow Γ1 n := by
  intro i hi; cases h1 : Γ1 i with
  | none => rfl
  | some b => have := splitE_left (hs i) h1; rw [h i hi] at this; cases this

theorem ctxBelow_split_right {Γ Γ1 Γ2 : Ctx} {n} (hs : Split Γ Γ1 Γ2) (h : CtxBelow Γ n) : CtxBelow Γ2 n := by
  intro i hi; cases h2 : Γ2 i with
  | none => rfl
  | some b => have := splitE_right (hs i) h2; rw [h i hi] at this; cases this

theorem ctxBelow_ext {Γ : Ctx} {n} (h : CtxBelow Γ n) (b : Ty × Mode) : CtxBelow (Γ.ext b) (n + 1) := by
  intro i hi; cases i with
  | zero => omega
  | succ i => exact h i (by omega)

theorem ctxBelow_two (b1 b0 : Ty × Mode) : CtxBelow (Ctx.two b1 b0) 2 := by
  intro i hi
  match i, hi with
  | i + 2, _ => rfl

/-- Well-typed terms mention only the variables of their context. -/
theorem typed_below {Γ R e τ m ε} (h : HasType Sig Γ R e τ m ε) : ∀ {n}, CtxBelow Γ n → Below n e := by
  induction h with
  | var hi =>
    intro n hn
    refine .var ?_
    apply Classical.byContradiction; intro h'
    rw [hn _ (by omega)] at hi; cases hi
  | unitV => intro _ _; exact .unitV
  | natV => intro _ _; exact .natV
  | hdlV => intro _ _; exact .hdlV
  | kontV _ _ _ _ _ _ _ _ _ ihb => intro _ _; exact .kontV (ihb (ctxBelow_two _ _))
  | clos _ _ _ _ _ iha ihb => intro _ hn; exact .clos (iha hn) (ihb (ctxBelow_two _ _))
  | pair _ _ hsΓ _ _ _ _ _ _ ih1 ih2 =>
    intro _ hn
    exact .pair (ih1 (ctxBelow_split_left hsΓ hn)) (ih2 (ctxBelow_split_right hsΓ hn))
  | letE hsΓ _ _ _ _ _ _ ih1 ih2 =>
    intro _ hn
    exact .letE (ih1 (ctxBelow_split_left hsΓ hn)) (ih2 (ctxBelow_ext (ctxBelow_split_right hsΓ hn) _))
  | letPair _ hsΓ _ _ _ iha ihe =>
    intro _ hn
    exact .letPair (iha (ctxBelow_split_left hsΓ hn))
      (ihe (ctxBelow_ext (ctxBelow_ext (ctxBelow_split_right hsΓ hn) _) _))
  | ifz _ hsΓ _ _ _ _ iha ih1 ih2 =>
    intro _ hn
    exact .ifz (iha (ctxBelow_split_left hsΓ hn)) (ih1 (ctxBelow_split_right hsΓ hn))
      (ih2 (ctxBelow_split_right hsΓ hn))
  | mint => intro _ _; exact .mint
  | consume _ _ ih => intro _ hn; exact .consume (ih hn)
  | arith _ _ hsΓ _ _ _ _ iha ihb =>
    intro _ hn
    exact .arith (iha (ctxBelow_split_left hsΓ hn)) (ihb (ctxBelow_split_right hsΓ hn))
  | arithLiteral => intro _ _; exact .arith .natV .natV
  | abort => intro _ _; exact .abort
  | perform _ _ _ _ _ ih => intro _ hn; exact .perform (ih hn)
  | handle _ hsΓ hsΓ' _ _ _ _ _ _ _ _ _ _ ihs ihb ihr ihc =>
    intro n hn
    have h' := ctxBelow_split_right hsΓ hn
    have hh := ctxBelow_split_right hsΓ' h'
    refine .handle (ihs (ctxBelow_split_left hsΓ hn)) (ihb (ctxBelow_split_left hsΓ' h'))
      (ihr (ctxBelow_ext (ctxBelow_ext hh _) _)) ?_
    intro o c hc
    exact ihc o c hc (ctxBelow_ext (ctxBelow_ext (ctxBelow_ext hh _) _) _)
  | resume _ _ _ hsΓ hsΓ' _ _ _ _ _ _ ihk iha ihs =>
    intro _ hn
    have h' := ctxBelow_split_right hsΓ hn
    exact .resume (ihk (ctxBelow_split_left hsΓ hn)) (iha (ctxBelow_split_left hsΓ' h'))
      (ihs (ctxBelow_split_right hsΓ' h'))
  | app _ _ hsΓ _ _ _ _ ihf iha =>
    intro _ hn
    exact .app (ihf (ctxBelow_split_left hsΓ hn)) (iha (ctxBelow_split_right hsΓ hn))
  | get _ _ _ _ _ ih => intro _ hn; exact .get (ih hn)
  | swap _ _ hsΓ _ _ _ _ _ iha ihb =>
    intro _ hn
    exact .swap (iha (ctxBelow_split_left hsΓ hn)) (ihb (ctxBelow_split_right hsΓ hn))
  | swap2 _ _ _ _ _ _ ih => intro _ hn; exact .swap2 (ih hn)

/-- The continuation captured from a well-typed handler is well typed in its
own two-parameter context. -/
theorem captured_kont_typed {R l s E o v ret cls τ m ε}
    (h : HasType Sig Ctx.empty R (.handle l s (plug E (.perform l o v)) ret cls) τ m ε) :
    ∃ τs ms εc RE, HasType Sig (Ctx.two ((Sig l).res o, ((Sig l).res o).kind) (τs, ms)) RE (kontBody l E ret cls) τ m εc := by
  cases h with
  | @handle _ _ _ _ _ _ _ _ _ _ _ _ _ τs ms _ _ _ _ _ εc _ hsa hsΓ hsΓ' _ hsR hst hms hb hsub hr hall hcl hc' =>
    obtain ⟨g1, g2, g3, g4⟩ := split3_empty hsΓ hsΓ'
    subst g1; subst g2; subst g3; subst g4
    obtain ⟨Rp, RE, τh, mh, εh, hsp, hperf, hrepl, hfr⟩ := plug_inv E hb
    cases hperf with
    | perform _ _ hva hl hm =>
      obtain ⟨_, _, _, _, _, hks⟩ := val_inv hst (atom_closed hsa hst)
      exact ⟨τs, ms, εc, RE, .handle (.var 0) (split_two _ _) (split_left_all _) allCopy_empty
        (splitR_empty_left _) (var_only0 (b := (τs, ms)) hks) hms
        (hrepl _ RCtx.empty RE _ (var_only1 hm) (splitR_empty_left _)) hsub hr hall hcl (rowSub_refl _)⟩

/-- **Self-contained continuations** (the syntactic core of the Zero-Pin
corollary for this model): along any well-typed run, every continuation
captured by a step has a body whose only free variables are its two
parameters. -/
theorem captured_kont_self_contained {c a c' τ m ε} (hwt : WT Sig c τ m ε) (hs : Step Sig c a c')
    {r q} (ha : a = .capture r q) :
    ∃ E0 E l ret cls, Below 2 (kontBody l E ret cls) ∧
      ∃ e0', c'.e = plug E0 e0' ∧ ∃ v s c, e0' = subst 0 v (subst 0 s (subst 0
        (.kontV r (kontBody l E ret cls)) c)) := by
  cases hs with
  | @ctx e L n _ e' L' n' E0 hh =>
    subst ha
    cases hh with
    | @capture l o E v s ret cls cl _ _ hfree hv hsv hc =>
      obtain ⟨ht, _⟩ := hwt
      obtain ⟨_, _, _, _, _, _, hhole, _, _⟩ := plug_inv E0 ht
      obtain ⟨_, _, _, _, hbody⟩ := captured_kont_typed hhole
      exact ⟨E0, E, l, ret, cls, typed_below hbody (ctxBelow_two _ _), _, rfl, v, s, cl, rfl⟩

end ZeroPin

/-! ## Worked examples -/

section Examples

/-- Effect 0 is a linear (Storage-like) effect with two operations,
`read : nat → nat` and `flush : unit → nat`; every other label is an affine
(Fail-like) effect with one operation `raise : unit → nat`. -/
def exSig : Nat → EffSig
  | 0 => ⟨.linear, 2, fun | 0 => .nat | _ => .unit, fun _ => .nat⟩
  | _ => ⟨.affine, 1, fun _ => .unit, fun _ => .nat⟩

theorem exSig_inv (θ : Nat → Row) : SigInv exSig θ := by
  intro l o
  cases l with
  | zero => cases o <;> exact ⟨rfl, rfl⟩
  | succ l => exact ⟨rfl, rfl⟩

/-- A context with a single binding at index `i`. -/
def Ctx.at (i : Nat) (b : Ty × Mode) : Ctx := fun j => if j = i then some b else none

theorem var_at {Sig : Nat → EffSig} {i : Nat} {b : Ty × Mode} {m ε} (hk : Mode.Le b.1.kind m) :
    HasType Sig (Ctx.at i b) RCtx.empty (.var i) b.1 m ε :=
  .var (mx := b.2) (by simp [Ctx.at]) (fun j t m' hj h => by
      simp only [Ctx.at] at h; split at h
      · exact absurd (by assumption) hj
      · cases h)
    discardR_empty hk

theorem frame_empty (q : RMode) : FrameOK Ctx.empty RCtx.empty q :=
  ⟨fun _ _ _ h => (by cases h), fun _ _ h => (by cases h)⟩

theorem discard_two {t1 t0 : Ty} {m1 m0 : Mode} (h1 : Mode.Le m1 .affine) (h0 : Mode.Le m0 .affine) :
    Discard (Ctx.two (t1, m1) (t0, m0)) := by
  intro i t m h
  match i with
  | 0 => simp only [Ctx.two, Ctx.ext, Option.some.injEq, Prod.mk.injEq] at h; rw [← h.2]; exact h0
  | 1 =>
    simp only [Ctx.two, Ctx.one, Ctx.ext, Option.some.injEq, Prod.mk.injEq] at h; rw [← h.2]; exact h1
  | i + 2 => simp [Ctx.two, Ctx.one, Ctx.ext, Ctx.empty] at h

theorem discard_one_copy (t : Ty) : Discard (Ctx.one (t, .copy)) := by
  intro i t' m h
  cases i with
  | zero => simp [Ctx.one, Ctx.ext] at h; rw [← h.2]; decide
  | succ i => simp [Ctx.one, Ctx.ext, Ctx.empty] at h

/-! ### [T-Abs]: the capture premise -/

/-- A copy closure cannot capture a linear handle: `m_z ⊑ m_fn` fails. -/
theorem linear_capture_copy_closure_rejected (R : RCtx) (body : Expr) (σ τ : Ty) (am m : Mode)
    (εf ε : Row) :
    ¬ HasType exSig Ctx.empty R (.clos (.hdlV 0) body) (.arrow σ τ am εf .copy) m ε := by
  intro h
  cases h with
  | clos _ ha hle _ _ =>
    cases ha with
    | hdlV _ _ _ _ hm => exact absurd (Mode.le_trans hm hle) (by decide)

/-- `let t = mint in let f = (λ_. consume t) in f ()`: a closure capturing a
linear handle is linear and runs once. -/
def linClosure : Expr :=
  .letE .mint (.letE (.clos (.var 0) (.consume (.var 1))) (.app (.var 0) .unitV))

theorem lin_closure_typed : HasType exSig Ctx.empty RCtx.empty linClosure .unit .copy [] := by
  refine .letE (split_left_all _) (splitR_empty_left _) (ε1 := []) (τ1 := .hdl) (m1 := .linear)
    (.mint discard_empty discardR_empty (Mode.le_refl _)) ?_ (frame_empty _) (rowSub_refl _) (rowSub_refl _)
  refine .letE (split_left_all _) (splitR_empty_left _) (ε1 := [])
    (τ1 := .arrow .unit .unit .copy [] .linear) (m1 := .linear)
    (.clos (.var 0) (var_only0 (b := (.hdl, .linear)) (Mode.le_refl _)) (Mode.le_refl _) ?_ (Mode.le_refl _))
    ?_ (frame_empty _) (rowSub_refl _) (rowSub_refl _)
  · refine .consume (.var 1) (.var (mx := .linear) rfl ?_ discardR_empty (Mode.le_refl _))
    intro j t m' hj h
    match j with
    | 0 => simp [Ctx.two, Ctx.ext] at h; rw [← h.2]; decide
    | 1 => exact absurd rfl hj
    | j + 2 => simp [Ctx.two, Ctx.one, Ctx.ext, Ctx.empty] at h
  · exact .app (.var 0) (.val .unitV) (split_left_all _) (splitR_empty_left _)
      (var_only0 (b := (.arrow .unit .unit .copy [] .linear, .linear)) (Mode.le_refl _))
      (.unitV discard_empty discardR_empty) (rowSub_refl _)

theorem lin_closure_runs : ∃ L n,
    Steps exSig ⟨linClosure, RCtx.empty, 0⟩ [.mint 0, .silent, .silent, .silent, .consume 0] ⟨.unitV, L, n⟩ := by
  refine ⟨?L, ?n, ?main⟩
  case main =>
    apply Steps.cons (Step.ctx (.letC .hole (.letE (.clos (.var 0) (.consume (.var 1))) (.app (.var 0) .unitV)))
      Head.mint)
    apply Steps.cons (Step.ctx .hole (Head.letV (.hdlV 0)))
    apply Steps.cons (Step.ctx .hole (Head.letV (.clos _ (.hdlV 0))))
    apply Steps.cons (Step.ctx .hole (Head.app (.hdlV 0) .unitV))
    apply Steps.cons (Step.ctx .hole (Head.consume (by simp [RCtx.set])))
    exact .refl _

/-- Theorem 2 per kind on that run: the captured handle is consumed exactly
once (when the closure runs) and never resumed. -/
theorem lin_closure_exhausts :
    ∀ L n, Steps exSig ⟨linClosure, RCtx.empty, 0⟩ [.mint 0, .silent, .silent, .silent, .consume 0]
      ⟨.unitV, L, n⟩ →
      [Act.mint 0, .silent, .silent, .silent, .consume 0].count (.consume 0) = 1 ∧
      [Act.mint 0, .silent, .silent, .silent, .consume 0].count (.resume 0) = 0 :=
  fun _ _ hs => (total_linear_exhaustion_per_kind lin_closure_typed (fun l h => by simp at h) hs
    (Or.inl .unitV)).1 0 (by simp)

/-- A linear closure cannot be called twice: `let t = mint in let f = (λ_.
consume t) in f (); f ()` is rejected, because `f` is a linear binding and
context splitting does not contract it. -/
def linTwice : Expr :=
  .letE .mint (.letE (.clos (.var 0) (.consume (.var 1)))
    (.letE (.app (.var 0) .unitV) (.app (.var 1) .unitV)))

theorem lin_closure_twice_rejected (τ : Ty) (m : Mode) (ε : Row) :
    ¬ HasType exSig Ctx.empty RCtx.empty linTwice τ m ε := by
  intro h
  cases h with
  | letE hsΓ _ h1 h2 _ _ _ =>
    obtain ⟨g1, g2⟩ := split_empty hsΓ; subst g1; subst g2
    cases h1
    cases h2 with
    | @letE _ Γ1 Γ2 _ _ _ _ _ τ1 m1 _ _ _ _ _ hsΓ2 _ hc h3 _ _ _ =>
      cases hc with
      | clos _ hva hle _ hm =>
        cases hva with
        | var hi _ _ hk =>
          have e0 := splitE_left (hsΓ2 0) hi
          simp only [Ctx.ext, Option.some.injEq, Prod.mk.injEq] at e0
          obtain ⟨rfl, _⟩ := e0
          have hlin : Mode.Le .linear m1 := Mode.le_trans hk (Mode.le_trans hle hm)
          cases h3 with
          | letE hsΓ3 _ h4 h5 _ _ _ =>
            cases h4 with
            | app _ _ hsΓ4 _ hf _ _ =>
              cases hf with
              | var hi4 _ _ _ =>
                have e4 := splitE_left (hsΓ4 0) hi4
                cases h5 with
                | app _ _ hsΓ5 _ hf5 _ _ =>
                  cases hf5 with
                  | var hi5 _ _ _ =>
                    have e5 := splitE_left (hsΓ5 1) hi5
                    simp only [Ctx.ext] at e5
                    have h0 := hsΓ3 0
                    simp only [Ctx.ext] at h0
                    rw [e4, e5] at h0
                    cases h0
                    exact absurd hlin (by decide)

/-! ### [T-Handle] with linear state and two operations -/

/-- `let t = mint in handle 0 state s = t in { let x = read(5); flush() }
with { return(x, s) => consume s; x, read(y, s, k) => resume(k, y, s),
flush(y, s, k) => resume(k, 7, s) }`: the linear handle is threaded through
both clauses as the handler state and consumed by the return clause. -/
def stHandler : Expr :=
  .letE .mint
    (.handle 0 (.var 0)
      (.letE (.perform 0 0 (.natV 5)) (.perform 0 1 .unitV))
      (.letE (.consume (.var 0)) (.var 2))
      (.ccons (.resume (.var 0) (.var 2) (.var 1)) (.ccons (.resume (.var 0) (.natV 7) (.var 1)) .cnil)))

/-- The continuation type of both clauses. -/
def stKont : Ty := .kont .nat .hdl .linear .nat .copy [] .linear

theorem st_clause_split0 :
    Split (clauseCtx exSig Ctx.empty 0 0 .hdl .linear .nat .copy [])
      (Ctx.at 0 (stKont, .linear)) (fun j => if j = 1 then some (.hdl, .linear) else
        if j = 2 then some (.nat, .copy) else none) := by
  intro i
  match i with
  | 0 => exact .left _
  | 1 => exact .right _
  | 2 => exact .right _
  | i + 3 => exact .none

theorem st_clause_split1 :
    Split (clauseCtx exSig Ctx.empty 0 1 .hdl .linear .nat .copy [])
      (Ctx.at 0 (stKont, .linear)) (fun j => if j = 1 then some (.hdl, .linear) else
        if j = 2 then some (.unit, .copy) else none) := by
  intro i
  match i with
  | 0 => exact .left _
  | 1 => exact .right _
  | 2 => exact .right _
  | i + 3 => exact .none

theorem st_rest_split (t : Ty) :
    Split (fun j => if j = 1 then some (.hdl, .linear) else if j = 2 then some (t, .copy) else none)
      (Ctx.at 2 (t, .copy)) (Ctx.at 1 (.hdl, .linear)) := by
  intro i
  match i with
  | 0 => exact .none
  | 1 => exact .right _
  | 2 => exact .left _
  | i + 3 => exact .none

theorem st_handler_typed : HasType exSig Ctx.empty RCtx.empty stHandler .nat .copy [] := by
  refine .letE (split_left_all _) (splitR_empty_left _) (ε1 := []) (τ1 := .hdl) (m1 := .linear)
    (.mint discard_empty discardR_empty (Mode.le_refl _)) ?_ (frame_empty _) (rowSub_refl _) (rowSub_refl _)
  refine .handle (εb := [.user 0]) (εc := []) (τb := .nat) (mb := .copy) (τs := .hdl) (ms := .linear)
    (.var 0) (split_left_all _) (split_left_all _) allCopy_empty (splitR_empty_left _)
    (var_only0 (b := (.hdl, .linear)) (Mode.le_refl _)) (by decide) ?_ (rowSub_refl _) ?_ ?_ ?_ (rowSub_refl _)
  · -- the body performs both operations
    refine .letE (split_left_all _) (splitR_empty_left _) (τ1 := .nat) (m1 := .copy) (ε1 := [.user 0])
      (.perform (o := 0) (by decide) (.val (.natV 5)) (.natV 5 discard_empty discardR_empty) (by simp)
        (Mode.le_refl _)) ?_ (frame_empty _) (rowSub_refl _) (rowSub_refl _)
    exact .perform (o := 1) (by decide) (.val .unitV) (.unitV (discard_one_copy _) discardR_empty) (by simp)
      (Mode.le_refl _)
  · -- return(x, s) => consume s; x
    refine .letE (split_two (.nat, .copy) (.hdl, .linear)) (splitR_empty_left _) (τ1 := .unit) (m1 := .copy)
      (ε1 := []) (.consume (.var 0) (var_only0 (b := (.hdl, .linear)) (Mode.le_refl _))) ?_
      ⟨fun i t m h => (by
          simp only [Ctx.only1] at h; split at h
          · cases h; exact Mode.copy_le _
          · cases h),
        fun _ _ h => (by cases h)⟩ (rowSub_refl _) (rowSub_refl _)
    refine .var (mx := .copy) (by simp [Ctx.ext, Ctx.only1]) ?_ discardR_empty (Mode.le_refl _)
    intro j t m' hj h
    match j with
    | 0 => simp [Ctx.ext] at h; rw [← h.2]; decide
    | 1 => simp [Ctx.ext, Ctx.only1] at h
    | 2 => exact absurd rfl hj
    | j + 3 => simp [Ctx.ext, Ctx.only1] at h
  · intro o ho
    match o, ho with
    | 0, _ => rfl
    | 1, _ => rfl
  · intro o c hc
    match o with
    | 0 =>
      simp only [nthC, Option.some.injEq] at hc; subst hc
      refine .resume (.var 0) (.var 2) (.var 1) st_clause_split0 (st_rest_split .nat)
        (splitR_empty_left _) (splitR_empty_left _) (var_at (b := (stKont, .linear)) (Mode.le_refl _))
        (var_at (b := (.nat, .copy)) (Mode.copy_le _)) (var_at (b := (.hdl, .linear)) (Mode.le_refl _))
        (rowSub_refl _)
    | 1 =>
      simp only [nthC, Option.some.injEq] at hc; subst hc
      refine .resume (.var 0) (.val (.natV 7)) (.var 1) st_clause_split1 (st_rest_split .unit)
        (splitR_empty_left _) (splitR_empty_left _) (var_at (b := (stKont, .linear)) (Mode.le_refl _))
        (.natV 7 ?_ discardR_empty) (var_at (b := (.hdl, .linear)) (Mode.le_refl _)) (rowSub_refl _)
      intro i t m h
      simp only [Ctx.at] at h; split at h
      · cases h; decide
      · cases h
    | o + 2 => simp [nthC] at hc

/-- The run: mint, two captures and resumptions threading the state, the
return clause consuming the state. -/
def stActs : List Act :=
  [.mint 0, .silent, .capture 1 .linear, .resume 1, .silent, .capture 2 .linear, .resume 2, .silent,
    .consume 0, .silent]

theorem st_handler_runs : ∃ L n, Steps exSig ⟨stHandler, RCtx.empty, 0⟩ stActs ⟨.natV 7, L, n⟩ := by
  refine ⟨?L, ?n, ?main⟩
  case main =>
    apply Steps.cons (Step.ctx (.letC .hole _) Head.mint)
    apply Steps.cons (Step.ctx .hole (Head.letV (.hdlV 0)))
    refine Steps.cons (Step.ctx .hole (Head.capture (E := .letC .hole (.perform 0 1 .unitV)) (o := 0)
      trivial (.natV 5) (.hdlV 0) rfl)) ?_
    apply Steps.cons (Step.ctx .hole (Head.resume (q := .linear) (by simp [RCtx.set, exSig]) (.natV 5) (.hdlV 0)))
    apply Steps.cons (Step.ctx (.handleC 0 (.hdlV 0) .hole _ _) (Head.letV (.natV 5)))
    refine Steps.cons (Step.ctx .hole (Head.capture (E := .hole) (o := 1) trivial .unitV (.hdlV 0) rfl)) ?_
    apply Steps.cons (Step.ctx .hole (Head.resume (q := .linear) (by simp [RCtx.set, exSig]) (.natV 7) (.hdlV 0)))
    apply Steps.cons (Step.ctx .hole (Head.handleRet (.hdlV 0) (.natV 7)))
    apply Steps.cons (Step.ctx (.letC .hole _) (Head.consume (by simp [RCtx.set])))
    apply Steps.cons (Step.ctx .hole (Head.letV .unitV))
    exact .refl _

/-- Theorem 2 per kind on that run: the state handle is consumed once and
never resumed; each continuation is resumed once and never consumed. -/
theorem st_handler_exhausts : ∀ L n, Steps exSig ⟨stHandler, RCtx.empty, 0⟩ stActs ⟨.natV 7, L, n⟩ →
    (stActs.count (.consume 0) = 1 ∧ stActs.count (.resume 0) = 0) ∧
    (stActs.count (.resume 1) = 1 ∧ stActs.count (.consume 1) = 0) ∧
    (stActs.count (.resume 2) = 1 ∧ stActs.count (.consume 2) = 0) := by
  intro L n hs
  have h := total_linear_exhaustion_per_kind st_handler_typed (fun l h => by simp at h) hs (Or.inl (.natV 7))
  exact ⟨h.1 0 (by simp [stActs]), h.2 1 (by simp [stActs]), h.2 2 (by simp [stActs])⟩

/-- The state premise `m_s ⊑ res(ε)` is needed: a linear handle as the state
of a handler whose body may abort is rejected (the abort would drop it). -/
theorem linear_state_across_abort_rejected (R : RCtx) (ret cls : Expr) (τ : Ty) (m : Mode) (ε : Row) :
    ¬ HasType exSig Ctx.empty R (.handle 1 (.hdlV 0) .abort ret cls) τ m ε := by
  intro h
  cases h with
  | handle _ _ _ _ _ hst hms hb hsub _ _ _ _ =>
    cases hst with
    | hdlV _ _ _ _ hlin =>
      cases hb with
      | abort _ _ hab =>
        have hmem := hsub _ hab
        simp only [List.mem_cons, reduceCtorEq, false_or] at hmem
        have := res_le exSig hmem
        have h1 := Mode.le_trans hlin (Mode.le_trans hms this)
        exact absurd h1 (by decide)

/-! ### Effect-row variables -/

/-- `callIt = λg. g ()`, typed once for every row ρ:
`(unit -[ρ]@copy-> nat) -[ρ]@copy-> nat`. -/
def callIt : Expr := .clos .unitV (.app (.var 0) .unitV)

def callItTy : Ty := .arrow (.arrow .unit .nat .copy [.rvar 0] .copy) .nat .copy [.rvar 0] .copy

theorem call_it_typed : HasType exSig Ctx.empty RCtx.empty callIt callItTy .copy [] := by
  refine .clos (.val .unitV) (.unitV discard_empty discardR_empty) (Mode.le_refl _) ?_ (Mode.le_refl _)
  refine .app (σ := .unit) (mf := .copy) (.var 0) (.val .unitV) (split_left_all _) (splitR_empty_left _) ?_ ?_
    (rowSub_refl _)
  · refine .var (mx := .copy) rfl ?_ discardR_empty (Mode.le_refl _)
    intro j t m' hj h
    match j with
    | 0 => exact absurd rfl hj
    | 1 => simp [Ctx.two, Ctx.one, Ctx.ext] at h; rw [← h.2]; decide
    | j + 2 => simp [Ctx.two, Ctx.one, Ctx.ext, Ctx.empty] at h
  · exact .unitV discard_empty discardR_empty

/-- Instantiating ρ with the affine Fail effect. -/
theorem call_it_at_fail :
    HasType exSig Ctx.empty RCtx.empty callIt
      (.arrow (.arrow .unit .nat .copy [.user 1] .copy) .nat .copy [.user 1] .copy) .copy [] :=
  inst_ty (θ := fun _ => [.user 1]) (exSig_inv _) call_it_typed

/-- Instantiating ρ with the empty row. -/
theorem call_it_pure :
    HasType exSig Ctx.empty RCtx.empty callIt
      (.arrow (.arrow .unit .nat .copy [] .copy) .nat .copy [] .copy) .copy [] :=
  inst_ty (θ := fun _ => []) (exSig_inv _) call_it_typed

end Examples

-- Kernel axiom audit: every theorem of this file.
#print axioms Vesper.Metatheory.Closures.Mode.le_refl
#print axioms Vesper.Metatheory.Closures.Mode.le_trans
#print axioms Vesper.Metatheory.Closures.Mode.le_antisymm
#print axioms Vesper.Metatheory.Closures.Mode.copy_le
#print axioms Vesper.Metatheory.Closures.Mode.le_linear
#print axioms Vesper.Metatheory.Closures.Mode.le_join_left
#print axioms Vesper.Metatheory.Closures.Mode.le_join_right
#print axioms Vesper.Metatheory.Closures.Mode.join_le
#print axioms Vesper.Metatheory.Closures.Mode.meet_le_left
#print axioms Vesper.Metatheory.Closures.Mode.meet_of_le
#print axioms Vesper.Metatheory.Closures.Mode.le_copy
#print axioms Vesper.Metatheory.Closures.RMode.le_refl
#print axioms Vesper.Metatheory.Closures.RMode.le_trans
#print axioms Vesper.Metatheory.Closures.RMode.affine_le
#print axioms Vesper.Metatheory.Closures.RMode.not_le_copy
#print axioms Vesper.Metatheory.Closures.RMode.le_affine_iff
#print axioms Vesper.Metatheory.Closures.RMode.affine_le_mode
#print axioms Vesper.Metatheory.Closures.res_le
#print axioms Vesper.Metatheory.Closures.res_mono
#print axioms Vesper.Metatheory.Closures.nthC_val
#print axioms Vesper.Metatheory.Closures.nthC_subst
#print axioms Vesper.Metatheory.Closures.splitR_comm
#print axioms Vesper.Metatheory.Closures.splitR_empty_left
#print axioms Vesper.Metatheory.Closures.splitR_empty_right
#print axioms Vesper.Metatheory.Closures.splitR_of_empty_left
#print axioms Vesper.Metatheory.Closures.splitR_of_empty_right
#print axioms Vesper.Metatheory.Closures.splitR_congr
#print axioms Vesper.Metatheory.Closures.splitR_assoc
#print axioms Vesper.Metatheory.Closures.splitR_assoc'
#print axioms Vesper.Metatheory.Closures.splitR_swap
#print axioms Vesper.Metatheory.Closures.discardR_split
#print axioms Vesper.Metatheory.Closures.discardR_join
#print axioms Vesper.Metatheory.Closures.discardR_empty
#print axioms Vesper.Metatheory.Closures.rleM_split
#print axioms Vesper.Metatheory.Closures.rleM_part
#print axioms Vesper.Metatheory.Closures.rleM_mono
#print axioms Vesper.Metatheory.Closures.rleM_copy
#print axioms Vesper.Metatheory.Closures.rleM_affine_discard
#print axioms Vesper.Metatheory.Closures.discardR_rleM
#print axioms Vesper.Metatheory.Closures.empty_rleM
#print axioms Vesper.Metatheory.Closures.empty_discardR
#print axioms Vesper.Metatheory.Closures.splitE_left
#print axioms Vesper.Metatheory.Closures.splitE_right
#print axioms Vesper.Metatheory.Closures.split_empty
#print axioms Vesper.Metatheory.Closures.split_left_all
#print axioms Vesper.Metatheory.Closures.split_right_all
#print axioms Vesper.Metatheory.Closures.discard_empty
#print axioms Vesper.Metatheory.Closures.allCopy_empty
#print axioms Vesper.Metatheory.Closures.ext_del
#print axioms Vesper.Metatheory.Closures.split_del
#print axioms Vesper.Metatheory.Closures.discard_del
#print axioms Vesper.Metatheory.Closures.allCopy_del
#print axioms Vesper.Metatheory.Closures.discard_split
#print axioms Vesper.Metatheory.Closures.split4
#print axioms Vesper.Metatheory.Closures.splitR_congr_left
#print axioms Vesper.Metatheory.Closures.splitR_self_empty
#print axioms Vesper.Metatheory.Closures.rempty_eq
#print axioms Vesper.Metatheory.Closures.splitR_of_empty
#print axioms Vesper.Metatheory.Closures.del_eq
#print axioms Vesper.Metatheory.Closures.splitE_none
#print axioms Vesper.Metatheory.Closures.splitE_some
#print axioms Vesper.Metatheory.Closures.split3_empty
#print axioms Vesper.Metatheory.Closures.vt_isVal
#print axioms Vesper.Metatheory.Closures.vt_rleM
#print axioms Vesper.Metatheory.Closures.val_inv
#print axioms Vesper.Metatheory.Closures.val_ty
#print axioms Vesper.Metatheory.Closures.subst_val
#print axioms Vesper.Metatheory.Closures.substOK_ext
#print axioms Vesper.Metatheory.Closures.substOK_discard
#print axioms Vesper.Metatheory.Closures.substOK_split
#print axioms Vesper.Metatheory.Closures.substOK_handle
#print axioms Vesper.Metatheory.Closures.substOK_frame
#print axioms Vesper.Metatheory.Closures.isAtom_subst
#print axioms Vesper.Metatheory.Closures.discard_rest
#print axioms Vesper.Metatheory.Closures.clauseCtx_del
#print axioms Vesper.Metatheory.Closures.nthC_subst_some
#print axioms Vesper.Metatheory.Closures.nthC_subst_isSome
#print axioms Vesper.Metatheory.Closures.subst_ty
#print axioms Vesper.Metatheory.Closures.weakenR
#print axioms Vesper.Metatheory.Closures.rowSub_trans
#print axioms Vesper.Metatheory.Closures.rowSub_refl
#print axioms Vesper.Metatheory.Closures.weakenRow
#print axioms Vesper.Metatheory.Closures.splitR_rot
#print axioms Vesper.Metatheory.Closures.vt_at
#print axioms Vesper.Metatheory.Closures.vt_xchg
#print axioms Vesper.Metatheory.Closures.vt_swap2
#print axioms Vesper.Metatheory.Closures.bind_map_l
#print axioms Vesper.Metatheory.Closures.bind_map_r
#print axioms Vesper.Metatheory.Closures.vSet_comm_disjoint
#print axioms Vesper.Metatheory.Closures.vSet_overlap_not_comm
#print axioms Vesper.Metatheory.Closures.overlap_not_disj
#print axioms Vesper.Metatheory.Closures.frame_rleM
#print axioms Vesper.Metatheory.Closures.atom_closed
#print axioms Vesper.Metatheory.Closures.state_rleM
#print axioms Vesper.Metatheory.Closures.plug_inv
#print axioms Vesper.Metatheory.Closures.ext_del_zero
#print axioms Vesper.Metatheory.Closures.two_del_zero
#print axioms Vesper.Metatheory.Closures.one_del_zero
#print axioms Vesper.Metatheory.Closures.regroup
#print axioms Vesper.Metatheory.Closures.nat_discard
#print axioms Vesper.Metatheory.Closures.splitR_left_of_empty
#print axioms Vesper.Metatheory.Closures.substOK_val
#print axioms Vesper.Metatheory.Closures.split_two
#print axioms Vesper.Metatheory.Closures.var_only0
#print axioms Vesper.Metatheory.Closures.var_only1
#print axioms Vesper.Metatheory.Closures.capture_splits
#print axioms Vesper.Metatheory.Closures.split_remove
#print axioms Vesper.Metatheory.Closures.resume_splits
#print axioms Vesper.Metatheory.Closures.head_pres
#print axioms Vesper.Metatheory.Closures.preservation
#print axioms Vesper.Metatheory.Closures.sub_left
#print axioms Vesper.Metatheory.Closures.sub_right
#print axioms Vesper.Metatheory.Closures.canon_nat
#print axioms Vesper.Metatheory.Closures.canon_hdl
#print axioms Vesper.Metatheory.Closures.canon_kont
#print axioms Vesper.Metatheory.Closures.canon_arrow
#print axioms Vesper.Metatheory.Closures.canon_pair
#print axioms Vesper.Metatheory.Closures.vt_set_exists
#print axioms Vesper.Metatheory.Closures.progress_gen
#print axioms Vesper.Metatheory.Closures.progress
#print axioms Vesper.Metatheory.Closures.type_safety
#print axioms Vesper.Metatheory.Closures.head_next
#print axioms Vesper.Metatheory.Closures.head_fresh
#print axioms Vesper.Metatheory.Closures.head_dead
#print axioms Vesper.Metatheory.Closures.head_release
#print axioms Vesper.Metatheory.Closures.head_keep
#print axioms Vesper.Metatheory.Closures.head_alloc
#print axioms Vesper.Metatheory.Closures.step_parts
#print axioms Vesper.Metatheory.Closures.step_fresh
#print axioms Vesper.Metatheory.Closures.step_dead
#print axioms Vesper.Metatheory.Closures.steps_fresh
#print axioms Vesper.Metatheory.Closures.steps_dead
#print axioms Vesper.Metatheory.Closures.release_at_most_once
#print axioms Vesper.Metatheory.Closures.count_resume_le_rel
#print axioms Vesper.Metatheory.Closures.one_shot_resumption
#print axioms Vesper.Metatheory.Closures.steps_keep
#print axioms Vesper.Metatheory.Closures.alloc_live
#print axioms Vesper.Metatheory.Closures.terminal_no_linear
#print axioms Vesper.Metatheory.Closures.total_linear_exhaustion
#print axioms Vesper.Metatheory.Closures.head_consume_hdl
#print axioms Vesper.Metatheory.Closures.head_resume_kont
#print axioms Vesper.Metatheory.Closures.steps_alloc_ge
#print axioms Vesper.Metatheory.Closures.count_consume_le_rel
#print axioms Vesper.Metatheory.Closures.relCount_eq
#print axioms Vesper.Metatheory.Closures.dead_no_release
#print axioms Vesper.Metatheory.Closures.live_hdl_never_resumed
#print axioms Vesper.Metatheory.Closures.live_kont_never_consumed
#print axioms Vesper.Metatheory.Closures.minted_never_resumed
#print axioms Vesper.Metatheory.Closures.captured_never_consumed
#print axioms Vesper.Metatheory.Closures.total_linear_exhaustion_per_kind
#print axioms Vesper.Metatheory.Closures.kind_inst
#print axioms Vesper.Metatheory.Closures.at_inst
#print axioms Vesper.Metatheory.Closures.mem_inst_of
#print axioms Vesper.Metatheory.Closures.rowSub_inst
#print axioms Vesper.Metatheory.Closures.inst_cons_user
#print axioms Vesper.Metatheory.Closures.res_inst
#print axioms Vesper.Metatheory.Closures.inst_ext
#print axioms Vesper.Metatheory.Closures.inst_empty
#print axioms Vesper.Metatheory.Closures.inst_two
#print axioms Vesper.Metatheory.Closures.splitE_inst
#print axioms Vesper.Metatheory.Closures.split_inst
#print axioms Vesper.Metatheory.Closures.discard_inst
#print axioms Vesper.Metatheory.Closures.allCopy_inst
#print axioms Vesper.Metatheory.Closures.frameOK_inst
#print axioms Vesper.Metatheory.Closures.clauseCtx_inst
#print axioms Vesper.Metatheory.Closures.inst_ty
#print axioms Vesper.Metatheory.Closures.row_poly_safety
#print axioms Vesper.Metatheory.Closures.ctxBelow_split_left
#print axioms Vesper.Metatheory.Closures.ctxBelow_split_right
#print axioms Vesper.Metatheory.Closures.ctxBelow_ext
#print axioms Vesper.Metatheory.Closures.ctxBelow_two
#print axioms Vesper.Metatheory.Closures.typed_below
#print axioms Vesper.Metatheory.Closures.captured_kont_typed
#print axioms Vesper.Metatheory.Closures.captured_kont_self_contained
#print axioms Vesper.Metatheory.Closures.exSig_inv
#print axioms Vesper.Metatheory.Closures.var_at
#print axioms Vesper.Metatheory.Closures.frame_empty
#print axioms Vesper.Metatheory.Closures.discard_two
#print axioms Vesper.Metatheory.Closures.discard_one_copy
#print axioms Vesper.Metatheory.Closures.linear_capture_copy_closure_rejected
#print axioms Vesper.Metatheory.Closures.lin_closure_typed
#print axioms Vesper.Metatheory.Closures.lin_closure_runs
#print axioms Vesper.Metatheory.Closures.lin_closure_exhausts
#print axioms Vesper.Metatheory.Closures.lin_closure_twice_rejected
#print axioms Vesper.Metatheory.Closures.st_clause_split0
#print axioms Vesper.Metatheory.Closures.st_clause_split1
#print axioms Vesper.Metatheory.Closures.st_rest_split
#print axioms Vesper.Metatheory.Closures.st_handler_typed
#print axioms Vesper.Metatheory.Closures.st_handler_runs
#print axioms Vesper.Metatheory.Closures.st_handler_exhausts
#print axioms Vesper.Metatheory.Closures.linear_state_across_abort_rejected
#print axioms Vesper.Metatheory.Closures.call_it_typed
#print axioms Vesper.Metatheory.Closures.call_it_at_fail
#print axioms Vesper.Metatheory.Closures.call_it_pure

end Vesper.Metatheory.Closures
