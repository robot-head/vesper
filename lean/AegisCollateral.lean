/-!
# Vesper Aegis: Intel TDX TCB evaluation order and monotonicity

A Lean model of `evaluate_tdx_tcb` in `crates/vesper-aegis/src/intel.rs`
(design §33) and of the collateral anti-rollback floor
(`TdxCollateralFloor`, `check_snp_crl_floor`).

The model:

* TCB statuses are Intel's seven. They are ordered by what they demand of
  the relying party: a status is "no better than" another when it is
  revoked, or when it needs at least the same updates (out of date,
  configuration, software hardening). `OutOfDate` subsumes software
  hardening. This is a partial order: `ConfigurationNeeded` and `OutOfDate`
  are incomparable.
* QE identity and TDX module identity levels carry only `UpToDate`,
  `OutOfDate` or `Revoked` (the Rust parser rejects anything else).
* Each level list is searched first-match, as Intel publishes levels
  newest first. A platform level matches when its SGX components and PCESVN
  are component-wise at most the PCK certificate's and its TDX components
  at most the TD report's `TEE_TCB_SVN`; an ISV level matches when its
  ISVSVN is at most the evidence's.
* Evaluation order (as in the Rust): TDX module identity level, then the
  platform level, then the QE identity level; the first missing level is
  the error. The result converges platform with QE, then with the module
  (`TcbStatus::converge`).

Proved about this model:

1. `converge` is monotone in both arguments;
2. first-match selection over a list sorted from best to worst status is
   monotone: raising every SVN never selects a worse level, and a level is
   still found;
3. the whole evaluation is monotone (`evaluate_monotone`) and a revoked
   QE or module makes the result revoked (`evaluate_revoked_qe`,
   `evaluate_revoked_module`); the first missing level, in evaluation
   order, is the reported error (`evaluate_module_first`);
4. the anti-rollback floor only moves up, accepts what raised it, and
   rejects anything older afterwards.

These are theorems about this model, not about the Rust code. The Rust
test `tests/lean_model.rs` compares the printed `converge` table with
`TcbStatus::converge` on all 21 inputs.
-/

namespace AegisCollateral

def modelId : String := "vesper.aegis.tcb-evaluation-model/1"
def modelClaim : String :=
  "Lean model of Intel TDX TCB evaluation; proofs cover the model, not Rust"

/-! ## Statuses and their order -/

inductive Status where
  | upToDate
  | swHardeningNeeded
  | configurationNeeded
  | configurationAndSWHardeningNeeded
  | outOfDate
  | outOfDateConfigurationNeeded
  | revoked
  deriving DecidableEq, Repr

/-- QE identity and TDX module identity statuses. -/
inductive IsvStatus where
  | upToDate
  | outOfDate
  | revoked
  deriving DecidableEq, Repr

def IsvStatus.toStatus : IsvStatus → Status
  | .upToDate => .upToDate
  | .outOfDate => .outOfDate
  | .revoked => .revoked

/-- Needs a TCB update (out of date). -/
def Status.ood : Status → Bool
  | .outOfDate | .outOfDateConfigurationNeeded | .revoked => true
  | _ => false

/-- Needs a configuration change. -/
def Status.cfg : Status → Bool
  | .configurationNeeded | .configurationAndSWHardeningNeeded
  | .outOfDateConfigurationNeeded | .revoked => true
  | _ => false

/-- Needs software hardening (implied by out of date). -/
def Status.sw : Status → Bool
  | .upToDate | .configurationNeeded => false
  | _ => true

def ble (a b : Bool) : Bool := !a || b

/-- `sle s t`: `s` is at least as good as `t`. -/
def sle (s t : Status) : Bool :=
  t == .revoked ||
    (s != .revoked && ble s.ood t.ood && ble s.cfg t.cfg && ble s.sw t.sw)

/-- ISV statuses are totally ordered: up to date, out of date, revoked. -/
def IsvStatus.rank : IsvStatus → Nat
  | .upToDate => 0
  | .outOfDate => 1
  | .revoked => 2

def isle (a b : IsvStatus) : Bool := Nat.ble a.rank b.rank

theorem sle_refl : ∀ s, sle s s = true := by
  intro s; cases s <;> rfl

theorem sle_trans : ∀ a b c, sle a b = true → sle b c = true → sle a c = true := by
  intro a b c; cases a <;> cases b <;> cases c <;> decide

theorem isle_sle : ∀ a b, isle a b = true → sle a.toStatus b.toStatus = true := by
  intro a b; cases a <;> cases b <;> decide

