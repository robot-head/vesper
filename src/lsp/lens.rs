//! Lens presentation analysis (§21.2, §21.1.5): poisoned recovery, live linear
//! obligations at a cursor, and obligation-guided member completion.
//!
//! A syntax-damaged function is analysed through a *repaired prefix*: the
//! original bytes up to the last complete top-level statement before the
//! damage, closed with `}`, with every byte offset unchanged. The repaired
//! module is checked by the normal checker only to recover binding types and
//! method signatures for presentation. It never mints a `CheckedSource`,
//! certificate, edit, or diagnostic of its own: every scope-exit diagnostic the
//! truncated prefix provokes is absorbed into one `PoisonedRecovery` notice per
//! live linear binding.
use crate::{
    ast::*,
    check::{self, Checked, World},
    lexer::{self, TokenKind as K},
    parser,
    tooling::frontend::{self, AnalysisFailure, AnalysisLimits, CheckedSource},
};
use std::{collections::BTreeMap, sync::Arc};
use vesper_lsp::lens::{CandidateMethod, RankedCompletion, ReceiverPass, rank_methods};
use vesper_syntax::recovery::{DeclarationKind as D, DeclarationStatus as S, RecoverySyntax};

/// Finite traversal budget for one view (expressions, bindings and events).
const MAX_WORK: usize = 1 << 18;
const MAX_BINDINGS: usize = 4096;

/// Why a recovered document has no Lens view. Always reported, never guessed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unavailable {
    OpaqueDeclaration,
    DamagedContext,
    DamagedHeader,
    RepairUnparsed,
    Resource,
    Guard,
    /// The request that asked for the view was cancelled.
    Cancelled,
}
impl Unavailable {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::OpaqueDeclaration => "opaque_declaration",
            Self::DamagedContext => "damaged_non_function_context",
            Self::DamagedHeader => "damaged_function_header",
            Self::RepairUnparsed => "repaired_prefix_does_not_parse",
            Self::Resource => "lens_resource_limit",
            Self::Guard => "editor_guard_rejected",
            Self::Cancelled => "request_cancelled",
        }
    }
}

/// What a repaired region stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RegionKind {
    /// Unfinished statements in a function or method body: the body is cut
    /// at the last complete statement of the innermost block that can be
    /// closed, and linear bindings live there are poisoned.
    Body,
    /// An unfinished parameter list or signature: the header is cut at its
    /// last complete parameter and the complete body is kept. Nothing is
    /// poisoned; the function's scope-exit checks are simply not published.
    Header,
    /// An unfinished struct, enum or effect: cut at its last complete member.
    Member,
}
impl RegionKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Body => "body",
            Self::Header => "header",
            Self::Member => "member",
        }
    }
}

/// A syntax-damaged declaration and the cut its repaired view ends at.
#[derive(Clone, Debug)]
pub(crate) struct Region {
    pub name: String,
    pub kind: RegionKind,
    /// Original declaration span.
    pub span: Span,
    /// Original offsets that belong to the damaged function or member.
    pub scope: Span,
    /// Byte offset where the repaired text is cut (the inserted closers).
    pub cut: usize,
    /// Nesting depth of the block the cut closes (0 = the body itself).
    pub depth: usize,
    /// The unfinished code: from the first token after the cut through the
    /// first syntax damage.
    pub damage: Span,
}

enum Basis {
    Checked(Arc<CheckedSource>),
    /// `clean`: the Lens profile checked the text without errors.
    Repaired {
        checked: Box<Checked>,
        clean: bool,
    },
}

/// Source facts used for presentation. Offsets are original-source offsets.
pub(crate) struct View {
    basis: Basis,
    regions: Vec<Region>,
    functions: Vec<FunctionFacts>,
    /// Scope-exit diagnostics the repaired prefix produced and Lens absorbed.
    absorbed: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct Binding {
    pub name: String,
    pub origin: Span,
    pub ty: Type,
    pub linear: bool,
    /// Owned by this frame (let binding or sink parameter).
    pub owned: bool,
    scope: Span,
    events: Vec<Event>,
}
#[derive(Clone, Debug)]
struct Event {
    span: Span,
    /// (branch arm, whole branching construct) pairs enclosing the event.
    branches: Vec<(Span, Span)>,
}
impl Binding {
    fn visible_at(&self, at: usize) -> bool {
        self.origin.end <= at && at <= self.scope.end
    }
    /// A consumption has happened before `at` on the path through `at`.
    pub(crate) fn consumed_before(&self, at: usize) -> Option<&Span> {
        self.events
            .iter()
            .find(|event| {
                event.span.end <= at
                    && event.branches.iter().all(|(arm, construct)| {
                        (arm.start <= at && at <= arm.end) || construct.end <= at
                    })
            })
            .map(|event| &event.span)
    }
    pub(crate) fn first_consumption_after(&self, at: usize) -> Option<&Span> {
        self.events
            .iter()
            .filter(|event| event.span.start >= at)
            .map(|event| &event.span)
            .min_by_key(|span| span.start)
    }
}

pub(crate) struct FunctionFacts {
    pub span: Span,
    pub bindings: Vec<Binding>,
}

impl View {
    pub(crate) fn checked(snapshot: Arc<CheckedSource>) -> Result<Self, Unavailable> {
        let functions = facts(&snapshot.parsed().module, snapshot.checked())?;
        Ok(Self {
            basis: Basis::Checked(snapshot),
            regions: Vec::new(),
            functions,
            absorbed: 0,
        })
    }

