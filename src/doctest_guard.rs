//! The fail-closed source profile of `vesper test --doc` (the checks a
//! doctest module must pass after parsing and before the checker runs)
//! behind a swap point (self-hosting SH5 lane 14, #1337).
//!
//! The Rust implementation (moved here unchanged from `src/doctest.rs`) is
//! the default [`DocTestGuardTable`]. `@vesper/test-runner`
//! (packages/vesper-test-runner/src/guard.ves) implements the same check
//! over the source; tests/self_host_doctest_guard.rs runs both on the
//! repository's corpus and requires equal results.

/// The limits [`guard`] enforces (`DocTestLimits` fields).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuardLimits {
    pub max_declarations: usize,
    pub max_nominal_expansion: usize,
}

/// The swappable function: the guard of a module parsed without
/// diagnostics, given both its source and the parsed module.
#[derive(Clone, Copy)]
pub struct DocTestGuardTable {
    /// [`guard`].
    pub guard: fn(&str, &crate::ast::Module, GuardLimits) -> Result<(), String>,
}

/// The Rust implementation, the default [`DocTestGuardTable`].
pub const RUST_DOCTEST_GUARD: DocTestGuardTable = DocTestGuardTable {
    guard: |_, module, limits| rust_guard(module, limits),
};

static TABLE: std::sync::OnceLock<DocTestGuardTable> = std::sync::OnceLock::new();

/// Replace [`RUST_DOCTEST_GUARD`] for the rest of the process. Only the
/// first call takes effect; a later one returns its table back.
pub fn install_doctest_guard(table: DocTestGuardTable) -> Result<(), DocTestGuardTable> {
    TABLE.set(table)
}

/// The guard of `module`, the diagnostic-free parse of `source`.
pub fn guard(source: &str, module: &crate::ast::Module, limits: GuardLimits) -> Result<(), String> {
    (TABLE.get().unwrap_or(&RUST_DOCTEST_GUARD).guard)(source, module, limits)
}