/-- Intel QVL convergence of a platform status with a QE or TDX module
status (`TcbStatus::converge`). -/
def converge (p : Status) (o : IsvStatus) : Status :=
  match p, o with
  | .revoked, _ => .revoked
  | _, .revoked => .revoked
  | .upToDate, .outOfDate => .outOfDate
  | .swHardeningNeeded, .outOfDate => .outOfDate
  | .configurationNeeded, .outOfDate => .outOfDateConfigurationNeeded
  | .configurationAndSWHardeningNeeded, .outOfDate => .outOfDateConfigurationNeeded
  | p, _ => p

theorem converge_monotone :
    ∀ p p' o o', sle p p' = true → isle o o' = true →
      sle (converge p o) (converge p' o') = true := by
  intro p p' o o'
  cases p <;> cases p' <;> cases o <;> cases o' <;> decide

theorem converge_revoked : ∀ p, converge p .revoked = .revoked := by
  intro p; cases p <;> rfl

theorem converge_upToDate : ∀ p, converge p .upToDate = p := by
  intro p; cases p <;> rfl

/-! ## Component-wise SVN comparison -/

/-- `vle a b`: every component of `a` is at most the matching one of `b`
(vectors of equal length; a shorter `b` fails). -/
def vle : List Nat → List Nat → Bool
  | [], _ => true
  | _ :: _, [] => false
  | a :: as, b :: bs => Nat.ble a b && vle as bs

theorem vle_trans : ∀ a b c, vle a b = true → vle b c = true → vle a c = true := by
  intro a
  induction a with
  | nil => intro b c _ _; rfl
  | cons x xs ih =>
    intro b c hab hbc
    cases b with
    | nil => simp [vle] at hab
    | cons y ys =>
      cases c with
      | nil => simp [vle] at hbc
      | cons z zs =>
        simp only [vle, Bool.and_eq_true, Nat.ble_eq] at hab hbc ⊢
        exact ⟨Nat.le_trans hab.1 hbc.1, ih ys zs hab.2 hbc.2⟩

/-! ## First-match selection -/

/-- A list sorted from best to worst status. -/
def SortedBy {α : Type} (st : α → Status) : List α → Prop
  | [] => True
  | l :: ls => (∀ m ∈ ls, sle (st l) (st m) = true) ∧ SortedBy st ls

theorem find_monotone {α : Type} (st : α → Status) (okx oky : α → Bool)
    (hxy : ∀ a, okx a = true → oky a = true) :
    ∀ (ls : List α), SortedBy st ls → ∀ ax, ls.find? okx = some ax →
      ∃ ay, ls.find? oky = some ay ∧ sle (st ay) (st ax) = true := by
  intro ls
  induction ls with
  | nil => intro _ ax h; simp at h
  | cons l ls ih =>
    intro hs ax hx
    rcases hs with ⟨hhead, htail⟩
    by_cases hy : oky l = true
    · refine ⟨l, by simp [List.find?, hy], ?_⟩
      by_cases hxl : okx l = true
      · have : ax = l := by simp [List.find?, hxl] at hx; exact hx.symm
        subst this; exact sle_refl _
      · have hfx : ls.find? okx = some ax := by
          simpa [List.find?, hxl] using hx
        exact hhead ax (List.mem_of_find?_eq_some hfx)
    · have hxl : okx l = false := by
        cases h : okx l
        · rfl
        · exact absurd (hxy l h) hy
      have hfx : ls.find? okx = some ax := by simpa [List.find?, hxl] using hx
      obtain ⟨ay, hay, hle⟩ := ih htail ax hfx
      have hyl : oky l = false := by simpa using hy
      exact ⟨ay, by simp [List.find?, hyl, hay], hle⟩

/-! ## Levels and evaluation -/

structure PlatformLevel where
  sgx : List Nat
  pceSvn : Nat
  tdx : List Nat
  status : Status

structure IsvLevel where
  isvSvn : Nat
  status : IsvStatus

/-- The evidence: PCK SGX components and PCESVN, the TD report's TDX
components (already sliced as the Rust slices them), QE ISVSVN, and the
TDX module SVN (`TEE_TCB_SVN[0]`). -/
structure Evidence where
  sgx : List Nat
  pceSvn : Nat
  tdx : List Nat
  qeSvn : Nat
  moduleSvn : Nat

structure Collateral where
  platform : List PlatformLevel
  qe : List IsvLevel
  module : List IsvLevel

def platformMatches (e : Evidence) (l : PlatformLevel) : Bool :=
  vle l.sgx e.sgx && Nat.ble l.pceSvn e.pceSvn && vle l.tdx e.tdx

