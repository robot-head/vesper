//! Functional state passing from the same basic-block IR as both executors.
//! Live slots become block arguments; mutable call results restore only the
//! declared projections, preserving updates to disjoint parts of one root.
use crate::{
    ast::*,
    check::{Place, World},
    ir::{Argument, FunctionIr, Op, Terminator, ValueId},
    lean_nodes as nodes,
};
use std::collections::{BTreeMap, BTreeSet};
use syntax::{
    Binder, BinderKind, Declaration, Definition, DefinitionKind, DoStatement, LetBinding, Term,
};
use vesper_lean_syntax as syntax;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Local {
    Slot(String),
    Value(ValueId),
}
impl Local {
    fn raw_name(&self) -> String {
        match self {
            Self::Slot(n) => n.clone(),
            Self::Value(v) => format!("$value{v}"),
        }
    }
    fn term(&self) -> Result<Term, String> {
        nodes::source_ref(&self.raw_name())
    }
    fn ty<'a>(&self, f: &'a FunctionIr) -> &'a Type {
        match self {
            Self::Slot(n) => &f.locals[n],
            Self::Value(v) => &f.values[v],
        }
    }
}
fn value(v: ValueId) -> Result<Term, String> {
    Local::Value(v).term()
}
fn slots(p: &Pattern, result: &mut BTreeSet<Local>) {
    match &p.kind {
        PatternKind::Bind(n) => {
            result.insert(Local::Slot(n.clone()));
        }
        PatternKind::Tuple(ps) => {
            for p in ps {
                slots(p, result);
            }
        }
        PatternKind::Constructor { fields, .. } => {
            for (_, p) in fields {
                slots(p, result);
            }
        }
        _ => {}
    }
}
fn call_states<'a>(
    world: &'a World,
    callee: &str,
    args: &'a [Argument],
) -> Vec<(&'a Param, &'a Argument)> {
    world
        .functions
        .get(callee)
        .into_iter()
        .flat_map(|f| f.params.iter().zip(args))
        .filter(|(p, _)| p.pass == Pass::Inout)
        .collect()
}
fn uses_defs(world: &World, op: &Op) -> (BTreeSet<Local>, BTreeSet<Local>) {
    let mut uses = BTreeSet::new();
    let mut defs = BTreeSet::new();
    match op {
        Op::Read { root, .. } => {
            if !world.functions.contains_key(root) {
                uses.insert(Local::Slot(root.clone()));
            }
        }
        Op::Bind { pattern, value } => {
            uses.insert(Local::Value(*value));
            slots(pattern, &mut defs);
        }
        Op::Write { place, op, value } => {
            uses.insert(Local::Value(*value));
            let root = place.split('.').next().unwrap().to_owned();
            if place.contains('.') || op != "=" {
                uses.insert(Local::Slot(root.clone()));
            }
            defs.insert(Local::Slot(root));
        }
        Op::Unary(_, v) | Op::Project(v, _) => {
            uses.insert(Local::Value(*v));
        }
        Op::Binary(_, a, b) | Op::Index(a, b) => {
            uses.extend([Local::Value(*a), Local::Value(*b)]);
        }
        Op::Tuple(vs) => {
            uses.extend(vs.iter().map(|v| Local::Value(*v)));
        }
        Op::Data(_, fs) => {
            uses.extend(fs.iter().map(|(_, v)| Local::Value(*v)));
        }
        Op::Closure { expr, aliases, .. } => {
            if let ExprKind::Closure { params, body, .. } = &expr.kind {
                let mut free = crate::check::free_names(body);
                for p in params {
                    free.remove(&p.name);
                }
                uses.extend(
                    free.into_iter()
                        .filter_map(|n| aliases.get(&n).cloned().map(Local::Slot)),
                );
            }
        }
        Op::Indirect { callee, args } => {
            uses.insert(Local::Value(*callee));
            uses.extend(args.iter().map(|a| Local::Value(a.value)));
            for arg in args.iter().filter(|a| a.pass == Pass::Inout) {
                if let Some(p) = &arg.place {
                    uses.insert(Local::Slot(p.root.clone()));
                    defs.insert(Local::Slot(p.root.clone()));
                }
            }
        }
        Op::Call { callee, args, .. } => {
            uses.extend(args.iter().map(|a| Local::Value(a.value)));
            for (_, arg) in call_states(world, callee, args) {
                if let Some(p) = &arg.place {
                    uses.insert(Local::Slot(p.root.clone()));
                    defs.insert(Local::Slot(p.root.clone()));
                }
            }
        }
        _ => {}
    }
    (uses, defs)
}
fn arm_bindings(f: &FunctionIr) -> BTreeMap<usize, &Pattern> {
    f.blocks
        .iter()
        .filter_map(|b| {
            if let Terminator::Match { arms, .. } = &b.terminator {
                Some(arms)
            } else {
                None
            }
        })
        .flatten()
        .filter_map(|(_, target)| {
            f.blocks[*target].instructions.first().and_then(|i| {
                if let Op::Bind { pattern, .. } = &i.op {
                    Some((*target, pattern))
                } else {
                    None
                }
            })
        })
        .collect()
}
fn liveness(world: &World, f: &FunctionIr) -> Vec<BTreeSet<Local>> {
    let arms = arm_bindings(f);
    let mut live = vec![BTreeSet::new(); f.blocks.len()];
    loop {
        let mut changed = false;
        for (id, block) in f.blocks.iter().enumerate().rev() {
            let mut required = BTreeSet::new();
            match &block.terminator {
                Terminator::Return(v) => {
                    required.insert(Local::Value(*v));
                    required.extend(
                        f.params
                            .iter()
                            .filter(|p| p.pass == Pass::Inout)
                            .map(|p| Local::Slot(p.name.clone())),
                    );
                }
                Terminator::Jump { block, args } => {
                    required.extend(live[*block].clone());
                    for (param, arg) in f.blocks[*block].params.iter().zip(args) {
                        if required.remove(&Local::Value(*param)) {
                            required.insert(Local::Value(*arg));
                        }
                    }
                }
                Terminator::Branch { condition, yes, no } => {
                    required.insert(Local::Value(*condition));
                    required.extend(live[*yes].clone());
                    required.extend(live[*no].clone());
                }
                Terminator::Match { value, arms } => {
                    required.insert(Local::Value(*value));
                    for (_, b) in arms {
                        let mut incoming = live[*b].clone();
                        if let Some(pattern) = arm_bindings(f).get(b) {
                            let mut bindings = BTreeSet::new();
                            slots(pattern, &mut bindings);
                            for binding in bindings {
                                incoming.remove(&binding);
                            }
                        }
                        required.extend(incoming);
                    }
                }
                Terminator::Unreachable => {}
            }
            for (index, i) in block.instructions.iter().enumerate().rev() {
                required.remove(&Local::Value(i.dest));
                if index == 0 && arms.contains_key(&id) {
                    continue;
                }
                let (uses, defs) = uses_defs(world, &i.op);
                for d in defs {
                    required.remove(&d);
                }
                required.extend(uses);
            }
            if live[id] != required {
                live[id] = required;
                changed = true;
            }
        }
        if !changed {
            return live;
        }
    }
}
/// Return structured declarations, leaving rendering to the module boundary.
pub(crate) fn extract_function_nodes(
    world: &World,
    f: &FunctionIr,
) -> Option<Result<Vec<Declaration>, String>> {
    if f.values.values().any(nodes::effectful_arrow)
        || !f.effects.instances.is_empty()
        || f.effects.tail.is_some()
        || f.blocks.iter().flat_map(|b| &b.instructions).any(|i| {
            i.fallible
                || world.partial_ops.contains(&(i.span.start, i.span.end))
                || world.overflow_ops.contains(&(i.span.start, i.span.end))
                || match &i.op {
                    Op::Handle { .. } | Op::Try(_) | Op::Index(..) => true,
                    Op::Call { callee, .. } => world.functions.get(callee).is_none_or(|f| {
                        !f.effects.instances.is_empty() || f.effects.tail.is_some()
                    }),
                    _ => false,
                }
        })
    {
        return None;
    }
    extract_closures(world, f).transpose()
}

