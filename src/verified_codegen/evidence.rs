//! Milestone 11: tail-resumptive handlers of user effects, and calls that
//! pass a named function as a function value, rewritten into first-order
//! Place IR before planning.
//!
//! **Function-value arguments.** A call `g(f)` whose argument is a named
//! function (not a local) gets its own instance `g<p=f>` of `g`, with the
//! parameter `p` removed and every call `p(..)` turned into the direct call
//! `f(..)`, through the nested functions of `g` that capture `p`. A
//! parameter used other than by calling it is left alone (and refused by
//! the planner as a function value).
//!
//! **Handlers.** `handle E in body with { return v => r, op(x, k) => c }`
//! for a user effect `E` (not `Fail`, not a region block) is compiled when
//! every clause is *tail-resumptive*: its continuation `k` is used once,
//! as `k(e)` in tail position. Then performing `op` is a call to the
//! clause with the continuation erased (it returns `e`), and the handler is
//! passed as *evidence*:
//! - The clauses' captures become hidden parameters (`$ev<n>.<name>`) of
//!   the handler body and of an instance `f<E#n>` of every function with
//!   `E` in its row reached from the body, appended after the declared
//!   parameters. `E` leaves their rows; the region capabilities the clauses
//!   use join them.
//! - `E::op(a)` becomes `clause(evidence.., a)`.
//! - The `handle` instruction becomes a call of the body and then of the
//!   `return` clause.
//!
//! A clause runs in the handler's context, so a failure it raises must not
//! be caught by a `handle Fail` inside the body. The planner gives such
//! failures site-private *escape* codes on the way out of the clause call
//! and restores them after the body call ([`Sites`]).
use super::mono::nested_instance;
use super::{Unsupported, VerifiedError};
use crate::ast::{Effect, ExprKind, Pass, PatternKind, Type};
use crate::check::{Place, World};
use crate::ir::{Argument, FunctionIr, Instruction, Op, Program, Terminator, ValueId, nested_name};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Failure-code rewriting the planner applies around calls (see the module
/// documentation): by callee, the handler site whose escape codes it uses.
#[derive(Clone, Debug, Default)]
pub struct Sites {
    /// Clause functions: their failures leave as the site's escape codes.
    pub escape: BTreeMap<String, String>,
    /// Handler bodies: the site's escape codes are restored after them.
    pub restore: BTreeMap<String, String>,
}
impl Sites {
    pub fn tags(&self) -> BTreeSet<&String> {
        self.escape.values().chain(self.restore.values()).collect()
    }
}

/// Whether `e` is a user effect: not `Fail`, not the region effect.
pub fn is_user_effect(e: &Effect) -> bool {
    e.name != "Fail" && e.name != crate::regions::REGION_EFFECT
}

/// `name` with `item` added to its instance suffix.
fn with_suffix(name: &str, item: &str) -> String {
    match name.find('<') {
        Some(_) if name.ends_with('>') => format!("{},{item}>", &name[..name.len() - 1]),
        _ => format!("{name}<{item}>"),
    }
}

/// The value ids an operation reads.
fn op_uses(op: &Op) -> Vec<ValueId> {
    let args = |a: &[Argument]| a.iter().map(|a| a.value).collect::<Vec<_>>();
    match op {
        Op::Constant(_) | Op::Unit | Op::Read { .. } | Op::Closure { .. } => vec![],
        Op::Bind { value, .. } | Op::Write { value, .. } => vec![*value],
        Op::Unary(_, v) | Op::Project(v, _) | Op::Try(v) => vec![*v],
        Op::Binary(_, a, b) | Op::Index(a, b) => vec![*a, *b],
        Op::Tuple(vs) => vs.clone(),
        Op::Vector { args, .. } => args.clone(),
        Op::Data(_, fs) => fs.iter().map(|(_, v)| *v).collect(),
        Op::Call { args: a, .. } | Op::Tiled { args: a, .. } | Op::Fip { args: a, .. } => args(a),
        Op::Indirect { callee, args: a } => {
            let mut out = vec![*callee];
            out.extend(args(a));
            out
        }
        Op::Handle { state_value, .. } => state_value.iter().copied().collect(),
    }
}

fn terminator_uses(t: &Terminator) -> Vec<ValueId> {
    match t {
        Terminator::Return(v) => vec![*v],
        Terminator::Jump { args, .. } => args.clone(),
        Terminator::Branch { condition, .. } => vec![*condition],
        Terminator::Match { value, .. } => vec![*value],
        Terminator::Unreachable => vec![],
    }
}

/// How often each value is read in `f`.
fn use_counts(f: &FunctionIr) -> BTreeMap<ValueId, usize> {
    let mut out = BTreeMap::new();
    for b in &f.blocks {
        for v in b
            .instructions
            .iter()
            .flat_map(|i| op_uses(&i.op))
            .chain(terminator_uses(&b.terminator))
        {
            *out.entry(v).or_default() += 1;
        }
    }
    out
}

fn next_value(f: &FunctionIr) -> ValueId {
    f.blocks
        .iter()
        .flat_map(|b| {
            b.instructions
                .iter()
                .map(|i| i.dest)
                .chain(b.params.clone())
        })
        .chain(f.values.keys().copied())
        .max()
        .map_or(0, |v| v + 1)
}

/// The nested functions a `handle` instruction of `parent` refers to:
/// body, `return` clause and operation clauses (by operation name).
fn nested_of(parent: &str, op: &Op) -> Vec<String> {
    let Op::Handle { expr, .. } = op else {
        return vec![];
    };
    let ExprKind::Handle { body, ret, ops, .. } = &expr.kind else {
        return vec![];
    };
    let mut out = vec![
        nested_instance(parent, &nested_name("handler_body", &body.span)),
        nested_instance(parent, &nested_name("handler_clause", &ret.body.span)),
    ];
    out.extend(
        ops.iter()
            .map(|c| nested_instance(parent, &nested_name("handler_clause", &c.body.span))),
    );
    out
}

/// A read of a local or capture, as an argument for a parameter `p`.
fn read_argument(
    f: &mut FunctionIr,
    out: &mut Vec<Instruction>,
    next: &mut ValueId,
    root: &str,
    pass: Pass,
    ty: &Type,
    span: &crate::ast::Span,
) -> Argument {
    let dest = *next;
    *next += 1;
    out.push(Instruction {
        dest,
        op: Op::Read {
            root: root.to_owned(),
            path: vec![],
            pass,
            projections: vec![],
        },
        ty: ty.clone(),
        span: span.clone(),
        token: None,
        fallible: false,
    });
    f.values.insert(dest, ty.clone());
    Argument {
        pass,
        value: dest,
        place: (pass == Pass::Inout).then(|| Place {
            root: root.to_owned(),
            path: vec![],
        }),
    }
}

/// Rewrite `program` (already monomorphized). With `refused`, a handler
/// that cannot be rewritten is recorded under its function and left for
/// the planner; otherwise it is an error.
pub fn rewrite(
    world: &World,
    program: &mut Program,
    entries: &[String],
    mut refused: Option<&mut BTreeMap<String, Vec<VerifiedError>>>,
) -> Result<Sites, VerifiedError> {
    specialize_function_arguments(program);
    // The instances the specialization replaced are no longer reached.
    prune(program, entries);
    let mut sites = Sites::default();
    let regions = region_labels(world, program);
    let mut found = vec![];
    for (name, f) in &program.functions {
        for (b, block) in f.blocks.iter().enumerate() {
            for (k, i) in block.instructions.iter().enumerate() {
                if let Op::Handle { expr, .. } = &i.op
                    && let ExprKind::Handle { effect, .. } = &expr.kind
                    && is_user_effect(effect)
                    && super::lower::region_block(world, &i.op, name).is_none()
                {
                    found.push((name.clone(), b, k));
                }
            }
        }
    }
    // Rewriting a site moves the instructions after it; go backwards.
    found.reverse();
    for (n, (name, b, k)) in found.into_iter().enumerate() {
        let tag = {
            let Op::Handle { expr, .. } = &program.functions[&name].blocks[b].instructions[k].op
            else {
                unreachable!()
            };
            let ExprKind::Handle { effect, .. } = &expr.kind else {
                unreachable!()
            };
            format!("{}#{n}", effect.name)
        };
        let mut copy = program.clone();
        match site(&mut copy, &regions, &name, b, k, &tag, &mut sites) {
            Ok(()) => *program = copy,
            Err(e) => match refused.as_deref_mut() {
                Some(r) => r.entry(name).or_default().push(e),
                None => return Err(e),
            },
        }
    }
    Ok(sites)
}

/// Drop the functions `entries` do not reach by calls and handlers.
fn prune(program: &mut Program, entries: &[String]) {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<String> = entries.iter().cloned().collect();
    while let Some(name) = queue.pop_front() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let Some(f) = program.functions.get(&name) else {
            continue;
        };
        for i in f.blocks.iter().flat_map(|b| &b.instructions) {
            match &i.op {
                Op::Call { callee, .. } => queue.push_back(callee.clone()),
                Op::Handle { .. } => queue.extend(nested_of(&name, &i.op)),
                _ => {}
            }
        }
    }
    program.functions.retain(|n, _| seen.contains(n));
}

