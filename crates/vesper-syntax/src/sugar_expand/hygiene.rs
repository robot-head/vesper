//! An introduced free name resolves in the definition's value namespace.
//! When an existing caller binding would capture it, alpha-rename that
//! binding and its caller-owned uses, including uses spliced from holes.
use super::*;
use crate::ast::{ExprKind, Function, Param, Pattern, PatternKind, Statement};
use crate::lexer::Token;

struct Binding {
    name: String,
    binder: Span,
    scope: Span,
}
struct Names<'a> {
    state: &'a ExpandedSugar,
    tokens: &'a [Token],
    bindings: Vec<Binding>,
    refs: BTreeMap<String, Vec<(Span, bool)>>,
    shorthand: BTreeSet<usize>,
}
impl Names<'_> {
    fn copied(&self, at: usize) -> bool {
        let i = self
            .state
            .mappings
            .partition_point(|m| m.expanded.end <= at);
        self.state.mappings.get(i).is_some_and(|m| m.copied)
    }

    fn field(&mut self, field: &str, span: &Span) {
        if self.state.source.get(span.clone()) != Some(field) {
            return;
        }
        let i = self.tokens.partition_point(|t| t.span.end <= span.start);
        if i > 0 && self.tokens[i - 1].kind != K::Colon {
            self.shorthand.insert(span.start);
        }
    }

    fn pat(&mut self, p: &Pattern, scope: &Span) {
        match &p.kind {
            PatternKind::Bind(name) if self.state.source.get(p.span.clone()) == Some(name) => {
                self.bindings.push(Binding {
                    name: name.clone(),
                    binder: p.span.clone(),
                    scope: scope.clone(),
                });
            }
            PatternKind::Tuple(ps) => {
                for p in ps {
                    self.pat(p, scope);
                }
            }
            PatternKind::Constructor { fields, named, .. } => {
                for (key, p) in fields {
                    if *named {
                        self.field(key, &p.span);
                    }
                    self.pat(p, scope);
                }
            }
            _ => {}
        }
    }

    fn params(&mut self, params: &[Param], scope: &Span) {
        for p in params {
            let i = self.tokens.partition_point(|t| t.span.end <= p.span.start);
            if let Some(t) = self.tokens[i..]
                .iter()
                .take_while(|t| t.span.start < p.span.end)
                .find(|t| self.state.source[t.span.clone()] == p.name)
            {
                self.bindings.push(Binding {
                    name: p.name.clone(),
                    binder: t.span.clone(),
                    scope: scope.clone(),
                });
            }
        }
    }

    fn scan(&mut self, f: &mut Function) {
        let Some(body) = &mut f.body else {
            return;
        };
        self.params(&f.params, &body.span);
        parser::walk_exprs(body, &mut |e| match &e.kind {
            ExprKind::Name(n) if self.state.source.get(e.span.clone()) == Some(n) => {
                let copied = self.copied(e.span.start);
                self.refs
                    .entry(n.clone())
                    .or_default()
                    .push((e.span.clone(), copied));
            }
            ExprKind::Struct { fields, .. } => {
                for (key, v) in fields {
                    self.field(key, &v.span);
                }
            }
            ExprKind::Block { statements, .. } => {
                for stmt in statements {
                    if let Statement::Let { pat, value, .. } = stmt {
                        self.pat(pat, &(value.span.end..e.span.end));
                    }
                }
            }
            ExprKind::Match { arms, .. } => {
                for (p, arm) in arms {
                    self.pat(p, &arm.span);
                }
            }
            ExprKind::For { pat, body, .. } => self.pat(pat, &body.span),
            ExprKind::Closure { params, body, .. } => self.params(params, &body.span),
            ExprKind::Handle {
                state,
                body,
                ret,
                ops,
                ..
            } => {
                if let Some((p, _)) = state {
                    self.pat(p, &(body.span.start..e.span.end));
                }
                for clause in std::iter::once(&**ret).chain(ops) {
                    for p in &clause.pats {
                        self.pat(p, &clause.body.span);
                    }
                }
            }
            _ => {}
        });
    }

    fn edits(mut self, mark: &str) -> BTreeMap<usize, (Span, String)> {
        // For a shadowed name, rename all caller bindings of that spelling.
        // Keeping the same fresh spelling across nested scopes preserves their
        // existing shadowing, including arms sharing a desugared AST body.
        let mut generated: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (name, refs) in &self.refs {
            let mut v: Vec<_> = refs
                .iter()
                .filter_map(|(r, c)| (!c).then_some(r.start))
                .collect();
            v.sort_unstable();
            if !v.is_empty() && !name.starts_with(mark) {
                generated.insert(name, v);
            }
        }
        let conflicts: BTreeSet<_> = self
            .bindings
            .iter()
            .filter(|b| self.copied(b.binder.start))
            .filter(|b| {
                generated.get(b.name.as_str()).is_some_and(|v| {
                    let i = v.partition_point(|p| *p < b.scope.start);
                    v.get(i).is_some_and(|p| *p < b.scope.end)
                })
            })
            .map(|b| b.name.clone())
            .collect();
        let mut edits = BTreeMap::new();
        for (id, name) in conflicts.iter().enumerate() {
            let fresh = format!("{mark}caller_{id}");
            let mut scopes: Vec<Span> = Vec::new();
            let mut binders: Vec<_> = self
                .bindings
                .iter()
                .filter(|b| b.name == *name && self.copied(b.binder.start))
                .collect();
            binders.sort_by_key(|b| b.scope.start);
            let mut rename = |r: Span| {
                let text = if self.shorthand.contains(&r.start) {
                    format!("{name}: {fresh}")
                } else {
                    fresh.clone()
                };
                edits.insert(r.start, (r, text));
            };
            for b in binders {
                rename(b.binder.clone());
                if let Some(prev) = scopes.last_mut()
                    && b.scope.start <= prev.end
                {
                    prev.end = prev.end.max(b.scope.end);
                } else {
                    scopes.push(b.scope.clone());
                }
            }
            for (r, copied) in self.refs.remove(name).unwrap_or_default() {
                let i = scopes.partition_point(|s| s.end <= r.start);
                if copied && scopes.get(i).is_some_and(|s| s.start <= r.start) {
                    rename(r);
                }
            }
        }
        edits
    }
}

