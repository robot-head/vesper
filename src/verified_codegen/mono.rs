//! Monomorphization before slot lowering (milestone 8).
//!
//! The checker fixes the type arguments of every call to a generic function
//! (`World::calls`, keyed by the call's span, plus explicit `f[T](..)`
//! arguments). Starting from the entry, this pass collects the reachable
//! instances `(function, type arguments)` and gives each one a copy of its
//! Place IR with every type substituted and every direct callee renamed to
//! the callee's instance. Nested functions the IR made for handler bodies
//! (`$handler_body_*`) are generic in their parent's parameters and are
//! instantiated with the parent's arguments.
//!
//! An instance's name is the function's name when it has no type arguments
//! (so non-generic programs are unchanged), else `name<T=i64,U=bool>`.
//! A call whose type arguments are not all concrete after substitution is a
//! typed [`Unsupported::Generic`]: there is no uniform representation here.
use super::{Unsupported, VerifiedError};
use crate::ast::{ExprKind, Type, substitute};
use crate::check::World;
use crate::ir::{FunctionIr, Op, Program, nested_name};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Type arguments of one instance.
pub type Vars = BTreeMap<String, Type>;

/// The name of `base` instantiated at `vars`.
pub fn instance_name(base: &str, vars: &Vars) -> String {
    if vars.is_empty() {
        return base.to_owned();
    }
    let args: Vec<String> = vars.iter().map(|(k, t)| format!("{k}={t}")).collect();
    format!("{base}<{}>", args.join(","))
}

/// Whether `t` still names a type parameter (or an unsolved type).
pub fn mentions_generic(world: &World, t: &Type) -> bool {
    match t {
        Type::Named(n, args) => {
            (args.is_empty()
                && !world.adts.contains_key(n)
                && !matches!(
                    n.as_str(),
                    "i64"
                        | "u64"
                        | "u32"
                        | "u8"
                        | "bool"
                        | "f64"
                        | "f32"
                        | "Unit"
                        | "Never"
                        | "String"
                ))
                || args.iter().any(|a| mentions_generic(world, a))
        }
        Type::Tuple(ts) => ts.iter().any(|a| mentions_generic(world, a)),
        // Rows carry region capabilities (`RegionBox[eff [r: Region], T]`),
        // not type parameters.
        Type::Row(_) => false,
        Type::Function { .. } | Type::Infer => true,
        Type::IntLiteral(_) => false,
    }
}

fn subst(t: &Type, vars: &Vars) -> Type {
    if vars.is_empty() {
        return t.clone();
    }
    crate::numeric::default_type(&substitute(t, vars))
}

/// `f` with every type substituted by `vars`.
fn instantiate(f: &FunctionIr, name: String, vars: &Vars) -> FunctionIr {
    let mut g = f.clone();
    g.name = name;
    if vars.is_empty() {
        return g;
    }
    for p in &mut g.params {
        p.ty = subst(&p.ty, vars);
    }
    g.result = subst(&g.result, vars);
    for e in &mut g.effects.instances {
        for a in &mut e.args {
            *a = subst(a, vars);
        }
    }
    for t in g.values.values_mut() {
        *t = subst(t, vars);
    }
    for t in g.locals.values_mut() {
        *t = subst(t, vars);
    }
    for b in &mut g.blocks {
        for i in &mut b.instructions {
            i.ty = subst(&i.ty, vars);
            if let Op::Call { types, .. } = &mut i.op {
                for t in types {
                    *t = subst(t, vars);
                }
            }
        }
    }
    g
}

