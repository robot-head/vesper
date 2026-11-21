//! Scope and control-flow analysis of the template, before caller fragments
//! are substituted. The private parser skeleton uses unspellable-at-this-call
//! markers for captured values, so caller identifiers never acquire a new mark.
use super::*;
use crate::{ast::*, parser};

const PREFIX: &str = "fn template_body() {";

enum Edit {
    Literal(String),
    LazyUse {
        id: usize,
        name: String,
    },
    Hole(usize),
    Binding {
        id: usize,
        name: String,
        block: bool,
        lazy: bool,
    },
}
struct Replacement {
    range: Span,
    edit: Edit,
}
#[derive(Clone, Copy, PartialEq)]
enum Edge {
    Always,
    Branch,
    Conditional,
    Repeated,
}
struct Node {
    parent: Option<usize>,
    edge: Edge,
    span: Span,
    statement_start: Option<usize>,
    earlier_effect: bool,
}

struct Flow<'a, 'b> {
    sk: &'a mut Skeleton<'b>,
    nodes: Vec<Node>,
    usages: Vec<Vec<(usize, usize)>>,
    writes: BTreeSet<usize>,
    literals: BTreeMap<(usize, usize), String>,
    declarations: Vec<(usize, Option<usize>, usize, String, bool)>,
    tokens: Vec<Token>,
    fresh: usize,
}

