//! Bounded expansion of locally declared and explicitly imported sugar rules.
//! Formatting reads the surface CST directly and never calls this module.
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::{
    ast::{Diagnostic, Effects, Item, Span, SugarEffectRegion},
    lexer::{self, TokenKind as K},
    parser::{self, SyntaxNode},
};
use rowan::SyntaxKind;

mod hygiene;
mod matcher;
pub(super) mod scope;
mod template;

const MAX_DEPTH: usize = 64;
const MAX_STEPS: usize = 8192;
const MAX_BYTES: usize = lexer::HARD_SOURCE_BYTES;
const WORK: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct SugarSourceSpan {
    pub expanded: Span,
    pub original: Span,
    /// Copied bytes have an exact, linear correspondence with the input.
    pub copied: bool,
}

#[derive(Clone, Debug)]
pub struct ExpandedSugar {
    pub source: String,
    pub fresh_prefix: String,
    pub regions: Vec<SugarEffectRegion>,
    /// Index in the caller-supplied imports for each region's definition.
    pub import_origins: Vec<Option<usize>>,
    pub mappings: Vec<SugarSourceSpan>,
}

impl ExpandedSugar {
    pub fn original_span(&self, span: Span) -> Span {
        let i = self
            .mappings
            .partition_point(|m| m.expanded.end <= span.start);
        if let Some(first) = self.mappings.get(i) {
            let start = first.project(span.start..span.start);
            if span.is_empty() {
                return start;
            }
            let j = self
                .mappings
                .partition_point(|m| m.expanded.start < span.end);
            let end = self.mappings[j.saturating_sub(1)].project(span.end..span.end);
            return start.start.min(end.start)..start.end.max(end.end);
        }
        self.mappings
            .last()
            .map_or(0..0, |m| m.original.end..m.original.end)
    }
}

impl SugarSourceSpan {
    fn project(&self, range: Span) -> Span {
        if !self.copied {
            return self.original.clone();
        }
        let offset = |x: usize| {
            self.original.start + x.clamp(self.expanded.start, self.expanded.end)
                - self.expanded.start
        };
        offset(range.start)..offset(range.end)
    }
}

#[derive(Clone)]
struct Rule {
    pattern: String,
    head_span: Span,
    template: String,
    span: Span,
    tail: bool,
}

struct Definition {
    name: String,
    keys: Vec<String>,
    sort: String,
    symbol: Option<String>,
    nonassoc: bool,
    emits: Effects,
    rules: Vec<Rule>,
    import: Option<usize>,
}

fn range(node: &SyntaxNode) -> Span {
    usize::from(node.text_range().start())..usize::from(node.text_range().end())
}

fn token_words(
    node: &SyntaxNode,
) -> impl Iterator<Item = rowan::SyntaxToken<parser::VesperLanguage>> + '_ {
    node.descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| {
            ![K::Whitespace, K::Comment, K::BlockComment]
                .iter()
                .any(|k| t.kind().0 == *k as u16)
        })
}

fn significant_range(node: &SyntaxNode) -> Span {
    let first = token_words(node)
        .next()
        .map(|t| usize::from(t.text_range().start()));
    first.unwrap_or_else(|| range(node).start)..range(node).end
}

fn group_contents<'a>(node: &SyntaxNode, source: &'a str) -> &'a str {
    let r = range(node);
    &source[r.start + 1..r.end - 1]
}

fn diag(message: impl Into<String>, span: Span) -> Diagnostic {
    Diagnostic::new("E0141", message, span)
}

fn limit(span: Span) -> Diagnostic {
    Diagnostic::new(
        "E0143",
        "sugar expansion exceeded its depth, size, or work limit",
        span,
    )
}

fn charge(work: &mut usize, units: usize, span: Span) -> Result<(), Diagnostic> {
    *work = work.checked_sub(units).ok_or_else(|| limit(span))?;
    Ok(())
}

