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
        }
    }
}

/// A syntax-damaged function and the cut its repaired prefix ends at.
#[derive(Clone, Debug)]
pub(crate) struct Region {
    pub name: String,
    /// Original declaration span.
    pub span: Span,
    /// Byte offset where the repaired prefix ends (the inserted `}`).
    pub cut: usize,
    /// The unfinished code: from the first token after the cut through the
    /// first syntax damage.
    pub damage: Span,
}

enum Basis {
    Checked(Arc<CheckedSource>),
    Repaired { checked: Box<Checked> },
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
        self.origin.end <= at && at <= self.scope.end && self.scope.start <= self.origin.start
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

    /// Build a repaired-prefix view for syntax-damaged original source.
    pub(crate) fn recovered(
        syntax: &RecoverySyntax,
        limits: AnalysisLimits,
    ) -> Result<Self, Unavailable> {
        let source = syntax.source();
        let lexed = lexer::lex_with_limits(
            source,
            lexer::LexLimits {
                max_source_bytes: limits.max_source_bytes,
                max_tokens: 16_384,
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
        let mut bytes = source.as_bytes().to_vec();
        let mut regions = Vec::new();
        let declarations = syntax.declarations();
        for (index, d) in declarations.iter().enumerate() {
            if d.status == S::Complete {
                continue;
            }
            if d.status == S::Opaque {
                return Err(Unavailable::OpaqueDeclaration);
            }
            if d.kind != D::Function {
                return Err(Unavailable::DamagedContext);
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
                        .filter(|diag| {
                            d.span.start <= diag.span.start && diag.span.start < d.span.end
                        })
                        .map(|diag| diag.span.clone()),
                )
                .min_by_key(|span| span.start)
                .unwrap_or(d.span.end..d.span.end);
            let (Some(body), Some(name)) = (&d.body_span, &d.name_span) else {
                return Err(Unavailable::DamagedHeader);
            };
            if damage.start <= body.start || source.as_bytes().get(body.start) != Some(&b'{') {
                return Err(Unavailable::DamagedHeader);
            }
            let cut = statement_cut(&tokens, body.start, damage.start);
            if cut >= d.span.end {
                return Err(Unavailable::RepairUnparsed);
            }
            for byte in &mut bytes[cut..d.span.end] {
                if *byte != b'\n' && *byte != b'\r' {
                    *byte = b' ';
                }
            }
            let unfinished = cut
                + source[cut..]
                    .find(|c: char| !c.is_whitespace())
                    .unwrap_or(0);
            let damage = if unfinished < damage.end {
                unfinished..damage.end
            } else {
                damage
            };
            bytes[cut] = b'}';
            regions.push(Region {
                name: source[name.clone()].to_owned(),
                span: d.span.clone(),
                cut,
                damage,
            });
        }
        // Sites outside a damaged function cannot be repaired by a function cut.
        if syntax.sites().iter().any(|site| {
            site.owner
                .and_then(|i| declarations.get(i))
                .is_none_or(|d| d.kind != D::Function || d.status == S::Complete)
        }) {
            return Err(Unavailable::DamagedContext);
        }
        let repaired = String::from_utf8(bytes).map_err(|_| Unavailable::RepairUnparsed)?;
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
        match frontend::check_source(text, limits) {
            Ok(_) | Err(AnalysisFailure::Semantic { .. }) => {}
            Err(AnalysisFailure::Parse { .. }) => return Err(Unavailable::RepairUnparsed),
            Err(AnalysisFailure::Resource { .. }) => return Err(Unavailable::Resource),
            Err(_) => return Err(Unavailable::Guard),
        }
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
        if let Some(region) = self
            .regions
            .iter()
            .find(|region| region.span.start <= at && at <= region.span.end)
        {
            let facts = self
                .functions
                .iter()
                .find(|f| region.span.start <= f.span.start && f.span.start < region.span.end)?;
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

    /// Live linear bindings at each damaged function's cut: the poisoned set.
    pub(crate) fn poisoned(&self) -> Vec<(&Region, Vec<&Binding>)> {
        self.regions
            .iter()
            .filter_map(|region| {
                let function = self.functions.iter().find(|f| {
                    region.span.start <= f.span.start && f.span.start < region.span.end
                })?;
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

/// The end of the last complete top-level statement of the body that starts
/// before `damage`, or just after the opening brace.
fn statement_cut(tokens: &[&lexer::Token], body: usize, damage: usize) -> usize {
    let mut cut = body + 1;
    let mut depth = 0usize;
    let mut iter = tokens
        .iter()
        .skip_while(|t| t.span.start < body)
        .take_while(|t| t.span.start < damage)
        .peekable();
    while let Some(token) = iter.next() {
        match token.kind {
            K::LBrace | K::LParen | K::LBracket => depth += 1,
            K::RBrace | K::RParen | K::RBracket => {
                depth = depth.saturating_sub(1);
                // A block statement such as `if c { .. }` ends at its brace
                // unless an `else` or a continuation follows.
                if depth == 1
                    && token.kind == K::RBrace
                    && iter.peek().is_some_and(|next| {
                        matches!(next.kind, K::Let | K::Lower | K::Upper)
                            && next.span.start < damage
                    })
                {
                    cut = token.span.end;
                }
            }
            K::Semi if depth == 1 => cut = token.span.end,
            _ => {}
        }
    }
    cut
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
        Ok(())
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
            PatternKind::Constructor { fields, .. } => {
                for (_, p) in fields {
                    self.pattern(p, Type::Infer, scope)?;
                }
                Ok(())
            }
            PatternKind::Wild | PatternKind::Literal(_) => Ok(()),
        }
    }
    fn args(&mut self, args: &[Arg], params: &[Param], default: Pass) -> Result<(), Unavailable> {
        for (i, a) in args.iter().enumerate() {
            let pass = if a.explicit {
                a.pass
            } else {
                params.get(i).map(|p| p.pass).unwrap_or(default)
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
                _ => {
                    self.expr(callee, false)?;
                    self.args(args, &[], Pass::Sink)?;
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
                for (pat, body) in arms {
                    self.scopes.push(Vec::new());
                    self.pattern(pat, Type::Infer, &body.span)?;
                    self.branch(body, &e.span, consume)?;
                    self.scopes.pop();
                }
            }
            ExprKind::Closure { params, body, .. } => {
                self.scopes.push(Vec::new());
                for p in params {
                    self.bind(
                        &p.name,
                        p.span.clone(),
                        p.ty.clone(),
                        false,
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
