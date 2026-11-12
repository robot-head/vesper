//! Editor-only driver. Failed AST productions never leave this module.
use super::*;
use crate::recovery::{
    self, Declaration, DeclarationKind as DK, DeclarationStatus as DS, Error, Limits, RecoverySite,
    RecoverySyntax, Resource, SiteKind,
};
use rowan::{GreenToken, NodeOrToken};
use std::sync::Arc;

struct Work(usize);
impl Work {
    fn charge(&mut self, n: usize) -> Result<(), Error> {
        self.0 = self.0.checked_sub(n).ok_or(Error::Budget(Resource::Work))?;
        Ok(())
    }
    fn parse(&mut self, tokens: usize, bytes: usize) -> Result<(), Error> {
        self.charge(
            tokens
                .checked_mul(tokens.checked_add(1).ok_or(Error::Budget(Resource::Work))?)
                .and_then(|n| bytes.checked_mul(4).and_then(|b| n.checked_add(b)))
                .ok_or(Error::Budget(Resource::Work))?,
        )
    }
}
fn lex(source: &str, limits: Limits, work: &mut Work) -> Result<lexer::Lexed, Error> {
    limits.preflight(source)?;
    work.charge(source.len())?;
    let lexed = lexer::lex_with_limits(
        source,
        lexer::LexLimits {
            max_source_bytes: limits.parse.max_source_bytes,
            max_tokens: limits.parse.max_tokens,
            max_diagnostics: limits.parse.max_diagnostics,
        },
    )
    .map_err(|_| Error::Budget(Resource::Source))?;
    if let Some(limit) = lexed.limit {
        return Err(Error::Budget(match limit.kind {
            lexer::LexLimitKind::Tokens => Resource::Tokens,
            lexer::LexLimitKind::Diagnostics => Resource::Diagnostics,
            lexer::LexLimitKind::Depth => Resource::Depth,
            _ => Resource::Source,
        }));
    }
    for t in &lexed.tokens {
        work.charge(1)?;
        if matches!(t.kind, K::Upper | K::Lower) && t.span.len() > limits.max_identifier_bytes {
            return Err(Error::Budget(Resource::Identifier));
        }
    }
    Ok(lexed)
}
fn parser<'a>(source: &'a str, tokens: Vec<Token>, limits: Limits) -> Parser<'a> {
    Parser {
        source,
        tokens,
        pos: 0,
        builder: GreenNodeBuilder::new(),
        diagnostics: vec![],
        no_struct: false,
        row_names: Default::default(),
        bound_values: Default::default(),
        model_uses: vec![],
        trait_bounds: vec![],
        pending_bounds: vec![],
        limits: limits.parse,
        depth: 0,
        halted: false,
        halt_kind: None,
        expr_heights: Default::default(),
        editor: true,
        site_limit: limits.max_sites,
        site_exhausted: false,
        sites: vec![],
        headers: vec![],
    }
}
fn next(tokens: &[Token], mut i: usize) -> usize {
    while i < tokens.len() - 1 && tokens[i].kind.trivia() {
        i += 1;
    }
    i
}
fn token_kind(tokens: &[Token], i: usize) -> K {
    tokens.get(i).map_or(K::Eof, |t| t.kind)
}
fn word(source: &str, tokens: &[Token], i: usize, text: &str) -> bool {
    tokens
        .get(i)
        .is_some_and(|t| t.kind == K::Lower && source.get(t.span.clone()) == Some(text))
}
fn prefix(source: &str, tokens: &[Token], at: usize) -> Option<(DK, Option<Span>)> {
    let mut i = at;
    if matches!(token_kind(tokens, i), K::Copy | K::Affine | K::Linear) {
        i = next(tokens, i + 1);
    }
    if word(source, tokens, i, "pub") {
        i = next(tokens, i + 1);
        if !word(source, tokens, i, "trait") && !word(source, tokens, i, "model") {
            return None;
        }
    }
    let n = next(tokens, i + 1);
    let (kind, namekind) = match token_kind(tokens, i) {
        K::Fn => (DK::Function, Some(K::Lower)),
        K::Struct => (DK::Struct, Some(K::Upper)),
        K::Enum => (DK::Enum, Some(K::Upper)),
        K::Effect => (DK::Effect, Some(K::Upper)),
        K::Impl => (DK::Impl, None),
        _ if word(source, tokens, i, "trait") => (DK::Trait, Some(K::Upper)),
        _ if word(source, tokens, i, "model") => (DK::Model, Some(K::Upper)),
        _ if word(source, tokens, i, "use") => (DK::Use, None),
        _ => return None,
    };
    let name = namekind
        .filter(|k| {
            token_kind(tokens, n) == *k
                || (kind == DK::Function && token_kind(tokens, n) == K::SelfKw)
        })
        .map(|_| tokens[n].span.clone());
    Some((kind, name))
}
fn anchor(source: &str, tokens: &[Token], at: usize) -> bool {
    prefix(source, tokens, at).is_some_and(|(kind, name)| match kind {
        DK::Impl => true,
        DK::Use => word(source, tokens, next(tokens, at + 1), "model"),
        _ => name.is_some(),
    })
}
fn closing(kind: K) -> Option<K> {
    match kind {
        K::LParen => Some(K::RParen),
        K::LBracket => Some(K::RBracket),
        K::LBrace => Some(K::RBrace),
        _ => None,
    }
}
fn macro_start(source: &str, tokens: &[Token], at: usize) -> Option<usize> {
    if !matches!(token_kind(tokens, at), K::Lower | K::Upper) {
        return None;
    }
    let n = next(tokens, at + 1);
    if token_kind(tokens, n) == K::Bang {
        let open = next(tokens, n + 1);
        return closing(token_kind(tokens, open)).map(|_| open);
    }
    if ["quote", "quasiquote", "syntax_quote"]
        .iter()
        .any(|s| word(source, tokens, at, s))
        && token_kind(tokens, n) == K::LBrace
    {
        Some(n)
    } else {
        None
    }
}
fn macro_end(
    tokens: &[Token],
    open: usize,
    limit: usize,
    limits: Limits,
    work: &mut Work,
) -> Result<Option<usize>, Error> {
    let mut stack = vec![closing(tokens[open].kind).ok_or(Error::InvalidInventory)?];
    let mut i = open + 1;
    while i < limit {
        work.charge(1)?;
        let k = tokens[i].kind;
        if let Some(close) = closing(k) {
            if stack.len() >= limits.parse.max_depth {
                return Err(Error::Budget(Resource::Depth));
            }
            stack.push(close);
        } else if matches!(k, K::RBrace | K::RParen | K::RBracket) {
            if stack.pop() != Some(k) {
                return Ok(None);
            }
            if stack.is_empty() {
                return Ok(Some(i + 1));
            }
        }
        i += 1;
    }
    Ok(None)
}
struct Extent {
    end: usize,
    body: Option<Span>,
    uncertain: bool,
    macros: Vec<Span>,
}
fn extent(
    source: &str,
    tokens: &[Token],
    start: usize,
    kind: DK,
    opaque: usize,
    limits: Limits,
    work: &mut Work,
) -> Result<Extent, Error> {
    let mut head = start;
    if matches!(token_kind(tokens, head), K::Copy | K::Affine | K::Linear) {
        head = next(tokens, head + 1);
    }
    if word(source, tokens, head, "pub") {
        head = next(tokens, head + 1);
    }
    let mut i = if kind == DK::Other { start } else { head + 1 };
    let mut stack = Vec::new();
    let mut body_start = None;
    let mut uncertain = false;
    let mut macros = vec![];
    while i < tokens.len() - 1 {
        work.charge(1)?;
        if tokens[i].span.start >= opaque {
            return Ok(Extent {
                end: tokens.len() - 1,
                body: body_start.map(|s| s..source.len()),
                uncertain: true,
                macros,
            });
        }
        if let Some(open) = macro_start(source, tokens, i) {
            let end = macro_end(tokens, open, tokens.len() - 1, limits, work)?;
            let at = tokens[i].span.start;
            let end = end.unwrap_or(tokens.len() - 1);
            if macros.len() >= limits.max_sites {
                return Err(Error::Budget(Resource::Sites));
            }
            macros.push(
                at..tokens
                    .get(end.wrapping_sub(1))
                    .map_or(source.len(), |t| t.span.end),
            );
            if end == tokens.len() - 1
                && token_kind(tokens, end.wrapping_sub(1)) != closing(tokens[open].kind).unwrap()
            {
                return Ok(Extent {
                    end,
                    body: body_start.map(|s| s..source.len()),
                    uncertain: true,
                    macros,
                });
            }
            i = end;
            continue;
        }
        if !tokens[i].kind.trivia()
            && anchor(source, tokens, i)
            && (kind == DK::Function || body_start.is_none())
        {
            return Ok(Extent {
                end: i,
                body: body_start.map(|s| s..tokens[i].span.start),
                uncertain: true,
                macros,
            });
        }
        let k = tokens[i].kind;
        if k == K::LBrace && body_start.is_none() {
            let prev = (start..i).rev().find(|n| !tokens[*n].kind.trivia());
            if prev.is_none_or(|n| tokens[n].kind != K::Dot) {
                if !stack.is_empty() {
                    uncertain = true;
                    stack.clear();
                }
                body_start = Some(tokens[i].span.start);
                stack.push(K::RBrace);
                i += 1;
                continue;
            }
        }
        if let Some(close) = closing(k) {
            if stack.len() >= limits.parse.max_depth {
                return Err(Error::Budget(Resource::Depth));
            }
            stack.push(close);
        } else if matches!(k, K::RBrace | K::RParen | K::RBracket) {
            if stack.last() == Some(&k) {
                stack.pop();
                if body_start.is_some() && stack.is_empty() {
                    return Ok(Extent {
                        end: i + 1,
                        body: body_start.map(|s| s..tokens[i].span.end),
                        uncertain,
                        macros,
                    });
                }
            } else {
                uncertain = true;
            }
        } else if k == K::Semi && body_start.is_none() {
            return Ok(Extent {
                end: i + 1,
                body: None,
                uncertain,
                macros,
            });
        }
        i += 1;
    }
    Ok(Extent {
        end: i,
        body: body_start.map(|s| s..source.len()),
        uncertain: !stack.is_empty() || (kind != DK::Other && body_start.is_none()),
        macros,
    })
}
fn raw_green(source: &str, tokens: &[Token], kind: SyntaxKind) -> GreenNode {
    GreenNode::new(
        kind,
        tokens.iter().map(|t| {
            NodeOrToken::Token(GreenToken::new(
                SyntaxKind(t.kind as u16),
                &source[t.span.clone()],
            ))
        }),
    )
}
fn ranged_tokens(tokens: &[Token], start: usize, end: usize, eof: usize) -> Vec<Token> {
    let mut out = tokens[start..end].to_vec();
    out.push(Token {
        kind: K::Eof,
        span: eof..eof,
    });
    out
}
struct ParsedItem {
    green: GreenNode,
    item: Option<Item>,
    diagnostics: Vec<Diagnostic>,
    sites: Vec<TraceSite>,
    header: Option<HeaderNote>,
    model_uses: Vec<ModelUse>,
    trait_bounds: Vec<TraitBoundSyntax>,
    exact: bool,
}
fn parse_item(source: &str, tokens: Vec<Token>, limits: Limits) -> Result<ParsedItem, Error> {
    let mut p = parser(source, tokens, limits);
    p.builder.start_node(ITEM);
    let item = p.item();
    let exact = p.at(K::Eof);
    if !exact {
        p.site(p.span(), SiteKind::Unexpected, None);
        p.error("unexpected tokens after recovered declaration");
        p.builder.start_node(ERROR_NODE);
        while !p.at(K::Eof) {
            p.bump();
        }
        p.builder.finish_node();
    }
    p.trivia();
    p.builder.finish_node();
    if p.halted {
        return Err(Error::Budget(match p.halt_kind {
            Some(lexer::LexLimitKind::Diagnostics) => Resource::Diagnostics,
            _ => Resource::Depth,
        }));
    }
    if p.site_exhausted {
        return Err(Error::Budget(Resource::Sites));
    }
    Ok(ParsedItem {
        green: p.builder.finish(),
        item,
        diagnostics: p.diagnostics,
        sites: p.sites,
        header: p.headers.into_iter().next(),
        model_uses: p.model_uses,
        trait_bounds: p.trait_bounds,
        exact,
    })
}
fn item_span(item: &Item) -> Option<Span> {
    match item {
        Item::Function(f) => Some(f.span.clone()),
        Item::Adt(a) => Some(a.span.clone()),
        Item::Effect(e) => Some(e.span.clone()),
        Item::Trait(t) => Some(t.span.clone()),
        Item::Conformance(c) => Some(c.span.clone()),
        Item::Impl { .. } => None,
    }
}
fn push_site(
    sites: &mut Vec<RecoverySite>,
    site: RecoverySite,
    limits: Limits,
) -> Result<(), Error> {
    if sites.len() >= limits.max_sites {
        return Err(Error::Budget(Resource::Sites));
    }
    sites.push(site);
    Ok(())
}

