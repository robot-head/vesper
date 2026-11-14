/-!
# Vesper fusion rules: one refinement lemma per egglog rule (design §28, §32, §7 Stage 4)

Model identifier: `vesper.fusion.rules-model/1`.

`crates/vesper-fusion` saturates Tensor graphs and Resonance stereo chains
with named egglog rewrites. Each rewrite `lhs => rhs` gets a lemma here saying
that, in a model of the operators, `rhs` denotes the same value as `lhs`
(`⟦rhs⟧ = ⟦lhs⟧`). What is proved is about these models, not a proof about the
Rust implementation, about egglog's saturation or extraction, or about the
`f32` code paths; the crate's tests compare the fused and unfused Rust
evaluators numerically on random inputs.

Two kinds of model:

* **Arbitrary carrier** (`section Generic`). Tensors are lane functions
  `Nat → α` over any type `α` with arbitrary `add`, `mul`, activation and dot
  operations; DSP sections are arbitrary state machines `tick : C → σ → α → σ × α`.
  No algebraic law is assumed, so these lemmas say the fused form performs the
  same operations on the same operands per lane or per sample. They therefore
  also describe IEEE evaluation, which is why the Rust tests demand
  bit-identical results for these rules: `ew-act`, `ew-add`, `ew-mul`,
  `ew-scale`, `ew-inline`, `matvec-epilogue`, `dequant-into-matmul`,
  `gain-stereo`, `pan-stereo`, `biquad-cascade`, `cascade-merge`,
  `cascade-append-nil`, `cascade-append-cons`. The central one is
  `cascade_fused_eq_seq`: running every section sample by sample in one loop
  equals running each section over the whole block in turn.
* **Exact rationals** (`section Exact`). The rules that reassociate are proved
  over `Rat` only: `rmsnorm-prologue` (the inverse RMS factors out of a row dot
  product), `stereo-compose` (gain products), `mix-sends` (gain sums), and
  `gain-into-biquad` / `gain-before-biquad` (a gain commutes with a
  transposed-direct-form-II biquad started from zero state and folds into its
  feed-forward coefficients). In `f32` these hold only within the tolerances
  stated in the crate.

Dequantization is modelled by its entry function `scale * code`; the lemma says
that a consumer which dequantizes in place reads the same entries as one that
reads a materialized dequantized matrix. The e-graph's congruence closure is
egglog's: the semantics here are compositional, so an equal-denotation
rewrite at a subterm preserves the whole term's denotation, but no lemma here
quantifies over rewrite sequences.
-/

namespace Vesper.FusionRules

def modelId : String := "vesper.fusion.rules-model/1"
def modelClaim : String :=
  "Fusion rule refinement over operator models; not a proof about the Rust implementation, egglog or f32"

section Generic

/-! ## Tensor terms over an arbitrary carrier -/

inductive Act where
  | relu
  | silu

/-- Matrix operands: dense, materialized dequantization, in-place dequantization. -/
inductive Mat where
  | dense (name : String)
  | deq (name : String)
  | qview (name : String)

mutual
/-- Tensor terms (`T` in the egglog rule set). -/
inductive T where
  | input (name : String)
  | matvec (m : Mat) (x : T)
  | act (a : Act) (x : T)
  | add (x y : T)
  | mul (x y : T)
  | scale (p : Nat) (x : T)
  | fuse (e : E)
/-- Lane expressions inside a fused kernel (`E`). -/
inductive E where
  | lane (t : T)
  | dot (m : Mat) (x : T)
  | eact (a : Act) (e : E)
  | eadd (x y : E)
  | emul (x y : E)
  | escale (p : Nat) (e : E)
end

/-- Operator model: arbitrary operations, no laws. -/
structure Ops (α : Type) where
  add : α → α → α
  mul : α → α → α
  act : Act → α → α
  zero : α

/-- Environment: named inputs, dense entries, quantized scale and code entries,
row lengths and scalar parameters. -/
structure Env (α : Type) where
  input : String → Nat → α
  dense : String → Nat → Nat → α
  qscale : String → Nat → Nat → α
  qcode : String → Nat → Nat → α
  cols : String → Nat
  param : Nat → α

variable {α : Type}

/-- Ascending-column accumulation `((0 + r0 x0) + r1 x1) + …`, the order the
Rust kernels use. -/
def dot (o : Ops α) : Nat → (Nat → α) → (Nat → α) → α
  | 0, _, _ => o.zero
  | n + 1, r, x => o.add (dot o n r x) (o.mul (r n) (x n))

