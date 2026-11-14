/-
Capability effects (design §23.2): a model of the ConstTime taint rules of
`src/capability_effects.rs` and a non-interference theorem about the model.

The model is a small first-order expression language with the constructs the
checker's taint rules distinguish:

* `bin`: a total, constant-time operation (the wrapping and bitwise
  intrinsics, and the built-in secret operations built from them);
* `sel`: a constant-time select (`ct_select`): every operand is evaluated;
* `branch`: a branch, whose decision is observable;
* `obs`: a variable-latency operation (division, remainder, indexing), whose
  operand is observable;
* `call body a b`: a direct call to a function with two parameters. The
  callee is checked with parameter labels taken from its callers
  (interprocedural secret-parameter inference);
* `perf` and `handle r body`: performing an effect operation returns the
  value the handler resumes with (`r`), tracked by taint;
* `decl`: an explicit declassification gate.

Observations are a trace of natural numbers: branch decisions and the
operands of variable-latency operations. The main theorem says that a
well-labelled, declassify-free expression produces the same trace for any two
environments that agree on public variables (constant time), and the same
value when its result is public.

This is a theorem about this model only. It is not a theorem about the Rust
checker, the evaluator, the native backend or the machine code.
-/
namespace CapabilityEffects

inductive Expr where
  | lit : Nat → Expr
  | var : Nat → Expr
  | bin : Expr → Expr → Expr
  | sel : Expr → Expr → Expr → Expr
  | branch : Expr → Expr → Expr → Expr
  | obs : Expr → Expr
  | call : Expr → Expr → Expr → Expr
  | perf : Expr
  | handle : Expr → Expr → Expr
  | decl : Expr → Expr
  deriving Repr

open Expr

/-- The callee's environment: parameters 0 and 1. -/
def env2 {α : Type} (x y d : α) : Nat → α :=
  fun i => if i = 0 then x else if i = 1 then y else d

/-- Big-step evaluation: the value and the observation trace. `op` is any
total binary operation; `k` is the value the innermost handler resumes
with. -/
def eval (op : Nat → Nat → Nat) : (Nat → Nat) → Nat → Expr → Nat × List Nat
  | _, _, lit n => (n, [])
  | ρ, _, var x => (ρ x, [])
  | ρ, k, bin a b =>
      (op (eval op ρ k a).1 (eval op ρ k b).1, (eval op ρ k a).2 ++ (eval op ρ k b).2)
  | ρ, k, sel m a b =>
      (if (eval op ρ k m).1 ≠ 0 then (eval op ρ k a).1 else (eval op ρ k b).1,
        (eval op ρ k m).2 ++ (eval op ρ k a).2 ++ (eval op ρ k b).2)
  | ρ, k, branch c a b =>
      if (eval op ρ k c).1 ≠ 0 then
        ((eval op ρ k a).1, (eval op ρ k c).2 ++ [1] ++ (eval op ρ k a).2)
      else
        ((eval op ρ k b).1, (eval op ρ k c).2 ++ [0] ++ (eval op ρ k b).2)
  | ρ, k, obs a => ((eval op ρ k a).1, (eval op ρ k a).2 ++ [(eval op ρ k a).1])
  | ρ, k, call body a b =>
      ((eval op (env2 (eval op ρ k a).1 (eval op ρ k b).1 0) k body).1,
        (eval op ρ k a).2 ++ (eval op ρ k b).2
          ++ (eval op (env2 (eval op ρ k a).1 (eval op ρ k b).1 0) k body).2)
  | _, k, perf => (k, [])
  | ρ, k, handle r body =>
      ((eval op ρ (eval op ρ k r).1 body).1, (eval op ρ k r).2 ++ (eval op ρ (eval op ρ k r).1 body).2)
  | ρ, k, decl a => eval op ρ k a

/-- Taint: whether the value is secret-derived, given the labels `Γ` of the
variables and the label `κ` of the resumed value (`true` is secret). A call's
result is the callee body's taint under the labels of the arguments; a
perform's is the resumed value's; a gate's result is public. -/
def taint : (Nat → Bool) → Bool → Expr → Bool
  | _, _, lit _ => false
  | Γ, _, var x => Γ x
  | Γ, κ, bin a b => taint Γ κ a || taint Γ κ b
  | Γ, κ, sel m a b => taint Γ κ m || taint Γ κ a || taint Γ κ b
  | Γ, κ, branch c a b => taint Γ κ c || taint Γ κ a || taint Γ κ b
  | Γ, κ, obs a => taint Γ κ a
  | Γ, κ, call body a b => taint (env2 (taint Γ κ a) (taint Γ κ b) false) κ body
  | _, κ, perf => κ
  | Γ, κ, handle r body => taint Γ (taint Γ κ r) body
  | _, _, decl _ => false

