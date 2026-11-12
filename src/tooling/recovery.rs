//! Diagnostic-only checking of independent complete original-source components.
//! Recovered syntax never mints a CheckedSource, World, signature or certificate.
use super::frontend::{self, AnalysisFailure, AnalysisLimits};
use crate::{ast::*, check, lexer::TokenKind as K, parser};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use vesper_syntax::recovery::{
    self as syntax, DeclarationKind as D, DeclarationStatus as S, RecoverySyntax, SiteKind,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RecoveryLimits {
    pub analysis: AnalysisLimits,
    pub max_functions: usize,
    pub max_context_items: usize,
    /// Aggregate logical staging/type visits; half is reserved for the parser
    /// helper and half for source/context/component work. Not a time/RSS bound.
    pub max_work: usize,
}
impl Default for RecoveryLimits {
    fn default() -> Self {
        Self {
            analysis: AnalysisLimits::default(),
            max_functions: 64,
            max_context_items: 64,
            max_work: 4 * 1024 * 1024,
        }
    }
}
impl RecoveryLimits {
    pub fn validate(self) -> Result<(), RecoveryFailure> {
        self.analysis
            .validate()
            .map_err(|_| resource("invalid_analysis_limits"))?;
        if self.max_functions == 0
            || self.max_functions > 128
            || self.max_context_items == 0
            || self.max_context_items > 128
            || self.max_work == 0
            || self.max_work > 32 * 1024 * 1024
        {
            return Err(resource("invalid_recovery_limits"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveryFailure {
    Resource { reason: &'static str },
    Unsupported { reason: &'static str },
    SourceMismatch,
}
impl std::fmt::Display for RecoveryFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resource { reason } | Self::Unsupported { reason } => f.write_str(reason),
            Self::SourceMismatch => f.write_str("recovery_source_mismatch"),
        }
    }
}
impl std::error::Error for RecoveryFailure {}
fn resource(reason: &'static str) -> RecoveryFailure {
    RecoveryFailure::Resource { reason }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnavailableReason {
    DamagedFunction,
    OpaqueContext,
    IncompleteContext,
    UnsupportedContext,
    DuplicateDeclaration,
    ReservedDeclaration,
    UnknownDependency,
    GlobalFunctionDependency,
    UnsupportedFunction,
    UnsupportedType,
    RecursiveContext,
    AmbiguousContext,
    GuardUnsupported,
    InvalidContext,
}
impl UnavailableReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DamagedFunction => "damaged_function",
            Self::OpaqueContext => "opaque_context",
            Self::IncompleteContext => "incomplete_context",
            Self::UnsupportedContext => "unsupported_context",
            Self::DuplicateDeclaration => "duplicate_declaration",
            Self::ReservedDeclaration => "reserved_declaration",
            Self::UnknownDependency => "unknown_dependency",
            Self::GlobalFunctionDependency => "global_function_dependency",
            Self::UnsupportedFunction => "unsupported_function",
            Self::UnsupportedType => "unsupported_type",
            Self::RecursiveContext => "recursive_context",
            Self::AmbiguousContext => "ambiguous_context",
            Self::GuardUnsupported => "guard_unsupported",
            Self::InvalidContext => "invalid_context",
        }
    }
}
#[derive(Debug)]
pub struct Coverage {
    pub span: Span,
    pub name: String,
}
#[derive(Debug)]
pub struct Unavailable {
    pub span: Span,
    pub reason: UnavailableReason,
}
/// Immutable output from current original bytes. Coverage is diagnostic scope,
/// never semantic success: analyzed functions can and do have ownership errors.
#[derive(Debug)]
pub struct RecoveryDiagnostics {
    source: Arc<str>,
    hash: [u8; 32],
    diagnostics: Vec<Diagnostic>,
    analyzed: Vec<Coverage>,
    unavailable: Vec<Unavailable>,
}
impl RecoveryDiagnostics {
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn source_hash(&self) -> [u8; 32] {
        self.hash
    }
    /// Actual normal-checker semantic diagnostics only. Original syntax errors
    /// remain on RecoverySyntax; the aggregate budget covers both sets.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
    pub fn analyzed_functions(&self) -> &[Coverage] {
        &self.analyzed
    }
    pub fn unavailable(&self) -> &[Unavailable] {
        &self.unavailable
    }
}
struct Budget {
    remaining: usize,
}
impl Budget {
    fn charge(&mut self, n: usize) -> Result<(), RecoveryFailure> {
        self.remaining = self
            .remaining
            .checked_sub(n)
            .ok_or_else(|| resource("aggregate_work_limit"))?;
        Ok(())
    }
    fn serialized<T: serde::Serialize>(&mut self, value: &T) -> Result<(), RecoveryFailure> {
        frontend::metadata(value, &mut self.remaining)
            .map_err(|_| resource("aggregate_staging_limit"))
    }
}
fn reserved(name: &str) -> bool {
    matches!(
        name,
        "Unit"
            | "Never"
            | "String"
            | "Option"
            | "Result"
            | "RuntimeError"
            | "ArithOverflow"
            | "Fail"
            | "u8"
            | "u32"
            | "u64"
            | "i64"
            | "f64"
            | "bool"
    ) || crate::numeric::wrapping_intrinsic(name).is_some()
}
fn primitive(name: &str) -> bool {
    matches!(
        name,
        "Unit"
            | "Never"
            | "String"
            | "u8"
            | "u32"
            | "u64"
            | "i64"
            | "f64"
            | "bool"
            | "RuntimeError"
            | "ArithOverflow"
    )
}
fn item_span(item: &Item) -> Option<&Span> {
    match item {
        Item::Function(f) => Some(&f.span),
        Item::Adt(a) => Some(&a.span),
        Item::Effect(e) => Some(&e.span),
        _ => None,
    }
}

fn unsupported_guard(error: AnalysisFailure) -> Result<UnavailableReason, RecoveryFailure> {
    match error {
        AnalysisFailure::Resource { .. } => Err(resource("component_guard_limit")),
        _ => Ok(UnavailableReason::GuardUnsupported),
    }
}

/// Analyze only syntax-damaged original input. Syntax-complete input must use
/// normal check_source/guarded_analysis, never a scoped substitute.
pub fn analyze(
    snapshot: &RecoverySyntax,
    limits: RecoveryLimits,
) -> Result<RecoveryDiagnostics, RecoveryFailure> {
    limits.validate()?;
    let source = snapshot.source();
    if source.len() > limits.analysis.max_source_bytes {
        return Err(resource("source_limit"));
    }
    if snapshot.diagnostics().is_empty() {
        return Err(RecoveryFailure::Unsupported {
            reason: "syntax_complete_use_check_source",
        });
    }
    let parser_work = limits.max_work / 2;
    if parser_work == 0 {
        return Err(resource("aggregate_work_limit"));
    }
    let mut budget = Budget {
        remaining: limits.max_work - parser_work,
    };
    budget.charge(source.len())?;
    let hash = *blake3::hash(source.as_bytes()).as_bytes();
    if hash != snapshot.source_hash() {
        return Err(RecoveryFailure::SourceMismatch);
    }
    frontend::diagnostics_bounded(snapshot.diagnostics(), source, limits.analysis)
        .map_err(|_| resource("syntax_diagnostic_limit"))?;
    let declarations = snapshot.declarations();
    if declarations.len() > limits.max_functions + limits.max_context_items {
        return Err(resource("declaration_limit"));
    }
    let functions = declarations
        .iter()
        .filter(|d| d.kind == D::Function)
        .count();
    if functions > limits.max_functions || declarations.len() - functions > limits.max_context_items
    {
        return Err(resource("component_count_limit"));
    }
    let mut blocked = None;
    let mut opaque_span: Option<Span> = None;
    let mut names = BTreeSet::new();
    let mut last = 0usize;
    let mut unavailable = Vec::new();
    for d in declarations {
        budget.charge(1)?;
        if d.span.start < last || source.get(d.span.clone()).is_none() {
            return Err(RecoveryFailure::SourceMismatch);
        }
        last = d.span.end;
        if d.status == S::Opaque {
            blocked = Some(UnavailableReason::OpaqueContext);
        }
        let name = d
            .name_span
            .as_ref()
            .and_then(|span| source.get(span.clone()));
        if let Some(name) = name {
            if name.len() > limits.analysis.max_identifier_bytes {
                return Err(resource("identifier_limit"));
            }
            if !names.insert(name) {
                blocked = Some(UnavailableReason::DuplicateDeclaration);
            }
            if reserved(name) {
                blocked = Some(UnavailableReason::ReservedDeclaration);
            }
        } else {
            blocked = Some(UnavailableReason::OpaqueContext);
        }
        match d.kind {
            D::Function => {
                if d.status != S::Complete {
                    unavailable.push(Unavailable {
                        span: d.span.clone(),
                        reason: UnavailableReason::DamagedFunction,
                    });
                }
            }
            D::Struct | D::Enum | D::Effect => {
                if d.status != S::Complete {
                    blocked = Some(UnavailableReason::IncompleteContext);
                }
            }
            _ => blocked = Some(UnavailableReason::UnsupportedContext),
        }
    }
    for site in snapshot.sites() {
        budget.charge(1)?;
        if matches!(site.kind, SiteKind::OpaqueLexical | SiteKind::OpaqueMacro)
            || site
                .owner
                .and_then(|i| declarations.get(i))
                .is_none_or(|d| d.kind != D::Function || d.status != S::Recovered)
        {
            blocked = Some(UnavailableReason::OpaqueContext);
            opaque_span = Some(match opaque_span {
                Some(old) => old.start.min(site.span.start)..old.end.max(site.span.end),
                None => site.span.clone(),
            });
        }
    }
    // Check original nontrivia coverage too: an unowned top-level token gap can
    // hide an unknown/duplicate header even when no declaration was recorded.
    let mut index = 0usize;
    let mut tokens = 0usize;
    for element in snapshot.syntax().descendants_with_tokens() {
        let Some(token) = element.into_token() else {
            continue;
        };
        budget.charge(1)?;
        tokens += 1;
        if tokens > 16_384 {
            return Err(resource("token_limit"));
        }
        if [K::Whitespace, K::Comment, K::BlockComment, K::Eof]
            .iter()
            .any(|kind| token.kind().0 == *kind as u16)
        {
            continue;
        }
        if [K::Lower, K::Upper]
            .iter()
            .any(|kind| token.kind().0 == *kind as u16)
            && token.text().len() > limits.analysis.max_identifier_bytes
        {
            return Err(resource("identifier_limit"));
        }
        let range = token.text_range();
        let start = u32::from(range.start()) as usize;
        let end = u32::from(range.end()) as usize;
        while index < declarations.len() && declarations[index].span.end <= start {
            index += 1;
        }
        if declarations
            .get(index)
            .is_none_or(|d| start < d.span.start || end > d.span.end)
        {
            blocked = Some(UnavailableReason::OpaqueContext);
            opaque_span = Some(match opaque_span {
                Some(old) => old.start.min(start)..old.end.max(end),
                None => start..end,
            });
        }
    }
    if let Some(reason) = blocked {
        for d in declarations {
            if d.kind != D::Function || d.status == S::Complete {
                unavailable.push(Unavailable {
                    span: d.span.clone(),
                    reason,
                });
            }
        }
        if let Some(span) = opaque_span {
            unavailable.push(Unavailable {
                span,
                reason: UnavailableReason::OpaqueContext,
            });
        }
        if unavailable.is_empty() {
            unavailable.push(Unavailable {
                span: 0..source.len(),
                reason,
            });
        }
        return Ok(RecoveryDiagnostics {
            source: Arc::clone(snapshot.source_arc()),
            hash,
            diagnostics: vec![],
            analyzed: vec![],
            unavailable,
        });
    }
    let indices = declarations
        .iter()
        .enumerate()
        .filter_map(|(i, d)| (d.status == S::Complete).then_some(i))
        .collect::<Vec<_>>();
    let mut syntax_limits = syntax::Limits::default();
    syntax_limits.parse.max_source_bytes = limits.analysis.max_source_bytes;
    syntax_limits.parse.max_depth = limits.analysis.max_depth;
    syntax_limits.parse.max_diagnostics = limits.analysis.max_diagnostics.min(128);
    syntax_limits.max_declarations = limits.max_functions + limits.max_context_items;
    syntax_limits.max_identifier_bytes = limits.analysis.max_identifier_bytes;
    syntax_limits.max_metadata_bytes = limits.analysis.max_metadata_bytes;
    syntax_limits.max_work = parser_work;
    let module = parser::parse_complete_items(snapshot, &indices, syntax_limits)
        .map_err(|_| resource("complete_item_parse_unavailable"))?;
    if !module.model_uses.is_empty() || !module.trait_bounds.is_empty() {
        return Err(RecoveryFailure::Unsupported {
            reason: "module_context_unsupported",
        });
    }
    budget.serialized(&module)?;
    let mut metadata = limits.analysis.max_metadata_bytes;
    frontend::metadata(&module, &mut metadata).map_err(|_| resource("ast_metadata_limit"))?;
    if module.items.len() != indices.len() {
        return Err(RecoveryFailure::SourceMismatch);
    }
    for (item, index) in module.items.iter().zip(&indices) {
        let descriptor = &declarations[*index];
        let (name, kind) = match item {
            Item::Function(f) => (f.name.as_str(), D::Function),
            Item::Adt(a) => (a.name.as_str(), if a.is_enum { D::Enum } else { D::Struct }),
            Item::Effect(e) => (e.name.as_str(), D::Effect),
            _ => {
                return Err(RecoveryFailure::Unsupported {
                    reason: "item_context_unsupported",
                });
            }
        };
        if item_span(item) != Some(&descriptor.span)
            || kind != descriptor.kind
            || descriptor
                .name_span
                .as_ref()
                .and_then(|r| source.get(r.clone()))
                != Some(name)
        {
            return Err(RecoveryFailure::SourceMismatch);
        }
    }
    let context = Context::new(&module);
    let context_items = module
        .items
        .iter()
        .filter(|i| !matches!(i, Item::Function(_)))
        .collect::<Vec<_>>();
    let (global_reason, nominal_cost) = context.validate(limits.analysis, &mut budget)?;
    let mut diagnostics = Vec::new();
    let mut analyzed = Vec::new();
    if global_reason.is_none() && !context_items.is_empty() {
        budget.serialized(&context_items)?;
        frontend::recovery_items_guard(&context_items, limits.analysis)
            .map_err(|_| resource("context_guard_limit"))?;
        budget.charge(
            nominal_cost
                .checked_mul(4)
                .ok_or_else(|| resource("context_check_work_overflow"))?,
        )?;
        let context_module = Module {
            items: context_items.iter().copied().cloned().collect(),
            ..Module::default()
        };
        let checked = check::check(&context_module);
        frontend::diagnostic_parts_bounded(
            &[snapshot.diagnostics(), &checked.diagnostics],
            source,
            limits.analysis,
        )
        .map_err(|_| resource("aggregate_diagnostic_limit"))?;
        if !checked.diagnostics.is_empty() {
            diagnostics = checked.diagnostics;
            for item in &module.items {
                if let Some(span) = item_span(item) {
                    unavailable.push(Unavailable {
                        span: span.clone(),
                        reason: UnavailableReason::InvalidContext,
                    });
                }
            }
            return Ok(RecoveryDiagnostics {
                source: Arc::clone(snapshot.source_arc()),
                hash,
                diagnostics,
                analyzed,
                unavailable,
            });
        }
    }
    for item in &module.items {
        let Item::Function(f) = item else { continue };
        if let Some(reason) = global_reason {
            unavailable.push(Unavailable {
                span: f.span.clone(),
                reason,
            });
            continue;
        }
        let mut refs = context_items.clone();
        refs.push(item);
        budget.serialized(&refs)?;
        if let Err(error) = frontend::recovery_items_guard(&refs, limits.analysis) {
            unavailable.push(Unavailable {
                span: f.span.clone(),
                reason: unsupported_guard(error)?,
            });
            continue;
        }
        let before_walk = budget.remaining;
        if let Err(reason) = context.function(f, &mut budget, limits.analysis)? {
            unavailable.push(Unavailable {
                span: f.span.clone(),
                reason,
            });
            continue;
        }
        let check_work = (before_walk - budget.remaining)
            .checked_mul(nominal_cost)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| resource("component_check_work_overflow"))?;
        budget.charge(check_work)?;
        // All closure, uniqueness, AST/inference/nominal and staging gates have
        // succeeded. Clone only these actual original items; never repaired AST.
        let component = Module {
            items: refs.into_iter().cloned().collect(),
            ..Module::default()
        };
        let checked = check::check(&component);
        frontend::diagnostic_parts_bounded(
            &[snapshot.diagnostics(), &diagnostics, &checked.diagnostics],
            source,
            limits.analysis,
        )
        .map_err(|_| resource("aggregate_diagnostic_limit"))?;
        diagnostics.extend(checked.diagnostics);
        analyzed.push(Coverage {
            span: f.span.clone(),
            name: f.name.clone(),
        });
        // The temporary checked World is dropped here and is never retained.
    }
    if let Some(reason) = global_reason {
        for item in context_items {
            unavailable.push(Unavailable {
                span: item_span(item)
                    .ok_or(RecoveryFailure::SourceMismatch)?
                    .clone(),
                reason,
            });
        }
    }
    Ok(RecoveryDiagnostics {
        source: Arc::clone(snapshot.source_arc()),
        hash,
        diagnostics,
        analyzed,
        unavailable,
    })
}

struct Context<'a> {
    adts: BTreeMap<&'a str, &'a Adt>,
    effects: BTreeMap<&'a str, &'a EffectDecl>,
    functions: BTreeSet<&'a str>,
}
impl<'a> Context<'a> {
    fn new(module: &'a Module) -> Self {
        let mut c = Self {
            adts: BTreeMap::new(),
            effects: BTreeMap::new(),
            functions: BTreeSet::new(),
        };
        for item in &module.items {
            match item {
                Item::Adt(a) => {
                    c.adts.insert(&a.name, a);
                }
                Item::Effect(e) => {
                    c.effects.insert(&e.name, e);
                }
                Item::Function(f) => {
                    c.functions.insert(&f.name);
                }
                _ => (),
            }
        }
        c
    }
    fn ty(&self, ty: &Type, local_infer: bool) -> Result<(), UnavailableReason> {
        match ty {
            Type::Named(name, args)
                if args.is_empty()
                    && (primitive(name) || self.adts.contains_key(name.as_str())) =>
            {
                Ok(())
            }
            Type::Tuple(ts) => {
                for t in ts {
                    self.ty(t, false)?;
                }
                Ok(())
            }
            Type::Infer if local_infer => Ok(()),
            Type::Named(name, _) if !self.adts.contains_key(name.as_str()) && !primitive(name) => {
                Err(UnavailableReason::UnknownDependency)
            }
            _ => Err(UnavailableReason::UnsupportedType),
        }
    }
    fn row(&self, row: &Effects) -> Result<(), UnavailableReason> {
        if row.tail.is_some() {
            return Err(UnavailableReason::UnsupportedType);
        }
        for effect in &row.instances {
            if effect.name == "Fail" && effect.args.len() == 1 {
                self.ty(&effect.args[0], false)?;
            } else if !self.effects.contains_key(effect.name.as_str()) {
                return Err(UnavailableReason::UnknownDependency);
            } else if !effect.args.is_empty() {
                return Err(UnavailableReason::UnsupportedType);
            }
        }
        Ok(())
    }
    fn constructor(&self, name: &str) -> bool {
        if let Some((ty, variant)) = name.split_once("::") {
            return self
                .adts
                .get(ty)
                .is_some_and(|a| a.variants.iter().any(|v| v.name == variant));
        }
        self.adts
            .values()
            .any(|a| a.variants.iter().any(|v| v.name == name))
    }
    fn validate(
        &self,
        limits: AnalysisLimits,
        budget: &mut Budget,
    ) -> Result<(Option<UnavailableReason>, usize), RecoveryFailure> {
        let mut variants = BTreeSet::new();
        for a in self.adts.values() {
            if !a.generics.is_empty() {
                return Ok((Some(UnavailableReason::UnsupportedContext), 0));
            }
            for v in &a.variants {
                if matches!(
                    v.name.as_str(),
                    "None"
                        | "Some"
                        | "Ok"
                        | "Err"
                        | "DivisionByZero"
                        | "IndexOutOfBounds"
                        | "Overflow"
                ) || !variants.insert(v.name.as_str())
                {
                    return Ok((Some(UnavailableReason::AmbiguousContext), 0));
                }
                let mut field_names = BTreeSet::new();
                for f in &v.fields {
                    if !field_names.insert(f.name.as_str()) {
                        return Ok((Some(UnavailableReason::AmbiguousContext), 0));
                    }
                    if let Err(reason) = self.ty(&f.ty, false) {
                        return Ok((Some(reason), 0));
                    }
                }
            }
        }
        for effect in self.effects.values() {
            if !effect.generics.is_empty() {
                return Ok((Some(UnavailableReason::UnsupportedContext), 0));
            }
            let mut operation_names = BTreeSet::new();
            for op in &effect.ops {
                if !operation_names.insert(op.name.as_str()) {
                    return Ok((Some(UnavailableReason::AmbiguousContext), 0));
                }
                if !op.generics.is_empty()
                    || op.body.is_some()
                    || op.effects.tail.is_some()
                    || !op.effects.instances.is_empty()
                {
                    return Ok((Some(UnavailableReason::UnsupportedContext), 0));
                }
                for p in &op.params {
                    if let Err(r) = self.ty(&p.ty, false) {
                        return Ok((Some(r), 0));
                    }
                }
                if let Err(r) = self.ty(&op.result, false) {
                    return Ok((Some(r), 0));
                }
            }
        }
        let mut remaining = limits.max_type_nodes;
        for name in self.adts.keys() {
            if self.expand(name, &mut vec![], 0, &mut remaining, limits, budget)? {
                return Ok((Some(UnavailableReason::RecursiveContext), 0));
            }
        }
        Ok((None, limits.max_type_nodes - remaining + 1))
    }
    fn expand(
        &self,
        name: &'a str,
        path: &mut Vec<&'a str>,
        depth: usize,
        remaining: &mut usize,
        limits: AnalysisLimits,
        budget: &mut Budget,
    ) -> Result<bool, RecoveryFailure> {
        budget.charge(1)?;
        *remaining = remaining
            .checked_sub(1)
            .ok_or_else(|| resource("nominal_expansion_limit"))?;
        if depth > limits.max_depth {
            return Err(resource("nominal_depth_limit"));
        }
        if path.contains(&name) {
            return Ok(true);
        }
        let Some(adt) = self.adts.get(name) else {
            return Ok(false);
        };
        path.push(name);
        for f in adt.variants.iter().flat_map(|v| &v.fields) {
            let mut types = vec![&f.ty];
            while let Some(ty) = types.pop() {
                budget.charge(1)?;
                *remaining = remaining
                    .checked_sub(1)
                    .ok_or_else(|| resource("nominal_expansion_limit"))?;
                match ty {
                    Type::Named(n, _) if self.adts.contains_key(n.as_str()) => {
                        if self.expand(n, path, depth + 1, remaining, limits, budget)? {
                            return Ok(true);
                        }
                    }
                    Type::Tuple(ts) => types.extend(ts),
                    _ => (),
                }
            }
        }
        path.pop();
        Ok(false)
    }
    fn function(
        &self,
        f: &'a Function,
        budget: &mut Budget,
        limits: AnalysisLimits,
    ) -> Result<Result<(), UnavailableReason>, RecoveryFailure> {
        if !f.generics.is_empty() || f.body.is_none() {
            return Ok(Err(UnavailableReason::UnsupportedFunction));
        }
        for p in &f.params {
            if let Err(r) = self.ty(&p.ty, false) {
                return Ok(Err(r));
            }
        }
        if let Err(r) = self.ty(&f.result, false).and_then(|_| self.row(&f.effects)) {
            return Ok(Err(r));
        }
        budget.charge(
            f.params
                .len()
                .checked_mul(core::mem::size_of::<&str>())
                .ok_or_else(|| resource("scope_work_overflow"))?,
        )?;
        let mut scope: BTreeSet<_> = f.params.iter().map(|p| p.name.as_str()).collect();
        if scope.len() != f.params.len() {
            return Ok(Err(UnavailableReason::UnsupportedFunction));
        }
        let mut walk = Walk {
            context: self,
            budget,
            remaining: limits.max_ast_nodes,
        };
        walk.expr(
            f.body.as_ref().expect("body checked"),
            &mut scope,
            0,
            limits.max_depth,
        )
    }
}
struct Walk<'a, 'b, 'c> {
    context: &'b Context<'a>,
    budget: &'c mut Budget,
    remaining: usize,
}
impl<'a> Walk<'a, '_, '_> {
    fn scope(&mut self, scope: &BTreeSet<&'a str>) -> Result<BTreeSet<&'a str>, RecoveryFailure> {
        self.budget.charge(
            scope
                .len()
                .checked_mul(core::mem::size_of::<&str>())
                .ok_or_else(|| resource("scope_work_overflow"))?,
        )?;
        Ok(scope.clone())
    }
    fn pattern(
        &mut self,
        p: &'a Pattern,
        scope: &mut BTreeSet<&'a str>,
    ) -> Result<Result<(), UnavailableReason>, RecoveryFailure> {
        self.budget.charge(1)?;
        self.remaining = self
            .remaining
            .checked_sub(1)
            .ok_or_else(|| resource("component_ast_limit"))?;
        match &p.kind {
            PatternKind::Bind(n) => {
                self.budget.charge(
                    core::mem::size_of::<&str>()
                        .checked_add(n.len())
                        .ok_or_else(|| resource("scope_work_overflow"))?,
                )?;
                scope.insert(n);
            }
            PatternKind::Tuple(ps) => {
                for p in ps {
                    if let Err(r) = self.pattern(p, scope)? {
                        return Ok(Err(r));
                    }
                }
            }
            PatternKind::Constructor { name, fields, .. } => {
                if !self.context.constructor(name) {
                    return Ok(Err(UnavailableReason::UnknownDependency));
                }
                for (_, p) in fields {
                    if let Err(r) = self.pattern(p, scope)? {
                        return Ok(Err(r));
                    }
                }
            }
            _ => (),
        }
        Ok(Ok(()))
    }
    fn expr(
        &mut self,
        e: &'a Expr,
        scope: &mut BTreeSet<&'a str>,
        depth: usize,
        max_depth: usize,
    ) -> Result<Result<(), UnavailableReason>, RecoveryFailure> {
        self.budget.charge(1)?;
        self.remaining = self
            .remaining
            .checked_sub(1)
            .ok_or_else(|| resource("component_ast_limit"))?;
        if depth > max_depth {
            return Err(resource("component_depth_limit"));
        }
        macro_rules! visit {
            ($e:expr,$scope:expr) => {
                if let Err(reason) = self.expr($e, $scope, depth + 1, max_depth)? {
                    return Ok(Err(reason));
                }
            };
        }
        match &e.kind {
            ExprKind::Literal(_) => (),
            ExprKind::Name(name) => {
                if !scope.contains(name.as_str()) && !self.context.constructor(name) {
                    return Ok(Err(if self.context.functions.contains(name.as_str()) {
                        UnavailableReason::GlobalFunctionDependency
                    } else {
                        UnavailableReason::UnknownDependency
                    }));
                }
            }
            ExprKind::Tuple(es) => {
                for e in es {
                    visit!(e, scope);
                }
            }
            ExprKind::Struct { name, fields } => {
                if !self.context.constructor(name) {
                    return Ok(Err(UnavailableReason::UnknownDependency));
                }
                for (_, e) in fields {
                    visit!(e, scope);
                }
            }
            ExprKind::Block { statements, tail } => {
                let mut inner = self.scope(scope)?;
                for s in statements {
                    match s {
                        Statement::Let { pat, ty, value } => {
                            if let Err(r) = self.context.ty(ty, true) {
                                return Ok(Err(r));
                            }
                            visit!(value, &mut inner);
                            if let Err(r) = self.pattern(pat, &mut inner)? {
                                return Ok(Err(r));
                            }
                        }
                        Statement::Expr(e) => visit!(e, &mut inner),
                    }
                }
                if let Some(e) = tail {
                    visit!(e, &mut inner);
                }
            }
            ExprKind::Call {
                callee,
                types,
                args,
            } => {
                if !types.is_empty() {
                    return Ok(Err(UnavailableReason::UnsupportedFunction));
                }
                let ExprKind::Name(name) = &callee.kind else {
                    return Ok(Err(UnavailableReason::UnsupportedFunction));
                };
                if scope.contains(name.as_str()) {
                    return Ok(Err(UnavailableReason::UnsupportedFunction));
                }
                let operation = name.split_once("::").is_some_and(|(effect, op)| {
                    self.context
                        .effects
                        .get(effect)
                        .is_some_and(|e| e.ops.iter().any(|f| f.name == op))
                });
                if !operation && !self.context.constructor(name) {
                    return Ok(Err(if self.context.functions.contains(name.as_str()) {
                        UnavailableReason::GlobalFunctionDependency
                    } else {
                        UnavailableReason::UnknownDependency
                    }));
                }
                for arg in args {
                    visit!(&arg.expr, scope);
                }
            }
            ExprKind::Field(e, _) | ExprKind::Unary(_, e) => visit!(e, scope),
            ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Assign(_, a, b) => {
                visit!(a, scope);
                visit!(b, scope);
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                visit!(condition, scope);
                let mut then_scope = self.scope(scope)?;
                visit!(then, &mut then_scope);
                if let Some(e) = otherwise {
                    let mut otherwise_scope = self.scope(scope)?;
                    visit!(e, &mut otherwise_scope);
                }
            }
            ExprKind::Match { value, arms } => {
                visit!(value, scope);
                for (p, e) in arms {
                    let mut inner = self.scope(scope)?;
                    if let Err(r) = self.pattern(p, &mut inner)? {
                        return Ok(Err(r));
                    }
                    visit!(e, &mut inner);
                }
            }
            ExprKind::Try(_) | ExprKind::Closure { .. } | ExprKind::Handle { .. } => {
                return Ok(Err(UnavailableReason::UnsupportedFunction));
            }
        }
        Ok(Ok(()))
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    #[test]
    fn scope_clone_and_pattern_insertion_are_charged_before_work() {
        let module = parser::parse("fn f(in first:u64,in second:u64){}").module;
        let context = Context::new(&module);
        let mut budget = Budget {
            remaining: core::mem::size_of::<&str>(),
        };
        let mut walk = Walk {
            context: &context,
            budget: &mut budget,
            remaining: 16,
        };
        let scope = BTreeSet::from(["first", "second"]);
        assert!(walk.scope(&scope).is_err());
        assert_eq!(scope.len(), 2);
        let pat = Pattern {
            kind: PatternKind::Bind("name".into()),
            span: 0..0,
        };
        let mut budget = Budget { remaining: 1 };
        let mut walk = Walk {
            context: &context,
            budget: &mut budget,
            remaining: 16,
        };
        let mut scope = BTreeSet::new();
        assert!(walk.pattern(&pat, &mut scope).is_err());
        assert!(scope.is_empty());
    }
    #[test]
    fn parameter_scope_collection_and_checked_work_overflow_fail_explicitly() {
        let module = parser::parse("fn f(in first:u64,in second:u64){}").module;
        let context = Context::new(&module);
        let Item::Function(f) = &module.items[0] else {
            unreachable!()
        };
        let mut budget = Budget { remaining: 1 };
        assert!(
            context
                .function(f, &mut budget, AnalysisLimits::default())
                .is_err()
        );
        let mut budget = Budget { remaining: 1 };
        assert!(budget.charge(usize::MAX).is_err());
        assert_eq!(budget.remaining, 1);
    }
}