/-- The materialized dequantized matrix: every entry written once. -/
def materialize (o : Ops α) (env : Env α) (q : String) : Nat → Nat → α :=
  fun r c => o.mul (env.qscale q r c) (env.qcode q r c)

/-- Entry `(r, c)` as the consuming kernel reads it. -/
def entry (o : Ops α) (env : Env α) : Mat → Nat → Nat → α
  | .dense n => env.dense n
  | .deq q => materialize o env q
  | .qview q => fun r c => o.mul (env.qscale q r c) (env.qcode q r c)

def width (env : Env α) : Mat → Nat
  | .dense n | .deq n | .qview n => env.cols n

mutual
def evalT (o : Ops α) (env : Env α) : T → Nat → α
  | .input n, i => env.input n i
  | .matvec m x, i => dot o (width env m) (entry o env m i) (evalT o env x)
  | .act a x, i => o.act a (evalT o env x i)
  | .add x y, i => o.add (evalT o env x i) (evalT o env y i)
  | .mul x y, i => o.mul (evalT o env x i) (evalT o env y i)
  | .scale p x, i => o.mul (env.param p) (evalT o env x i)
  | .fuse e, i => evalE o env e i
def evalE (o : Ops α) (env : Env α) : E → Nat → α
  | .lane t, i => evalT o env t i
  | .dot m x, i => dot o (width env m) (entry o env m i) (evalT o env x)
  | .eact a e, i => o.act a (evalE o env e i)
  | .eadd x y, i => o.add (evalE o env x i) (evalE o env y i)
  | .emul x y, i => o.mul (evalE o env x i) (evalE o env y i)
  | .escale p e, i => o.mul (env.param p) (evalE o env e i)
end

variable (o : Ops α) (env : Env α)

/-- `ew-act`: `(Act a x) => (Fuse (EAct a (Lane x)))`. -/
theorem ew_act_refines (a : Act) (x : T) :
    evalT o env (.fuse (.eact a (.lane x))) = evalT o env (.act a x) := by
  funext i; simp [evalT, evalE]

/-- `ew-add`: `(Add x y) => (Fuse (EAdd (Lane x) (Lane y)))`. -/
theorem ew_add_refines (x y : T) :
    evalT o env (.fuse (.eadd (.lane x) (.lane y))) = evalT o env (.add x y) := by
  funext i; simp [evalT, evalE]

/-- `ew-mul`: `(Mul x y) => (Fuse (EMul (Lane x) (Lane y)))`. -/
theorem ew_mul_refines (x y : T) :
    evalT o env (.fuse (.emul (.lane x) (.lane y))) = evalT o env (.mul x y) := by
  funext i; simp [evalT, evalE]

/-- `ew-scale`: `(Scale c x) => (Fuse (EScale c (Lane x)))`. -/
theorem ew_scale_refines (p : Nat) (x : T) :
    evalT o env (.fuse (.escale p (.lane x))) = evalT o env (.scale p x) := by
  funext i; simp [evalT, evalE]

/-- `ew-inline`: `(Lane (Fuse e)) => e`. -/
theorem ew_inline_refines (e : E) :
    evalE o env e = evalE o env (.lane (.fuse e)) := by
  funext i; simp [evalT, evalE]

/-- `matvec-epilogue`: `(Lane (MatVec m x)) => (Dot m x)`. -/
theorem matvec_epilogue_refines (m : Mat) (x : T) :
    evalE o env (.dot m x) = evalE o env (.lane (.matvec m x)) := by
  funext i; simp [evalT, evalE]

/-- `dequant-into-matmul`: `(Deq q) => (QView q)`, in both matrix positions. -/
theorem dequant_into_matmul_refines (q : String) (x : T) :
    evalT o env (.matvec (.qview q) x) = evalT o env (.matvec (.deq q) x) ∧
    evalE o env (.dot (.qview q) x) = evalE o env (.dot (.deq q) x) := by
  constructor <;> (funext i; simp only [evalT, evalE, entry, width]; rfl)

/-! ## Stereo gain stages over an arbitrary carrier (one channel each) -/

/-- Stereo signal model: left and right lanes per sample. -/
structure St (α : Type) where
  l : Nat → α
  r : Nat → α

def gainStage (mul : α → α → α) (gl gr : α) (s : St α) : St α :=
  ⟨fun t => mul gl (s.l t), fun t => mul gr (s.r t)⟩