/-- The ConstTime rules: no branch on a secret (E0702), no variable-latency
operation on a secret (E0703, E0704); a callee is checked under its callers'
labels and a handled body under the resumed value's label. -/
def ok : (Nat → Bool) → Bool → Expr → Prop
  | _, _, lit _ => True
  | _, _, var _ => True
  | Γ, κ, bin a b => ok Γ κ a ∧ ok Γ κ b
  | Γ, κ, sel m a b => ok Γ κ m ∧ ok Γ κ a ∧ ok Γ κ b
  | Γ, κ, branch c a b => taint Γ κ c = false ∧ ok Γ κ c ∧ ok Γ κ a ∧ ok Γ κ b
  | Γ, κ, obs a => taint Γ κ a = false ∧ ok Γ κ a
  | Γ, κ, call body a b =>
      ok Γ κ a ∧ ok Γ κ b ∧ ok (env2 (taint Γ κ a) (taint Γ κ b) false) κ body
  | _, _, perf => True
  | Γ, κ, handle r body => ok Γ κ r ∧ ok Γ (taint Γ κ r) body
  | Γ, κ, decl a => ok Γ κ a

/-- No declassification gate anywhere. -/
def noDecl : Expr → Prop
  | lit _ => True
  | var _ => True
  | bin a b => noDecl a ∧ noDecl b
  | sel m a b => noDecl m ∧ noDecl a ∧ noDecl b
  | branch c a b => noDecl c ∧ noDecl a ∧ noDecl b
  | obs a => noDecl a
  | call body a b => noDecl body ∧ noDecl a ∧ noDecl b
  | perf => True
  | handle r body => noDecl r ∧ noDecl body
  | decl _ => False

/-- Environments that agree on every public variable. -/
def LowEq (Γ : Nat → Bool) (ρ₁ ρ₂ : Nat → Nat) : Prop :=
  ∀ x, Γ x = false → ρ₁ x = ρ₂ x

theorem lowEq_env2 {Γa Γb : Bool} {x₁ x₂ y₁ y₂ : Nat}
    (hx : Γa = false → x₁ = x₂) (hy : Γb = false → y₁ = y₂) :
    LowEq (env2 Γa Γb false) (env2 x₁ y₁ 0) (env2 x₂ y₂ 0) := by
  intro i hi
  unfold env2 at hi ⊢
  by_cases h0 : i = 0
  · simp [h0] at hi ⊢; exact hx hi
  · by_cases h1 : i = 1
    · simp [h1] at hi ⊢; exact hy hi
    · simp [h0, h1]

