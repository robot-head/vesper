/-!
# Vesper Resonance: SVF BIBO stability and the torque-clamp invariant (design §32.1 item 5)

Model identifier: `vesper.resonance.stability-model/1`.

Two models over exact rational arithmetic (`Rat`), each proved in full. Neither
is a theorem about Rust code or about IEEE-754 `f32` arithmetic:
`crates/vesper-resonance` evaluates the same equations in `f32` (scalar and
`std::arch` SIMD), and its tests check boundedness on concrete runs.

## 1. TPT state-variable filter (the `SvfBank` voice)

State `c = (ic1eq, ic2eq)`, input `x`, warped cutoff `g = tan(π fc / fs)`,
damping `k = 1 / Q`. One sample, with `D = 1 + g² + g k`:

```text
v1 = (ic1eq + g (x - ic2eq)) / D        -- = a1 ic1eq + a2 (x - ic2eq)
v2 = ic2eq + g v1                       -- = ic2eq + a2 ic1eq + a3 (x - ic2eq)
ic1eq' = 2 v1 - ic1eq;  ic2eq' = 2 v2 - ic2eq;  y = gain * v2
```

These are the closed form of the trapezoidal (implicit) equations
`v1 = g (x - k v1 - v2) + ic1eq`, `v2 = g v1 + ic2eq` (`svf_step_implicit`).

Stated coefficient range: damping fixed with `0 < k < 2` (that is `Q > 1/2`),
cutoff free to change every sample with `0 ≤ g n ≤ G`. Time-varying damping is
not covered (the Lyapunov function below depends on `k`).

Lyapunov function `V(c) = W k c = c1² + k c1 c2 + c2²` (positive definite for
`0 < k < 2`). What is proved:

* `svf_energy_identity`: `V(c') - V(c) = 4 g (-(k/2) W(v) + x v1 + (k/2) x v2)`,
  the discrete passivity identity of the trapezoidal rule.
* `svf_lyapunov_step`: `2 D k V(c') ≤ (2 D k - g k²) V(c) + (2 g³ k² + 8 g D) x²`,
  i.e. `V' ≤ (1 - g k / (2D)) V + (g³ k / D + 4 g / k) x²`.
* `svf_step_bounded`: if `V(c) ≤ K`, `x² ≤ M²` and `(2 g² k² + 8 D) M² ≤ k² K`,
  then `V(c') ≤ K`.
* `svf_bibo`: for every input sequence with `x n ² ≤ M²` and every cutoff
  sequence in `[0, G]`, if `V(c 0) ≤ K` and `(2 G² k² + 8 (1 + G² + G k)) M² ≤ k² K`,
  then `V(c n) ≤ K` for all `n` (bounded input, bounded state).
* `svf_state_bounded` and `svf_output_bounded`: `(2 - k) (c1² + c2²) ≤ 2 K` and
  `(2 - k) v2² ≤ 2 K`, so states and the low-pass output `v2` are bounded.

## 2. PID torque clamp (the `PidController` step)

Gains `kp ki kd dt`, torque limit `τ ≥ 0` and integrator limit `ι ≥ 0`. A sample
is `some (setpoint, measurement)` or `none` (a non-finite sample, which the
Rust controller reports as a fault). On a sample the integrator becomes
`clamp (-ι) ι (i + ki e dt)` and the command is `clamp (-τ) τ u`; on a fault
the command is safe torque `0` and the state is unchanged.

* `clamp_mem`: `lo ≤ hi → lo ≤ clamp lo hi x ≤ hi`.
* `pid_torque_clamped`: every command satisfies `-τ ≤ u ≤ τ`.
* `pid_integral_clamped`: every step leaves `-ι ≤ i ≤ ι`.
* `pid_invariant`: along any sample sequence (faults included) every state
  keeps `-ι ≤ i ≤ ι` and every command keeps `-τ ≤ u ≤ τ`.

Rational arithmetic has no NaN or overflow; the Rust controller turns those
into typed faults before committing state, which is the `none` branch here.
-/

namespace Vesper.ResonanceStability

def modelId : String := "vesper.resonance.stability-model/1"
def modelClaim : String :=
  "SVF BIBO and torque-clamp models over Rat; not a proof about the Rust implementation or f32"

/-! ## Algebraic helpers -/

theorem sq_nonneg' (a : Rat) : 0 ≤ a * a := by
  rcases Rat.le_total (a := 0) (b := a) with h | h
  · exact Rat.mul_nonneg h h
  · have hn : 0 ≤ -a := by grind
    have := Rat.mul_nonneg hn hn
    grind

