//! The approval layer: trusted approval context, per-session hygiene host
//! and the fresh import/resolve/authority gate run on every use of a raw
//! execution, hit or miss.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use vesper_forge::{
    AcceptedExpansion, AuthorityGraph, AuthorityRejection, CallerCeiling, DefId,
    DefinitionRegistry, ExpansionId, ForgeError, HostToken, HygieneHost, IdentityLimits,
    InvocationSite, MacroPolicy, PackageId, PolicySet, QuoteLimits, ScopeId, SessionId,
    check_authority,
};
use vesper_loom_cas::{DerivationKey, Domain, ObjectId, blob_id};
use vesper_loom_sandbox::Diagnostic;

use crate::error::RunnerError;
use crate::request::{MacroKind, MacroRequest};
use crate::runner::{CacheStatus, ExecOptions, MacroRunner};

/// Version of the canonical approval-context encoding.
pub const APPROVAL_CONTEXT_SCHEMA_VERSION: u32 = 1;

/// A name brought into a package's top-level environment
/// ([`HygieneHost::declare_global`]).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalDecl {
    pub package: PackageId,
    pub name: String,
    pub def: DefId,
}

/// Trusted statement that `macro_def` is implemented by exactly this
/// component, kind and entry. Without a matching binding the runner never
/// executes or approves a component under that macro's policy.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MacroBinding {
    pub macro_def: DefId,
    /// CAS identity (Artifact blob) of the component bytes.
    pub component: String,
    pub kind: MacroKind,
    pub entry: String,
}

impl MacroBinding {
    pub fn for_component(
        macro_def: DefId,
        component: &[u8],
        kind: MacroKind,
        entry: impl Into<String>,
    ) -> Self {
        Self {
            macro_def,
            component: blob_id(Domain::Artifact, component).to_string(),
            kind,
            entry: entry.into(),
        }
    }
}

/// Every trusted input that resolution and the authority gate depend on.
/// Its identity is a digest of its complete canonical content, so changing
/// any definition, environment, graph edge, policy, ceiling or binding
/// changes it even when every version label stays the same.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalContext {
    pub registry: DefinitionRegistry,
    pub globals: Vec<GlobalDecl>,
    pub graph: AuthorityGraph,
    pub macro_policies: Vec<MacroPolicy>,
    pub ceilings: Vec<CallerCeiling>,
    pub bindings: Vec<MacroBinding>,
    pub identity_limits: IdentityLimits,
    pub quote_limits: QuoteLimits,
}

/// Digest of an [`ApprovalContext`]'s canonical encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ApprovalContextId(pub [u8; 32]);

impl fmt::Display for ApprovalContextId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

/// An approval context that cannot be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextError {
    DuplicateMacroPolicy(DefId),
    DuplicateCeiling(PackageId),
    DuplicateBinding(DefId),
    /// A global declaration the hygiene host refused.
    Global {
        decl: GlobalDecl,
        error: ForgeError,
    },
}

impl fmt::Display for ContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ContextError {}

#[derive(Serialize)]
struct ContextWire<'a> {
    schema_version: u32,
    registry: &'a DefinitionRegistry,
    globals: Vec<&'a GlobalDecl>,
    graph: &'a AuthorityGraph,
    macro_policies: Vec<&'a MacroPolicy>,
    ceilings: Vec<&'a CallerCeiling>,
    bindings: Vec<&'a MacroBinding>,
    identity_limits: &'a IdentityLimits,
    quote_limits: &'a QuoteLimits,
}

impl ApprovalContext {
    pub fn new(registry: DefinitionRegistry, graph: AuthorityGraph) -> Self {
        Self {
            registry,
            globals: Vec::new(),
            graph,
            macro_policies: Vec::new(),
            ceilings: Vec::new(),
            bindings: Vec::new(),
            identity_limits: IdentityLimits::default(),
            quote_limits: QuoteLimits::default(),
        }
    }

