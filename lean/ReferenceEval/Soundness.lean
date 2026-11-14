import ReferenceEval
import MetatheoryFull

namespace Vesper.ReferenceEval.Soundness

open Vesper.ReferenceEval
open Vesper.MetatheoryFull (HasType Ctx RCtx Split SplitR SplitE Discard DiscardR FrameOK RowSub IsAtom IsVal Label Mode Ty)

abbrev MExpr := Vesper.MetatheoryFull.Expr
abbrev MTy := Vesper.MetatheoryFull.Ty

/-- The model context of a list of names: every name is a copy `nat`. -/
def ctxOf (ns : List String) : Ctx := fun i => if i < ns.length then some (.nat, .copy) else none

def arithOf : BinOp → Option Vesper.MetatheoryFull.ArithOp
  | .add => some .add | .sub => some .sub | .mul => some .mul | _ => none

/-- Atoms of the overlap: a bound name or a non-negative literal. -/
inductive OAtom (ns : List String) : Expr → Prop where
  | var {x} : x ∈ ns → OAtom ns (.var x)
  | int {n : Int} : 0 ≤ n → OAtom ns (.int n)

/-- The overlap of the reference fragment with Appendix A's calculus, in
A-normal form, over u64: atoms, checked `+ - *` of atoms, `let x: u64`,
and `if a == 0 { .. } else { .. }` on an atom (the model's `ifz`). -/
inductive OK : List String → Expr → Prop where
  | atom {ns e} : OAtom ns e → OK ns e
  | arith {ns op k a b} : (arithOf op).isSome → OAtom ns a → OAtom ns b → OK ns (.bin op k a b)
  | letE {ns x v b} : OK ns v → OK (x :: ns) b → OK ns (.letE x (some (.int .u64)) v b)
  | ifz {ns k a t e} : OAtom ns a → OK ns t → OK ns e →
      OK ns (.ite (.bin .eq k a (.int 0)) t e)

def trAtom (ns : List String) : Expr → MExpr
  | .var x => .var (ns.idxOf x)
  | .int n => .natV n.toNat
  | _ => .unitV

/-- The translation into the model (de Bruijn indices by first occurrence). -/
def tr : List String → Expr → MExpr
  | ns, .bin op _ a b => match arithOf op with
      | some o => .arith o (trAtom ns a) (trAtom ns b)
      | none => .unitV
  | ns, .letE x _ v b => .letE (tr ns v) (tr (x :: ns) b)
  | ns, .ite (.bin .eq _ a (.int 0)) t e => .ifz (trAtom ns a) (tr ns t) (tr ns e)
  | ns, e => trAtom ns e

section
variable (Sig : Nat → Vesper.MetatheoryFull.EffSig)

theorem ctx_some {ns : List String} {i t m} (h : ctxOf ns i = some (t, m)) :
    t = .nat ∧ m = .copy := by
  unfold ctxOf at h; split at h
  · simp only [Option.some.injEq, Prod.mk.injEq] at h; exact ⟨h.1.symm, h.2.symm⟩
  · cases h

theorem ctx_discard (ns : List String) : Discard (ctxOf ns) := by
  intro i t m h; rw [(ctx_some h).2]; decide

theorem ctx_split (ns : List String) : Split (ctxOf ns) (ctxOf ns) (ctxOf ns) := by
  intro i; unfold ctxOf; split
  · exact SplitE.copy _
  · exact SplitE.none

theorem splitR_empty : SplitR RCtx.empty RCtx.empty RCtx.empty := fun _ => Or.inl ⟨rfl, rfl⟩

theorem discardR_empty : DiscardR RCtx.empty := fun _ h => by cases h

theorem ctx_ext (x : String) (ns : List String) :
    ctxOf (x :: ns) = (ctxOf ns).ext (.nat, .copy) := by
  funext i; cases i <;> simp [ctxOf, Ctx.ext]

theorem atom_isAtom {ns e} (h : OAtom ns e) : IsAtom (trAtom ns e) := by
  cases h with
  | var _ => exact IsAtom.var _
  | int _ => exact IsAtom.val (IsVal.natV _)

theorem atom_typed {ns e} (h : OAtom ns e) (ε) :
    HasType Sig (ctxOf ns) RCtx.empty (trAtom ns e) .nat .copy ε := by
  cases h with
  | @var x hx =>
      apply HasType.var (mx := .copy)
      · simp [ctxOf, List.idxOf_lt_length_iff.mpr hx]
      · intro j t m' _ h; rw [(ctx_some h).2]; decide
      · exact discardR_empty
      · exact Mode.le_refl _
  | int _ => exact HasType.natV _ (ctx_discard ns) discardR_empty

theorem frame_ok (ns : List String) (q) : FrameOK (ctxOf ns) RCtx.empty q := by
  constructor
  · intro i t m h; rw [(ctx_some h).2]; exact Mode.copy_le _
  · intro r q' h; cases h

/-- Lemma A: every overlap term is typed by Appendix A at `nat`, mode copy,
with the abort row, in the all-copy `nat` context of its names. -/
theorem ok_typed : ∀ {ns e}, OK ns e →
    HasType Sig (ctxOf ns) RCtx.empty (tr ns e) .nat .copy [.abort]
  | ns, _, .atom h => by
      cases h with
      | var hx => exact atom_typed Sig (.var hx) _
      | int hn => exact atom_typed Sig (.int hn) _
  | ns, .bin op k a b, .arith hop ha hb => by
      obtain ⟨o, ho⟩ := Option.isSome_iff_exists.mp hop
      simp only [tr, ho]
      exact HasType.arith (atom_isAtom ha) (atom_isAtom hb) (ctx_split ns) splitR_empty
        (atom_typed Sig ha _) (atom_typed Sig hb _) (by simp)
  | ns, .letE x _ v b, .letE hv hb => by
      simp only [tr]
      have hb' := ok_typed hb
      rw [ctx_ext] at hb'
      exact HasType.letE (ctx_split ns) splitR_empty (ok_typed hv) hb' (frame_ok ns _)
        (fun _ h => h) (fun _ h => h)
  | ns, .ite _ t e, .ifz ha ht he => by
      simp only [tr]
      exact HasType.ifz (atom_isAtom ha) (ctx_split ns) splitR_empty (atom_typed Sig ha _)
        (ok_typed ht) (ok_typed he)

end

/-! ## Lemma B: what the reference checker's acceptance says about the overlap -/

def names (env : Env) : List String := env.map (·.name)

/-- A checker computation that never changes the names of the environment. -/
def Pres {α : Type} (x : CM α) : Prop :=
  ∀ s r, x s = Except.ok r → names r.2.env = names s.env

theorem bind_ok {α β : Type} {x : CM α} {f : α → CM β} {s : St} {r : β × St}
    (h : (x >>= f) s = Except.ok r) : ∃ a s', x s = Except.ok (a, s') ∧ f a s' = Except.ok r := by
  simp only [bind, StateT.bind] at h
  cases hx : x s with
  | error e => rw [hx] at h; cases h
  | ok p => rw [hx] at h; exact ⟨p.1, p.2, rfl, h⟩

