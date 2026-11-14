/-
Vesper mechanized metatheory, wave six (issue #258).

A larger fragment of λ_Vesper than `lean/MetatheoryAppendix.lean` (which this
file does not import or change), with a small-step semantics and syntactic
progress and preservation proofs.

The fragment, in A-normal form:
* modes copy ⊑ affine ⊑ linear, modal kinds with [K-Adt] field overrides and
  the field-modality premise `k_i ⊑ m_i`;
* de Bruijn bindings at copy/affine/linear mode, context splitting ⊛ with copy
  contraction only, discard at leaves for at most affine bindings;
* [T-Let] with the Resumption Frame Condition, a uniform-consumption branch
  (`ifz`), and [T-Match]-style unpacking of a two-field struct;
* field-tree places: `get` (an `in` read of a copy leaf), `swap` (an `inout`
  replacement) and `swap2` (two `inout` projections of one root, required to
  be disjoint as in [T-CallProj]);
* deep, lexically scoped, one-shot effect handlers ([T-Perform], [T-Handle])
  whose continuations are first-class values of mode q, with copy-only ambient
  handler contexts;
* checked u64 arithmetic whose overflow/underflow aborts (an affine effect);
* linear resources (`mint`/`consume`, standing for e.g. `WriteTxn`).

Runtime configurations carry a resource store `live : Nat → Option RMode`
recording every live linear handle and every live continuation. Consuming a
handle or resuming a continuation requires it to be live and removes it, so
the store is the dynamic one-shot/exact-once check; the theorems show that a
well-typed program never fails that check (progress), that typing is
preserved, that each continuation is resumed at most once, and that a run that
ends (in a value at mode copy, or in an abort) leaves no linear resource or
linear continuation live (Theorem 2, total linear exhaustion).

Everything is a theorem about this Lean model, not about the Rust compiler.
-/
namespace Vesper.MetatheoryFull

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

/-- Resumption guarantees and resource modes: never copy. -/
inductive RMode : Type where
  | affine | linear
  deriving DecidableEq, Repr

namespace RMode
@[simp] def toMode : RMode → Mode
  | affine => .affine | linear => .linear
def Le (a b : RMode) : Prop := Mode.Le a.toMode b.toMode
theorem le_refl (a : RMode) : Le a a := Mode.le_refl _
theorem not_le_copy (q : RMode) : ¬ Mode.Le q.toMode .copy := by
  cases q <;> decide
theorem le_affine_iff (q : RMode) : Mode.Le q.toMode .affine ↔ q = .affine := by
  cases q <;> decide
end RMode

/-! ## Types, modal kinds and effect rows -/

inductive Label : Type where
  | abort
  | user (n : Nat)
  deriving DecidableEq, Repr

abbrev Row := List Label

inductive Ty : Type where
  | unit
  | nat
  /-- a linear resource handle, standing for `WriteTxn`, `NonceTicket`, ... -/
  | hdl
  /-- a two-field struct `T { f1 : t1 @@ m1, f2 : t2 @@ m2 }` with nominal floor `md` -/
  | pair (md : Mode) (t1 : Ty) (m1 : Mode) (t2 : Ty) (m2 : Mode)
  /-- a one-shot continuation `(arg) -[row]@q-> ans @ am` -/
  | kont (arg : Ty) (ans : Ty) (am : Mode) (row : Row) (q : RMode)

/-- [K-Prim], [K-Adt] (`m_decl ⊔ ⊔_i (k_i ⊓ m_i)`) and [K-Fn]. -/
def Ty.kind : Ty → Mode
  | .unit => .copy
  | .nat => .copy
  | .hdl => .linear
  | .pair md t1 m1 t2 m2 => Mode.join md (Mode.join (Mode.meet t1.kind m1) (Mode.meet t2.kind m2))
  | .kont _ _ _ _ q => q.toMode

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

structure EffSig where
  q : RMode
  arg : Ty
  res : Ty

section Sig
variable (Sig : Nat → EffSig)

def qOf : Label → RMode
  | .abort => .affine
  | .user l => (Sig l).q

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
  /-- a captured continuation with store id `r`; `body` binds the hole as `var 0` -/
  | kontV (r : Nat) (body : Expr)
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
  | perform (l : Nat) (a : Expr)
  /-- `handle l in body with { return x => ret, op(y, k) => opc }`;
  `ret` binds `x` as 0; `opc` binds `y` as 1 and `k` as 0 -/
  | handle (l : Nat) (body ret opc : Expr)
  | resume (k a : Expr)
  | get (a : Expr) (p : Path)
  | swap (a : Expr) (p : Path) (b : Expr)
  | swap2 (a : Expr) (p q : Path)

inductive IsVal : Expr → Prop where
  | unitV : IsVal .unitV
  | natV (n : Nat) : IsVal (.natV n)
  | hdlV (r : Nat) : IsVal (.hdlV r)
  | kontV (r : Nat) (b : Expr) : IsVal (.kontV r b)
  | pair {a b : Expr} : IsVal a → IsVal b → IsVal (.pair a b)

inductive IsAtom : Expr → Prop where
  | var (i : Nat) : IsAtom (.var i)
  | val {v : Expr} : IsVal v → IsAtom v

/-- Substitution of a closed value for `var k`, lowering the variables above
`k`. Values are closed (continuation bodies are typed in their own one-hole
context), so substitution does not descend into `kontV`. -/
def subst (k : Nat) (v : Expr) : Expr → Expr
  | .var i => if i = k then v else if k < i then .var (i - 1) else .var i
  | .unitV => .unitV
  | .natV n => .natV n
  | .hdlV r => .hdlV r
  | .kontV r b => .kontV r b
  | .pair a b => .pair (subst k v a) (subst k v b)
  | .letE e1 e2 => .letE (subst k v e1) (subst (k + 1) v e2)
  | .letPair a e => .letPair (subst k v a) (subst (k + 2) v e)
  | .ifz a e1 e2 => .ifz (subst k v a) (subst k v e1) (subst k v e2)
  | .mint => .mint
  | .consume a => .consume (subst k v a)
  | .arith op a b => .arith op (subst k v a) (subst k v b)
  | .abort => .abort
  | .perform l a => .perform l (subst k v a)
  | .handle l b r o => .handle l (subst k v b) (subst (k + 1) v r) (subst (k + 2) v o)
  | .resume a b => .resume (subst k v a) (subst k v b)
  | .get a p => .get (subst k v a) p
  | .swap a p b => .swap (subst k v a) p (subst k v b)
  | .swap2 a p q => .swap2 (subst k v a) p q

/-! ## Contexts -/

/-- Γ: de Bruijn index ↦ (type, binding mode). -/
abbrev Ctx := Nat → Option (Ty × Mode)
/-- Resource contexts / stores: id ↦ mode of a live handle or continuation. -/
abbrev RCtx := Nat → Option RMode

def Ctx.empty : Ctx := fun _ => none
def RCtx.empty : RCtx := fun _ => none
def RCtx.single (r : Nat) (q : RMode) : RCtx := fun x => if x = r then some q else none

def Ctx.ext (Γ : Ctx) (b : Ty × Mode) : Ctx
  | 0 => some b
  | i + 1 => Γ i

def Ctx.del (Γ : Ctx) (k : Nat) : Ctx := fun i => if i < k then Γ i else Γ (i + 1)

/-- The one-hole context of a continuation body. -/
def Ctx.one (b : Ty × Mode) : Ctx := Ctx.empty.ext b

/-- Γ = Γ1 ⊛ Γ2: copy bindings contract into both sides, others go to one. -/
inductive SplitE : Option (Ty × Mode) → Option (Ty × Mode) → Option (Ty × Mode) → Prop where
  | none : SplitE none none none
  | left (b) : SplitE (some b) (some b) none
  | right (b) : SplitE (some b) none (some b)
  | copy (t : Ty) : SplitE (some (t, .copy)) (some (t, .copy)) (some (t, .copy))

def Split (Γ Γ1 Γ2 : Ctx) : Prop := ∀ i, SplitE (Γ i) (Γ1 i) (Γ2 i)

/-- Resources never contract. -/
def SRE (x y z : Option RMode) : Prop := (y = none ∧ x = z) ∨ (z = none ∧ x = y)

def SplitR (R R1 R2 : RCtx) : Prop := ∀ r, SRE (R r) (R1 r) (R2 r)

/-- discard(Γ): every binding is at most affine. -/
def Discard (Γ : Ctx) : Prop := ∀ i t m, Γ i = some (t, m) → Mode.Le m .affine
def DiscardR (R : RCtx) : Prop := ∀ r, R r ≠ some .linear
def AllCopy (Γ : Ctx) : Prop := ∀ i t m, Γ i = some (t, m) → m = .copy

/-- The Resumption Frame Condition: every binding/resource of the residual
frame is at most `res(ε1)`. -/
def FrameOK (Γ : Ctx) (R : RCtx) (q : RMode) : Prop :=
  (∀ i t m, Γ i = some (t, m) → Mode.Le m q.toMode) ∧ (∀ r q', R r = some q' → RMode.Le q' q)

def RowSub (ε1 ε : Row) : Prop := ∀ l, l ∈ ε1 → l ∈ ε

/-! ## Typing: Δ; Σ; Γ ⊢ e : τ @ m | ε (with the resource context R) -/

section Typing
variable (Sig : Nat → EffSig)

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
      Discard Γ → SplitR R (RCtx.single r .linear) Rd → DiscardR Rd →
      Mode.Le .linear m → HasType Γ R (.hdlV r) .hdl m ε
  | kontV {Γ R R0 Rd Rb m ε σ τ am ε' q} (r : Nat) (body : Expr) :
      Discard Γ → SplitR R R0 Rd → DiscardR Rd →
      SplitR R0 (RCtx.single r q) Rb →
      HasType (Ctx.one (σ, .linear)) Rb body τ am ε' →
      (∀ r' q', Rb r' = some q' → RMode.Le q' q) →
      Mode.Le q.toMode m →
      HasType Γ R (.kontV r body) (.kont σ τ am ε' q) m ε
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
  /-- Closed literal arithmetic has a pure row when its actual evaluation
  succeeds and both operands are in u64 range. No symbolic operand or unchecked
  range assertion is admitted. -/
  | arithLiteral {Γ R op a b c m ε} :
      Discard Γ → DiscardR R → a < u64Bound → b < u64Bound → op.eval a b = some c →
      HasType Γ R (.arith op (.natV a) (.natV b)) .nat m ε
  | abort {Γ R τ m ε} : Discard Γ → DiscardR R → Label.abort ∈ ε → HasType Γ R .abort τ m ε
  /-- [T-Perform] -/
  | perform {Γ R l a m ε} : IsAtom a → HasType Γ R a (Sig l).arg .linear ε →
      Label.user l ∈ ε → Mode.Le (Sig l).res.kind m →
      HasType Γ R (.perform l a) (Sig l).res m ε
  /-- [T-Handle] (without state threading): copy-only ambient context Γh, no
  resources in the clauses, continuation `k` of mode q -/
  | handle {Γ Γb Γh R l body ret opc τb mb εb τ m εc ε} :
      Split Γ Γb Γh → AllCopy Γh →
      HasType Γb R body τb mb εb → RowSub εb (.user l :: εc) →
      HasType (Γh.ext (τb, mb)) RCtx.empty ret τ m εc →
      HasType ((Γh.ext ((Sig l).arg, .linear)).ext
        (.kont (Sig l).res τ m εc (Sig l).q, (Sig l).q.toMode)) RCtx.empty opc τ m εc →
      RowSub εc ε →
      HasType Γ R (.handle l body ret opc) τ m ε
  | resume {Γ Γ1 Γ2 R R1 R2 k a σ τ m ε' q ε} :
      IsAtom k → IsAtom a → Split Γ Γ1 Γ2 → SplitR R R1 R2 →
      HasType Γ1 R1 k (.kont σ τ m ε' q) q.toMode ε →
      HasType Γ2 R2 a σ .linear ε → RowSub ε' ε →
      HasType Γ R (.resume k a) τ m ε
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

def RCtx.set (R : RCtx) (r : Nat) (o : Option RMode) : RCtx := fun x => if x = r then o else R x

def REmpty (R : RCtx) : Prop := ∀ r, R r = none

/-- Resources of a context are at most mode `m`. -/
def RLeM (R : RCtx) (m : Mode) : Prop := ∀ r q, R r = some q → Mode.Le q.toMode m

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
  constructor <;> intro r hr <;> have := h r <;> have := hd r <;> unfold SRE at * <;> grind

theorem discardR_join {R R1 R2 : RCtx} (h : SplitR R R1 R2) (h1 : DiscardR R1) (h2 : DiscardR R2) :
    DiscardR R := by
  intro r hr; have := h r; have := h1 r; have := h2 r; unfold SRE at *; grind

theorem discardR_empty : DiscardR RCtx.empty := fun _ h => by cases h

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
  | some q => exact absurd (h r q hr) (RMode.not_le_copy q)

theorem rleM_affine_discard {R : RCtx} (h : RLeM R .affine) : DiscardR R := by
  intro r hr; have := h r _ hr; simp [Mode.Le] at this

theorem empty_rleM {R : RCtx} (h : REmpty R) (m : Mode) : RLeM R m := by
  intro r q hr; rw [h r] at hr; cases hr

theorem empty_discardR {R : RCtx} (h : REmpty R) : DiscardR R := by
  intro r hr; rw [h r] at hr; cases hr

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

/-! ## Closed value typing -/

section Values
variable {Sig : Nat → EffSig}

/-- Exact typing of closed values: `R` is precisely the resources inside. -/
inductive VT (Sig : Nat → EffSig) : RCtx → Expr → Ty → Prop where
  | unitV {R} : REmpty R → VT Sig R .unitV .unit
  | natV {R} (n : Nat) : REmpty R → VT Sig R (.natV n) .nat
  | hdlV {R} (r : Nat) : (∀ x, R x = RCtx.single r .linear x) → VT Sig R (.hdlV r) .hdl
  | kontV {R Rb σ τ am ε' q} (r : Nat) (body : Expr) :
      SplitR R (RCtx.single r q) Rb →
      HasType Sig (Ctx.one (σ, .linear)) Rb body τ am ε' →
      RLeM Rb q.toMode →
      VT Sig R (.kontV r body) (.kont σ τ am ε' q)
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
  | pair _ _ _ _ _ ih1 ih2 => exact .pair ih1 ih2

/-- Resources of a value are bounded by its type's kind. This is where the
field-modality premise `k_i ⊑ m_i` of [K-Adt] is used. -/
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
  | perform ha _ hl hm ih =>
    intro k v Rv R' hok hs
    exact .perform (isAtom_subst ha hok.1) (ih hok hs) hl hm
  | handle hsΓ hc _ hsub _ _ hc' ihb ihr iho =>
    intro k v Rv R' hok hs
    obtain ⟨hokb, hokh⟩ := substOK_handle hsΓ hc hok
    have ihr' := ihr (substOK_ext hokh _) (splitR_empty_left _)
    have iho' := iho (substOK_ext (substOK_ext hokh _) _) (splitR_empty_left _)
    rw [ext_del] at ihr'
    rw [ext_del, ext_del] at iho'
    exact .handle (split_del hsΓ k) (allCopy_del hc k) (ihb hokb hs) hsub ihr' iho' hc'
  | resume hk ha hsΓ hsR _ _ hsub ihk iha =>
    intro k v Rv R' hok hs
    obtain ⟨Ra, Rb, R1', R2', hoka, hokb, h1, h2, h12⟩ := substOK_split hsΓ hok hs hsR
    exact .resume (isAtom_subst hk hok.1) (isAtom_subst ha hok.1) (split_del hsΓ k) h12
      (ihk hoka h1) (iha hokb h2) hsub
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
  | perform ha _ hl hm ih => intro Rd R' hd hs; exact .perform ha (ih hd hs) hl hm
  | handle hsΓ hc _ hsub hr ho hc' ihb _ _ =>
    intro Rd R' hd hs; exact .handle hsΓ hc (ihb hd hs) hsub hr ho hc'
  | resume hk ha hsΓ hsR _ h2 hsub ihk _ =>
    intro Rd R' hd hs
    obtain ⟨hA, hB⟩ := splitR_swap hs hsR
    exact .resume hk ha hsΓ hA (ihk hd hB) h2 hsub
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
  | pair ha hb hsΓ hsR _ _ hk1 hk2 hk ih1 ih2 =>
    intro ε' hε; exact .pair ha hb hsΓ hsR (ih1 hε) (ih2 hε) hk1 hk2 hk
  | letE hsΓ hsR h1 h2 hf h1s h2s _ _ =>
    intro ε' hε; exact .letE hsΓ hsR h1 h2 hf (rowSub_trans h1s hε) (rowSub_trans h2s hε)
  | letPair ha hsΓ hsR _ _ iha ihe => intro ε' hε; exact .letPair ha hsΓ hsR (iha hε) (ihe hε)
  | ifz ha hsΓ hsR _ _ _ iha ih1 ih2 => intro ε' hε; exact .ifz ha hsΓ hsR (iha hε) (ih1 hε) (ih2 hε)
  | consume ha _ ih => intro ε' hε; exact .consume ha (ih hε)
  | arith ha hb hsΓ hsR _ _ hab iha ihb =>
    intro ε' hε; exact .arith ha hb hsΓ hsR (iha hε) (ihb hε) (hε _ hab)
  | perform ha _ hl hm ih => intro ε' hε; exact .perform ha (ih hε) (hε _ hl) hm
  | handle hsΓ hc hb hsub hr ho hc' _ _ _ =>
    intro ε' hε; exact .handle hsΓ hc hb hsub hr ho (rowSub_trans hc' hε)
  | resume hk ha hsΓ hsR _ _ hsub ihk iha =>
    intro ε' hε; exact .resume hk ha hsΓ hsR (ihk hε) (iha hε) (rowSub_trans hsub hε)
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
  | handleC (l : Nat) (E : EC) (ret opc : Expr)

def plug : EC → Expr → Expr
  | .hole, e => e
  | .letC E e2, e => .letE (plug E e) e2
  | .handleC l E r o, e => .handle l (plug E e) r o

/-- `E` has no handler for `lab` between its hole and its root. -/
def EC.Free (lab : Label) : EC → Prop
  | .hole => True
  | .letC E _ => E.Free lab
  | .handleC l E _ _ => lab ≠ .user l ∧ E.Free lab

section Decompose
variable {Sig : Nat → EffSig}

theorem frame_rleM {Γ R q} (h : FrameOK Γ R q) : RLeM R q.toMode := h.2

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
  | handleC l E r o ih =>
    intro R e τ m ε h
    cases h with
    | handle hsΓ hc hb hsub hr ho hc' =>
      obtain ⟨hΓ1, hΓ2⟩ := split_empty hsΓ
      subst hΓ1; subst hΓ2
      obtain ⟨R1, R2, τh, mh, εh, hs, hh, hrepl, hfr⟩ := ih hb
      refine ⟨R1, R2, τh, mh, εh, hs, hh, ?_, ?_⟩
      · intro Γ' R1' R' e' h' hs'
        exact .handle (split_left_all Γ') allCopy_empty (hrepl Γ' R1' R' e' h' hs') hsub hr ho hc'
      · intro lab hfree hl
        obtain ⟨hne, hfree'⟩ := hfree
        obtain ⟨hl1, hle⟩ := hfr lab hfree' hl
        have := hsub lab hl1
        simp only [List.mem_cons] at this
        rcases this with h | h
        · exact absurd h hne
        · exact ⟨hc' lab h, hle⟩

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

section Semantics
variable (Sig : Nat → EffSig)

inductive Head : Expr → RCtx → Nat → Act → Expr → RCtx → Nat → Prop where
  | letV {v e2 L n} : IsVal v → Head (.letE v e2) L n .silent (subst 0 v e2) L n
  | letAbort {e2 L n} : Head (.letE .abort e2) L n .silent .abort L n
  | letPairV {a b e L n} : IsVal a → IsVal b →
      Head (.letPair (.pair a b) e) L n .silent (subst 0 a (subst 0 b e)) L n
  | ifzZ {e1 e2 L n} : Head (.ifz (.natV 0) e1 e2) L n .silent e1 L n
  | ifzS {k e1 e2 L n} : Head (.ifz (.natV (k + 1)) e1 e2) L n .silent e2 L n
  | mint {L n} : Head .mint L n (.mint n) (.hdlV n) (L.set n (some .linear)) (n + 1)
  | consume {r L n} : L r = some .linear →
      Head (.consume (.hdlV r)) L n (.consume r) .unitV (L.set r none) n
  | arithOk {op a b c L n} : op.eval a b = some c →
      Head (.arith op (.natV a) (.natV b)) L n .silent (.natV c) L n
  | arithAbort {op a b L n} : op.eval a b = none →
      Head (.arith op (.natV a) (.natV b)) L n .silent .abort L n
  | handleRet {l v ret opc L n} : IsVal v →
      Head (.handle l v ret opc) L n .silent (subst 0 v ret) L n
  | handleAbort {l ret opc L n} : Head (.handle l .abort ret opc) L n .silent .abort L n
  /-- capture the delimited continuation up to the innermost `l` handler,
  reinstalling the (deep) handler inside it -/
  | capture {l E v ret opc L n} : E.Free (.user l) → IsVal v →
      Head (.handle l (plug E (.perform l v)) ret opc) L n (.capture n (Sig l).q)
        (subst 0 v (subst 0 (.kontV n (.handle l (plug E (.var 0)) ret opc)) opc))
        (L.set n (some (Sig l).q)) (n + 1)
  /-- one-shot resumption: the continuation must be live, and dies -/
  | resume {r body w q L n} : L r = some q → IsVal w →
      Head (.resume (.kontV r body) w) L n (.resume r) (subst 0 w body) (L.set r none) n
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

theorem regroup {R A B X Y : RCtx} (h : SplitR R A B) (h1 : SplitR A X Y) :
    SplitR (RCtx.union B X) B X ∧ SplitR R (RCtx.union B X) Y := by
  constructor <;> intro r <;> have := h r <;> have := h1 r <;> unfold SRE RCtx.union at * <;> grind

theorem nat_discard {R n τ m ε} (h : HasType Sig Ctx.empty R (.natV n) τ m ε) : DiscardR R := by
  obtain ⟨Rv, Rd, hs, hd, hvt, _⟩ := val_inv h (.natV n)
  cases hvt with
  | natV _ he => exact discardR_join hs (empty_discardR he) hd

theorem splitR_left_of_empty {R E : RCtx} (he : REmpty E) : SplitR R E R :=
  fun r => Or.inl ⟨he r, rfl⟩

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
      have h2' := subst_ty h2 (k := 0) ⟨hv, Or.inr ⟨τ1, m1, rfl, hvt, rleM_mono (vt_rleM hvt) hk⟩⟩ hA
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
      refine ⟨R1.set n (some .linear), .hdlV n discard_empty ?_ hd hm, ?_, ?_⟩
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
        · intro x hx; have := hsv x; have := he x; have := hd x
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
  | @handleRet l v ret opc _ _ hv =>
    intro R1 R2 τ m ε h hs hf
    cases h with
    | @handle _ Γb Γh _ _ _ _ _ τb mb εb _ _ εc _ hsΓ _ hb _ hr _ hc' =>
      obtain ⟨hΓ1, hΓ2⟩ := split_empty hsΓ
      subst hΓ1; subst hΓ2
      obtain ⟨Rv, Rd, hsv, hd, hvt, hk⟩ := val_inv hb hv
      have h' := subst_ty hr (k := 0) ⟨hv, Or.inr ⟨τb, mb, rfl, hvt, rleM_mono (vt_rleM hvt) hk⟩⟩
        (splitR_empty_left Rv)
      rw [ext_del_zero] at h'
      exact ⟨R1, weakenRow (weakenR h' hd hsv) hc', hs, hf⟩
  | handleAbort =>
    intro R1 R2 τ m ε h hs hf
    cases h with
    | handle hsΓ _ hb hsub _ _ hc' =>
      cases hb with
      | abort _ hd hab =>
        have := hsub _ hab
        simp only [List.mem_cons, reduceCtorEq, false_or] at this
        exact ⟨R1, .abort discard_empty hd (hc' _ this), hs, hf⟩
  | @capture l E v ret opc _ _ hfree hv =>
    intro R1 R2 τ m ε h hs hf
    cases h with
    | @handle _ Γb Γh _ _ _ _ _ τb mb εb _ _ εc _ hsΓ _ hb hsub hr ho hc' =>
      obtain ⟨hΓ1, hΓ2⟩ := split_empty hsΓ
      subst hΓ1; subst hΓ2
      obtain ⟨Rp, RE, τh, mh, εh, hsp, hperf, hrepl, hfr⟩ := plug_inv E hb
      cases hperf with
      | perform _ hva hl hm =>
        obtain ⟨_, hle⟩ := hfr _ hfree hl
        have hLn := hf n (Nat.le_refl _)
        -- the continuation's body: the deep handler around `E[var 0]`
        have hvar : HasType Sig (Ctx.one ((Sig l).res, .linear)) RCtx.empty (.var 0) (Sig l).res mh εh :=
          .var (mx := .linear) rfl
            (fun j t m' hj hj' => by
              cases j with
              | zero => exact absurd rfl hj
              | succ j => simp [Ctx.one, Ctx.ext, Ctx.empty] at hj')
            discardR_empty hm
        have hbody : HasType Sig (Ctx.one ((Sig l).res, .linear)) RE
            (.handle l (plug E (.var 0)) ret opc) τ m εc :=
          .handle (split_left_all _) allCopy_empty
            (hrepl _ RCtx.empty RE _ hvar (splitR_empty_left _)) hsub hr ho (rowSub_refl _)
        have hRE : SplitR (RCtx.union (RCtx.single n (Sig l).q) RE) (RCtx.single n (Sig l).q) RE := by
          intro r; have := hs r; have := hsp r; unfold SRE RCtx.union RCtx.single at *
          have := hf r; grind
        have hK : VT Sig (RCtx.union (RCtx.single n (Sig l).q) RE)
            (.kontV n (.handle l (plug E (.var 0)) ret opc))
            (.kont (Sig l).res τ m εc (Sig l).q) := .kontV n _ hRE hbody hle
        obtain ⟨Rv, Rd, hsv, hd, hvt, _⟩ := val_inv hva hv
        have e1 := subst_ty ho (k := 0) ⟨.kontV _ _, Or.inr ⟨_, _, rfl, hK, vt_rleM hK⟩⟩
          (splitR_empty_left _)
        rw [ext_del_zero] at e1
        have hsplit1 : SplitR (RCtx.union (RCtx.union (RCtx.single n (Sig l).q) RE) Rv)
            (RCtx.union (RCtx.single n (Sig l).q) RE) Rv := by
          intro r; have := hs r; have := hsp r; have := hsv r
          unfold SRE RCtx.union RCtx.single at *; have := hf r; grind
        have e2 := subst_ty e1 (k := 0) ⟨hv, Or.inr ⟨_, _, rfl, hvt, rleM_mono (vt_rleM hvt) (Mode.le_linear _)⟩⟩
          hsplit1
        rw [ext_del_zero] at e2
        have hsplit2 : SplitR (R1.set n (some (Sig l).q))
            (RCtx.union (RCtx.union (RCtx.single n (Sig l).q) RE) Rv) Rd := by
          intro r; have := hs r; have := hsp r; have := hsv r
          unfold SRE RCtx.union RCtx.single RCtx.set at *; have := hf r; grind
        refine ⟨R1.set n (some (Sig l).q), weakenRow (weakenR e2 hd hsplit2) hc', ?_, ?_⟩
        · intro r; have := hs r; unfold SRE RCtx.set at *; have := hf r; grind
        · intro r hr; unfold RCtx.set; rw [if_neg (by omega)]; exact hf r (by omega)
  | @resume r body w q _ _ hL hw =>
    intro R1 R2 τ m ε h hs hf
    cases h with
    | resume _ _ hsΓ hsR hk ha hsub =>
      obtain ⟨hΓ1, hΓ2⟩ := split_empty hsΓ
      subst hΓ1; subst hΓ2
      obtain ⟨Rk, Rdk, hsk, hdk, hvk, _⟩ := val_inv hk (.kontV r body)
      cases hvk with
      | @kontV _ Rbody σ _ _ _ _ _ _ hs0 hbody _ =>
        obtain ⟨Rw, Rdw, hsw, hdw, hvw, _⟩ := val_inv ha hw
        have hsplit1 : SplitR (RCtx.union Rbody Rw) Rbody Rw := by
          intro x; have := hsR x; have := hsk x; have := hs0 x; have := hsw x
          unfold SRE RCtx.union RCtx.single at *; grind
        have e1 := subst_ty hbody (k := 0) ⟨hw, Or.inr ⟨_, _, rfl, hvw, rleM_mono (vt_rleM hvw) (Mode.le_linear _)⟩⟩
          hsplit1
        simp only [Ctx.one] at e1
        rw [ext_del_zero] at e1
        have hsplit2 : SplitR (R1.set r none) (RCtx.union Rbody Rw) (RCtx.union Rdk Rdw) := by
          intro x; have := hsR x; have := hsk x; have := hs0 x; have := hsw x
          unfold SRE RCtx.union RCtx.single RCtx.set at *; grind
        have hsplit3 : SplitR (RCtx.union Rdk Rdw) Rdk Rdw := by
          intro x; have := hsR x; have := hsk x; have := hs0 x; have := hsw x
          unfold SRE RCtx.union RCtx.single at *; grind
        refine ⟨R1.set r none, weakenRow (weakenR e1 (discardR_join hsplit3 hdk hdw) hsplit2) hsub, ?_, ?_⟩
        · intro x; have := hs x; have := hsR x; have := hsk x; have := hs0 x
          unfold SRE RCtx.set RCtx.single at *; grind
        · intro x hx; unfold RCtx.set; split
          · rfl
          · exact hf x hx
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

theorem atom_closed {R a τ m ε} (ha : IsAtom a) (h : HasType Sig Ctx.empty R a τ m ε) : IsVal a := by
  cases ha with
  | var i => cases h with | var hi => simp [Ctx.empty] at hi
  | val hv => exact hv

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
    ∃ r, v = .hdlV r ∧ R r = some .linear := by
  obtain ⟨_, _, hs, _, hvt, _⟩ := val_inv h hv
  cases hvt with
  | hdlV r he =>
    refine ⟨r, rfl, ?_⟩
    have := hs r; have := he r; unfold SRE RCtx.single at *; grind

theorem canon_kont {R v σ τ am ε' q m ε} (h : HasType Sig Ctx.empty R v (.kont σ τ am ε' q) m ε)
    (hv : IsVal v) : ∃ r body, v = .kontV r body ∧ R r = some q := by
  obtain ⟨_, _, hs, _, hvt, _⟩ := val_inv h hv
  cases hvt with
  | kontV r body hs0 _ _ =>
    refine ⟨r, body, rfl, ?_⟩
    have := hs r; have := hs0 r; unfold SRE RCtx.single at *; grind

theorem canon_pair {R v md t1 m1 t2 m2 m ε} (h : HasType Sig Ctx.empty R v (.pair md t1 m1 t2 m2) m ε)
    (hv : IsVal v) : ∃ a b, v = .pair a b ∧ IsVal a ∧ IsVal b := by
  cases hv with
  | pair ha hb => exact ⟨_, _, rfl, ha, hb⟩
  | _ =>
    obtain ⟨_, _, _, _, hvt, _⟩ := val_inv h (by constructor)
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
effect in its row, or it takes a head step in some evaluation context. -/
theorem progress_gen {Γ R e τ m ε} (h : HasType Sig Γ R e τ m ε) : Γ = Ctx.empty →
    ∀ {L : RCtx} {n : Nat}, (∀ r q, R r = some q → L r = some q) →
      IsVal e ∨ e = .abort ∨
      (∃ E l v, E.Free (.user l) ∧ IsVal v ∧ e = plug E (.perform l v) ∧ Label.user l ∈ ε) ∨
      ∃ E e0 a e0' L' n', e = plug E e0 ∧ Head Sig e0 L n a e0' L' n' := by
  induction h with
  | var hi => intro hΓ; subst hΓ; simp [Ctx.empty] at hi
  | unitV => intro _ _ _ _; exact Or.inl .unitV
  | natV k => intro _ _ _ _; exact Or.inl (.natV k)
  | hdlV r => intro _ _ _ _; exact Or.inl (.hdlV r)
  | kontV r body => intro _ _ _ _; exact Or.inl (.kontV r body)
  | pair ha hb hsΓ _ h1 h2 =>
    intro hΓ L n _; subst hΓ
    obtain ⟨e1, e2⟩ := split_empty hsΓ; subst e1; subst e2
    exact Or.inl (.pair (atom_closed ha h1) (atom_closed hb h2))
  | @letE _ _ _ _ _ _ e1 e2 _ _ _ _ _ _ _ hsΓ hsR _ _ _ h1s _ ih1 _ =>
    intro hΓ L n hL; subst hΓ
    obtain ⟨g1, g2⟩ := split_empty hsΓ; subst g1; subst g2
    rcases ih1 rfl (sub_left hsR hL) (n := n) with hv | hab | ⟨E, l, v, hfree, hv, he, hl⟩ |
        ⟨E, e0, a, e0', L', n', he, hh⟩
    · exact Or.inr (Or.inr (Or.inr ⟨.hole, _, _, _, _, _, rfl, .letV hv⟩))
    · subst hab; exact Or.inr (Or.inr (Or.inr ⟨.hole, _, _, _, _, _, rfl, .letAbort⟩))
    · exact Or.inr (Or.inr (Or.inl ⟨.letC E e2, l, v, hfree, hv, by simp [plug, he], h1s _ hl⟩))
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
  | @perform _ _ l _ _ _ ha h1 hl =>
    intro hΓ L n _; subst hΓ
    exact Or.inr (Or.inr (Or.inl ⟨.hole, l, _, trivial, atom_closed ha h1, rfl, hl⟩))
  | @handle _ _ _ _ l _ ret opc _ _ _ _ _ _ _ hsΓ _ _ hsub _ _ hc' ihb =>
    intro hΓ L n hL; subst hΓ
    obtain ⟨g1, g2⟩ := split_empty hsΓ; subst g1; subst g2
    rcases ihb rfl hL (n := n) with hv | hab | ⟨E, l', v, hfree, hv, he, hl⟩ |
        ⟨E, e0, a, e0', L', n', he, hh⟩
    · exact Or.inr (Or.inr (Or.inr ⟨.hole, _, _, _, _, _, rfl, .handleRet hv⟩))
    · subst hab; exact Or.inr (Or.inr (Or.inr ⟨.hole, _, _, _, _, _, rfl, .handleAbort⟩))
    · subst he
      by_cases hll : l' = l
      · subst hll
        exact Or.inr (Or.inr (Or.inr ⟨.hole, _, _, _, _, _, rfl, .capture hfree hv⟩))
      · have hne : Label.user l' ≠ Label.user l := fun h => hll (Label.user.inj h)
        have hmem := hsub _ hl
        simp only [List.mem_cons] at hmem
        rcases hmem with h | h
        · exact absurd h hne
        · exact Or.inr (Or.inr (Or.inl ⟨.handleC l E ret opc, l', v, ⟨hne, hfree⟩, hv, rfl, hc' _ h⟩))
    · exact Or.inr (Or.inr (Or.inr ⟨.handleC l E ret opc, e0, a, e0', L', n', by simp [plug, he], hh⟩))
  | resume hk ha hsΓ hsR h1 h2 =>
    intro hΓ L n hL; subst hΓ
    obtain ⟨g1, g2⟩ := split_empty hsΓ; subst g1; subst g2
    obtain ⟨r, body, rfl, hr⟩ := canon_kont h1 (atom_closed hk h1)
    exact Or.inr (Or.inr (Or.inr ⟨.hole, _, _, _, _, _, rfl,
      .resume (sub_left hsR hL r _ hr) (atom_closed ha h2)⟩))
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
  rcases progress_gen ht rfl (L := L) (n := n) (fun _ _ h => h) with hv | hab | ⟨_, l, _, _, _, _, hl⟩ |
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

/-! ## One-shot resumption and total linear exhaustion (Theorem 2) -/

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
    (ha : a = .mint r ∨ a = .capture r q) : L' r = some q ∨ (a = .mint r ∧ L' r = some .linear) := by
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
    c'.live r = some .linear := by
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
      have hl : L' r = some .linear := by
        rcases head_alloc hh (q := .linear) hnow with h | ⟨_, h⟩ <;> exact h
      exact steps_keep hrest hf1 hl h0'
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
  | zero => exact absurd (alloc_live hs hf0 hr h) (hdisc r)
  | succ k => omega

end Exhaustion

/-! ## Checked arithmetic -/

theorem ArithOp.eval_some {op : ArithOp} {a b c : Nat} (h : op.eval a b = some c) :
    (op = .add ∧ c = a + b ∧ c < u64Bound) ∨ (op = .sub ∧ b ≤ a ∧ c = a - b ∧ c < u64Bound) ∨
      (op = .mul ∧ c = a * b ∧ c < u64Bound) := by
  cases op <;> simp only [ArithOp.eval] at h <;> split at h <;> simp_all

theorem add_overflow_aborts : ArithOp.eval .add (u64Bound - 1) 1 = none := by
  simp [ArithOp.eval, u64Bound]

/-- A closed literal arithmetic expression has a pure typing exactly when
both operands are in range and its actual primitive evaluation succeeds. -/
theorem literal_arith_pure_iff (Sig : Nat → EffSig) (op : ArithOp) (a b : Nat) :
    HasType Sig Ctx.empty RCtx.empty (.arith op (.natV a) (.natV b)) .nat .copy [] ↔
      a < u64Bound ∧ b < u64Bound ∧ ∃ c, op.eval a b = some c := by
  constructor
  · intro h
    cases h with
    | arith _ _ _ _ _ _ hab => cases hab
    | arithLiteral _ _ ha hb hev => exact ⟨ha, hb, _, hev⟩
  · rintro ⟨ha, hb, c, hev⟩
    exact .arithLiteral discard_empty discardR_empty ha hb hev

/-- Failure of the primitive cannot be reclassified as pure at any result
mode or type. -/
theorem literal_arith_failure_not_pure (Sig : Nat → EffSig) {op : ArithOp} {a b : Nat}
    (hev : op.eval a b = none) (τ : Ty) (m : Mode) :
    ¬ HasType Sig Ctx.empty RCtx.empty (.arith op (.natV a) (.natV b)) τ m [] := by
  intro h
  cases h with
  | arith _ _ _ _ _ _ hab => cases hab
  | arithLiteral _ _ _ _ hsafe => rw [hev] at hsafe; contradiction

theorem literal_add_boundary_pure (Sig : Nat → EffSig) :
    HasType Sig Ctx.empty RCtx.empty
      (.arith .add (.natV (u64Bound - 2)) (.natV 1)) .nat .copy [] :=
  .arithLiteral (c := u64Bound - 1) discard_empty discardR_empty (by decide) (by decide)
    (by simp [ArithOp.eval, u64Bound])

theorem literal_sub_boundary_pure (Sig : Nat → EffSig) :
    HasType Sig Ctx.empty RCtx.empty
      (.arith .sub (.natV (u64Bound - 1)) (.natV (u64Bound - 1))) .nat .copy [] :=
  .arithLiteral (c := 0) discard_empty discardR_empty (by decide) (by decide)
    (by simp [ArithOp.eval, u64Bound])

theorem literal_mul_boundary_pure (Sig : Nat → EffSig) :
    HasType Sig Ctx.empty RCtx.empty
      (.arith .mul (.natV (u64Bound - 1)) (.natV 1)) .nat .copy [] :=
  .arithLiteral (c := u64Bound - 1) discard_empty discardR_empty (by decide) (by decide)
    (by simp [ArithOp.eval, u64Bound])

theorem sub_underflow_aborts : ArithOp.eval .sub 0 1 = none := by
  simp [ArithOp.eval]

theorem mul_overflow_aborts : ArithOp.eval .mul (u64Bound - 1) 2 = none := by
  simp [ArithOp.eval, u64Bound]

theorem literal_add_overflow_not_pure (Sig : Nat → EffSig) (τ : Ty) (m : Mode) :
    ¬ HasType Sig Ctx.empty RCtx.empty
      (.arith .add (.natV (u64Bound - 1)) (.natV 1)) τ m [] :=
  literal_arith_failure_not_pure Sig add_overflow_aborts τ m

theorem literal_sub_underflow_not_pure (Sig : Nat → EffSig) (τ : Ty) (m : Mode) :
    ¬ HasType Sig Ctx.empty RCtx.empty (.arith .sub (.natV 0) (.natV 1)) τ m [] :=
  literal_arith_failure_not_pure Sig sub_underflow_aborts τ m

theorem literal_mul_overflow_not_pure (Sig : Nat → EffSig) (τ : Ty) (m : Mode) :
    ¬ HasType Sig Ctx.empty RCtx.empty
      (.arith .mul (.natV (u64Bound - 1)) (.natV 2)) τ m [] :=
  literal_arith_failure_not_pure Sig mul_overflow_aborts τ m

/-- Evaluation alone is insufficient: oversized natural literals are outside
this new u64 literal rule even if subtraction would produce zero. -/
theorem oversized_literal_sub_eval : ArithOp.eval .sub u64Bound u64Bound = some 0 := by
  simp [ArithOp.eval, u64Bound]

theorem oversized_literal_sub_not_pure (Sig : Nat → EffSig) (τ : Ty) (m : Mode) :
    ¬ HasType Sig Ctx.empty RCtx.empty
      (.arith .sub (.natV u64Bound) (.natV u64Bound)) τ m [] := by
  intro h
  cases h with
  | arith _ _ _ _ _ _ hab => cases hab
  | arithLiteral _ _ ha _ _ => exact (Nat.lt_irrefl _ ha)

/-! ## Modal kinds: the field-modality premise is necessary -/

/-- [K-Adt] for `copy struct Hidden { t : Ticket @@ copy, u : Unit @@ copy }`
computes kind copy, although the struct would contain a linear ticket. -/
theorem hidden_ticket_kind_is_copy : (Ty.pair .copy .hdl .copy .unit .copy).kind = .copy := by
  decide

/-- The premise `k_i ⊑ m_i` rejects every such value, at every resource context. -/
theorem hidden_ticket_rejected (Sig : Nat → EffSig) (R : RCtx) (m : Mode) (ε : Row) :
    ¬ HasType Sig Ctx.empty R (.pair (.hdlV 0) .unitV) (Ty.pair .copy .hdl .copy .unit .copy) m ε := by
  intro h
  obtain ⟨_, _, _, _, hvt, _⟩ := val_inv h (.pair (.hdlV 0) .unitV)
  cases hvt with
  | pair _ _ _ hk1 _ => exact absurd hk1 (by decide)

/-- `Option[WriteTxn]`-style: an unannotated (linear) field keeps the obligation. -/
theorem linear_field_kind : (Ty.pair .copy .hdl .linear .unit .linear).kind = .linear := by decide

/-! ## Worked examples -/

/-- Effect 0 is a linear (Storage-like) effect `nat → nat`; every other label
is an affine (Fail-like) effect `unit → nat`. -/
def exSig : Nat → EffSig
  | 0 => ⟨.linear, .nat, .nat⟩
  | _ => ⟨.affine, .unit, .nat⟩

/-- The §6 worked derivation: `let tx = begin_txn(); validate()?; tx.commit()`
with the abortable step modelled by checked arithmetic. -/
def txnAcrossAbort : Expr :=
  .letE .mint (.letE (.arith .add (.natV (u64Bound - 1)) (.natV 1)) (.consume (.var 1)))

/-- Rejected by the Resumption Frame Condition: `linear ⊑ affine` is false. -/
theorem txn_across_abort_rejected (τ : Ty) (m : Mode) (ε : Row) :
    ¬ HasType exSig Ctx.empty RCtx.empty txnAcrossAbort τ m ε := by
  intro h
  cases h with
  | @letE _ Γ1 Γ2 _ _ _ _ _ _ m1 _ _ _ _ _ hsΓ _ h1 h2 _ _ _ =>
    obtain ⟨g1, g2⟩ := split_empty hsΓ; subst g1; subst g2
    cases h1 with
    | mint _ _ hm1 =>
      cases h2 with
      | @letE _ Γ1' Γ2' _ _ _ _ _ _ _ ε1' _ _ _ _ hsΓ' _ h1' h2' hf' _ _ =>
        cases h1' with
        | arith _ _ _ _ _ _ hab =>
          cases h2' with
          | consume _ hv =>
            cases hv with
            | @var _ _ _ _ mx _ _ hi _ _ _ =>
              have h0 := hsΓ' 0
              simp only [Ctx.ext] at hi
              have heq := splitE_right h0 hi
              simp [Ctx.ext] at heq
              obtain ⟨_, rfl⟩ := heq
              have hfr := hf'.1 0 _ _ hi
              have hres := res_le exSig hab
              have : Mode.Le Mode.linear Mode.affine :=
                Mode.le_trans hm1 (Mode.le_trans hfr hres)
              exact absurd this (by decide)

        | arithLiteral _ _ _ _ hsafe =>
          rw [add_overflow_aborts] at hsafe
          contradiction

/-- Literal 1+2 is safe while the handle remains live in the let frame. -/
def txnAcrossSafeArith : Expr :=
  .letE .mint (.letE (.arith .add (.natV 1) (.natV 2)) (.consume (.var 1)))

theorem txn_across_safe_arith_typed :
    HasType exSig Ctx.empty RCtx.empty txnAcrossSafeArith .unit .copy [] := by
  refine .letE (split_left_all _) (splitR_empty_left _) (ε1 := []) (τ1 := .hdl) (m1 := .linear)
    (.mint discard_empty discardR_empty (Mode.le_refl _)) ?_
    ⟨fun _ _ _ h => (by cases h), fun _ _ h => (by cases h)⟩
    (rowSub_refl _) (rowSub_refl _)
  have hsplit : Split (Ctx.one (.hdl, .linear)) Ctx.empty (Ctx.one (.hdl, .linear)) := by
    intro i; cases i with
    | zero => exact .right _
    | succ i => exact .none
  refine .letE hsplit (splitR_empty_left _) (ε1 := []) (τ1 := .nat) (m1 := .copy)
    (.arithLiteral (c := 3) discard_empty discardR_empty (by decide) (by decide)
    (by simp [ArithOp.eval, u64Bound])) ?_
    ⟨fun _ _ m _ => Mode.le_linear m, fun _ _ h => (by cases h)⟩
    (rowSub_refl _) (rowSub_refl _)
  refine .consume (.var 1) (.var (mx := .linear) rfl ?_ discardR_empty (Mode.le_refl _))
  intro j t m hne hj
  cases j with
  | zero =>
    simp only [Ctx.ext, Option.some.injEq, Prod.mk.injEq] at hj
    rcases hj with ⟨rfl, rfl⟩
    exact Mode.copy_le _
  | succ j =>
    cases j with
    | zero => exact absurd rfl hne
    | succ j => simp [Ctx.one, Ctx.ext, Ctx.empty] at hj

theorem txn_across_safe_arith_runs : ∃ L n,
    Steps exSig ⟨txnAcrossSafeArith, RCtx.empty, 0⟩
      [.mint 0, .silent, .silent, .silent, .consume 0] ⟨.unitV, L, n⟩ := by
  refine ⟨?L, ?n, ?main⟩
  case main =>
    apply Steps.cons (Step.ctx (.letC .hole (.letE (.arith .add (.natV 1) (.natV 2)) (.consume (.var 1))))
      Head.mint)
    apply Steps.cons (Step.ctx .hole (Head.letV (.hdlV 0)))
    apply Steps.cons (Step.ctx (.letC .hole (.consume (.hdlV 0)))
      (Head.arithOk (c := 3) (by simp [ArithOp.eval, u64Bound])))
    apply Steps.cons (Step.ctx .hole (Head.letV (.natV 3)))
    apply Steps.cons (Step.ctx .hole (Head.consume (by simp [RCtx.set])))
    exact .refl _

theorem txn_across_safe_arith_exhausts :
    ∀ L n, Steps exSig ⟨txnAcrossSafeArith, RCtx.empty, 0⟩
      [.mint 0, .silent, .silent, .silent, .consume 0] ⟨.unitV, L, n⟩ →
      relCount 0 [.mint 0, .silent, .silent, .silent, .consume 0] = 1 :=
  fun _ _ hs => total_linear_exhaustion txn_across_safe_arith_typed (fun l h => by simp at h) hs
    (Or.inl .unitV) 0 (Or.inl (by simp))

/-- Consuming the handle before the abortable step is accepted. -/
def txnThenArith : Expr :=
  .letE .mint (.letE (.consume (.var 0)) (.arith .add (.natV 1) (.natV 2)))

theorem one_none_split (b : Ty × Mode) : Split (Ctx.one b) (Ctx.one b) Ctx.empty := split_left_all _

theorem txn_then_arith_typed :
    HasType exSig Ctx.empty RCtx.empty txnThenArith .nat .copy [.abort] := by
  refine .letE (split_left_all _) (splitR_empty_left _) (ε1 := []) (τ1 := .hdl) (m1 := .linear)
    (.mint discard_empty discardR_empty (Mode.le_refl _)) ?_ ⟨fun _ _ _ h => (by cases h), fun _ _ h => (by cases h)⟩
    (fun _ h => by cases h) (rowSub_refl _)
  refine .letE (split_left_all _) (splitR_empty_left _) (ε1 := []) (τ1 := .unit) (m1 := .copy)
    (.consume (.var 0) (.var (mx := .linear) rfl (fun j t m' hj h => by
        cases j with
        | zero => exact absurd rfl hj
        | succ j => simp [Ctx.ext, Ctx.empty] at h) discardR_empty (Mode.le_refl _)))
    ?_ ⟨fun _ _ _ h => (by cases h), fun _ _ h => (by cases h)⟩ (fun _ h => by cases h) (rowSub_refl _)
  have hd : Discard (Ctx.empty.ext (Ty.unit, Mode.copy)) := by
    intro i t m h
    cases i with
    | zero => simp [Ctx.ext] at h; rw [← h.2]; decide
    | succ i => simp [Ctx.ext, Ctx.empty] at h
  exact .arith (.val (.natV 1)) (.val (.natV 2)) (split_left_all _) (splitR_empty_left _)
    (.natV 1 hd discardR_empty) (.natV 2 discard_empty discardR_empty) (by simp)

theorem txn_then_arith_runs : ∃ L n,
    Steps exSig ⟨txnThenArith, RCtx.empty, 0⟩
      [.mint 0, .silent, .consume 0, .silent, .silent] ⟨.natV 3, L, n⟩ := by
  refine ⟨?L, ?n, ?main⟩
  case main =>
    apply Steps.cons (Step.ctx (.letC .hole (.letE (.consume (.var 0)) (.arith .add (.natV 1) (.natV 2))))
      Head.mint)
    apply Steps.cons (Step.ctx .hole (Head.letV (.hdlV 0)))
    apply Steps.cons (Step.ctx (.letC .hole (.arith .add (.natV 1) (.natV 2)))
      (Head.consume (by simp [RCtx.set])))
    apply Steps.cons (Step.ctx .hole (Head.letV .unitV))
    apply Steps.cons (Step.ctx .hole (Head.arithOk (c := 3) (by simp [ArithOp.eval, u64Bound])))
    exact .refl _

/-- The minted ticket is consumed exactly once, as Theorem 2 predicts. -/
example : relCount 0 [.mint 0, .silent, .consume 0, .silent, .silent] = 1 := by decide

/-- A linear handler: the clause must resume `k` exactly once. -/
def linHandler : Expr :=
  .handle 0 (.letE (.perform 0 (.natV 5)) (.var 0)) (.var 0) (.resume (.var 0) (.var 1))

theorem lin_handler_typed : HasType exSig Ctx.empty RCtx.empty linHandler .nat .copy [] := by
  have hd1 : Discard (Ctx.empty.ext (Ty.nat, Mode.copy)) := by
    intro i t m h
    cases i with
    | zero => simp [Ctx.ext] at h; rw [← h.2]; decide
    | succ i => simp [Ctx.ext, Ctx.empty] at h
  refine .handle (εb := [.user 0]) (εc := []) (τb := .nat) (mb := .copy) (split_left_all _) allCopy_empty
    ?_ (fun l h => by simp at h; simp [h]) ?_ ?_ (rowSub_refl _)
  · refine .letE (split_left_all _) (splitR_empty_left _) (τ1 := .nat) (m1 := .copy) (ε1 := [.user 0])
      (.perform (.val (.natV 5)) (.natV 5 discard_empty discardR_empty) (by simp) (Mode.le_refl _)) ?_
      ⟨fun _ _ _ h => (by cases h), fun _ _ h => (by cases h)⟩ (rowSub_refl _) (rowSub_refl _)
    exact .var (mx := .copy) rfl (fun j t m' hj h => by
        cases j with
        | zero => exact absurd rfl hj
        | succ j => simp [Ctx.ext, Ctx.empty] at h) discardR_empty (Mode.le_refl _)
  · exact .var (mx := .copy) rfl (fun j t m' hj h => by
        cases j with
        | zero => exact absurd rfl hj
        | succ j => simp [Ctx.ext, Ctx.empty] at h) discardR_empty (Mode.le_refl _)
  · refine .resume (Γ1 := fun i => if i = 0 then some (.kont .nat .nat .copy [] .linear, .linear) else none)
      (Γ2 := fun i => if i = 1 then some (.nat, .linear) else none)
      (σ := .nat) (q := .linear) (.var 0) (.var 1) ?_ (splitR_empty_left _) ?_ ?_ (rowSub_refl _)
    · intro i
      match i with
      | 0 => exact .left _
      | 1 => exact .right _
      | i + 2 => exact .none
    · exact .var (mx := .linear) rfl (fun j t m' hj h => by simp_all) discardR_empty (Mode.le_refl _)
    · exact .var (mx := .linear) rfl (fun j t m' hj h => by simp_all) discardR_empty (Mode.copy_le _)

theorem lin_handler_runs : ∃ L n,
    Steps exSig ⟨linHandler, RCtx.empty, 0⟩
      [.capture 0 .linear, .resume 0, .silent, .silent] ⟨.natV 5, L, n⟩ := by
  refine ⟨?L, ?n, ?main⟩
  case main =>
    apply Steps.cons (Step.ctx .hole (Head.capture (E := .letC .hole (.var 0)) trivial (.natV 5)))
    apply Steps.cons (Step.ctx .hole (Head.resume (q := .linear) (by simp [RCtx.set]; rfl) (.natV 5)))
    apply Steps.cons (Step.ctx (.handleC 0 .hole (.var 0) (.resume (.var 0) (.var 1))) (Head.letV (.natV 5)))
    apply Steps.cons (Step.ctx .hole (Head.handleRet (.natV 5)))
    exact .refl _

/-- An affine (Fail-like) handler may drop its continuation. -/
def affHandler : Expr :=
  .handle 1 (.perform 1 .unitV) (.var 0) (.letE (.var 1) (.natV 0))

theorem aff_handler_typed : HasType exSig Ctx.empty RCtx.empty affHandler .nat .copy [] := by
  refine .handle (εb := [.user 1]) (εc := []) (τb := .nat) (mb := .copy) (split_left_all _) allCopy_empty
    (.perform (.val .unitV) (.unitV discard_empty discardR_empty) (by simp) (Mode.le_refl _))
    (fun l h => by simp at h; simp [h]) ?_ ?_ (rowSub_refl _)
  · exact .var (mx := .copy) rfl (fun j t m' hj h => by
        cases j with
        | zero => exact absurd rfl hj
        | succ j => simp [Ctx.ext, Ctx.empty] at h) discardR_empty (Mode.le_refl _)
  · refine .letE (Γ1 := fun i => if i = 1 then some (.unit, .linear) else none)
      (Γ2 := fun i => if i = 0 then some (.kont .nat .nat .copy [] .affine, .affine) else none)
      ?_ (splitR_empty_left _) (τ1 := .unit) (m1 := .copy) (ε1 := []) ?_ ?_ ?_ (rowSub_refl _) (rowSub_refl _)
    · intro i
      match i with
      | 0 => exact .right _
      | 1 => exact .left _
      | i + 2 => exact .none
    · exact .var (mx := .linear) rfl (fun j t m' hj h => by simp_all) discardR_empty (Mode.copy_le _)
    · refine .natV 0 ?_ discardR_empty
      intro i t m h
      match i with
      | 0 => simp [Ctx.ext] at h; rw [← h.2]; decide
      | 1 => simp [Ctx.ext] at h; rw [← h.2]; decide
      | i + 2 => simp [Ctx.ext] at h
    · refine ⟨fun i t m h => ?_, fun _ _ h => by cases h⟩
      by_cases hi : i = 0
      · subst hi; simp at h; rw [← h.2]; decide
      · simp [hi] at h

theorem aff_handler_runs : ∃ L n,
    Steps exSig ⟨affHandler, RCtx.empty, 0⟩ [.capture 0 .affine, .silent] ⟨.natV 0, L, n⟩ ∧
      L 0 = some .affine := by
  refine ⟨RCtx.set RCtx.empty 0 (some (exSig 1).q), 1, ?_, by simp [RCtx.set]; rfl⟩
  apply Steps.cons (Step.ctx .hole (Head.capture (E := .hole) trivial .unitV))
  apply Steps.cons (Step.ctx .hole (Head.letV .unitV))
  exact .refl _

/-- A linear handler that drops its continuation is rejected. -/
theorem linear_drop_rejected (τ : Ty) (m : Mode) (ε : Row) :
    ¬ HasType exSig Ctx.empty RCtx.empty
      (.handle 0 (.perform 0 (.natV 5)) (.var 0) (.letE (.var 1) (.natV 0))) τ m ε := by
  intro h
  cases h with
  | handle hsΓ _ _ _ _ ho _ =>
    obtain ⟨g1, g2⟩ := split_empty hsΓ; subst g1; subst g2
    cases ho with
    | @letE _ Γ1 Γ2 _ _ _ _ _ _ _ _ _ _ _ _ hsΓ' _ h1 h2 _ _ _ =>
      have h0 := hsΓ' 0
      simp only [Ctx.ext] at h0
      rcases splitE_some h0 with ⟨e1, _⟩ | ⟨_, e2⟩ | ⟨_, _, hc⟩
      · cases h1 with
        | var _ hj _ _ =>
          have := hj 0 _ _ (by decide) e1
          exact absurd this (by decide)
      · cases h2 with
        | natV _ hd _ =>
          have := hd 1 _ _ (by simp [Ctx.ext]; exact e2)
          exact absurd this (by decide)
      · simp [exSig] at hc

/-- Writes through two disjoint places of one root are accepted; the `swap2`
of `x.0` and `x.1` exchanges the fields. -/
example : swap2V (.pair (.natV 1) (.natV 2)) [false] [true] = some (.pair (.natV 2) (.natV 1)) := by
  simp [swap2V, xchg, vAt, vSet]

theorem checked_overflow_runs : ∃ L n,
    Steps exSig ⟨.letE (.arith .add (.natV (u64Bound - 1)) (.natV 1)) (.natV 0), RCtx.empty, 0⟩
      [.silent, .silent] ⟨.abort, L, n⟩ := by
  refine ⟨?L, ?n, ?main⟩
  case main =>
    apply Steps.cons (Step.ctx (.letC .hole (.natV 0)) (Head.arithAbort add_overflow_aborts))
    apply Steps.cons (Step.ctx .hole Head.letAbort)
    exact .refl _

/-! Theorem 2 instantiated on the concrete runs (non-vacuity checks). -/

theorem txn_then_arith_exhausts :
    ∀ L n, Steps exSig ⟨txnThenArith, RCtx.empty, 0⟩
      [.mint 0, .silent, .consume 0, .silent, .silent] ⟨.natV 3, L, n⟩ →
      relCount 0 [.mint 0, .silent, .consume 0, .silent, .silent] = 1 :=
  fun _ _ hs => total_linear_exhaustion txn_then_arith_typed (fun l h => by simp at h) hs
    (Or.inl (.natV 3)) 0 (Or.inl (by simp))

theorem lin_handler_resumes_once :
    ∀ L n, Steps exSig ⟨linHandler, RCtx.empty, 0⟩
      [.capture 0 .linear, .resume 0, .silent, .silent] ⟨.natV 5, L, n⟩ →
      relCount 0 [.capture 0 .linear, .resume 0, .silent, .silent] = 1 ∧
      [Act.capture 0 .linear, .resume 0, .silent, .silent].count (.resume 0) ≤ 1 :=
  fun _ _ hs => ⟨total_linear_exhaustion lin_handler_typed (fun l h => by simp at h) hs
    (Or.inl (.natV 5)) 0 (Or.inr (by simp)), one_shot_resumption hs (fun _ _ => rfl) 0⟩

end Vesper.MetatheoryFull



-- Kernel axiom audit: every theorem of this file.
#print axioms Vesper.MetatheoryFull.Mode.le_refl
#print axioms Vesper.MetatheoryFull.Mode.le_trans
#print axioms Vesper.MetatheoryFull.Mode.le_antisymm
#print axioms Vesper.MetatheoryFull.Mode.copy_le
#print axioms Vesper.MetatheoryFull.Mode.le_linear
#print axioms Vesper.MetatheoryFull.Mode.le_join_left
#print axioms Vesper.MetatheoryFull.Mode.le_join_right
#print axioms Vesper.MetatheoryFull.Mode.join_le
#print axioms Vesper.MetatheoryFull.Mode.meet_le_left
#print axioms Vesper.MetatheoryFull.Mode.meet_of_le
#print axioms Vesper.MetatheoryFull.Mode.le_copy
#print axioms Vesper.MetatheoryFull.RMode.le_refl
#print axioms Vesper.MetatheoryFull.RMode.not_le_copy
#print axioms Vesper.MetatheoryFull.RMode.le_affine_iff
#print axioms Vesper.MetatheoryFull.res_le
#print axioms Vesper.MetatheoryFull.res_mono
#print axioms Vesper.MetatheoryFull.splitR_comm
#print axioms Vesper.MetatheoryFull.splitR_empty_left
#print axioms Vesper.MetatheoryFull.splitR_empty_right
#print axioms Vesper.MetatheoryFull.splitR_of_empty_left
#print axioms Vesper.MetatheoryFull.splitR_of_empty_right
#print axioms Vesper.MetatheoryFull.splitR_congr
#print axioms Vesper.MetatheoryFull.splitR_assoc
#print axioms Vesper.MetatheoryFull.splitR_assoc'
#print axioms Vesper.MetatheoryFull.splitR_swap
#print axioms Vesper.MetatheoryFull.discardR_split
#print axioms Vesper.MetatheoryFull.discardR_join
#print axioms Vesper.MetatheoryFull.discardR_empty
#print axioms Vesper.MetatheoryFull.rleM_split
#print axioms Vesper.MetatheoryFull.rleM_part
#print axioms Vesper.MetatheoryFull.rleM_mono
#print axioms Vesper.MetatheoryFull.rleM_copy
#print axioms Vesper.MetatheoryFull.rleM_affine_discard
#print axioms Vesper.MetatheoryFull.empty_rleM
#print axioms Vesper.MetatheoryFull.empty_discardR
#print axioms Vesper.MetatheoryFull.splitE_left
#print axioms Vesper.MetatheoryFull.splitE_right
#print axioms Vesper.MetatheoryFull.split_empty
#print axioms Vesper.MetatheoryFull.split_left_all
#print axioms Vesper.MetatheoryFull.discard_empty
#print axioms Vesper.MetatheoryFull.allCopy_empty
#print axioms Vesper.MetatheoryFull.ext_del
#print axioms Vesper.MetatheoryFull.split_del
#print axioms Vesper.MetatheoryFull.discard_del
#print axioms Vesper.MetatheoryFull.allCopy_del
#print axioms Vesper.MetatheoryFull.discard_split
#print axioms Vesper.MetatheoryFull.split4
#print axioms Vesper.MetatheoryFull.splitR_congr_left
#print axioms Vesper.MetatheoryFull.splitR_self_empty
#print axioms Vesper.MetatheoryFull.rempty_eq
#print axioms Vesper.MetatheoryFull.splitR_of_empty
#print axioms Vesper.MetatheoryFull.del_eq
#print axioms Vesper.MetatheoryFull.splitE_none
#print axioms Vesper.MetatheoryFull.splitE_some
#print axioms Vesper.MetatheoryFull.vt_isVal
#print axioms Vesper.MetatheoryFull.vt_rleM
#print axioms Vesper.MetatheoryFull.val_inv
#print axioms Vesper.MetatheoryFull.val_ty
#print axioms Vesper.MetatheoryFull.subst_val
#print axioms Vesper.MetatheoryFull.substOK_ext
#print axioms Vesper.MetatheoryFull.substOK_discard
#print axioms Vesper.MetatheoryFull.substOK_split
#print axioms Vesper.MetatheoryFull.substOK_handle
#print axioms Vesper.MetatheoryFull.substOK_frame
#print axioms Vesper.MetatheoryFull.isAtom_subst
#print axioms Vesper.MetatheoryFull.discard_rest
#print axioms Vesper.MetatheoryFull.subst_ty
#print axioms Vesper.MetatheoryFull.weakenR
#print axioms Vesper.MetatheoryFull.rowSub_trans
#print axioms Vesper.MetatheoryFull.rowSub_refl
#print axioms Vesper.MetatheoryFull.weakenRow
#print axioms Vesper.MetatheoryFull.splitR_rot
#print axioms Vesper.MetatheoryFull.vt_at
#print axioms Vesper.MetatheoryFull.vt_xchg
#print axioms Vesper.MetatheoryFull.vt_swap2
#print axioms Vesper.MetatheoryFull.bind_map_l
#print axioms Vesper.MetatheoryFull.bind_map_r
#print axioms Vesper.MetatheoryFull.vSet_comm_disjoint
#print axioms Vesper.MetatheoryFull.vSet_overlap_not_comm
#print axioms Vesper.MetatheoryFull.overlap_not_disj
#print axioms Vesper.MetatheoryFull.frame_rleM
#print axioms Vesper.MetatheoryFull.plug_inv
#print axioms Vesper.MetatheoryFull.ext_del_zero
#print axioms Vesper.MetatheoryFull.regroup
#print axioms Vesper.MetatheoryFull.nat_discard
#print axioms Vesper.MetatheoryFull.splitR_left_of_empty
#print axioms Vesper.MetatheoryFull.head_pres
#print axioms Vesper.MetatheoryFull.preservation
#print axioms Vesper.MetatheoryFull.atom_closed
#print axioms Vesper.MetatheoryFull.sub_left
#print axioms Vesper.MetatheoryFull.sub_right
#print axioms Vesper.MetatheoryFull.canon_nat
#print axioms Vesper.MetatheoryFull.canon_hdl
#print axioms Vesper.MetatheoryFull.canon_kont
#print axioms Vesper.MetatheoryFull.canon_pair
#print axioms Vesper.MetatheoryFull.vt_set_exists
#print axioms Vesper.MetatheoryFull.progress_gen
#print axioms Vesper.MetatheoryFull.progress
#print axioms Vesper.MetatheoryFull.type_safety
#print axioms Vesper.MetatheoryFull.head_next
#print axioms Vesper.MetatheoryFull.head_fresh
#print axioms Vesper.MetatheoryFull.head_dead
#print axioms Vesper.MetatheoryFull.head_release
#print axioms Vesper.MetatheoryFull.head_keep
#print axioms Vesper.MetatheoryFull.head_alloc
#print axioms Vesper.MetatheoryFull.step_parts
#print axioms Vesper.MetatheoryFull.step_fresh
#print axioms Vesper.MetatheoryFull.step_dead
#print axioms Vesper.MetatheoryFull.steps_fresh
#print axioms Vesper.MetatheoryFull.steps_dead
#print axioms Vesper.MetatheoryFull.release_at_most_once
#print axioms Vesper.MetatheoryFull.count_resume_le_rel
#print axioms Vesper.MetatheoryFull.one_shot_resumption
#print axioms Vesper.MetatheoryFull.steps_keep
#print axioms Vesper.MetatheoryFull.alloc_live
#print axioms Vesper.MetatheoryFull.terminal_no_linear
#print axioms Vesper.MetatheoryFull.total_linear_exhaustion
#print axioms Vesper.MetatheoryFull.ArithOp.eval_some
#print axioms Vesper.MetatheoryFull.add_overflow_aborts
#print axioms Vesper.MetatheoryFull.hidden_ticket_kind_is_copy
#print axioms Vesper.MetatheoryFull.hidden_ticket_rejected
#print axioms Vesper.MetatheoryFull.linear_field_kind
#print axioms Vesper.MetatheoryFull.txn_across_abort_rejected
#print axioms Vesper.MetatheoryFull.one_none_split
#print axioms Vesper.MetatheoryFull.txn_then_arith_typed
#print axioms Vesper.MetatheoryFull.txn_then_arith_runs
#print axioms Vesper.MetatheoryFull.lin_handler_typed
#print axioms Vesper.MetatheoryFull.lin_handler_runs
#print axioms Vesper.MetatheoryFull.aff_handler_typed
#print axioms Vesper.MetatheoryFull.aff_handler_runs
#print axioms Vesper.MetatheoryFull.linear_drop_rejected
#print axioms Vesper.MetatheoryFull.checked_overflow_runs
#print axioms Vesper.MetatheoryFull.txn_then_arith_exhausts
#print axioms Vesper.MetatheoryFull.lin_handler_resumes_once

#print axioms Vesper.MetatheoryFull.literal_arith_pure_iff
#print axioms Vesper.MetatheoryFull.literal_arith_failure_not_pure
#print axioms Vesper.MetatheoryFull.literal_add_boundary_pure
#print axioms Vesper.MetatheoryFull.literal_sub_boundary_pure
#print axioms Vesper.MetatheoryFull.literal_mul_boundary_pure
#print axioms Vesper.MetatheoryFull.sub_underflow_aborts
#print axioms Vesper.MetatheoryFull.mul_overflow_aborts
#print axioms Vesper.MetatheoryFull.literal_add_overflow_not_pure
#print axioms Vesper.MetatheoryFull.literal_sub_underflow_not_pure
#print axioms Vesper.MetatheoryFull.literal_mul_overflow_not_pure
#print axioms Vesper.MetatheoryFull.txn_across_safe_arith_typed
#print axioms Vesper.MetatheoryFull.txn_across_safe_arith_runs
#print axioms Vesper.MetatheoryFull.txn_across_safe_arith_exhausts

#print axioms Vesper.MetatheoryFull.oversized_literal_sub_eval
#print axioms Vesper.MetatheoryFull.oversized_literal_sub_not_pure