/// The type arguments of the call to `callee` at `span` inside an instance
/// with arguments `outer`.
fn call_vars(
    world: &World,
    caller: &str,
    callee: &str,
    span: &crate::ast::Span,
    explicit: &[Type],
    outer: &Vars,
) -> Result<Vars, VerifiedError> {
    let Some(f) = world.functions.get(callee) else {
        return Ok(Vars::new());
    };
    let mut vars = Vars::new();
    if let Some(found) = world.calls.get(&(span.start, span.end)) {
        for (k, t) in found {
            vars.insert(k.clone(), subst(t, outer));
        }
    }
    for (g, t) in f.generics.iter().zip(explicit) {
        if !g.row && !vars.contains_key(&g.name) {
            vars.insert(g.name.clone(), subst(t, outer));
        }
    }
    let mut out = Vars::new();
    for g in f.generics.iter().filter(|g| !g.row) {
        match vars.get(&g.name) {
            Some(t) if !mentions_generic(world, t) => {
                out.insert(g.name.clone(), crate::numeric::default_type(t));
            }
            other => {
                return Err(VerifiedError::unsupported(
                    Unsupported::Generic,
                    caller,
                    format!(
                        "call {callee} with type parameter {} = {}",
                        g.name,
                        other.map_or_else(|| "?".to_owned(), ToString::to_string)
                    ),
                ));
            }
        }
    }
    if f.generics.iter().any(|g| g.row) {
        return Err(VerifiedError::unsupported(
            Unsupported::Generic,
            caller,
            format!("call {callee} (effect-row parameters)"),
        ));
    }
    Ok(out)
}

/// The nested functions an instruction of `f` refers to by name: the body
/// of a `handle` (region blocks among them).
pub fn nested_bodies(op: &Op) -> Option<String> {
    if let Op::Handle { expr, .. } = op
        && let ExprKind::Handle { body, .. } = &expr.kind
    {
        return Some(nested_name("handler_body", &body.span));
    }
    None
}

/// The monomorphized program reachable from `entry`: one function per
/// instance, named by [`instance_name`]. Functions the entry does not reach
/// are dropped; callees with no body are left for the planner to refuse.
pub fn monomorphize(
    world: &World,
    program: &Program,
    entry: &str,
    max_instances: usize,
) -> Result<Program, VerifiedError> {
    let mut out = Program::default();
    let mut queue = VecDeque::from([(entry.to_owned(), Vars::new())]);
    let mut seen = BTreeSet::new();
    while let Some((base, vars)) = queue.pop_front() {
        let name = instance_name(&base, &vars);
        if !seen.insert(name.clone()) {
            continue;
        }
        if seen.len() > max_instances {
            return Err(VerifiedError::Limit(format!(
                "more than {max_instances} function instances"
            )));
        }
        let Some(f) = program.functions.get(&base) else {
            continue;
        };
        let mut g = instantiate(f, name.clone(), &vars);
        for b in &mut g.blocks {
            for i in &mut b.instructions {
                if let Some(nested) = nested_bodies(&i.op) {
                    queue.push_back((nested, vars.clone()));
                    // The return clauses of other handlers stay for the
                    // handler lowering; they are instantiated alike.
                    if let Op::Handle { expr, .. } = &i.op
                        && let ExprKind::Handle { ret, ops, .. } = &expr.kind
                    {
                        queue.push_back((
                            nested_name("handler_clause", &ret.body.span),
                            vars.clone(),
                        ));
                        for c in ops {
                            queue.push_back((
                                nested_name("handler_clause", &c.body.span),
                                vars.clone(),
                            ));
                        }
                    }
                }
                // A named function read as a value (`run(work)`), for the
                // function-value specialization (`evidence.rs`).
                if let Op::Read { root, path, .. } = &i.op
                    && path.is_empty()
                    && !f.params.iter().any(|p| &p.name == root)
                    && !f.locals.contains_key(root)
                    && world
                        .functions
                        .get(root)
                        .is_some_and(|g| g.generics.is_empty())
                    && program.functions.contains_key(root)
                {
                    queue.push_back((root.clone(), Vars::new()));
                }
                let Op::Call {
                    callee,
                    types,
                    capability,
                    ..
                } = &mut i.op
                else {
                    continue;
                };
                if capability.is_some()
                    || crate::numeric::wrapping_intrinsic(callee).is_some()
                    || !program.functions.contains_key(callee.as_str())
                {
                    continue;
                }
                let cv = call_vars(world, &name, callee, &i.span, types, &vars)?;
                queue.push_back((callee.clone(), cv.clone()));
                *callee = instance_name(callee, &cv);
                types.clear();
            }
        }
        out.functions.insert(name, g);
    }
    Ok(out)
}

/// Nested functions keep their parent's instance suffix: the instance name
/// of the nested function `nested` inside the instance `parent`.
pub fn nested_instance(parent: &str, nested: &str) -> String {
    match parent.find('<') {
        Some(i) if parent.ends_with('>') => format!("{nested}{}", &parent[i..]),
        _ => nested.to_owned(),
    }
}