/// The source label of each region block, by the checker's internal
/// capability label (`$cap<start>`) for it.
fn region_labels(world: &World, program: &Program) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (name, f) in &program.functions {
        for i in f.blocks.iter().flat_map(|b| &b.instructions) {
            if let Some((label, _, _)) = super::lower::region_block(world, &i.op, name)
                && let Op::Handle { expr, .. } = &i.op
            {
                out.insert(format!("$cap{}", expr.span.start), label);
            }
        }
    }
    out
}

/// Give rows the source labels of their region capabilities.
fn relabel_regions(f: &mut FunctionIr, regions: &BTreeMap<String, String>) {
    for e in &mut f.effects.instances {
        if e.name == crate::regions::REGION_EFFECT
            && let Some(l) = &e.label
            && let Some(source) = regions.get(l)
        {
            e.label = Some(source.clone());
        }
    }
    let mut seen = BTreeSet::new();
    f.effects
        .instances
        .retain(|e| e.name != crate::regions::REGION_EFFECT || seen.insert(e.label.clone()));
}

// --- Function-value arguments ---

fn specialize_function_arguments(program: &mut Program) {
    let mut queue: VecDeque<String> = program.functions.keys().cloned().collect();
    let mut rounds = 0usize;
    while let Some(name) = queue.pop_front() {
        rounds += 1;
        if rounds > 100_000 {
            return;
        }
        let Some(f) = program.functions.get(&name) else {
            continue;
        };
        let reads: BTreeMap<ValueId, String> = f
            .blocks
            .iter()
            .flat_map(|b| &b.instructions)
            .filter_map(|i| match &i.op {
                Op::Read { root, path, .. }
                    if path.is_empty()
                        && !f.params.iter().any(|p| &p.name == root)
                        && !f.locals.contains_key(root)
                        && program.functions.contains_key(root) =>
                {
                    Some((i.dest, root.clone()))
                }
                _ => None,
            })
            .collect();
        if reads.is_empty() {
            continue;
        }
        let uses = use_counts(f);
        let mut edits = vec![];
        for (b, block) in f.blocks.iter().enumerate() {
            for (k, i) in block.instructions.iter().enumerate() {
                let Op::Call {
                    callee,
                    args,
                    capability: None,
                    ..
                } = &i.op
                else {
                    continue;
                };
                let Some(g) = program.functions.get(callee) else {
                    continue;
                };
                if g.params.len() != args.len() {
                    continue;
                }
                let subst: Vec<(usize, String, String)> = args
                    .iter()
                    .enumerate()
                    .filter_map(|(j, a)| {
                        let target = reads.get(&a.value)?;
                        (matches!(g.params[j].ty, Type::Function { .. })
                            && uses.get(&a.value) == Some(&1))
                        .then(|| (j, g.params[j].name.clone(), target.clone()))
                    })
                    .collect();
                if !subst.is_empty() {
                    edits.push((b, k, callee.clone(), subst));
                }
            }
        }
        for (b, k, callee, subst) in edits {
            let map: BTreeMap<String, String> = subst
                .iter()
                .map(|(_, p, t)| (p.clone(), t.clone()))
                .collect();
            let item = map
                .iter()
                .map(|(p, t)| format!("{p}={t}"))
                .collect::<Vec<_>>()
                .join(",");
            let new = with_suffix(&callee, &item);
            if !program.functions.contains_key(&new) {
                let mut made = BTreeMap::new();
                if !substitute_tree(program, &callee, &new, &map, &mut made) {
                    continue;
                }
                for (n, g) in made {
                    queue.push_back(n.clone());
                    program.functions.insert(n, g);
                }
            }
            let f = program.functions.get_mut(&name).expect("caller");
            let removed: BTreeSet<usize> = subst.iter().map(|(j, _, _)| *j).collect();
            let mut dead = vec![];
            if let Op::Call {
                callee: c, args, ..
            } = &mut f.blocks[b].instructions[k].op
            {
                *c = new;
                let mut j = 0;
                args.retain(|a| {
                    let keep = !removed.contains(&j);
                    if !keep {
                        dead.push(a.value);
                    }
                    j += 1;
                    keep
                });
            }
            for block in &mut f.blocks {
                block.instructions.retain(|i| !dead.contains(&i.dest));
            }
            queue.push_back(name.clone());
        }
    }
}