    /// Build a repaired view for syntax-damaged original source.
    pub(crate) fn recovered(
        syntax: &RecoverySyntax,
        limits: AnalysisLimits,
    ) -> Result<Self, Unavailable> {
        let (repaired, regions) = repair(syntax, limits)?;
        Self::from_text(&repaired, regions, limits)
    }

    /// A view of syntax-complete source that has semantic errors. The normal
    /// checker's partial facts are used for presentation only.
    pub(crate) fn semantic(source: &str, limits: AnalysisLimits) -> Result<Self, Unavailable> {
        Self::from_text(source, Vec::new(), limits)
    }

    fn from_text(
        text: &str,
        regions: Vec<Region>,
        limits: AnalysisLimits,
    ) -> Result<Self, Unavailable> {
        // The normal editor guards run first. A semantic failure is expected:
        // the truncated prefix leaves its linear bindings unconsumed.
        let clean = match frontend::check_source(text, limits) {
            Ok(_) => true,
            Err(AnalysisFailure::Semantic { .. }) => false,
            Err(AnalysisFailure::Parse { .. }) => return Err(Unavailable::RepairUnparsed),
            Err(AnalysisFailure::Resource { .. }) => return Err(Unavailable::Resource),
            Err(_) => return Err(Unavailable::Guard),
        };
        let parsed = parser::parse_with_limits(
            text,
            parser::ParseLimits {
                max_source_bytes: limits.max_source_bytes,
                max_tokens: 16_384,
                max_diagnostics: limits.max_diagnostics,
                max_depth: limits.max_depth,
            },
        )
        .map_err(|_| Unavailable::Resource)?;
        if !parsed.diagnostics.is_empty() {
            return Err(Unavailable::RepairUnparsed);
        }
        let checked = check::check(&parsed.module);
        let functions = facts(&parsed.module, &checked)?;
        let absorbed = checked
            .diagnostics
            .iter()
            .filter(|diag| {
                regions.iter().any(|region| {
                    region.span.start <= diag.span.start && diag.span.end > region.cut
                })
            })
            .count();
        Ok(Self {
            basis: Basis::Repaired {
                checked: Box::new(checked),
                clean,
            },
            regions,
            functions,
            absorbed,
        })
    }

    fn checked_facts(&self) -> &Checked {
        match &self.basis {
            Basis::Checked(snapshot) => snapshot.checked(),
            Basis::Repaired { checked, .. } => checked,
        }
    }
    pub(crate) fn world(&self) -> &World {
        &self.checked_facts().world
    }
    /// Which facts back this view: the checked source, the checker's partial
    /// facts for source with semantic errors, or a repaired prefix.
    pub(crate) fn scope(&self) -> &'static str {
        match (&self.basis, self.regions.is_empty()) {
            (Basis::Checked(_), _) => "checked",
            (Basis::Repaired { clean: true, .. }, true) => "lens_profile_checked",
            (Basis::Repaired { .. }, true) => "semantic_error_partial",
            (Basis::Repaired { .. }, false) => "recovered_prefix",
        }
    }
    pub(crate) fn regions(&self) -> &[Region] {
        &self.regions
    }
    pub(crate) fn absorbed(&self) -> usize {
        self.absorbed
    }

    /// The function containing `at` and the offset obligations are read at.
    /// Inside a damaged function, positions after the cut read the prefix end.
    fn function_at(&self, at: usize) -> Option<(&FunctionFacts, usize)> {
        if let Some(region) = self.regions.iter().find(|region| {
            region.kind == RegionKind::Body && region.scope.start <= at && at <= region.scope.end
        }) {
            let facts = self.region_function(region)?;
            return Some((facts, at.min(region.cut)));
        }
        self.functions
            .iter()
            .filter(|f| f.span.start <= at && at <= f.span.end)
            .min_by_key(|f| f.span.len())
            .map(|f| (f, at))
    }

    /// Every owned linear binding in source order.
    pub(crate) fn linear_bindings(&self) -> impl Iterator<Item = &Binding> {
        self.functions
            .iter()
            .flat_map(|f| f.bindings.iter())
            .filter(|b| b.linear && b.owned)
    }

    /// Owned linear bindings visible and not yet consumed at `at`.
    pub(crate) fn outstanding(&self, at: usize) -> Vec<&Binding> {
        let Some((function, at)) = self.function_at(at) else {
            return Vec::new();
        };
        live(function, at)
    }

    /// The (innermost) function whose repaired body holds the region's cut.
    fn region_function(&self, region: &Region) -> Option<&FunctionFacts> {
        self.functions
            .iter()
            .filter(|f| f.span.start <= region.cut && region.cut <= f.span.end)
            .filter(|f| region.scope.start <= f.span.start)
            .min_by_key(|f| f.span.len())
    }