fn parse(source: &str, include_tests: bool) -> Result<parser::Parse, Diagnostic> {
    parser::parse_expansion_scopes(
        source,
        parser::ParseLimits {
            max_source_bytes: MAX_BYTES,
            max_tokens: lexer::HARD_TOKENS,
            ..Default::default()
        },
        include_tests,
    )
    .map_err(|e| e.diagnostic(0))
}

fn read_definition(
    source: &str,
    decl: &SyntaxNode,
    import: Option<usize>,
) -> Result<Definition, Diagnostic> {
    let span = range(decl);
    let mut header = token_words(decl)
        .skip_while(|t| t.text() != "sugar")
        .skip(1);
    let name = header
        .next()
        .ok_or_else(|| diag("missing sugar name", span.clone()))?
        .text()
        .to_owned();
    let sort = header
        .nth(1)
        .ok_or_else(|| diag("missing sugar sort", span.clone()))?
        .text()
        .to_owned();
    let header: Vec<_> = header
        .take_while(|t| t.kind().0 != K::LBrace as u16)
        .collect();
    let symbol = decl
        .children()
        .find(|n| n.kind() == parser::SUGAR_OPERATOR)
        .map(|n| n.text().to_string());
    let nonassoc = header.iter().any(|t| t.text() == "none");
    let mut emits = Effects::default();
    if let Some(i) = header.iter().position(|t| t.text() == "emits") {
        let start = usize::from(header[i + 1].text_range().start());
        let end = usize::from(header.last().unwrap().text_range().end());
        // Reuse core effect-row parsing, including labels and type arguments.
        let wrapped = format!("fn __sugar_row() {} {{}}", &source[start..end]);
        let parsed = parser::rust_parse(&wrapped);
        if !parsed.diagnostics.is_empty() {
            return Err(diag("invalid sugar effect row", span));
        }
        if let Some(Item::Function(f)) = parsed.module.items.into_iter().next() {
            emits = f.effects;
            if emits.tail.is_some() {
                return Err(diag("sugar emits must declare a closed effect row", span));
            }
        }
    }
    let mut rules = Vec::new();
    let mut keys = Vec::new();
    for rule in decl.children().filter(|n| n.kind() == parser::SUGAR_RULE) {
        let pattern = rule
            .children()
            .find(|n| n.kind() == parser::SUGAR_PATTERN)
            .unwrap();
        let body = rule
            .children()
            .find(|n| n.kind() == parser::SUGAR_TEMPLATE)
            .unwrap();
        let text = group_contents(&pattern, source).to_owned();
        let mut words = token_words(&pattern).skip(1);
        let mut head = words.next().unwrap();
        if head.text() == "ghost" {
            head = words
                .next()
                .ok_or_else(|| diag("missing ghost sugar head", range(&rule)))?;
        }
        let key = if let Some(op) = &symbol {
            op.clone()
        } else {
            if !matches!(head.kind().0,x if [K::Lower,K::State,K::Linear,K::With,K::Eff].iter().any(|k|x==*k as u16))
            {
                return Err(diag(
                    "sugar rule needs a literal head word",
                    range(&pattern),
                ));
            }
            head.text().to_owned()
        };
        if !keys.contains(&key) {
            keys.push(key);
        }
        let tail = matcher::captures_tail(&text);
        if tail && sort != "stmt" {
            return Err(diag(
                "a block-tail hole requires statement sugar",
                range(&rule),
            ));
        }
        rules.push(Rule {
            pattern: text,
            head_span: usize::from(head.text_range().start()) - range(&pattern).start - 1
                ..usize::from(head.text_range().end()) - range(&pattern).start - 1,
            template: group_contents(&body, source).to_owned(),
            span: range(&rule),
            tail,
        });
    }
    Ok(Definition {
        keys,
        name,
        sort,
        symbol,
        nonassoc,
        emits,
        rules,
        import,
    })
}

type Bindings = BTreeMap<String, (usize, bool)>;

#[derive(Default)]
struct Registry {
    defs: Vec<Definition>,
    heads: Bindings,
    scopes: Vec<Bindings>,
    imported_scope: Vec<usize>,
}