/// Copy `old` (and its nested functions) as `new` with the function-typed
/// locals of `map` replaced by direct calls of the named functions.
/// `false` when one is used other than by calling it.
fn substitute_tree(
    program: &Program,
    old: &str,
    new: &str,
    map: &BTreeMap<String, String>,
    made: &mut BTreeMap<String, FunctionIr>,
) -> bool {
    let Some(f) = program.functions.get(old) else {
        return false;
    };
    let mut g = f.clone();
    g.name = new.to_owned();
    g.params.retain(|p| !map.contains_key(&p.name));
    for p in map.keys() {
        g.locals.remove(p);
    }
    let mut targets: BTreeMap<ValueId, String> = BTreeMap::new();
    for i in g.blocks.iter().flat_map(|b| &b.instructions) {
        if let Op::Read { root, path, .. } = &i.op
            && let Some(t) = map.get(root)
        {
            if !path.is_empty() {
                return false;
            }
            targets.insert(i.dest, t.clone());
        }
    }
    // Every read of a substituted local is the callee of one indirect call.
    let uses = use_counts(&g);
    let mut called = BTreeSet::new();
    for i in g.blocks.iter().flat_map(|b| &b.instructions) {
        if let Op::Indirect { callee, args } = &i.op
            && targets.contains_key(callee)
        {
            if args.iter().any(|a| targets.contains_key(&a.value)) {
                return false;
            }
            called.insert(*callee);
        }
    }
    if targets
        .keys()
        .any(|v| !called.contains(v) || uses.get(v) != Some(&1))
    {
        return false;
    }
    let mut nested = vec![];
    for b in &mut g.blocks {
        b.instructions.retain(|i| !targets.contains_key(&i.dest));
        for i in &mut b.instructions {
            match &mut i.op {
                Op::Indirect { callee, args } => {
                    if let Some(t) = targets.get(callee) {
                        i.op = Op::Call {
                            callee: t.clone(),
                            args: std::mem::take(args),
                            types: vec![],
                            capability: None,
                        };
                    }
                }
                Op::Closure { expr, aliases, .. } => {
                    let free = crate::check::free_names(expr);
                    if free
                        .iter()
                        .any(|n| map.contains_key(aliases.get(n).map(String::as_str).unwrap_or(n)))
                    {
                        return false;
                    }
                }
                Op::Handle { aliases, .. } => {
                    let aliases = aliases.clone();
                    nested.push((nested_of(old, &i.op), nested_of(new, &i.op), aliases));
                }
                _ => {}
            }
        }
    }
    made.insert(new.to_owned(), g);
    for (olds, news, aliases) in nested {
        for (o, n) in olds.iter().zip(&news) {
            let Some(h) = program.functions.get(o) else {
                continue;
            };
            let inner: BTreeMap<String, String> = h
                .params
                .iter()
                .filter_map(|p| {
                    let source = aliases.get(&p.name).unwrap_or(&p.name);
                    map.get(source).map(|t| (p.name.clone(), t.clone()))
                })
                .collect();
            if !substitute_tree(program, o, n, &inner, made) {
                return false;
            }
        }
    }
    true
}