    /// Live linear bindings at each damaged body's cut: the poisoned set.
    pub(crate) fn poisoned(&self) -> Vec<(&Region, Vec<&Binding>)> {
        self.regions
            .iter()
            .filter(|region| region.kind == RegionKind::Body)
            .filter_map(|region| {
                let function = self.region_function(region)?;
                Some((region, live(function, region.cut)))
            })
            .collect()
    }

    /// Resolve the binding named `name` visible at `at`.
    pub(crate) fn binding(&self, name: &str, at: usize) -> Option<(&Binding, usize)> {
        let (function, at) = self.function_at(at)?;
        resolve(function, name, at).map(|binding| (binding, at))
    }

    /// Ranked member completions for a receiver binding at `at`.
    pub(crate) fn complete_member(
        &self,
        receiver: &str,
        at: usize,
    ) -> Option<(&Binding, Vec<RankedCompletion>)> {
        let (binding, at) = self.binding(receiver, at)?;
        let world = self.world();
        let prefix = format!("{}::", binding.ty.name());
        if binding.ty.name().is_empty() {
            return Some((binding, Vec::new()));
        }
        let methods: Vec<_> = world
            .functions
            .iter()
            .filter_map(|(name, f)| {
                let method = name.strip_prefix(&prefix)?;
                let receiver = f.params.first().filter(|p| p.name == "self")?;
                Some(CandidateMethod {
                    name: method.to_owned(),
                    receiver: match receiver.pass {
                        Pass::In => ReceiverPass::In,
                        Pass::Inout => ReceiverPass::Inout,
                        Pass::Sink => ReceiverPass::Sink,
                    },
                    abortable_effects: f
                        .effects
                        .instances
                        .iter()
                        .filter(|e| world.effects.get(&e.name).is_some_and(|decl| !decl.linear))
                        .map(|e| e.name.clone())
                        .collect(),
                    detail: signature(f),
                })
            })
            .collect();
        let live = self.outstanding(at);
        let receiver_live = live.iter().any(|b| b.origin == binding.origin);
        let others: Vec<String> = live
            .iter()
            .filter(|b| b.origin != binding.origin)
            .map(|b| format!("'{}'", b.name))
            .collect();
        Some((binding, rank_methods(receiver_live, &others, &methods)))
    }
}

fn live(function: &FunctionFacts, at: usize) -> Vec<&Binding> {
    function
        .bindings
        .iter()
        .filter(|b| b.linear && b.owned && b.visible_at(at) && b.consumed_before(at).is_none())
        .filter(|b| resolve(function, &b.name, at).is_some_and(|r| r.origin == b.origin))
        .collect()
}

/// Innermost visible binding of `name` at `at` (latest introduction wins).
fn resolve<'a>(function: &'a FunctionFacts, name: &str, at: usize) -> Option<&'a Binding> {
    function
        .bindings
        .iter()
        .filter(|b| b.name == name && b.visible_at(at))
        .max_by_key(|b| b.origin.start)
}

pub(crate) fn signature(f: &Function) -> String {
    let params: Vec<String> = f
        .params
        .iter()
        .map(|p| {
            let pass = match p.pass {
                Pass::In => "",
                Pass::Inout => "inout ",
                Pass::Sink => "sink ",
            };
            if p.name == "self" {
                format!("{pass}self")
            } else {
                format!("{pass}{}: {}", p.name, p.ty)
            }
        })
        .collect();
    let short = f.name.rsplit("::").next().unwrap_or(&f.name);
    let mut text = format!("fn {short}({})", params.join(", "));
    let result = f.result.to_string();
    if f.result != Type::Infer && result != "()" && result != "Unit" {
        text.push_str(&format!(" -> {}", f.result));
    }
    if !f.effects.instances.is_empty() {
        let effects: Vec<String> = f.effects.instances.iter().map(|e| e.name.clone()).collect();
        text.push_str(&format!(" eff [{}]", effects.join(", ")));
    }
    text
}

/// An open delimiter at the damage, with the end of the last complete
/// element (statement, arm, field, parameter) directly inside it.
#[derive(Clone, Copy, Debug)]
struct Open {
    kind: K,
    cut: usize,
    /// A `let` at this level is not yet terminated by `;`.
    in_let: bool,
}

/// Closers for `levels` (outermost first), innermost first. A level whose
/// parent block is inside an unterminated `let` gets the `;` it needs.
fn closers(levels: &[Open]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, open) in levels.iter().enumerate().rev() {
        out.push(closer(open.kind));
        if i > 0 && levels[i - 1].kind == K::LBrace && levels[i - 1].in_let {
            out.push(b';');
        }
    }
    out
}

fn closer(kind: K) -> u8 {
    match kind {
        K::LParen => b')',
        K::LBracket => b']',
        _ => b'}',
    }
}

/// Tokens that start a statement or an arm after a closing brace.
fn starts_element(kind: K) -> bool {
    matches!(
        kind,
        K::Let | K::Lower | K::Upper | K::SelfKw | K::If | K::Match | K::Handle | K::Return
    )
}

