//! A bounded Place IR inliner for the native backend (Stage 1 performance
//! wave, lane 1, #931).
//!
//! Cranelift does no inlining across functions, so every small Vesper helper
//! (a saturating add, a field accessor, a clamp) costs a full call: argument
//! spills, a stack frame for its locals, and the result copied back. This pass
//! splices the body of a small callee into its caller's control-flow graph.
//!
//! It runs only on the native path, only when `optimize` is on, after
//! [`crate::ir_opt::optimize`] and before [`crate::fip::elaborate_non_fip_drops`]:
//! in the JIT, in library exports (`vesper build --lib`, which ports use) and
//! in sessions. Executable objects keep their calls, because the
//! integration-B verifiers report and budget the call graph of exactly the
//! objects `vesper build` emits.
//! It is never called from `ir_opt::optimize`, which the verified backend
//! also runs, so the verified backend's input and the Stage 1 lock corpus
//! (built with `optimize` off) do not change.
//!
//! # What is inlined
//!
//! A direct [`Op::Call`] to a world function whose IR is in the program, when
//!
//! * the callee is not recursive (no cycle through it in the call graph), not
//!   generic and not `fip`;
//! * its [`cost`] is at most [`Budget::small_callee`], or it has a
//!   single call site in the program and is at most
//!   [`Budget::single_site_callee`] instructions;
//! * its effect row is empty, or it is exactly the caller's row and holds only
//!   unlabeled `Fail[E]` instances. A raise in the inlined body then performs
//!   the caller's `Fail[E]` capability, which is what the caller's re-raise
//!   after the call did. Rows with labels, tails, handlers or capability
//!   effects are not inlined, and callers whose row names a capability effect
//!   (`ConstTime`, `Realtime`, `DetFloat`) are left alone so the per-function
//!   capability verifiers see the code they saw before, and so are callers
//!   holding `Box` values, whose drops are placed per block;
//! * its body has no closures, handlers, `?`, indirect calls, tiled or `Box`
//!   operations, no type in it mentions `Box` or a function type, and no
//!   slice parameter is `inout` or `sink` (an `in` slice is a view value: it
//!   becomes the caller's slice local, or a bound copy of the view);
//! * every `inout` argument (and every borrowed aggregate or sequence `in`
//!   argument) names a place rooted at one of the caller's locals.
//!
//! Recursive, generic, closure and indirect calls stay calls, and so does
//! anything over budget. Growth is capped per caller
//! ([`Budget::caller_growth`]) and per program ([`Budget::program_growth`]).
//!
//! # How a call is replaced
//!
//! The caller's block is split at the call. The prefix binds each by-value
//! parameter to a fresh, renamed local (`name$inlN`) and jumps to the
//! callee's entry. Borrowed parameters are not bound: the callee's reads,
//! writes and argument places of `p.f` become reads, writes and places of the
//! caller's argument place (`caller.path.f`), which is the place the callee
//! would have reached through its pointer. Callee values and blocks are
//! renumbered, never merged with the caller's, and its `return` (both the
//! block terminator and `Op::Exit`) becomes a jump to the continuation block,
//! whose parameter is the call's result value. State tokens are re-threaded
//! afterwards with [`crate::ir::thread_tokens`].
use crate::{
    ast::*,
    check::{Place, World},
    ir::{Argument, Block, FunctionIr, Instruction, Op, Program, Terminator, ValueId},
};
use std::collections::{BTreeMap, BTreeSet};

/// Size limits, counted with [`cost`]: Place IR instructions that emit code, plus one per block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Budget {
    /// A callee of at most this size is inlined at every eligible site.
    pub small_callee: usize,
    /// A callee with exactly one call site in the program is inlined up to
    /// this size.
    pub single_site_callee: usize,
    /// Most instructions inlining may add to one caller.
    pub caller_growth: usize,
    /// Most instructions inlining may add to the whole program.
    pub program_growth: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            small_callee: 96,
            single_site_callee: 240,
            caller_growth: 2_000,
            program_growth: 40_000,
        }
    }
}

impl Budget {
    /// A budget that inlines nothing.
    pub fn none() -> Self {
        Self {
            small_callee: 0,
            single_site_callee: 0,
            caller_growth: 0,
            program_growth: 0,
        }
    }
}

