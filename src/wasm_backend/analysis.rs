//! Reachability from the entry and the static plan for every lexical handler:
//! which nested IR functions implement its body and clauses, what each clause
//! captures, and how each operation clause is lowered.
use super::{HandlerMode, Unsupported, WasmError};
use crate::{
    ast::*,
    check::World,
    ir::{FunctionIr, Op, Program, Terminator, ValueId, nested_name},
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// How a handler's nested function is compiled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    Ordinary,
    /// Every parameter is a captured place, passed by cell so that the
    /// body's writes reach the enclosing function (as in the evaluator).
    HandlerBody,
    /// A `return` clause.
    ReturnClause,
    /// An operation clause of `handler`, operation `op` (declaration index).
    OpClause {
        handler: usize,
        op: usize,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct OpClause {
    pub op: usize,
    pub function: String,
    pub mode: HandlerMode,
    pub arg_patterns: Vec<Pattern>,
    pub arg_types: Vec<Type>,
    pub state_pattern: Option<Pattern>,
    pub resume: Option<String>,
    pub captures: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct Handler {
    /// Function containing the `handle` expression and its instruction.
    pub site: (String, ValueId),
    pub effect: Effect,
    pub keys: Vec<String>,
    pub state: Option<Type>,
    pub body: String,
    pub body_type: Type,
    pub ret: String,
    pub ret_pattern: Pattern,
    pub ret_captures: Vec<String>,
    pub ops: Vec<OpClause>,
    /// Some clause needs a captured continuation (WasmFX).
    pub stack_switching: bool,
    pub aliases: BTreeMap<String, String>,
}

#[derive(Default)]
pub(crate) struct Plan {
    /// Reachable IR functions in a deterministic order.
    pub functions: Vec<String>,
    pub roles: BTreeMap<String, Role>,
    pub handlers: Vec<Handler>,
    pub handler_at: BTreeMap<(String, ValueId), usize>,
    /// Handler labels lexically visible in each function.
    pub labels: BTreeMap<String, BTreeSet<String>>,
    /// Every dynamic evidence key some handler installs.
    pub keys: BTreeSet<String>,
    /// Largest operation arity among the program's effects.
    pub max_arity: usize,
}

pub(crate) fn handler_keys(effect: &Effect) -> Vec<String> {
    let mut keys = vec![format!("$effect:{}", effect.name)];
    if let Some(k) = crate::numeric::instance_key(&effect.name, &effect.args) {
        keys.push(k);
    }
    if let Some(l) = &effect.label {
        keys.push(format!("$label:{l}"));
    }
    keys
}

/// Free names of `names` that are visible locals of the enclosing function,
/// in the order `ir::Lower::capture_params` produces them.
fn captures(
    names: BTreeSet<String>,
    aliases: &BTreeMap<String, String>,
    outer: &FunctionIr,
) -> Vec<String> {
    names
        .into_iter()
        .filter(|n| aliases.get(n).is_some_and(|s| outer.locals.contains_key(s)))
        .collect()
}

pub(crate) fn plan(
    world: &World,
    program: &Program,
    entry: &str,
    stack_switching: bool,
    max_functions: usize,
) -> Result<Plan, WasmError> {
    let mut p = Plan {
        max_arity: world
            .effects
            .values()
            .flat_map(|e| e.ops.iter().map(|o| o.params.len()))
            .max()
            .unwrap_or(0)
            .max(1),
        ..Plan::default()
    };
    let mut queue = VecDeque::from([(entry.to_owned(), Role::Ordinary, BTreeSet::new())]);
    let mut seen = BTreeSet::new();
    while let Some((name, role, labels)) = queue.pop_front() {
        if !seen.insert(name.clone()) {
            continue;
        }
        if seen.len() > max_functions {
            return Err(WasmError::Limit(format!(
                "more than {max_functions} reachable functions"
            )));
        }
        let f = program.functions.get(&name).ok_or_else(|| {
            WasmError::unsupported(
                Unsupported::ExternalFunction,
                &name,
                "function has no lowered body",
            )
        })?;
        p.functions.push(name.clone());
        p.roles.insert(name.clone(), role);
        p.labels.insert(name.clone(), labels.clone());
        for block in &f.blocks {
            for inst in &block.instructions {
                match &inst.op {
                    Op::Call { callee, .. } if program.functions.contains_key(callee) => {
                        queue.push_back((callee.clone(), Role::Ordinary, BTreeSet::new()));
                    }
                    Op::Handle {
                        expr,
                        aliases,
                        state_value,
                        ..
                    } => {
                        let id = p.handlers.len();
                        let h = handler(
                            world,
                            program,
                            f,
                            &name,
                            inst.dest,
                            expr,
                            aliases,
                            *state_value,
                            stack_switching,
                        )?;
                        let mut inner = labels.clone();
                        if let Some(l) = &h.effect.label {
                            inner.insert(l.clone());
                        }
                        queue.push_back((h.body.clone(), Role::HandlerBody, inner));
                        queue.push_back((h.ret.clone(), Role::ReturnClause, labels.clone()));
                        for c in &h.ops {
                            queue.push_back((
                                c.function.clone(),
                                Role::OpClause {
                                    handler: id,
                                    op: c.op,
                                },
                                labels.clone(),
                            ));
                        }
                        p.keys.extend(h.keys.iter().cloned());
                        p.handler_at.insert((name.clone(), inst.dest), id);
                        p.handlers.push(h);
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(p)
}

#[allow(clippy::too_many_arguments)]
fn handler(
    world: &World,
    program: &Program,
    outer: &FunctionIr,
    site: &str,
    dest: ValueId,
    expr: &Expr,
    aliases: &BTreeMap<String, String>,
    state_value: Option<ValueId>,
    stack_switching: bool,
) -> Result<Handler, WasmError> {
    let ExprKind::Handle {
        effect,
        state,
        body,
        ret,
        ops,
    } = &expr.kind
    else {
        return Err(WasmError::Invalid(
            "handle instruction without a handler".into(),
        ));
    };
    let generic_args = effect.args.iter().any(|t| mentions_generic(world, t));
    if generic_args {
        return Err(WasmError::unsupported(
            Unsupported::GenericEffectInstance,
            site,
            format!("handler for {} with generic arguments", effect.name),
        ));
    }
    let decl = world
        .effects
        .get(&effect.name)
        .ok_or_else(|| WasmError::Invalid(format!("unknown effect '{}'", effect.name)))?;
    let vars: BTreeMap<String, Type> = decl
        .generics
        .iter()
        .zip(&effect.args)
        .map(|(g, t)| (g.name.clone(), t.clone()))
        .collect();
    let body_fn = nested_name("handler_body", &body.span);
    let ret_fn = nested_name("handler_clause", &ret.body.span);
    for f in [&body_fn, &ret_fn] {
        if !program.functions.contains_key(f) {
            return Err(WasmError::Invalid(format!(
                "handler function {f} was not lowered"
            )));
        }
    }
    let state_type = match (state, state_value) {
        (Some(_), Some(v)) => Some(
            outer
                .values
                .get(&v)
                .cloned()
                .ok_or_else(|| WasmError::Invalid("handler state has no type".into()))?,
        ),
        (None, _) => None,
        (Some(_), None) => return Err(WasmError::Invalid("handler state was not lowered".into())),
    };
    let ret_captures = captures(crate::check::clause_free_names(ret), aliases, outer);
    check_prefix(program, &ret_fn, &ret_captures)?;
    let mut clauses = vec![];
    for clause in ops {
        let Some(op) = decl.ops.iter().position(|o| o.name == clause.name) else {
            return Err(WasmError::Invalid(format!(
                "clause '{}' is not an operation of {}",
                clause.name, effect.name
            )));
        };
        let sig = &decl.ops[op];
        if sig.params.iter().any(|p| p.pass == Pass::Inout) {
            return Err(WasmError::unsupported(
                Unsupported::InoutEffectArgument,
                site,
                format!("{}::{} takes an inout argument", effect.name, sig.name),
            ));
        }
        let function = nested_name("handler_clause", &clause.body.span);
        let ir = program.functions.get(&function).ok_or_else(|| {
            WasmError::Invalid(format!("handler clause {function} was not lowered"))
        })?;
        let n = sig.params.len();
        let state_pattern = state.as_ref().and_then(|_| clause.pats.get(n).cloned());
        let k_index = n + usize::from(state.is_some());
        let resume = match clause.pats.get(k_index).map(|p| &p.kind) {
            Some(PatternKind::Bind(name)) => Some(name.clone()),
            _ => None,
        };
        let caps = captures(crate::check::clause_free_names(clause), aliases, outer);
        check_prefix(program, &function, &caps)?;
        let mode = clause_mode(ir, resume.as_deref(), &function)?;
        if mode == HandlerMode::StackSwitching && !stack_switching {
            return Err(WasmError::unsupported(
                Unsupported::NonTailResumptiveHandler,
                site,
                format!(
                    "clause '{}' of {} uses its continuation before returning; portable lowering supports aborting and tail-resumptive clauses only",
                    clause.name, effect.name
                ),
            ));
        }
        clauses.push(OpClause {
            op,
            function,
            mode,
            arg_patterns: clause.pats.iter().take(n).cloned().collect(),
            arg_types: sig
                .params
                .iter()
                .map(|p| substitute(&p.ty, &vars))
                .collect(),
            state_pattern,
            resume,
            captures: caps,
        });
    }
    Ok(Handler {
        site: (site.to_owned(), dest),
        keys: handler_keys(effect),
        effect: effect.clone(),
        state: state_type,
        body: body_fn,
        body_type: program.functions[&nested_name("handler_body", &body.span)]
            .result
            .clone(),
        ret: ret_fn,
        ret_pattern: ret
            .pats
            .first()
            .cloned()
            .ok_or_else(|| WasmError::Invalid("return clause without a pattern".into()))?,
        ret_captures,
        stack_switching: clauses
            .iter()
            .any(|c| c.mode == HandlerMode::StackSwitching),
        ops: clauses,
        aliases: aliases.clone(),
    })
}

fn check_prefix(program: &Program, function: &str, captures: &[String]) -> Result<(), WasmError> {
    let f = &program.functions[function];
    let names: Vec<_> = f
        .params
        .iter()
        .take(captures.len())
        .map(|p| &p.name)
        .collect();
    if names.len() != captures.len() || names.iter().zip(captures).any(|(a, b)| *a != b) {
        return Err(WasmError::Invalid(format!(
            "handler function {function} does not start with its captures {captures:?}"
        )));
    }
    Ok(())
}

fn mentions_generic(world: &World, t: &Type) -> bool {
    match t {
        Type::Named(n, args) => {
            (args.is_empty()
                && !world.adts.contains_key(n)
                && !matches!(
                    n.as_str(),
                    "i64" | "u64" | "u32" | "u8" | "bool" | "f64" | "Unit" | "Never" | "String"
                ))
                || args.iter().any(|a| mentions_generic(world, a))
        }
        Type::Tuple(ts) => ts.iter().any(|a| mentions_generic(world, a)),
        Type::Function { .. } | Type::Infer | Type::Row(_) => true,
        Type::IntLiteral(_) => false,
    }
}

/// Classify an operation clause by how it uses its continuation `resume`.
fn clause_mode(f: &FunctionIr, resume: Option<&str>, name: &str) -> Result<HandlerMode, WasmError> {
    let Some(resume) = resume else {
        return Ok(HandlerMode::Abort);
    };
    let mut reads = BTreeSet::new();
    let mut calls = BTreeSet::new();
    for b in &f.blocks {
        for i in &b.instructions {
            if let Op::Read { root, path, .. } = &i.op
                && root == resume
            {
                if !path.is_empty() {
                    return Err(first_class(name));
                }
                reads.insert(i.dest);
            }
        }
    }
    // Every read of the continuation must be the callee of an `Indirect`.
    for b in &f.blocks {
        for i in &b.instructions {
            let used = operands(&i.op);
            match &i.op {
                Op::Indirect { callee, args } if reads.contains(callee) => {
                    if args.iter().any(|a| reads.contains(&a.value)) {
                        return Err(first_class(name));
                    }
                    calls.insert(i.dest);
                }
                _ => {
                    if used.iter().any(|v| reads.contains(v)) {
                        return Err(first_class(name));
                    }
                }
            }
        }
        if terminator_operands(&b.terminator)
            .iter()
            .any(|v| reads.contains(v))
        {
            return Err(first_class(name));
        }
    }
    if calls.is_empty() {
        return Ok(HandlerMode::Abort);
    }
    // Tail-resumptive: each resumption result flows unchanged to a return,
    // and every return returns a resumption result.
    let mut tail = BTreeSet::new();
    for (bi, b) in f.blocks.iter().enumerate() {
        if let Some(last) = b.instructions.last()
            && calls.contains(&last.dest)
            && flows_to_return(f, bi, last.dest, 0)
        {
            tail.insert(last.dest);
        }
    }
    let used_elsewhere = f.blocks.iter().any(|b| {
        b.instructions
            .iter()
            .any(|i| operands(&i.op).iter().any(|v| calls.contains(v)))
    });
    let returns_resumed = f
        .blocks
        .iter()
        .enumerate()
        .all(|(bi, b)| match &b.terminator {
            Terminator::Return(v) => resumed_value(f, bi, *v, &tail, 0),
            _ => true,
        });
    if tail.len() == calls.len() && !used_elsewhere && returns_resumed {
        Ok(HandlerMode::Tail)
    } else {
        Ok(HandlerMode::StackSwitching)
    }
}

fn first_class(name: &str) -> WasmError {
    WasmError::unsupported(
        Unsupported::FirstClassContinuation,
        name,
        "the continuation may only be called directly",
    )
}

/// Whether `value`, defined last in block `b`, reaches a `Return` unchanged.
pub(crate) fn flows_to_return(f: &FunctionIr, b: usize, value: ValueId, depth: usize) -> bool {
    if depth > 64 {
        return false;
    }
    match &f.blocks[b].terminator {
        Terminator::Return(v) => *v == value,
        Terminator::Jump { block, args } => {
            let target = &f.blocks[*block];
            let Some(pos) = args.iter().position(|a| *a == value) else {
                return false;
            };
            if args.iter().filter(|a| **a == value).count() != 1 || !target.instructions.is_empty()
            {
                return false;
            }
            flows_to_return(f, *block, target.params[pos], depth + 1)
        }
        _ => false,
    }
}

/// Whether `value` returned from block `b` is (through join parameters fed
/// only by them) a tail resumption result.
fn resumed_value(
    f: &FunctionIr,
    b: usize,
    value: ValueId,
    tail: &BTreeSet<ValueId>,
    depth: usize,
) -> bool {
    if tail.contains(&value) {
        return true;
    }
    if depth > 64 {
        return false;
    }
    let Some(pos) = f.blocks[b].params.iter().position(|p| *p == value) else {
        return false;
    };
    let mut any = false;
    for (pi, pred) in f.blocks.iter().enumerate() {
        if let Terminator::Jump { block, args } = &pred.terminator
            && *block == b
        {
            any = true;
            if !resumed_value(f, pi, args[pos], tail, depth + 1) {
                return false;
            }
        }
        if matches!(&pred.terminator, Terminator::Branch { yes, no, .. } if *yes == b || *no == b)
            || matches!(&pred.terminator, Terminator::Match { arms, .. } if arms.iter().any(|(_, t)| *t == b))
        {
            return false;
        }
    }
    any
}

/// Values an operation reads.
pub(crate) fn operands(op: &Op) -> Vec<ValueId> {
    match op {
        Op::Constant(_) | Op::Unit | Op::Read { .. } | Op::Closure { .. } => vec![],
        Op::Bind { value, .. } | Op::Write { value, .. } | Op::Unary(_, value) => vec![*value],
        Op::Binary(_, a, b) | Op::Index(a, b) => vec![*a, *b],
        Op::Tuple(vs) => vs.clone(),
        Op::Data(_, fs) => fs.iter().map(|(_, v)| *v).collect(),
        Op::Project(v, _) | Op::Try(v) => vec![*v],
        Op::Call { args, .. } => args.iter().map(|a| a.value).collect(),
        Op::Indirect { callee, args } => {
            let mut v = vec![*callee];
            v.extend(args.iter().map(|a| a.value));
            v
        }
        Op::Handle { state_value, .. } => state_value.iter().copied().collect(),
    }
}

fn terminator_operands(t: &Terminator) -> Vec<ValueId> {
    match t {
        Terminator::Return(v) => vec![*v],
        Terminator::Jump { args, .. } => args.clone(),
        Terminator::Branch { condition, .. } => vec![*condition],
        Terminator::Match { value, .. } => vec![*value],
        Terminator::Unreachable => vec![],
    }
}