/// The delimiters open at `damage`, scanning the declaration from `from`.
fn open_at(tokens: &[&lexer::Token], from: usize, damage: usize) -> Vec<Open> {
    let mut stack: Vec<Open> = Vec::new();
    let mut iter = tokens
        .iter()
        .skip_while(|t| t.span.start < from)
        .take_while(|t| t.span.start < damage)
        .peekable();
    while let Some(token) = iter.next() {
        match token.kind {
            K::LBrace | K::LParen | K::LBracket => stack.push(Open {
                kind: token.kind,
                cut: token.span.end,
                in_let: false,
            }),
            K::Let => {
                if let Some(top) = stack.last_mut() {
                    top.in_let = true;
                }
            }
            K::RBrace | K::RParen | K::RBracket => {
                let closed = stack.pop();
                // A block such as `if c { .. }` ends its statement at the
                // brace unless an `else` or another continuation follows.
                if closed.is_some_and(|o| o.kind == K::LBrace)
                    && let Some(parent) = stack.last_mut()
                    && parent.kind == K::LBrace
                    && iter
                        .peek()
                        .is_some_and(|next| starts_element(next.kind) && next.span.start < damage)
                {
                    parent.cut = token.span.end;
                }
            }
            K::Semi | K::Comma => {
                if let Some(top) = stack.last_mut() {
                    top.cut = token.span.end;
                    if token.kind == K::Semi {
                        top.in_let = false;
                    }
                }
            }
            _ => {}
        }
    }
    stack
}

/// One candidate repair of one damaged declaration, in original offsets.
#[derive(Clone, Debug)]
struct Patch {
    /// Bytes from here up to `blank_end` become spaces (newlines kept).
    cut: usize,
    blank_end: usize,
    /// Written over the first non-newline bytes at the cut.
    closers: Vec<u8>,
    kind: RegionKind,
    depth: usize,
    /// For header repairs: keep these bytes (the body) unblanked.
    keep_from: Option<usize>,
}

fn apply(bytes: &mut [u8], patch: &Patch) -> bool {
    let end = patch.keep_from.unwrap_or(patch.blank_end).min(bytes.len());
    if patch.cut > end {
        return false;
    }
    for byte in &mut bytes[patch.cut..end] {
        if *byte != b'\n' && *byte != b'\r' {
            *byte = b' ';
        }
    }
    let mut at = patch.cut;
    for &c in &patch.closers {
        while at < end && matches!(bytes[at], b'\n' | b'\r') {
            at += 1;
        }
        if at >= end {
            return false;
        }
        bytes[at] = c;
        at += 1;
    }
    true
}

fn blank(bytes: &mut [u8], span: &Span) {
    let end = span.end.min(bytes.len());
    for byte in &mut bytes[span.start..end] {
        if *byte != b'\n' && *byte != b'\r' {
            *byte = b' ';
        }
    }
}

/// Candidate patches for one damaged declaration, most specific first.
fn candidates(
    tokens: &[&lexer::Token],
    kind: D,
    span: &Span,
    damage: usize,
) -> Vec<(Patch, Span, Option<usize>)> {
    let stack = open_at(tokens, span.start, damage);
    let mut out = Vec::new();
    let tokens_in = || {
        tokens
            .iter()
            .filter(move |t| span.start <= t.span.start && t.span.end <= span.end)
    };
    // The `fn` keyword of the damaged function: the declaration itself, or
    // the last method header before the damage inside an impl.
    let fn_start = tokens_in()
        .filter(|t| t.kind == K::Fn && t.span.start < damage)
        .map(|t| t.span.start)
        .next_back();
    let next_fn = tokens_in()
        .find(|t| t.kind == K::Fn && t.span.start >= damage)
        .map(|t| t.span.start);
    let member = matches!(kind, D::Struct | D::Enum | D::Effect);
    // The method (or function) body is the first brace after its `fn` at
    // the method's own level.
    let base = usize::from(kind == D::Impl);
    let body_level = stack
        .iter()
        .enumerate()
        .skip(base)
        .find(|(_, o)| o.kind == K::LBrace)
        .map(|(i, _)| i);
    let scope_from = if member {
        span.start
    } else {
        fn_start.unwrap_or(span.start)
    };
    match (member, body_level) {
        // Statement repair at each enclosing block, innermost first.
        (false, Some(body)) => {
            for level in (body..stack.len()).rev() {
                if stack[level].kind != K::LBrace {
                    continue;
                }
                let cut = stack[level].cut;
                // Inside an impl, first keep the later methods.
                if kind == D::Impl
                    && let Some(next) = next_fn
                {
                    out.push((
                        Patch {
                            cut,
                            blank_end: next,
                            closers: closers(&stack[base..=level]),
                            kind: RegionKind::Body,
                            depth: level - body,
                            keep_from: None,
                        },
                        scope_from..next,
                        fn_start,
                    ));
                }
                out.push((
                    Patch {
                        cut,
                        blank_end: span.end,
                        closers: closers(&stack[..=level]),
                        kind: RegionKind::Body,
                        depth: level - body,
                        keep_from: None,
                    },
                    scope_from..span.end,
                    fn_start,
                ));
            }
        }
        // Header repair: close the parameter list at its last complete
        // parameter (or drop a damaged signature tail) and keep the body.
        (false, None) => {
            let Some(fn_start) = fn_start else {
                return out;
            };
            let limit = next_fn.unwrap_or(span.end);
            // The first `{` after the damage at the method's own level starts
            // the complete body that is kept.
            let body = tokens_in()
                .find(|t| t.kind == K::LBrace && t.span.start >= damage && t.span.start < limit)
                .map(|t| t.span.start);
            let Some(body) = body else {
                return out;
            };
            let params = stack.iter().skip(base).find(|o| o.kind == K::LParen);
            let header_cut = match params {
                Some(open) => Some((open.cut, vec![b')'])),
                None => tokens_in()
                    .filter(|t| {
                        t.kind == K::RParen && t.span.start >= fn_start && t.span.end <= damage
                    })
                    .map(|t| (t.span.end, Vec::new()))
                    .next_back(),
            };
            if let Some((cut, closers)) = header_cut {
                out.push((
                    Patch {
                        cut,
                        blank_end: body,
                        closers,
                        kind: RegionKind::Header,
                        depth: 0,
                        keep_from: Some(body),
                    },
                    fn_start..limit,
                    Some(fn_start),
                ));
            }
        }
        // Member repair: close the struct, enum or effect at its last
        // complete member.
        (true, _) => {
            if let Some(level) = stack.iter().position(|o| o.kind == K::LBrace) {
                out.push((
                    Patch {
                        cut: stack[level].cut,
                        blank_end: span.end,
                        closers: closers(&stack[..=level]),
                        kind: RegionKind::Member,
                        depth: stack.len() - 1 - level,
                        keep_from: None,
                    },
                    span.clone(),
                    None,
                ));
            }
        }
    }
    out
}