def isvMatches (svn : Nat) (l : IsvLevel) : Bool := Nat.ble l.isvSvn svn

inductive EvalError where
  | moduleLevelNotFound
  | platformLevelNotFound
  | qeLevelNotFound
  deriving DecidableEq, Repr

/-- Evaluation in the Rust order: module identity, platform, QE. -/
def evaluate (c : Collateral) (e : Evidence) : Except EvalError Status :=
  match c.module.find? (isvMatches e.moduleSvn) with
  | none => .error .moduleLevelNotFound
  | some m =>
    match c.platform.find? (platformMatches e) with
    | none => .error .platformLevelNotFound
    | some p =>
      match c.qe.find? (isvMatches e.qeSvn) with
      | none => .error .qeLevelNotFound
      | some q => .ok (converge (converge p.status q.status) m.status)

/-- `x ≤ y` on evidence: every SVN of `y` at least `x`'s. -/
def EvidenceLe (x y : Evidence) : Prop :=
  vle x.sgx y.sgx = true ∧ x.pceSvn ≤ y.pceSvn ∧ vle x.tdx y.tdx = true ∧
    x.qeSvn ≤ y.qeSvn ∧ x.moduleSvn ≤ y.moduleSvn

def isvSt (l : IsvLevel) : Status := l.status.toStatus

/-- Well-formed collateral: each list sorted from best to worst status. -/
def Sorted (c : Collateral) : Prop :=
  SortedBy PlatformLevel.status c.platform ∧ SortedBy isvSt c.qe ∧ SortedBy isvSt c.module

theorem platformMatches_mono (x y : Evidence) (h : EvidenceLe x y) :
    ∀ l, platformMatches x l = true → platformMatches y l = true := by
  intro l hl
  rcases h with ⟨hs, hp, ht, _, _⟩
  simp only [platformMatches, Bool.and_eq_true, Nat.ble_eq] at hl ⊢
  exact ⟨⟨vle_trans _ _ _ hl.1.1 hs, Nat.le_trans hl.1.2 hp⟩, vle_trans _ _ _ hl.2 ht⟩

theorem isvMatches_mono (a b : Nat) (h : a ≤ b) :
    ∀ l, isvMatches a l = true → isvMatches b l = true := by
  intro l hl
  simp only [isvMatches, Nat.ble_eq] at hl ⊢
  exact Nat.le_trans hl h

/-- An ISV status is determined by its embedding into `Status`. -/
theorem isv_sle_isle : ∀ a b, sle a.toStatus b.toStatus = true → isle a b = true := by
  intro a b; cases a <;> cases b <;> decide

/-- Monotonicity: if evidence `x` evaluates to `s`, evidence `y` with every
SVN at least `x`'s evaluates (no level goes missing) to a status at least as
good as `s`. -/
theorem evaluate_monotone (c : Collateral) (hc : Sorted c) (x y : Evidence)
    (hxy : EvidenceLe x y) (s : Status) (hs : evaluate c x = .ok s) :
    ∃ s', evaluate c y = .ok s' ∧ sle s' s = true := by
  rcases hc with ⟨hp, hq, hm⟩
  unfold evaluate at hs ⊢
  split at hs
  · cases hs
  · rename_i mx hmx
    split at hs
    · cases hs
    · rename_i px hpx
      split at hs
      · cases hs
      · rename_i qx hqx
        cases hs
        obtain ⟨my, hmy, hmle⟩ := find_monotone isvSt _ _
          (isvMatches_mono _ _ hxy.2.2.2.2) c.module hm mx hmx
        obtain ⟨py, hpy, hple⟩ := find_monotone PlatformLevel.status _ _
          (platformMatches_mono x y hxy) c.platform hp px hpx
        obtain ⟨qy, hqy, hqle⟩ := find_monotone isvSt _ _
          (isvMatches_mono _ _ hxy.2.2.2.1) c.qe hq qx hqx
        refine ⟨converge (converge py.status qy.status) my.status, ?_, ?_⟩
        · simp [hmy, hpy, hqy]
        · exact converge_monotone _ _ _ _
            (converge_monotone _ _ _ _ hple (isv_sle_isle _ _ hqle))
            (isv_sle_isle _ _ hmle)

/-- A revoked QE level makes the result revoked, whatever the platform
and module say. -/
theorem evaluate_revoked_qe (c : Collateral) (e : Evidence) (m : IsvLevel)
    (p : PlatformLevel) (q : IsvLevel)
    (hm : c.module.find? (isvMatches e.moduleSvn) = some m)
    (hp : c.platform.find? (platformMatches e) = some p)
    (hq : c.qe.find? (isvMatches e.qeSvn) = some q) (hr : q.status = .revoked) :
    evaluate c e = .ok .revoked := by
  simp [evaluate, hm, hp, hq, hr, converge_revoked]
  cases m.status <;> cases p.status <;> rfl