// --- Tail-resumptive handlers ---

/// One operation clause, rewritten to return the value it resumes with.
struct OpClause {
    function: String,
    /// The clause's capture parameters (leading), by name.
    captures: Vec<String>,
    /// Per operation argument: whether the clause binds it (`_` does not).
    keep: Vec<bool>,
}

struct Ctx<'a> {
    effect: &'a str,
    tag: &'a str,
    /// Evidence parameters: (parameter name, enclosing local, pass, type).
    evidence: Vec<(String, String, Pass, Type)>,
    /// By operation name.
    ops: BTreeMap<String, OpClause>,
    caps: Vec<String>,
    regions: &'a BTreeMap<String, String>,
}
impl Ctx<'_> {
    fn evidence_param(&self, capture: &str) -> Option<&(String, String, Pass, Type)> {
        self.evidence
            .iter()
            .find(|(p, _, _, _)| p.rsplit_once('.').is_some_and(|(_, n)| n == capture))
    }
}

#[allow(clippy::too_many_arguments)]
fn site(
    program: &mut Program,
    regions: &BTreeMap<String, String>,
    parent: &str,
    b: usize,
    k: usize,
    tag: &str,
    sites: &mut Sites,
) -> Result<(), VerifiedError> {
    let unsupported =
        |detail: String| VerifiedError::unsupported(Unsupported::Effect, parent, detail);
    let i = program.functions[parent].blocks[b].instructions[k].clone();
    let Op::Handle {
        state_value,
        expr,
        aliases,
        ..
    } = &i.op
    else {
        return Err(VerifiedError::Invalid("not a handle instruction".into()));
    };
    let ExprKind::Handle {
        effect,
        state,
        body,
        ret,
        ops,
    } = &expr.kind
    else {
        return Err(VerifiedError::Invalid("not a handle expression".into()));
    };
    if state.is_some() || state_value.is_some() {
        return Err(unsupported(format!("stateful handler for {}", effect.name)));
    }
    if effect.label.is_some() || !effect.args.is_empty() {
        return Err(unsupported(format!(
            "labeled or parameterized handler for {}",
            effect.name
        )));
    }
    let body_fn = nested_instance(parent, &nested_name("handler_body", &body.span));
    let ret_fn = nested_instance(parent, &nested_name("handler_clause", &ret.body.span));
    for f in [&body_fn, &ret_fn] {
        if !program.functions.contains_key(f) {
            return Err(VerifiedError::Invalid(format!(
                "handler function {f} was not lowered"
            )));
        }
    }
    let mut ctx = Ctx {
        effect: &effect.name,
        tag,
        evidence: vec![],
        ops: BTreeMap::new(),
        caps: vec![],
        regions,
    };
    for clause in ops {
        let function = nested_instance(parent, &nested_name("handler_clause", &clause.body.span));
        let c = program
            .functions
            .get_mut(&function)
            .ok_or_else(|| VerifiedError::Invalid(format!("clause {function} was not lowered")))?;
        let (last, args) = clause
            .pats
            .split_last()
            .ok_or_else(|| VerifiedError::Invalid("operation clause without patterns".into()))?;
        let mut keep = vec![];
        for p in args {
            match p.kind {
                PatternKind::Bind(_) => keep.push(true),
                PatternKind::Wild => keep.push(false),
                _ => {
                    return Err(VerifiedError::unsupported(
                        Unsupported::Match,
                        parent,
                        "a handler clause pattern other than a name or `_`",
                    ));
                }
            }
        }
        if !matches!(last.kind, PatternKind::Bind(_)) {
            return Err(unsupported(format!(
                "handler for {}: clause {} never resumes (only tail-resumptive handlers)",
                effect.name, clause.name
            )));
        }
        let bound = keep.iter().filter(|k| **k).count() + 1;
        let captures = tail_resumptive(c, bound).map_err(|why| {
            unsupported(format!(
                "handler for {}: clause {} {why} (only tail-resumptive handlers)",
                effect.name, clause.name
            ))
        })?;
        if c.effects.instances.iter().any(|e| e.name == effect.name) {
            return Err(unsupported(format!(
                "handler for {}: clause {} performs the handled effect",
                effect.name, clause.name
            )));
        }
        relabel_regions(c, regions);
        for e in &c.effects.instances {
            if e.name == crate::regions::REGION_EFFECT
                && let Some(l) = &e.label
                && !l.starts_with('$')
                && !ctx.caps.contains(l)
            {
                ctx.caps.push(l.clone());
            }
        }
        for p in c.params.iter().take(captures.len()) {
            if ctx.evidence_param(&p.name).is_none() {
                let local = aliases.get(&p.name).cloned().unwrap_or(p.name.clone());
                let n = tag.rsplit_once('#').map_or("0", |(_, n)| n);
                ctx.evidence
                    .push((format!("$ev{n}.{}", p.name), local, p.pass, p.ty.clone()));
            }
        }
        sites.escape.insert(function.clone(), tag.to_owned());
        ctx.ops.insert(
            clause.name.clone(),
            OpClause {
                function,
                captures,
                keep,
            },
        );
    }
    // The body, in place (only this site refers to it), then everything
    // with the effect it reaches.
    let mut queue = VecDeque::new();
    let mut done = BTreeSet::new();
    specialize(program, &ctx, &body_fn, &body_fn, &mut queue, &mut done)?;
    while let Some(g) = queue.pop_front() {
        let new = with_suffix(&g, tag);
        specialize(program, &ctx, &g, &new, &mut queue, &mut done)?;
    }
    sites.restore.insert(body_fn.clone(), tag.to_owned());
    // The handle instruction: call the body, then the `return` clause.
    let ret_bind = match ret.pats.first().map(|p| &p.kind) {
        Some(PatternKind::Bind(n)) => Some(n.clone()),
        Some(PatternKind::Wild) | None => None,
        Some(_) => {
            return Err(VerifiedError::unsupported(
                Unsupported::Match,
                parent,
                "a handler clause pattern other than a name or `_`",
            ));
        }
    };
    let body_params = program.functions[&body_fn].params.clone();
    let body_result = program.functions[&body_fn].result.clone();
    let ret_params = program.functions[&ret_fn].params.clone();
    let f = program.functions.get_mut(parent).expect("site");
    let mut next = next_value(f);
    let mut out = vec![];
    let mut args = vec![];
    for p in &body_params {
        let root = match ctx.evidence.iter().find(|(e, _, _, _)| *e == p.name) {
            Some((_, local, _, _)) => local.clone(),
            None => aliases.get(&p.name).cloned().unwrap_or(p.name.clone()),
        };
        args.push(read_argument(
            f, &mut out, &mut next, &root, p.pass, &p.ty, &i.span,
        ));
    }
    let value = next;
    next += 1;
    out.push(Instruction {
        dest: value,
        op: Op::Call {
            callee: body_fn,
            args,
            types: vec![],
            capability: None,
        },
        ty: body_result.clone(),
        span: i.span.clone(),
        token: None,
        fallible: false,
    });
    f.values.insert(value, body_result);
    let mut args = vec![];
    for p in &ret_params {
        if ret_bind.as_deref() == Some(p.name.as_str()) {
            args.push(Argument {
                pass: p.pass,
                value,
                place: None,
            });
            continue;
        }
        let root = aliases.get(&p.name).cloned().unwrap_or(p.name.clone());
        args.push(read_argument(
            f, &mut out, &mut next, &root, p.pass, &p.ty, &i.span,
        ));
    }
    out.push(Instruction {
        dest: i.dest,
        op: Op::Call {
            callee: ret_fn,
            args,
            types: vec![],
            capability: None,
        },
        ty: i.ty.clone(),
        span: i.span.clone(),
        token: i.token,
        fallible: false,
    });
    f.blocks[b].instructions.splice(k..=k, out);
    Ok(())
}