/// Conservative expansion guard BEFORE World::mode/checking. Repeated nominal
/// fields unfold independently against one global budget. Recursive nominal
/// types are explicitly unsupported: a name-only cycle shortcut would not bound
/// the checker's mode-keyed generic cycle exploration.
fn nominal_expansion(module: &crate::ast::Module, limit: usize) -> Result<(), String> {
    use crate::ast::{Adt, Item, Type};
    use std::collections::BTreeMap;
    let adts: BTreeMap<_, _> = module
        .items
        .iter()
        .filter_map(|i| {
            if let Item::Adt(a) = i {
                Some((a.name.as_str(), a))
            } else {
                None
            }
        })
        .collect();
    fn visit<'a>(
        ty: &Type,
        adts: &BTreeMap<&'a str, &'a Adt>,
        path: &mut Vec<String>,
        remaining: &mut usize,
        depth: usize,
    ) -> Result<(), String> {
        *remaining = remaining
            .checked_sub(1)
            .ok_or("doctest nominal expansion limit")?;
        if depth > 48 {
            return Err("doctest nominal expansion depth limit".into());
        }
        match ty {
            Type::Named(name, args) => {
                for ty in args {
                    visit(ty, adts, path, remaining, depth + 1)?;
                }
                if let Some(adt) = adts.get(name.as_str()) {
                    if path.contains(name) {
                        return Err(
                            "recursive nominal doctest types are unsupported before checking"
                                .into(),
                        );
                    }
                    path.push(name.clone());
                    for field in adt.variants.iter().flat_map(|v| &v.fields) {
                        visit(&field.ty, adts, path, remaining, depth + 1)?;
                    }
                    path.pop();
                }
            }
            Type::Tuple(types) => {
                for ty in types {
                    visit(ty, adts, path, remaining, depth + 1)?;
                }
            }
            Type::Function { params, result, .. } => {
                for (_, ty) in params {
                    visit(ty, adts, path, remaining, depth + 1)?;
                }
                visit(result, adts, path, remaining, depth + 1)?;
            }
            _ => {}
        }
        Ok(())
    }
    let mut remaining = limit;
    for name in adts.keys() {
        visit(
            &Type::named(name),
            &adts,
            &mut Vec::new(),
            &mut remaining,
            0,
        )?;
    }
    Ok(())
}
/// Fail-closed source profile, called after parsing and BEFORE checker inference.
/// This guard never invokes the checker. Inferred composite growth is not modeled:
/// nonempty tuples, generic producers/applications, closures and handlers are
/// unsupported. Closed nongeneric nominal data (including flat linear cleanup)
/// remains available under the separate nominal expansion bound.
pub fn rust_guard(module: &crate::ast::Module, limits: GuardLimits) -> Result<(), String> {
    use crate::ast::*;
    enum Node<'a> {
        Expr(&'a Expr, usize),
        Ty(&'a Type, usize, bool),
        Pat(&'a Pattern, usize),
    }
    fn function<'a>(
        f: &'a Function,
        implicit_self: bool,
        todo: &mut Vec<Node<'a>>,
    ) -> Result<(), String> {
        if !f.generics.is_empty() {
            return Err(
                "doctest inferred composite growth: generic functions unsupported before checking"
                    .into(),
            );
        }
        todo.push(Node::Ty(&f.result, 1, false));
        for p in &f.params {
            if implicit_self && p.name == "self" && p.ty == Type::Infer {
                continue;
            }
            todo.push(Node::Ty(&p.ty, 1, false));
        }
        for effect in &f.effects.instances {
            for ty in &effect.args {
                todo.push(Node::Ty(ty, 1, false));
            }
        }
        if let Some(body) = &f.body {
            todo.push(Node::Expr(body, 1));
        }
        Ok(())
    }
    fn builtin_constructor(name: &str) -> bool {
        matches!(
            name.rsplit("::").next(),
            Some("Some" | "None" | "Ok" | "Err" | "Option" | "Result")
        )
    }
    if module.items.len() > limits.max_declarations {
        return Err("doctest parsed module item limit".into());
    }
    let mut todo = Vec::new();
    for item in &module.items {
        match item {
            Item::Function(f) => function(f, false, &mut todo)?,
            Item::Adt(a) => {
                if !a.generics.is_empty() {
                    return Err("doctest inferred composite growth: generic nominal types unsupported before checking".into());
                }
                for field in a.variants.iter().flat_map(|v| &v.fields) {
                    todo.push(Node::Ty(&field.ty, 1, false));
                }
            }
            Item::Effect(e) => {
                if !e.generics.is_empty() {
                    return Err(
                        "doctest generic effect declarations unsupported before checking".into(),
                    );
                }
                for f in &e.ops {
                    function(f, false, &mut todo)?;
                }
            }
            Item::Impl {
                ty,
                generics,
                methods,
            } => {
                if !generics.is_empty() {
                    return Err(
                        "doctest generic implementations unsupported before checking".into(),
                    );
                }
                todo.push(Node::Ty(ty, 1, false));
                for f in methods {
                    function(f, true, &mut todo)?;
                }
            }
            Item::Trait(_) | Item::Conformance(_) => {
                return Err("doctest trait/model inference unsupported before checking".into());
            }
        }
        if todo.len() > 16384 {
            return Err("doctest parsed AST volume limit".into());
        }
    }
    let mut count = 0usize;
    while let Some(node) = todo.pop() {
        count = count.checked_add(1).ok_or("parsed AST count overflow")?;
        if count > 16384 {
            return Err("doctest parsed AST volume limit".into());
        }
        let depth = match &node {
            Node::Expr(_, d) | Node::Ty(_, d, _) | Node::Pat(_, d) => *d,
        };
        if depth > 64 {
            return Err("doctest parsed AST depth limit".into());
        }
        let next = depth.checked_add(1).ok_or("parsed AST depth overflow")?;
        match node {
            Node::Ty(ty,_,allow_infer) => match ty {
                Type::Tuple(ts) if !ts.is_empty() => return Err("doctest inferred composite growth: nonempty tuple types unsupported before checking".into()),
                Type::Named(name,args) if !args.is_empty() || matches!(name.as_str(), "Option" | "Result") => return Err("doctest inferred composite growth: generic type applications unsupported before checking".into()),
                Type::Infer if !allow_infer => return Err("doctest inferred function/field signatures unsupported before checking".into()),
                Type::Function { params, result, effects, .. } => {
                    for (_,t) in params { todo.push(Node::Ty(t,next,false)); }
                    todo.push(Node::Ty(result,next,false));
                    for e in &effects.instances { for ty in &e.args { todo.push(Node::Ty(ty,next,false)); } }
                }
                Type::Row(effects) => for e in &effects.instances { for ty in &e.args { todo.push(Node::Ty(ty,next,false)); } },
                _ => {}
            },
            Node::Pat(p,_) => match &p.kind {
                PatternKind::Tuple(ps) if !ps.is_empty() => return Err("doctest inferred composite growth: tuple patterns unsupported before checking".into()),
                PatternKind::Constructor { name,fields,.. } => {
                    if builtin_constructor(name) { return Err("doctest inferred composite growth: builtin generic constructors unsupported before checking".into()); }
                    for (_,p) in fields { todo.push(Node::Pat(p,next)); }
                }
                _ => {}
            },
            Node::Expr(expr,_) => match &expr.kind {
                ExprKind::Tuple(es) if !es.is_empty() => return Err("doctest inferred composite growth: nonempty tuples unsupported before checking".into()),
                ExprKind::Closure { .. } | ExprKind::Handle { .. } => return Err("doctest inferred composite growth: closures/handlers unsupported before checking".into()),
                ExprKind::Name(name) if builtin_constructor(name) => return Err("doctest inferred composite growth: builtin generic constructors unsupported before checking".into()),
                ExprKind::Struct { name, fields } => {
                    if builtin_constructor(name) { return Err("doctest builtin generic constructors unsupported before checking".into()); }
                    for (_,e) in fields { todo.push(Node::Expr(e,next)); }
                }
                ExprKind::Block { statements,tail } => {
                    for statement in statements { match statement {
                        Statement::Let {pat,ty,value, ..} => { todo.push(Node::Pat(pat,next)); todo.push(Node::Ty(ty,next,true)); todo.push(Node::Expr(value,next)); }
                        Statement::Expr(e) => todo.push(Node::Expr(e,next)),
                    } }
                    if let Some(e) = tail { todo.push(Node::Expr(e,next)); }
                }
                ExprKind::Call { callee,args,types } => {
                    if !types.is_empty() { return Err("doctest inferred composite growth: generic calls unsupported before checking".into()); }
                    todo.push(Node::Expr(callee,next));
                    for a in args { todo.push(Node::Expr(&a.expr,next)); }
                }
                ExprKind::Field(_,name) if builtin_constructor(name) => return Err("doctest inferred composite growth: builtin generic constructors unsupported before checking".into()),
                ExprKind::Unary(_,e) | ExprKind::Field(e,_) | ExprKind::Try(e) => todo.push(Node::Expr(e,next)),
                ExprKind::Binary(_,a,b) | ExprKind::Assign(_,a,b) | ExprKind::Index(a,b) => { todo.push(Node::Expr(a,next)); todo.push(Node::Expr(b,next)); }
                ExprKind::If { condition,then,otherwise } => { todo.push(Node::Expr(condition,next)); todo.push(Node::Expr(then,next)); if let Some(e) = otherwise { todo.push(Node::Expr(e,next)); } }
                ExprKind::Match { value,arms } => { todo.push(Node::Expr(value,next)); for (p,e) in arms { todo.push(Node::Pat(p,next)); todo.push(Node::Expr(e,next)); } }
                _ => {}
            },
        }
        if count
            .checked_add(todo.len())
            .ok_or("parsed AST count overflow")?
            > 16384
        {
            return Err("doctest parsed AST volume limit".into());
        }
    }
    nominal_expansion(module, limits.max_nominal_expansion)
}
