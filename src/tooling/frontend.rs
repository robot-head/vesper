//! Bounded editor snapshots from the actual parser and checker. No I/O, host
//! execution, proof execution, caller-supplied World or trusted boolean occurs.
use crate::{ast::*, check, lexer, parser};
use std::{collections::BTreeSet, fmt::Write as _, io, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AnalysisLimits {
    pub max_source_bytes: usize,
    pub max_ast_nodes: usize,
    pub max_type_nodes: usize,
    pub max_depth: usize,
    pub max_identifier_bytes: usize,
    pub max_printed_type_bytes: usize,
    pub max_diagnostics: usize,
    pub max_diagnostic_bytes: usize,
    /// Serialized AST/type metadata bytes; a logical bound, not RSS.
    pub max_metadata_bytes: usize,
}
impl Default for AnalysisLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: 131_072,
            max_ast_nodes: 2048,
            max_type_nodes: 2048,
            max_depth: 48,
            max_identifier_bytes: 256,
            max_printed_type_bytes: 2048,
            max_diagnostics: 64,
            max_diagnostic_bytes: 65_536,
            max_metadata_bytes: 524_288,
        }
    }
}
impl AnalysisLimits {
    pub fn validate(self) -> Result<(), AnalysisFailure> {
        for (value, hard) in [
            (self.max_source_bytes, 262_144),
            (self.max_ast_nodes, 8192),
            (self.max_type_nodes, 32768),
            (self.max_depth, 64),
            (self.max_identifier_bytes, 1024),
            (self.max_printed_type_bytes, 8192),
            (self.max_diagnostics, 256),
            (self.max_diagnostic_bytes, 262_144),
            (self.max_metadata_bytes, 2_097_152),
        ] {
            if value == 0 || value > hard {
                return Err(resource("invalid analysis limits"));
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnalysisFailure {
    Parse { diagnostics: Vec<Diagnostic> },
    Semantic { diagnostics: Vec<Diagnostic> },
    Resource { reason: String },
    Unsupported { reason: String },
}
impl AnalysisFailure {
    pub fn diagnostics(&self) -> &[Diagnostic] {
        match self {
            Self::Parse { diagnostics } | Self::Semantic { diagnostics } => diagnostics,
            _ => &[],
        }
    }
}
impl std::fmt::Display for AnalysisFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resource { reason } | Self::Unsupported { reason } => f.write_str(reason),
            Self::Parse { diagnostics } => {
                write!(f, "parse failed ({} diagnostics)", diagnostics.len())
            }
            Self::Semantic { diagnostics } => write!(
                f,
                "semantic check failed ({} diagnostics)",
                diagnostics.len()
            ),
        }
    }
}
impl std::error::Error for AnalysisFailure {}
fn resource(reason: &str) -> AnalysisFailure {
    AnalysisFailure::Resource {
        reason: reason.into(),
    }
}
fn guard_failure(reason: String) -> AnalysisFailure {
    if reason.contains("budget") || reason.contains("depth") || reason.contains("limit") {
        AnalysisFailure::Resource { reason }
    } else {
        AnalysisFailure::Unsupported { reason }
    }
}
/// Private construction ensures every published World belongs to these exact
/// bytes and passed the editor guards before checker traversal/cloning.
#[derive(Debug)]
pub struct CheckedSource {
    source: Arc<str>,
    source_hash: [u8; 32],
    parsed: parser::Parse,
    checked: check::Checked,
}
impl CheckedSource {
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn source_hash(&self) -> [u8; 32] {
        self.source_hash
    }
    pub fn parsed(&self) -> &parser::Parse {
        &self.parsed
    }
    pub fn checked(&self) -> &check::Checked {
        &self.checked
    }
    pub fn world(&self) -> &check::World {
        &self.checked.world
    }
}
fn diagnostics_bounded(
    diagnostics: &[Diagnostic],
    source: &str,
    limits: AnalysisLimits,
) -> Result<(), AnalysisFailure> {
    if diagnostics.len() > limits.max_diagnostics {
        return Err(resource("analysis diagnostic count exceeded"));
    }
    let mut bytes = 0usize;
    for d in diagnostics {
        bytes = bytes
            .checked_add(d.code.len())
            .and_then(|n| n.checked_add(d.message.len()))
            .ok_or_else(|| resource("analysis diagnostic size overflow"))?;
        if bytes > limits.max_diagnostic_bytes
            || d.span.start > d.span.end
            || d.span.end > source.len()
            || !source.is_char_boundary(d.span.start)
            || !source.is_char_boundary(d.span.end)
        {
            return Err(resource("analysis diagnostic metadata exceeded"));
        }
    }
    Ok(())
}
struct Bytes {
    remaining: usize,
}
impl io::Write for Bytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.remaining = self
            .remaining
            .checked_sub(bytes.len())
            .ok_or_else(|| io::Error::other("metadata budget"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn metadata<T: serde::Serialize>(value: &T, remaining: &mut usize) -> Result<(), AnalysisFailure> {
    let mut writer = Bytes {
        remaining: *remaining,
    };
    serde_json::to_writer(&mut writer, value)
        .map_err(|_| resource("analysis metadata byte budget exceeded"))?;
    *remaining = writer.remaining;
    Ok(())
}
struct Printed {
    remaining: usize,
}
impl std::fmt::Write for Printed {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.remaining = self.remaining.checked_sub(s.len()).ok_or(std::fmt::Error)?;
        Ok(())
    }
}
fn printed_type(ty: &Type, limits: AnalysisLimits) -> Result<(), AnalysisFailure> {
    write!(
        &mut Printed {
            remaining: limits.max_printed_type_bytes
        },
        "{ty}"
    )
    .map_err(|_| resource("analysis printed type budget exceeded"))
}
fn type_budget<'a>(
    types: impl IntoIterator<Item = &'a Type>,
    limits: AnalysisLimits,
    remaining: &mut usize,
) -> Result<(), AnalysisFailure> {
    let mut roots = Vec::new();
    for ty in types {
        if roots.len() >= *remaining {
            return Err(resource("analysis type node budget exceeded"));
        }
        roots.push(ty);
    }
    let mut stack: Vec<_> = roots.iter().map(|ty| (*ty, 0usize)).collect();
    while let Some((ty, depth)) = stack.pop() {
        *remaining = remaining
            .checked_sub(1)
            .ok_or_else(|| resource("analysis type node budget exceeded"))?;
        if depth > limits.max_depth {
            return Err(resource("analysis type depth exceeded"));
        }
        match ty {
            Type::Named(_, ts) | Type::Tuple(ts) => stack.extend(ts.iter().map(|t| (t, depth + 1))),
            Type::Function {
                params,
                result,
                effects,
                ..
            } => {
                stack.extend(params.iter().map(|(_, t)| (t, depth + 1)));
                stack.push((result, depth + 1));
                stack.extend(
                    effects
                        .instances
                        .iter()
                        .flat_map(|e| &e.args)
                        .map(|t| (t, depth + 1)),
                );
            }
            Type::Row(e) => stack.extend(
                e.instances
                    .iter()
                    .flat_map(|e| &e.args)
                    .map(|t| (t, depth + 1)),
            ),
            _ => (),
        }
        if stack.len() > *remaining {
            return Err(resource("analysis type pending node budget exceeded"));
        }
    }
    // Formatting happens only after the full structures are depth/node checked.
    for ty in roots {
        printed_type(ty, limits)?;
    }
    Ok(())
}
/// New editor profile. Legacy doc/lint entry points retain their original
/// independent profiles; this API never silently changes those CLI contracts.
pub fn check_source(
    source: &str,
    limits: AnalysisLimits,
) -> Result<CheckedSource, AnalysisFailure> {
    limits.validate()?;
    if source.len() > limits.max_source_bytes {
        return Err(resource("analysis source byte budget exceeded"));
    }
    let lexed = lexer::lex_with_limits(
        source,
        lexer::LexLimits {
            max_source_bytes: limits.max_source_bytes,
            max_tokens: 16_384,
            max_diagnostics: limits.max_diagnostics,
        },
    )
    .map_err(|_| resource("analysis lexical limits exceeded"))?;
    if lexed.limit.is_some() {
        return Err(resource("analysis lexical limits exceeded"));
    }
    diagnostics_bounded(&lexed.diagnostics, source, limits)?;
    if !lexed.diagnostics.is_empty() {
        return Err(AnalysisFailure::Parse {
            diagnostics: lexed.diagnostics,
        });
    }
    for t in &lexed.tokens {
        if matches!(t.kind, lexer::TokenKind::Lower | lexer::TokenKind::Upper)
            && t.span.len() > limits.max_identifier_bytes
        {
            return Err(resource("analysis identifier byte budget exceeded"));
        }
    }
    let doc_limits = vesper_doc::Limits {
        max_source_bytes: limits.max_source_bytes,
        max_type_nodes: limits.max_type_nodes,
        ..vesper_doc::Limits::default()
    };
    vesper_doc::source_preflight(source, doc_limits).map_err(guard_failure)?;
    let parsed = parser::parse_with_limits(
        source,
        parser::ParseLimits {
            max_source_bytes: limits.max_source_bytes,
            max_tokens: 16_384,
            max_diagnostics: limits.max_diagnostics,
            max_depth: limits.max_depth,
        },
    )
    .map_err(|_| resource("analysis parser limits exceeded"))?;
    if parsed.diagnostics.iter().any(|d| d.code == "E0003") {
        return Err(resource("analysis parser limits exceeded"));
    }
    diagnostics_bounded(&parsed.diagnostics, source, limits)?;
    if !parsed.diagnostics.is_empty() {
        return Err(AnalysisFailure::Parse {
            diagnostics: parsed.diagnostics,
        });
    }
    editor_ast_guard(&parsed.module, limits).map_err(guard_failure)?;
    super::doc::surface_expansion(&parsed.module, limits.max_type_nodes).map_err(guard_failure)?;
    let mut bytes = limits.max_metadata_bytes;
    metadata(&parsed.module, &mut bytes)?;
    let checked = check::check(&parsed.module);
    diagnostics_bounded(&checked.diagnostics, source, limits)?;
    if !checked.diagnostics.is_empty() {
        return Err(AnalysisFailure::Semantic {
            diagnostics: checked.diagnostics,
        });
    }
    let mut remaining = limits.max_type_nodes;
    for ts in [&checked.expression_types, &checked.world.types] {
        type_budget(ts.values(), limits, &mut remaining)?;
        for (key, ty) in ts {
            metadata(key, &mut bytes)?;
            metadata(ty, &mut bytes)?;
        }
    }
    type_budget(
        checked.world.function_results.values(),
        limits,
        &mut remaining,
    )?;
    metadata(&checked.world.function_results, &mut bytes)?;
    for vars in checked.world.calls.values() {
        type_budget(vars.values(), limits, &mut remaining)?;
        metadata(vars, &mut bytes)?;
    }
    for row in checked.world.handler_effects.values() {
        type_budget(
            row.instances.iter().flat_map(|e| &e.args),
            limits,
            &mut remaining,
        )?;
        metadata(row, &mut bytes)?;
    }
    metadata(&checked.world.elaboration, &mut bytes)?;
    for places in checked.world.moves.values() {
        metadata(places, &mut bytes)?;
    }
    Ok(CheckedSource {
        source: Arc::from(source),
        source_hash: *blake3::hash(source.as_bytes()).as_bytes(),
        parsed,
        checked,
    })
}

fn editor_ast_guard(module: &crate::ast::Module, limits: AnalysisLimits) -> Result<(), String> {
    use crate::ast::{
        Clause, Expr, ExprKind as E, Function, Item, Pattern, PatternKind as P, Statement,
    };
    enum Node<'a> {
        Item(&'a Item),
        Function(&'a Function),
        Type(&'a Type),
        Effects(&'a Effects),
        Expr(&'a Expr),
        Pattern(&'a Pattern),
        Clause(&'a Clause),
    }
    let mut generic_values = BTreeSet::new();
    let mut generic_constructors: BTreeSet<String> = [
        "Some",
        "None",
        "Ok",
        "Err",
        "Option::Some",
        "Option::None",
        "Result::Ok",
        "Result::Err",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    for item in &module.items {
        match item {
            Item::Function(f) if !f.generics.is_empty() => {
                generic_values.insert(f.name.clone());
            }
            Item::Impl {
                ty,
                generics,
                methods,
            } => {
                for f in methods {
                    if !generics.is_empty() || !f.generics.is_empty() {
                        generic_values.insert(f.name.clone());
                        generic_values.insert(format!("{}::{}", ty.name(), f.name));
                    }
                }
            }
            Item::Adt(a) if !a.generics.is_empty() => {
                generic_constructors.insert(a.name.clone());
                for v in &a.variants {
                    generic_constructors.insert(v.name.clone());
                    generic_constructors.insert(format!("{}::{}", a.name, v.name));
                }
            }
            Item::Trait(t) => {
                for f in &t.methods {
                    if !t.generics.is_empty() || !f.generics.is_empty() {
                        generic_values.insert(f.name.clone());
                        generic_values.insert(format!("{}::{}", t.name, f.name));
                    }
                }
            }
            Item::Conformance(c) => {
                if !c.generics.is_empty() {
                    return Err(
                        "generic conformance inference is unsupported before checking".into(),
                    );
                }
                for f in &c.methods {
                    if !f.generics.is_empty() {
                        generic_values.insert(f.name.clone());
                        generic_values.insert(format!("{}::{}", c.self_ty.name(), f.name));
                        generic_values.insert(format!("{}::{}", c.trait_ref.name, f.name));
                    }
                }
            }
            _ => (),
        }
    }
    let mut allowed_closures = BTreeSet::new();
    let mut stack: Vec<_> = module
        .items
        .iter()
        .map(|item| (Node::Item(item), 0usize))
        .collect();
    for bound in &module.trait_bounds {
        for arg in &bound.trait_ref.args {
            stack.push((Node::Type(arg), 0));
        }
    }
    let mut count = module
        .model_uses
        .len()
        .checked_add(module.trait_bounds.len())
        .ok_or("analysis AST metadata overflow")?;
    if count > limits.max_ast_nodes {
        return Err("analysis AST metadata budget exceeded".into());
    }
    while let Some((node, depth)) = stack.pop() {
        count += 1;
        if count > limits.max_ast_nodes || depth > limits.max_depth {
            return Err("parsed AST node/depth budget exceeded".into());
        }
        let next = depth + 1;
        if let Node::Type(ty) = &node {
            printed_type(ty, limits).map_err(|e| e.to_string())?;
        }
        if stack.len() > limits.max_ast_nodes {
            return Err("analysis pending AST node budget exceeded".into());
        }
        match node {
            Node::Item(item) => match item {
                Item::Adt(a) => {
                    count = count
                        .checked_add(a.variants.len() + a.generics.len())
                        .ok_or("analysis AST metadata overflow")?;
                    if count > limits.max_ast_nodes {
                        return Err("analysis AST metadata budget exceeded".into());
                    }
                    for f in a.variants.iter().flat_map(|v| &v.fields) {
                        stack.push((Node::Type(&f.ty), next));
                    }
                }
                Item::Function(f) => stack.push((Node::Function(f), next)),
                Item::Effect(e) => {
                    if e.generics.len() > 64 {
                        return Err("analysis effect generic count limit exceeded".into());
                    }
                    count = count
                        .checked_add(e.generics.len())
                        .ok_or("analysis AST metadata overflow")?;
                    for f in &e.ops {
                        stack.push((Node::Function(f), next));
                    }
                }
                Item::Impl { ty, methods, .. } => {
                    stack.push((Node::Type(ty), next));
                    for f in methods {
                        stack.push((Node::Function(f), next));
                    }
                }
                Item::Trait(t) => {
                    for f in &t.methods {
                        stack.push((Node::Function(f), next));
                    }
                }
                Item::Conformance(c) => {
                    stack.push((Node::Type(&c.self_ty), next));
                    for t in &c.trait_ref.args {
                        stack.push((Node::Type(t), next));
                    }
                    for b in &c.assoc {
                        stack.push((Node::Type(&b.ty), next));
                    }
                    for w in &c.where_clauses {
                        stack.push((Node::Type(&w.ty), next));
                        for t in &w.trait_ref.args {
                            stack.push((Node::Type(t), next));
                        }
                    }
                    for f in &c.methods {
                        stack.push((Node::Function(f), next));
                    }
                }
            },
            Node::Function(f) => {
                if f.params.len() > 64 || f.generics.len() > 64 {
                    return Err("analysis function parameter/generic count limit exceeded".into());
                }
                count = count
                    .checked_add(f.params.len() + f.generics.len())
                    .ok_or("analysis AST metadata overflow")?;
                if count > limits.max_ast_nodes {
                    return Err("analysis AST metadata budget exceeded".into());
                }
                if matches!(f.result, Type::Function { .. })
                    && let Some(Expr {
                        kind:
                            E::Block {
                                tail: Some(tail), ..
                            },
                        ..
                    }) = &f.body
                    && matches!(tail.kind, E::Closure { .. })
                {
                    allowed_closures.insert((tail.span.start, tail.span.end));
                }
                for p in &f.params {
                    for path in &p.projections {
                        count = count
                            .checked_add(path.len() + 1)
                            .ok_or("analysis AST metadata overflow")?;
                        if count > limits.max_ast_nodes {
                            return Err("analysis selector metadata budget exceeded".into());
                        }
                    }
                    stack.push((Node::Type(&p.ty), next));
                }
                stack.push((Node::Type(&f.result), next));
                stack.push((Node::Effects(&f.effects), next));
                if let Some(e) = &f.body {
                    stack.push((Node::Expr(e), next));
                }
            }
            Node::Type(t) => match t {
                Type::Named(_, ts) | Type::Tuple(ts) => {
                    for t in ts {
                        stack.push((Node::Type(t), next));
                    }
                }
                Type::Function {
                    params,
                    result,
                    effects,
                    ..
                } => {
                    for (_, t) in params {
                        stack.push((Node::Type(t), next));
                    }
                    stack.push((Node::Type(result), next));
                    stack.push((Node::Effects(effects), next));
                }
                Type::Row(e) => stack.push((Node::Effects(e), next)),
                Type::Infer | Type::IntLiteral(_) => (),
            },
            Node::Effects(e) => {
                if e.instances.len() > 64 {
                    return Err("analysis effect instance count limit exceeded".into());
                }
                count = count
                    .checked_add(e.instances.len())
                    .ok_or("analysis AST metadata overflow")?;
                if count > limits.max_ast_nodes {
                    return Err("analysis AST metadata budget exceeded".into());
                }
                for effect in &e.instances {
                    for t in &effect.args {
                        stack.push((Node::Type(t), next));
                    }
                }
            }
            Node::Pattern(p) => match &p.kind {
                P::Tuple(ps) => {
                    for p in ps {
                        stack.push((Node::Pattern(p), next));
                    }
                }
                P::Constructor { fields, .. } => {
                    for (_, p) in fields {
                        stack.push((Node::Pattern(p), next));
                    }
                }
                _ => (),
            },
            Node::Clause(c) => {
                for p in &c.pats {
                    stack.push((Node::Pattern(p), next));
                }
                stack.push((Node::Expr(&c.body), next));
            }
            Node::Expr(e) => match &e.kind {
                E::Literal(_) => (),
                E::Name(name) => {
                    if generic_values.contains(name) || generic_constructors.contains(name) {
                        return Err(
                            "generic value/constructor inference is unsupported before checking"
                                .into(),
                        );
                    }
                }
                E::Tuple(es) => {
                    if !es.is_empty() {
                        return Err(
                            "inferred tuple aggregate construction is unsupported before checking"
                                .into(),
                        );
                    }
                    for e in es {
                        stack.push((Node::Expr(e), next));
                    }
                }
                E::Struct { name, fields, .. } => {
                    if generic_constructors.contains(name) {
                        return Err(
                            "generic aggregate construction is unsupported before checking".into(),
                        );
                    }
                    for (_, e) in fields {
                        stack.push((Node::Expr(e), next));
                    }
                }
                E::Block { statements, tail } => {
                    for s in statements {
                        match s {
                            Statement::Let { pat, ty, value } => {
                                if matches!(ty, Type::Function { .. })
                                    && matches!(value.kind, E::Closure { .. })
                                {
                                    allowed_closures.insert((value.span.start, value.span.end));
                                }
                                stack.push((Node::Pattern(pat), next));
                                stack.push((Node::Type(ty), next));
                                stack.push((Node::Expr(value), next));
                            }
                            Statement::Expr(e) => stack.push((Node::Expr(e), next)),
                        }
                    }
                    if let Some(e) = tail {
                        stack.push((Node::Expr(e), next));
                    }
                }
                E::Call {
                    callee,
                    types,
                    args,
                } => {
                    stack.push((Node::Expr(callee), next));
                    for t in types {
                        stack.push((Node::Type(t), next));
                    }
                    for a in args {
                        stack.push((Node::Expr(&a.expr), next));
                    }
                }
                E::Field(_, name) if generic_values.contains(name) => {
                    return Err("generic method inference is unsupported before checking".into());
                }
                E::Field(e, _) | E::Try(e) | E::Unary(_, e) => stack.push((Node::Expr(e), next)),
                E::Index(a, b) | E::Binary(_, a, b) | E::Assign(_, a, b) => {
                    stack.push((Node::Expr(a), next));
                    stack.push((Node::Expr(b), next));
                }
                E::If {
                    condition,
                    then,
                    otherwise,
                } => {
                    stack.push((Node::Expr(condition), next));
                    stack.push((Node::Expr(then), next));
                    if let Some(e) = otherwise {
                        stack.push((Node::Expr(e), next));
                    }
                }
                E::Match { value, arms } => {
                    stack.push((Node::Expr(value), next));
                    for (p, e) in arms {
                        stack.push((Node::Pattern(p), next));
                        stack.push((Node::Expr(e), next));
                    }
                }
                E::Closure {
                    params,
                    result,
                    effects,
                    body,
                    ..
                } => {
                    if !allowed_closures.contains(&(e.span.start, e.span.end)) {
                        return Err("inferred closure growth is unsupported without a direct complete arrow annotation".into());
                    }
                    for p in params {
                        stack.push((Node::Type(&p.ty), next));
                    }
                    stack.push((Node::Type(result), next));
                    stack.push((Node::Effects(effects), next));
                    stack.push((Node::Expr(body), next));
                }
                E::Handle {
                    effect,
                    state,
                    body,
                    ret,
                    ops,
                } => {
                    for t in &effect.args {
                        stack.push((Node::Type(t), next));
                    }
                    if let Some((p, e)) = state {
                        stack.push((Node::Pattern(p), next));
                        stack.push((Node::Expr(e), next));
                    }
                    stack.push((Node::Expr(body), next));
                    stack.push((Node::Clause(ret), next));
                    for c in ops {
                        stack.push((Node::Clause(c), next));
                    }
                }
            },
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_snapshot_retains_actual_source_hash_ast_world_and_doc_facts() {
        let text = "/// scalar docs\nfn value()->i64{42}";
        let snapshot = check_source(text, AnalysisLimits::default()).unwrap();
        assert_eq!(snapshot.source(), text);
        assert_eq!(
            snapshot.source_hash(),
            *blake3::hash(text.as_bytes()).as_bytes()
        );
        assert!(snapshot.parsed().diagnostics.is_empty());
        assert!(snapshot.checked().diagnostics.is_empty());
        assert!(std::ptr::eq(snapshot.world(), &snapshot.checked().world));
        let doc =
            super::super::doc::document_checked(&snapshot, vesper_doc::Limits::default()).unwrap();
        let old = super::super::doc::document_source(text, vesper_doc::Limits::default()).unwrap();
        assert_eq!(
            serde_json::to_value(doc).unwrap(),
            serde_json::to_value(old).unwrap()
        );
        let changed =
            check_source(&format!("{text}\n// comment"), AnalysisLimits::default()).unwrap();
        assert_ne!(snapshot.source_hash(), changed.source_hash());
    }
    #[test]
    fn parse_semantic_and_resources_never_publish_snapshots() {
        assert!(matches!(
            check_source("fn broken(", AnalysisLimits::default()),
            Err(AnalysisFailure::Parse { .. })
        ));
        let failure = check_source("fn f()->i64{true}", AnalysisLimits::default()).unwrap_err();
        assert!(matches!(failure, AnalysisFailure::Semantic { .. }));
        assert!(!failure.diagnostics().is_empty());
        for limits in [
            AnalysisLimits {
                max_source_bytes: 1,
                ..AnalysisLimits::default()
            },
            AnalysisLimits {
                max_ast_nodes: 1,
                ..AnalysisLimits::default()
            },
            AnalysisLimits {
                max_type_nodes: 1,
                ..AnalysisLimits::default()
            },
            AnalysisLimits {
                max_metadata_bytes: 1,
                ..AnalysisLimits::default()
            },
            AnalysisLimits {
                max_depth: 0,
                ..AnalysisLimits::default()
            },
        ] {
            assert!(
                matches!(
                    check_source("fn f()->i64{1}", limits),
                    Err(AnalysisFailure::Resource { .. })
                ),
                "{limits:?}"
            );
        }
        assert!(matches!(
            check_source(
                "fn f()->i64{true}",
                AnalysisLimits {
                    max_diagnostic_bytes: 1,
                    ..AnalysisLimits::default()
                }
            ),
            Err(AnalysisFailure::Resource { .. })
        ));
        let long = format!("fn {}(){{}}", "x".repeat(257));
        assert!(matches!(
            check_source(&long, AnalysisLimits::default()),
            Err(AnalysisFailure::Resource { .. })
        ));
    }
    #[test]
    fn inferred_aggregate_and_nominal_growth_fail_before_checker() {
        let mut tuple = "fn main(){let x=0;".to_owned();
        for _ in 0..25 {
            tuple.push_str("let x=(x,x);");
        }
        tuple.push('}');
        assert!(matches!(
            check_source(&tuple, AnalysisLimits::default()),
            Err(AnalysisFailure::Unsupported { .. })
        ));
        let mut dag = "copy struct D0{x:i64}".to_owned();
        for i in 1..15 {
            dag.push_str(&format!("copy struct D{i}{{a:D{},b:D{}}}", i - 1, i - 1));
        }
        dag.push_str("fn main(){}");
        assert!(matches!(
            check_source(&dag, AnalysisLimits::default()),
            Err(AnalysisFailure::Resource { .. })
        ));
        // Zero-argument metadata still consumes the editor AST/count profile;
        // exercise guards directly without invoking a checker on hostile input.
        let row = format!(
            "effect E{{fn e();}} fn f() eff[{}]{{}}",
            vec!["E"; 65].join(",")
        );
        let parsed = parser::parse(&row);
        assert!(parsed.diagnostics.is_empty());
        assert!(editor_ast_guard(&parsed.module, AnalysisLimits::default()).is_err());
        let generic_method =
            "trait Dup{fn dup[T:mod copy](in self,in x:T)->(T,T);} fn f(){Dup::dup(0);}";
        let parsed = parser::parse(generic_method);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        assert!(editor_ast_guard(&parsed.module, AnalysisLimits::default()).is_err());
        let uses = "use model M;".repeat(2049);
        let parsed = parser::parse(&uses);
        assert!(parsed.diagnostics.is_empty());
        assert!(editor_ast_guard(&parsed.module, AnalysisLimits::default()).is_err());
        let recursive = "copy struct Node{x:Node} fn main(){}";
        assert!(matches!(
            check_source(recursive, AnalysisLimits::default()),
            Err(AnalysisFailure::Unsupported { .. })
        ));
    }
    #[test]
    fn scalar_inference_and_complete_latent_arrow_are_supported() {
        assert!(matches!(
            check_source("fn value()->_{42}", AnalysisLimits::default()),
            Err(AnalysisFailure::Semantic { .. })
        ));
        let snapshot=check_source("linear effect Ask{fn ask()->i64;} fn make()->fn()->i64 eff[Ask]{|| eff[Ask] Ask::ask()}",AnalysisLimits::default()).unwrap();
        let actual = &snapshot.world().function_results["make"];
        assert!(matches!(actual, Type::Function { .. }));
        let prefix = "linear effect Ask{fn ask()->i64;} fn make()->fn()->i64 eff[Ask]";
        let effectful = check_source(
            &format!("{prefix}{{|| Ask::ask()}}"),
            AnalysisLimits::default(),
        )
        .unwrap();
        let pure = check_source(&format!("{prefix}{{|| 0}}"), AnalysisLimits::default()).unwrap();
        assert_eq!(
            effectful.world().functions["make"].result,
            pure.world().functions["make"].result
        );
        assert_ne!(
            effectful.world().function_results["make"],
            pure.world().function_results["make"]
        );
    }
    #[test]
    fn printed_types_and_diagnostics_have_explicit_metadata_limits() {
        let text = "copy struct SomeLongType{x:i64} fn f(in x:SomeLongType)->SomeLongType{x}";
        assert!(matches!(
            check_source(
                text,
                AnalysisLimits {
                    max_printed_type_bytes: 4,
                    ..AnalysisLimits::default()
                }
            ),
            Err(AnalysisFailure::Resource { .. })
        ));
        let source = (0..8)
            .map(|i| format!("fn f{i}()->i64{{true}} "))
            .collect::<String>();
        assert!(matches!(
            check_source(
                &source,
                AnalysisLimits {
                    max_diagnostics: 2,
                    ..AnalysisLimits::default()
                }
            ),
            Err(AnalysisFailure::Resource { .. })
        ));
    }
}
