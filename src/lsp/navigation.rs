//! Pure navigation over a whole, validated checked source and its exact text index.
//! Type modes describe types, never current ownership, obligations or proof status.
mod doc_links;

use crate::{
    ast::*,
    check::World,
    lexer::{self, Token, TokenKind as K},
    tooling::frontend::CheckedSource,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::{self, Write},
};
use vesper_span::{ByteSpan, LineIndex, Position, Range};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_source_bytes: usize,
    pub max_nodes: usize,
    pub max_type_work: usize,
    pub max_depth: usize,
    pub max_symbols: usize,
    pub max_documents: usize,
    pub max_uri_bytes: usize,
    pub max_query_bytes: usize,
    pub max_output_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_source_bytes: 128 * 1024,
            max_nodes: 16_384,
            max_type_work: 8192,
            max_depth: 64,
            max_symbols: 1024,
            max_documents: 32,
            max_uri_bytes: 4096,
            max_query_bytes: 256,
            max_output_bytes: 256 * 1024,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    InvalidLimits,
    SourceMismatch,
    InvalidPosition(String),
    InvalidSpan,
    Resource(&'static str),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "navigation: {self:?}")
    }
}
impl std::error::Error for Error {}
impl Limits {
    fn validate(self) -> Result<(), Error> {
        if self.max_source_bytes == 0
            || self.max_source_bytes > 1024 * 1024
            || self.max_nodes == 0
            || self.max_nodes > 65_536
            || self.max_type_work == 0
            || self.max_type_work > 65_536
            || self.max_depth == 0
            || self.max_depth > 128
            || self.max_symbols == 0
            || self.max_symbols > 4096
            || self.max_documents == 0
            || self.max_documents > 128
            || self.max_uri_bytes == 0
            || self.max_uri_bytes > 16_384
            || self.max_query_bytes == 0
            || self.max_query_bytes > 4096
            || self.max_output_bytes == 0
            || self.max_output_bytes > 2 * 1024 * 1024
        {
            Err(Error::InvalidLimits)
        } else {
            Ok(())
        }
    }
}
struct Budget {
    left: usize,
    depth: usize,
}
impl Budget {
    fn charge(&mut self, depth: usize) -> Result<(), Error> {
        if self.left == 0 || depth > self.depth {
            return Err(Error::Resource("traversal work/depth"));
        }
        self.left -= 1;
        Ok(())
    }
}
struct Text {
    value: String,
    cap: usize,
}
impl Write for Text {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if s.len() > self.cap.saturating_sub(self.value.len()) {
            return Err(fmt::Error);
        }
        self.value.push_str(s);
        Ok(())
    }
}
fn position(p: Position) -> Value {
    json!({"line":p.line,"character":p.character})
}
fn range(r: Range) -> Value {
    json!({"start":position(r.start),"end":position(r.end)})
}
fn source_range(index: &LineIndex, span: &Span) -> Result<Value, Error> {
    index
        .range(ByteSpan {
            start: span.start,
            end: span.end,
        })
        .map(range)
        .map_err(|_| Error::InvalidSpan)
}
fn output(value: &Value, limit: usize) -> Result<(), Error> {
    struct Count {
        n: usize,
        cap: usize,
    }
    impl std::io::Write for Count {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.n = self
                .n
                .checked_add(b.len())
                .filter(|n| *n <= self.cap)
                .ok_or_else(|| std::io::Error::other("output limit"))?;
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Count { n: 0, cap: limit }, value)
        .map_err(|_| Error::Resource("output bytes"))
}
fn validate<'a>(
    snapshot: &'a CheckedSource,
    index: &LineIndex,
    limits: Limits,
) -> Result<&'a str, Error> {
    limits.validate()?;
    if snapshot.source() != index.text() {
        return Err(Error::SourceMismatch);
    }
    if snapshot.source().len() > limits.max_source_bytes {
        return Err(Error::Resource("source bytes"));
    }
    Ok(snapshot.source())
}
fn uri(uri: &str, limits: Limits) -> Result<(), Error> {
    if uri.is_empty() || uri.len() > limits.max_uri_bytes {
        Err(Error::Resource("URI bytes"))
    } else {
        Ok(())
    }
}
#[derive(Clone)]
struct Binding<'a> {
    name: &'a str,
    target: Span,
    param: Option<&'a Param>,
}
struct Node<'a> {
    expr: &'a Expr,
    function: &'a Function,
    binding: Option<Binding<'a>>,
    target: Option<Span>,
}
struct Declaration<'a> {
    name: &'a str,
    span: Span,
    selection: Span,
    kind: u32,
    function: Option<&'a Function>,
}
struct Index<'a> {
    source: &'a str,
    tokens: Vec<Token>,
    declarations: Vec<Declaration<'a>>,
    globals: BTreeMap<&'a str, Span>,
    nodes: Vec<Node<'a>>,
    budget: Budget,
    limits: Limits,
}
impl<'a> Index<'a> {
    fn new(snapshot: &'a CheckedSource, index: &LineIndex, limits: Limits) -> Result<Self, Error> {
        let source = validate(snapshot, index, limits)?;
        let lexed = lexer::lex_with_limits(
            source,
            lexer::LexLimits {
                max_source_bytes: limits.max_source_bytes,
                max_tokens: limits.max_nodes,
                max_diagnostics: 1,
            },
        )
        .map_err(|_| Error::Resource("token work"))?;
        if lexed.limit.is_some() || !lexed.diagnostics.is_empty() {
            return Err(Error::Resource("token work"));
        }
        let mut this = Self {
            source,
            tokens: lexed.tokens,
            declarations: Vec::new(),
            globals: BTreeMap::new(),
            nodes: Vec::new(),
            budget: Budget {
                left: limits.max_nodes,
                depth: limits.max_depth,
            },
            limits,
        };
        for item in &snapshot.parsed().module.items {
            this.item(item)?;
        }
        let functions: Vec<_> = this
            .declarations
            .iter()
            .filter_map(|d| d.function)
            .collect();
        for f in functions {
            if let Some(body) = &f.body {
                let mut env = Vec::new();
                for p in &f.params {
                    env.push(this.parameter(p)?);
                }
                this.expr(body, f, &mut env, 0, true)?;
            }
        }
        Ok(this)
    }
    fn name(&self, span: &Span, name: &str) -> Result<Span, Error> {
        let begin = self.tokens.partition_point(|t| t.span.start < span.start);
        let end = self.tokens.partition_point(|t| t.span.end <= span.end);
        self.tokens[begin..end]
            .iter()
            .filter(|t| matches!(t.kind, K::Lower | K::Upper | K::SelfKw))
            .find(|t| self.source.get(t.span.clone()) == Some(name))
            .map(|t| t.span.clone())
            .ok_or(Error::InvalidSpan)
    }
    fn declaration(
        &mut self,
        name: &'a str,
        span: Span,
        kind: u32,
        function: Option<&'a Function>,
    ) -> Result<(), Error> {
        self.budget.charge(0)?;
        if self.declarations.len() >= self.limits.max_symbols {
            return Err(Error::Resource("symbols"));
        }
        let selection = self.name(&span, name)?;
        if kind == 12 {
            self.globals.insert(name, selection.clone());
        }
        self.declarations.push(Declaration {
            name,
            span,
            selection,
            kind,
            function,
        });
        Ok(())
    }
    fn item(&mut self, item: &'a Item) -> Result<(), Error> {
        match item {
            Item::Function(f) => self.declaration(&f.name, f.span.clone(), 12, Some(f))?,
            Item::Adt(a) => self.declaration(
                &a.name,
                a.span.clone(),
                if a.is_enum { 10 } else { 23 },
                None,
            )?,
            Item::Effect(e) => {
                self.declaration(&e.name, e.span.clone(), 11, None)?;
                for f in &e.ops {
                    self.declaration(&f.name, f.span.clone(), 6, None)?;
                }
            }
            Item::Trait(t) => self.declaration(&t.name, t.span.clone(), 11, None)?,
            Item::Conformance(c) => {
                if let ConformanceKind::Model { name, .. } = &c.kind {
                    self.declaration(name, c.span.clone(), 2, None)?;
                }
            }
            // Methods are not bare globals. Without a source dispatch resolver,
            // their calls deliberately have no definition target.
            Item::Impl { methods, .. } => {
                for f in methods {
                    self.declaration(&f.name, f.span.clone(), 6, None)?;
                }
            }
        }
        Ok(())
    }
    fn parameter(&self, p: &'a Param) -> Result<Binding<'a>, Error> {
        Ok(Binding {
            name: &p.name,
            target: self.name(&p.span, &p.name)?,
            param: Some(p),
        })
    }
    fn pattern(
        &mut self,
        p: &'a Pattern,
        env: &mut Vec<Binding<'a>>,
        depth: usize,
    ) -> Result<(), Error> {
        self.budget.charge(depth)?;
        match &p.kind {
            PatternKind::Bind(name) => env.push(Binding {
                name,
                target: self.name(&p.span, name)?,
                param: None,
            }),
            PatternKind::Tuple(ps) => {
                for p in ps {
                    self.pattern(p, env, depth + 1)?;
                }
            }
            PatternKind::Constructor { fields, .. } => {
                for (_, p) in fields {
                    self.pattern(p, env, depth + 1)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn expr(
        &mut self,
        e: &'a Expr,
        f: &'a Function,
        env: &mut Vec<Binding<'a>>,
        depth: usize,
        resolve: bool,
    ) -> Result<(), Error> {
        self.budget.charge(depth)?;
        let binding = if resolve {
            if let ExprKind::Name(name) = &e.kind {
                env.iter().rev().find(|b| b.name == name).cloned()
            } else {
                None
            }
        } else {
            None
        };
        let target = if !resolve {
            None
        } else if let Some(b) = &binding {
            Some(b.target.clone())
        } else if let ExprKind::Name(name) = &e.kind {
            self.globals.get(name.as_str()).cloned()
        } else {
            None
        };
        self.nodes.push(Node {
            expr: e,
            function: f,
            binding,
            target,
        });
        match &e.kind {
            ExprKind::Block { statements, tail } => {
                let old = env.len();
                for s in statements {
                    match s {
                        Statement::Let { pat, value, .. } => {
                            self.expr(value, f, env, depth + 1, resolve)?;
                            self.pattern(pat, env, depth + 1)?;
                        }
                        Statement::Expr(e) => self.expr(e, f, env, depth + 1, resolve)?,
                    }
                }
                if let Some(t) = tail {
                    self.expr(t, f, env, depth + 1, resolve)?;
                }
                env.truncate(old);
            }
            ExprKind::Tuple(es) => {
                for e in es {
                    self.expr(e, f, env, depth + 1, resolve)?;
                }
            }
            ExprKind::Struct { fields, .. } => {
                for (_, e) in fields {
                    self.expr(e, f, env, depth + 1, resolve)?;
                }
            }
            ExprKind::Call { callee, args, .. } => {
                // A bare method receiver can denote a lexical effect capability
                // instead of a value. The checker gives actual value bindings
                // precedence, but a same-named global function does not establish
                // that receiver's identity. Without resolver evidence, suppress
                // the global guess only for this unbound callee receiver; keep
                // real local/parameter bindings and argument references intact.
                let callee_resolve = resolve
                    && !matches!(
                        &callee.kind,
                        ExprKind::Field(base, _) if matches!(
                            &base.kind, ExprKind::Name(name)
                                if !env.iter().any(|binding|binding.name==name)
                        )
                    );
                self.expr(callee, f, env, depth + 1, callee_resolve)?;
                for a in args {
                    self.expr(&a.expr, f, env, depth + 1, resolve)?;
                }
            }
            ExprKind::Field(e, _) | ExprKind::Try(e) | ExprKind::Unary(_, e) => {
                self.expr(e, f, env, depth + 1, resolve)?
            }
            ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Assign(_, a, b) => {
                self.expr(a, f, env, depth + 1, resolve)?;
                self.expr(b, f, env, depth + 1, resolve)?;
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                self.expr(condition, f, env, depth + 1, resolve)?;
                self.expr(then, f, env, depth + 1, resolve)?;
                if let Some(e) = otherwise {
                    self.expr(e, f, env, depth + 1, resolve)?;
                }
            }
            ExprKind::Match { value, arms } => {
                self.expr(value, f, env, depth + 1, resolve)?;
                for (p, e) in arms {
                    let old = env.len();
                    self.pattern(p, env, depth + 1)?;
                    self.expr(e, f, env, depth + 1, resolve)?;
                    env.truncate(old);
                }
            }
            ExprKind::Closure { params, body, .. } => {
                let old = env.len();
                for p in params {
                    env.push(self.parameter(p)?);
                }
                self.expr(body, f, env, depth + 1, resolve)?;
                env.truncate(old);
            }
            // Handler state/continuation scopes need frontend resolver evidence.
            // Preserve actual type hover, but suppress every guessed name target.
            ExprKind::Handle {
                state,
                body,
                ret,
                ops,
                ..
            } => {
                if let Some((_, e)) = state {
                    self.expr(e, f, env, depth + 1, false)?;
                }
                self.expr(body, f, env, depth + 1, false)?;
                self.expr(&ret.body, f, env, depth + 1, false)?;
                for c in ops {
                    self.expr(&c.body, f, env, depth + 1, false)?;
                }
            }
            ExprKind::Literal(_) | ExprKind::Name(_) => {}
        }
        Ok(())
    }
}
fn contains(span: &Span, offset: usize) -> bool {
    span.start <= offset && offset < span.end
}
fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::Copy => "copy",
        Mode::Affine => "affine",
        Mode::Linear => "linear",
    }
}
fn pass_name(p: Pass) -> &'static str {
    match p {
        Pass::In => "in",
        Pass::Inout => "inout",
        Pass::Sink => "sink",
    }
}
fn text_write(out: &mut Text, s: &str) -> Result<(), Error> {
    out.write_str(s)
        .map_err(|_| Error::Resource("output bytes"))
}
fn print_type(out: &mut Text, t: &Type, b: &mut Budget, depth: usize) -> Result<(), Error> {
    b.charge(depth)?;
    match t {
        Type::Infer => text_write(out, "_")?,
        Type::IntLiteral(n) => {
            write!(out, "integer literal {n}").map_err(|_| Error::Resource("output bytes"))?
        }
        Type::Named(n, args) => {
            text_write(out, n)?;
            if !args.is_empty() {
                text_write(out, "[")?;
                for (i, t) in args.iter().enumerate() {
                    if i > 0 {
                        text_write(out, ", ")?;
                    }
                    print_type(out, t, b, depth + 1)?;
                }
                text_write(out, "]")?;
            }
        }
        Type::Tuple(ts) => {
            text_write(out, "(")?;
            for (i, t) in ts.iter().enumerate() {
                if i > 0 {
                    text_write(out, ", ")?;
                }
                print_type(out, t, b, depth + 1)?;
            }
            text_write(out, ")")?;
        }
        Type::Row(e) => print_effects(out, e, b, depth + 1)?,
        Type::Function {
            params,
            result,
            effects,
            mode,
        } => {
            text_write(out, mode_name(*mode))?;
            text_write(out, " fn(")?;
            for (i, (p, t)) in params.iter().enumerate() {
                if i > 0 {
                    text_write(out, ", ")?;
                }
                text_write(out, pass_name(*p))?;
                text_write(out, " ")?;
                print_type(out, t, b, depth + 1)?;
            }
            text_write(out, ") -> ")?;
            print_type(out, result, b, depth + 1)?;
            text_write(out, " ")?;
            print_effects(out, effects, b, depth + 1)?;
        }
    }
    Ok(())
}
fn print_effects(out: &mut Text, e: &Effects, b: &mut Budget, depth: usize) -> Result<(), Error> {
    b.charge(depth)?;
    text_write(out, "eff [")?;
    for (i, e) in e.instances.iter().enumerate() {
        b.charge(depth)?;
        if i > 0 {
            text_write(out, ", ")?;
        }
        if let Some(label) = &e.label {
            text_write(out, label)?;
            text_write(out, ": ")?;
        }
        text_write(out, &e.name)?;
        if !e.args.is_empty() {
            text_write(out, "[")?;
            for (i, t) in e.args.iter().enumerate() {
                if i > 0 {
                    text_write(out, ", ")?;
                }
                print_type(out, t, b, depth + 1)?;
            }
            text_write(out, "]")?;
        }
    }
    if let Some(t) = &e.tail {
        if !e.instances.is_empty() {
            text_write(out, " | ")?;
        }
        text_write(out, t)?;
    }
    text_write(out, "]")
}
// Mirror the *cost* of World::mode before invoking that public checked operation.
// Keys include argument modes, exactly as World::mode's recursion guard does;
// merely detecting a nominal name cycle would undercount some generic cases.
fn mode_work(
    w: &World,
    t: &Type,
    bounds: &BTreeMap<String, Mode>,
    seen: &mut BTreeSet<(String, Vec<Mode>)>,
    b: &mut Budget,
    depth: usize,
) -> Result<Mode, Error> {
    b.charge(depth)?;
    Ok(match t {
        Type::Infer | Type::IntLiteral(_) | Type::Row(_) => Mode::Copy,
        Type::Function { mode, .. } => *mode,
        Type::Tuple(ts) => {
            let mut m = Mode::Copy;
            for t in ts {
                m = m.join(mode_work(w, t, bounds, seen, b, depth + 1)?);
            }
            m
        }
        Type::Named(n, args) => {
            if let Some(m) = bounds.get(n) {
                *m
            } else if matches!(
                n.as_str(),
                "u8" | "u32" | "u64" | "i64" | "f64" | "bool" | "Unit" | "Never"
            ) {
                Mode::Copy
            } else if n == "String" {
                Mode::Affine
            } else if let Some(a) = w.adts.get(n) {
                let mut modes = Vec::new();
                for t in args {
                    modes.push(mode_work(w, t, bounds, seen, b, depth + 1)?);
                }
                let key = (n.clone(), modes.clone());
                if !seen.insert(key.clone()) {
                    a.mode
                } else {
                    let inner = a
                        .generics
                        .iter()
                        .zip(modes)
                        .map(|(g, m)| (g.name.clone(), m))
                        .collect();
                    let mut m = a.mode;
                    for field in a.variants.iter().flat_map(|v| &v.fields) {
                        m = m.join(
                            mode_work(w, &field.ty, &inner, seen, b, depth + 1)?
                                .meet(field.modality),
                        );
                    }
                    seen.remove(&key);
                    m
                }
            } else {
                Mode::Linear
            }
        }
    })
}
fn checked_mode(w: &World, t: &Type, f: &Function, b: &mut Budget) -> Result<Mode, Error> {
    // The generic bounds come from this actual lexical function, not a guessed
    // universal Copy default. The same bounds are passed to the checked World.
    for _ in &f.generics {
        b.charge(0)?;
    }
    let bounds = f
        .generics
        .iter()
        .map(|g| (g.name.clone(), g.bound))
        .collect();
    mode_work(w, t, &bounds, &mut BTreeSet::new(), b, 0)?;
    Ok(w.mode(t, &bounds))
}
fn signature(out: &mut Text, f: &Function, w: &World, b: &mut Budget) -> Result<(), Error> {
    text_write(out, "fn ")?;
    text_write(out, &f.name)?;
    if !f.generics.is_empty() {
        text_write(out, "[")?;
        for (i, g) in f.generics.iter().enumerate() {
            b.charge(0)?;
            if i > 0 {
                text_write(out, ", ")?;
            }
            text_write(out, &g.name)?;
            text_write(out, if g.row { ": eff" } else { ": mod " })?;
            if !g.row {
                text_write(out, mode_name(g.bound))?;
            }
        }
        text_write(out, "]")?;
    }
    text_write(out, "(")?;
    for (i, p) in f.params.iter().enumerate() {
        b.charge(0)?;
        if i > 0 {
            text_write(out, ", ")?;
        }
        text_write(out, pass_name(p.pass))?;
        text_write(out, " ")?;
        text_write(out, &p.name)?;
        selectors(out, p, b)?;
        text_write(out, ": ")?;
        print_type(out, &p.ty, b, 0)?;
    }
    text_write(out, ") -> ")?;
    let result = w
        .functions
        .get(&f.name)
        .filter(|actual| actual.span == f.span)
        .and_then(|_| w.function_results.get(&f.name))
        .unwrap_or(&f.result);
    print_type(out, result, b, 0)?;
    text_write(out, " ")?;
    print_effects(out, &f.effects, b, 0)
}
fn selectors(out: &mut Text, p: &Param, b: &mut Budget) -> Result<(), Error> {
    if p.projections.is_empty() {
        return Ok(());
    }
    text_write(out, ".{")?;
    for (i, path) in p.projections.iter().enumerate() {
        if i > 0 {
            text_write(out, ", ")?;
        }
        for (j, n) in path.iter().enumerate() {
            b.charge(0)?;
            if j > 0 {
                text_write(out, ".")?;
            }
            text_write(out, n)?;
        }
    }
    text_write(out, "}")
}
fn docs(out: &mut Text, index: &Index<'_>, span: &Span) -> Result<(), Error> {
    // Use the same real /// attachment rule as documentation, without repeatedly
    // lexing or copying every comment in the file. Plaintext is intentional.
    let mut pending = Vec::new();
    for token in &index.tokens {
        if token.span.start >= span.start {
            break;
        }
        let s = &index.source[token.span.clone()];
        match token.kind {
            K::Comment if s.starts_with("///") && !s.starts_with("////") => {
                pending.push(s[3..].strip_prefix(' ').unwrap_or(&s[3..]))
            }
            K::Whitespace => {
                if s.bytes().filter(|b| *b == b'\n').count() > 1 {
                    pending.clear();
                }
            }
            _ => pending.clear(),
        }
    }
    for line in pending {
        text_write(out, "\n")?;
        text_write(out, line)?;
    }
    Ok(())
}
/// Standard LSP plaintext Hover. No source or comment is interpreted as markup.
pub fn hover(
    snapshot: &CheckedSource,
    index: &LineIndex,
    at: Position,
    limits: Limits,
) -> Result<Option<Value>, Error> {
    let data = Index::new(snapshot, index, limits)?;
    let offset = index
        .offset(at)
        .map_err(|e| Error::InvalidPosition(e.to_string()))?;
    let mut out = Text {
        value: String::new(),
        cap: limits.max_output_bytes,
    };
    let mut work = Budget {
        left: limits.max_type_work,
        depth: limits.max_depth,
    };
    let selected;
    if let Some(node) = data
        .nodes
        .iter()
        .filter(|n| contains(&n.expr.span, offset))
        .min_by_key(|n| n.expr.span.len())
    {
        // Some place bases have no expression-type entry. Only an exact source
        // parameter matched to the checked World may supply that missing fact;
        // raw source projections or contextually inferred closure types cannot.
        let ty = snapshot
            .checked()
            .expression_types
            .get(&(node.expr.span.start, node.expr.span.end))
            .or_else(|| {
                node.binding
                    .as_ref()
                    .and_then(|b| b.param)
                    .and_then(|p| {
                        snapshot
                            .world()
                            .functions
                            .get(&node.function.name)
                            .filter(|f| f.span == node.function.span)
                            .and_then(|f| {
                                f.params
                                    .iter()
                                    .find(|actual| actual.span == p.span && actual.name == p.name)
                            })
                    })
                    .map(|p| &p.ty)
                    .filter(|t| **t != Type::Infer)
            });
        let Some(ty) = ty else {
            return Ok(None);
        };
        text_write(&mut out, "type: ")?;
        print_type(&mut out, ty, &mut work, 0)?;
        let mode = checked_mode(snapshot.world(), ty, node.function, &mut work)?;
        text_write(&mut out, "\ntype mode: ")?;
        text_write(&mut out, mode_name(mode))?;
        text_write(&mut out, " (not current ownership or a live obligation)")?;
        if let Some(binding) = &node.binding
            && let Some(p) = binding.param
        {
            text_write(&mut out, "\nparameter: ")?;
            text_write(&mut out, pass_name(p.pass))?;
            text_write(&mut out, " ")?;
            text_write(&mut out, &p.name)?;
            selectors(&mut out, p, &mut work)?;
        }
        if let Some(target) = &node.target
            && let Some(d) = data.declarations.iter().find(|d| &d.selection == target)
            && let Some(f) = d.function
        {
            text_write(&mut out, "\n")?;
            signature(&mut out, f, snapshot.world(), &mut work)?;
            docs(&mut out, &data, &d.span)?;
        }
        selected = &node.expr.span;
    } else if let Some(d) = data
        .declarations
        .iter()
        .find(|d| contains(&d.selection, offset))
    {
        if let Some(f) = d.function {
            signature(&mut out, f, snapshot.world(), &mut work)?;
        } else {
            text_write(&mut out, d.name)?;
            if let Some(a) = snapshot.world().adts.get(d.name) {
                text_write(&mut out, "\ndeclared mode: ")?;
                text_write(&mut out, mode_name(a.mode))?;
            }
        }
        docs(&mut out, &data, &d.span)?;
        selected = &d.selection;
    } else {
        return Ok(None);
    }
    let result = json!({"contents":{"kind":"plaintext","value":out.value},"range":source_range(index,selected)?});
    output(&result, limits.max_output_bytes)?;
    Ok(Some(result))
}
/// Source-only lexical resolution. No ambient filesystem or cross-file guesses.
pub fn definition(
    uri_label: &str,
    snapshot: &CheckedSource,
    index: &LineIndex,
    at: Position,
    limits: Limits,
) -> Result<Option<Value>, Error> {
    uri(uri_label, limits)?;
    let data = Index::new(snapshot, index, limits)?;
    let offset = index
        .offset(at)
        .map_err(|e| Error::InvalidPosition(e.to_string()))?;
    let target = data
        .nodes
        .iter()
        .filter(|n| contains(&n.expr.span, offset))
        .min_by_key(|n| n.expr.span.len())
        .and_then(|n| n.target.as_ref());
    let fallback;
    let target = if let Some(target) = target {
        target
    } else {
        fallback = doc_links::target(snapshot, &data.tokens, offset, limits)?;
        let Some(target) = fallback.as_ref() else {
            return Ok(None);
        };
        target
    };
    let result = json!({"uri":uri_label,"range":source_range(index,target)?});
    output(&result, limits.max_output_bytes)?;
    Ok(Some(result))
}
/// Current-source declaration ranges; no signature-hash-based range caching.
pub fn document_symbols(
    snapshot: &CheckedSource,
    index: &LineIndex,
    limits: Limits,
) -> Result<Vec<Value>, Error> {
    let data = Index::new(snapshot, index, limits)?;
    let mut result = Vec::new();
    let mut remaining = limits.max_output_bytes;
    for d in data.declarations {
        if d.name.len() > remaining {
            return Err(Error::Resource("output bytes"));
        }
        let value = json!({"name":d.name,"kind":d.kind,"range":source_range(index,&d.span)?,"selectionRange":source_range(index,&d.selection)?});
        output(&value, remaining)?;
        let bytes = serde_json::to_vec(&value)
            .map_err(|_| Error::Resource("output bytes"))?
            .len();
        remaining = remaining
            .checked_sub(bytes + 1)
            .ok_or(Error::Resource("output bytes"))?;
        result.push(value);
    }
    output(&json!(&result), limits.max_output_bytes)?;
    Ok(result)
}
/// An explicit caller-owned open document, never authority to read its URI.
pub struct OpenDocument<'a> {
    pub uri: &'a str,
    pub index: &'a LineIndex,
    pub snapshot: &'a CheckedSource,
}
pub fn workspace_symbols(
    documents: &[OpenDocument<'_>],
    query: &str,
    limits: Limits,
) -> Result<Vec<Value>, Error> {
    limits.validate()?;
    if documents.len() > limits.max_documents || query.len() > limits.max_query_bytes {
        return Err(Error::Resource("documents/query"));
    }
    let mut result = Vec::new();
    let mut remaining = limits.max_output_bytes;
    for doc in documents {
        uri(doc.uri, limits)?;
        let data = Index::new(doc.snapshot, doc.index, limits)?;
        for d in data.declarations {
            if !d.name.contains(query) {
                continue;
            }
            if result.len() >= limits.max_symbols {
                return Err(Error::Resource("symbols"));
            }
            if d.name
                .len()
                .checked_add(doc.uri.len())
                .is_none_or(|n| n > remaining)
            {
                return Err(Error::Resource("output bytes"));
            }
            let value = json!({"name":d.name,"kind":d.kind,"location":{"uri":doc.uri,"range":source_range(doc.index,&d.selection)?}});
            output(&value, remaining)?;
            let bytes = serde_json::to_vec(&value)
                .map_err(|_| Error::Resource("output bytes"))?
                .len();
            remaining = remaining
                .checked_sub(bytes + 1)
                .ok_or(Error::Resource("output bytes"))?;
            result.push(value);
        }
    }
    output(&json!(&result), limits.max_output_bytes)?;
    Ok(result)
}