/// What [`inline`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Call sites replaced by the callee's body.
    pub sites: usize,
    /// Inlined call sites per callee.
    pub callees: BTreeMap<String, usize>,
    /// Instructions added to the program.
    pub added: usize,
    /// Caller -> every function whose body now appears in it, including
    /// bodies inlined into those callees first. Verifiers use it to carry
    /// per-function policy facts (for example declared realtime effects) to
    /// the code that holds the inlined copy (#959).
    pub inlined_into: BTreeMap<String, BTreeSet<String>>,
}

/// Inline small, non-recursive direct calls in `program`, bottom-up over the
/// call graph, within `budget`. See the module documentation for the rules.
pub fn inline(world: &World, program: &mut Program, budget: &Budget) -> Report {
    let mut report = Report::default();
    if budget.small_callee == 0 && budget.single_site_callee == 0 {
        return report;
    }
    let mut graph = petgraph::graphmap::DiGraphMap::<&str, ()>::new();
    let mut sites = BTreeMap::<String, usize>::new();
    for (name, f) in &program.functions {
        graph.add_node(name.as_str());
        for i in f.blocks.iter().flat_map(|b| &b.instructions) {
            if let Op::Call { callee, .. } = &i.op
                && let Some((key, _)) = program.functions.get_key_value(callee)
            {
                graph.add_edge(name.as_str(), key.as_str(), ());
                *sites.entry(callee.clone()).or_default() += 1;
            }
        }
    }
    // Tarjan's algorithm yields strongly connected components callees-first.
    let components = petgraph::algo::tarjan_scc(&graph);
    let mut recursive = BTreeSet::new();
    let mut order = Vec::new();
    for component in &components {
        if component.len() > 1 || graph.contains_edge(component[0], component[0]) {
            recursive.extend(component.iter().map(|n| n.to_string()));
        }
        order.extend(component.iter().map(|n| n.to_string()));
    }
    drop(graph);
    let candidates: BTreeSet<String> = program
        .functions
        .values()
        .filter(|f| !recursive.contains(&f.name) && inlinable(world, f))
        .map(|f| f.name.clone())
        .collect();
    if candidates.is_empty() {
        return report;
    }
    let mut serial = 0usize;
    for name in order {
        if report.added >= budget.program_growth {
            break;
        }
        let Some(mut caller) = program.functions.remove(&name) else {
            continue;
        };
        if caller_eligible(world, &caller) {
            inline_into(
                world,
                program,
                &mut caller,
                &candidates,
                &sites,
                budget,
                &mut serial,
                &mut report,
            );
        }
        program.functions.insert(name, caller);
    }
    if report.sites > 0 {
        crate::ir::thread_tokens(program);
    }
    report
}

/// IR size: instructions plus one per block terminator.
pub fn size(f: &FunctionIr) -> usize {
    f.blocks.iter().map(|b| b.instructions.len() + 1).sum()
}

/// The inlining cost of `f`: [`size`] without the instructions that lower to
/// no machine code once locals live in registers (constants, unit values and
/// whole-local reads). A short-circuit predicate such as
/// `(b >= 48 && b <= 57) || ...` is mostly those, so its cost tracks its
/// compares and blocks, not its operand reads.
pub fn cost(f: &FunctionIr) -> usize {
    f.blocks
        .iter()
        .map(|b| {
            1 + b
                .instructions
                .iter()
                .filter(|i| {
                    !matches!(&i.op, Op::Constant(_) | Op::Unit | Op::Loop)
                        && !matches!(&i.op, Op::Read { path, .. } if path.is_empty())
                })
                .count()
        })
        .sum()
}

fn fail_row_only(effects: &Effects) -> bool {
    effects.tail.is_none()
        && effects
            .instances
            .iter()
            .all(|e| e.name == "Fail" && e.label.is_none() && e.args.len() == 1)
}

fn mentions_function(world: &World, t: &Type) -> bool {
    crate::native::contains_function(world, t)
}

/// Callee properties that do not depend on the call site.
fn inlinable(world: &World, f: &FunctionIr) -> bool {
    let Some(source) = world.functions.get(&f.name) else {
        return false;
    };
    if source.fip
        || !source.generics.is_empty()
        || !f.bounds.is_empty()
        || !f.external_captures.is_empty()
        || !fail_row_only(&f.effects)
        || !f.fip_call_slots.is_empty()
        || !f.fip_join_slots.is_empty()
        || !f.box_drops_before_inst.is_empty()
        || !f.box_drops_after_inst.is_empty()
        || !f.box_drops_before_term.is_empty()
        || f.result.name() == "Never"
        || f.blocks.is_empty()
        || !f.blocks[0].params.is_empty()
    {
        return false;
    }
    let types = || {
        f.values
            .values()
            .chain(f.locals.values())
            .chain(f.params.iter().map(|p| &p.ty))
            .chain(std::iter::once(&f.result))
    };
    if types().any(|t| holds_box(world, t) || mentions_function(world, t))
        || f.params
            .iter()
            .any(|p| p.pass != Pass::In && crate::seq::is_slice_type(&p.ty))
    {
        return false;
    }
    f.blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .all(|i| match &i.op {
            Op::Closure { .. }
            | Op::Handle { .. }
            | Op::Try(_)
            | Op::Indirect { .. }
            | Op::Tiled { .. }
            | Op::Fip { .. } => false,
            Op::Exit { owner, .. } => *owner == f.name,
            _ => true,
        })
}

