//! Functional state passing from the same basic-block IR as both executors.
//! Live slots become block arguments; mutable call results restore only the
//! declared projections, preserving updates to disjoint parts of one root.
use crate::{
    ast::*,
    check::{Place, World},
    ir::{Argument, FunctionIr, Op, Terminator, ValueId},
    lean::{name, ty},
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Local {
    Slot(String),
    Value(ValueId),
}
impl Local {
    fn text(&self) -> String {
        name(&match self {
            Self::Slot(n) => n.clone(),
            Self::Value(v) => format!("$value{v}"),
        })
    }
    fn ty<'a>(&self, f: &'a FunctionIr) -> &'a Type {
        match self {
            Self::Slot(n) => &f.locals[n],
            Self::Value(v) => &f.values[v],
        }
    }
}
fn value(v: ValueId) -> String {
    Local::Value(v).text()
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
pub(crate) fn extract_function(world: &World, f: &FunctionIr) -> Option<Result<String, String>> {
    if f.values.values().any(crate::lean::effectful_arrow)
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
    // A closure whose body needs effect lowering moves its whole enclosing
    // function to the effectful generator instead of failing extraction.
    extract_closures(world, f).transpose()
}
struct Lifted {
    name: String,
    captures: Vec<String>,
    params: Vec<(Pass, Type)>,
}
fn extract_closures(world: &World, f: &FunctionIr) -> Result<Option<String>, String> {
    let mut definitions = String::new();
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
            let Some(body) = extract_function(&extended, &program.functions[&name]) else {
                return Ok(None);
            };
            definitions.push_str(&body?);
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
    definitions.push_str(
        &Emitter {
            world,
            f,
            live: liveness(world, f),
            closures,
        }
        .emit()?,
    );
    Ok(Some(definitions))
}
struct Emitter<'a> {
    world: &'a World,
    f: &'a FunctionIr,
    live: Vec<BTreeSet<Local>>,
    closures: BTreeMap<ValueId, Lifted>,
}
impl Emitter<'_> {
    fn block_name(&self, b: usize) -> String {
        name(&format!("$block_{}_{}", self.f.name, b))
    }
    fn type_params(&self) -> Result<String, String> {
        Ok(self.world.functions[&self.f.name]
            .generics
            .iter()
            .filter(|g| !g.row)
            .map(|g| format!("{{{} : Type}} ", name(&g.name)))
            .collect())
    }
    fn result_type(&self) -> Result<String, String> {
        let mut types = vec![ty(&self.f.result)?];
        types.extend(
            self.f
                .params
                .iter()
                .filter(|p| p.pass == Pass::Inout)
                .map(|p| ty(&p.ty))
                .collect::<Result<Vec<_>, _>>()?,
        );
        Ok(if types.len() == 1 {
            types[0].clone()
        } else {
            format!("({})", types.join(" × "))
        })
    }
    fn call_block(&self, b: usize, replacements: &BTreeMap<ValueId, ValueId>) -> String {
        let args = self.live[b]
            .iter()
            .map(|l| {
                if let Local::Value(v) = l
                    && let Some(arg) = replacements.get(v)
                {
                    return value(*arg);
                }
                l.text()
            })
            .collect::<Vec<_>>()
            .join(" ");
        let gs = self.world.functions[&self.f.name]
            .generics
            .iter()
            .filter(|g| !g.row)
            .map(|g| format!(" ({} := {})", name(&g.name), name(&g.name)))
            .collect::<String>();
        format!("({}{gs} {args})", self.block_name(b))
    }
    fn emit(&self) -> Result<String, String> {
        let mut out = String::new();
        for (b, block) in self.f.blocks.iter().enumerate() {
            out.push_str(&format!(
                "  partial def {} {}",
                self.block_name(b),
                self.type_params()?
            ));
            for l in &self.live[b] {
                out.push_str(&format!("({} : {}) ", l.text(), ty(l.ty(self.f))?));
            }
            out.push_str(&format!(": {} := Id.run do\n", self.result_type()?));
            for (index, i) in block.instructions.iter().enumerate() {
                if index == 0 && arm_bindings(self.f).contains_key(&b) {
                    out.push_str(&format!("    let {} : Unit := ()\n", value(i.dest)));
                } else {
                    self.instruction(i, &mut out)?;
                }
            }
            match &block.terminator {
                Terminator::Return(v) => {
                    let mut results = vec![value(*v)];
                    results.extend(
                        self.f
                            .params
                            .iter()
                            .filter(|p| p.pass == Pass::Inout)
                            .map(|p| name(&p.name)),
                    );
                    out.push_str(&format!(
                        "    pure {}\n",
                        if results.len() == 1 {
                            results[0].clone()
                        } else {
                            format!("({})", results.join(", "))
                        }
                    ));
                }
                Terminator::Jump { block, args } => {
                    let replacements = self.f.blocks[*block]
                        .params
                        .iter()
                        .copied()
                        .zip(args.iter().copied())
                        .collect();
                    out.push_str(&format!(
                        "    pure {}\n",
                        self.call_block(*block, &replacements)
                    ));
                }
                Terminator::Branch { condition, yes, no } => out.push_str(&format!(
                    "    if {} then\n      pure {}\n    else\n      pure {}\n",
                    value(*condition),
                    self.call_block(*yes, &BTreeMap::new()),
                    self.call_block(*no, &BTreeMap::new())
                )),
                Terminator::Match { value: v, arms } => {
                    out.push_str(&format!("    match {} with\n", value(*v)));
                    for (p, target) in arms {
                        out.push_str(&format!(
                            "    | {} => pure {}\n",
                            self.pattern(arm_bindings(self.f).get(target).copied().unwrap_or(p))?,
                            self.call_block(*target, &BTreeMap::new())
                        ));
                    }
                }
                Terminator::Unreachable => {
                    return Err("unreachable control flow has no pure Lean value".into());
                }
            }
            out.push('\n');
        }
        out.push_str(&format!(
            "  partial def {} {}",
            name(&self.f.name),
            self.type_params()?
        ));
        for p in &self.f.params {
            out.push_str(&format!("({} : {}) ", name(&p.name), ty(&p.ty)?));
        }
        out.push_str(&format!(
            ": {} := {}\n\n",
            self.result_type()?,
            self.call_block(0, &BTreeMap::new())
        ));
        Ok(out)
    }
    fn constructor(&self, n: &str) -> Result<String, String> {
        Ok(match n.rsplit("::").next().unwrap_or(n) {
            "Some"
                if self
                    .world
                    .constructor(n)
                    .is_some_and(|(a, _)| a.name == "Option") =>
            {
                "Option.some".into()
            }
            "None"
                if self
                    .world
                    .constructor(n)
                    .is_some_and(|(a, _)| a.name == "Option") =>
            {
                "Option.none".into()
            }
            "Ok" if self
                .world
                .constructor(n)
                .is_some_and(|(a, _)| a.name == "Result") =>
            {
                "Except.ok".into()
            }
            "Err"
                if self
                    .world
                    .constructor(n)
                    .is_some_and(|(a, _)| a.name == "Result") =>
            {
                "Except.error".into()
            }
            _ => {
                let (a, v) = self
                    .world
                    .constructor(n)
                    .ok_or("unknown Lean constructor")?;
                format!(
                    "{}.{}",
                    name(&a.name),
                    if a.is_enum {
                        name(&v.name)
                    } else {
                        "mk".into()
                    }
                )
            }
        })
    }
    fn pattern(&self, p: &Pattern) -> Result<String, String> {
        Ok(match &p.kind {
            PatternKind::Bind(n) => name(n),
            PatternKind::Wild => "_".into(),
            PatternKind::Literal(l) => super::lean::literal(l),
            PatternKind::Tuple(ps) => format!(
                "({})",
                ps.iter()
                    .map(|p| self.pattern(p))
                    .collect::<Result<Vec<_>, _>>()?
                    .join(", ")
            ),
            PatternKind::Constructor {
                name: n, fields, ..
            } => {
                let (_, v) = self.world.constructor(n).ok_or("unknown Lean pattern")?;
                let ps = v
                    .fields
                    .iter()
                    .map(|f| {
                        self.pattern(
                            &fields
                                .iter()
                                .find(|(n, _)| n == &f.name)
                                .ok_or("missing pattern field")?
                                .1,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                format!("({} {})", self.constructor(n)?, ps.join(" "))
            }
        })
    }
    fn instruction(&self, i: &crate::ir::Instruction, out: &mut String) -> Result<(), String> {
        let rhs = match &i.op {
            Op::Constant(l) => format!("({} : {})", super::lean::literal(l), ty(&i.ty)?),
            Op::Unit => "()".into(),
            Op::Read { root, path, .. } => {
                if let Some(f) = self
                    .world
                    .functions
                    .get(root)
                    .filter(|_| !self.f.locals.contains_key(root))
                {
                    if f.params.is_empty() {
                        format!("(fun (_ : Unit) => {})", name(root))
                    } else {
                        name(root)
                    }
                } else {
                    project(name(root), path, self.f.locals.get(root))
                }
            }
            Op::Closure { .. } => {
                let closure = &self.closures[&i.dest];
                let mut args = closure.captures.iter().map(|s| name(s)).collect::<Vec<_>>();
                let params = if closure.params.is_empty() {
                    String::from("(_ : Unit)")
                } else {
                    closure
                        .params
                        .iter()
                        .enumerate()
                        .map(|(n, (_, t))| {
                            ty(t).map(|t| format!("({} : {t})", name(&format!("$arg{n}"))))
                        })
                        .collect::<Result<Vec<_>, _>>()?
                        .join(" ")
                };
                args.extend(
                    closure
                        .params
                        .iter()
                        .enumerate()
                        .map(|(n, _)| name(&format!("$arg{n}"))),
                );
                let gs = self.world.functions[&self.f.name]
                    .generics
                    .iter()
                    .filter(|g| !g.row)
                    .map(|g| format!(" ({} := {})", name(&g.name), name(&g.name)))
                    .collect::<String>();
                format!(
                    "(fun {params} => ({}{gs} {}))",
                    name(&closure.name),
                    args.join(" ")
                )
            }
            Op::Indirect { callee, args } => {
                let values = if args.is_empty() {
                    "()".into()
                } else {
                    args.iter()
                        .map(|a| value(a.value))
                        .collect::<Vec<_>>()
                        .join(" ")
                };
                let call = format!("({} {values})", value(*callee));
                let states = args
                    .iter()
                    .filter(|a| a.pass == Pass::Inout)
                    .collect::<Vec<_>>();
                if states.is_empty() {
                    call
                } else {
                    let names = (0..states.len())
                        .map(|n| name(&format!("$state{}_{}", i.dest, n)))
                        .collect::<Vec<_>>();
                    out.push_str(&format!(
                        "    let ({}, {}) := {call}\n",
                        value(i.dest),
                        names.join(", ")
                    ));
                    for (arg, new) in states.into_iter().zip(names) {
                        let p = arg
                            .place
                            .as_ref()
                            .ok_or("mutable indirect argument requires place")?;
                        out.push_str(&format!(
                            "    let {} := {}\n",
                            name(&p.root),
                            super::lean::update_place(p, &new)
                        ));
                    }
                    return Ok(());
                }
            }
            Op::Unary(op, v) => format!("({op}{})", value(*v)),
            Op::Binary(op, a, b) => {
                // Only proved-in-range checked operations reach pure IR
                // extraction, so they share Lean's modular operators with
                // the wrapping intrinsics.
                let op = crate::numeric::wrapping_intrinsic(op).unwrap_or(op);
                if matches!(op, "<" | "<=" | ">" | ">=") {
                    format!("(decide ({} {op} {}))", value(*a), value(*b))
                } else {
                    let kind = self
                        .f
                        .values
                        .get(a)
                        .and_then(crate::numeric::IntegerKind::from_type);
                    super::lean::arith_term(op, kind, &value(*a), &value(*b))
                }
            }
            Op::Tuple(vs) => format!(
                "({})",
                vs.iter().map(|v| value(*v)).collect::<Vec<_>>().join(", ")
            ),
            Op::Data(n, fs) => {
                let (_, variant) = self.world.constructor(n).ok_or("unknown Lean data")?;
                let values = variant
                    .fields
                    .iter()
                    .map(|f| {
                        fs.iter()
                            .find(|(n, _)| n == &f.name)
                            .map(|(_, v)| value(*v))
                            .ok_or("missing Lean field")
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                format!("({} {})", self.constructor(n)?, values.join(" "))
            }
            Op::Project(v, field) => {
                project(value(*v), std::slice::from_ref(field), self.f.values.get(v))
            }
            Op::Bind { pattern, value: v } => {
                out.push_str(&format!(
                    "    let {} := {}\n",
                    self.pattern(pattern)?,
                    value(*v)
                ));
                "()".into()
            }
            Op::Write {
                place,
                op,
                value: v,
            } => {
                let mut parts = place.split('.');
                let p = Place {
                    root: parts.next().unwrap().into(),
                    path: parts.map(str::to_owned).collect(),
                };
                let new = if op == "=" {
                    value(*v)
                } else {
                    let kind = self
                        .f
                        .values
                        .get(v)
                        .and_then(crate::numeric::IntegerKind::from_type);
                    super::lean::arith_term(
                        &op[..1],
                        kind,
                        &project(name(&p.root), &p.path, self.f.locals.get(&p.root)),
                        &value(*v),
                    )
                };
                out.push_str(&format!(
                    "    let {} := {}\n",
                    name(&p.root),
                    super::lean::update_place(&p, &new)
                ));
                "()".into()
            }
            Op::Call { callee, args, .. } => {
                let function = &self.world.functions[callee];
                let vars = self.world.calls.get(&(i.span.start, i.span.end));
                let gs = function
                    .generics
                    .iter()
                    .filter(|g| !g.row)
                    .filter_map(|g| {
                        vars.and_then(|v| v.get(&g.name))
                            .map(|t| ty(t).map(|t| format!(" ({} := {t})", name(&g.name))))
                    })
                    .collect::<Result<String, _>>()?;
                let call = format!(
                    "({}{gs} {})",
                    name(callee),
                    args.iter()
                        .map(|a| value(a.value))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
                let states = call_states(self.world, callee, args);
                if states.is_empty() {
                    call
                } else {
                    let state_names = (0..states.len())
                        .map(|n| name(&format!("$state{}_{}", i.dest, n)))
                        .collect::<Vec<_>>();
                    out.push_str(&format!(
                        "    let ({}, {}) := {call}\n",
                        value(i.dest),
                        state_names.join(", ")
                    ));
                    for ((param, arg), state) in states.iter().zip(state_names) {
                        let p = arg
                            .place
                            .as_ref()
                            .ok_or("mutable Lean argument requires a place")?;
                        let paths = if param.projections.is_empty() {
                            vec![vec![]]
                        } else {
                            param.projections.clone()
                        };
                        for path in paths {
                            let new = project(state.clone(), &path, Some(&param.ty));
                            let mut place = p.clone();
                            place.path.extend(path);
                            out.push_str(&format!(
                                "    let {} := {}\n",
                                name(&place.root),
                                super::lean::update_place(&place, &new)
                            ));
                        }
                    }
                    return Ok(());
                }
            }
            _ => return Err("unsupported operation in Lean CFG extraction".into()),
        };
        out.push_str(&format!(
            "    let {} : {} := {rhs}\n",
            value(i.dest),
            ty(&i.ty)?
        ));
        Ok(())
    }
}
fn project(mut base: String, path: &[String], ty: Option<&Type>) -> String {
    let mut current = ty;
    for f in path {
        if let Some(Type::Tuple(ts)) = current
            && let Ok(index) = f.parse::<usize>()
        {
            let mut projection = ".2".repeat(index);
            if index + 1 < ts.len() {
                projection.push_str(".1");
            }
            base = format!("({base}){projection}");
            current = ts.get(index);
            continue;
        }
        base = format!("({base}).{}", name(f));
        current = None;
    }
    base
}