/-- A revoked module level makes the result revoked. -/
theorem evaluate_revoked_module (c : Collateral) (e : Evidence) (m : IsvLevel)
    (p : PlatformLevel) (q : IsvLevel)
    (hm : c.module.find? (isvMatches e.moduleSvn) = some m)
    (hp : c.platform.find? (platformMatches e) = some p)
    (hq : c.qe.find? (isvMatches e.qeSvn) = some q) (hr : m.status = .revoked) :
    evaluate c e = .ok .revoked := by
  simp [evaluate, hm, hp, hq, hr, converge_revoked]

/-- Evaluation order: a missing module level is reported first, whatever
the platform and QE levels are. -/
theorem evaluate_module_first (c : Collateral) (e : Evidence)
    (hm : c.module.find? (isvMatches e.moduleSvn) = none) :
    evaluate c e = .error .moduleLevelNotFound := by
  simp [evaluate, hm]

/-- With a module level present, a missing platform level is reported
before the QE identity is consulted. -/
theorem evaluate_platform_before_qe (c : Collateral) (e : Evidence) (m : IsvLevel)
    (hm : c.module.find? (isvMatches e.moduleSvn) = some m)
    (hp : c.platform.find? (platformMatches e) = none) :
    evaluate c e = .error .platformLevelNotFound := by
  simp [evaluate, hm, hp]

/-! ## Anti-rollback floor -/

/-- Collateral with freshness `n` (a `tcbEvaluationDataNumber`, an
`issueDate` or a CRL `thisUpdate`) is accepted against `floor`. -/
def accepts (floor n : Nat) : Bool := Nat.ble floor n

/-- The floor after accepting `n` (`TdxCollateralFloor::raised_by`). -/
def raise (floor n : Nat) : Nat := max floor n

theorem raise_never_lowers (floor n : Nat) : floor ≤ raise floor n :=
  Nat.le_max_left floor n

theorem raise_accepts_its_input (floor n : Nat) (h : accepts floor n = true) :
    accepts (raise floor n) n = true := by
  simp only [accepts, Nat.ble_eq] at h ⊢
  simp [raise, Nat.max_def]; split <;> omega

theorem rollback_rejected (floor n m : Nat) (h : m < n) :
    accepts (raise floor n) m = false := by
  simp only [accepts, raise]
  cases hb : Nat.ble (max floor n) m
  · rfl
  · simp only [Nat.ble_eq] at hb; omega

theorem accepts_antitone (f g n : Nat) (hfg : f ≤ g) (h : accepts g n = true) :
    accepts f n = true := by
  simp only [accepts, Nat.ble_eq] at h ⊢
  omega

/-! ## Differential fixture -/

def Status.name : Status → String
  | .upToDate => "UpToDate"
  | .swHardeningNeeded => "SWHardeningNeeded"
  | .configurationNeeded => "ConfigurationNeeded"
  | .configurationAndSWHardeningNeeded => "ConfigurationAndSWHardeningNeeded"
  | .outOfDate => "OutOfDate"
  | .outOfDateConfigurationNeeded => "OutOfDateConfigurationNeeded"
  | .revoked => "Revoked"

def allStatus : List Status :=
  [.upToDate, .swHardeningNeeded, .configurationNeeded,
   .configurationAndSWHardeningNeeded, .outOfDate, .outOfDateConfigurationNeeded, .revoked]

def allIsv : List IsvStatus := [.upToDate, .outOfDate, .revoked]

/-- `converge` on every (platform, ISV) pair, platform-major. -/
def convergeTable : String :=
  ",".intercalate (allStatus.flatMap fun p => allIsv.map fun o => (converge p o).name)

#eval IO.println ("AEGIS-CONVERGE " ++ convergeTable)
#eval IO.println ("AEGIS-COLLATERAL-MODEL " ++ modelId)

#print axioms sle_trans
#print axioms converge_monotone
#print axioms vle_trans
#print axioms find_monotone
#print axioms evaluate_monotone
#print axioms evaluate_revoked_qe
#print axioms evaluate_revoked_module
#print axioms evaluate_module_first
#print axioms evaluate_platform_before_qe
#print axioms raise_accepts_its_input
#print axioms rollback_rejected
#print axioms accepts_antitone

end AegisCollateral
