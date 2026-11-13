//! Cached, real WASI execution of Forge `ProcMacro`/`DeriveMacro`
//! components, with approval recomputed on every use (design §§8.2, 16.4).
//!
//! Two layers are kept strictly apart:
//!
//! * **Execution** ([`MacroRunner`]). A macro component runs under the
//!   `vesper-loom-sandbox` host with CAS-snapshotted, read-only declared
//!   inputs, deterministic clocks/randomness and explicit budgets. Its flat
//!   wire result is untrusted. A successful result is stored as a bounded
//!   [`RawRecord`] under an execution key that binds the exact component
//!   bytes, the canonical invocation (kind, entry, args, input token text
//!   and display spans), every input snapshot, every observable sandbox
//!   limit and determinism setting, and the measured [`HostIdentity`]
//!   (executable and mapped-object digests, compiler, target, CPU
//!   features, WIT). A hit is accepted only after the record is rehashed,
//!   size-checked before it is read, strictly decoded within the request's
//!   limits and found to name its own key.
//! * **Approval** ([`ForgeSession`]). Every hit or miss is imported into a
//!   fresh `vesper-forge` expansion of the caller's session: new
//!   introduction scope, all tokens macro-originated, guest scopes dropped,
//!   spans display-only, `pre-resolved` ids only claims. Resolution and
//!   the transitive authority gate run against the *current*
//!   [`ApprovalContext`], whose identity is a digest of its complete
//!   canonical content (registry, environments, graph, policies, ceilings,
//!   component bindings and limits), not of its version labels.
//!
//! No host scope, expansion id, splice handle, resolution, audit or
//! [`vesper_forge::AcceptedExpansion`] is ever written to the cache, so an
//! approval-context change can reuse a raw execution but never an approval.
//!
//! Flat output carries no splice evidence: even echoed caller tokens are
//! attributed to the macro. Caller-local antiquotation is unsupported.
//!
//! Hashes and audits here are conditional local policy evidence. They are
//! not package confinement, registry provenance, signatures or a build-graph
//! claim; this crate performs no network access.

#![warn(missing_debug_implementations)]

mod error;
mod identity;
mod key;
pub mod quote_key;
pub mod quote_record;
pub mod quote_runner;
pub mod quote_session;
mod record;
mod request;
mod runner;
mod session;

pub use error::RunnerError;
pub use identity::HostIdentity;
pub use key::{KEY_SCHEMA_VERSION, RUNNER_NAME, canonical_invocation};
pub use record::{RECORD_SCHEMA_VERSION, RawRecord, RecordError, RecordLimits};
pub use request::{InputSpec, MacroKind, MacroRequest, RequestError, RunnerLimits};
pub use runner::{
    CachePolicy, CacheStatus, CorruptionPolicy, ExecOptions, MacroEngine, MacroRunner,
    PreparedExecution, RawExecution,
};
pub use session::{
    APPROVAL_CONTEXT_SCHEMA_VERSION, ApprovalContext, ApprovalContextId, ContextError,
    ExecutionSummary, ForgeSession, FreshApproval, GlobalDecl, MacroBinding, MacroCall,
    MacroOutcome, Rejection, Verdict,
};