fn parses(text: &str, limits: AnalysisLimits) -> bool {
    parser::parse_with_limits(
        text,
        parser::ParseLimits {
            max_source_bytes: limits.max_source_bytes,
            max_tokens: 65_536,
            max_diagnostics: 1,
            max_depth: limits.max_depth,
        },
    )
    .is_ok_and(|parsed| parsed.diagnostics.is_empty())
}

/// Repair every damaged declaration of `syntax`, keeping byte offsets.
fn repair(
    syntax: &RecoverySyntax,
    limits: AnalysisLimits,
) -> Result<(String, Vec<Region>), Unavailable> {
    let source = syntax.source();
    let lexed = lexer::lex_with_limits(
        source,
        lexer::LexLimits {
            max_source_bytes: limits.max_source_bytes,
            max_tokens: 65_536,
            max_diagnostics: limits.max_diagnostics,
        },
    )
    .map_err(|_| Unavailable::Resource)?;
    if lexed.limit.is_some() {
        return Err(Unavailable::Resource);
    }
    let tokens: Vec<_> = lexed
        .tokens
        .iter()
        .filter(|t| {
            !matches!(
                t.kind,
                K::Whitespace | K::Comment | K::BlockComment | K::Eof
            )
        })
        .collect();
    let declarations = syntax.declarations();
    // Sites outside every damaged declaration cannot be repaired by a cut.
    if syntax.sites().iter().any(|site| {
        site.owner
            .and_then(|i| declarations.get(i))
            .is_none_or(|d| d.status == S::Complete)
    }) {
        return Err(Unavailable::DamagedContext);
    }
    let damaged: Vec<_> = declarations
        .iter()
        .enumerate()
        .filter(|(_, d)| d.status != S::Complete)
        .collect();
    if damaged.len() > 16 {
        return Err(Unavailable::Resource);
    }
    let mut base = source.as_bytes().to_vec();
    for (_, d) in &damaged {
        // An opaque function or impl (for example a broken parameter list)
        // may still have a header repair; every repair is parse-validated.
        if d.status == S::Opaque && !matches!(d.kind, D::Function | D::Impl) {
            return Err(Unavailable::OpaqueDeclaration);
        }
        if !matches!(
            d.kind,
            D::Function | D::Impl | D::Struct | D::Enum | D::Effect
        ) {
            return Err(Unavailable::DamagedContext);
        }
        blank(&mut base, &d.span);
    }
    let mut chosen = Vec::new();
    let mut parses_left = 64usize;
    for (index, d) in damaged {
        if crate::lsp::request_cancelled() {
            return Err(Unavailable::Cancelled);
        }
        let damage = syntax
            .sites()
            .iter()
            .filter(|site| site.owner == Some(index))
            .map(|site| site.span.clone())
            .chain(
                syntax
                    .diagnostics()
                    .iter()
                    .filter(|diag| d.span.start <= diag.span.start && diag.span.start < d.span.end)
                    .map(|diag| diag.span.clone()),
            )
            .min_by_key(|span| span.start)
            .unwrap_or(d.span.end..d.span.end);
        let options = candidates(&tokens, d.kind, &d.span, damage.start);
        if options.is_empty() {
            return Err(match d.kind {
                D::Function | D::Impl => Unavailable::DamagedHeader,
                _ => Unavailable::DamagedContext,
            });
        }
        let mut accepted = None;
        for (patch, scope, fn_start) in options {
            if parses_left == 0 {
                return Err(Unavailable::Resource);
            }
            parses_left -= 1;
            // Validate this repair alone: the declaration's original bytes
            // with the patch, every other damaged declaration blanked.
            let mut trial = base.clone();
            trial[d.span.clone()].copy_from_slice(&source.as_bytes()[d.span.clone()]);
            if !apply(&mut trial, &patch) {
                continue;
            }
            if let Ok(text) = std::str::from_utf8(&trial)
                && parses(text, limits)
            {
                accepted = Some((patch, scope, fn_start));
                break;
            }
        }
        let Some((patch, scope, fn_start)) = accepted else {
            return Err(Unavailable::RepairUnparsed);
        };
        let name = match (d.kind, fn_start) {
            (D::Impl, Some(start)) => {
                let method = tokens
                    .iter()
                    .find(|t| t.span.start > start && t.kind == K::Lower)
                    .map(|t| &source[t.span.clone()])
                    .unwrap_or("?");
                let owner = d
                    .name_span
                    .as_ref()
                    .map(|n| &source[n.clone()])
                    .or_else(|| {
                        tokens
                            .iter()
                            .find(|t| t.span.start > d.span.start && t.kind == K::Upper)
                            .map(|t| &source[t.span.clone()])
                    })
                    .unwrap_or("?");
                format!("{owner}::{method}")
            }
            _ => d
                .name_span
                .as_ref()
                .map(|n| source[n.clone()].to_owned())
                .unwrap_or_default(),
        };
        let cut = patch.cut;
        let unfinished = cut
            + source[cut..]
                .find(|c: char| !c.is_whitespace())
                .unwrap_or(0);
        let damage = if unfinished < damage.end {
            unfinished..damage.end
        } else {
            damage
        };
        chosen.push((
            patch.clone(),
            Region {
                name,
                kind: patch.kind,
                span: d.span.clone(),
                scope,
                cut: if patch.kind == RegionKind::Header {
                    d.span.end
                } else {
                    cut
                },
                depth: patch.depth,
                damage,
            },
        ));
    }
    let mut bytes = source.as_bytes().to_vec();
    for (patch, _) in &chosen {
        if !apply(&mut bytes, patch) {
            return Err(Unavailable::RepairUnparsed);
        }
    }
    let repaired = String::from_utf8(bytes).map_err(|_| Unavailable::RepairUnparsed)?;
    Ok((repaired, chosen.into_iter().map(|(_, r)| r).collect()))
}