/// Check that clause `c` (with `pats` pattern parameters, the continuation
/// last) only resumes in tail position, and rewrite it to return the value
/// it resumes with. Returns the names of its capture parameters.
fn tail_resumptive(c: &mut FunctionIr, pats: usize) -> Result<Vec<String>, &'static str> {
    if pats == 0 || c.params.len() < pats {
        return Err("has no continuation");
    }
    let resume = c.params.last().expect("params").name.clone();
    if !matches!(c.params.last().expect("params").ty, Type::Function { .. }) {
        return Err("has no continuation");
    }
    let reads: BTreeSet<ValueId> = c
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .filter(
            |i| matches!(&i.op, Op::Read { root, path, .. } if *root == resume && path.is_empty()),
        )
        .map(|i| i.dest)
        .collect();
    for i in c.blocks.iter().flat_map(|b| &b.instructions) {
        match &i.op {
            Op::Read { root, path, .. } if *root == resume && !path.is_empty() => {
                return Err("projects its continuation");
            }
            Op::Closure { expr, aliases, .. } | Op::Handle { expr, aliases, .. }
                if crate::check::free_names(expr)
                    .iter()
                    .any(|n| aliases.get(n).unwrap_or(n) == &resume) =>
            {
                return Err("captures its continuation");
            }
            _ => {}
        }
    }
    if reads.is_empty() {
        return Err("never resumes");
    }
    let uses = use_counts(c);
    // Each resumption `k(e)` is returned directly: its value is `e`.
    let mut resumed: BTreeMap<ValueId, ValueId> = BTreeMap::new();
    for i in c.blocks.iter().flat_map(|b| &b.instructions) {
        if let Op::Indirect { callee, args } = &i.op
            && reads.contains(callee)
        {
            let [arg] = args.as_slice() else {
                return Err("resumes with several values");
            };
            if uses.get(callee) != Some(&1) || reads.contains(&arg.value) {
                return Err("uses its continuation other than by resuming");
            }
            resumed.insert(i.dest, arg.value);
        }
    }
    if resumed.len() != reads.len() {
        return Err("uses its continuation other than by resuming");
    }
    for b in &c.blocks {
        for v in terminator_uses(&b.terminator)
            .into_iter()
            .chain(b.instructions.iter().flat_map(|i| op_uses(&i.op)))
        {
            if resumed.contains_key(&v) && !matches!(b.terminator, Terminator::Return(r) if r == v)
            {
                return Err("resumes other than in tail position");
            }
        }
        if let Terminator::Return(r) = b.terminator
            && !resumed.contains_key(&r)
            && c.values
                .get(&r)
                .or_else(|| b.instructions.iter().find(|i| i.dest == r).map(|i| &i.ty))
                .is_none_or(|t| t.name() != "Never")
        {
            return Err("returns without resuming");
        }
    }
    let mut result = None;
    for b in &mut c.blocks {
        b.instructions
            .retain(|i| !reads.contains(&i.dest) && !resumed.contains_key(&i.dest));
        if let Terminator::Return(r) = &mut b.terminator
            && let Some(v) = resumed.get(r)
        {
            *r = *v;
            if result.is_none() {
                result = c.values.get(v).cloned().or_else(|| {
                    b.instructions
                        .iter()
                        .find(|i| i.dest == *v)
                        .map(|i| i.ty.clone())
                });
            }
        }
    }
    // The resumed value's type is the operation's result type.
    if let Type::Function { params, .. } = &c.params.last().expect("params").ty
        && let [(_, t)] = params.as_slice()
    {
        result = Some(t.clone());
    }
    c.result = result.ok_or("resumes with an untyped value")?;
    c.params.pop();
    c.locals.remove(&resume);
    let captures = c.params.len() + 1 - pats;
    Ok(c.params[..captures]
        .iter()
        .map(|p| p.name.clone())
        .collect())
}