/// Parse editor syntax from the exact original source without producing a checked AST.
/// Damaged, opaque, and complete declarations remain distinct; quotas fail atomically.
pub fn parse_recovery(source: &str, limits: Limits) -> Result<RecoverySyntax, Error> {
    limits.preflight(source)?;
    let mut work = Work(limits.max_work);
    let lexed = lex(source, limits, &mut work)?;
    let opaque = lexed
        .tokens
        .iter()
        .find(|t| t.kind == K::Error)
        .map_or(source.len(), |t| t.span.start);
    let tokens = &lexed.tokens;
    let mut diagnostics = lexed.diagnostics;
    let mut declarations = Vec::new();
    let mut sites = Vec::new();
    let mut children = Vec::new();
    let mut cursor = 0;
    while cursor < tokens.len() - 1 {
        work.charge(1)?;
        let start = next(tokens, cursor);
        for t in &tokens[cursor..start] {
            children.push(NodeOrToken::Token(GreenToken::new(
                SyntaxKind(t.kind as u16),
                &source[t.span.clone()],
            )));
        }
        if start == tokens.len() - 1 {
            break;
        }
        if declarations.len() >= limits.max_declarations {
            return Err(Error::Budget(Resource::Declarations));
        }
        let owner = declarations.len();
        let (kind, name) = prefix(source, tokens, start).unwrap_or((DK::Other, None));
        let ext = extent(source, tokens, start, kind, opaque, limits, &mut work)?;
        let end = ext.end.max(start + 1).min(tokens.len() - 1);
        let span = tokens[start].span.start..tokens[end - 1].span.end;
        let lexical = span.end > opaque;
        let opaque_item = lexical
            || !ext.macros.is_empty()
            || kind == DK::Other
            || (ext.uncertain && kind != DK::Function)
            || (name.is_none() && !matches!(kind, DK::Impl | DK::Use));
        let mut status = DS::Recovered;
        let mut header_span = span.start..ext.body.as_ref().map_or(span.end, |b| b.start);
        let mut body_green_span = None;
        let mut context = None;
        let mut declared_span = span.clone();
        let green = if opaque_item {
            if lexical {
                push_site(
                    &mut sites,
                    RecoverySite {
                        span: opaque.max(span.start)..source.len(),
                        kind: SiteKind::OpaqueLexical,
                        owner: Some(owner),
                        receiver: None,
                    },
                    limits,
                )?;
            }
            for macro_span in ext.macros {
                push_site(
                    &mut sites,
                    RecoverySite {
                        span: macro_span,
                        kind: SiteKind::OpaqueMacro,
                        owner: Some(owner),
                        receiver: None,
                    },
                    limits,
                )?;
            }
            if !lexical && sites.last().is_none_or(|s| s.owner != Some(owner)) {
                push_site(
                    &mut sites,
                    RecoverySite {
                        span: span.clone(),
                        kind: SiteKind::Unexpected,
                        owner: Some(owner),
                        receiver: None,
                    },
                    limits,
                )?;
            }
            status = DS::Opaque;
            GreenNode::new(
                ITEM,
                [NodeOrToken::Node(raw_green(
                    source,
                    &tokens[start..end],
                    ERROR_NODE,
                ))],
            )
        } else {
            work.parse(end - start, span.len())?;
            let parsed = parse_item(source, ranged_tokens(tokens, start, end, span.end), limits)?;
            if kind == DK::Function {
                if let Some(header) = &parsed.header
                    && header.clean
                    && ext
                        .body
                        .as_ref()
                        .is_some_and(|b| b.start == header.body_start)
                {
                    header_span = header.start..header.body_start;
                } else {
                    status = DS::Opaque;
                }
            }
            if parsed.diagnostics.is_empty() && parsed.exact && !ext.uncertain {
                status = DS::Complete;
                if let Some(item) = &parsed.item {
                    if let Some(span) = item_span(item) {
                        declared_span = span;
                    }
                    header_span.start = declared_span.start;
                }
                if kind == DK::Function
                    && let (Some(header), Some(body)) = (&parsed.header, &ext.body)
                {
                    if header.clean && header.nongeneric {
                        context = Some(recovery::BlockContext {
                            row_names: header.row_names.clone(),
                            bound_values: header.bound_values.clone(),
                            no_struct: false,
                        });
                    }
                    body_green_span = Some(body.clone());
                }
            }
            for site in parsed.sites {
                push_site(
                    &mut sites,
                    RecoverySite {
                        span: site.span,
                        kind: site.kind,
                        owner: Some(owner),
                        receiver: site.receiver,
                    },
                    limits,
                )?;
            }
            if !parsed.diagnostics.is_empty() && sites.last().is_none_or(|s| s.owner != Some(owner))
            {
                push_site(
                    &mut sites,
                    RecoverySite {
                        span: span.clone(),
                        kind: SiteKind::Unexpected,
                        owner: Some(owner),
                        receiver: None,
                    },
                    limits,
                )?;
            }
            if parsed.diagnostics.len() > limits.parse.max_diagnostics - diagnostics.len() {
                return Err(Error::Budget(Resource::Diagnostics));
            }
            diagnostics.extend(parsed.diagnostics);
            parsed.green
        };
        if diagnostics.len() > limits.parse.max_diagnostics {
            return Err(Error::Budget(Resource::Diagnostics));
        }
        if status != DS::Complete
            && !diagnostics
                .iter()
                .any(|d| d.span.start >= span.start && d.span.start <= span.end)
        {
            if diagnostics.len() == limits.parse.max_diagnostics {
                return Err(Error::Budget(Resource::Diagnostics));
            }
            diagnostics.push(Diagnostic::new(
                "E0002",
                "declaration syntax is recovered or opaque",
                span.clone(),
            ));
        }
        declarations.push(Declaration {
            kind,
            name_span: name,
            span: declared_span,
            header_span,
            body_span: ext.body,
            body_green_span,
            status,
            body_context: context,
        });
        children.push(NodeOrToken::Node(green));
        cursor = end;
    }
    let green = GreenNode::new(MODULE, children);
    if work.0 == 0 {
        return Err(Error::Budget(Resource::Work));
    }
    recovery::assemble(
        Arc::from(source),
        green,
        diagnostics,
        declarations,
        sites,
        Limits {
            max_work: work.0,
            ..limits
        },
    )
}