fn mentions(ty: &Type, name: &str) -> bool {
    match ty {
        Type::Named(n, args) => n == name || args.iter().any(|a| mentions(a, name)),
        Type::Tuple(items) => items.iter().any(|t| mentions(t, name)),
        Type::Function { params, result, .. } => {
            params.iter().any(|(_, t)| mentions(t, name)) || mentions(result, name)
        }
        _ => false,
    }
}

struct Walker<'a> {
    world: &'a World,
    types: &'a TypeMap,
    bindings: Vec<Binding>,
    scopes: Vec<Vec<usize>>,
    branches: Vec<(Span, Span)>,
    work: usize,
}

fn facts(module: &Module, checked: &Checked) -> Result<Vec<FunctionFacts>, Unavailable> {
    let mut out = Vec::new();
    let mut work = 0usize;
    let mut visit = |f: &Function, owner: Option<&Type>| -> Result<(), Unavailable> {
        let Some(body) = &f.body else { return Ok(()) };
        let mut walker = Walker {
            world: &checked.world,
            types: &checked.expression_types,
            bindings: Vec::new(),
            scopes: vec![Vec::new()],
            branches: Vec::new(),
            work,
        };
        for p in &f.params {
            let ty = if p.name == "self" && p.ty == Type::Infer {
                owner.cloned().unwrap_or(Type::Infer)
            } else {
                p.ty.clone()
            };
            walker.bind(
                &p.name,
                p.span.clone(),
                ty,
                p.pass == Pass::Sink,
                body.span.clone(),
            )?;
        }
        walker.expr(body, true)?;
        work = walker.work;
        out.push(FunctionFacts {
            span: f.span.clone(),
            bindings: walker.bindings,
        });
        Ok(())
    };
    for item in &module.items {
        match item {
            Item::Function(f) => visit(f, None)?,
            Item::Impl { ty, methods, .. } => {
                for m in methods {
                    let mut named = m.clone();
                    named.name = format!("{}::{}", ty.name(), m.name);
                    visit(&named, Some(ty))?;
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

impl Walker<'_> {
    fn charge(&mut self) -> Result<(), Unavailable> {
        self.work += 1;
        if self.work > MAX_WORK || self.bindings.len() > MAX_BINDINGS {
            return Err(Unavailable::Resource);
        }
        // Long traversals stop as soon as their request is cancelled.
        if self.work.is_multiple_of(256) && crate::lsp::request_cancelled() {
            return Err(Unavailable::Cancelled);
        }
        Ok(())
    }
    /// Field types of constructor `name` for a scrutinee of type `ty`, with
    /// the ADT's type parameters substituted when `ty` names its arguments.
    fn constructor_fields(&self, name: &str, ty: &Type) -> Vec<(String, Type)> {
        let Some((adt, variant)) = self.world.constructor(name) else {
            return Vec::new();
        };
        let params: Vec<&str> = adt
            .generics
            .iter()
            .filter(|g| !g.row)
            .map(|g| g.name.as_str())
            .collect();
        let vars: BTreeMap<String, Type> = match ty {
            Type::Named(n, args) if *n == adt.name && args.len() == params.len() => params
                .iter()
                .map(|p| (*p).to_owned())
                .zip(args.iter().cloned())
                .collect(),
            _ => BTreeMap::new(),
        };
        variant
            .fields
            .iter()
            .map(|f| {
                let ty = if vars.is_empty() && !params.is_empty() {
                    // Unknown type arguments: never guess a mode for them.
                    if params.iter().any(|p| mentions(&f.ty, p)) {
                        Type::Infer
                    } else {
                        f.ty.clone()
                    }
                } else {
                    substitute(&f.ty, &vars)
                };
                (f.name.clone(), ty)
            })
            .collect()
    }
    fn bind(
        &mut self,
        name: &str,
        origin: Span,
        ty: Type,
        owned: bool,
        scope: Span,
    ) -> Result<(), Unavailable> {
        self.charge()?;
        let linear = ty != Type::Infer && self.world.mode(&ty, &BTreeMap::new()) == Mode::Linear;
        self.bindings.push(Binding {
            name: name.to_owned(),
            origin,
            ty,
            linear,
            owned,
            scope,
            events: Vec::new(),
        });
        let index = self.bindings.len() - 1;
        self.scopes
            .last_mut()
            .expect("walker always has a scope")
            .push(index);
        Ok(())
    }
    fn lookup(&self, name: &str) -> Option<usize> {
        self.scopes
            .iter()
            .rev()
            .flat_map(|scope| scope.iter().rev())
            .copied()
            .find(|&i| self.bindings[i].name == name)
    }
    fn consume(&mut self, name: &str, span: Span) {
        if let Some(i) = self.lookup(name) {
            let branches = self.branches.clone();
            self.bindings[i].events.push(Event { span, branches });
        }
    }
    fn ty_of(&self, e: &Expr) -> Type {
        if let ExprKind::Name(n) = &e.kind
            && let Some(i) = self.lookup(n)
        {
            return self.bindings[i].ty.clone();
        }
        self.types
            .get(&(e.span.start, e.span.end))
            .cloned()
            .unwrap_or(Type::Infer)
    }
    fn pattern(&mut self, pat: &Pattern, ty: Type, scope: &Span) -> Result<(), Unavailable> {
        self.charge()?;
        match &pat.kind {
            PatternKind::Bind(n) => self.bind(n, pat.span.clone(), ty, true, scope.clone()),
            PatternKind::Tuple(items) => {
                for (i, p) in items.iter().enumerate() {
                    let ty = match &ty {
                        Type::Tuple(ts) => ts.get(i).cloned().unwrap_or(Type::Infer),
                        _ => Type::Infer,
                    };
                    self.pattern(p, ty, scope)?;
                }
                Ok(())
            }
            // Destructured fields and match-arm payloads are owned by the
            // pattern: a linear field becomes an obligation of its binding.
            PatternKind::Constructor {
                name,
                fields,
                named,
            } => {
                let types = self.constructor_fields(name, &ty);
                for (i, (field, p)) in fields.iter().enumerate() {
                    let ty = if *named {
                        types.iter().find(|(n, _)| n == field)
                    } else {
                        field
                            .parse::<usize>()
                            .ok()
                            .and_then(|i| types.get(i))
                            .or_else(|| types.iter().find(|(n, _)| n == field))
                            .or_else(|| types.get(i))
                    }
                    .map(|(_, ty)| ty.clone())
                    .unwrap_or(Type::Infer);
                    self.pattern(p, ty, scope)?;
                }
                Ok(())
            }
            PatternKind::Wild | PatternKind::Literal(_) => Ok(()),
        }
    }
    fn args(&mut self, args: &[Arg], params: &[Param], default: Pass) -> Result<(), Unavailable> {
        let passes: Vec<Pass> = params.iter().map(|p| p.pass).collect();
        self.args_by_pass(args, &passes, default)
    }
    fn args_by_pass(
        &mut self,
        args: &[Arg],
        passes: &[Pass],
        default: Pass,
    ) -> Result<(), Unavailable> {
        for (i, a) in args.iter().enumerate() {
            let pass = if a.explicit {
                a.pass
            } else {
                passes.get(i).copied().unwrap_or(default)
            };
            self.expr(&a.expr, pass == Pass::Sink)?;
        }
        Ok(())
    }
    fn branch(&mut self, arm: &Expr, construct: &Span, consume: bool) -> Result<(), Unavailable> {
        self.branches.push((arm.span.clone(), construct.clone()));
        let result = self.expr(arm, consume);
        self.branches.pop();
        result
    }
    fn expr(&mut self, e: &Expr, consume: bool) -> Result<(), Unavailable> {
        self.charge()?;
        match &e.kind {
            ExprKind::Literal(_) => {}
            ExprKind::Name(n) => {
                if consume {
                    self.consume(n, e.span.clone());
                }
            }
            ExprKind::Tuple(items) => {
                for item in items {
                    self.expr(item, true)?;
                }
            }
            ExprKind::Struct { fields, .. } => {
                for (_, value) in fields {
                    self.expr(value, true)?;
                }
            }
            ExprKind::Block { statements, tail } => {
                self.scopes.push(Vec::new());
                for statement in statements {
                    match statement {
                        Statement::Let { pat, ty, value } => {
                            self.expr(value, true)?;
                            let ty = if *ty == Type::Infer {
                                self.ty_of(value)
                            } else {
                                ty.clone()
                            };
                            self.pattern(pat, ty, &e.span)?;
                        }
                        Statement::Expr(x) => self.expr(x, true)?,
                    }
                }
                if let Some(tail) = tail {
                    self.expr(tail, consume)?;
                }
                self.scopes.pop();
            }
            ExprKind::Call { callee, args, .. } => match &callee.kind {
                ExprKind::Name(name) if self.lookup(name).is_none() => {
                    if self.world.constructor(name).is_some() {
                        self.args(args, &[], Pass::Sink)?;
                    } else if let Some(f) = self.world.functions.get(name) {
                        let params = f.params.clone();
                        self.args(args, &params, Pass::Sink)?;
                    } else if let Some((effect, op)) = name.split_once("::")
                        && let Some(f) = self
                            .world
                            .effects
                            .get(effect)
                            .and_then(|d| d.ops.iter().find(|f| f.name == op))
                    {
                        let params = f.params.clone();
                        self.args(args, &params, Pass::Sink)?;
                    } else {
                        self.args(args, &[], Pass::Sink)?;
                    }
                }
                ExprKind::Field(base, method) => {
                    let ty = self.ty_of(base);
                    let method = self
                        .world
                        .functions
                        .get(&format!("{}::{method}", ty.name()))
                        .map(|f| f.params.clone());
                    match method {
                        Some(params) if params.first().is_some_and(|p| p.name == "self") => {
                            self.expr(base, params[0].pass == Pass::Sink)?;
                            self.args(args, &params[1..], Pass::Sink)?;
                        }
                        _ => {
                            self.expr(base, false)?;
                            self.args(args, &[], Pass::Sink)?;
                        }
                    }
                }
                // A function value (local closure binding or any other callee
                // expression): like the checker, the callee is used owned and
                // its arguments follow the modes of its function type.
                _ => {
                    let passes: Vec<Pass> = match self.ty_of(callee) {
                        Type::Function { params, .. } => params.iter().map(|(p, _)| *p).collect(),
                        _ => Vec::new(),
                    };
                    self.expr(callee, true)?;
                    self.args_by_pass(args, &passes, Pass::Sink)?;
                }
            },
            ExprKind::Field(base, _) => self.expr(base, false)?,
            ExprKind::Index(base, index) => {
                self.expr(base, false)?;
                self.expr(index, true)?;
            }
            ExprKind::Try(x) => self.expr(x, true)?,
            ExprKind::Unary(_, x) => self.expr(x, false)?,
            ExprKind::Binary(_, a, b) => {
                self.expr(a, false)?;
                self.expr(b, false)?;
            }
            ExprKind::Assign(_, target, value) => {
                self.expr(target, false)?;
                self.expr(value, true)?;
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                self.expr(condition, false)?;
                self.branch(then, &e.span, consume)?;
                if let Some(otherwise) = otherwise {
                    self.branch(otherwise, &e.span, consume)?;
                }
            }
            ExprKind::Match { value, arms } => {
                self.expr(value, true)?;
                let scrutinee = self.ty_of(value);
                for (pat, body) in arms {
                    self.scopes.push(Vec::new());
                    self.pattern(pat, scrutinee.clone(), &body.span)?;
                    self.branch(body, &e.span, consume)?;
                    self.scopes.pop();
                }
            }
            ExprKind::Closure { params, body, .. } => {
                self.scopes.push(Vec::new());
                for p in params {
                    // A closure parameter's span may run into its body.
                    let origin = p.span.start..p.span.end.min(body.span.start).max(p.span.start);
                    self.bind(
                        &p.name,
                        origin,
                        p.ty.clone(),
                        p.pass == Pass::Sink,
                        body.span.clone(),
                    )?;
                }
                self.expr(body, true)?;
                self.scopes.pop();
            }
            ExprKind::Handle {
                state,
                body,
                ret,
                ops,
                ..
            } => {
                if let Some((_, init)) = state {
                    self.expr(init, true)?;
                }
                self.expr(body, true)?;
                for clause in std::iter::once(ret.as_ref()).chain(ops.iter()) {
                    self.scopes.push(Vec::new());
                    for pat in &clause.pats {
                        self.pattern(pat, Type::Infer, &clause.body.span)?;
                    }
                    self.branch(&clause.body, &e.span, consume)?;
                    self.scopes.pop();
                }
            }
        }
        Ok(())
    }
}
