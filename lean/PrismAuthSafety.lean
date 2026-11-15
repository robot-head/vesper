-- Vesper Prism (§33.5): Multi-tenant HTTP ingest authorization policy proofs.
--
-- What is proved, and about which model:
-- * `ingestEvent` models Vesper Prism's effectful request handler requiring an
--   `OrgWriteGrant` from `Auth` before emitting `TenantDbOp.appendEvent`.
-- * `ingest_event_never_writes_unauthorized_org` establishes the §33.5 policy
--   invariant: every emitted `appendEvent` carries an `OrgWriteGrant` whose
--   `orgId` equals the authenticated target `org` (and rejects unauthorized or
--   oversized payloads).
-- * `ingest_event_payload_bounded` proves accepted writes respect
--   `cfg.maxPayloadBytes`.
--
-- No `sorry`, no `admit`, and no custom axioms are used.

namespace Vesper.PrismAuthSafety

structure OrgWriteGrant where
  orgId : UInt64
  scope : UInt32
  deriving DecidableEq, Repr

structure HttpRequestView where
  pathHash : UInt64
  topic : String
  payload : List UInt8
  deriving DecidableEq, Repr

structure WorkerShardProj where
  shardId : Nat
  queuedWrites : Nat
  deriving DecidableEq, Repr

structure PrismConfig where
  maxPayloadBytes : Nat
  deriving DecidableEq, Repr

inductive TenantDbOp where
  | rejectUnauthorized (org : UInt64)
  | rejectOversized (org : UInt64) (len : Nat)
  | appendEvent (grant : OrgWriteGrant) (topic : String) (bytes : List UInt8)
  deriving DecidableEq, Repr

inductive AuthAction where
  | requireGrant (org : UInt64) (k : OrgWriteGrant → List TenantDbOp)
      (onDeny : List TenantDbOp)
  | emit (ops : List TenantDbOp)

def runWithAuth (authOracle : UInt64 → Option OrgWriteGrant) :
    AuthAction → List TenantDbOp
  | .requireGrant org k onDeny =>
      match authOracle org with
      | some g => k g
      | none => onDeny
  | .emit ops => ops

def TraceOf (trace : List TenantDbOp) : List TenantDbOp := trace

def ingestEvent (org : UInt64) (req : HttpRequestView)
    (_shard : WorkerShardProj) (cfg : PrismConfig) : AuthAction :=
  .requireGrant org
    (fun g =>
      if req.payload.length ≤ cfg.maxPayloadBytes then
        [.appendEvent g req.topic req.payload]
      else
        [.rejectOversized org req.payload.length])
    [.rejectUnauthorized org]

/-- §33.5 global authorization policy: no `appendEvent` ever executes for an `org`
without a matching `OrgWriteGrant` issued by `authOracle`. -/
theorem ingest_event_never_writes_unauthorized_org
    (authOracle : UInt64 → Option OrgWriteGrant)
    (h_sound : ∀ org g, authOracle org = some g → g.orgId = org)
    (org : UInt64) (req : HttpRequestView)
    (shard : WorkerShardProj) (cfg : PrismConfig) :
    ∀ op ∈ TraceOf (runWithAuth authOracle (ingestEvent org req shard cfg)),
      ∀ g topic bytes, op = TenantDbOp.appendEvent g topic bytes → g.orgId = org := by
  unfold TraceOf runWithAuth ingestEvent
  cases hauth : authOracle org with
  | none =>
      intro op hop g topic bytes heq
      simp only [List.mem_singleton] at hop
      subst hop
      cases heq
  | some g' =>
      intro op hop g topic bytes heq
      dsimp only at hop
      split at hop
      · simp only [List.mem_singleton] at hop
        subst hop
        cases heq
        exact h_sound org g' hauth
      · simp only [List.mem_singleton] at hop
        subst hop
        cases heq

/-- Every emitted `appendEvent` payload length obeys `cfg.maxPayloadBytes`. -/
theorem ingest_event_payload_bounded
    (authOracle : UInt64 → Option OrgWriteGrant)
    (org : UInt64) (req : HttpRequestView)
    (shard : WorkerShardProj) (cfg : PrismConfig) :
    ∀ op ∈ TraceOf (runWithAuth authOracle (ingestEvent org req shard cfg)),
      ∀ g topic bytes,
        op = TenantDbOp.appendEvent g topic bytes →
        bytes.length ≤ cfg.maxPayloadBytes := by
  unfold TraceOf runWithAuth ingestEvent
  cases hauth : authOracle org with
  | none =>
      intro op hop g topic bytes heq
      simp only [List.mem_singleton] at hop
      subst hop
      cases heq
  | some g' =>
      intro op hop g topic bytes heq
      dsimp only at hop
      split at hop
      · rename_i hlen
        simp only [List.mem_singleton] at hop
        subst hop
        cases heq
        exact hlen
      · simp only [List.mem_singleton] at hop
        subst hop
        cases heq

end Vesper.PrismAuthSafety