theorem pres_bind {α β : Type} {x : CM α} {f : α → CM β} (hx : Pres x) (hf : ∀ a, Pres (f a)) :
    Pres (x >>= f) := by
  intro s r h
  obtain ⟨a, s', h1, h2⟩ := bind_ok h
  rw [hf a s' r h2, hx s (a, s') h1]

theorem pres_pure {α : Type} (a : α) : Pres (pure a : CM α) := by
  intro s r h; cases h; rfl

theorem pres_err {α : Type} (c m : String) : Pres (err c m : CM α) := by
  intro s r h; cases h

theorem pres_ite {α : Type} {c : Prop} [Decidable c] {x y : CM α} (hx : Pres x) (hy : Pres y) :
    Pres (if c then x else y) := by
  split
  · exact hx
  · exact hy

theorem pres_getEnv : Pres getEnv := by intro s r h; cases h; rfl
theorem pres_getObs : Pres getObs := by intro s r h; cases h; rfl
theorem pres_setObs (o : List Eff) : Pres (setObs o) := by intro s r h; cases h; rfl

theorem pres_expectTy (a b : Ty) : Pres (expectTy a b) := by
  unfold expectTy; exact pres_ite (pres_pure _) (pres_err _ _)

theorem pres_forIn {β : Type} (l : List Binding) (init : β)
    (f : Binding → β → CM (ForInStep β)) (hf : ∀ b r, Pres (f b r)) : Pres (forIn l init f) := by
  induction l generalizing init with
  | nil => exact pres_pure _
  | cons b l ih =>
      rw [List.forIn_cons]
      apply pres_bind (hf b init)
      intro r; cases r with
      | done x => exact pres_pure _
      | yield x => exact ih x