/-- `gain-stereo`: `(Gain g s) => (Stereo (GP g) (GP g) s)`; the unfused gain
pass multiplies both channels by the same `g`. -/
theorem gain_stereo_refines (mul : α → α → α) (g : α) (s : St α) :
    gainStage mul g g s = ⟨fun t => mul g (s.l t), fun t => mul g (s.r t)⟩ := rfl

/-- `pan-stereo`: `(Pan p s) => (Stereo (PanL p) (PanR p) s)`. -/
theorem pan_stereo_refines (mul : α → α → α) (panL panR : α) (s : St α) :
    gainStage mul panL panR s = ⟨fun t => mul panL (s.l t), fun t => mul panR (s.r t)⟩ := rfl

/-! ## Biquad cascades over arbitrary sections -/

variable {σ C : Type} (tick : C → σ → α → σ × α)

/-- One section over a whole block (one unfused pass). -/
def runSec : C × σ → List α → List α
  | _, [] => []
  | (c, s), x :: xs => (tick c s x).2 :: runSec (c, (tick c s x).1) xs

/-- One sample through every section, in order. -/
def stepAll : List (C × σ) → α → List (C × σ) × α
  | [], x => ([], x)
  | (c, s) :: rest, x =>
      ((c, (tick c s x).1) :: (stepAll rest (tick c s x).2).1, (stepAll rest (tick c s x).2).2)

/-- The fused cascade: one loop, each sample through all sections. -/
def runFused : List (C × σ) → List α → List α
  | _, [] => []
  | secs, x :: xs => (stepAll tick secs x).2 :: runFused (stepAll tick secs x).1 xs

/-- The unfused cascade: each section over the whole block in turn. -/
def runSeq : List (C × σ) → List α → List α
  | [], xs => xs
  | sec :: rest, xs => runSeq rest (runSec tick sec xs)

theorem runFused_nil (xs : List α) : runFused tick [] xs = xs := by
  induction xs with
  | nil => rfl
  | cons x xs ih => simp [runFused, stepAll, ih]

theorem runFused_cons (c : C) (rest : List (C × σ)) (s : σ) (xs : List α) :
    runFused tick ((c, s) :: rest) xs = runFused tick rest (runSec tick (c, s) xs) := by
  induction xs generalizing s rest with
  | nil => simp [runFused, runSec]
  | cons x xs ih => simp [runFused, runSec, stepAll, ih]

/-- Loop interchange: the fused per-sample cascade equals the sequence of
whole-block section passes, for any section semantics. -/
theorem cascade_fused_eq_seq (secs : List (C × σ)) (xs : List α) :
    runFused tick secs xs = runSeq tick secs xs := by
  induction secs generalizing xs with
  | nil => simp [runSeq, runFused_nil]
  | cons sec rest ih =>
      obtain ⟨c, s⟩ := sec
      simp [runSeq, runFused_cons, ih]

theorem runSeq_append (q1 q2 : List (C × σ)) (xs : List α) :
    runSeq tick (q1 ++ q2) xs = runSeq tick q2 (runSeq tick q1 xs) := by
  induction q1 generalizing xs with
  | nil => rfl
  | cons sec rest ih => simp [runSeq, ih]

/-- `biquad-cascade`: `(Biquad c s) => (Cascade (SCons (Coef c) (SNil)) s)`. -/
theorem biquad_cascade_refines (sec : C × σ) (xs : List α) :
    runFused tick [sec] xs = runSec tick sec xs := by
  simp [cascade_fused_eq_seq, runSeq]

/-- `cascade-merge`: `(Cascade q2 (Cascade q1 s)) => (Cascade (QApp q1 q2) s)`. -/
theorem cascade_merge_refines (q1 q2 : List (C × σ)) (xs : List α) :
    runFused tick (q1 ++ q2) xs = runFused tick q2 (runFused tick q1 xs) := by
  simp [cascade_fused_eq_seq, runSeq_append]

/-- `cascade-append-nil`: `(QApp (SNil) q) => q`. -/
theorem cascade_append_nil_refines (q : List (C × σ)) : [] ++ q = q := rfl

/-- `cascade-append-cons`: `(QApp (SCons c r) q) => (SCons c (QApp r q))`. -/
theorem cascade_append_cons_refines (c : C × σ) (r q : List (C × σ)) :
    (c :: r) ++ q = c :: (r ++ q) := rfl