/// Callers whose code a per-function verifier inspects, or that hold `Box`
/// values, keep their calls.
fn caller_eligible(world: &World, f: &FunctionIr) -> bool {
    world.functions.get(&f.name).is_none_or(|s| !s.fip)
        && f.fip_call_slots.is_empty()
        && f.fip_join_slots.is_empty()
        && f.box_drops_before_inst.is_empty()
        && f.box_drops_after_inst.is_empty()
        && f.box_drops_before_term.is_empty()
        && !f
            .effects
            .instances
            .iter()
            .any(|e| crate::capability_effects::is_capability_effect(&e.name))
        // `fip::elaborate_non_fip_drops` places `Box` drops per block; a
        // split block would let it drop a moved-out value still waiting in
        // a temporary for the call after the inlined body.
        && !f
            .values
            .values()
            .chain(f.locals.values())
            .chain(f.params.iter().map(|p| &p.ty))
            .any(|t| holds_box(world, t))
}

/// `Box` anywhere in `t`, including inside the fields of a named type.
fn holds_box(world: &World, t: &Type) -> bool {
    crate::fip::mentions_box(world, t) || crate::fip::type_contains_box(world, t)
}

/// How one callee parameter is supplied at a call site.
enum Binding {
    /// Bound by value to a fresh local.
    Bind(ValueId),
    /// Replaced by the caller's place.
    Place(Place),
}

fn place_of(written: &str) -> Place {
    let mut path = written.split('.');
    Place {
        root: path.next().unwrap_or_default().to_owned(),
        path: path.map(str::to_owned).collect(),
    }
}

fn pattern_binds(p: &Pattern, out: &mut BTreeSet<String>) {
    match &p.kind {
        PatternKind::Bind(n) => {
            out.insert(n.clone());
        }
        PatternKind::Tuple(ps) => ps.iter().for_each(|p| pattern_binds(p, out)),
        PatternKind::Constructor { fields, .. } => {
            fields.iter().for_each(|(_, p)| pattern_binds(p, out))
        }
        PatternKind::Wild | PatternKind::Literal(_) => {}
    }
}

fn mentions_names(t: &Type, names: &BTreeSet<&String>) -> bool {
    match t {
        Type::Named(n, args) => {
            (args.is_empty() && names.contains(n)) || args.iter().any(|t| mentions_names(t, names))
        }
        Type::Tuple(ts) => ts.iter().any(|t| mentions_names(t, names)),
        Type::Function { .. } => true,
        _ => false,
    }
}