pub(super) fn receiver(expr: &Expr) -> Option<recovery::Receiver> {
    let mut projections = Vec::new();
    let mut current = expr;
    while let ExprKind::Field(base, name) = &current.kind {
        if name == "<incomplete>" {
            return None;
        }
        let end = current.span.end;
        let start = end.checked_sub(name.len())?;
        projections.push(start..end);
        current = base;
    }
    let ExprKind::Name(name) = &current.kind else {
        return None;
    };
    if name.contains("::")
        || !name
            .as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_lowercase() || *b == b'_')
    {
        return None;
    }
    projections.reverse();
    Some(recovery::Receiver {
        root: current.span.clone(),
        projections,
    })
}

/// Reparse selected complete declarations from a sealed original-source snapshot.
/// Indices must be strictly increasing. Recovered/opaque productions are unavailable;
/// successful ASTs retain original absolute spans and still require normal checking.
pub fn parse_complete_items(
    snapshot: &RecoverySyntax,
    indices: &[usize],
    limits: Limits,
) -> Result<Module, Error> {
    limits.preflight(snapshot.source())?;
    if indices.len() > limits.max_declarations {
        return Err(Error::Budget(Resource::Declarations));
    }
    let mut work = Work(limits.max_work);
    let lexed = lex(snapshot.source(), limits, &mut work)?;
    let mut out = Module::default();
    let mut previous = None;
    for &index in indices {
        work.charge(1)?;
        if previous.is_some_and(|p| p >= index) {
            return Err(Error::InvalidInventory);
        }
        previous = Some(index);
        let declaration = snapshot
            .declarations()
            .get(index)
            .ok_or(Error::InvalidInventory)?;
        if declaration.status != DS::Complete {
            return Err(Error::Unsupported);
        }
        if lexed
            .diagnostics
            .iter()
            .any(|d| d.span.start < declaration.span.end && declaration.span.start < d.span.end)
        {
            return Err(Error::Unsupported);
        }
        let start = lexed
            .tokens
            .partition_point(|t| t.span.start < declaration.span.start);
        let end = lexed
            .tokens
            .partition_point(|t| t.kind != K::Eof && t.span.end <= declaration.span.end);
        if start >= end
            || lexed.tokens[start].span.start != declaration.span.start
            || lexed.tokens[end - 1].span.end != declaration.span.end
        {
            return Err(Error::InvalidSpan);
        }
        work.parse(end - start, declaration.span.len())?;
        let parsed = parse_item(
            snapshot.source(),
            ranged_tokens(&lexed.tokens, start, end, declaration.span.end),
            limits,
        )?;
        if !parsed.exact || !parsed.diagnostics.is_empty() {
            return Err(Error::Unsupported);
        }
        if let Some(item) = parsed.item {
            if item_span(&item).ok_or(Error::Unsupported)? != declaration.span {
                return Err(Error::InvalidInventory);
            }
            out.items.push(item);
        } else if parsed.model_uses.is_empty() {
            return Err(Error::Unsupported);
        }
        out.model_uses.extend(parsed.model_uses);
        out.trait_bounds.extend(parsed.trait_bounds);
    }
    Ok(out)
}