end Generic

section Exact

/-! ## Reassociating rules over exact rationals -/

/-- `Σ_{c<n} f c * g c`, accumulated in ascending order. -/
def dotR : Nat → (Nat → Rat) → (Nat → Rat) → Rat
  | 0, _, _ => 0
  | n + 1, f, g => dotR n f g + f n * g n

/-- `rmsnorm-prologue`: `(MatVec m (RmsNorm x w e)) => (RmsMatVec m x w e)`.
Unfused, each normalized entry is `w c * (inv * x c)` (the order of
`vesper_tensor::ops::rms_norm`); fused, `inv` multiplies the row dot product
of `W` with `w ⊙ x`. -/
theorem rmsnorm_prologue_refines (n : Nat) (W w x : Nat → Rat) (inv : Rat) :
    inv * dotR n W (fun c => w c * x c) = dotR n W (fun c => w c * (inv * x c)) := by
  induction n with
  | zero => simp [dotR]
  | succ n ih =>
      simp only [dotR, ← ih]
      grind

/-- `stereo-compose`: `(Stereo a b (Stereo c d s)) => (Stereo (GMul a c) (GMul b d) s)`,
per channel. -/
theorem stereo_compose_refines (a c x : Rat) : (a * c) * x = a * (c * x) := by
  grind

/-- `mix-sends`: `(Mix (Stereo a b s) (Stereo c d s)) => (Stereo (GAdd a c) (GAdd b d) s)`,
per channel. -/
theorem mix_sends_refines (a c x : Rat) : (a + c) * x = a * x + c * x := by
  grind

/-- Normalized biquad coefficients. -/
structure Coef where
  b0 : Rat
  b1 : Rat
  b2 : Rat
  a1 : Rat
  a2 : Rat

/-- Transposed direct form II, as in `vesper_fusion::dsp`:
`y = b0 x + s1; s1' = b1 x - a1 y + s2; s2' = b2 x - a2 y`. -/
def biquad (k : Coef) (s : Rat × Rat) (x : Rat) : (Rat × Rat) × Rat :=
  let y := k.b0 * x + s.1
  ((k.b1 * x - k.a1 * y + s.2, k.b2 * x - k.a2 * y), y)

/-- Feed-forward coefficients scaled by `g` (the `CScale` node). -/
def scaleB (g : Rat) (k : Coef) : Coef := ⟨g * k.b0, g * k.b1, g * k.b2, k.a1, k.a2⟩

def gainL (g : Rat) (xs : List Rat) : List Rat := xs.map (g * ·)