/// The bindings for a call, or `None` when the site must stay a call.
fn site_bindings(
    world: &World,
    caller: &FunctionIr,
    callee: &FunctionIr,
    dest: ValueId,
    args: &[Argument],
) -> Option<Vec<Binding>> {
    if args.len() != callee.params.len()
        || caller.values.get(&dest) != Some(&callee.result)
        || (!callee.effects.instances.is_empty() && callee.effects != caller.effects)
    {
        return None;
    }
    if !caller.bounds.is_empty() {
        let generics = caller.bounds.keys().collect::<BTreeSet<_>>();
        if callee
            .values
            .values()
            .chain(callee.locals.values())
            .any(|t| mentions_names(t, &generics))
        {
            return None;
        }
    }
    let mut out = Vec::with_capacity(args.len());
    for (a, p) in args.iter().zip(&callee.params) {
        let local_place = a
            .place
            .as_ref()
            .filter(|place| caller.locals.contains_key(&place.root));
        let aggregate = crate::layout::Layout::of(world, &p.ty)
            .map(|l| l.aggregate())
            .unwrap_or(true);
        let sequence = crate::seq::holds_sequence(world, &p.ty);
        if crate::seq::is_slice_type(&p.ty) {
            // A borrowed slice view is a value (pointer and length): the
            // callee's reads go through the caller's own slice local when the
            // argument names one, and through a bound copy of the view
            // otherwise (a sub-range `s[lo..hi]`, or `q.as_slice()`).
            let same_view = local_place.filter(|place| {
                place.path.is_empty() && caller.locals.get(&place.root) == Some(&p.ty)
            });
            out.push(match same_view {
                Some(place) => Binding::Place(place.clone()),
                None => Binding::Bind(a.value),
            });
            continue;
        }
        out.push(match p.pass {
            Pass::Inout => Binding::Place(local_place?.clone()),
            Pass::In if aggregate || sequence => match local_place {
                Some(place) => Binding::Place(place.clone()),
                None if !sequence => Binding::Bind(a.value),
                None => return None,
            },
            Pass::In | Pass::Sink => Binding::Bind(a.value),
        });
    }
    // A borrowed parameter is replaced by the caller's place, so the callee
    // must never re-bind its name.
    let replaced = callee
        .params
        .iter()
        .zip(&out)
        .filter(|(_, b)| matches!(b, Binding::Place(_)))
        .map(|(p, _)| p.name.clone())
        .collect::<BTreeSet<_>>();
    if !replaced.is_empty() {
        let mut bound = BTreeSet::new();
        for b in &callee.blocks {
            for i in &b.instructions {
                if let Op::Bind { pattern, .. } = &i.op {
                    pattern_binds(pattern, &mut bound);
                }
            }
            if let Terminator::Match { arms, .. } = &b.terminator {
                arms.iter().for_each(|(p, _)| pattern_binds(p, &mut bound));
            }
        }
        if !bound.is_disjoint(&replaced) {
            return None;
        }
    }
    Some(out)
}

#[allow(clippy::too_many_arguments)]
fn inline_into(
    world: &World,
    program: &Program,
    caller: &mut FunctionIr,
    candidates: &BTreeSet<String>,
    sites: &BTreeMap<String, usize>,
    budget: &Budget,
    serial: &mut usize,
    report: &mut Report,
) {
    let mut added = 0usize;
    // Blocks spliced in from a callee were already processed bottom-up.
    let mut spliced = BTreeSet::new();
    let mut b = 0;
    while b < caller.blocks.len() {
        if spliced.contains(&b) {
            b += 1;
            continue;
        }
        let mut i = 0;
        while i < caller.blocks[b].instructions.len() {
            let inst = &caller.blocks[b].instructions[i];
            let Op::Call {
                callee,
                args,
                types,
                capability,
            } = &inst.op
            else {
                i += 1;
                continue;
            };
            if !types.is_empty() || capability.is_some() {
                i += 1;
                continue;
            }
            let Some(target) = candidates
                .contains(callee)
                .then(|| program.functions.get(callee))
                .flatten()
                .filter(|t| t.name != caller.name)
            else {
                i += 1;
                continue;
            };
            let cost = cost(target);
            let fits = cost <= budget.small_callee
                || (sites.get(callee) == Some(&1) && cost <= budget.single_site_callee);
            if !fits
                || added + cost > budget.caller_growth
                || report.added + cost > budget.program_growth
            {
                i += 1;
                continue;
            }
            let Some(bindings) = site_bindings(world, caller, target, inst.dest, args) else {
                i += 1;
                continue;
            };
            let target = target.clone();
            *serial += 1;
            let first = splice(caller, b, i, &target, &bindings, *serial);
            spliced.extend(first..caller.blocks.len());
            added += cost;
            report.added += cost;
            report.sites += 1;
            *report.callees.entry(target.name.clone()).or_default() += 1;
            let nested = report
                .inlined_into
                .get(&target.name)
                .cloned()
                .unwrap_or_default();
            let into = report.inlined_into.entry(caller.name.clone()).or_default();
            into.insert(target.name.clone());
            into.extend(nested);
            // The rest of this block moved to the continuation block, which
            // the outer loop reaches later.
            break;
        }
        b += 1;
    }
}

struct Rename<'a> {
    values: ValueId,
    blocks: usize,
    names: BTreeMap<String, String>,
    places: BTreeMap<String, &'a Place>,
}

