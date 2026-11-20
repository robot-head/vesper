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
//! * the callee is not recursive (no cycle through it in the call graph) and
//!   not `fip`. Trait-bounded generics reach the IR monomorphized
//!   (`f$mono$T`, ordinary functions). Any other generic callee is inlined
//!   as a copy typed with the call's type arguments, when its body needs no
//!   type the native backend would look up by source span (see
//!   `generic_body_ok`);
//! * its [`cost`] is at most [`Budget::small_callee`], or it has a
//!   single call site in the program and is at most
//!   [`Budget::single_site_callee`] instructions;
//! * its effect row is empty, or it is exactly the caller's row and holds only
//!   unlabeled `Fail[E]` instances. A raise in the inlined body then performs
//!   the caller's `Fail[E]` capability, which is what the caller's re-raise
//!   after the call did. Rows with labels, tails or capability effects are
//!   not inlined, and callers whose row names a capability effect
//!   (`ConstTime`, `Realtime`, `DetFloat`) are left alone so the per-function
//!   capability verifiers see the code they saw before, and so are callers
//!   holding `Box` values, whose drops are placed per block;
//! * its body has no closures, `?`, indirect calls, tiled or `Box`
//!   operations, and no type in it mentions `Box` or a function type. A
//!   slice parameter (`in` or `inout`, E1520) is a view value: it becomes
//!   the caller's slice local or parameter, or a bound copy of the view (a
//!   sub-range or `as_slice_mut()`). An `inout` slice writes elements
//!   through the view's pointer and cannot re-point the view, so a copy
//!   reaches the same buffer (#1089). An `in Str` is a 24-byte view value
//!   and is bound or borrowed like any aggregate;
//! * a local `handle` in the body keeps its scope: the handled body and its
//!   clauses are lowered from their own shared graphs, which reach the
//!   enclosing function's locals only through the handle's `aliases`, so
//!   the copy re-points those at the renamed locals (#989). A callee with a
//!   `return` inside a handled body stays a call, since that `return` leaves
//!   the callee itself;
//! * every `inout` argument (and every borrowed aggregate or sequence `in`
//!   argument) names a place rooted at one of the caller's locals, and a
//!   callee with a handler borrows only whole caller locals.
//!
//! Recursive, closure and indirect calls stay calls, and so does anything
//! over budget. Growth is capped per caller ([`Budget::caller_growth`]) and
//! per program ([`Budget::program_growth`]). [`Report::refused`] says why
//! each remaining direct call stayed a call.
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

mod site;

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
    /// Most instructions the copies of one callee may add to the program.
    /// It bounds the code a small, widely used callee (a derived `cmp` or
    /// `eq` called from every sort and lookup) multiplies into.
    pub callee_growth: usize,
    /// Most handles inlining may copy into one caller. The native backend
    /// copies the enclosing scope's bindings at every handle, so a caller
    /// with many inlined handles compiles slowly (`integer_widths` calls a
    /// doubly handled helper dozens of times from one function).
    pub caller_handles: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            small_callee: 96,
            single_site_callee: 240,
            caller_growth: 2_000,
            program_growth: 40_000,
            callee_growth: 4_000,
            caller_handles: 8,
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
            callee_growth: 0,
            caller_handles: 0,
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
    /// Call sites left as calls, per callee and reason (see [`Refusal`]).
    /// Only direct calls of functions whose IR is in the program count.
    pub refused: BTreeMap<String, BTreeMap<Refusal, usize>>,
    /// Inlined call sites per callee that kept only the hot part of the
    /// callee and call it on the cold side (partial inlining, #1025).
    pub partial: BTreeMap<String, usize>,
    /// Inlined call sites whose known arguments removed branches or blocks
    /// from the copy.
    pub folded: usize,
    /// Instructions the copies of each callee added, including the copies
    /// that arrived inside another inlined body.
    pub growth: BTreeMap<String, usize>,
    /// Uses of a module constant whose function returns one literal,
    /// replaced by that literal (see [`fold_literal_constants`]).
    pub constants: usize,
    /// Function -> units of each callee body copied into it (directly or
    /// nested), so inlining that function charges those callees again.
    contents: BTreeMap<String, BTreeMap<String, usize>>,
}

