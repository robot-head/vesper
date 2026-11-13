//! Explicit Tier1 source expansion. Only the fully checked expanded artifact
//! carries semantic authority; original macro source and provenance do not.
pub mod authority;
pub mod lower;

use crate::{
    ast::{Diagnostic, Item, Span},
    ir, ir_check,
    tooling::frontend::{AnalysisFailure, AnalysisLimits, CheckedSource, check_source},
};
use authority::{AuthorityLimits, SourceBindings};
use lower::{LowerLimits, LoweredBody, SegmentOrigin};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    sync::Arc,
};
use vesper_forge::{
    AuthorityAudit, AuthorityGrant, CallerCeiling, ExpansionId, HygieneHost, InvocationSite,
    MacroPolicy, OperationKind, Origin, PolicySet, SessionId, check_authority,
    typed_quote::{ResolvedBody, TypedBudget, TypedLimits},
};
use vesper_syntax::macro_source::{self, FunctionId, MacroSource};

pub const SCHEMA: &str = "vesper.forge.source/1";
pub const POLICY_SCHEMA: &str = "vesper.forge.policy/1";
const STAGE_WORK: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MacroGrant {
    pub name: String,
    pub effects: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyConfig {
    pub schema: String,
    #[serde(default)]
    pub macro_emits: Vec<MacroGrant>,
    #[serde(default)]
    pub caller_ceiling: Vec<String>,
}
impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            schema: POLICY_SCHEMA.into(),
            macro_emits: vec![],
            caller_ceiling: vec![],
        }
    }
}
#[derive(Clone, Debug)]
pub struct CompileConfig {
    pub package: String,
    pub policy: PolicyConfig,
}
impl Default for CompileConfig {
    fn default() -> Self {
        Self {
            package: "@local/source".into(),
            policy: PolicyConfig::default(),
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct CompileLimits {
    pub max_source_bytes: usize,
    pub max_expanded_bytes: usize,
    pub max_map_entries: usize,
    pub max_metadata_bytes: usize,
    pub max_identifier_bytes: usize,
    pub max_work: usize,
}
impl Default for CompileLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: 128 * 1024,
            max_expanded_bytes: 256 * 1024,
            max_map_entries: 32768,
            max_metadata_bytes: 2 * 1024 * 1024,
            max_identifier_bytes: 256,
            max_work: 128 * 1024 * 1024,
        }
    }
}
impl CompileLimits {
    pub const HARD: Self = Self {
        max_source_bytes: 256 * 1024,
        max_expanded_bytes: 256 * 1024,
        max_map_entries: 65536,
        max_metadata_bytes: 8 * 1024 * 1024,
        max_identifier_bytes: 1024,
        max_work: 512 * 1024 * 1024,
    };
    pub fn validate(self) -> Result<(), CompileFailure> {
        let h = Self::HARD;
        for (n, cap) in [
            (self.max_source_bytes, h.max_source_bytes),
            (self.max_expanded_bytes, h.max_expanded_bytes),
            (self.max_map_entries, h.max_map_entries),
            (self.max_metadata_bytes, h.max_metadata_bytes),
            (self.max_identifier_bytes, h.max_identifier_bytes),
            (self.max_work, h.max_work),
        ] {
            if n == 0 || n > cap {
                return Err(failure(Stage::Config, "invalid source compiler limits"));
            }
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Parse,
    Expand,
    Resolve,
    Lower,
    Check,
    Authority,
    Ir,
    Resource,
    Unsupported,
    Config,
    Io,
}
/// A rejected candidate is diagnostic data, never a partly accepted artifact.
#[derive(Debug)]
pub struct CompileFailure {
    stage: Stage,
    message: String,
    diagnostics: Vec<Diagnostic>,
    expanded_source: Option<Arc<str>>,
}
impl CompileFailure {
    pub fn stage(&self) -> Stage {
        self.stage
    }
    pub fn message(&self) -> &str {
        &self.message
    }
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
    pub fn expanded_source(&self) -> Option<&str> {
        self.expanded_source.as_deref()
    }
}
impl std::fmt::Display for CompileFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for CompileFailure {}
fn failure(stage: Stage, message: impl AsRef<str>) -> CompileFailure {
    let message = message.as_ref();
    let mut end = message.len().min(4096);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    CompileFailure {
        stage,
        message: message[..end].to_owned(),
        diagnostics: vec![],
        expanded_source: None,
    }
}
fn stage_error(stage: Stage, error: impl std::fmt::Display) -> CompileFailure {
    failure(stage, error.to_string())
}
fn authority_error(stage: Stage, error: authority::AuthorityFailure) -> CompileFailure {
    let stage = match &error {
        authority::AuthorityFailure::Resource(_) => Stage::Resource,
        authority::AuthorityFailure::InvalidPackage => Stage::Config,
        authority::AuthorityFailure::Unsupported(_) => Stage::Unsupported,
        _ => stage,
    };
    stage_error(stage, error)
}
fn typed_error(stage: Stage, error: vesper_forge::typed_quote::TypedError) -> CompileFailure {
    use vesper_forge::typed_quote::TypedError;
    let stage = match &error {
        TypedError::Budget(_) => Stage::Resource,
        TypedError::InvalidLimits => Stage::Config,
        TypedError::Unsupported => Stage::Unsupported,
        _ => stage,
    };
    stage_error(stage, error)
}
fn lower_error(error: lower::LowerError) -> CompileFailure {
    let stage = match &error {
        lower::LowerError::Budget(_) | lower::LowerError::Allocation => Stage::Resource,
        lower::LowerError::InvalidLimits => Stage::Config,
        lower::LowerError::Unsupported(_) => Stage::Unsupported,
        _ => Stage::Lower,
    };
    stage_error(stage, error)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}
impl From<&Span> for ByteRange {
    fn from(s: &Span) -> Self {
        Self {
            start: s.start,
            end: s.end,
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MapKind {
    Source,
    Generated,
}
#[derive(Debug, Serialize)]
pub struct SourceMapSegment {
    pub expanded: ByteRange,
    pub original: ByteRange,
    pub kind: MapKind,
    pub r#macro: Option<String>,
}
/// Each invocation names its real whole-function audit. Shared audit contents
/// deliberately cover all contributing expansions in that function.
#[derive(Debug, Serialize)]
pub struct InvocationAudit {
    pub r#macro: String,
    pub invocation: ByteRange,
    pub scope: &'static str,
    pub audit: Arc<AuthorityAudit>,
}
#[derive(Debug)]
pub struct ExpandedChecked {
    original: Arc<str>,
    original_hash: [u8; 32],
    package: String,
    checked: CheckedSource,
    program: ir::Program,
    map: Vec<SourceMapSegment>,
    audits: Vec<InvocationAudit>,
    definitions: usize,
    invocations: usize,
}
impl ExpandedChecked {
    pub fn original_source(&self) -> &str {
        &self.original
    }
    pub fn expanded_source(&self) -> &str {
        self.checked.source()
    }
    pub fn original_hash(&self) -> [u8; 32] {
        self.original_hash
    }
    pub fn expanded_hash(&self) -> [u8; 32] {
        self.checked.source_hash()
    }
    pub fn package(&self) -> &str {
        &self.package
    }
    pub fn checked(&self) -> &CheckedSource {
        &self.checked
    }
    pub fn ir(&self) -> &ir::Program {
        &self.program
    }
    pub fn source_map(&self) -> &[SourceMapSegment] {
        &self.map
    }
    pub fn audits(&self) -> &[InvocationAudit] {
        &self.audits
    }
    pub fn macro_definitions(&self) -> usize {
        self.definitions
    }
    pub fn invocations(&self) -> usize {
        self.invocations
    }
}
struct Work {
    remaining: usize,
}
impl Work {
    fn charge(&mut self, n: usize) -> Result<(), CompileFailure> {
        self.remaining = self
            .remaining
            .checked_sub(n)
            .ok_or_else(|| failure(Stage::Resource, "source macro aggregate work exceeded"))?;
        Ok(())
    }
    fn reserve(&mut self) -> Result<usize, CompileFailure> {
        self.charge(STAGE_WORK)?;
        Ok(STAGE_WORK)
    }
}
fn mul(a: usize, b: usize) -> Result<usize, CompileFailure> {
    a.checked_mul(b)
        .ok_or_else(|| failure(Stage::Resource, "source macro size overflow"))
}
struct SizeLimit {
    remaining: usize,
    used: usize,
}
impl io::Write for SizeLimit {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.remaining = self
            .remaining
            .checked_sub(bytes.len())
            .ok_or_else(|| io::Error::other("macro metadata budget"))?;
        self.used = self
            .used
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("macro metadata overflow"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn encoded_size(value: &impl Serialize, cap: usize) -> Result<usize, CompileFailure> {
    let mut sink = SizeLimit {
        remaining: cap,
        used: 0,
    };
    serde_json::to_writer(&mut sink, value)
        .map_err(|_| failure(Stage::Resource, "source macro metadata exceeded"))?;
    Ok(sink.used)
}
fn validate_config(config: &CompileConfig, limits: CompileLimits) -> Result<(), CompileFailure> {
    if config.package.is_empty()
        || config.package.len() > 256
        || config.policy.schema != POLICY_SCHEMA
        || config.policy.macro_emits.len() > 128
        || config.policy.caller_ceiling.len() > 128
    {
        return Err(failure(Stage::Config, "invalid source macro configuration"));
    }
    let mut names = BTreeSet::new();
    for m in &config.policy.macro_emits {
        if m.name.is_empty()
            || m.name.len() > limits.max_identifier_bytes
            || !names.insert(m.name.as_str())
        {
            return Err(failure(Stage::Config, "duplicate or invalid macro policy"));
        }
        validate_grants(&m.effects)?;
    }
    validate_grants(&config.policy.caller_ceiling)?;
    encoded_size(&config.policy, 64 * 1024)?;
    Ok(())
}
fn validate_grants(grants: &[String]) -> Result<(), CompileFailure> {
    if grants.len() > 128 {
        return Err(failure(Stage::Config, "too many effect grants"));
    }
    let mut seen = BTreeSet::new();
    for name in grants {
        if name.is_empty() || name.len() > 256 || !seen.insert(name.as_str()) {
            return Err(failure(Stage::Config, "duplicate or invalid effect grant"));
        }
    }
    Ok(())
}
fn policies(
    source: &MacroSource,
    bindings: &SourceBindings,
    config: &CompileConfig,
) -> Result<PolicySet, CompileFailure> {
    let grants = |names: &[String]| -> Result<Vec<AuthorityGrant>, CompileFailure> {
        names
            .iter()
            .map(|name| {
                bindings
                    .effect_id(name)
                    .map(AuthorityGrant::AnyArguments)
                    .map_err(|e| stage_error(Stage::Config, e))
            })
            .collect()
    };
    let mut set = PolicySet::new().ceiling(CallerCeiling {
        package: bindings.source_package(),
        version: 1,
        grants: grants(&config.policy.caller_ceiling)?,
    });
    for configured in &config.policy.macro_emits {
        if !source
            .definitions()
            .iter()
            .any(|d| source.source()[d.name_span.clone()] == configured.name)
        {
            return Err(failure(Stage::Config, "policy names an unknown macro"));
        }
    }
    for definition in source.definitions() {
        let name = &source.source()[definition.name_span.clone()];
        let configured = config.policy.macro_emits.iter().find(|m| m.name == name);
        let emits = match configured {
            Some(m) => grants(&m.effects)?,
            None => vec![],
        };
        set = set.macro_policy(MacroPolicy {
            macro_def: bindings
                .macro_def(definition.id)
                .map_err(|e| stage_error(Stage::Config, e))?,
            version: 1,
            emits,
        });
    }
    Ok(set)
}
struct InvocationInfo {
    name: String,
    span: Span,
    function: FunctionId,
}
struct Body {
    function: FunctionId,
    name: String,
    resolved: ResolvedBody,
    lowered: LoweredBody,
}
struct Edit {
    span: Span,
    body: Option<usize>,
}
fn valid_span(text: &str, span: &Span) -> bool {
    span.start <= span.end && text.get(span.clone()).is_some()
}
fn append_map(
    map: &mut Vec<SourceMapSegment>,
    entry: SourceMapSegment,
    limits: CompileLimits,
    original: &str,
    expanded: &str,
    work: &mut Work,
) -> Result<(), CompileFailure> {
    work.charge(std::mem::size_of::<SourceMapSegment>())?;
    if map.len() >= limits.max_map_entries
        || !valid_span(original, &(entry.original.start..entry.original.end))
        || !valid_span(expanded, &(entry.expanded.start..entry.expanded.end))
    {
        return Err(failure(Stage::Resource, "source macro map bounds exceeded"));
    }
    if let Some(name) = &entry.r#macro {
        work.charge(name.len())?;
    }
    map.push(entry);
    Ok(())
}
fn render(
    source: &MacroSource,
    bodies: &[Body],
    origins: &BTreeMap<ExpansionId, InvocationInfo>,
    limits: CompileLimits,
    work: &mut Work,
) -> Result<(Arc<str>, Vec<SourceMapSegment>), CompileFailure> {
    let edit_count = source
        .definitions()
        .len()
        .checked_add(bodies.len())
        .ok_or_else(|| failure(Stage::Resource, "source edit count overflow"))?;
    work.charge(mul(edit_count, std::mem::size_of::<Edit>())?)?;
    let mut edits = Vec::new();
    edits
        .try_reserve_exact(edit_count)
        .map_err(|_| failure(Stage::Resource, "source edit allocation failed"))?;
    edits.extend(source.definitions().iter().map(|d| Edit {
        span: d.span.clone(),
        body: None,
    }));
    for (index, body) in bodies.iter().enumerate() {
        edits.push(Edit {
            span: source
                .function(body.function)
                .map_err(|e| stage_error(Stage::Lower, e))?
                .body_span
                .clone(),
            body: Some(index),
        });
    }
    edits.sort_by_key(|e| e.span.start);
    let mut length = source.source().len();
    let mut end = 0usize;
    for edit in &edits {
        work.charge(1)?;
        if !valid_span(source.source(), &edit.span) || edit.span.start < end {
            return Err(failure(Stage::Lower, "overlapping macro source edits"));
        }
        end = edit.span.end;
        length = length
            .checked_sub(edit.span.len())
            .and_then(|n| n.checked_add(edit.body.map_or(0, |i| bodies[i].lowered.source().len())))
            .ok_or_else(|| failure(Stage::Resource, "expanded source size overflow"))?;
    }
    if length > limits.max_expanded_bytes {
        return Err(failure(
            Stage::Resource,
            "expanded source byte limit exceeded",
        ));
    }
    work.charge(length)?;
    let map_count = bodies.iter().try_fold(
        edits
            .len()
            .checked_add(1)
            .ok_or_else(|| failure(Stage::Resource, "source map count overflow"))?,
        |n, b| {
            n.checked_add(b.lowered.segments().len())
                .ok_or_else(|| failure(Stage::Resource, "source map count overflow"))
        },
    )?;
    if map_count > limits.max_map_entries {
        return Err(failure(Stage::Resource, "source map count exceeded"));
    }
    work.charge(mul(
        map_count,
        std::mem::size_of::<(Span, Span, MapKind, Option<String>)>(),
    )?)?;
    let mut output = String::new();
    output
        .try_reserve_exact(length)
        .map_err(|_| failure(Stage::Resource, "expanded source allocation failed"))?;
    let mut cursor = 0;
    let mut raw_maps = Vec::new();
    raw_maps
        .try_reserve_exact(map_count)
        .map_err(|_| failure(Stage::Resource, "source map allocation failed"))?;
    for edit in edits {
        if cursor < edit.span.start {
            let at = output.len();
            output.push_str(&source.source()[cursor..edit.span.start]);
            raw_maps.push((
                at..output.len(),
                cursor..edit.span.start,
                MapKind::Source,
                None,
            ));
        }
        if let Some(index) = edit.body {
            let body = &bodies[index].lowered;
            let offset = output.len();
            output.push_str(body.source());
            for segment in body.segments() {
                work.charge(1)?;
                if !valid_span(body.source(), &segment.span) {
                    return Err(failure(Stage::Lower, "invalid lowered source map"));
                }
                let (original, kind, name) = match &segment.origin {
                    SegmentOrigin::Source { span } => (span.clone(), MapKind::Source, None),
                    SegmentOrigin::Generated { expansion } => {
                        let site = origins.get(expansion).ok_or_else(|| {
                            failure(Stage::Lower, "unknown generated source origin")
                        })?;
                        work.charge(site.name.len())?;
                        (
                            site.span.clone(),
                            MapKind::Generated,
                            Some(site.name.clone()),
                        )
                    }
                };
                raw_maps.push((
                    offset + segment.span.start..offset + segment.span.end,
                    original,
                    kind,
                    name,
                ));
            }
        }
        cursor = edit.span.end;
    }
    if cursor < source.source().len() {
        let at = output.len();
        output.push_str(&source.source()[cursor..]);
        raw_maps.push((
            at..output.len(),
            cursor..source.source().len(),
            MapKind::Source,
            None,
        ));
    }
    if output.len() != length || raw_maps.len() > limits.max_map_entries {
        return Err(failure(
            Stage::Resource,
            "expanded source/map limit exceeded",
        ));
    }
    work.charge(mul(
        raw_maps.len(),
        std::mem::size_of::<SourceMapSegment>(),
    )?)?;
    let mut map = Vec::new();
    map.try_reserve_exact(raw_maps.len())
        .map_err(|_| failure(Stage::Resource, "source map allocation failed"))?;
    for (expanded, original, kind, name) in raw_maps {
        append_map(
            &mut map,
            SourceMapSegment {
                expanded: (&expanded).into(),
                original: (&original).into(),
                kind,
                r#macro: name,
            },
            limits,
            source.source(),
            &output,
            work,
        )?;
    }
    encoded_size(&map, limits.max_metadata_bytes)?;
    Ok((Arc::from(output), map))
}
/// No WASI/native execution occurs. The only success is a fully checked normal
/// expanded program with current conditional macro authority audits.
pub fn expand_source(
    source: &str,
    config: &CompileConfig,
    limits: CompileLimits,
) -> Result<ExpandedChecked, CompileFailure> {
    limits.validate()?;
    validate_config(config, limits)?;
    if source.len() > limits.max_source_bytes {
        return Err(failure(
            Stage::Resource,
            "source macro input byte limit exceeded",
        ));
    }
    let mut total = Work {
        remaining: limits.max_work,
    };
    let mut overhead = Work {
        remaining: total.reserve()?,
    };
    let parsed = macro_source::parse(
        source,
        macro_source::Limits {
            max_source_bytes: limits.max_source_bytes,
            max_metadata_bytes: limits.max_metadata_bytes,
            max_work: total.reserve()?,
            ..Default::default()
        },
    )
    .map_err(|e| {
        let mut f = stage_error(
            match e.kind {
                macro_source::ErrorKind::Budget(_) => Stage::Resource,
                macro_source::ErrorKind::Unsupported => Stage::Unsupported,
                _ => Stage::Parse,
            },
            &e,
        );
        if let Some(span) = e.span.as_ref() {
            f.diagnostics
                .push(Diagnostic::new("FORGE_PARSE", e.to_string(), span.clone()));
        }
        f
    })?;
    for token in parsed.tokens() {
        overhead.charge(1)?;
        if matches!(
            token.kind,
            crate::lexer::TokenKind::Lower | crate::lexer::TokenKind::Upper
        ) && token.span.len() > limits.max_identifier_bytes
        {
            return Err(failure(
                Stage::Resource,
                "source macro identifier limit exceeded",
            ));
        }
    }
    let bind_limits = AuthorityLimits {
        max_source_bytes: limits.max_source_bytes,
        max_metadata_bytes: limits.max_metadata_bytes,
        max_work: total.reserve()?,
        ..Default::default()
    };
    let bindings = authority::bind_source(&parsed, &config.package, bind_limits)
        .map_err(|e| authority_error(Stage::Unsupported, e))?;
    let policy = policies(&parsed, &bindings, config)?;
    overhead.charge(limits.max_metadata_bytes)?; // bounded registry clone allowance
    encoded_size(bindings.registry(), limits.max_metadata_bytes)?;
    let mut host = HygieneHost::new(SessionId(1), bindings.registry().clone());
    for name in bindings.global_names() {
        overhead.charge(name.len())?;
        if let Some(def) = bindings.def_id(name) {
            host.declare_global(bindings.source_package(), name, def)
                .map_err(|e| stage_error(Stage::Resolve, e))?;
        }
    }
    let mut typed = TypedBudget::new(TypedLimits {
        max_input_bytes: limits.max_source_bytes,
        max_output_bytes: limits.max_expanded_bytes,
        max_metadata_bytes: limits.max_metadata_bytes,
        max_work: total.reserve()?,
        ..Default::default()
    })
    .map_err(|e| stage_error(Stage::Resource, e))?;
    let imported = host
        .import_expr_source(bindings.source_package(), source, &mut typed)
        .map_err(|e| typed_error(Stage::Expand, e))?;
    let mut templates = Vec::new();
    for definition in parsed.definitions() {
        let parameters = definition
            .parameters
            .iter()
            .map(|p| p.name_span.clone())
            .collect::<Vec<_>>();
        let template = host
            .expr_quotation(
                &imported,
                bindings
                    .macro_def(definition.id)
                    .map_err(|e| stage_error(Stage::Resolve, e))?,
                definition.quote_span.clone(),
                &parameters,
                &mut typed,
            )
            .map_err(|e| typed_error(Stage::Expand, e))?;
        templates.push(template);
    }
    let mut lower_work = Work {
        remaining: total.reserve()?,
    };
    let mut bodies = Vec::new();
    let mut origins = BTreeMap::new();
    for function in parsed
        .functions()
        .iter()
        .filter(|f| !f.invocations.is_empty())
    {
        let mut replacements = Vec::new();
        for id in &function.invocations {
            let invocation = parsed
                .invocation(*id)
                .map_err(|e| stage_error(Stage::Parse, e))?;
            let mut inputs = Vec::new();
            for argument in &invocation.arguments {
                inputs.push(
                    host.expr_input(&imported, argument.clone(), &mut typed)
                        .map_err(|e| typed_error(Stage::Expand, e))?,
                );
            }
            let expansion = host
                .begin_expr_expansion(
                    bindings
                        .macro_def(invocation.definition)
                        .map_err(|e| stage_error(Stage::Resolve, e))?,
                    InvocationSite::Source(bindings.source_package()),
                    inputs,
                    &mut typed,
                )
                .map_err(|e| typed_error(Stage::Expand, e))?;
            let expansion_id = expansion.id();
            let definition = parsed
                .definition(invocation.definition)
                .map_err(|e| stage_error(Stage::Parse, e))?;
            overhead.charge(definition.name_span.len() + std::mem::size_of::<InvocationInfo>())?;
            let name = source[definition.name_span.clone()].to_owned();
            origins.insert(
                expansion_id,
                InvocationInfo {
                    name,
                    span: invocation.span.clone(),
                    function: function.id,
                },
            );
            let expression = host
                .finish_expr(
                    expansion,
                    templates[invocation.definition.index()].items(),
                    &mut typed,
                )
                .map_err(|e| typed_error(Stage::Expand, e))?;
            replacements.push((invocation.span.clone(), expression));
        }
        let resolved = host
            .resolve_expr_body(
                &imported,
                function.body_span.clone(),
                replacements,
                &mut typed,
            )
            .map_err(|e| typed_error(Stage::Resolve, e))?;
        let lowered = lower::lower_body(
            &resolved,
            &host,
            &bindings,
            LowerLimits {
                max_output_bytes: limits.max_expanded_bytes,
                max_map_entries: limits.max_map_entries,
                max_identifier_bytes: limits.max_identifier_bytes,
                max_work: lower_work.remaining,
                ..Default::default()
            },
        )
        .map_err(lower_error)?;
        lower_work.charge(lowered.work_used())?;
        bodies.push(Body {
            function: function.id,
            name: function.header.name.clone(),
            resolved,
            lowered,
        });
    }
    let (expanded, map) = render(&parsed, &bodies, &origins, limits, &mut overhead)?;
    total.reserve()?; // complete normal frontend/nominal/type checking allowance
    let checked = check_source(
        &expanded,
        AnalysisLimits {
            max_source_bytes: limits.max_expanded_bytes,
            ..Default::default()
        },
    )
    .map_err(|error| {
        let stage = match &error {
            AnalysisFailure::Resource { .. } => Stage::Resource,
            AnalysisFailure::Unsupported { .. } => Stage::Unsupported,
            _ => Stage::Check,
        };
        CompileFailure {
            stage,
            message: error.to_string(),
            diagnostics: error.diagnostics().to_vec(),
            expanded_source: Some(Arc::clone(&expanded)),
        }
    })?;
    for body in &bodies {
        let function = checked
            .parsed()
            .module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(f) if f.name == body.name => Some(f),
                _ => None,
            })
            .ok_or_else(|| failure(Stage::Lower, "expanded function is missing"))?;
        let actual = function
            .body
            .as_ref()
            .ok_or_else(|| failure(Stage::Lower, "expanded body is missing"))?;
        let comparison_work = mul(
            encoded_size(body.lowered.expected(), limits.max_metadata_bytes)?,
            4,
        )?;
        lower_work.charge(comparison_work)?;
        let matched = body
            .lowered
            .shape_matches(
                actual,
                LowerLimits {
                    max_work: comparison_work,
                    ..Default::default()
                },
            )
            .map_err(lower_error)?;
        if !matched {
            return Err(failure(
                Stage::Lower,
                "expanded AST differs from resolved identity lowering",
            ));
        }
    }
    let derived = authority::derive(
        &checked,
        &bindings,
        AuthorityLimits {
            max_source_bytes: limits.max_expanded_bytes,
            max_metadata_bytes: limits.max_metadata_bytes,
            max_work: total.reserve()?,
            ..Default::default()
        },
    )
    .map_err(|e| authority_error(Stage::Authority, e))?;
    let graph_size = encoded_size(derived.graph(), limits.max_metadata_bytes)?;
    let mut audits = Vec::new();
    for body in bodies {
        for operation in &body.resolved.fragment().operations {
            if operation
                .origins
                .iter()
                .any(|o| matches!(o, Origin::Expansion(_)))
                && let OperationKind::Use { def } = operation.kind
                && derived.unsupported_emitted_def(def)
            {
                return Err(failure(
                    Stage::Unsupported,
                    "macro-generated generic effect or unsupported definition",
                ));
            }
        }
        overhead.charge(mul(
            body.resolved.fragment().operations.len().max(1),
            mul(graph_size.max(1), bind_limits.max_definitions)?,
        )?)?;
        let accepted = check_authority(
            host.session(),
            body.resolved.into_fragment(),
            derived.graph(),
            &policy,
        )
        .map_err(|e| stage_error(Stage::Authority, e))?;
        let audit = Arc::new(accepted.audit);
        for info in origins.values().filter(|i| i.function == body.function) {
            overhead.charge(info.name.len() + std::mem::size_of::<InvocationAudit>())?;
            audits.push(InvocationAudit {
                r#macro: info.name.clone(),
                invocation: (&info.span).into(),
                scope: "expanded_function",
                audit: Arc::clone(&audit),
            });
        }
    }
    encoded_size(&audits, limits.max_metadata_bytes)?;
    total.reserve()?; // actual IR lowering and flow/state-token validation
    let program = ir::lower(checked.checked());
    let diagnostics = ir_check::validate(checked.world(), &program);
    if !diagnostics.is_empty() {
        return Err(CompileFailure {
            stage: Stage::Ir,
            message: "expanded IR validation failed".into(),
            diagnostics,
            expanded_source: Some(expanded),
        });
    }
    Ok(ExpandedChecked {
        original: Arc::from(source),
        original_hash: parsed.source_hash(),
        package: config.package.clone(),
        checked,
        program,
        map,
        audits,
        definitions: parsed.definitions().len(),
        invocations: parsed.invocations().len(),
    })
}