    fn check_unique(&self) -> Result<(), ContextError> {
        let mut seen = std::collections::BTreeSet::new();
        for p in &self.macro_policies {
            if !seen.insert(p.macro_def) {
                return Err(ContextError::DuplicateMacroPolicy(p.macro_def));
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for c in &self.ceilings {
            if !seen.insert(c.package) {
                return Err(ContextError::DuplicateCeiling(c.package));
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for b in &self.bindings {
            if !seen.insert(b.macro_def) {
                return Err(ContextError::DuplicateBinding(b.macro_def));
            }
        }
        Ok(())
    }

    /// Canonical encoding: schema-versioned JSON with every collection in
    /// a defined order. Duplicated policies, ceilings or bindings (which a
    /// map would silently collapse) are rejected.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ContextError> {
        self.check_unique()?;
        let mut globals: Vec<_> = self.globals.iter().collect();
        globals.sort();
        let mut macro_policies: Vec<_> = self.macro_policies.iter().collect();
        macro_policies.sort_by_key(|p| p.macro_def);
        let mut ceilings: Vec<_> = self.ceilings.iter().collect();
        ceilings.sort_by_key(|c| c.package);
        let mut bindings: Vec<_> = self.bindings.iter().collect();
        bindings.sort();
        let wire = ContextWire {
            schema_version: APPROVAL_CONTEXT_SCHEMA_VERSION,
            registry: &self.registry,
            globals,
            graph: &self.graph,
            macro_policies,
            ceilings,
            bindings,
            identity_limits: &self.identity_limits,
            quote_limits: &self.quote_limits,
        };
        Ok(serde_json::to_vec(&wire).expect("approval context serializes"))
    }

    pub fn id(&self) -> Result<ApprovalContextId, ContextError> {
        let bytes = self.canonical_bytes()?;
        let mut h = blake3::Hasher::new_derive_key("vesper-forge-runner 2026-11 approval-context");
        h.update(&bytes);
        Ok(ApprovalContextId(*h.finalize().as_bytes()))
    }

    /// Starts a fresh hygiene session over a private copy of this context.
    /// Every scope, expansion and symbol identity is minted by it; none is
    /// read from a cache.
    pub fn open_session(&self, session: SessionId) -> Result<ForgeSession, ContextError> {
        let id = self.id()?;
        let mut host = HygieneHost::with_limits(
            session,
            self.registry.clone(),
            self.identity_limits,
            self.quote_limits,
        );
        let mut globals = self.globals.clone();
        globals.sort();
        for g in &globals {
            host.declare_global(g.package, &g.name, g.def)
                .map_err(|error| ContextError::Global {
                    decl: g.clone(),
                    error,
                })?;
        }
        let mut policies = PolicySet::new();
        for p in &self.macro_policies {
            policies = policies.macro_policy(p.clone());
        }
        for c in &self.ceilings {
            policies = policies.ceiling(c.clone());
        }
        Ok(ForgeSession {
            host,
            graph: self.graph.clone(),
            policies,
            bindings: self
                .bindings
                .iter()
                .map(|b| (b.macro_def, b.clone()))
                .collect(),
            context_id: id,
        })
    }
}

/// One macro invocation to expand in a session.
#[derive(Clone, Debug)]
pub struct MacroCall<'a> {
    pub macro_def: DefId,
    pub site: InvocationSite,
    /// The caller's input syntax (host tokens of this session). It is sent
    /// to the guest as text, kinds and display spans only.
    pub input: Vec<HostToken>,
    /// Surrounding host tokens the expansion is resolved within.
    pub before: Vec<HostToken>,
    pub after: Vec<HostToken>,
    pub request: &'a MacroRequest,
}

/// Execution half of a [`MacroOutcome`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionSummary {
    pub status: CacheStatus,
    pub executed: bool,
    pub newly_recorded: bool,
    pub key: DerivationKey,
    /// CAS identity of the raw, untrusted record.
    pub raw_output: ObjectId,
    pub fuel_consumed: u64,
    pub peak_memory_bytes: u64,
    /// Guest notes and warnings (untrusted display text).
    pub diagnostics: Vec<Diagnostic>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// An approval computed in this session from the current context. It is
/// never stored by this crate and is not a reusable authorization: it is
/// conditional on the context named by `approval_context` and on the
/// truthfulness of that context's trusted graph and registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FreshApproval {
    /// Expansion output tokens (all macro-originated).
    pub tokens: Vec<HostToken>,
    pub accepted: AcceptedExpansion,
}

/// Why an expansion was not accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rejection {
    /// The context binds no (or a different) component to the macro.
    UnboundComponent {
        macro_def: DefId,
        reason: String,
    },
    /// Begin/import/resolution failure, including forged input tokens,
    /// invalid claims and unbound or ambiguous names.
    Hygiene(ForgeError),
    Authority(AuthorityRejection),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Accepted(Box<FreshApproval>),
    Rejected(Rejection),
}

/// Result of [`ForgeSession::expand`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MacroOutcome {
    pub session: SessionId,
    pub approval_context: ApprovalContextId,
    /// The expansion minted for this call (closed whatever the verdict).
    pub expansion: Option<ExpansionId>,
    /// `None` when the call was rejected before execution.
    pub execution: Option<ExecutionSummary>,
    pub verdict: Verdict,
}

impl MacroOutcome {
    pub fn accepted(&self) -> Option<&FreshApproval> {
        match &self.verdict {
            Verdict::Accepted(a) => Some(a),
            Verdict::Rejected(_) => None,
        }
    }

    pub fn rejection(&self) -> Option<&Rejection> {
        match &self.verdict {
            Verdict::Rejected(r) => Some(r),
            Verdict::Accepted(_) => None,
        }
    }
}

/// A hygiene session bound to one approval context.
///
/// The hygiene host is owned privately so that its environment cannot drift
/// from the context the session was opened with.
#[derive(Debug)]
pub struct ForgeSession {
    host: HygieneHost,
    graph: AuthorityGraph,
    policies: PolicySet,
    bindings: BTreeMap<DefId, MacroBinding>,
    context_id: ApprovalContextId,
}

impl ForgeSession {
    pub fn session(&self) -> SessionId {
        self.host.session()
    }

