//! Optional conditional fragment approval over a real raw quotation execution.
//! This is not a source compiler or ExpandedChecked constructor. The supplied
//! registry/graph/policies are trusted host context, freshly checked every use.
use crate::{
    ExecOptions, MacroKind, MacroRequest,
    quote_key::PROFILE,
    quote_runner::{QuoteRunner, RawExecution},
};
use std::ops::Range;
use vesper_forge::quote_transport as adapter;
use vesper_forge::typed_quote::{ExprInput, ExprSource, TypedBudget, TypedError};
use vesper_forge::{
    AcceptedExpansion, AuthorityGraph, AuthorityRejection, DefId, HygieneHost, InvocationSite,
    PolicySet, check_authority,
};
use vesper_loom_cas::{Domain, ObjectId, blob_id};
use vesper_loom_sandbox::quote_transport as q;
#[derive(Debug)]
pub struct ComponentBinding {
    macro_def: DefId,
    component: ObjectId,
    kind: MacroKind,
    entry: String,
    profile: &'static str,
}
impl ComponentBinding {
    /// A trusted host binding, not a guest-provided attestation. It is separate
    /// from flat bindings and permanently bound to the structured profile.
    pub fn new(macro_def: DefId, request: &MacroRequest) -> Result<Self, Error> {
        if request.component.len() > 16 << 20
            || request.entry.is_empty()
            || request.entry.len() > 256
            || request.entry.chars().any(char::is_control)
        {
            return Err(Error::Binding);
        }
        Ok(Self {
            macro_def,
            component: blob_id(Domain::Artifact, &request.component),
            kind: request.kind,
            entry: request.entry.clone(),
            profile: PROFILE,
        })
    }
    pub fn macro_def(&self) -> DefId {
        self.macro_def
    }
    pub fn profile(&self) -> &str {
        self.profile
    }
    fn check(&self, request: &MacroRequest) -> Result<(), Error> {
        if request.component.len() > 16 << 20
            || self.profile != PROFILE
            || self.kind != request.kind
            || self.entry != request.entry
            || self.component != blob_id(Domain::Artifact, &request.component)
        {
            Err(Error::Binding)
        } else {
            Ok(())
        }
    }
}
#[derive(Debug)]
pub enum Error {
    Binding,
    Runner(crate::quote_runner::Error),
    Export(adapter::Error),
    InputSource,
    Typed(TypedError),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "conditional quotation fragment {self:?}")
    }
}
impl std::error::Error for Error {}
#[derive(Debug)]
pub enum Rejection {
    Typed(adapter::Error),
    Body(TypedError),
    Authority(AuthorityRejection),
}
#[derive(Debug)]
enum Verdict {
    Accepted(AcceptedExpansion),
    Rejected(Rejection),
}
#[derive(Debug)]
pub struct FragmentOutcome {
    execution: RawExecution,
    verdict: Verdict,
}
impl FragmentOutcome {
    pub fn execution(&self) -> &RawExecution {
        &self.execution
    }
    pub fn accepted(&self) -> Option<&AcceptedExpansion> {
        match &self.verdict {
            Verdict::Accepted(v) => Some(v),
            _ => None,
        }
    }
    pub fn rejection(&self) -> Option<&Rejection> {
        match &self.verdict {
            Verdict::Rejected(v) => Some(v),
            _ => None,
        }
    }
}
pub struct QuoteCall<'a> {
    pub binding: &'a ComponentBinding,
    pub request: &'a MacroRequest,
    pub site: InvocationSite,
    pub source: &'a ExprSource,
    pub body: Range<usize>,
    pub invocation: Range<usize>,
    pub inputs: Vec<ExprInput>,
    pub limits: q::Limits,
}
impl std::fmt::Debug for QuoteCall<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuoteCall")
            .field("binding", &self.binding)
            .field("body", &self.body)
            .field("invocation", &self.invocation)
            .field("input_count", &self.inputs.len())
            .finish_non_exhaustive()
    }
}
/// The same fresh import, whole-body resolution and authority gate run after a
/// miss OR hit. Only the raw execution is cached. No resolved fragment is reused.
pub fn expand_fragment(
    runner: &QuoteRunner,
    host: &mut HygieneHost,
    call: QuoteCall<'_>,
    graph: &AuthorityGraph,
    policies: &PolicySet,
    budget: &mut TypedBudget,
    options: ExecOptions,
) -> Result<FragmentOutcome, Error> {
    call.binding.check(call.request)?;
    if call
        .source
        .original_source()
        .get(call.body.clone())
        .is_none()
        || call.body.start > call.invocation.start
        || call.invocation.end > call.body.end
        || call
            .source
            .original_source()
            .get(call.invocation.clone())
            .is_none()
    {
        return Err(Error::InputSource);
    }
    for input in &call.inputs {
        budget
            .charge_work(call.source.original_source().len())
            .map_err(Error::Typed)?;
        if input.original_source() != call.source.original_source() {
            return Err(Error::InputSource);
        }
    }
    let expansion = host
        .begin_expr_expansion(call.binding.macro_def, call.site, call.inputs, budget)
        .map_err(Error::Typed)?;
    let kind = match call.request.kind {
        MacroKind::ProcMacro => q::MacroKind::ProcMacro,
        MacroKind::DeriveMacro => q::MacroKind::DeriveMacro,
    };
    let invocation = match adapter::invocation_for_expr(
        host,
        &expansion,
        kind,
        &call.request.entry,
        &call.request.args,
        call.limits,
        budget,
    ) {
        Ok(v) => v,
        Err(error) => {
            let _ = host.abandon_expr(expansion);
            return Err(Error::Export(error));
        }
    };
    let execution = match runner
        .prepare(call.request, &invocation, call.limits)
        .and_then(|p| runner.execute(&p, options))
    {
        Ok(v) => v,
        Err(error) => {
            let _ = host.abandon_expr(expansion);
            return Err(Error::Runner(error));
        }
    };
    let expanded = match adapter::finish_expr_outcome(
        host,
        expansion,
        &execution.record.outcome,
        call.limits,
        budget,
    ) {
        Ok(v) => v,
        Err(error) => {
            return Ok(FragmentOutcome {
                execution,
                verdict: Verdict::Rejected(Rejection::Typed(error)),
            });
        }
    };
    let body = match host.resolve_expr_body(
        call.source,
        call.body,
        vec![(call.invocation, expanded)],
        budget,
    ) {
        Ok(v) => v,
        Err(error) => {
            return Ok(FragmentOutcome {
                execution,
                verdict: Verdict::Rejected(Rejection::Body(error)),
            });
        }
    };
    let verdict = match check_authority(host.session(), body.into_fragment(), graph, policies) {
        Ok(v) => Verdict::Accepted(v),
        Err(e) => Verdict::Rejected(Rejection::Authority(e)),
    };
    Ok(FragmentOutcome { execution, verdict })
}