fn bind(
    defs: &[Definition],
    heads: &mut Bindings,
    id: usize,
    visible: &str,
) -> Result<(), Diagnostic> {
    let def = defs
        .get(id)
        .ok_or_else(|| diag("unknown imported sugar declaration", 0..0))?;
    let alias = def.symbol.is_none() && visible != def.name;
    let keys = if alias {
        std::slice::from_ref(&visible)
    } else {
        &[]
    };
    for key in keys
        .iter()
        .copied()
        .chain(def.keys.iter().filter(|_| !alias).map(String::as_str))
    {
        if heads.insert(key.to_owned(), (id, alias)).is_some() {
            return Err(diag(
                format!("duplicate sugar binding for `{key}`"),
                def.rules[0].span.clone(),
            ));
        }
    }
    Ok(())
}

impl Registry {
    fn get(
        &self,
        key: &str,
        call: &Span,
        author: Option<usize>,
    ) -> Result<(&Definition, bool), Diagnostic> {
        let env = author
            .and_then(|id| self.imported_scope.get(id))
            .map_or(&self.heads, |&id| &self.scopes[id]);
        env.get(key)
            .map(|&(id, alias)| (&self.defs[id], alias))
            .ok_or_else(|| {
                Diagnostic::new("E0142", format!("unresolved sugar `{key}`"), call.clone())
            })
    }
}

fn head_range(node: &SyntaxNode) -> Span {
    node.children()
        .find(|n| n.kind() == parser::SUGAR_OPERATOR)
        .map(|n| range(&n))
        .unwrap_or_else(|| {
            let s = significant_range(node).start;
            s..s + 1
        })
}

fn author(state: &ExpandedSugar, node: &SyntaxNode) -> Option<usize> {
    let span = head_range(node);
    state
        .regions
        .iter()
        .zip(&state.import_origins)
        .rev()
        .filter(|(r, _)| {
            r.expanded.start <= span.start
                && span.end <= r.expanded.end
                && !r
                    .holes
                    .iter()
                    .any(|h| h.start <= span.start && span.end <= h.end)
        })
        .min_by_key(|(r, _)| r.expanded.len())
        .and_then(|(_, source)| *source)
}

fn fresh_prefix<'a>(sources: impl Iterator<Item = &'a str>) -> String {
    const BASE: &str = "__vesper_sugar_";
    let mut used = BTreeSet::new();
    for source in sources {
        for (start, _) in source.match_indices(BASE) {
            let tail = &source[start + BASE.len()..];
            let n = tail.bytes().take_while(|b| b.is_ascii_digit()).count();
            if tail.as_bytes().get(n) == Some(&b'_')
                && let Ok(i) = tail[..n].parse::<usize>()
            {
                used.insert(i);
            }
        }
    }
    let id = (0..=used.len()).find(|i| !used.contains(i)).unwrap();
    format!("{BASE}{id}_")
}

fn operator(node: &SyntaxNode) -> Option<String> {
    node.children()
        .find(|n| n.kind() == parser::SUGAR_OPERATOR)
        .map(|n| n.text().to_string())
}

fn call_key(node: &SyntaxNode) -> String {
    if let Some(key) = operator(node) {
        return key;
    }
    let mut tokens = token_words(node);
    let first = tokens.next().unwrap();
    if first.text() == "ghost"
        && let Some(next) = tokens.next()
        && next.kind().0 == K::Lower as u16
    {
        return next.text().to_owned();
    }
    first.text().to_owned()
}

fn call_node(k: SyntaxKind) -> bool {
    matches!(
        k,
        parser::SUGAR_ITEM
            | parser::SUGAR_PAT
            | parser::SUGAR_STMT
            | parser::SUGAR_EXPR
            | parser::DOTTED_EXPR
            | parser::PIPE_EXPR
            | parser::TAGGED_LITERAL
    )
}