theorem le_of_sub_eq {a b c : Rat} (e : b - a = c) (h : 0 ≤ c) : a ≤ b := by grind

theorem mul_le_mul_left' {a b c : Rat} (hc : 0 ≤ c) (h : a ≤ b) : c * a ≤ c * b :=
  Rat.mul_le_mul_of_nonneg_left h hc

/-- The quadratic form `a² + k a b + b²`. -/
def W (k a b : Rat) : Rat := a * a + k * a * b + b * b

theorem W_nonneg (k a b : Rat) (h0 : 0 ≤ k) (h2 : k ≤ 2) : 0 ≤ W k a b := by
  have h1 := sq_nonneg' (2 * a + k * b)
  have h4 : 0 ≤ 4 - k * k := by
    have : k * k ≤ 2 * k := Rat.mul_le_mul_of_nonneg_right h2 h0
    grind
  have h3 := Rat.mul_nonneg h4 (sq_nonneg' b)
  unfold W
  grind

/-- `W` dominates the Euclidean norm: `(2 - k) (a² + b²) ≤ 2 W`. -/
theorem W_coercive (k a b : Rat) (h0 : 0 ≤ k) : (2 - k) * (a * a + b * b) ≤ 2 * W k a b := by
  have h := Rat.mul_nonneg h0 (sq_nonneg' (a + b))
  unfold W
  grind

/-! ## 1. SVF model -/

structure SvfState where
  c1 : Rat
  c2 : Rat

def svfD (g k : Rat) : Rat := 1 + g * g + g * k

def svfV1 (g k x : Rat) (s : SvfState) : Rat := (s.c1 + g * (x - s.c2)) / svfD g k

def svfV2 (g k x : Rat) (s : SvfState) : Rat := s.c2 + g * svfV1 g k x s

/-- One sample of the TPT SVF. -/
def svfStep (g k x : Rat) (s : SvfState) : SvfState :=
  ⟨2 * svfV1 g k x s - s.c1, 2 * svfV2 g k x s - s.c2⟩

/-- The Lyapunov function. -/
def svfV (k : Rat) (s : SvfState) : Rat := W k s.c1 s.c2

theorem svfD_pos (g k : Rat) (hg : 0 ≤ g) (hk : 0 ≤ k) : 0 < svfD g k := by
  have := Rat.mul_nonneg hg hg
  have := Rat.mul_nonneg hg hk
  unfold svfD
  grind

/-- The closed form solves the trapezoidal equations. -/
theorem svf_step_implicit (g k x : Rat) (s : SvfState) (hg : 0 ≤ g) (hk : 0 ≤ k) :
    svfV1 g k x s = g * (x - k * svfV1 g k x s - svfV2 g k x s) + s.c1 ∧
    svfV2 g k x s = g * svfV1 g k x s + s.c2 := by
  have hD := svfD_pos g k hg hk
  refine ⟨?_, by unfold svfV2; grind⟩
  have hv : svfV1 g k x s * svfD g k = s.c1 + g * (x - s.c2) := by
    unfold svfV1
    grind
  unfold svfV2
  unfold svfD at hv
  grind

/-- Discrete passivity identity, stated over the midpoint values `v`. -/
theorem svf_energy_identity (g k x v1 v2 : Rat) :
    W k (2 * v1 - (v1 - g * (x - k * v1 - v2))) (2 * v2 - (v2 - g * v1))
      - W k (v1 - g * (x - k * v1 - v2)) (v2 - g * v1)
      = 4 * g * (-(k / 2) * W k v1 v2 + (x * v1 + (k / 2) * x * v2)) := by
  unfold W
  grind

/-- Cauchy-Schwarz in the `W` geometry: `k (x v1 + (k/2) x v2) ≤ (k²/4) W(v) + x²`. -/
theorem svf_cross_bound (k x v1 v2 : Rat) (h0 : 0 ≤ k) (h2 : k ≤ 2) :
    k * (x * v1 + (k / 2) * x * v2) ≤ (k * k / 4) * W k v1 v2 + x * x := by
  apply le_of_sub_eq _ (W_nonneg k ((k / 2) * v1 - x) ((k / 2) * v2) h0 h2)
  unfold W
  grind

/-- `W(N v) = D W(v)` for `N = [[1 + g k, g], [-g, 1]]`. -/
theorem svf_stretch_identity (g k v1 v2 : Rat) :
    W k ((1 + g * k) * v1 + g * v2) (v2 - g * v1) = svfD g k * W k v1 v2 := by
  unfold W svfD
  grind

/-- `W(c) ≤ 2 D W(v) + 2 g² x²` where `c = N v - g x e1`. -/
theorem svf_state_from_mid (g k x v1 v2 : Rat) (h0 : 0 ≤ k) (h2 : k ≤ 2) :
    W k (v1 - g * (x - k * v1 - v2)) (v2 - g * v1)
      ≤ 2 * svfD g k * W k v1 v2 + 2 * (g * g) * (x * x) := by
  apply le_of_sub_eq _ (W_nonneg k ((1 + g * k) * v1 + g * v2 + g * x) (v2 - g * v1) h0 h2)
  unfold W svfD
  grind

/-- The linear core of the Lyapunov step, over named quantities. -/
theorem svf_lyapunov_combine (g k D Vn Vo Wm B x2 : Rat) (hg : 0 ≤ g) (hk : 0 ≤ k) (hD : 0 ≤ D)
    (hid : Vn - Vo = 4 * g * (-(k / 2) * Wm + B))
    (hcs : k * B ≤ (k * k / 4) * Wm + x2)
    (hmid : Vo ≤ 2 * D * Wm + 2 * (g * g) * x2) :
    2 * D * k * Vn ≤ (2 * D * k - g * k * k) * Vo + (2 * (g * g * g) * (k * k) + 8 * g * D) * x2 := by
  have t1 : 0 ≤ (k * k / 4) * Wm + x2 - k * B := by grind
  have t2 : 0 ≤ 2 * D * Wm + 2 * (g * g) * x2 - Vo := by grind
  have p1 := Rat.mul_nonneg (Rat.mul_nonneg (Rat.mul_nonneg (by grind : (0 : Rat) ≤ 8) hg) hD) t1
  have p2 := Rat.mul_nonneg (Rat.mul_nonneg hg (Rat.mul_nonneg hk hk)) t2
  have hn : Vn = Vo + 4 * g * (-(k / 2) * Wm + B) := by grind
  subst hn
  apply le_of_sub_eq _ (Rat.add_nonneg p1 p2)
  grind

/-- One-step Lyapunov inequality for the closed-form step. -/
theorem svf_lyapunov_step (g k x : Rat) (s : SvfState) (hg : 0 ≤ g) (h0 : 0 < k) (h2 : k ≤ 2) :
    2 * svfD g k * k * svfV k (svfStep g k x s)
      ≤ (2 * svfD g k * k - g * k * k) * svfV k s
        + (2 * (g * g * g) * (k * k) + 8 * g * svfD g k) * (x * x) := by
  have hk : 0 ≤ k := Rat.le_of_lt h0
  obtain ⟨h1, h2'⟩ := svf_step_implicit g k x s hg hk
  generalize hv1 : svfV1 g k x s = v1 at *
  generalize hv2 : svfV2 g k x s = v2 at *
  have ec1 : s.c1 = v1 - g * (x - k * v1 - v2) := by grind
  have ec2 : s.c2 = v2 - g * v1 := by grind
  have hs : svfV k (svfStep g k x s)
      = W k (2 * v1 - (v1 - g * (x - k * v1 - v2))) (2 * v2 - (v2 - g * v1)) := by
    unfold svfV svfStep; rw [hv1, hv2, ← ec1, ← ec2]
  have hv : svfV k s = W k (v1 - g * (x - k * v1 - v2)) (v2 - g * v1) := by
    unfold svfV; rw [← ec1, ← ec2]
  rw [hs, hv]
  exact svf_lyapunov_combine g k (svfD g k) _ _ (W k v1 v2) (x * v1 + (k / 2) * x * v2) (x * x)
    hg hk (Rat.le_of_lt (svfD_pos g k hg hk)) (svf_energy_identity g k x v1 v2)
    (svf_cross_bound k x v1 v2 hk h2) (svf_state_from_mid g k x v1 v2 hk h2)

/-- The linear core of the invariant step. -/
theorem svf_bound_combine (g k D Vn Vo K x2 Msq : Rat) (hg : 0 ≤ g) (h0 : 0 < k) (hD : 0 < D)
    (hgk : g * k ≤ 2 * D)
    (step : 2 * D * k * Vn ≤ (2 * D * k - g * k * k) * Vo + (2 * (g * g * g) * (k * k) + 8 * g * D) * x2)
    (hV : Vo ≤ K) (hx : x2 ≤ Msq)
    (hK : (2 * (g * g) * (k * k) + 8 * D) * Msq ≤ k * k * K) : Vn ≤ K := by
  have hk : 0 ≤ k := Rat.le_of_lt h0
  have lam : 0 ≤ 2 * D * k - g * k * k := by
    have := Rat.mul_nonneg hk (by grind : 0 ≤ 2 * D - g * k)
    apply le_of_sub_eq _ this
    grind
  have mu : 0 ≤ 2 * (g * g) * (k * k) + 8 * D := by
    have := Rat.mul_nonneg (Rat.mul_nonneg hg hg) (Rat.mul_nonneg hk hk)
    grind
  have p1 := Rat.mul_nonneg lam (by grind : 0 ≤ K - Vo)
  have p2 := Rat.mul_nonneg (Rat.mul_nonneg hg mu) (by grind : 0 ≤ Msq - x2)
  have p3 := Rat.mul_nonneg hg (by grind : 0 ≤ k * k * K - (2 * (g * g) * (k * k) + 8 * D) * Msq)
  have a4 : 2 * D * k * Vn ≤ 2 * D * k * K := by
    apply Rat.le_trans step
    apply le_of_sub_eq _ (Rat.add_nonneg (Rat.add_nonneg p1 p2) p3)
    grind
  have pos : 0 < 2 * D * k := Rat.mul_pos (Rat.mul_pos (by grind) hD) h0
  exact Rat.le_of_mul_le_mul_left a4 pos

/-- The invariant `V ≤ K` is preserved by one sample. -/
theorem svf_step_bounded (g k x K Msq : Rat) (s : SvfState)
    (hg : 0 ≤ g) (h0 : 0 < k) (h2 : k ≤ 2)
    (hV : svfV k s ≤ K) (hx : x * x ≤ Msq)
    (hK : (2 * (g * g) * (k * k) + 8 * svfD g k) * Msq ≤ k * k * K) :
    svfV k (svfStep g k x s) ≤ K := by
  have hk : 0 ≤ k := Rat.le_of_lt h0
  have hgk : g * k ≤ 2 * svfD g k := by
    have := Rat.mul_nonneg hg hg
    have := Rat.mul_nonneg hg hk
    unfold svfD; grind
  exact svf_bound_combine g k (svfD g k) _ _ K (x * x) Msq hg h0 (svfD_pos g k hg hk) hgk
    (svf_lyapunov_step g k x s hg h0 h2) hV hx hK

/-- The bound constant is monotone in the cutoff. -/
theorem svf_bound_mono (g G k : Rat) (gl : 0 ≤ g) (gh : g ≤ G) (hk : 0 ≤ k) :
    2 * (g * g) * (k * k) + 8 * svfD g k ≤ 2 * (G * G) * (k * k) + 8 * (1 + G * G + G * k) := by
  have hG : 0 ≤ G := Rat.le_trans gl gh
  have d : 0 ≤ G - g := by grind
  have gg : 0 ≤ G * G - g * g := by
    have := Rat.mul_nonneg (Rat.add_nonneg hG gl) d
    apply le_of_sub_eq _ this
    grind
  have q1 := Rat.mul_nonneg gg (Rat.mul_nonneg hk hk)
  have q2 := Rat.mul_nonneg d hk
  apply le_of_sub_eq _ (Rat.add_nonneg (Rat.add_nonneg (Rat.add_nonneg q1 q1) (Rat.mul_nonneg (by grind : (0 : Rat) ≤ 8) gg))
    (Rat.mul_nonneg (by grind : (0 : Rat) ≤ 8) q2))
  unfold svfD
  grind

/-- Bounded input, bounded state: cutoff may change every sample within `[0, G]`. -/
theorem svf_bibo (k G Msq K : Rat) (g x : Nat → Rat) (c : Nat → SvfState)
    (h0 : 0 < k) (h2 : k ≤ 2)
    (hg : ∀ n, 0 ≤ g n ∧ g n ≤ G)
    (hx : ∀ n, x n * x n ≤ Msq)
    (hstep : ∀ n, c (n + 1) = svfStep (g n) k (x n) (c n))
    (hinit : svfV k (c 0) ≤ K)
    (hK : (2 * (G * G) * (k * k) + 8 * (1 + G * G + G * k)) * Msq ≤ k * k * K) :
    ∀ n, svfV k (c n) ≤ K := by
  have hk : 0 ≤ k := Rat.le_of_lt h0
  have hM : 0 ≤ Msq := Rat.le_trans (sq_nonneg' (x 0)) (hx 0)
  intro n
  induction n with
  | zero => exact hinit
  | succ n ih =>
    rw [hstep n]
    obtain ⟨gl, gh⟩ := hg n
    have c2 := Rat.mul_le_mul_of_nonneg_right (svf_bound_mono (g n) G k gl gh hk) hM
    exact svf_step_bounded (g n) k (x n) K Msq (c n) gl h0 h2 ih (hx n) (Rat.le_trans c2 hK)

/-- Bounded Lyapunov value bounds both states. -/
theorem svf_state_bounded (k K : Rat) (s : SvfState) (h0 : 0 ≤ k) (hV : svfV k s ≤ K) :
    (2 - k) * (s.c1 * s.c1 + s.c2 * s.c2) ≤ 2 * K := by
  have := W_coercive k s.c1 s.c2 h0
  unfold svfV at hV
  grind

/-- The low-pass output `v2` is the midpoint of `ic2eq` and `ic2eq'`, so it is bounded too. -/
theorem svf_output_bounded (g k x K : Rat) (s : SvfState)
    (h0 : 0 ≤ k) (h2 : k ≤ 2)
    (hV : svfV k s ≤ K) (hV' : svfV k (svfStep g k x s) ≤ K) :
    (2 - k) * (svfV2 g k x s * svfV2 g k x s) ≤ 2 * K := by
  have a := svf_state_bounded k K s h0 hV
  have b := svf_state_bounded k K (svfStep g k x s) h0 hV'
  have mid : (svfStep g k x s).c2 = 2 * svfV2 g k x s - s.c2 := rfl
  generalize (svfStep g k x s).c1 = p1 at *
  generalize (svfStep g k x s).c2 = p2 at *
  generalize svfV2 g k x s = y at *
  have hk2 : 0 ≤ 2 - k := by grind
  -- 4 y² + (c2 - p2)² = 2 c2² + 2 p2² when p2 = 2 y - c2.
  have q := Rat.mul_nonneg hk2 (sq_nonneg' (s.c2 - (2 * y - s.c2)))
  have e1 := Rat.mul_nonneg hk2 (sq_nonneg' s.c1)
  have e2 := Rat.mul_nonneg hk2 (sq_nonneg' p1)
  subst mid
  generalize hA : (2 - k) * (y * y) = A at *
  generalize hQ : (2 - k) * ((s.c2 - (2 * y - s.c2)) * (s.c2 - (2 * y - s.c2))) = Q at *
  generalize hC1 : (2 - k) * (s.c1 * s.c1) = C1 at *
  generalize hP1 : (2 - k) * (p1 * p1) = P1 at *
  have ea : (2 - k) * (s.c1 * s.c1 + s.c2 * s.c2) = C1 + (2 - k) * (s.c2 * s.c2) := by
    rw [← hC1]; grind
  have eb : (2 - k) * (p1 * p1 + (2 * y - s.c2) * (2 * y - s.c2))
      = P1 + (2 - k) * ((2 * y - s.c2) * (2 * y - s.c2)) := by
    rw [← hP1]; grind
  have ec : 4 * A + Q = 2 * ((2 - k) * (s.c2 * s.c2))
      + 2 * ((2 - k) * ((2 * y - s.c2) * (2 * y - s.c2))) := by
    rw [← hA, ← hQ]; grind
  rw [ea] at a
  rw [eb] at b
  generalize (2 - k) * (s.c2 * s.c2) = C2 at *
  generalize (2 - k) * ((2 * y - s.c2) * (2 * y - s.c2)) = P2 at *
  grind

/-! ## 2. PID torque clamp -/

/-- `f32::clamp`: `x < lo` gives `lo`, `x > hi` gives `hi`, otherwise `x`. -/
def clamp (lo hi x : Rat) : Rat := if x < lo then lo else if hi < x then hi else x

theorem clamp_mem (lo hi x : Rat) (h : lo ≤ hi) : lo ≤ clamp lo hi x ∧ clamp lo hi x ≤ hi := by
  unfold clamp
  split
  · grind
  · split <;> grind

structure PidGains where
  kp : Rat
  ki : Rat
  kd : Rat
  dt : Rat
  torqueLimit : Rat
  integralLimit : Rat

structure PidState where
  integral : Rat
  prev : Option Rat

/-- One period: `some (setpoint, measurement)`, or `none` for a faulted sample. -/
def pidStep (p : PidGains) (st : PidState) : Option (Rat × Rat) → Rat × PidState
  | none => (0, st)
  | some (sp, meas) =>
    let e := sp - meas
    let i := clamp (-p.integralLimit) p.integralLimit (st.integral + p.ki * e * p.dt)
    let d := match st.prev with
      | some prev => -(meas - prev) / p.dt
      | none => 0
    let u := p.kp * e + i + p.kd * d
    (clamp (-p.torqueLimit) p.torqueLimit u, ⟨i, some meas⟩)

theorem pid_torque_clamped (p : PidGains) (st : PidState) (inp : Option (Rat × Rat))
    (ht : 0 ≤ p.torqueLimit) :
    -p.torqueLimit ≤ (pidStep p st inp).1 ∧ (pidStep p st inp).1 ≤ p.torqueLimit := by
  cases inp with
  | none => simp only [pidStep]; grind
  | some v =>
    obtain ⟨sp, meas⟩ := v
    simp only [pidStep]
    exact clamp_mem _ _ _ (by grind)

theorem pid_integral_clamped (p : PidGains) (st : PidState) (inp : Option (Rat × Rat))
    (hi : 0 ≤ p.integralLimit)
    (hst : -p.integralLimit ≤ st.integral ∧ st.integral ≤ p.integralLimit) :
    -p.integralLimit ≤ (pidStep p st inp).2.integral ∧
      (pidStep p st inp).2.integral ≤ p.integralLimit := by
  cases inp with
  | none => simp only [pidStep]; exact hst
  | some v =>
    obtain ⟨sp, meas⟩ := v
    simp only [pidStep]
    exact clamp_mem _ _ _ (by grind)

/-- Along any sample sequence, faults included, the integrator and every
torque command stay inside their clamps. -/
theorem pid_invariant (p : PidGains) (inp : Nat → Option (Rat × Rat))
    (st : Nat → PidState) (u : Nat → Rat)
    (ht : 0 ≤ p.torqueLimit) (hi : 0 ≤ p.integralLimit)
    (h0 : -p.integralLimit ≤ (st 0).integral ∧ (st 0).integral ≤ p.integralLimit)
    (hstep : ∀ n, (u n, st (n + 1)) = pidStep p (st n) (inp n)) :
    ∀ n, (-p.integralLimit ≤ (st n).integral ∧ (st n).integral ≤ p.integralLimit) ∧
      (-p.torqueLimit ≤ u n ∧ u n ≤ p.torqueLimit) := by
  have integ : ∀ n, -p.integralLimit ≤ (st n).integral ∧ (st n).integral ≤ p.integralLimit := by
    intro n
    induction n with
    | zero => exact h0
    | succ n ih =>
      have e := congrArg Prod.snd (hstep n)
      simp only at e
      rw [e]
      exact pid_integral_clamped p (st n) (inp n) hi ih
  intro n
  refine ⟨integ n, ?_⟩
  have e := congrArg Prod.fst (hstep n)
  simp only at e
  rw [e]
  exact pid_torque_clamped p (st n) (inp n) ht

end Vesper.ResonanceStability

#print axioms Vesper.ResonanceStability.W_nonneg
#print axioms Vesper.ResonanceStability.W_coercive
#print axioms Vesper.ResonanceStability.svf_step_implicit
#print axioms Vesper.ResonanceStability.svf_energy_identity
#print axioms Vesper.ResonanceStability.svf_cross_bound
#print axioms Vesper.ResonanceStability.svf_stretch_identity
#print axioms Vesper.ResonanceStability.svf_state_from_mid
#print axioms Vesper.ResonanceStability.svf_lyapunov_combine
#print axioms Vesper.ResonanceStability.svf_lyapunov_step
#print axioms Vesper.ResonanceStability.svf_bound_combine
#print axioms Vesper.ResonanceStability.svf_step_bounded
#print axioms Vesper.ResonanceStability.svf_bound_mono
#print axioms Vesper.ResonanceStability.svf_bibo
#print axioms Vesper.ResonanceStability.svf_state_bounded
#print axioms Vesper.ResonanceStability.svf_output_bounded
#print axioms Vesper.ResonanceStability.clamp_mem
#print axioms Vesper.ResonanceStability.pid_torque_clamped
#print axioms Vesper.ResonanceStability.pid_integral_clamped
#print axioms Vesper.ResonanceStability.pid_invariant