struct Lifted {
    name: String,
    captures: Vec<String>,
    params: Vec<(Pass, Type)>,
}
fn extract_closures(world: &World, f: &FunctionIr) -> Result<Option<Vec<Declaration>>, String> {
    let mut definitions = vec![];
    let mut closures = BTreeMap::new();
    for i in f.blocks.iter().flat_map(|b| &b.instructions) {
        if let Op::Closure { expr, aliases, .. } = &i.op {
            let ExprKind::Closure { params, body, .. } = &expr.kind else {
                return Err("invalid closure IR".into());
            };
            let Type::Function {
                params: typed_params,
                result,
                effects,
                ..
            } = &i.ty
            else {
                return Err("closure lacks type".into());
            };
            let mut free = crate::check::free_names(body);
            for p in params {
                free.remove(&p.name);
            }
            let mut captured = vec![];
            let mut captures = vec![];
            for n in free {
                if let Some(slot) = aliases.get(&n) {
                    captured.push(Param {
                        name: n,
                        ty: f.locals[slot].clone(),
                        pass: Pass::Sink,
                        projections: vec![],
                        span: expr.span.clone(),
                    });
                    captures.push(slot.clone());
                }
            }
            let mut actual = params.clone();
            for (p, (pass, ty)) in actual.iter_mut().zip(typed_params) {
                p.pass = *pass;
                p.ty = ty.clone();
            }
            captured.extend(actual);
            let name = format!("$lambda_{}_{}", f.name, i.dest);
            let function = Function {
                name: name.clone(),
                params: captured,
                result: *result.clone(),
                effects: effects.clone(),
                generics: world.functions[&f.name].generics.clone(),
                body: Some(*body.clone()),
                span: expr.span.clone(),
            };
            let mut extended = world.clone();
            extended.functions.insert(name.clone(), function);
            let program = crate::ir::lower_world(&extended);
            let Some(body) = extract_function_nodes(&extended, &program.functions[&name]) else {
                return Ok(None);
            };
            definitions.extend(body?);
            closures.insert(
                i.dest,
                Lifted {
                    name,
                    captures,
                    params: typed_params.clone(),
                },
            );
        }
    }
    definitions.extend(
        Emitter {
            world,
            f,
            live: liveness(world, f),
            closures,
        }
        .emit()?,
    );
    Ok(Some(definitions))
}
fn apply(function: Term, arguments: Vec<syntax::Argument>) -> Term {
    Term::App {
        function: Box::new(function),
        arguments,
    }
}
fn tuple(mut values: Vec<Term>) -> Term {
    if values.len() == 1 {
        values.remove(0)
    } else {
        Term::Tuple(values)
    }
}
fn tuple_pattern(mut values: Vec<syntax::Pattern>) -> syntax::Pattern {
    if values.len() == 1 {
        values.remove(0)
    } else {
        syntax::Pattern::Tuple(values)
    }
}
fn let_value(pattern: syntax::Pattern, ty: Option<Term>, value: Term) -> DoStatement {
    DoStatement::Let {
        binding: LetBinding {
            pattern,
            ty: ty.map(Box::new),
            value,
        },
        mutable: false,
    }
}
fn named_path(parts: &[&str], rooted: bool) -> Result<syntax::Name, String> {
    let name = syntax::Name::qualified(
        parts
            .iter()
            .map(|s| nodes::ident(s))
            .collect::<Result<Vec<_>, _>>()?,
    )
    .map_err(|e| e.to_string())?;
    Ok(if rooted { name.rooted() } else { name })
}
struct Emitter<'a> {
    world: &'a World,
    f: &'a FunctionIr,
    live: Vec<BTreeSet<Local>>,
    closures: BTreeMap<ValueId, Lifted>,
}
impl Emitter<'_> {
    fn block_name(&self, b: usize) -> Result<syntax::Name, String> {
        nodes::source_name(&format!("$block_{}_{}", self.f.name, b))
    }
    fn type_params(&self) -> Result<Vec<Binder>, String> {
        self.world.functions[&self.f.name]
            .generics
            .iter()
            .filter(|g| !g.row)
            .map(|g| {
                Ok(Binder {
                    name: Some(nodes::ident(&g.name)?),
                    ty: Box::new(Term::Sort(syntax::Sort::Type(0))),
                    kind: BinderKind::Implicit,
                })
            })
            .collect()
    }
    fn generic_args(&self) -> Result<Vec<syntax::Argument>, String> {
        self.world.functions[&self.f.name]
            .generics
            .iter()
            .filter(|g| !g.row)
            .map(|g| {
                Ok(syntax::Argument::Named {
                    name: nodes::ident(&g.name)?,
                    value: nodes::source_ref(&g.name)?,
                })
            })
            .collect()
    }
    fn result_type(&self) -> Result<Term, String> {
        let mut types = vec![nodes::ty(&self.f.result)?];
        types.extend(
            self.f
                .params
                .iter()
                .filter(|p| p.pass == Pass::Inout)
                .map(|p| nodes::ty(&p.ty))
                .collect::<Result<Vec<_>, _>>()?,
        );
        Ok(if types.len() == 1 {
            types.remove(0)
        } else {
            Term::ProductType(types)
        })
    }
    fn call_block(
        &self,
        b: usize,
        replacements: &BTreeMap<ValueId, ValueId>,
    ) -> Result<Term, String> {
        let mut args = self.generic_args()?;
        for local in &self.live[b] {
            let term = if let Local::Value(v) = local
                && let Some(arg) = replacements.get(v)
            {
                value(*arg)?
            } else {
                local.term()?
            };
            args.push(syntax::Argument::Positional(term));
        }
        Ok(apply(Term::Ref(self.block_name(b)?), args))
    }
    fn emit(&self) -> Result<Vec<Declaration>, String> {
        let mut out = vec![];
        let arms = arm_bindings(self.f);
        for (b, block) in self.f.blocks.iter().enumerate() {
            let mut binders = self.type_params()?;
            for local in &self.live[b] {
                binders.push(Binder::explicit(
                    nodes::ident(&local.raw_name())?,
                    nodes::ty(local.ty(self.f))?,
                ));
            }
            let mut statements = vec![];
            for (index, i) in block.instructions.iter().enumerate() {
                if index == 0 && arms.contains_key(&b) {
                    statements.push(let_value(
                        syntax::Pattern::Bind(nodes::ident(&Local::Value(i.dest).raw_name())?),
                        Some(nodes::global(&["Unit"])),
                        Term::Tuple(vec![]),
                    ));
                } else {
                    self.instruction(i, &mut statements)?;
                }
            }
            let result = match &block.terminator {
                Terminator::Return(v) => {
                    let mut values = vec![value(*v)?];
                    values.extend(
                        self.f
                            .params
                            .iter()
                            .filter(|p| p.pass == Pass::Inout)
                            .map(|p| nodes::source_ref(&p.name))
                            .collect::<Result<Vec<_>, _>>()?,
                    );
                    Term::app(nodes::global(&["Pure", "pure"]), [tuple(values)])
                }
                Terminator::Jump { block, args } => {
                    let replacements = self.f.blocks[*block]
                        .params
                        .iter()
                        .copied()
                        .zip(args.iter().copied())
                        .collect();
                    Term::app(
                        nodes::global(&["Pure", "pure"]),
                        [self.call_block(*block, &replacements)?],
                    )
                }
                Terminator::Branch { condition, yes, no } => Term::If {
                    condition: Box::new(value(*condition)?),
                    yes: Box::new(Term::app(
                        nodes::global(&["Pure", "pure"]),
                        [self.call_block(*yes, &BTreeMap::new())?],
                    )),
                    no: Box::new(Term::app(
                        nodes::global(&["Pure", "pure"]),
                        [self.call_block(*no, &BTreeMap::new())?],
                    )),
                },
                Terminator::Match {
                    value: v,
                    arms: matches,
                } => Term::Match {
                    scrutinee: Box::new(value(*v)?),
                    arms: matches
                        .iter()
                        .map(|(pattern, target)| {
                            Ok(syntax::MatchArm {
                                pattern: self
                                    .pattern(arms.get(target).copied().unwrap_or(pattern))?,
                                body: Term::app(
                                    nodes::global(&["Pure", "pure"]),
                                    [self.call_block(*target, &BTreeMap::new())?],
                                ),
                            })
                        })
                        .collect::<Result<Vec<_>, String>>()?,
                },
                Terminator::Unreachable => {
                    return Err("unreachable control flow has no pure Lean value".into());
                }
            };
            let body = Term::app(
                nodes::global(&["Id", "run"]),
                [Term::Do {
                    statements,
                    result: Box::new(result),
                }],
            );
            out.push(Declaration::Definition(Definition {
                name: self.block_name(b)?,
                binders,
                result: self.result_type()?,
                body,
                kind: DefinitionKind::PartialDef,
            }));
        }
        let mut binders = self.type_params()?;
        for p in &self.f.params {
            binders.push(Binder::explicit(nodes::ident(&p.name)?, nodes::ty(&p.ty)?));
        }
        out.push(Declaration::Definition(Definition {
            name: nodes::source_name(&self.f.name)?,
            binders,
            result: self.result_type()?,
            body: self.call_block(0, &BTreeMap::new())?,
            kind: DefinitionKind::PartialDef,
        }));
        Ok(out)
    }
    fn constructor(&self, n: &str) -> Result<syntax::Name, String> {
        let (adt, variant) = self
            .world
            .constructor(n)
            .ok_or("unknown Lean constructor")?;
        match (adt.name.as_str(), variant.name.as_str()) {
            ("Option", "Some") => named_path(&["Option", "some"], true),
            ("Option", "None") => named_path(&["Option", "none"], true),
            ("Result", "Ok") => named_path(&["Except", "ok"], true),
            ("Result", "Err") => named_path(&["Except", "error"], true),
            _ => named_path(
                &[&adt.name, if adt.is_enum { &variant.name } else { "mk" }],
                false,
            ),
        }
    }
    fn pattern(&self, p: &Pattern) -> Result<syntax::Pattern, String> {
        Ok(match &p.kind {
            PatternKind::Bind(n) => syntax::Pattern::Bind(nodes::ident(n)?),
            PatternKind::Wild => syntax::Pattern::Wildcard,
            PatternKind::Literal(Literal::Int(n)) => syntax::Pattern::Int(*n),
            PatternKind::Literal(Literal::Bool(b)) => syntax::Pattern::Bool(*b),
            PatternKind::Literal(Literal::String(s)) => syntax::Pattern::String(s.clone()),
            PatternKind::Literal(Literal::Float(_)) => {
                return Err("floating-point patterns are unsupported in Lean extraction".into());
            }
            PatternKind::Tuple(ps) => tuple_pattern(
                ps.iter()
                    .map(|p| self.pattern(p))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            PatternKind::Constructor {
                name: n, fields, ..
            } => {
                let (_, variant) = self.world.constructor(n).ok_or("unknown Lean pattern")?;
                let arguments = variant
                    .fields
                    .iter()
                    .map(|field| {
                        self.pattern(
                            &fields
                                .iter()
                                .find(|(name, _)| name == &field.name)
                                .ok_or("missing pattern field")?
                                .1,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                syntax::Pattern::Constructor {
                    name: self.constructor(n)?,
                    arguments,
                }
            }
        })
    }
    fn instruction(
        &self,
        i: &crate::ir::Instruction,
        out: &mut Vec<DoStatement>,
    ) -> Result<(), String> {
        let rhs = match &i.op {
            Op::Constant(l) => Term::Ascribe {
                value: Box::new(nodes::literal(l)),
                ty: Box::new(nodes::ty(&i.ty)?),
            },
            Op::Unit => Term::Tuple(vec![]),
            Op::Read { root, path, .. } => {
                if let Some(function) = self
                    .world
                    .functions
                    .get(root)
                    .filter(|_| !self.f.locals.contains_key(root))
                {
                    if function.params.is_empty() {
                        Term::Lambda {
                            binders: vec![Binder {
                                name: None,
                                ty: Box::new(nodes::global(&["Unit"])),
                                kind: BinderKind::Explicit,
                            }],
                            body: Box::new(nodes::source_ref(root)?),
                        }
                    } else {
                        nodes::source_ref(root)?
                    }
                } else {
                    project(nodes::source_ref(root)?, path, self.f.locals.get(root))?
                }
            }
            Op::Closure { .. } => {
                let closure = &self.closures[&i.dest];
                let mut arguments = self.generic_args()?;
                arguments.extend(
                    closure
                        .captures
                        .iter()
                        .map(|capture| nodes::source_ref(capture).map(syntax::Argument::Positional))
                        .collect::<Result<Vec<_>, _>>()?,
                );
                let binders = if closure.params.is_empty() {
                    vec![Binder {
                        name: None,
                        ty: Box::new(nodes::global(&["Unit"])),
                        kind: BinderKind::Explicit,
                    }]
                } else {
                    closure
                        .params
                        .iter()
                        .enumerate()
                        .map(|(index, (_, ty))| {
                            Ok(Binder::explicit(
                                nodes::ident(&format!("$arg{index}"))?,
                                nodes::ty(ty)?,
                            ))
                        })
                        .collect::<Result<Vec<_>, String>>()?
                };
                for (index, _) in closure.params.iter().enumerate() {
                    arguments.push(syntax::Argument::Positional(nodes::source_ref(&format!(
                        "$arg{index}"
                    ))?));
                }
                Term::Lambda {
                    binders,
                    body: Box::new(apply(nodes::source_ref(&closure.name)?, arguments)),
                }
            }
            Op::Indirect { callee, args } => {
                let values = if args.is_empty() {
                    vec![Term::Tuple(vec![])]
                } else {
                    args.iter()
                        .map(|arg| value(arg.value))
                        .collect::<Result<Vec<_>, _>>()?
                };
                let call = Term::app(value(*callee)?, values);
                let states = args
                    .iter()
                    .filter(|arg| arg.pass == Pass::Inout)
                    .collect::<Vec<_>>();
                if states.is_empty() {
                    call
                } else {
                    let names = (0..states.len())
                        .map(|index| format!("$state{}_{}", i.dest, index))
                        .collect::<Vec<_>>();
                    let mut ps = vec![syntax::Pattern::Bind(nodes::ident(
                        &Local::Value(i.dest).raw_name(),
                    )?)];
                    ps.extend(
                        names
                            .iter()
                            .map(|name| nodes::ident(name).map(syntax::Pattern::Bind))
                            .collect::<Result<Vec<_>, _>>()?,
                    );
                    out.push(let_value(tuple_pattern(ps), None, call));
                    for (arg, new) in states.into_iter().zip(names) {
                        let place = arg
                            .place
                            .as_ref()
                            .ok_or("mutable indirect argument requires place")?;
                        out.push(let_value(
                            syntax::Pattern::Bind(nodes::ident(&place.root)?),
                            None,
                            nodes::update_place(place, nodes::source_ref(&new)?)?,
                        ));
                    }
                    return Ok(());
                }
            }
            Op::Unary(op, v) => Term::Unary {
                op: match op.as_str() {
                    "-" => syntax::UnaryOp::Neg,
                    "!" => syntax::UnaryOp::BoolNot,
                    _ => return Err("unsupported Lean unary operator".into()),
                },
                value: Box::new(value(*v)?),
            },
            Op::Binary(op, a, b) => {
                let op = crate::numeric::wrapping_intrinsic(op).unwrap_or(op);
                if let Some(op) = match op {
                    "<" => Some(syntax::BinaryOp::Lt),
                    "<=" => Some(syntax::BinaryOp::Le),
                    ">" => Some(syntax::BinaryOp::Gt),
                    ">=" => Some(syntax::BinaryOp::Ge),
                    _ => None,
                } {
                    Term::app(
                        nodes::global(&["Decidable", "decide"]),
                        [Term::binary(op, value(*a)?, value(*b)?)],
                    )
                } else {
                    let kind = self
                        .f
                        .values
                        .get(a)
                        .and_then(crate::numeric::IntegerKind::from_type);
                    nodes::arith_term(op, kind, value(*a)?, value(*b)?)?
                }
            }
            Op::Tuple(vs) => tuple(
                vs.iter()
                    .map(|v| value(*v))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            Op::Data(name, fields) => {
                let (_, variant) = self.world.constructor(name).ok_or("unknown Lean data")?;
                let values = variant
                    .fields
                    .iter()
                    .map(|field| {
                        fields
                            .iter()
                            .find(|(name, _)| name == &field.name)
                            .map(|(_, v)| value(*v))
                            .ok_or("missing Lean field")?
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Term::app(Term::Ref(self.constructor(name)?), values)
            }
            Op::Project(v, field) => project(
                value(*v)?,
                std::slice::from_ref(field),
                self.f.values.get(v),
            )?,
            Op::Bind { pattern, value: v } => {
                out.push(let_value(self.pattern(pattern)?, None, value(*v)?));
                Term::Tuple(vec![])
            }
            Op::Write {
                place,
                op,
                value: v,
            } => {
                let mut parts = place.split('.');
                let place = Place {
                    root: parts.next().ok_or("empty Lean write place")?.into(),
                    path: parts.map(str::to_owned).collect(),
                };
                let new = if op == "=" {
                    value(*v)?
                } else {
                    let kind = self
                        .f
                        .values
                        .get(v)
                        .and_then(crate::numeric::IntegerKind::from_type);
                    nodes::arith_term(
                        op.strip_suffix('=')
                            .ok_or("invalid Lean compound operator")?,
                        kind,
                        project(
                            nodes::source_ref(&place.root)?,
                            &place.path,
                            self.f.locals.get(&place.root),
                        )?,
                        value(*v)?,
                    )?
                };
                out.push(let_value(
                    syntax::Pattern::Bind(nodes::ident(&place.root)?),
                    None,
                    nodes::update_place(&place, new)?,
                ));
                Term::Tuple(vec![])
            }
            Op::Call { callee, args, .. } => {
                let function = &self.world.functions[callee];
                let vars = self.world.calls.get(&(i.span.start, i.span.end));
                let mut arguments = function
                    .generics
                    .iter()
                    .filter(|g| !g.row)
                    .filter_map(|g| {
                        vars.and_then(|v| v.get(&g.name)).map(|ty| {
                            Ok(syntax::Argument::Named {
                                name: nodes::ident(&g.name)?,
                                value: nodes::ty(ty)?,
                            })
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                arguments.extend(
                    args.iter()
                        .map(|arg| value(arg.value).map(syntax::Argument::Positional))
                        .collect::<Result<Vec<_>, _>>()?,
                );
                let call = apply(nodes::source_ref(callee)?, arguments);
                let states = call_states(self.world, callee, args);
                if states.is_empty() {
                    call
                } else {
                    let names = (0..states.len())
                        .map(|index| format!("$state{}_{}", i.dest, index))
                        .collect::<Vec<_>>();
                    let mut ps = vec![syntax::Pattern::Bind(nodes::ident(
                        &Local::Value(i.dest).raw_name(),
                    )?)];
                    ps.extend(
                        names
                            .iter()
                            .map(|name| nodes::ident(name).map(syntax::Pattern::Bind))
                            .collect::<Result<Vec<_>, _>>()?,
                    );
                    out.push(let_value(tuple_pattern(ps), None, call));
                    for ((param, arg), state) in states.iter().zip(names) {
                        let place = arg
                            .place
                            .as_ref()
                            .ok_or("mutable Lean argument requires a place")?;
                        let paths = if param.projections.is_empty() {
                            vec![vec![]]
                        } else {
                            param.projections.clone()
                        };
                        for path in paths {
                            let new = project(nodes::source_ref(&state)?, &path, Some(&param.ty))?;
                            let mut projected = place.clone();
                            projected.path.extend(path);
                            out.push(let_value(
                                syntax::Pattern::Bind(nodes::ident(&projected.root)?),
                                None,
                                nodes::update_place(&projected, new)?,
                            ));
                        }
                    }
                    return Ok(());
                }
            }
            _ => return Err("unsupported operation in Lean CFG extraction".into()),
        };
        out.push(let_value(
            syntax::Pattern::Bind(nodes::ident(&Local::Value(i.dest).raw_name())?),
            Some(nodes::ty(&i.ty)?),
            rhs,
        ));
        Ok(())
    }
}
fn project(mut base: Term, path: &[String], ty: Option<&Type>) -> Result<Term, String> {
    let mut current = ty;
    for field in path {
        if let Some(Type::Tuple(types)) = current
            && let Ok(index) = field.parse::<usize>()
        {
            if index >= types.len() {
                return Err("tuple projection is outside Lean tuple type".into());
            }
            for _ in 0..index {
                base = Term::Project {
                    base: Box::new(base),
                    projection: syntax::Projection::Tuple(std::num::NonZeroU32::new(2).unwrap()),
                };
            }
            if index + 1 < types.len() {
                base = Term::Project {
                    base: Box::new(base),
                    projection: syntax::Projection::Tuple(std::num::NonZeroU32::new(1).unwrap()),
                };
            }
            current = types.get(index);
        } else {
            base = Term::Project {
                base: Box::new(base),
                projection: syntax::Projection::Field(nodes::ident(field)?),
            };
            current = None;
        }
    }
    Ok(base)
}
