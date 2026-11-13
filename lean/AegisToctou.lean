/-!
# Vesper Aegis: post-snapshot TOCTOU non-interference and policy soundness

A Lean model of two pieces of `crates/vesper-aegis` (design §33.4):

1. The single-fetch ingress (`BounceAperture::snapshot_from_host_bounce`
   followed by the gate's authenticate/evaluate steps). Host shared memory
   is a timeline `time → address → byte`; the ingress reads it only at the
   snapshot instant `t0`. We prove that two host timelines which agree on
   the snapshotted bytes (and reported length) at `t0` produce the same
   output, for *every* authentication and evaluation function, so host
   mutations after `t0` cannot change the result. A double-fetch ingress
   that re-reads shared memory at a later time does not have this property
   (counterexample below).

2. The RA-TLS relying-party check (`verify_attestation_claims`): an
   accepted claim set satisfies every policy condition, conditions are
   reported in the design's order, and the check accepts exactly when all
   conditions hold.

These are theorems about this model, not about the Rust code. The Rust
test `tests/lean_model.rs` compares `checkPolicy` with the Rust policy on
all 128 single-condition-failure combinations printed below.
-/

namespace AegisToctou

def modelId : String := "vesper.aegis.toctou-policy-model/1"
def modelClaim : String :=
  "Lean model of Aegis ingress and policy; proofs cover the model, not Rust"

/-! ## Part 1: single-fetch ingress -/

/-- Host-shared memory over time. -/
abbrev HostTimeline := Nat → Nat → UInt8

/-- The host-reported length over time (an `UntrustedHostVal[u32]`). -/
abbrev HostLength := Nat → Nat

inductive IngressError where
  | hostLengthOutOfRange
  | unauthenticated
  deriving DecidableEq, Repr

/-- Copy `len` bytes of shared memory, all read at time `t0`. -/
def snapshot (host : HostTimeline) (t0 len : Nat) : List UInt8 :=
  (List.range len).map (fun i => host t0 i)

/-- The modelled gate: validate the host length against the slot
capacity, snapshot once, authenticate, evaluate. `auth` and `eval` are
arbitrary functions of private data only. -/
def ingress {α : Type} (cap : Nat) (auth : List UInt8 → Option (List UInt8))
    (eval : List UInt8 → α) (host : HostTimeline) (hostLen : HostLength) (t0 : Nat) :
    Except IngressError α :=
  let len := hostLen t0
  if cap < len then .error .hostLengthOutOfRange
  else
    match auth (snapshot host t0 len) with
    | none => .error .unauthenticated
    | some plain => .ok (eval plain)

theorem snapshot_congr (h1 h2 : HostTimeline) (t0 len : Nat)
    (hagree : ∀ i, i < len → h1 t0 i = h2 t0 i) :
    snapshot h1 t0 len = snapshot h2 t0 len := by
  unfold snapshot
  apply List.map_congr_left
  intro i hi
  exact hagree i (List.mem_range.mp hi)

/-- Post-snapshot TOCTOU immunity: agreement on the snapshotted bytes and
the reported length at `t0` fixes the output. -/
theorem snapshot_blocks_hypervisor_toctou {α : Type} (cap : Nat)
    (auth : List UInt8 → Option (List UInt8)) (eval : List UInt8 → α)
    (h1 h2 : HostTimeline) (l1 l2 : HostLength) (t0 : Nat)
    (hlen : l1 t0 = l2 t0)
    (hagree : ∀ i, i < l1 t0 → h1 t0 i = h2 t0 i) :
    ingress cap auth eval h1 l1 t0 = ingress cap auth eval h2 l2 t0 := by
  unfold ingress
  simp only
  rw [snapshot_congr h1 h2 t0 (l1 t0) hagree, hlen]

/-- The host may rewrite anything at any time after `t0`. -/
def mutateAfter (host : HostTimeline) (t0 : Nat) (evil : HostTimeline) : HostTimeline :=
  fun t a => if t ≤ t0 then host t a else evil t a

theorem mutation_after_snapshot_is_invisible {α : Type} (cap : Nat)
    (auth : List UInt8 → Option (List UInt8)) (eval : List UInt8 → α)
    (host evil : HostTimeline) (len evilLen : HostLength) (t0 : Nat) :
    ingress cap auth eval (mutateAfter host t0 evil)
        (fun t => if t ≤ t0 then len t else evilLen t) t0 =
      ingress cap auth eval host len t0 := by
  apply snapshot_blocks_hypervisor_toctou
  · simp
  · intro i _
    simp [mutateAfter]

/-- A double-fetch ingress: validates the header byte at `t0`, reads the
body again at `t1`. -/
def doubleFetch (host : HostTimeline) (t0 t1 : Nat) : Except IngressError UInt8 :=
  if host t0 0 = 0 then .error .unauthenticated else .ok (host t1 0)

/-- Double fetch is not immune: two timelines equal at `t0` give
different outputs. -/
theorem double_fetch_interferes :
    ∃ h1 h2 : HostTimeline, h1 0 = h2 0 ∧ doubleFetch h1 0 1 ≠ doubleFetch h2 0 1 := by
  refine ⟨fun _ _ => 1, fun t _ => if t = 0 then 1 else 2, ?_, ?_⟩
  · funext a; rfl
  · simp [doubleFetch]

/-! ## Part 2: relying-party policy -/

structure Claims where
  arch : Nat
  debugDisabled : Bool
  tcbSvn : Nat
  mr : Nat
  loom : Nat
  olean : Nat
  binding : Nat

structure Policy where
  allowedArchs : List Nat
  floor : Nat → Nat
  allowedMeasurements : List (Nat × Nat)
  loom : Nat
  olean : Nat

inductive PolicyError where
  | arch | debug | tcb | measurement | loom | olean | binding
  deriving DecidableEq, Repr

/-- Transcription of the first seven checks of `verify_attestation_claims`
in the design's order (the SNP-only checks are not modelled). -/
def checkPolicy (c : Claims) (p : Policy) (expected : Nat) : Except PolicyError Unit :=
  if c.arch ∉ p.allowedArchs then .error .arch
  else if c.debugDisabled = false then .error .debug
  else if c.tcbSvn < p.floor c.arch then .error .tcb
  else if (c.arch, c.mr) ∉ p.allowedMeasurements then .error .measurement
  else if c.loom ≠ p.loom then .error .loom
  else if c.olean ≠ p.olean then .error .olean
  else if c.binding ≠ expected then .error .binding
  else .ok ()

/-- Relying-party quote soundness. -/
theorem verify_quote_enforces_supply_chain_and_tcb (c : Claims) (p : Policy) (e : Nat)
    (h : checkPolicy c p e = .ok ()) :
    c.arch ∈ p.allowedArchs ∧ c.debugDisabled = true ∧ c.tcbSvn ≥ p.floor c.arch ∧
      (c.arch, c.mr) ∈ p.allowedMeasurements ∧ c.loom = p.loom ∧ c.olean = p.olean ∧
      c.binding = e := by
  unfold checkPolicy at h
  by_cases h1 : c.arch ∈ p.allowedArchs <;> simp [h1] at h
  by_cases h2 : c.debugDisabled = false <;> simp [h2] at h
  by_cases h3 : c.tcbSvn < p.floor c.arch <;> simp [h3] at h
  by_cases h4 : (c.arch, c.mr) ∈ p.allowedMeasurements <;> simp [h4] at h
  by_cases h5 : c.loom = p.loom <;> simp [h5] at h
  by_cases h6 : c.olean = p.olean <;> simp [h6] at h
  refine ⟨h1, ?_, Nat.not_lt.mp h3, h4, h5, h6, ?_⟩
  · cases hd : c.debugDisabled <;> simp_all
  · by_cases h7 : c.binding = e
    · exact h7
    · simp [h7] at h

/-- Completeness: every condition holding means acceptance. -/
theorem checkPolicy_complete (c : Claims) (p : Policy) (e : Nat)
    (h1 : c.arch ∈ p.allowedArchs) (h2 : c.debugDisabled = true)
    (h3 : c.tcbSvn ≥ p.floor c.arch) (h4 : (c.arch, c.mr) ∈ p.allowedMeasurements)
    (h5 : c.loom = p.loom) (h6 : c.olean = p.olean) (h7 : c.binding = e) :
    checkPolicy c p e = .ok () := by
  unfold checkPolicy
  simp [h1, h2, h4, h5, h6, h7, Nat.not_lt.mpr h3]

/-- Debug mode is reported before any TCB, measurement or digest failure. -/
theorem debug_reported_first (c : Claims) (p : Policy) (e : Nat)
    (h1 : c.arch ∈ p.allowedArchs) (h2 : c.debugDisabled = false) :
    checkPolicy c p e = .error .debug := by
  unfold checkPolicy
  simp [h1, h2]

/-! ## Differential fixture

Bit `k` of `mask` breaks condition `k` (0 arch, 1 debug, 2 TCB,
3 measurement, 4 Loom, 5 olean, 6 binding). The Rust test builds the same
128 claim sets and compares the first reported error. -/

def fixturePolicy : Policy :=
  { allowedArchs := [1, 2], floor := fun _ => 10,
    allowedMeasurements := [(1, 1), (2, 1)], loom := 2, olean := 3 }

def bit (mask k : Nat) : Bool := (mask >>> k) % 2 = 1

def fixtureClaims (mask : Nat) : Claims :=
  { arch := if bit mask 0 then 3 else 1,
    debugDisabled := !bit mask 1,
    tcbSvn := if bit mask 2 then 9 else 10,
    mr := if bit mask 3 then 0 else 1,
    loom := if bit mask 4 then 5 else 2,
    olean := if bit mask 5 then 6 else 3,
    binding := if bit mask 6 then 7 else 4 }

def resultName : Except PolicyError Unit → String
  | .ok () => "ok"
  | .error .arch => "arch"
  | .error .debug => "debug"
  | .error .tcb => "tcb"
  | .error .measurement => "measurement"
  | .error .loom => "loom"
  | .error .olean => "olean"
  | .error .binding => "binding"

def fixtureLine : String :=
  "AEGIS-POLICY " ++ String.intercalate ","
    ((List.range 128).map (fun m => resultName (checkPolicy (fixtureClaims m) fixturePolicy 4)))

example : checkPolicy (fixtureClaims 0) fixturePolicy 4 = .ok () := rfl
example : checkPolicy (fixtureClaims 6) fixturePolicy 4 = .error .debug := rfl

#eval IO.println fixtureLine

#print axioms snapshot_congr
#print axioms snapshot_blocks_hypervisor_toctou
#print axioms mutation_after_snapshot_is_invisible
#print axioms double_fetch_interferes
#print axioms verify_quote_enforces_supply_chain_and_tcb
#print axioms checkPolicy_complete
#print axioms debug_reported_first

end AegisToctou