fn fits_sort(k: SyntaxKind, sort: &str) -> bool {
    match k {
        parser::SUGAR_ITEM => sort == "item",
        parser::SUGAR_PAT => sort == "pat",
        parser::SUGAR_STMT => matches!(sort, "stmt" | "expr"),
        parser::SUGAR_EXPR => sort == "expr",
        parser::PIPE_EXPR | parser::DOTTED_EXPR => sort == "infix",
        parser::TAGGED_LITERAL => sort == "tag",
        _ => false,
    }
}

/// End of the statement's enclosing lexical block. Tokens inside comments and
/// strings are indivisible and cannot impersonate delimiters.
fn block_end(node: &SyntaxNode, work: &mut usize) -> Result<usize, Diagnostic> {
    let mut depth = 0usize;
    let mut token = node.last_token().and_then(|t| t.next_token());
    while let Some(t) = token {
        charge(work, 1, range(node))?;
        let kind = t.kind().0;
        if kind == K::LBrace as u16 {
            depth += 1;
        }
        if kind == K::RBrace as u16 {
            if depth == 0 {
                return Ok(t.text_range().start().into());
            }
            depth -= 1;
        }
        token = t.next_token();
    }
    Err(diag(
        "statement sugar needs an enclosing block",
        range(node),
    ))
}

struct Edit {
    old: Span,
    text: String,
    region: Option<SugarEffectRegion>,
    import: Option<usize>,
    // New (replacement-relative) byte ranges copied from old source ranges.
    copies: Vec<(Span, Span)>,
    origin: Span,
}

impl ExpandedSugar {
    fn apply(&mut self, edit: Edit, work: &mut usize) -> Result<(), Diagnostic> {
        let Edit {
            old,
            text,
            region,
            import,
            copies,
            origin,
        } = edit;
        let end = old.start + text.len();
        let size = self.source.len() - old.len() + text.len();
        if size > MAX_BYTES {
            return Err(limit(origin));
        }
        charge(
            work,
            size + self.mappings.len() + self.regions.len(),
            origin.clone(),
        )?;
        // A caller-owned fragment inside a newly expanded template follows
        // only its copies. The generated operations around it stay subject
        // to the outer rule's ceiling, even when the inner rule allows more.
        let project = |r: &Span| -> Vec<Span> {
            if r.end <= old.start {
                return vec![r.clone()];
            }
            if r.start >= old.end {
                let shifted = end + (r.start - old.end)..end + (r.end - old.end);
                return vec![shifted];
            }
            if r.start <= old.start && r.end >= old.end {
                let enclosing = r.start..end + (r.end - old.end);
                return vec![enclosing];
            }
            let mut spans = Vec::new();
            if r.start < old.start {
                spans.push(r.start..old.start);
            }
            for (new, prior) in &copies {
                let a = r.start.max(prior.start);
                let b = r.end.min(prior.end);
                if a < b && new.len() == prior.len() {
                    let base = old.start + new.start;
                    spans.push(base + (a - prior.start)..base + (b - prior.start));
                }
            }
            if r.end > old.end {
                spans.push(end..end + (r.end - old.end));
            }
            spans
        };
        let mut rebased = Vec::new();
        let mut import_origins = Vec::new();
        for (r, source) in self.regions.drain(..).zip(self.import_origins.drain(..)) {
            charge(
                work,
                (1 + r.holes.len() + r.places.len()).saturating_mul(copies.len() + 1),
                origin.clone(),
            )?;
            let regions = project(&r.expanded);
            let holes: Vec<_> = r.holes.iter().flat_map(&project).collect();
            let places: Vec<_> = r
                .places
                .iter()
                .flat_map(|(p, mode)| project(p).into_iter().map(|span| (span, *mode)))
                .collect();
            for expanded in regions {
                rebased.push(SugarEffectRegion {
                    expanded,
                    holes: holes.clone(),
                    places: places.clone(),
                    ..r.clone()
                });
                import_origins.push(source);
            }
        }
        self.regions = rebased;
        self.import_origins = import_origins;
        let mut mappings = Vec::new();
        let mut emit = |from: Span, to: usize| {
            let at = self
                .mappings
                .partition_point(|m| m.expanded.end <= from.start);
            for m in &self.mappings[at..] {
                if m.expanded.start >= from.end {
                    break;
                }
                let a = m.expanded.start.max(from.start);
                let b = m.expanded.end.min(from.end);
                if a < b {
                    mappings.push(SugarSourceSpan {
                        expanded: to + a - from.start..to + b - from.start,
                        original: m.project(a..b),
                        copied: m.copied,
                    });
                }
            }
        };
        emit(0..old.start, 0);
        for (new, prior) in &copies {
            if new.len() == prior.len() {
                emit(prior.clone(), old.start + new.start);
            }
        }
        emit(old.end..self.source.len(), end);
        let mut fill = 0;
        let mut complete = Vec::new();
        for m in mappings {
            if fill < m.expanded.start {
                complete.push(SugarSourceSpan {
                    expanded: fill..m.expanded.start,
                    original: origin.clone(),
                    copied: false,
                });
            }
            fill = m.expanded.end;
            complete.push(m);
        }
        if fill < size {
            complete.push(SugarSourceSpan {
                expanded: fill..size,
                original: origin,
                copied: false,
            });
        }
        self.mappings = complete;
        self.source.replace_range(old, text.as_str());
        if let Some(r) = region {
            self.regions.push(r);
            self.import_origins.push(import);
        }
        Ok(())
    }
}