#[allow(
    dead_code,
    reason = "consumed by separately integrated incremental module"
)]
pub(crate) struct BodyContext {
    source: Arc<str>,
    body: Span,
    context: recovery::BlockContext,
}
#[allow(
    dead_code,
    reason = "consumed by separately integrated incremental module"
)]
pub(crate) fn capture_body_context(
    snapshot: &RecoverySyntax,
    index: usize,
    limits: Limits,
) -> Result<BodyContext, Error> {
    limits.preflight(snapshot.source())?;
    let d = snapshot
        .declarations()
        .get(index)
        .ok_or(Error::InvalidInventory)?;
    if d.kind != DK::Function || d.status != DS::Complete {
        return Err(Error::Unsupported);
    }
    let body = d
        .body_span
        .clone()
        .filter(|b| Some(b) == d.body_green_span.as_ref())
        .ok_or(Error::Unsupported)?;
    let stored = d.body_context.as_ref().ok_or(Error::Unsupported)?;
    let mut work = Work(limits.max_work);
    work.charge(snapshot.source().len())?;
    let count = stored
        .row_names
        .len()
        .checked_add(stored.bound_values.len())
        .ok_or(Error::Budget(Resource::Work))?;
    if count > limits.parse.max_tokens {
        return Err(Error::Budget(Resource::Tokens));
    }
    for name in stored.row_names.iter().chain(&stored.bound_values) {
        if name.len() > limits.max_identifier_bytes {
            return Err(Error::Budget(Resource::Identifier));
        }
        work.charge(
            name.len()
                .checked_add(1)
                .ok_or(Error::Budget(Resource::Work))?,
        )?;
    }
    let context = stored.clone();
    Ok(BodyContext {
        source: Arc::clone(snapshot.source_arc()),
        body,
        context,
    })
}
#[allow(
    dead_code,
    reason = "consumed by separately integrated incremental module"
)]
pub(crate) fn parse_body_fragment(
    source: &str,
    body: Span,
    context: &BodyContext,
    limits: Limits,
) -> Result<GreenNode, Error> {
    limits.preflight(source)?;
    let mut work = Work(limits.max_work);
    work.charge(
        source
            .len()
            .checked_add(context.source.len())
            .ok_or(Error::Budget(Resource::Work))?,
    )?;
    if source.get(..body.start) != context.source.get(..context.body.start)
        || source.get(body.end..) != context.source.get(context.body.end..)
    {
        return Err(Error::Unsupported);
    }
    let lexed = lex(source, limits, &mut work)?;
    if !lexed.diagnostics.is_empty() {
        return Err(Error::Unsupported);
    }
    let start = lexed.tokens.partition_point(|t| t.span.start < body.start);
    let end = lexed
        .tokens
        .partition_point(|t| t.kind != K::Eof && t.span.end <= body.end);
    if start >= end
        || lexed.tokens[start].kind != K::LBrace
        || lexed.tokens[end - 1].kind != K::RBrace
        || lexed.tokens[start].span.start != body.start
        || lexed.tokens[end - 1].span.end != body.end
    {
        return Err(Error::Unsupported);
    }
    // The same context/depth as function()->block() in the full parser.
    work.parse(end - start, body.len())?;
    let mut p = parser(
        source,
        ranged_tokens(&lexed.tokens, start, end, body.end),
        limits,
    );
    p.row_names = context.context.row_names.clone();
    p.bound_values = context.context.bound_values.clone();
    p.no_struct = context.context.no_struct;
    p.block();
    if !p.at(K::Eof)
        || !p.diagnostics.is_empty()
        || p.halted
        || !p.model_uses.is_empty()
        || !p.trait_bounds.is_empty()
    {
        return Err(Error::Unsupported);
    }
    Ok(p.builder.finish())
}