pub(super) fn protect(
    state: &mut ExpandedSugar,
    mark: &str,
    work: &mut usize,
    mut ast: crate::ast::Module,
) -> Result<(), Diagnostic> {
    if state.regions.is_empty() {
        return Ok(());
    }
    charge(work, state.source.len(), 0..0)?;
    let tokens = lexer::sugar_lex_with_limits(
        &state.source,
        lexer::LexLimits {
            max_source_bytes: MAX_BYTES,
            max_tokens: lexer::HARD_TOKENS,
            ..Default::default()
        },
    )
    .map_err(|e| e.diagnostic(0))?
    .tokens;
    let tokens: Vec<_> = tokens.into_iter().filter(|t| !t.kind.trivia()).collect();
    let mut names = Names {
        state,
        tokens: &tokens,
        bindings: Vec::new(),
        refs: BTreeMap::new(),
        shorthand: BTreeSet::new(),
    };
    for item in &mut ast.items {
        match item {
            Item::Function(f) => names.scan(f),
            Item::Impl { methods, .. } => methods.iter_mut().for_each(|f| names.scan(f)),
            Item::Conformance(c) => c.methods.iter_mut().for_each(|f| names.scan(f)),
            _ => {}
        }
    }
    for (_, (old, text)) in names.edits(mark).into_iter().rev() {
        let origin = state.original_span(old.clone());
        state.apply(
            Edit {
                old,
                text,
                origin,
                region: None,
                import: None,
                copies: Vec::new(),
            },
            work,
        )?;
    }
    Ok(())
}

/// A binder literally introduced by a pattern template cannot bind a caller
/// identifier in the following arm. A binder spliced from a pattern hole can.
pub(super) fn pattern(rendered: &mut super::template::Rendered, mark: &str) -> Result<(), String> {
    const PREFIX: &str = "fn __pattern() {match 0 { ";
    let src = format!("{PREFIX} {} => 0 }} }}", rendered.text);
    let parsed = parser::parse_expansion_scopes(&src, Default::default(), false)
        .map_err(|e| e.to_string())?;
    if let Some(d) = parsed.diagnostics.first() {
        return Err(format!("invalid expanded sugar pattern: {}", d.message));
    }
    let Some(Item::Function(f)) = parsed.module.items.first() else {
        return Err("missing pattern template wrapper".into());
    };
    let Some(body) = &f.body else {
        return Err("missing pattern template".into());
    };
    let ExprKind::Block {
        tail: Some(tail), ..
    } = &body.kind
    else {
        return Err("missing match pattern".into());
    };
    let ExprKind::Match { arms, .. } = &tail.kind else {
        return Err("expected a match pattern".into());
    };
    let mut stack: Vec<_> = arms.iter().map(|(pat, _)| (pat, None::<&str>)).collect();
    let mut names = BTreeMap::new();
    let mut replacements = BTreeMap::new();
    while let Some((pattern, field)) = stack.pop() {
        match &pattern.kind {
            PatternKind::Tuple(xs) => stack.extend(xs.iter().map(|p| (p, None))),
            PatternKind::Constructor { fields, named, .. } => {
                stack.extend(fields.iter().map(|(n, p)| (p, named.then_some(n.as_str()))))
            }
            PatternKind::Bind(n) => {
                let off = PREFIX.len() + 1;
                let r = pattern.span.start - off..pattern.span.end - off;
                if rendered
                    .holes
                    .iter()
                    .any(|h| h.start <= r.start && r.end <= h.end)
                {
                    continue;
                }
                let id = names.len();
                let fresh = names
                    .entry(n.clone())
                    .or_insert_with(|| format!("{mark}pattern_{id}"));
                let shorthand = field == Some(n.as_str())
                    && !rendered.text[..r.start].trim_end().ends_with(':');
                let new = if shorthand {
                    format!("{n}: {fresh}")
                } else {
                    fresh.clone()
                };
                replacements.insert(r.start, (r, new));
            }
            _ => {}
        }
    }
    for (_, (r, text)) in replacements.into_iter().rev() {
        let shift = |p: usize| {
            if p <= r.start {
                p
            } else {
                p - r.len() + text.len()
            }
        };
        let adjust = |s: &mut Span| {
            *s = shift(s.start)..shift(s.end);
        };
        for h in &mut rendered.holes {
            adjust(h);
        }
        for (s, _) in &mut rendered.places {
            adjust(s);
        }
        for (s, _, _) in &mut rendered.origins {
            adjust(s);
        }
        rendered.text.replace_range(r, &text);
    }
    Ok(())
}