/-- Non-interference for the model (constant time and public results): two
runs of a well-labelled, declassify-free expression whose inputs agree on the
public variables, and on the resumed value when it is public, observe the
same trace, and compute the same value when the result is public. -/
theorem noninterference (op : Nat → Nat → Nat) :
    ∀ (e : Expr) (Γ : Nat → Bool) (κ : Bool) (ρ₁ ρ₂ : Nat → Nat) (k₁ k₂ : Nat),
      ok Γ κ e → noDecl e → LowEq Γ ρ₁ ρ₂ → (κ = false → k₁ = k₂) →
      (eval op ρ₁ k₁ e).2 = (eval op ρ₂ k₂ e).2 ∧
        (taint Γ κ e = false → (eval op ρ₁ k₁ e).1 = (eval op ρ₂ k₂ e).1) := by
  intro e
  induction e with
  | lit n =>
      intro Γ κ ρ₁ ρ₂ k₁ k₂ _ _ _ _
      simp [eval]
  | var x =>
      intro Γ κ ρ₁ ρ₂ k₁ k₂ _ _ hρ _
      refine ⟨by simp [eval], ?_⟩
      intro ht
      simp only [taint] at ht
      simp [eval, hρ x ht]
  | bin a b iha ihb =>
      intro Γ κ ρ₁ ρ₂ k₁ k₂ hok hnd hρ hk
      simp only [ok] at hok
      simp only [noDecl] at hnd
      have ha := iha Γ κ ρ₁ ρ₂ k₁ k₂ hok.1 hnd.1 hρ hk
      have hb := ihb Γ κ ρ₁ ρ₂ k₁ k₂ hok.2 hnd.2 hρ hk
      refine ⟨by simp [eval, ha.1, hb.1], ?_⟩
      intro ht
      simp only [taint, Bool.or_eq_false_iff] at ht
      simp [eval, ha.2 ht.1, hb.2 ht.2]
  | sel m a b ihm iha ihb =>
      intro Γ κ ρ₁ ρ₂ k₁ k₂ hok hnd hρ hk
      simp only [ok] at hok
      simp only [noDecl] at hnd
      have hm := ihm Γ κ ρ₁ ρ₂ k₁ k₂ hok.1 hnd.1 hρ hk
      have ha := iha Γ κ ρ₁ ρ₂ k₁ k₂ hok.2.1 hnd.2.1 hρ hk
      have hb := ihb Γ κ ρ₁ ρ₂ k₁ k₂ hok.2.2 hnd.2.2 hρ hk
      refine ⟨by simp [eval, hm.1, ha.1, hb.1], ?_⟩
      intro ht
      simp only [taint, Bool.or_eq_false_iff] at ht
      simp [eval, hm.2 ht.1.1, ha.2 ht.1.2, hb.2 ht.2]
  | branch c a b ihc iha ihb =>
      intro Γ κ ρ₁ ρ₂ k₁ k₂ hok hnd hρ hk
      simp only [ok] at hok
      simp only [noDecl] at hnd
      have hc := ihc Γ κ ρ₁ ρ₂ k₁ k₂ hok.2.1 hnd.1 hρ hk
      have ha := iha Γ κ ρ₁ ρ₂ k₁ k₂ hok.2.2.1 hnd.2.1 hρ hk
      have hb := ihb Γ κ ρ₁ ρ₂ k₁ k₂ hok.2.2.2 hnd.2.2 hρ hk
      -- The condition is public, so both runs take the same branch.
      have hcv := hc.2 hok.1
      by_cases h : (eval op ρ₁ k₁ c).1 ≠ 0
      · have h' : (eval op ρ₂ k₂ c).1 ≠ 0 := hcv ▸ h
        refine ⟨by simp [eval, h, h', hc.1, ha.1], ?_⟩
        intro ht
        simp only [taint, Bool.or_eq_false_iff] at ht
        simp [eval, h, h', ha.2 ht.1.2]
      · have h' : ¬ (eval op ρ₂ k₂ c).1 ≠ 0 := hcv ▸ h
        refine ⟨by simp [eval, h, h', hc.1, hb.1], ?_⟩
        intro ht
        simp only [taint, Bool.or_eq_false_iff] at ht
        simp [eval, h, h', hb.2 ht.2]
  | obs a iha =>
      intro Γ κ ρ₁ ρ₂ k₁ k₂ hok hnd hρ hk
      simp only [ok] at hok
      simp only [noDecl] at hnd
      have ha := iha Γ κ ρ₁ ρ₂ k₁ k₂ hok.2 hnd hρ hk
      -- The observed operand is public, so it is the same in both runs.
      have hv := ha.2 hok.1
      refine ⟨by simp [eval, ha.1, hv], ?_⟩
      intro _
      simp [eval, hv]
  | call body a b ihbody iha ihb =>
      intro Γ κ ρ₁ ρ₂ k₁ k₂ hok hnd hρ hk
      simp only [ok] at hok
      simp only [noDecl] at hnd
      have ha := iha Γ κ ρ₁ ρ₂ k₁ k₂ hok.1 hnd.2.1 hρ hk
      have hb := ihb Γ κ ρ₁ ρ₂ k₁ k₂ hok.2.1 hnd.2.2 hρ hk
      -- The callee's environments agree on the parameters its callers
      -- pass public values to.
      have henv := lowEq_env2 (Γa := taint Γ κ a) (Γb := taint Γ κ b)
        (x₁ := (eval op ρ₁ k₁ a).1) (x₂ := (eval op ρ₂ k₂ a).1)
        (y₁ := (eval op ρ₁ k₁ b).1) (y₂ := (eval op ρ₂ k₂ b).1) ha.2 hb.2
      have hbody := ihbody _ κ _ _ k₁ k₂ hok.2.2 hnd.1 henv hk
      refine ⟨by simp [eval, ha.1, hb.1, hbody.1], ?_⟩
      intro ht
      simp only [taint] at ht
      simp [eval, hbody.2 ht]
  | perf =>
      intro Γ κ ρ₁ ρ₂ k₁ k₂ _ _ _ hk
      refine ⟨by simp [eval], ?_⟩
      intro ht
      simp only [taint] at ht
      simp [eval, hk ht]
  | handle r body ihr ihbody =>
      intro Γ κ ρ₁ ρ₂ k₁ k₂ hok hnd hρ hk
      simp only [ok] at hok
      simp only [noDecl] at hnd
      have hr := ihr Γ κ ρ₁ ρ₂ k₁ k₂ hok.1 hnd.1 hρ hk
      -- The handled body resumes with the handler's value, under its label.
      have hbody := ihbody Γ (taint Γ κ r) ρ₁ ρ₂ _ _ hok.2 hnd.2 hρ hr.2
      refine ⟨by simp [eval, hr.1, hbody.1], ?_⟩
      intro ht
      simp only [taint] at ht
      simp [eval, hbody.2 ht]
  | decl a _ =>
      intro Γ κ ρ₁ ρ₂ k₁ k₂ _ hnd _ _
      simp [noDecl] at hnd

/-! ## Labels only ever grow: checking under more secrets is stricter

The checker infers one label set per function (the union over its call
sites) and checks the body once under it. Monotonicity makes that sound for
every call site: a body checked under the union is also well labelled under
each call site's own labels. -/

/-- Pointwise order on labels. -/
def LabelLe (Γ Γ' : Nat → Bool) : Prop := ∀ x, Γ x = true → Γ' x = true

theorem labelLe_env2 {a a' b b' : Bool} (ha : a = true → a' = true)
    (hb : b = true → b' = true) : LabelLe (env2 a b false) (env2 a' b' false) := by
  intro i hi
  unfold env2 at hi ⊢
  by_cases h0 : i = 0
  · simp [h0] at hi ⊢; exact ha hi
  · by_cases h1 : i = 1
    · simp [h1] at hi ⊢; exact hb hi
    · simp [h0, h1] at hi

theorem taint_mono :
    ∀ (e : Expr) (Γ Γ' : Nat → Bool) (κ κ' : Bool),
      LabelLe Γ Γ' → (κ = true → κ' = true) → taint Γ κ e = true → taint Γ' κ' e = true := by
  intro e
  induction e with
  | lit n => intro Γ Γ' κ κ' _ _ h; simp [taint] at h
  | var x => intro Γ Γ' κ κ' hΓ _ h; simp only [taint] at h ⊢; exact hΓ x h
  | bin a b iha ihb =>
      intro Γ Γ' κ κ' hΓ hκ h
      simp only [taint, Bool.or_eq_true] at h ⊢
      rcases h with h | h
      · exact Or.inl (iha Γ Γ' κ κ' hΓ hκ h)
      · exact Or.inr (ihb Γ Γ' κ κ' hΓ hκ h)
  | sel m a b ihm iha ihb =>
      intro Γ Γ' κ κ' hΓ hκ h
      simp only [taint, Bool.or_eq_true] at h ⊢
      rcases h with (h | h) | h
      · exact Or.inl (Or.inl (ihm Γ Γ' κ κ' hΓ hκ h))
      · exact Or.inl (Or.inr (iha Γ Γ' κ κ' hΓ hκ h))
      · exact Or.inr (ihb Γ Γ' κ κ' hΓ hκ h)
  | branch c a b ihc iha ihb =>
      intro Γ Γ' κ κ' hΓ hκ h
      simp only [taint, Bool.or_eq_true] at h ⊢
      rcases h with (h | h) | h
      · exact Or.inl (Or.inl (ihc Γ Γ' κ κ' hΓ hκ h))
      · exact Or.inl (Or.inr (iha Γ Γ' κ κ' hΓ hκ h))
      · exact Or.inr (ihb Γ Γ' κ κ' hΓ hκ h)
  | obs a iha => intro Γ Γ' κ κ' hΓ hκ h; simp only [taint] at h ⊢; exact iha Γ Γ' κ κ' hΓ hκ h
  | call body a b ihbody iha ihb =>
      intro Γ Γ' κ κ' hΓ hκ h
      simp only [taint] at h ⊢
      exact ihbody _ _ κ κ'
        (labelLe_env2 (iha Γ Γ' κ κ' hΓ hκ) (ihb Γ Γ' κ κ' hΓ hκ)) hκ h
  | perf => intro Γ Γ' κ κ' _ hκ h; simp only [taint] at h ⊢; exact hκ h
  | handle r body ihr ihbody =>
      intro Γ Γ' κ κ' hΓ hκ h
      simp only [taint] at h ⊢
      exact ihbody Γ Γ' _ _ hΓ (ihr Γ Γ' κ κ' hΓ hκ) h
  | decl a _ => intro Γ Γ' κ κ' _ _ h; simp [taint] at h

theorem taint_mono_false {e : Expr} {Γ Γ' : Nat → Bool} {κ κ' : Bool}
    (hΓ : LabelLe Γ Γ') (hκ : κ = true → κ' = true) (h : taint Γ' κ' e = false) :
    taint Γ κ e = false := by
  cases hv : taint Γ κ e
  · rfl
  · have := taint_mono e Γ Γ' κ κ' hΓ hκ hv
    rw [this] at h
    exact absurd h (by decide)

theorem ok_mono :
    ∀ (e : Expr) (Γ Γ' : Nat → Bool) (κ κ' : Bool),
      LabelLe Γ Γ' → (κ = true → κ' = true) → ok Γ' κ' e → ok Γ κ e := by
  intro e
  induction e with
  | lit n => intro _ _ _ _ _ _ _; simp [ok]
  | var x => intro _ _ _ _ _ _ _; simp [ok]
  | bin a b iha ihb =>
      intro Γ Γ' κ κ' hΓ hκ h
      simp only [ok] at h ⊢
      exact ⟨iha Γ Γ' κ κ' hΓ hκ h.1, ihb Γ Γ' κ κ' hΓ hκ h.2⟩
  | sel m a b ihm iha ihb =>
      intro Γ Γ' κ κ' hΓ hκ h
      simp only [ok] at h ⊢
      exact ⟨ihm Γ Γ' κ κ' hΓ hκ h.1, iha Γ Γ' κ κ' hΓ hκ h.2.1, ihb Γ Γ' κ κ' hΓ hκ h.2.2⟩
  | branch c a b ihc iha ihb =>
      intro Γ Γ' κ κ' hΓ hκ h
      simp only [ok] at h ⊢
      exact ⟨taint_mono_false hΓ hκ h.1, ihc Γ Γ' κ κ' hΓ hκ h.2.1,
        iha Γ Γ' κ κ' hΓ hκ h.2.2.1, ihb Γ Γ' κ κ' hΓ hκ h.2.2.2⟩
  | obs a iha =>
      intro Γ Γ' κ κ' hΓ hκ h
      simp only [ok] at h ⊢
      exact ⟨taint_mono_false hΓ hκ h.1, iha Γ Γ' κ κ' hΓ hκ h.2⟩
  | call body a b ihbody iha ihb =>
      intro Γ Γ' κ κ' hΓ hκ h
      simp only [ok] at h ⊢
      exact ⟨iha Γ Γ' κ κ' hΓ hκ h.1, ihb Γ Γ' κ κ' hΓ hκ h.2.1,
        ihbody _ _ κ κ'
          (labelLe_env2 (taint_mono a Γ Γ' κ κ' hΓ hκ) (taint_mono b Γ Γ' κ κ' hΓ hκ)) hκ
          h.2.2⟩
  | perf => intro _ _ _ _ _ _ _; simp [ok]
  | handle r body ihr ihbody =>
      intro Γ Γ' κ κ' hΓ hκ h
      simp only [ok] at h ⊢
      exact ⟨ihr Γ Γ' κ κ' hΓ hκ h.1, ihbody Γ Γ' _ _ hΓ (taint_mono r Γ Γ' κ κ' hΓ hκ) h.2⟩
  | decl a iha =>
      intro Γ Γ' κ κ' hΓ hκ h
      simp only [ok] at h ⊢
      exact iha Γ Γ' κ κ' hΓ hκ h

/-- Summary soundness: a callee body checked once under summary labels
`sa`, `sb` that cover this call site's argument labels is well labelled at
this call site, so `noninterference` applies to the call. -/
theorem call_ok_of_summary {Γ : Nat → Bool} {κ : Bool} {body a b : Expr} {sa sb : Bool}
    (ha : ok Γ κ a) (hb : ok Γ κ b)
    (hsa : taint Γ κ a = true → sa = true) (hsb : taint Γ κ b = true → sb = true)
    (hbody : ok (env2 sa sb false) κ body) :
    ok Γ κ (call body a b) := by
  simp only [ok]
  exact ⟨ha, hb, ok_mono body _ _ κ κ (labelLe_env2 hsa hsb) id hbody⟩

/-- A secret branch is rejected: the rules are not vacuous. -/
example : ¬ ok (fun _ => true) false (branch (var 0) (lit 1) (lit 2)) := by
  simp [ok, taint]

/-- A branch on a value resumed from a secret is rejected. -/
example : ¬ ok (fun _ => true) false (handle (var 0) (branch perf (lit 1) (lit 2))) := by
  simp [ok, taint]

/-- A callee that branches on its parameter is rejected at a call that
passes it a secret. -/
example : ¬ ok (fun _ => true) false (call (branch (var 0) (lit 1) (lit 2)) (var 7) (lit 0)) := by
  simp [ok, taint, env2]

/-- Constant-time selects over secrets are accepted. -/
example : ok (fun _ => true) false (sel (var 0) (var 1) (var 2)) := by
  simp [ok]

end CapabilityEffects