impl Flow<'_, '_> {
    fn rename(&mut self, r: Span, text: String) {
        if r.start < PREFIX.len() || r.end > PREFIX.len() + self.sk.source.len() {
            return;
        }
        self.literals
            .insert((r.start - PREFIX.len(), r.end - PREFIX.len()), text);
    }
    fn slice(&self, r: &Span) -> &str {
        r.start
            .checked_sub(PREFIX.len())
            .and_then(|s| {
                r.end
                    .checked_sub(PREFIX.len())
                    .and_then(|e| self.sk.source.get(s..e))
            })
            .unwrap_or("")
    }
    fn slot(&self, start: usize) -> Option<usize> {
        let off = start.checked_sub(PREFIX.len())?;
        self.sk
            .slots
            .binary_search_by_key(&off, |s| s.span.start)
            .ok()
    }
    fn fresh(&mut self, purpose: &str) -> String {
        self.fresh += 1;
        format!("{}{purpose}{}", self.sk.mark, self.fresh)
    }
    fn pattern(&mut self, p: &Pattern, env: &mut Vec<(String, String)>, shorthand: bool) {
        self.pattern_using(p, env, shorthand, &[]);
    }
    fn pattern_using(
        &mut self,
        p: &Pattern,
        env: &mut Vec<(String, String)>,
        shorthand: bool,
        alternatives: &[(String, String)],
    ) {
        match &p.kind {
            PatternKind::Bind(n) if !n.starts_with(self.sk.mark) && self.slice(&p.span) == n => {
                let fresh = alternatives
                    .iter()
                    .find(|(name, _)| name == n)
                    .map(|(_, fresh)| fresh.clone())
                    .unwrap_or_else(|| self.fresh("local"));
                self.rename(
                    p.span.clone(),
                    if shorthand {
                        format!("{n}: {fresh}")
                    } else {
                        fresh.clone()
                    },
                );
                env.push((n.clone(), fresh));
            }
            PatternKind::Tuple(ps) => {
                for p in ps {
                    self.pattern_using(p, env, false, alternatives);
                }
            }
            PatternKind::Constructor { fields, named, .. } => {
                for (n, p) in fields {
                    // In a named field the parser's shorthand binder has the field's
                    // exact spelling and no preceding colon.
                    self.pattern_using(p, env, *named && self.shorthand(n, &p.span), alternatives);
                }
            }
            _ => {}
        }
    }
    fn shorthand(&self, name: &str, r: &Span) -> bool {
        if self.slice(r) != name {
            return false;
        }
        let at = self.tokens.partition_point(|t| t.span.end <= r.start);
        self.tokens[..at]
            .iter()
            .rev()
            .find(|t| !t.kind.trivia())
            .is_none_or(|t| t.kind != K::Colon)
    }
    fn place(&mut self, mut e: &Expr, use_mode: SugarPass) -> Result<(), String> {
        while let ExprKind::Field(base, _) | ExprKind::Index(base, _) = &e.kind {
            e = base
        }
        let Some(k) = self.slot(e.span.start) else {
            return Ok(());
        };
        let h = &self.sk.holes[self.sk.slots[k].id];
        let Some(floor) = permission(h)? else {
            return Ok(());
        };
        if !floor.permits(use_mode) {
            return Err(format!(
                "sugar template upgrades a {floor:?} capture to {use_mode:?}"
            ));
        }
        if use_mode != SugarPass::In {
            self.writes.insert(k);
        }
        Ok(())
    }
    fn inert(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Literal(_) => true,
            ExprKind::Name(n) => !n.starts_with(self.sk.mark),
            _ => false,
        }
    }
    fn walk(
        &mut self,
        e: &Expr,
        parent: Option<usize>,
        edge: Edge,
        env: &mut Vec<(String, String)>,
    ) -> Result<(), String> {
        let node = self.nodes.len();
        self.nodes.push(Node {
            parent,
            edge,
            span: e.span.clone(),
            statement_start: None,
            earlier_effect: false,
        });
        let parent = Some(node);
        let outer = env.len();
        match &e.kind {
            ExprKind::Name(n) => {
                if let Some(k) = self.slot(e.span.start) {
                    if self.sk.holes[self.sk.slots[k].id].marker == *n {
                        self.usages[self.sk.slots[k].id].push((k, node));
                    }
                } else if let Some((_, new)) = env.iter().rev().find(|(old, _)| old == n)
                    && self.slice(&e.span) == n
                {
                    self.rename(e.span.clone(), new.clone());
                }
            }
            ExprKind::Literal(_) | ExprKind::Continue => {}
            ExprKind::Tuple(xs) => {
                let mut earlier = false;
                for x in xs {
                    let child = self.nodes.len();
                    self.walk(x, parent, Edge::Always, env)?;
                    self.nodes[child].earlier_effect = earlier;
                    earlier |= !self.inert(x);
                }
            }
            ExprKind::Struct { fields, .. } => {
                let mut earlier = false;
                for (n, x) in fields {
                    let child = self.nodes.len();
                    self.walk(x, parent, Edge::Always, env)?;
                    self.nodes[child].earlier_effect = earlier;
                    earlier |= !self.inert(x);
                    if self.shorthand(n, &x.span)
                        && let Some(r) = x.span.start.checked_sub(PREFIX.len())
                        && let Some(replacement) = self.literals.get_mut(&(r, r + n.len()))
                    {
                        *replacement = format!("{n}: {replacement}");
                    }
                }
            }
            ExprKind::Block { statements, tail } => {
                let block_start = e.span.start + 1;
                let mut after = block_start;
                for stmt in statements {
                    let first = self.nodes.len();
                    match stmt {
                        Statement::Let { value, pat, .. } => {
                            self.walk(value, parent, Edge::Always, env)?;
                            self.pattern(pat, env, false);
                        }
                        Statement::Expr(x) => self.walk(x, parent, Edge::Always, env)?,
                    }
                    self.nodes[first].earlier_effect = after != block_start;
                    after = self.statement(first, after);
                }
                if let Some(x) = tail {
                    let first = self.nodes.len();
                    self.walk(x, parent, Edge::Always, env)?;
                    self.nodes[first].earlier_effect = after != block_start;
                    self.statement(first, after);
                }
            }
            ExprKind::Call { callee, args, .. } => {
                self.walk(callee, parent, Edge::Always, env)?;
                let mut earlier = !self.inert(callee);
                for a in args {
                    if a.explicit {
                        self.place(
                            &a.expr,
                            match a.pass {
                                Pass::In => SugarPass::In,
                                Pass::Inout => SugarPass::Inout,
                                Pass::Sink => SugarPass::Sink,
                            },
                        )?;
                    }
                    let child = self.nodes.len();
                    self.walk(&a.expr, parent, Edge::Always, env)?;
                    self.nodes[child].earlier_effect = earlier;
                    earlier |= !self.inert(&a.expr);
                }
            }
            ExprKind::Field(b, _) | ExprKind::Try(b) | ExprKind::Unary(_, b) => {
                self.walk(b, parent, Edge::Always, env)?
            }
            ExprKind::Index(a, b)
            | ExprKind::Range {
                start: a, end: b, ..
            } => {
                self.walk(a, parent, Edge::Always, env)?;
                let child = self.nodes.len();
                self.walk(b, parent, Edge::Always, env)?;
                self.nodes[child].earlier_effect = !self.inert(a);
            }
            ExprKind::Binary(op, a, b) => {
                self.walk(a, parent, Edge::Always, env)?;
                let child = self.nodes.len();
                self.walk(
                    b,
                    parent,
                    if matches!(op.as_str(), "&&" | "||") {
                        Edge::Conditional
                    } else {
                        Edge::Always
                    },
                    env,
                )?;
                self.nodes[child].earlier_effect = !self.inert(a);
            }
            ExprKind::Assign(op, a, b) => {
                let mut base = &**a;
                while let ExprKind::Field(v, _) | ExprKind::Index(v, _) = &base.kind {
                    base = v
                }
                let init_out = op == "="
                    && self.slot(base.span.start).is_some_and(|k| {
                        self.sk.holes[self.sk.slots[k].id].cap.pass.as_deref() == Some("out")
                    });
                self.place(
                    a,
                    if init_out {
                        SugarPass::Out
                    } else {
                        SugarPass::Inout
                    },
                )?;
                self.walk(a, parent, Edge::Always, env)?;
                self.walk(b, parent, Edge::Always, env)?;
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                self.walk(condition, parent, Edge::Always, env)?;
                self.walk(then, parent, Edge::Branch, env)?;
                if let Some(x) = otherwise {
                    self.walk(x, parent, Edge::Branch, env)?
                }
            }
            ExprKind::Match { value, arms } => {
                self.walk(value, parent, Edge::Always, env)?;
                // The parser lowers an or-pattern to multiple arms sharing the
                // same source body. All alternatives introduce the same bindings.
                for alternatives in arms.chunk_by(|a, b| a.1.span == b.1.span) {
                    let (p, x) = &alternatives[0];
                    self.pattern(p, env, false);
                    self.walk(x, parent, Edge::Branch, env)?;
                    let bindings = env.split_off(outer);
                    for (other, _) in &alternatives[1..] {
                        self.pattern_using(other, env, false, &bindings);
                        env.truncate(outer);
                    }
                }
            }
            ExprKind::Closure { params, body, .. } => {
                for p in params {
                    if p.name.starts_with(self.sk.mark) {
                        continue;
                    }
                    let fragment = self.slice(&p.span);
                    let (ts, _) = lexer::lex(fragment);
                    if let Some(t) = ts.iter().find(|t| fragment[t.span.clone()] == p.name) {
                        let s = p.span.start + t.span.start;
                        let new = self.fresh("param");
                        self.rename(s..s + p.name.len(), new.clone());
                        env.push((p.name.clone(), new));
                    }
                }
                self.walk(body, parent, Edge::Repeated, env)?;
            }
            ExprKind::Handle {
                state,
                body,
                ret,
                ops,
                ..
            } => {
                if let Some((p, v)) = state {
                    self.walk(v, parent, Edge::Always, env)?;
                    self.pattern(p, env, false);
                }
                self.walk(body, parent, Edge::Always, env)?;
                let base = env.len();
                for (clause, edge) in std::iter::once((&**ret, Edge::Conditional))
                    .chain(ops.iter().map(|op| (op, Edge::Repeated)))
                {
                    for p in &clause.pats {
                        self.pattern(p, env, false);
                    }
                    self.walk(&clause.body, parent, edge, env)?;
                    env.truncate(base);
                }
            }
            ExprKind::Loop(body) => self.walk(body, parent, Edge::Repeated, env)?,
            ExprKind::For { pat, iter, body } => {
                self.walk(iter, parent, Edge::Always, env)?;
                self.pattern(pat, env, false);
                self.walk(body, parent, Edge::Repeated, env)?;
            }
            ExprKind::Return(v) | ExprKind::Break(v) => {
                if let Some(v) = v {
                    self.walk(v, parent, Edge::Always, env)?
                }
            }
        }
        env.truncate(outer);
        Ok(())
    }
    // The AST omits let keywords, grouping parentheses and semicolons.
    // Locate the actual beginning of the statement following the last one.
    fn statement(&mut self, child: usize, after: usize) -> usize {
        let at = self.tokens.partition_point(|t| t.span.end <= after);
        if self.nodes[child]
            .parent
            .is_some_and(|p| self.nodes[p].span.start < PREFIX.len())
        {
            return self.nodes[child].span.end;
        }
        self.nodes[child].statement_start = self.tokens[at..]
            .iter()
            .find(|t| !t.kind.trivia() && !matches!(t.kind, K::RParen | K::Semi))
            .map(|t| t.span.start);
        self.nodes[child].span.end
    }
    fn path(&self, mut node: usize) -> Vec<usize> {
        let mut path = vec![node];
        while let Some(p) = self.nodes[node].parent {
            node = p;
            path.push(p)
        }
        path.reverse();
        path
    }
    fn plan(
        &mut self,
        id: usize,
        uses: &[(usize, Vec<usize>)],
        depth: usize,
    ) -> Result<(), String> {
        if uses.is_empty() {
            return Ok(());
        }
        let repeatable = uses
            .iter()
            .any(|(_, path)| path.iter().any(|&n| self.nodes[n].edge == Edge::Repeated));
        if uses.len() < 2 && !repeatable {
            return Ok(());
        }
        let mut common = depth;
        while let Some(n) = uses[0].1.get(common + 1) {
            if !uses.iter().all(|(_, path)| path.get(common + 1) == Some(n)) {
                break;
            }
            common += 1;
        }
        if !repeatable
            && uses.iter().all(|(_, path)| {
                path.get(common + 1)
                    .is_some_and(|&n| self.nodes[n].edge == Edge::Branch)
            })
        {
            let mut start = 0;
            while start < uses.len() {
                let branch = uses[start].1[common + 1];
                let count = uses[start..]
                    .iter()
                    .take_while(|(_, p)| p[common + 1] == branch)
                    .count();
                self.plan(id, &uses[start..start + count], common + 1)?;
                start += count;
            }
            return Ok(());
        }
        if self.sk.holes[id].cap.sort == "place" {
            return Err(
                "a modifying sugar place is used more than once on a control-flow path".into(),
            );
        }
        let path = &uses[0].1;
        if repeatable
            && let Some(repeated) = path[..=common]
                .iter()
                .position(|&n| self.nodes[n].edge == Edge::Repeated)
        {
            common = repeated.saturating_sub(1);
        }
        let lazy = repeatable
            || path[common + 1..].iter().enumerate().any(|(i, &n)| {
                let child = &self.nodes[n];
                child.edge != Edge::Always
                    || child.earlier_effect && (i != 0 || child.statement_start.is_none())
            });
        let span = &self.nodes[path[common]].span;
        let statement = path
            .get(common + 1)
            .and_then(|&n| self.nodes[n].statement_start)
            .or(self.nodes[path[common]].statement_start);
        let start = statement.unwrap_or(span.start).saturating_sub(PREFIX.len());
        let end = statement.is_none().then(|| {
            span.end
                .saturating_sub(PREFIX.len())
                .min(self.sk.source.len())
        });
        let name = self.fresh("once");
        self.declarations.push((start, end, id, name.clone(), lazy));
        for (slot, _) in uses {
            self.sk.slots[*slot].memo = Some((name.clone(), lazy))
        }
        Ok(())
    }
}