theorem biquad_scaled_input (k : Coef) (g s1 s2 : Rat) (xs : List Rat) :
    runSec biquad (k, (g * s1, g * s2)) (gainL g xs) = gainL g (runSec biquad (k, (s1, s2)) xs) := by
  induction xs generalizing s1 s2 with
  | nil => rfl
  | cons x xs ih =>
      simp only [gainL, List.map, runSec, biquad] at *
      rw [List.cons.injEq]
      refine ⟨?_, ?_⟩
      · grind
      · have h2 : k.b2 * (g * x) - k.a2 * (k.b0 * (g * x) + g * s1)
            = g * (k.b2 * x - k.a2 * (k.b0 * x + s1)) := by grind
        have h1' : k.b1 * (g * x) - k.a1 * (k.b0 * (g * x) + g * s1) + g * s2
            = g * (k.b1 * x - k.a1 * (k.b0 * x + s1) + s2) := by grind
        rw [h1', h2]
        exact ih _ _

theorem biquad_scaled_coef (k : Coef) (g s1 s2 : Rat) (xs : List Rat) :
    runSec biquad (scaleB g k, (g * s1, g * s2)) xs = gainL g (runSec biquad (k, (s1, s2)) xs) := by
  induction xs generalizing s1 s2 with
  | nil => rfl
  | cons x xs ih =>
      simp only [gainL, List.map, runSec, biquad, scaleB] at *
      rw [List.cons.injEq]
      refine ⟨?_, ?_⟩
      · grind
      · have h1 : g * k.b1 * x - k.a1 * (g * k.b0 * x + g * s1) + g * s2
            = g * (k.b1 * x - k.a1 * (k.b0 * x + s1) + s2) := by grind
        have h2 : g * k.b2 * x - k.a2 * (g * k.b0 * x + g * s1)
            = g * (k.b2 * x - k.a2 * (k.b0 * x + s1)) := by grind
        rw [h1, h2]
        exact ih _ _

/-- A cascade whose sections all start from zero state. -/
def zeroed (ks : List Coef) : List (Coef × (Rat × Rat)) := ks.map (·, (0, 0))

theorem biquad_zero_input (k : Coef) (g : Rat) (xs : List Rat) :
    runSec biquad (k, (0, 0)) (gainL g xs) = gainL g (runSec biquad (k, (0, 0)) xs) := by
  have h := biquad_scaled_input k g 0 0 xs
  simpa using h

theorem biquad_zero_coef (k : Coef) (g : Rat) (xs : List Rat) :
    runSec biquad (scaleB g k, (0, 0)) xs = gainL g (runSec biquad (k, (0, 0)) xs) := by
  have h := biquad_scaled_coef k g 0 0 xs
  simpa using h

theorem gain_commutes_cascade (g : Rat) (ks : List Coef) (xs : List Rat) :
    runSeq biquad (zeroed ks) (gainL g xs) = gainL g (runSeq biquad (zeroed ks) xs) := by
  induction ks generalizing xs with
  | nil => rfl
  | cons k ks ih =>
      show runSeq biquad (zeroed ks) (runSec biquad (k, (0, 0)) (gainL g xs))
        = gainL g (runSeq biquad (zeroed ks) (runSec biquad (k, (0, 0)) xs))
      rw [biquad_zero_input, ih]

/-- `gain-before-biquad`:
`(Cascade (SCons c r) (Stereo g g s)) => (Cascade (SCons (CScale g c) r) s)`,
from zero state, per channel. -/
theorem gain_before_biquad_refines (g : Rat) (k : Coef) (ks : List Coef) (xs : List Rat) :
    runFused biquad (zeroed (scaleB g k :: ks)) xs = runFused biquad (zeroed (k :: ks)) (gainL g xs) := by
  rw [cascade_fused_eq_seq, cascade_fused_eq_seq]
  show runSeq biquad (zeroed ks) (runSec biquad (scaleB g k, (0, 0)) xs)
    = runSeq biquad (zeroed ks) (runSec biquad (k, (0, 0)) (gainL g xs))
  rw [biquad_zero_input, biquad_zero_coef]

/-- `gain-into-biquad`:
`(Stereo g g (Cascade (SCons c r) s)) => (Cascade (SCons (CScale g c) r) s)`,
from zero state, per channel. -/
theorem gain_into_biquad_refines (g : Rat) (k : Coef) (ks : List Coef) (xs : List Rat) :
    runFused biquad (zeroed (scaleB g k :: ks)) xs = gainL g (runFused biquad (zeroed (k :: ks)) xs) := by
  rw [cascade_fused_eq_seq, cascade_fused_eq_seq]
  show runSeq biquad (zeroed ks) (runSec biquad (scaleB g k, (0, 0)) xs)
    = gainL g (runSeq biquad (zeroed ks) (runSec biquad (k, (0, 0)) xs))
  rw [biquad_zero_coef, gain_commutes_cascade]

end Exact

end Vesper.FusionRules

#print axioms Vesper.FusionRules.ew_act_refines
#print axioms Vesper.FusionRules.ew_add_refines
#print axioms Vesper.FusionRules.ew_mul_refines
#print axioms Vesper.FusionRules.ew_scale_refines
#print axioms Vesper.FusionRules.ew_inline_refines
#print axioms Vesper.FusionRules.matvec_epilogue_refines
#print axioms Vesper.FusionRules.dequant_into_matmul_refines
#print axioms Vesper.FusionRules.gain_stereo_refines
#print axioms Vesper.FusionRules.pan_stereo_refines
#print axioms Vesper.FusionRules.cascade_fused_eq_seq
#print axioms Vesper.FusionRules.biquad_cascade_refines
#print axioms Vesper.FusionRules.cascade_merge_refines
#print axioms Vesper.FusionRules.cascade_append_nil_refines
#print axioms Vesper.FusionRules.cascade_append_cons_refines
#print axioms Vesper.FusionRules.rmsnorm_prologue_refines
#print axioms Vesper.FusionRules.stereo_compose_refines
#print axioms Vesper.FusionRules.mix_sends_refines
#print axioms Vesper.FusionRules.gain_before_biquad_refines
#print axioms Vesper.FusionRules.gain_into_biquad_refines