pub fn expand_sugar(source: &str) -> Result<Option<String>, Diagnostic> {
    Ok(expand_sugar_mapped(source)?.map(|s| s.source))
}

pub fn expand_sugar_mapped(source: &str) -> Result<Option<ExpandedSugar>, Diagnostic> {
    expand_sugar_with_imports(source, &[])
}

/// Compile sugar using the same test declaration visibility as the core parser.
pub fn expand_sugar_for_execution(
    source: &str,
    include_tests: bool,
) -> Result<Option<ExpandedSugar>, Diagnostic> {
    expand_sugar_with_scopes_for_execution(source, &[], &[], &[], include_tests)
}

/// Resolve imported declaration slices under the names visible to this module.
pub fn expand_sugar_with_imports(
    source: &str,
    imports: &[(&str, &str)],
) -> Result<Option<ExpandedSugar>, Diagnostic> {
    let visible: Vec<_> = (0..imports.len()).collect();
    expand_sugar_with_scopes(source, imports, &[], &visible)
}

/// Resolve nested template calls in their defining module. Scope entries
/// identify an imported declaration and the alias under which it is visible.
pub fn expand_sugar_with_scopes(
    source: &str,
    imports: &[(&str, &str)],
    scopes: &[&[(String, usize)]],
    visible: &[usize],
) -> Result<Option<ExpandedSugar>, Diagnostic> {
    expand_sugar_with_scopes_for_execution(source, imports, scopes, visible, true)
}