pub(super) fn render(mut sk: Skeleton<'_>) -> Result<Rendered, String> {
    let wrapper = format!("{PREFIX}{}\n}}", sk.source);
    let parsed = parser::rust_parse(&wrapper);
    let mut flow = Flow {
        tokens: lexer::lex(&wrapper).0,
        nodes: vec![],
        usages: vec![vec![]; sk.holes.len()],
        writes: BTreeSet::new(),
        literals: BTreeMap::new(),
        declarations: vec![],
        fresh: 0,
        sk: &mut sk,
    };
    if parsed.diagnostics.is_empty()
        && let Some(Item::Function(f)) = parsed.module.items.first()
        && let Some(body) = &f.body
    {
        flow.walk(body, None, Edge::Always, &mut vec![])?;
        for id in 0..flow.usages.len() {
            let uses = &flow.usages[id];
            if flow.sk.holes[id].cap.sort != "expr"
                && !uses.iter().any(|(k, _)| flow.writes.contains(k))
            {
                continue;
            }
            let paths: Vec<_> = uses.iter().map(|(k, n)| (*k, flow.path(*n))).collect();
            flow.plan(id, &paths, 0)?;
        }
    } else {
        // Item-producing and nested-sugar templates need no core expression AST
        // when they contain no local binders or repeated/privileged expressions.
        let (tokens, _) = lexer::lex(&sk.source);
        if tokens
            .iter()
            .any(|t| matches!(t.kind, K::Let | K::FatArrow | K::Inout | K::Sink))
            || sk
                .slots
                .iter()
                .filter(|s| sk.holes[s.id].cap.sort == "expr")
                .map(|s| s.id)
                .collect::<BTreeSet<_>>()
                .len()
                != sk
                    .slots
                    .iter()
                    .filter(|s| sk.holes[s.id].cap.sort == "expr")
                    .count()
        {
            return Err(
                "cannot prove hygiene and single evaluation for this non-core sugar template"
                    .into(),
            );
        }
        return emit(&sk, vec![]);
    }
    let mut replacements = Vec::new();
    for ((start, end), s) in flow.literals {
        replacements.push(Replacement {
            range: start..end,
            edit: Edit::Literal(s),
        })
    }
    // The first insertion at a common start opens the widest wrapper first.
    flow.declarations
        .sort_by_key(|(start, end, _, _, _)| (*start, std::cmp::Reverse(*end)));
    for (start, end, id, name, lazy) in flow.declarations {
        replacements.push(Replacement {
            range: start..start,
            edit: Edit::Binding {
                id,
                name,
                block: end.is_some(),
                lazy,
            },
        });
        if let Some(end) = end {
            replacements.push(Replacement {
                range: end..end,
                edit: Edit::Literal(" }".into()),
            });
        }
    }
    emit(&sk, replacements)
}