theorem names_markMoved (x : String) : ∀ env, names (markMoved x env) = names env
  | [] => rfl
  | b :: bs => by
      unfold markMoved; split
      · simp [names]
      · simp only [names, List.map_cons] at *; rw [← names, names_markMoved x bs]; rfl

theorem pres_setMoved (x : String) : Pres (setMoved x) := by
  intro s r h; cases h; exact names_markMoved x s.env

/-- Discharge `Pres` for straight-line checker code without recursive calls. -/
macro "pres_tac" : tactic => `(tactic| (try dsimp only; repeat (first
  | exact pres_pure _
  | exact pres_err _ _
  | exact pres_getEnv
  | exact pres_getObs
  | exact pres_setObs _
  | exact pres_expectTy _ _
  | exact pres_setMoved _
  | exact pres_requireEff _ _
  | exact pres_binTail _ _ _ _ _ _ _ _ _
  | refine pres_bind ?_ (fun _ => ?_)
  | refine pres_ite ?_ ?_
  | refine pres_forIn _ _ _ (fun _ _ => ?_)
  | split)))

theorem pres_requireEff (cx : Cx) (e : Eff) : Pres (requireEff cx e) := by
  unfold requireEff; pres_tac

theorem pres_apply {α : Type} {m : CM α} {s : St} {r : α × St} (hm : Pres m)
    (h : m s = Except.ok r) : names r.2.env = names s.env := hm s r h

set_option maxRecDepth 8000 in
theorem pres_binTail (cx : Cx) (op : BinOp) (r l' r' : Expr) (a b : Ty) (lf rf : Option Int) :
    Pres (binTail cx op r l' r' a b lf rf) := by
  unfold binTail; pres_tac

theorem lookup_ok {x : String} {s : St} {o s'} (h : lookup x s = Except.ok (o, s')) :
    s' = s ∧ o = s.env.find? (·.name == x) := by
  simp only [lookup, getEnv, bind, StateT.bind, get, getThe, MonadStateOf.get, StateT.get,
    pure, StateT.pure, Except.pure, Except.bind] at h
  cases h; exact ⟨rfl, rfl⟩

theorem mem_names_of_find {env : Env} {x : String} {b : Binding}
    (h : env.find? (·.name == x) = some b) : x ∈ names env := by
  have hm := List.mem_of_find?_eq_some h
  have hb := List.find?_some h
  simp only [beq_iff_eq] at hb
  exact List.mem_map.mpr ⟨b, hm, hb⟩

theorem infer_var {fuel : Nat} {cx : Cx} {x : String} {exp : Option Ty} {s : St} {r : Out × St}
    (hc : cx.prog.ctorOf x = none) (h : inferExpr (fuel + 1) cx (.var x) exp s = Except.ok r) :
    x ∈ names s.env ∧ names r.2.env = names s.env := by
  simp only [inferExpr] at h
  obtain ⟨o, s1, h1, h2⟩ := bind_ok h
  obtain ⟨rfl, rfl⟩ := lookup_ok h1
  cases hf : s1.env.find? (·.name == x) with
  | none =>
      rw [hf] at h2; simp only [hc] at h2; cases h2
  | some b =>
      rw [hf] at h2
      refine ⟨mem_names_of_find hf, pres_apply ?_ h2⟩
      pres_tac

theorem infer_int {fuel : Nat} {cx : Cx} {n : Int} {exp : Option Ty} {s : St} {r : Out × St}
    (h : inferExpr (fuel + 1) cx (.int n) exp s = Except.ok r) : names r.2.env = names s.env := by
  simp only [inferExpr] at h
  refine pres_apply ?_ h
  rcases exp.bind kindOf with _ | k <;> pres_tac

theorem check_zero {cx : Cx} {e : Expr} {exp : Option Ty} {s : St} {r : Out × St}
    (h : checkExpr 0 cx e exp s = Except.ok r) : False := by
  simp only [checkExpr, err, throw, throwThe, MonadExceptOf.throw] at h
  cases h

theorem infer_zero {cx : Cx} {e : Expr} {exp : Option Ty} {s : St} {r : Out × St}
    (h : inferExpr 0 cx e exp s = Except.ok r) : False := by
  simp only [inferExpr, err, throw, throwThe, MonadExceptOf.throw] at h
  cases h

theorem check_inv {fuel : Nat} {cx : Cx} {e : Expr} {exp : Option Ty} {s : St} {r : Out × St}
    (h : checkExpr (fuel + 1) cx e exp s = Except.ok r) :
    ∃ q s1, inferExpr fuel cx e exp s = Except.ok (q, s1) ∧ names r.2.env = names s1.env := by
  simp only [checkExpr] at h
  obtain ⟨q, s1, h1, h2⟩ := bind_ok h
  refine ⟨q, s1, h1, pres_apply ?_ h2⟩
  cases exp <;> pres_tac

/-- Surface atoms of the overlap. -/
inductive SAtom (P : Program) : Expr → Prop where
  | var {x} : P.ctorOf x = none → SAtom P (.var x)
  | int {n : Int} : 0 ≤ n → SAtom P (.int n)

/-- Surface shape of the overlap, before scoping. -/
inductive Shape (P : Program) : Expr → Prop where
  | atom {e} : SAtom P e → Shape P e
  | arith {op k a b} : (arithOf op).isSome → SAtom P a → SAtom P b → Shape P (.bin op k a b)
  | letE {x v b} : Shape P v → Shape P b → Shape P (.letE x (some (.int .u64)) v b)
  | ifz {k a t e} : SAtom P a → Shape P t → Shape P e → Shape P (.ite (.bin .eq k a (.int 0)) t e)

theorem atom_ok {cx : Cx} {e : Expr} (he : SAtom cx.prog e) :
    ∀ {fuel exp s r}, checkExpr fuel cx e exp s = Except.ok r →
      OAtom (names s.env) e ∧ names r.2.env = names s.env
  | 0, _, _, _, h => (check_zero h).elim
  | 1, _, _, _, h => by
      obtain ⟨_, _, h1, _⟩ := check_inv h; exact (infer_zero h1).elim
  | fuel + 2, _, _, _, h => by
      obtain ⟨q, s1, h1, h2⟩ := check_inv h
      rw [h2]
      cases he with
      | var hc => obtain ⟨hm, hn⟩ := infer_var hc h1; exact ⟨.var hm, hn⟩
      | int hn => exact ⟨.int hn, infer_int h1⟩

theorem logic_of_arith {op : BinOp} (h : (arithOf op).isSome) : op.logic = false := by
  cases op <;> simp_all [arithOf, BinOp.logic]

/-- A non-logical binary operator on atoms. -/
theorem binatom_ok {cx : Cx} {op : BinOp} {k : IntK} {a b : Expr} (hop : op.logic = false)
    (ha : SAtom cx.prog a) (hb : SAtom cx.prog b) :
    ∀ {fuel exp s r}, checkExpr fuel cx (.bin op k a b) exp s = Except.ok r →
      OAtom (names s.env) a ∧ OAtom (names s.env) b ∧ names r.2.env = names s.env
  | 0, _, _, _, h => (check_zero h).elim
  | 1, _, _, _, h => by
      obtain ⟨_, _, h1, _⟩ := check_inv h; exact (infer_zero h1).elim
  | fuel + 2, exp, s, r, h => by
      obtain ⟨q, s1, h1, h2⟩ := check_inv h
      rw [h2]
      simp only [inferExpr] at h1
      obtain ⟨pl, s2, hl, h3⟩ := bind_ok h1
      obtain ⟨oa, hn2⟩ := atom_ok ha hl
      obtain ⟨l', ta, lf⟩ := pl
      simp only [hop, Bool.false_eq_true, if_false] at h3
      obtain ⟨pr, s3, hr, h4⟩ := bind_ok h3
      obtain ⟨ob, hn3⟩ := atom_ok hb hr
      obtain ⟨r', tb, rf⟩ := pr
      have hn4 := pres_apply (pres_binTail _ _ _ _ _ _ _ _ _) h4
      rw [hn2] at ob
      exact ⟨oa, ob, by rw [hn4, hn3, hn2]⟩

theorem getEnv_ok {s : St} {a : Env} {s' : St} (h : getEnv s = Except.ok (a, s')) :
    a = s.env ∧ s' = s := by
  simp only [getEnv, bind, StateT.bind, get, getThe, MonadStateOf.get, StateT.get, pure,
    StateT.pure, Except.pure, Except.bind] at h
  cases h; exact ⟨rfl, rfl⟩

theorem setEnv_ok {e : Env} {s : St} {u : Unit} {s' : St} (h : setEnv e s = Except.ok (u, s')) :
    s'.env = e := by
  simp only [setEnv, modify, modifyGet, MonadStateOf.modifyGet, StateT.modifyGet, pure,
    Except.pure] at h
  cases h; rfl

theorem push_ok {b : Binding} {s : St} {u : Unit} {s' : St} (h : push b s = Except.ok (u, s')) :
    s'.env = b :: s.env := by
  simp only [push, modify, modifyGet, MonadStateOf.modifyGet, StateT.modifyGet, pure,
    Except.pure] at h
  cases h; rfl

theorem pop_ok {s : St} {u : Unit} {s' : St} (h : pop s = Except.ok (u, s')) :
    names s'.env = (names s.env).tail := by
  unfold pop at h
  obtain ⟨a, s1, h1, h2⟩ := bind_ok h
  obtain ⟨ha, hs1⟩ := getEnv_ok h1
  rw [ha, hs1] at h2
  revert h2
  cases hs : s.env with
  | nil =>
      intro h2
      simp only [pure, StateT.pure, Except.pure] at h2
      cases h2; simp [names, hs]
  | cons b bs =>
      intro h2
      simp only at h2
      split at h2
      · simp only [err, throw, throwThe, MonadExceptOf.throw] at h2; cases h2
      · rw [setEnv_ok h2]; simp [names]

theorem liftE_ok {α : Type} {x : Except Diag α} {s : St} {a : α} {s' : St}
    (h : liftE x s = Except.ok (a, s')) : s' = s := by
  simp only [liftE] at h
  cases hx : x with
  | error e => rw [hx] at h; cases h
  | ok v => rw [hx] at h; cases h; rfl

theorem names_refine_go (x : String) (upd : Binding → Binding) (hu : ∀ b, (upd b).name = b.name) :
    ∀ env : Env, names (refine.go x upd env) = names env
  | [] => rfl
  | b :: bs => by
      simp only [refine.go]
      split
      · simp [names, hu]
      · simp only [names, List.map_cons, List.cons.injEq, true_and]
        exact names_refine_go x upd hu bs

theorem names_refine (c : Expr) (t : Bool) (env : Env) : names (refine c t env) = names env := by
  unfold refine
  dsimp only
  split <;> (try dsimp only) <;> first | rfl | (apply names_refine_go; intro b; split <;> rfl)

theorem mergeEnv_names : ∀ {l r m : Env}, mergeEnv l r = Except.ok m → names l = names r →
    names m = names l
  | [], _, m, h, _ => by
      simp only [mergeEnv, pure, Except.pure] at h; cases h; rfl
  | _ :: _, [], _, _, hn => by simp [names] at hn
  | a :: as, b :: bs, m, h, hn => by
      simp only [mergeEnv] at h
      simp only [names, List.map_cons, List.cons.injEq] at hn
      split at h
      · simp only [bind, Except.bind] at h; cases h
      · simp only [bind, Except.bind, pure, Except.pure] at h
        cases hm : mergeEnv as bs with
        | error e => rw [hm] at h; cases h
        | ok rest =>
            rw [hm] at h; cases h
            have := mergeEnv_names hm hn.2
            simp only [names, List.map_cons] at this ⊢
            rw [this]

theorem pure_ok {α : Type} {a : α} {s : St} {r : α × St} (h : (pure a : CM α) s = Except.ok r) :
    names r.2.env = names s.env := pres_pure a s r h

/-- Lemma B: on the overlap's surface shape, acceptance by the reference checker
implies the term is well scoped in the overlap (`OK`) and the checker leaves
the environment's names as they were. -/
theorem shape_ok {cx : Cx} : ∀ {fuel : Nat} {e : Expr} {exp : Option Ty} {s : St} {r : Out × St},
    Shape cx.prog e → checkExpr fuel cx e exp s = Except.ok r →
      OK (names s.env) e ∧ names r.2.env = names s.env
  | 0, _, _, _, _, _, h => (check_zero h).elim
  | 1, _, _, _, _, _, h => by
      obtain ⟨_, _, h1, _⟩ := check_inv h; exact (infer_zero h1).elim
  | fuel + 2, _, exp, s, r, hs, h => by
      cases hs with
      | atom ha => obtain ⟨o, n⟩ := atom_ok ha h; exact ⟨.atom o, n⟩
      | arith hop ha hb =>
          obtain ⟨oa, ob, n⟩ := binatom_ok (logic_of_arith hop) ha hb h
          exact ⟨.arith hop oa ob, n⟩
      | @letE x v b hv hb =>
          obtain ⟨q, s1, h1, h2⟩ := check_inv h
          rw [h2]
          simp only [inferExpr] at h1
          obtain ⟨⟨v', actual, f⟩, s2, hv', h3⟩ := bind_ok h1
          obtain ⟨okv, nv⟩ := shape_ok hv hv'
          obtain ⟨envNow, s3, h4, h5⟩ := bind_ok h3
          obtain ⟨-, e3⟩ := getEnv_ok h4
          obtain ⟨_, s4, h6, h7⟩ := bind_ok h5
          have e4 := push_ok h6
          obtain ⟨⟨b', bt, bf⟩, s5, hb', h8⟩ := bind_ok h7
          obtain ⟨okb, nb⟩ := shape_ok hb hb'
          obtain ⟨_, s6, h9, h10⟩ := bind_ok h8
          have e6 := pop_ok h9
          have e7 := pure_ok h10
          have hx : names s4.env = x :: names s.env := by
            rw [e4, e3]; simp only [names, List.map_cons]; rw [← names, nv]; rfl
          rw [hx] at okb
          refine ⟨.letE okv okb, ?_⟩
          rw [e7, e6, nb, hx]; rfl
      | @ifz k a t el ha ht he =>
          obtain ⟨q, s1, h1, h2⟩ := check_inv h
          rw [h2]
          simp only [inferExpr] at h1
          obtain ⟨⟨c', ct, cf⟩, s2, hc, h3⟩ := bind_ok h1
          obtain ⟨oa, -, nc⟩ := binatom_ok (op := .eq) rfl ha (.int (Int.le_refl 0)) hc
          obtain ⟨_, s3, h4, h5⟩ := bind_ok h3
          have n3 := pres_expectTy _ _ _ _ h4
          obtain ⟨env, s4, h6, h7⟩ := bind_ok h5
          obtain ⟨he4, e4⟩ := getEnv_ok h6
          obtain ⟨_, s5, h8, h9⟩ := bind_ok h7
          have e5 := setEnv_ok h8
          obtain ⟨⟨t', ta, tf⟩, s6, ht', h10⟩ := bind_ok h9
          obtain ⟨okt, nt⟩ := shape_ok ht ht'
          obtain ⟨left, s7, h11, h12⟩ := bind_ok h10
          obtain ⟨hl, e7⟩ := getEnv_ok h11
          obtain ⟨_, s8, h13, h14⟩ := bind_ok h12
          have e8 := setEnv_ok h13
          obtain ⟨⟨e', eb, ef⟩, s9, he', h15⟩ := bind_ok h14
          obtain ⟨oke, ne⟩ := shape_ok he he'
          obtain ⟨right, s10, h16, h17⟩ := bind_ok h15
          obtain ⟨hr, e10⟩ := getEnv_ok h16
          obtain ⟨_, s11, h18, h19⟩ := bind_ok h17
          have n11 := pres_expectTy _ _ _ _ h18
          obtain ⟨_, s12, h20, h21⟩ := bind_ok h19
          have n12 := pres_expectTy _ _ _ _ h20
          have base : names env = names s.env := by rw [he4, n3, nc]
          have nL : names left = names s.env := by
            rw [hl, nt, e5, names_refine, base]
          have nR : names right = names s.env := by
            rw [hr, ne, e8, names_refine, base]
          have n13 : names s1.env = names s.env := by
            split at h21
            · obtain ⟨_, s', hs', hp⟩ := bind_ok h21
              rw [show names s1.env = names s'.env from pure_ok hp, setEnv_ok hs', nR]
            · split at h21
              · obtain ⟨_, s', hs', hp⟩ := bind_ok h21
                rw [show names s1.env = names s'.env from pure_ok hp, setEnv_ok hs', nL]
              · obtain ⟨m, s', hm, h24⟩ := bind_ok h21
                obtain ⟨_, s'', hs'', hp⟩ := bind_ok h24
                rw [show names s1.env = names s''.env from pure_ok hp, setEnv_ok hs'']
                simp only [liftE] at hm
                cases hme : mergeEnv left right with
                | error d => rw [hme] at hm; cases hm
                | ok m' =>
                    rw [hme] at hm; cases hm
                    rw [mergeEnv_names hme (by rw [nL, nR]), nL]
          rw [e5, names_refine, base] at okt
          rw [e8, names_refine, base] at oke
          exact ⟨.ifz oa okt oke, n13⟩

/-- Soundness on the overlap: whenever the reference checker accepts a term of
the overlap's shape (under any environment, effect context and expected type),
its translation is well typed in Appendix A's model at `u64` (`.nat`), as a
copy value, with only the `abort` effect (the model's `Fail[ArithOverflow]`). -/
theorem reference_checker_sound (Sig : Nat → Vesper.MetatheoryFull.EffSig) {cx : Cx} {fuel : Nat}
    {e : Expr} {exp : Option Ty} {s : St} {r : Out × St}
    (hs : Shape cx.prog e) (h : checkExpr fuel cx e exp s = Except.ok r) :
    HasType Sig (ctxOf (names s.env)) RCtx.empty (tr (names s.env) e) .nat .copy [.abort] :=
  ok_typed Sig (shape_ok hs h).1

/-- The closed case: a term the reference accepts in an empty environment. -/
theorem reference_checker_sound_closed (Sig : Nat → Vesper.MetatheoryFull.EffSig) {cx : Cx}
    {fuel : Nat} {e : Expr} {exp : Option Ty} {s : St} {r : Out × St} (hempty : s.env = [])
    (hs : Shape cx.prog e) (h : checkExpr fuel cx e exp s = Except.ok r) :
    HasType Sig (ctxOf []) RCtx.empty (tr [] e) .nat .copy [.abort] := by
  have := reference_checker_sound Sig hs h
  rw [hempty] at this
  exact this

end Vesper.ReferenceEval.Soundness

#print axioms Vesper.ReferenceEval.Soundness.ok_typed
#print axioms Vesper.ReferenceEval.Soundness.shape_ok
#print axioms Vesper.ReferenceEval.Soundness.reference_checker_sound
#print axioms Vesper.ReferenceEval.Soundness.reference_checker_sound_closed