/// Why a direct call stayed a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Refusal {
    /// The callee calls itself, directly or through others.
    Recursive,
    /// A property of the callee's body or signature (see [`inlinable`]).
    Callee(&'static str),
    /// The caller is one whose code a per-function verifier inspects, or it
    /// holds `Box` values.
    Caller,
    /// Over [`Budget::small_callee`] (or the single-site limit).
    Size,
    /// The caller's or the program's growth budget is spent.
    Growth,
    /// A property of this call site (see [`site_bindings`]).
    Site(&'static str),
}

/// Inline small, non-recursive direct calls in `program`, bottom-up over the
/// call graph, within `budget`. See the module documentation for the rules.
pub fn inline(world: &World, program: &mut Program, budget: &Budget) -> Report {
    let mut report = Report::default();
    if budget.small_callee == 0 && budget.single_site_callee == 0 {
        return report;
    }
    report.constants = fold_literal_constants(world, program);
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
        // A handled body and its clauses are lowered inside the function
        // that holds the handle, so a call in them is a call of that
        // function: without these edges a function that calls itself from a
        // handled body looks non-recursive, gets inlined into its own
        // handler graph, and the native backend expands that graph forever.
        for nested in handler_graphs(f) {
            if let Some((key, _)) = program.functions.get_key_value(&nested) {
                graph.add_edge(name.as_str(), key.as_str(), ());
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
    // Functions that a `return` in another graph leaves: a `return` inside
    // a handled body is lowered in the handler's own graph as an exit to
    // the enclosing function, which an inlined copy no longer is.
    let exited_from_outside = program
        .functions
        .values()
        .flat_map(|g| {
            g.blocks
                .iter()
                .flat_map(|b| &b.instructions)
                .filter_map(move |i| match &i.op {
                    Op::Exit { owner, .. } if *owner != g.name => Some(owner.clone()),
                    _ => None,
                })
        })
        .collect::<BTreeSet<_>>();
    let mut candidates = BTreeMap::<String, Result<(), Refusal>>::new();
    for f in program.functions.values() {
        let verdict = if recursive.contains(&f.name) {
            Err(Refusal::Recursive)
        } else if exited_from_outside.contains(&f.name) {
            Err(Refusal::Callee("return inside a handler"))
        } else {
            inlinable(world, f).map_err(Refusal::Callee)
        };
        candidates.insert(f.name.clone(), verdict);
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
        } else {
            for i in caller.blocks.iter().flat_map(|b| &b.instructions) {
                if let Op::Call { callee, .. } = &i.op
                    && candidates.contains_key(callee)
                {
                    report.refuse(callee, Refusal::Caller);
                }
            }
        }
        program.functions.insert(name, caller);
    }
    if report.sites > 0 || report.constants > 0 {
        crate::ir::thread_tokens(program);
    }
    report
}

/// Replaces each call of a module constant whose function only returns a
/// literal (`const NAME: T = 3;`, manuscript §2.2) with that literal, in
/// every caller [`caller_eligible`] accepts. A named scalar constant then
/// costs what the literal did: no call, no inlining budget, and a literal
/// operand for the passes after this one. Returns the uses replaced.
fn fold_literal_constants(world: &World, program: &mut Program) -> usize {
    let literals = program
        .functions
        .values()
        .filter(|f| crate::arrays::is_const_function(&f.name) && f.params.is_empty())
        .filter_map(|f| match f.blocks.as_slice() {
            [block] => match (block.instructions.as_slice(), &block.terminator) {
                ([i], Terminator::Return(v)) if i.dest == *v && !i.fallible => match &i.op {
                    Op::Constant(literal) => Some((f.name.clone(), literal.clone())),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    if literals.is_empty() {
        return 0;
    }
    let mut folded = 0;
    for f in program.functions.values_mut() {
        if !caller_eligible(world, f) {
            continue;
        }
        for i in f.blocks.iter_mut().flat_map(|b| &mut b.instructions) {
            if let Op::Call { callee, args, .. } = &i.op
                && args.is_empty()
                && let Some(literal) = literals.get(callee)
            {
                i.op = Op::Constant(literal.clone());
                i.token = None;
                i.fallible = false;
                folded += 1;
            }
        }
    }
    folded
}

/// The shared graphs (`ir::nested_name`) the handles in `f` lower: each
/// handled body and each clause.
fn handler_graphs(f: &FunctionIr) -> Vec<String> {
    let mut out = Vec::new();
    for i in f.blocks.iter().flat_map(|b| &b.instructions) {
        if let Op::Handle { expr, .. } = &i.op
            && let ExprKind::Handle { body, ret, ops, .. } = &expr.kind
        {
            out.push(crate::ir::nested_name("handler_body", &body.span));
            out.push(crate::ir::nested_name("handler_clause", &ret.body.span));
            for op in ops {
                out.push(crate::ir::nested_name("handler_clause", &op.body.span));
            }
        }
    }
    out
}

/// The handles in `f` and, transitively, in its handler graphs: what the
/// native backend emits for each copy of `f`.
fn handles(program: &Program, f: &FunctionIr) -> usize {
    let own = |g: &FunctionIr| {
        g.blocks
            .iter()
            .flat_map(|b| &b.instructions)
            .filter(|i| matches!(i.op, Op::Handle { .. }))
            .count()
    };
    let mut total = own(f);
    let mut pending = handler_graphs(f);
    let mut seen = BTreeSet::new();
    while let Some(name) = pending.pop() {
        if seen.insert(name.clone())
            && let Some(g) = program.functions.get(&name)
        {
            total += own(g);
            pending.extend(handler_graphs(g));
        }
    }
    total
}

/// [`cost`] of `f` plus that of every handler graph it holds (transitively):
/// the native backend emits those graphs at each handle, so each inlined
/// copy of a handled callee duplicates them.
fn cost_with_handlers(world: &World, program: &Program, f: &FunctionIr) -> usize {
    let mut total = cost(world, f);
    let mut pending = handler_graphs(f);
    let mut seen = BTreeSet::new();
    while let Some(name) = pending.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        if let Some(g) = program.functions.get(&name) {
            total += cost(world, g);
            pending.extend(handler_graphs(g));
        }
    }
    total
}

/// IR size: instructions plus one per block terminator.
pub fn size(f: &FunctionIr) -> usize {
    f.blocks.iter().map(|b| b.instructions.len() + 1).sum()
}

/// The inlining cost of `f`: [`size`] without what lowers to no machine
/// code once locals live in registers and Cranelift folds jumps:
///
/// * constants, unit values, loop headers and whole-local reads;
/// * nullary constructors of scalar enums (`Less`, `Option::None` of a
///   scalar layout), which are constants;
/// * a name binding of a scalar value (a register move);
/// * a block that holds only those and ends in a jump. Inlined bodies end in
///   chains of such forwarding blocks (each callee `return` becomes one), so
///   without this a callee's cost grew with every callee inlined into it: a
///   derived `Ord::cmp` over five `u64` fields cost 36 before its field
///   compares were inlined and 106 after, over [`Budget::small_callee`].
///
/// A short-circuit predicate such as `(b >= 48 && b <= 57) || ...` is mostly
/// operand reads, so its cost tracks its compares and branches.
pub fn cost(world: &World, f: &FunctionIr) -> usize {
    let free = cost_parts(world, f);
    f.blocks
        .iter()
        .map(|b| {
            let work = b.instructions.iter().filter(|i| !free(i)).count();
            let forward = work == 0 && matches!(b.terminator, Terminator::Jump { .. });
            work + usize::from(!forward)
        })
        .sum()
}

/// Whether an instruction of `f` is free in [`cost`].
fn cost_parts<'a>(world: &'a World, f: &'a FunctionIr) -> impl Fn(&Instruction) -> bool + 'a {
    let scalar = move |t: &Type| {
        crate::layout::Layout::of(world, t).is_ok_and(|l| !l.aggregate())
            && !crate::seq::holds_sequence(world, t)
    };
    move |i: &Instruction| match &i.op {
        Op::Constant(_) | Op::Unit | Op::Loop => true,
        Op::Read { path, .. } => path.is_empty(),
        Op::Data(_, fields) => fields.is_empty() && scalar(&i.ty),
        Op::Bind { pattern, value } => {
            matches!(pattern.kind, PatternKind::Bind(_) | PatternKind::Wild)
                && f.values.get(value).is_some_and(scalar)
        }
        _ => false,
    }
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
fn inlinable(world: &World, f: &FunctionIr) -> Result<(), &'static str> {
    let Some(source) = world.functions.get(&f.name) else {
        return Err("not a world function");
    };
    let refuse = |c: bool, why| if c { Err(why) } else { Ok(()) };
    refuse(source.fip, "fip")?;
    // A `dyn` function has one body shared by every vtable (§7.1).
    refuse(
        world.dyn_objects.params.contains_key(&f.name),
        "dyn parameters",
    )?;
    // Trait-bounded generics never get here: the front end monomorphizes
    // them into ordinary `f$mono$T` functions. Any other generic callee is
    // instantiated per call site (`instance`); see `generic_body_ok`.
    if !source.generics.is_empty() {
        generic_body_ok(world, f)?;
    }
    refuse(!f.external_captures.is_empty(), "captures")?;
    refuse(!fail_row_only(&f.effects), "effect row")?;
    refuse(
        !f.fip_call_slots.is_empty()
            || !f.fip_join_slots.is_empty()
            || !f.box_drops_before_inst.is_empty()
            || !f.box_drops_after_inst.is_empty()
            || !f.box_drops_before_term.is_empty(),
        "fip slots or Box drops",
    )?;
    refuse(f.result.name() == "Never", "Never result")?;
    refuse(
        f.blocks.is_empty() || !f.blocks[0].params.is_empty(),
        "entry block",
    )?;
    let types = || {
        f.values
            .values()
            .chain(f.locals.values())
            .chain(f.params.iter().map(|p| &p.ty))
            .chain(std::iter::once(&f.result))
    };
    refuse(types().any(|t| holds_box(world, t)), "Box type")?;
    refuse(
        types().any(|t| mentions_function(world, t)),
        "function type",
    )?;
    // A slice parameter is `in` or `inout` (E1520). Both pass the view
    // (pointer and length) by address and neither can re-point it: an
    // `inout` callee writes elements through the view's pointer, so a copy
    // of the view reaches the same buffer (#1089).
    refuse(
        f.params
            .iter()
            .any(|p| p.pass == Pass::Sink && crate::seq::is_slice_type(&p.ty)),
        "sink slice",
    )?;
    for i in f.blocks.iter().flat_map(|b| &b.instructions) {
        match &i.op {
            Op::Closure { .. } => return Err("closure"),
            Op::Try(_) => return Err("try"),
            Op::Indirect { .. } => return Err("indirect call"),
            Op::Tiled { .. } | Op::Fip { .. } => return Err("tiled or fip op"),
            Op::Exit { owner, .. } if *owner != f.name => return Err("foreign exit"),
            _ => {}
        }
    }
    Ok(())
}

/// An unbounded generic callee is inlined as a copy with its type
/// parameters replaced by the call's type arguments ([`instance`]). The
/// native backend lowers most of the copy from its instruction and value
/// types, which the copy carries substituted. What it would instead look up
/// by source span with the enclosing function's type arguments, and so get
/// wrong in a copy, stays out: calls with type arguments of their own
/// (`World::calls` is keyed by span), handlers (their bodies are shared
/// graphs typed with the enclosing function's type arguments), and every
/// instruction typed by a bare type parameter whose native lowering asks the
/// span for its type (operators, indexing and field projections).
fn generic_body_ok(world: &World, f: &FunctionIr) -> Result<(), &'static str> {
    for i in f.blocks.iter().flat_map(|b| &b.instructions) {
        match &i.op {
            Op::Handle { .. } => return Err("generic callee with a handler"),
            Op::Call { types, .. } if !types.is_empty() => {
                return Err("generic callee with explicit call types");
            }
            Op::Call { .. } | Op::Seq { .. }
                if world
                    .calls
                    .get(&(i.span.start, i.span.end))
                    .is_some_and(|vars| !vars.is_empty()) =>
            {
                return Err("generic callee calls a generic function");
            }
            Op::Unary(..) | Op::Binary(..) | Op::Index(..) | Op::Project(..) => {
                return Err("generic callee with operators or projections");
            }
            _ => {}
        }
    }
    Ok(())
}

/// The copy of the unbounded generic `callee` for the call at `span`, with
/// the call's type arguments substituted, or `None` when the checker did not
/// record them all.
fn instance(world: &World, callee: &FunctionIr, span: &Span) -> Option<FunctionIr> {
    let generics = &world.functions.get(&callee.name)?.generics;
    let vars = world.calls.get(&(span.start, span.end))?;
    if generics.iter().any(|g| !vars.contains_key(&g.name)) {
        return None;
    }
    let mut f = callee.clone();
    let sub = |t: &mut Type| *t = substitute(t, vars);
    f.params.iter_mut().for_each(|p| sub(&mut p.ty));
    sub(&mut f.result);
    f.effects = substitute_effects(&f.effects, vars);
    f.values.values_mut().for_each(sub);
    f.locals.values_mut().for_each(sub);
    for i in f.blocks.iter_mut().flat_map(|b| &mut b.instructions) {
        sub(&mut i.ty);
    }
    Some(f)
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
) -> Result<Vec<Binding>, &'static str> {
    if args.len() != callee.params.len() || caller.values.get(&dest) != Some(&callee.result) {
        return Err("signature");
    }
    if !callee.effects.instances.is_empty() && callee.effects != caller.effects {
        return Err("effect row differs from the caller's");
    }
    if !caller.bounds.is_empty() {
        let generics = caller.bounds.keys().collect::<BTreeSet<_>>();
        if callee
            .values
            .values()
            .chain(callee.locals.values())
            .any(|t| mentions_names(t, &generics))
        {
            return Err("caller generics");
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
            Pass::Inout => Binding::Place(
                local_place
                    .ok_or("inout argument is not a local place")?
                    .clone(),
            ),
            Pass::In if aggregate || sequence => match local_place {
                Some(place) => Binding::Place(place.clone()),
                None if !sequence => Binding::Bind(a.value),
                None => return Err("borrowed sequence argument is not a local place"),
            },
            Pass::In | Pass::Sink => Binding::Bind(a.value),
        });
    }
    let handles = callee
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .any(|i| matches!(i.op, Op::Handle { .. }));
    if handles
        && out
            .iter()
            .any(|b| matches!(b, Binding::Place(p) if !p.path.is_empty()))
    {
        // A handler names its captures as whole locals (`aliases`).
        return Err("handled callee borrows a field place");
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
            return Err("callee re-binds a borrowed parameter");
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn inline_into(
    world: &World,
    program: &Program,
    caller: &mut FunctionIr,
    candidates: &BTreeMap<String, Result<(), Refusal>>,
    sites: &BTreeMap<String, usize>,
    budget: &Budget,
    serial: &mut usize,
    report: &mut Report,
) {
    let mut added = 0usize;
    let mut handles_added = 0usize;
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
            let Some(verdict) = candidates.get(callee) else {
                i += 1;
                continue;
            };
            let decision = verdict.and_then(|()| {
                let original = &program.functions[callee];
                if !types.is_empty() || capability.is_some() || original.name == caller.name {
                    return Err(Refusal::Site("explicit types or capability"));
                }
                let generic = world
                    .functions
                    .get(callee)
                    .is_some_and(|s| !s.generics.is_empty());
                let copy = if generic {
                    Some(
                        instance(world, original, &inst.span)
                            .ok_or(Refusal::Site("type arguments not recorded"))?,
                    )
                } else {
                    None
                };
                let target = copy.as_ref().unwrap_or(original);
                let bindings =
                    site_bindings(world, caller, target, inst.dest, args).map_err(Refusal::Site)?;
                let whole = cost_with_handlers(world, program, target);
                let handlers = whole - cost(world, target);
                let folded = site::fold(world, caller, target, &bindings)
                    .unwrap_or_else(|| site::Folded::none(target));
                let at_site = folded.cost(world, target) + handlers;
                let fits = |c: usize| {
                    c <= budget.small_callee
                        || (sites.get(callee) == Some(&1) && c <= budget.single_site_callee)
                };
                let (cost, split) = if fits(at_site) {
                    if !fits(whole) {
                        log(format_args!(
                            "full\t{}\t{callee}\t{whole}\t{at_site}",
                            caller.name
                        ));
                    }
                    (at_site, None)
                } else if let Some(split) = (handlers == 0 && partial_ok(world, target, &bindings))
                    .then(|| site::split(world, target, &folded, budget.small_callee))
                    .flatten()
                {
                    log(format_args!(
                        "partial\t{}\t{callee}\t{whole}\t{at_site}\t{}\t{}",
                        caller.name, split.hot_cost, split.cold_cost
                    ));
                    (split.hot_cost, Some(split))
                } else {
                    log(format_args!(
                        "size\t{}\t{callee}\t{whole}\t{at_site}",
                        caller.name
                    ));
                    return Err(Refusal::Size);
                };
                // The copy also duplicates every body already inlined into
                // the callee: charge those callees' caps too.
                let nested = report.contents.get(callee);
                let over_cap = std::iter::once((callee, cost))
                    .chain(nested.into_iter().flatten().map(|(n, u)| (n, *u)))
                    .any(|(n, u)| {
                        report.growth.get(n).copied().unwrap_or(0) + u > budget.callee_growth
                    });
                if added + cost > budget.caller_growth
                    || report.added + cost > budget.program_growth
                    || over_cap
                    || handles_added + handles(program, target) > budget.caller_handles
                {
                    return Err(Refusal::Growth);
                }
                Ok((cost, bindings, copy, folded, split))
            });
            let (cost, bindings, copy, folded, split) = match decision {
                Ok(d) => d,
                Err(why) => {
                    report.refuse(callee, why);
                    i += 1;
                    continue;
                }
            };
            let target = copy.unwrap_or_else(|| program.functions[callee].clone());
            *serial += 1;
            let first = splice(
                caller,
                b,
                i,
                &target,
                &bindings,
                *serial,
                &folded,
                split.as_ref(),
            );
            spliced.extend(first..caller.blocks.len());
            added += cost;
            handles_added += handles(program, &target);
            report.added += cost;
            report.sites += 1;
            let nested = report
                .contents
                .get(&target.name)
                .cloned()
                .unwrap_or_default();
            let holds = report.contents.entry(caller.name.clone()).or_default();
            *holds.entry(target.name.clone()).or_default() += cost;
            for (n, u) in &nested {
                *holds.entry(n.clone()).or_default() += u;
            }
            *report.growth.entry(target.name.clone()).or_default() += cost;
            for (n, u) in nested {
                *report.growth.entry(n).or_default() += u;
            }
            *report.callees.entry(target.name.clone()).or_default() += 1;
            if split.is_some() {
                *report.partial.entry(target.name.clone()).or_default() += 1;
            }
            if folded.decides() {
                report.folded += 1;
            }
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

/// Partial inlining calls the callee again with the site's arguments, so
/// every argument bound by value must be one it can pass twice: a value
/// that owns nothing, or a slice view.
fn partial_ok(world: &World, callee: &FunctionIr, bindings: &[Binding]) -> bool {
    callee.params.iter().zip(bindings).all(|(p, b)| match b {
        Binding::Place(_) => true,
        Binding::Bind(_) => crate::seq::is_slice_type(&p.ty) || site::plain(world, &p.ty),
    })
}

/// With `VESPER_INLINE_LOG` set to a path, append one line per decision
/// on a callee over its budget before site folding: `full` (it fits after
/// folding), `partial` (hot and cold cost) or `size` (it stays a call).
/// The lane report's tables come from it.
fn log(line: std::fmt::Arguments<'_>) {
    use std::io::Write as _;
    if let Some(path) = std::env::var_os("VESPER_INLINE_LOG")
        && let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    {
        let _ = writeln!(file, "{line}");
    }
}

impl Report {
    fn refuse(&mut self, callee: &str, why: Refusal) {
        *self
            .refused
            .entry(callee.to_owned())
            .or_default()
            .entry(why)
            .or_default() += 1;
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
            // The handled body and its clauses are lowered from their own
            // graphs (`ir::nested_name`), shared by every copy; they name
            // the enclosing function's locals only through `aliases`, so the
            // copy re-points those at the renamed (or the caller's) locals.
            // `site_bindings` only replaces such a callee's borrowed
            // parameters by whole caller locals.
            Op::Handle {
                state_value,
                aliases,
                moves,
                ..
            } => {
                if let Some(v) = state_value {
                    *v = self.value(*v);
                }
                for slot in aliases.values_mut() {
                    *slot = self.root(slot, &[]).0;
                }
                for m in moves {
                    *m = self.place(m);
                }
            }
            // Excluded by `inlinable`.
            Op::Closure { .. } => {}
        }
    }
}

/// Replace the call at `caller.blocks[b].instructions[i]` with `callee`'s
/// body. Returns the index of the first block copied from the callee.
#[allow(clippy::too_many_arguments)]
fn splice(
    caller: &mut FunctionIr,
    b: usize,
    i: usize,
    callee: &FunctionIr,
    bindings: &[Binding],
    serial: usize,
    folded: &site::Folded,
    split: Option<&site::Split>,
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
    // The cold side of a partial inlining: the original call, after the
    // copied blocks.
    let cold_block = first + callee.blocks.len();
    let keep = |k: usize| match split {
        Some(s) => s.hot[k],
        None => folded.live[k],
    };
    for (k, source) in callee.blocks.iter().enumerate() {
        if !keep(k) {
            // Control never reaches this copy of the block.
            caller.blocks.push(Block {
                entry_token: 0,
                incoming_tokens: vec![],
                outgoing_token: 0,
                params: source.params.iter().map(|v| rename.value(*v)).collect(),
                instructions: vec![],
                terminator: Terminator::Unreachable,
            });
            continue;
        }
        let target = |t: usize| {
            if split.is_some_and(|s| s.block == k && s.cold == t) {
                cold_block
            } else {
                t + rename.blocks
            }
        };
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
        let decided = folded.jumps.get(&k).map(|t| Terminator::Jump {
            block: target(*t),
            args: vec![],
        });
        let terminator = terminator
            .or(decided)
            .unwrap_or_else(|| match &source.terminator {
                Terminator::Return(v) => back(rename.value(*v)),
                Terminator::Jump { block, args } => Terminator::Jump {
                    block: target(*block),
                    args: args.iter().map(|v| rename.value(*v)).collect(),
                },
                Terminator::Branch { condition, yes, no } => Terminator::Branch {
                    condition: rename.value(*condition),
                    yes: target(*yes),
                    no: target(*no),
                },
                Terminator::Match { value, arms } => Terminator::Match {
                    value: rename.value(*value),
                    arms: arms
                        .iter()
                        .map(|(p, t)| {
                            let mut p = p.clone();
                            rename.pattern(&mut p);
                            (p, target(*t))
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
    if split.is_some() {
        let dest = fresh;
        caller.values.insert(dest, callee.result.clone());
        let mut again = call.clone();
        again.dest = dest;
        again.token = None;
        caller.blocks.push(Block {
            entry_token: 0,
            incoming_tokens: vec![],
            outgoing_token: 0,
            params: vec![],
            instructions: vec![again],
            terminator: back(dest),
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