impl Rename<'_> {
    fn value(&self, v: ValueId) -> ValueId {
        v + self.values
    }
    fn root(&self, root: &str, path: &[String]) -> (String, Vec<String>) {
        if let Some(place) = self.places.get(root) {
            let mut full = place.path.clone();
            full.extend(path.iter().cloned());
            (place.root.clone(), full)
        } else if let Some(name) = self.names.get(root) {
            (name.clone(), path.to_vec())
        } else {
            (root.to_owned(), path.to_vec())
        }
    }
    fn place(&self, p: &Place) -> Place {
        let (root, path) = self.root(&p.root, &p.path);
        Place { root, path }
    }
    fn pattern(&self, p: &mut Pattern) {
        match &mut p.kind {
            PatternKind::Bind(n) => {
                if let Some(name) = self.names.get(n) {
                    *n = name.clone();
                }
            }
            PatternKind::Tuple(ps) => ps.iter_mut().for_each(|p| self.pattern(p)),
            PatternKind::Constructor { fields, .. } => {
                fields.iter_mut().for_each(|(_, p)| self.pattern(p))
            }
            PatternKind::Wild | PatternKind::Literal(_) => {}
        }
    }
    fn args(&self, args: &mut [Argument]) {
        for a in args {
            a.value = self.value(a.value);
            a.place = a.place.as_ref().map(|p| self.place(p));
        }
    }
    fn op(&self, op: &mut Op) {
        match op {
            Op::Constant(_) | Op::Unit | Op::Loop => {}
            Op::Read { root, path, .. } => {
                let (r, p) = self.root(root, path);
                *root = r;
                *path = p;
            }
            Op::Bind { pattern, value } => {
                self.pattern(pattern);
                *value = self.value(*value);
            }
            Op::Write { place, value, .. } => {
                let p = self.place(&place_of(place));
                *place = std::iter::once(p.root)
                    .chain(p.path)
                    .collect::<Vec<_>>()
                    .join(".");
                *value = self.value(*value);
            }
            Op::Unary(_, v) | Op::Project(v, _) | Op::Try(v) => *v = self.value(*v),
            Op::Binary(_, a, b) | Op::Index(a, b) => {
                *a = self.value(*a);
                *b = self.value(*b);
            }
            Op::Tuple(vs) | Op::Vector { args: vs, .. } => {
                vs.iter_mut().for_each(|v| *v = self.value(*v))
            }
            Op::Data(_, fields) => fields.iter_mut().for_each(|(_, v)| *v = self.value(*v)),
            Op::Call { args, .. }
            | Op::Tiled { args, .. }
            | Op::Seq { args, .. }
            | Op::Fip { args, .. } => self.args(args),
            Op::Indirect { callee, args } => {
                *callee = self.value(*callee);
                self.args(args);
            }
            Op::Exit { value, .. } => *value = self.value(*value),
            // Excluded by `inlinable`.
            Op::Closure { .. } | Op::Handle { .. } => {}
        }
    }
}