    pub fn approval_context(&self) -> ApprovalContextId {
        self.context_id
    }

    pub fn host(&self) -> &HygieneHost {
        &self.host
    }

    /// Lexes trusted source text of `package` into this session's tokens.
    pub fn lex_source(
        &mut self,
        package: PackageId,
        src: &str,
    ) -> Result<Vec<HostToken>, ForgeError> {
        self.host.lex_source(package, src)
    }

    pub fn module_scope(&mut self, package: PackageId) -> Result<ScopeId, ForgeError> {
        self.host.module_scope(package)
    }

    fn outcome(
        &self,
        expansion: Option<ExpansionId>,
        execution: Option<ExecutionSummary>,
        verdict: Verdict,
    ) -> MacroOutcome {
        MacroOutcome {
            session: self.session(),
            approval_context: self.context_id,
            expansion,
            execution,
            verdict,
        }
    }

    /// Expands one macro call:
    ///
    /// 1. the context must bind exactly this component, kind and entry to
    ///    `macro_def` (checked before anything executes);
    /// 2. a fresh expansion (introduction scope, splice arena) is minted;
    ///    the input tokens must be valid tokens of this session;
    /// 3. the raw execution is obtained from the runner (verified hit or
    ///    sandboxed run); its output is untrusted;
    /// 4. the flat output is imported as macro-originated tokens (guest
    ///    scopes dropped, spans display-only, claims unvalidated), resolved
    ///    with the surrounding tokens against the current registry and
    ///    environments, and checked against the current graph, emits
    ///    policies and caller ceilings.
    ///
    /// Infrastructure failures (sandbox, guest failure, corrupt cache) are
    /// `Err`; hygiene and authority outcomes are a [`Verdict`]. In every
    /// non-accepted case the minted expansion is closed as failed.
    pub fn expand(
        &mut self,
        runner: &MacroRunner,
        call: MacroCall<'_>,
        options: ExecOptions,
    ) -> Result<MacroOutcome, RunnerError> {
        let req = call.request;
        let expected =
            MacroBinding::for_component(call.macro_def, &req.component, req.kind, &req.entry);
        match self.bindings.get(&call.macro_def) {
            Some(b) if *b == expected => {}
            found => {
                let reason = match found {
                    None => "no component is bound to this macro".to_owned(),
                    Some(b) => format!(
                        "bound to {} {} `{}`, request is {} {} `{}`",
                        b.component,
                        b.kind.as_str(),
                        b.entry,
                        expected.component,
                        expected.kind.as_str(),
                        expected.entry
                    ),
                };
                return Ok(self.outcome(
                    None,
                    None,
                    Verdict::Rejected(Rejection::UnboundComponent {
                        macro_def: call.macro_def,
                        reason,
                    }),
                ));
            }
        }

        let expansion =
            match self
                .host
                .begin_expansion(call.macro_def, call.site, vec![call.input.clone()])
            {
                Ok(x) => x,
                Err(e) => {
                    return Ok(self.outcome(None, None, Verdict::Rejected(Rejection::Hygiene(e))));
                }
            };
        let id = expansion.id;
        let invocation = self
            .host
            .invocation_for(req.kind.step(), &req.entry, &call.input);
        let raw = match runner
            .prepare(req, invocation.tokens)
            .and_then(|p| runner.execute(&p, options))
        {
            Ok(r) => r,
            Err(e) => {
                let _ = self.host.abandon(id);
                return Err(e);
            }
        };
        let summary = ExecutionSummary {
            status: raw.status,
            executed: raw.executed,
            newly_recorded: raw.newly_recorded,
            key: raw.key,
            raw_output: raw.record_id,
            fuel_consumed: raw.record.fuel_consumed,
            peak_memory_bytes: raw.record.peak_memory_bytes,
            diagnostics: raw.record.outcome.diagnostics.clone(),
            stdout: raw.record.stdout.clone(),
            stderr: raw.record.stderr.clone(),
        };

        let tokens =
            match self
                .host
                .finish_flat_outcome(id, &raw.record.outcome, &req.limits.transport)
            {
                Ok(t) => t,
                Err(e) => {
                    return Ok(self.outcome(
                        Some(id),
                        Some(summary),
                        Verdict::Rejected(Rejection::Hygiene(e)),
                    ));
                }
            };
        let mut program = call.before;
        program.extend(tokens.iter().cloned());
        program.extend(call.after);
        let fragment = match self.host.resolve(program) {
            Ok(f) => f,
            Err(e) => {
                return Ok(self.outcome(
                    Some(id),
                    Some(summary),
                    Verdict::Rejected(Rejection::Hygiene(e)),
                ));
            }
        };
        let verdict = match check_authority(self.session(), fragment, &self.graph, &self.policies) {
            Ok(accepted) => Verdict::Accepted(Box::new(FreshApproval { tokens, accepted })),
            Err(r) => Verdict::Rejected(Rejection::Authority(r)),
        };
        Ok(self.outcome(Some(id), Some(summary), verdict))
    }
}