/// Copy `old` as `new` (in place when equal) for the handler site of
/// `ctx`: evidence parameters where its row has the effect, operations as
/// clause calls, calls to functions with the effect as calls to their
/// instances (queued), and the same for its nested functions.
fn specialize(
    program: &mut Program,
    ctx: &Ctx<'_>,
    old: &str,
    new: &str,
    queue: &mut VecDeque<String>,
    done: &mut BTreeSet<String>,
) -> Result<(), VerifiedError> {
    if !done.insert(new.to_owned()) {
        return Ok(());
    }
    let unsupported = |detail: String| VerifiedError::unsupported(Unsupported::Effect, old, detail);
    let mut f = program
        .functions
        .get(old)
        .cloned()
        .ok_or_else(|| VerifiedError::Invalid(format!("no IR for {old}")))?;
    f.name = new.to_owned();
    let has = |e: &Effect| e.name == ctx.effect;
    // Every copy takes the evidence (a handler body's row need not name
    // the effect it handles; a nested function reads it from its parent).
    {
        f.effects.instances.retain(|e| !has(e));
        relabel_regions(&mut f, ctx.regions);
        for c in &ctx.caps {
            if !f
                .effects
                .instances
                .iter()
                .any(|e| e.label.as_ref() == Some(c))
            {
                f.effects.instances.push(Effect {
                    label: Some(c.clone()),
                    name: crate::regions::REGION_EFFECT.into(),
                    args: vec![],
                });
            }
        }
        for (name, _, pass, ty) in &ctx.evidence {
            f.params.push(crate::ast::Param {
                name: name.clone(),
                pass: *pass,
                projections: vec![],
                ty: ty.clone(),
                span: Default::default(),
            });
            f.locals.insert(name.clone(), ty.clone());
        }
    }
    let effectful = |name: &str| {
        !name.starts_with('$')
            && program
                .functions
                .get(name)
                .is_some_and(|g| g.effects.instances.iter().any(has))
    };
    let mut next = next_value(&f);
    let mut nested = vec![];
    let prefix = format!("{}::", ctx.effect);
    let mut blocks = std::mem::take(&mut f.blocks);
    for block in &mut blocks {
        let mut out = vec![];
        for mut i in std::mem::take(&mut block.instructions) {
            match &mut i.op {
                Op::Call {
                    callee,
                    args,
                    capability: None,
                    ..
                } if callee.starts_with(&prefix) => {
                    let op = &callee[prefix.len()..];
                    let clause = ctx.ops.get(op).ok_or_else(|| {
                        unsupported(format!("operation {callee} without a clause"))
                    })?;
                    let mut full = vec![];
                    for c in &clause.captures {
                        let (param, _, pass, ty) = ctx.evidence_param(c).ok_or_else(|| {
                            VerifiedError::Invalid(format!("no evidence for capture {c}"))
                        })?;
                        full.push(read_argument(
                            &mut f, &mut out, &mut next, param, *pass, ty, &i.span,
                        ));
                    }
                    if args.len() != clause.keep.len() {
                        return Err(VerifiedError::Invalid(format!("arity of {callee}")));
                    }
                    full.extend(
                        args.drain(..)
                            .zip(&clause.keep)
                            .filter(|(_, k)| **k)
                            .map(|(a, _)| a),
                    );
                    *args = full;
                    *callee = clause.function.clone();
                }
                Op::Call {
                    callee,
                    args,
                    capability: None,
                    ..
                } if effectful(callee) => {
                    for (param, _, pass, ty) in &ctx.evidence {
                        let a =
                            read_argument(&mut f, &mut out, &mut next, param, *pass, ty, &i.span);
                        args.push(a);
                    }
                    queue.push_back(callee.clone());
                    *callee = with_suffix(callee, ctx.tag);
                }
                Op::Handle { expr, .. } => {
                    if let ExprKind::Handle { effect, .. } = &expr.kind
                        && effect.name == ctx.effect
                    {
                        return Err(unsupported(format!(
                            "a handler for {} inside another one",
                            ctx.effect
                        )));
                    }
                    nested.push((nested_of(old, &i.op), nested_of(new, &i.op)));
                }
                Op::Indirect { callee, .. } => {
                    let ty = f.values.get(callee).cloned().or_else(|| {
                        out.iter()
                            .find(|x: &&Instruction| x.dest == *callee)
                            .map(|x| x.ty.clone())
                    });
                    if let Some(Type::Function { effects, .. }) = ty
                        && effects.instances.iter().any(has)
                    {
                        return Err(unsupported(format!(
                            "a function value performing {}",
                            ctx.effect
                        )));
                    }
                }
                _ => {}
            }
            out.push(i);
        }
        block.instructions = out;
    }
    f.blocks = blocks;
    program.functions.insert(new.to_owned(), f);
    for (olds, news) in nested {
        for (o, n) in olds.iter().zip(&news) {
            if program.functions.contains_key(o) {
                specialize(program, ctx, o, n, queue, done)?;
            }
        }
    }
    Ok(())
}