fn emit(sk: &Skeleton<'_>, mut edits: Vec<Replacement>) -> Result<Rendered, String> {
    let tokens = lexer::lex(&sk.source).0;
    let mut commas = BTreeSet::new();
    for slot in &sk.slots {
        let h = &sk.holes[slot.id];
        if matches!(h.cap.sort.as_str(), "args" | "pats") && h.value.text.trim().is_empty() {
            let next = tokens.partition_point(|t| t.span.end <= slot.span.end);
            if let Some(t) = tokens[next..].iter().find(|t| !t.kind.trivia())
                && t.kind == K::Comma
                && commas.insert(t.span.start)
            {
                edits.push(Replacement {
                    range: t.span.clone(),
                    edit: Edit::Literal(String::new()),
                });
            }
        }
        let edit = match &slot.memo {
            Some((name, true)) => Edit::LazyUse {
                id: slot.id,
                name: name.clone(),
            },
            Some((name, false)) => Edit::Literal(name.clone()),
            None => Edit::Hole(slot.id),
        };
        edits.push(Replacement {
            range: slot.span.clone(),
            edit,
        });
    }
    edits.sort_by_key(|e| (e.range.start, e.range.end));
    let mut result = Rendered::default();
    let mut pos = 0;
    for e in edits {
        if e.range.start < pos {
            return Err("overlapping sugar template edits".into());
        }
        bounded(&mut result.text, &sk.source[pos..e.range.start], sk.max)?;
        match e.edit {
            Edit::Literal(t) => bounded(&mut result.text, &t, sk.max)?,
            Edit::Hole(id) => result.hole(&sk.holes[id], sk.max)?,
            Edit::LazyUse { id, name } => {
                bounded(
                    &mut result.text,
                    &format!(
                        "{{ match {name} {{ Option::Some({name}_v) => {name}_v, Option::None => {{ let {name}_v = "
                    ),
                    sk.max,
                )?;
                result.hole(&sk.holes[id], sk.max)?;
                bounded(
                    &mut result.text,
                    &format!("; {name} = Option::Some({name}_v); {name}_v }} }} }}"),
                    sk.max,
                )?;
            }
            Edit::Binding {
                id,
                name,
                block,
                lazy,
            } => {
                if block {
                    bounded(&mut result.text, "{ ", sk.max)?;
                }
                // None needs the payload type. The unreachable arm supplies it
                // without executing the expression or requiring an annotation.
                if lazy {
                    bounded(
                        &mut result.text,
                        &format!("let mut {name} = if false {{ Option::Some("),
                        sk.max,
                    )?;
                    result.hole(&sk.holes[id], sk.max)?;
                    bounded(&mut result.text, ") } else { Option::None }; ", sk.max)?;
                } else {
                    bounded(&mut result.text, &format!("let {name} = "), sk.max)?;
                    result.hole(&sk.holes[id], sk.max)?;
                    bounded(&mut result.text, "; ", sk.max)?;
                }
            }
        }
        pos = e.range.end;
    }
    bounded(&mut result.text, &sk.source[pos..], sk.max)?;
    Ok(result)
}
