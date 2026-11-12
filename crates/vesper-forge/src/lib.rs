//! Vesper Forge: quotation hygiene, emission provenance and the emitted-code
//! authority gate (design §16.4, contract audit "Macro capability
//! provenance" and "Authority versus discharged rows").
//!
//! This crate is the compiler-independent core that untrusted WASI macro
//! output will pass through:
//!
//! 1. [`HygieneHost`] owns every scope, expansion, splice handle, binder and
//!    symbol identity. Identities are session-qualified and minted from
//!    checked counters that fail instead of wrapping.
//! 2. An expansion stores caller syntax in a host arena and hands out
//!    [`SpliceHandle`]s. Only syntax substituted from that arena keeps its
//!    original origin; every other output token is attributed to the macro.
//!    Flat sandbox output ([`HygieneHost::finish_flat_outcome`]) carries no
//!    splice evidence, so all of it is macro-generated.
//! 3. [`HygieneHost::resolve`] parses the documented lexical fragment (see
//!    [`syntax`]) and resolves binders and references with scope-set
//!    inclusion, picking the unique most specific binding and failing on
//!    ambiguity. Pre-resolved global claims are validated against the trusted
//!    [`DefinitionRegistry`] from the emitting package's point of view.
//! 4. [`check_authority`] computes transitive authority over a trusted,
//!    versioned [`AuthorityGraph`] for every operation an expansion
//!    contributed, and requires each requirement to fit both the macro's
//!    `emits` policy (and those of enclosing expansions) and the root
//!    caller's ceiling. Accepted output carries an [`AuthorityAudit`] naming
//!    the policy and graph versions it is conditional on.
//!
//! It is not a source macro expander, a typed CST arena ABI, a Loom
//! confinement certificate, or an audit of arbitrary source: authority
//! evidence is only as good as the resolved graph the compiler supplies.

mod authority;
mod error;
mod host;
mod identity;
mod lex;
mod registry;
mod resolve;
pub mod syntax;
mod token;
#[cfg(feature = "sandbox-transport")]
pub mod transport;
pub mod typed_quote;

pub use authority::{
    AUTHORITY_SCHEMA_VERSION, AcceptedExpansion, Authority, AuthorityArg, AuthorityAudit,
    AuthorityGrant, AuthorityGraph, AuthorityNode, AuthorityRejection, CallerCeiling, Denial,
    EffectId, GraphEdge, GraphFault, HandlerRule, MacroPolicy, OperationAudit, PolicySet,
    PolicyStamp, Requirements, Violation, check_authority,
};
pub use error::{ForgeError, IdentityKind, SpliceFault};
pub use host::{Expansion, ExpansionRecord, HygieneHost, InvocationSite};
pub use identity::{
    BinderId, ExpansionId, IDENTITY_SCHEMA_VERSION, IdentityLimits, ScopeId, ScopeSet, SessionId,
    SpliceHandle, Symbol,
};
pub use lex::lex_fragment;
pub use registry::{
    DefId, DefInfo, DefKind, DefinitionRegistry, PackageId, PackageInfo, REGISTRY_SCHEMA_VERSION,
    Visibility,
};
pub use resolve::{
    BinderInfo, EmittedOperation, OperationKind, Resolution, ResolvedFragment, ResolvedRef,
};
pub use token::{
    Delim, GeneratedToken, HostToken, Origin, Quotation, QuoteItem, QuoteLimits, ReportedSpan,
    TokenKind,
};