/// Package compiler entry point with explicit test visibility.
pub fn expand_sugar_with_scopes_for_execution(
    source: &str,
    imports: &[(&str, &str)],
    scopes: &[&[(String, usize)]],
    visible: &[usize],
    include_tests: bool,
) -> Result<Option<ExpandedSugar>, Diagnostic> {
    if source.len() > MAX_BYTES || imports.len() > MAX_STEPS {
        return Err(limit(0..0));
    }
    let mut work = WORK;
    let mut registry = Registry::default();
    if !scopes.is_empty() && scopes.len() != imports.len() {
        return Err(diag("incomplete sugar definition scopes", 0..0));
    }
    for (i, (_, source)) in imports.iter().enumerate() {
        charge(&mut work, source.len(), 0..0)?;
        let p = parse(source, true)?;
        if let Some(d) = p.diagnostics.first() {
            return Err(d.clone());
        }
        let decl = p
            .syntax()
            .descendants()
            .find(|n| n.kind() == parser::SUGAR_DECL)
            .ok_or_else(|| diag("imported sugar binding is missing its declaration", 0..0))?;
        registry.defs.push(read_definition(source, &decl, Some(i))?);
    }
    for &id in visible {
        let (alias, _) = imports
            .get(id)
            .ok_or_else(|| diag("invalid sugar import", 0..0))?;
        bind(&registry.defs, &mut registry.heads, id, alias)?;
    }
    let mut shared = BTreeMap::new();
    for &scope in scopes {
        let key = (scope.as_ptr() as usize, scope.len());
        let index = if let Some(&id) = shared.get(&key) {
            id
        } else {
            let mut env = Bindings::new();
            for (name, id) in scope {
                charge(&mut work, name.len() + 1, 0..0)?;
                bind(&registry.defs, &mut env, *id, name)?;
            }
            let id = registry.scopes.len();
            registry.scopes.push(env);
            shared.insert(key, id);
            id
        };
        registry.imported_scope.push(index);
    }
    let mark = fresh_prefix(std::iter::once(source).chain(imports.iter().map(|(_, decl)| *decl)));
    let mut state = ExpandedSugar {
        fresh_prefix: mark.clone(),
        source: source.into(),
        mappings: vec![SugarSourceSpan {
            original: 0..source.len(),
            expanded: 0..source.len(),
            copied: true,
        }],
        regions: Vec::new(),
        import_origins: Vec::new(),
    };
    let mut changed = false;
    let mut steps = 0;
    for depth in 0..=MAX_DEPTH {
        charge(&mut work, state.source.len(), 0..0)?;
        let parsed = parse(&state.source, include_tests)?;
        let root = parsed.syntax();
        let decls: Vec<_> = root
            .descendants()
            .filter(|n| n.kind() == parser::SUGAR_DECL)
            .collect();
        let calls: Vec<_> = root.descendants().filter(|n| call_node(n.kind())).collect();
        let starts: Vec<_> = calls.iter().map(|n| significant_range(n).start).collect();
        let has_calls = !calls.is_empty();
        if let Some(d) = parsed.diagnostics.into_iter().next() {
            // Preserve the existing parser's diagnostics for ordinary input.
            if !changed && decls.is_empty() && !has_calls {
                return Ok(None);
            }
            let mut mapped =
                Diagnostic::new(&d.code, d.message, state.original_span(d.span.clone()));
            if let Some(r) = state
                .regions
                .iter()
                .rev()
                .find(|r| r.expanded.start <= d.span.start && d.span.start <= r.expanded.end)
            {
                mapped = mapped.with_related(r.rule.clone(), "expanded from this sugar rule");
            }
            return Err(mapped);
        }
        if !has_calls && decls.is_empty() {
            if changed {
                hygiene::protect(&mut state, &mark, &mut work, parsed.module)?;
            }
            return Ok(changed.then_some(state));
        }
        if depth == MAX_DEPTH {
            return Err(limit(state.regions.last().map_or(0..0, |r| r.call.clone())));
        }
        let mut edits = Vec::new();
        for decl in decls {
            if changed {
                return Err(diag(
                    "a sugar template cannot introduce new sugar declarations",
                    state.original_span(range(&decl)),
                ));
            }
            let def = read_definition(&state.source, &decl, None)?;
            let name = def.name.clone();
            let id = registry.defs.len();
            registry.defs.push(def);
            bind(&registry.defs, &mut registry.heads, id, &name)?;
            let old = range(&decl);
            edits.push(Edit {
                old: old.clone(),
                origin: old,
                text: " ".into(),
                region: None,
                import: None,
                copies: vec![],
            });
        }
        let mut skip_to = 0;
        let mut pending = VecDeque::from(calls);
        while let Some(node) = pending.pop_front() {
            let raw = range(&node);
            if raw.start < skip_to {
                continue;
            }
            let call = significant_range(&node);
            let original = state.original_span(call.clone());
            steps += 1;
            if steps > MAX_STEPS {
                return Err(limit(original));
            }
            let key = call_key(&node);
            let (def, alias) = registry.get(&key, &original, author(&state, &node))?;
            if !fits_sort(node.kind(), &def.sort) {
                return Err(diag(
                    format!(
                        "sugar `{}` has sort {}, incompatible with this use",
                        def.name, def.sort
                    ),
                    original,
                )
                .with_related(def.rules[0].span.clone(), "declared by this rule"));
            }
            if def.symbol.is_some() {
                for child in node
                    .children()
                    .filter(|n| matches!(n.kind(), parser::PIPE_EXPR | parser::DOTTED_EXPR))
                {
                    let op = operator(&child).unwrap();
                    let (inner, _) = registry.get(
                        &op,
                        &state.original_span(range(&child)),
                        author(&state, &child),
                    )?;
                    if (def.nonassoc || inner.nonassoc)
                        && def.symbol != inner.symbol
                        && operator_level(def) == operator_level(inner)
                    {
                        return Err(Diagnostic::new(
                            "E0140",
                            "parenthesize mixed non-associative sugar operators",
                            original,
                        ));
                    }
                }
            }
            let inv = &state.source[call.clone()];
            let mut chosen = None;
            let mut tail_end = None;
            for rule in &def.rules {
                let tail = if rule.tail {
                    let end = if let Some(end) = tail_end {
                        end
                    } else {
                        let end = block_end(&node, &mut work)?;
                        tail_end = Some(end);
                        end
                    };
                    Some(&state.source[call.end..end])
                } else {
                    None
                };
                let mut pattern = Cow::Borrowed(rule.pattern.as_str());
                if alias {
                    pattern.to_mut().replace_range(rule.head_span.clone(), &key);
                }
                if let Some(captures) = matcher::match_rule(&pattern, inv, tail, &mut work)
                    .map_err(|m| {
                        diag(m, original.clone())
                            .with_related(rule.span.clone(), "while matching this sugar rule")
                    })?
                {
                    chosen = Some((rule, captures));
                    break;
                }
            }
            let (rule, captures) = chosen.ok_or_else(|| {
                diag(
                    format!("no rule of `{}` matches this sugar use", def.name),
                    original.clone(),
                )
                .with_related(def.rules[0].span.clone(), "candidate sugar rule")
            })?;
            // An exit-aware scope must see the exits in nested expansions.
            // Expand the tail first; its generated source retains its origin.
            let mut postpone = false;
            for value in captures
                .values()
                .filter(|c| c.sort == "scope")
                .flat_map(|c| &c.values)
            {
                let base = if value.from_tail {
                    call.end
                } else {
                    call.start
                };
                let start = base + value.span.start;
                let end = base + value.span.end;
                if starts
                    .get(starts.partition_point(|&n| n < start || n <= call.start))
                    .is_some_and(|&n| n < end)
                {
                    postpone = true;
                } else if !value.from_tail {
                    // Stage 1 deliberately leaves braced heads as token trees.
                    // Reparse a scope-typed hole as a block. Padding preserves
                    // absolute byte offsets for rule lookup, edits and labels.
                    const HEADER: &str = "fn f()";
                    if start < HEADER.len() {
                        return Err(diag("scope has no enclosing function", original));
                    }
                    charge(&mut work, end, original.clone())?;
                    let wrapped = format!(
                        "{HEADER}{}{}",
                        " ".repeat(start - HEADER.len()),
                        &state.source[start..end]
                    );
                    let inside = parse(&wrapped, include_tests)?.syntax();
                    let nested: Vec<_> = inside
                        .descendants()
                        .filter(|n| call_node(n.kind()))
                        .collect();
                    if !nested.is_empty() {
                        postpone = true;
                        for n in nested.into_iter().rev() {
                            pending.push_front(n);
                        }
                    }
                }
            }
            if postpone {
                continue;
            }
            let head = head_range(&node);
            if state
                .regions
                .iter()
                .zip(&state.import_origins)
                .any(|(r, &import)| {
                    import == def.import
                        && r.rule == rule.span
                        && r.expanded.start <= head.start
                        && head.end <= r.expanded.end
                        && !r
                            .holes
                            .iter()
                            .any(|h| h.start <= head.start && head.end <= h.end)
                })
            {
                return Err(Diagnostic::new(
                    "E0143",
                    "recursive sugar template does not decrease expansion rank",
                    original,
                )
                .with_related(
                    rule.span.clone(),
                    "this rule occurs earlier in the same expansion",
                ));
            }
            let mut rendered = template::render(
                &rule.template,
                &captures,
                &format!("{mark}{steps}_"),
                MAX_BYTES,
            )
            .map_err(|m| {
                diag(m, original.clone()).with_related(rule.span.clone(), "in this sugar template")
            })?;
            if node.kind() == parser::SUGAR_PAT {
                hygiene::pattern(&mut rendered, &format!("{mark}{steps}_")).map_err(|m| {
                    diag(m, original.clone())
                        .with_related(rule.span.clone(), "in this pattern template")
                })?;
            }
            if captures
                .values()
                .any(|c| c.sort == "scope" && !c.values.is_empty())
            {
                charge(&mut work, state.source.len(), original.clone())?;
            }
            let scope_items = scope::lower(
                &mut rendered,
                &captures,
                &state.source,
                call.clone(),
                &format!("{mark}{steps}_"),
                MAX_BYTES,
            )
            .map_err(|m| {
                diag(m, original.clone()).with_related(rule.span.clone(), "in this scope template")
            })?;
            if !scope_items.is_empty() {
                let at = state.source.len();
                edits.push(Edit {
                    old: at..at,
                    text: format!("\n{scope_items}\n"),
                    region: None,
                    import: None,
                    copies: Vec::new(),
                    origin: original.clone(),
                });
            }
            let end = if rule.tail {
                tail_end.unwrap()
            } else {
                call.end
            };
            let mut text = rendered.text;
            let expr = matches!(def.sort.as_str(), "expr" | "infix" | "tag");
            let prefix = usize::from(expr);
            if expr {
                text = format!("({text}\n)");
            }
            if node.kind() == parser::SUGAR_STMT && !rule.tail && !text.trim_end().ends_with(';') {
                let next = node.last_token().and_then(|t| t.next_token());
                let next = std::iter::successors(next, |t| t.next_token()).find(|t| {
                    ![K::Whitespace, K::Comment, K::BlockComment]
                        .iter()
                        .any(|k| t.kind().0 == *k as u16)
                });
                if inv.trim_end().ends_with(';')
                    || next.is_some_and(|t| t.kind().0 != K::RBrace as u16)
                {
                    text.push(';');
                }
            }
            let absolute = |r: Span| call.start + prefix + r.start..call.start + prefix + r.end;
            let mut copies: Vec<_> = rendered
                .origins
                .into_iter()
                .map(|(new, old, tail)| {
                    let base = if tail { call.end } else { call.start };
                    (
                        new.start + prefix..new.end + prefix,
                        base + old.start..base + old.end,
                    )
                })
                .collect();
            copies.sort_by_key(|(r, _)| r.start);
            let region = SugarEffectRegion {
                expanded: call.start..call.start + text.len(),
                holes: rendered.holes.into_iter().map(absolute).collect(),
                places: rendered
                    .places
                    .into_iter()
                    .map(|(r, p)| (absolute(r), p))
                    .collect(),
                emits: def.emits.clone(),
                call: original.clone(),
                rule: rule.span.clone(),
            };
            edits.push(Edit {
                old: call.start..end,
                text,
                region: Some(region),
                import: def.import,
                copies,
                origin: original,
            });
            skip_to = end;
        }
        edits.sort_by_key(|e| e.old.start);
        for edit in edits.into_iter().rev() {
            state.apply(edit, &mut work)?;
        }
        changed = true;
    }
    unreachable!()
}

fn operator_level(def: &Definition) -> Option<u8> {
    let op = def.symbol.as_deref()?;
    let lexed = lexer::sugar_lex_with_limits(op, Default::default()).ok()?;
    parser::sugar_operator(&lexed.tokens, 0).map(|(level, _)| level)
}