/// Replace the call at `caller.blocks[b].instructions[i]` with `callee`'s
/// body. Returns the index of the first block copied from the callee.
fn splice(
    caller: &mut FunctionIr,
    b: usize,
    i: usize,
    callee: &FunctionIr,
    bindings: &[Binding],
    serial: usize,
) -> usize {
    let next_value = caller
        .values
        .keys()
        .copied()
        .chain(caller.blocks.iter().flat_map(|b| {
            b.instructions
                .iter()
                .map(|i| i.dest)
                .chain(b.params.clone())
        }))
        .max()
        .map_or(0, |v| v + 1);
    let callee_values = callee
        .values
        .keys()
        .copied()
        .chain(callee.blocks.iter().flat_map(|b| {
            b.instructions
                .iter()
                .map(|i| i.dest)
                .chain(b.params.clone())
        }))
        .max()
        .map_or(0, |v| v + 1);
    let continuation = caller.blocks.len();
    let first = continuation + 1;
    let places = callee
        .params
        .iter()
        .zip(bindings)
        .filter_map(|(p, b)| match b {
            Binding::Place(place) => Some((p.name.clone(), place)),
            Binding::Bind(_) => None,
        })
        .collect::<BTreeMap<_, _>>();
    let names = callee
        .locals
        .keys()
        .filter(|n| !places.contains_key(*n))
        .map(|n| (n.clone(), format!("{n}$inl{serial}")))
        .collect::<BTreeMap<_, _>>();
    let rename = Rename {
        values: next_value,
        blocks: first,
        names,
        places,
    };
    for (n, t) in &callee.locals {
        if let Some(name) = rename.names.get(n) {
            caller.locals.insert(name.clone(), t.clone());
        }
    }
    for (v, t) in &callee.values {
        caller.values.insert(rename.value(*v), t.clone());
    }
    let mut fresh = next_value + callee_values;
    // Split the caller's block at the call.
    let block = &mut caller.blocks[b];
    let mut rest = block.instructions.split_off(i);
    let call = rest.remove(0);
    let terminator = std::mem::replace(
        &mut block.terminator,
        Terminator::Jump {
            block: first,
            args: vec![],
        },
    );
    for (p, binding) in callee.params.iter().zip(bindings) {
        if let Binding::Bind(value) = binding {
            let dest = fresh;
            fresh += 1;
            caller.values.insert(dest, Type::unit());
            block.instructions.push(Instruction {
                dest,
                op: Op::Bind {
                    pattern: Pattern {
                        kind: PatternKind::Bind(rename.names[&p.name].clone()),
                        span: call.span.clone(),
                    },
                    value: *value,
                },
                ty: Type::unit(),
                span: call.span.clone(),
                token: None,
                fallible: false,
            });
        }
    }
    caller.blocks.push(Block {
        entry_token: 0,
        incoming_tokens: vec![],
        outgoing_token: 0,
        params: vec![call.dest],
        instructions: rest,
        terminator,
    });
    let back = |v: ValueId| Terminator::Jump {
        block: continuation,
        args: vec![v],
    };
    for source in &callee.blocks {
        let mut instructions = Vec::with_capacity(source.instructions.len());
        let mut terminator = None;
        for inst in &source.instructions {
            let mut inst = inst.clone();
            inst.dest = rename.value(inst.dest);
            inst.token = None;
            rename.op(&mut inst.op);
            if let Op::Exit { value, .. } = inst.op {
                // `return value`: the rest of this block is unreachable.
                terminator = Some(back(value));
                break;
            }
            instructions.push(inst);
        }
        let terminator = terminator.unwrap_or_else(|| match &source.terminator {
            Terminator::Return(v) => back(rename.value(*v)),
            Terminator::Jump { block, args } => Terminator::Jump {
                block: block + rename.blocks,
                args: args.iter().map(|v| rename.value(*v)).collect(),
            },
            Terminator::Branch { condition, yes, no } => Terminator::Branch {
                condition: rename.value(*condition),
                yes: yes + rename.blocks,
                no: no + rename.blocks,
            },
            Terminator::Match { value, arms } => Terminator::Match {
                value: rename.value(*value),
                arms: arms
                    .iter()
                    .map(|(p, t)| {
                        let mut p = p.clone();
                        rename.pattern(&mut p);
                        (p, t + rename.blocks)
                    })
                    .collect(),
            },
            Terminator::Unreachable => Terminator::Unreachable,
        });
        caller.blocks.push(Block {
            entry_token: 0,
            incoming_tokens: vec![],
            outgoing_token: 0,
            params: source.params.iter().map(|v| rename.value(*v)).collect(),
            instructions,
            terminator,
        });
    }
    first
}

/// The values an instruction reads, in no particular order.
pub fn operands(op: &Op) -> Vec<ValueId> {
    let args = |args: &[Argument]| args.iter().map(|a| a.value).collect::<Vec<_>>();
    match op {
        Op::Constant(_) | Op::Unit | Op::Loop | Op::Read { .. } | Op::Closure { .. } => vec![],
        Op::Handle { state_value, .. } => state_value.iter().copied().collect(),
        Op::Bind { value, .. } | Op::Write { value, .. } | Op::Exit { value, .. } => vec![*value],
        Op::Unary(_, v) | Op::Project(v, _) | Op::Try(v) => vec![*v],
        Op::Binary(_, a, b) | Op::Index(a, b) => vec![*a, *b],
        Op::Tuple(vs) | Op::Vector { args: vs, .. } => vs.clone(),
        Op::Data(_, fields) => fields.iter().map(|(_, v)| *v).collect(),
        Op::Call { args: a, .. }
        | Op::Tiled { args: a, .. }
        | Op::Seq { args: a, .. }
        | Op::Fip { args: a, .. } => args(a),
        Op::Indirect { callee, args: a } => {
            let mut out = args(a);
            out.push(*callee);
            out
        }
    }
}

/// The values a block terminator reads.
pub fn terminator_operands(t: &Terminator) -> Vec<ValueId> {
    match t {
        Terminator::Return(v) => vec![*v],
        Terminator::Jump { args, .. } => args.clone(),
        Terminator::Branch { condition, .. } => vec![*condition],
        Terminator::Match { value, .. } => vec![*value],
        Terminator::Unreachable => vec![],
    }
}