#[cfg(test)]
mod helper_tests {
    use super::*;

    #[test]
    fn last_body_fragment_is_actual_block_and_matches_full_parse_oracle() {
        let old = "// 🦀\r\nfn first()->u64{7}\r\nfn f(in arr:Vec[u64], in idx:u64)->u64{arr[idx]}";
        let snapshot = parse_recovery(old, Limits::default()).unwrap();
        let context = capture_body_context(&snapshot, 1, Limits::default()).unwrap();
        let new = old.replace("arr[idx]}", "arr[idx]+1}");
        let start = snapshot.declarations()[1].body_span.as_ref().unwrap().start;
        let actual =
            parse_body_fragment(&new, start..new.len(), &context, Limits::default()).unwrap();
        let oracle = parse_recovery(&new, Limits::default()).unwrap();
        let expected = oracle
            .syntax()
            .descendants()
            .find(|n| n.kind() == BLOCK && usize::from(n.text_range().start()) == start)
            .unwrap();
        assert_eq!(actual, expected.green().into_owned());
        assert_eq!(actual.to_string(), &new[start..]);
        assert!(
            parse_body_fragment(
                &new.replace("fn f", "fn g"),
                start..new.len(),
                &context,
                Limits::default()
            )
            .is_err()
        );
        assert!(
            parse_body_fragment(
                &new.replace("in idx", "in other"),
                start..new.len() + 2,
                &context,
                Limits::default()
            )
            .is_err()
        );
        let tight = Limits {
            max_work: new.len(),
            ..Limits::default()
        };
        assert_eq!(
            parse_body_fragment(&new, start..new.len(), &context, tight).unwrap_err(),
            Error::Budget(Resource::Work)
        );
    }

    #[test]
    fn statement_sync_does_not_anchor_inside_nested_skipped_tokens() {
        let source = "junk(let hidden=1;) let after=2;";
        let mut work = Work(Limits::default().max_work);
        let tokens = lex(source, Limits::default(), &mut work).unwrap().tokens;
        let mut p = parser(source, tokens, Limits::default());
        p.builder.start_node(MODULE);
        p.recover_statement();
        assert_eq!(&source[p.start()..], "let after=2;");
        p.builder.finish_node();
        let green = p.builder.finish();
        assert_eq!(green.to_string(), "junk(let hidden=1;)");
        assert_eq!(p.sites.len(), 1);
    }

    #[test]
    fn context_is_not_available_for_generic_or_recovered_functions() {
        for source in ["fn f[T](in x:T)->T{x}", "fn f(){x.}"] {
            let snapshot = parse_recovery(source, Limits::default()).unwrap();
            assert!(matches!(
                capture_body_context(&snapshot, 0, Limits::default()),
                Err(Error::Unsupported)
            ));
        }
    }
}
