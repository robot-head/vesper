//! Aggregates built where they end up (port gaps lane 7, #990).
//!
//! On the native optimize path a constructed aggregate (`Op::Data`,
//! `Op::Tuple`) or an aggregate call result is a pointer to a stack slot.
//! Without a plan each one is built in its own slot and copied word by word
//! into the next: a variant payload into its variant, the variant into the
//! function's result. [`plan`] picks, for each such value that is used
//! exactly once, the place its single use copies it into:
//!
//! - [`Home::Result`]: the value is the block's `return` (or an `Op::Exit` of
//!   the graph itself), so it is built in the result buffer;
//! - [`Home::Field`]: the value is a field of a later constructor in the same
//!   block, so it is built inside that constructor's storage.
//!
//! Constructors of constants then become stores of constants straight into
//! the result (BBR's `outer_new`), and a variant whose payload is a struct
//! literal writes the struct's words into the result once (Ignis's `check`).
//!
//! Soundness: a home is chosen only inside one block, and only for a value
//! with exactly one use, so nothing else can read the value between its
//! construction and its consumer. The result buffer is the caller's fresh
//! output (or the function's own register-result buffer), and a field home
//! belongs to a constructor that does not exist yet, so no operand of the
//! instructions in between can alias it. Graphs that mention `Box` keep the
//! old lowering (FIP reuse slots and drop elaboration track their slots).
use super::*;
use crate::ir::{FunctionIr, Op, Terminator, ValueId};

/// Where an aggregate value of a graph is built ([`plan`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Home {
    /// The graph's result buffer (`sret`).
    Result,
    /// The field at byte `offset` of the constructor that defines value
    /// `parent`, of type `parent_ty`.
    Field {
        parent: ValueId,
        offset: i32,
        parent_ty: Type,
    },
    /// The destination of the `place = value` write at instruction `at`
    /// (block, index) of the graph (port speed lane 2, #1022).
    Place { place: String, at: (usize, usize) },
}

/// How many joins (block parameters, pass-through locals) a value may pass
/// through to its home. Joins outside loops form a DAG, so this only bounds
/// the work of a long `else if` ladder.
const MAX_DEPTH: usize = 64;

/// The facts about graph `f` that [`plan`] consults.
struct Graph<'a> {
    world: &'a World,
    f: &'a FunctionIr,
    generics: &'a BTreeMap<String, Type>,
    uses: BTreeMap<ValueId, usize>,
    /// The defining instruction of each value: (block, index).
    defs: BTreeMap<ValueId, (usize, usize)>,
    /// The block whose parameter each block parameter is.
    params: BTreeMap<ValueId, usize>,
    /// The predecessors of each block, and whether every edge into it is a
    /// `Jump` (which alone passes block arguments).
    preds: BTreeMap<usize, Vec<usize>>,
    only_jumps: BTreeSet<usize>,
    /// Blocks on a cycle (a loop header or body).
    cyclic: BTreeSet<usize>,
    result: Type,
    /// True when the graph is nested in another (a handled body or clause):
    /// its locals may be the enclosing graph's.
    nested: bool,
    /// How often each local is named: bound, read, written, lent as a
    /// place or captured.
    mentions: BTreeMap<String, usize>,
    /// Locals split into word variables ([`Split`]): they have no storage
    /// to build in or to yield.
    split: &'a BTreeMap<String, Vec<Vec<String>>>,
}

impl Graph<'_> {
    fn ty(&self, t: &Type) -> Type {
        crate::numeric::default_type(&substitute(t, self.generics))
    }
    fn value_ty(&self, id: ValueId) -> Option<Type> {
        self.f.values.get(&id).map(|t| self.ty(t))
    }
    /// A constructor or direct call: its storage is fresh unless it has a
    /// home.
    fn candidate(&self, op: &Op) -> bool {
        match op {
            Op::Data(..) | Op::Tuple(_) => true,
            Op::Call { callee, .. } => self.world.functions.contains_key(callee),
            _ => false,
        }
    }
    fn op(&self, id: ValueId) -> Option<&Op> {
        let (b, i) = self.defs.get(&id)?;
        Some(&self.f.blocks[*b].instructions[*i].op)
    }
    /// The home of value `id` of type `own`, from its single consumer after
    /// instruction `after` of block `block` (`None`: from the block's start,
    /// for a block parameter).
    fn consumer_home(
        &self,
        block: usize,
        after: Option<usize>,
        id: ValueId,
        own: &Type,
        depth: usize,
    ) -> Option<Home> {
        let b = &self.f.blocks[block];
        let start = after.map_or(0, |a| a + 1);
        let consumer = b.instructions[start..]
            .iter()
            .enumerate()
            .find(|(_, later)| crate::ir_inline::operands(&later.op).contains(&id))
            .map(|(k, later)| (start + k, later));
        let world = self.world;
        match consumer {
            Some((_, later)) => match &later.op {
                Op::Data(name, fields) => fields
                    .iter()
                    .find(|(_, v)| *v == id)
                    .map(|(n, _)| n.clone())
                    .filter(|n| {
                        field_type(world, &self.ty(&later.ty), Some(name), n).as_ref() == Some(own)
                    })
                    .and_then(|n| {
                        field_home(world, later.dest, &self.ty(&later.ty), Some(name), &n)
                    }),
                Op::Tuple(ids) => ids
                    .iter()
                    .position(|v| *v == id)
                    .map(|i| i.to_string())
                    .filter(|n| {
                        field_type(world, &self.ty(&later.ty), None, n).as_ref() == Some(own)
                    })
                    .and_then(|n| field_home(world, later.dest, &self.ty(&later.ty), None, &n)),
                Op::Exit { value, owner } => {
                    (*value == id && *owner == self.f.name && *own == self.result)
                        .then_some(Home::Result)
                }
                Op::Bind { pattern, value } if *value == id => {
                    let (k, read) = self.pass_through(block, consumer.map(|(k, _)| k)?, pattern)?;
                    (self.value_ty(read).as_ref() == Some(own) && self.uses.get(&read) == Some(&1))
                        .then(|| self.consumer_home(block, Some(k), read, own, depth + 1))
                        .flatten()
                }
                Op::Write { place, op, value } => {
                    let at = (block, consumer.map(|(k, _)| k)?);
                    (op == "=" && *value == id && self.place_type(place).as_ref() == Some(own))
                        .then(|| Home::Place {
                            place: place.clone(),
                            at,
                        })
                        .filter(|_| drop_free(world, own))
                        .filter(|_| {
                            !self
                                .split
                                .contains_key(place.split('.').next().unwrap_or(""))
                        })
                }
                _ => None,
            },
            None => match &b.terminator {
                Terminator::Return(v) if *v == id => (*own == self.result).then_some(Home::Result),
                Terminator::Jump {
                    block: target,
                    args,
                } if args.iter().filter(|v| **v == id).count() == 1 => {
                    let k = args.iter().position(|v| *v == id)?;
                    self.param_home(*target, k, own, depth + 1)
                }
                _ => None,
            },
        }
    }
    /// The home of parameter `k` of block `target`, when every value that
    /// reaches it is a single-use constructor or call of type `own` that can
    /// be built there (through more single-use parameters, too).
    fn param_home(&self, target: usize, k: usize, own: &Type, depth: usize) -> Option<Home> {
        if depth > MAX_DEPTH {
            return None;
        }
        let p = *self.f.blocks[target].params.get(k)?;
        if self.uses.get(&p) != Some(&1) || self.value_ty(p).as_ref() != Some(own) {
            return None;
        }
        self.sources(p, depth)?;
        self.consumer_home(target, None, p, own, depth)
    }
    /// The constructors and calls whose values reach block parameter `p`,
    /// and the parameters they pass through (`p` included); `None` when some
    /// value reaching `p` cannot be built in `p`'s home.
    fn sources(&self, p: ValueId, depth: usize) -> Option<(Vec<ValueId>, Vec<ValueId>)> {
        if depth > MAX_DEPTH {
            return None;
        }
        let target = *self.params.get(&p)?;
        let k = self.f.blocks[target].params.iter().position(|v| *v == p)?;
        if self.cyclic.contains(&target) || !self.only_jumps.contains(&target) {
            return None;
        }
        let own = self.value_ty(p)?;
        let mut built = vec![];
        let mut through = vec![p];
        for pred in self.preds.get(&target)? {
            let Terminator::Jump { args, .. } = &self.f.blocks[*pred].terminator else {
                return None;
            };
            let w = *args.get(k)?;
            if self.value_ty(w).is_some_and(|t| t.name() == "Never") {
                // The edge is never taken: control left before the jump.
                continue;
            }
            if self.uses.get(&w) != Some(&1) || self.value_ty(w).as_ref() != Some(&own) {
                return None;
            }
            match self.defs.get(&w) {
                Some((b, _)) if *b == *pred && self.op(w).is_some_and(|op| self.candidate(op)) => {
                    built.push(w)
                }
                Some(_) => return None,
                None if self.params.get(&w) == Some(pred) => {
                    let (more_built, more_through) = self.sources(w, depth + 1)?;
                    built.extend(more_built);
                    through.extend(more_through);
                }
                None => return None,
            }
        }
        Some((built, through))
    }
    /// The one `Read` of the local that the `let` at instruction `at` of
    /// `block` binds, when that local is a pass-through: bound once, read
    /// once later in the same block as a whole move, and named nowhere
    /// else, with a type that drops nothing. The read then yields the
    /// bound storage itself, so the bound value can be built in the read's
    /// home.
    fn pass_through(&self, block: usize, at: usize, pattern: &Pattern) -> Option<(usize, ValueId)> {
        let PatternKind::Bind(name) = &pattern.kind else {
            return None;
        };
        if self.mentions.get(name) != Some(&2)
            || self.split.contains_key(name)
            || self.f.params.iter().any(|p| &p.name == name)
            || self.f.external_captures.contains(name)
            || !self
                .f
                .locals
                .get(name)
                .is_some_and(|t| drop_free(self.world, &self.ty(t)))
        {
            return None;
        }
        self.f.blocks[block].instructions[at + 1..]
            .iter()
            .enumerate()
            .find_map(|(k, inst)| match &inst.op {
                Op::Read {
                    root, path, pass, ..
                } if root == name && path.is_empty() && *pass == Pass::Sink => {
                    Some((at + 1 + k, inst.dest))
                }
                _ => None,
            })
    }
    /// The type of place `a.b.c`, through struct fields.
    fn place_type(&self, place: &str) -> Option<Type> {
        let mut parts = place.split('.');
        let mut ty = self.ty(self.f.locals.get(parts.next()?)?);
        for field in parts {
            let layout = Layout::of(self.world, &ty).ok()?;
            let LayoutKind::Product(fields) = &layout.kind else {
                return None;
            };
            ty = crate::numeric::default_type(&fields.iter().find(|f| f.name == field)?.ty);
        }
        Some(ty)
    }
    /// True when a fault (a `Fail` or an effect that does not resume)
    /// between the first in-place store into `place` and its write could
    /// leave the half-written destination visible: the root is a parameter
    /// (an `inout` caller's place) or a local of an enclosing graph.
    fn observable(&self, place: &str) -> bool {
        let root = place.split('.').next().unwrap_or("");
        self.nested
            || !self.f.locals.contains_key(root)
            || self.f.external_captures.contains(root)
            || self.f.params.iter().any(|p| p.name == root)
    }
    /// True when a call of `callee` cannot leave a half-written destination
    /// visible: the destination dies with the graph on a fault, or the
    /// callee's effect row is empty, so nothing it does is handled by a
    /// caller that could then read the destination.
    fn quiet(&self, callee: &str, observable: bool) -> bool {
        self.world.functions.get(callee).is_some_and(|c| {
            !observable || (c.effects.instances.is_empty() && c.effects.tail.is_none())
        })
    }
    /// A local whose value is a scalar: reading it loads its own variable or
    /// slot, never the storage of an aggregate.
    fn scalar_local(&self, name: &str) -> bool {
        self.f.locals.get(name).is_some_and(|t| {
            Layout::of(self.world, &self.ty(t)).is_ok_and(|l| !l.aggregate())
                && !matches!(t, Type::Function { .. })
        })
    }
    fn aggregate(&self, id: ValueId) -> bool {
        self.value_ty(id)
            .is_some_and(|t| Layout::of(self.world, &t).is_ok_and(|l| l.aggregate()))
    }
}

/// The homes of the aggregate values of graph `f`: each value of a
/// constructor or a direct call whose single use, later in its own block or
/// through single-use block parameters, copies it into the graph's result, a
/// field of a constructor, or the destination of a plain write.
pub(super) fn plan(
    world: &World,
    f: &FunctionIr,
    generics: &BTreeMap<String, Type>,
    nested: bool,
    split: &BTreeMap<String, Vec<Vec<String>>>,
) -> BTreeMap<ValueId, Home> {
    let mut homes = BTreeMap::new();
    if f.values
        .values()
        .chain(f.locals.values())
        .any(|t| crate::fip::mentions_box(world, t))
    {
        return homes;
    }
    let mut uses: BTreeMap<ValueId, usize> = BTreeMap::new();
    let mut defs = BTreeMap::new();
    let mut params = BTreeMap::new();
    let mut preds: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut only_jumps: BTreeSet<usize> = (0..f.blocks.len()).collect();
    let mut graph = petgraph::graphmap::DiGraphMap::<usize, ()>::new();
    for (b, block) in f.blocks.iter().enumerate() {
        graph.add_node(b);
        for p in &block.params {
            params.insert(*p, b);
        }
        for (i, inst) in block.instructions.iter().enumerate() {
            defs.insert(inst.dest, (b, i));
            for id in crate::ir_inline::operands(&inst.op) {
                *uses.entry(id).or_default() += 1;
            }
        }
        for id in crate::ir_inline::terminator_operands(&block.terminator) {
            *uses.entry(id).or_default() += 1;
        }
        let (targets, jump) = match &block.terminator {
            Terminator::Jump { block, .. } => (vec![*block], true),
            Terminator::Branch { yes, no, .. } => (vec![*yes, *no], false),
            Terminator::Match { arms, .. } => (arms.iter().map(|(_, b)| *b).collect(), false),
            _ => (vec![], false),
        };
        for t in targets {
            graph.add_edge(b, t, ());
            preds.entry(t).or_default().push(b);
            if !jump {
                only_jumps.remove(&t);
            }
        }
    }
    let mut mentions: BTreeMap<String, usize> = BTreeMap::new();
    for inst in f.blocks.iter().flat_map(|b| &b.instructions) {
        let mut names: Vec<String> = vec![];
        let places = |args: &[crate::ir::Argument]| {
            args.iter()
                .filter_map(|a| a.place.as_ref().map(|p| p.root.clone()))
                .collect::<Vec<_>>()
        };
        match &inst.op {
            Op::Read { root, .. } => names.push(root.clone()),
            Op::Write { place, .. } => names.push(place.split('.').next().unwrap_or("").to_owned()),
            Op::Bind { pattern, .. } => {
                let mut bound = BTreeSet::new();
                pattern_binds(pattern, &mut bound);
                names.extend(bound);
            }
            Op::Call { args, .. }
            | Op::Indirect { args, .. }
            | Op::Seq { args, .. }
            | Op::Fip { args, .. }
            | Op::Tiled { args, .. } => names.extend(places(args)),
            Op::Closure { aliases, moves, .. } => {
                names.extend(aliases.keys().chain(aliases.values()).cloned());
                names.extend(moves.iter().map(|p| p.root.clone()));
            }
            Op::Handle { aliases, .. } => {
                names.extend(aliases.keys().chain(aliases.values()).cloned())
            }
            _ => {}
        }
        for n in names {
            *mentions.entry(n).or_default() += 1;
        }
    }
    let cyclic = petgraph::algo::tarjan_scc(&graph)
        .into_iter()
        .filter(|c| c.len() > 1 || graph.contains_edge(c[0], c[0]))
        .flatten()
        .collect();
    let g = Graph {
        world,
        f,
        generics,
        uses,
        defs,
        params,
        preds,
        only_jumps,
        cyclic,
        result: crate::numeric::default_type(&substitute(&f.result, generics)),
        nested,
        mentions,
        split,
    };
    for (b, block) in f.blocks.iter().enumerate() {
        for (at, inst) in block.instructions.iter().enumerate() {
            let own = g.ty(&inst.ty);
            if !g.candidate(&inst.op)
                || g.uses.get(&inst.dest) != Some(&1)
                || !Layout::of(world, &own).is_ok_and(|l| l.aggregate())
            {
                continue;
            }
            // A value passed to a block parameter gets the parameter's home
            // below, with every other value that reaches it.
            let jumps = matches!(&block.terminator, Terminator::Jump { args, .. } if args.contains(&inst.dest))
                && !block.instructions[at + 1..]
                    .iter()
                    .any(|later| crate::ir_inline::operands(&later.op).contains(&inst.dest));
            if jumps {
                continue;
            }
            if let Some(home) = g.consumer_home(b, Some(at), inst.dest, &own, 0) {
                homes.insert(inst.dest, home);
            }
        }
    }
    // A block parameter shares the home of the values that reach it, and
    // they get it together, so its consumer skips the copy exactly when all
    // of them were built there.
    for (b, block) in f.blocks.iter().enumerate() {
        for (k, p) in block.params.iter().enumerate() {
            let Some(own) = g.value_ty(*p) else { continue };
            if !Layout::of(world, &own).is_ok_and(|l| l.aggregate()) {
                continue;
            }
            if let Some(home) = g.param_home(b, k, &own, 0)
                && let Some((built, through)) = g.sources(*p, 0)
            {
                for v in built.into_iter().chain(through) {
                    homes.insert(v, home.clone());
                }
            }
        }
    }
    // The read of a pass-through local shares the home of the value bound
    // to it (`Graph::pass_through`).
    for (b, block) in f.blocks.iter().enumerate() {
        for (at, inst) in block.instructions.iter().enumerate() {
            if let Op::Bind { pattern, value } = &inst.op
                && let Some(home) = homes.get(value).cloned()
                && let Some((_, read)) = g.pass_through(b, at, pattern)
            {
                homes.insert(read, home);
            }
        }
    }
    drop_unsafe_places(&g, &mut homes);
    // A single-use aggregate read of a split local stores its words straight
    // into the result or a field of a later constructor (#1090). The words
    // are SSA values read at the read itself, so a later write to the local
    // cannot change them; a home inside a write's destination is left out,
    // since that write's window rule does not cover these stores.
    for (b, block) in f.blocks.iter().enumerate() {
        for (at, inst) in block.instructions.iter().enumerate() {
            let Op::Read { root, .. } = &inst.op else {
                continue;
            };
            let own = g.ty(&inst.ty);
            if !split.contains_key(root)
                || g.uses.get(&inst.dest) != Some(&1)
                || !Layout::of(world, &own).is_ok_and(|l| l.aggregate())
            {
                continue;
            }
            let home = match g.consumer_home(b, Some(at), inst.dest, &own, 0) {
                Some(home @ Home::Result) => home,
                Some(home @ Home::Field { .. }) if !in_place(&homes, &home) => home,
                _ => continue,
            };
            homes.insert(inst.dest, home);
        }
    }
    homes
}

/// True when `home` lies inside the destination of a write
/// ([`Home::Place`]), through its parents.
fn in_place(homes: &BTreeMap<ValueId, Home>, home: &Home) -> bool {
    let mut home = home;
    for _ in 0..=homes.len() {
        match home {
            Home::Place { .. } => return true,
            Home::Result => return false,
            Home::Field { parent, .. } => match homes.get(parent) {
                Some(h) => home = h,
                None => return false,
            },
        }
    }
    true
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

/// Removes each `Write` home whose in-place construction could be observed.
///
/// Building `place = T { .. }` in `place` itself is sound when nothing
/// reads `place`, and nothing can fault visibly, between the first store
/// into it and the write. Call the instructions from the first such store
/// (the earliest constructor of the value's tree) to the write the window.
/// The rule kept here, for every instruction in the window:
///
/// - A member of the tree (a constructor or call built in the destination)
///   copies only fresh values: the scalars it stores are SSA values read
///   before the window, and an aggregate operand is either another member
///   (already in place, so skipped) or a fresh constructor or call result
///   (its own stack storage, which cannot be the destination).
/// - Any other instruction is a constant, a read of a scalar local (its own
///   variable or slot), arithmetic that cannot raise, a constructor that
///   copies only fresh values, or (when the destination dies with the graph
///   on a fault) raising arithmetic or a direct call of scalar arguments.
///   No other `Read`, no `Write`, place argument, effect or handler can run
///   in the window, so no operand reads the destination after the first
///   in-place store.
/// - A member call writes its output into the destination; when that call
///   can fault and the destination is observable after the fault (an
///   `inout` parameter, a local of an enclosing graph), the temporary is
///   kept, so a failing call never leaves a half-written `place`.
///
/// When the rule fails, the whole tree keeps its temporary (its members'
/// field homes then live in a fresh parent), and the write copies as
/// before.
fn drop_unsafe_places(g: &Graph, homes: &mut BTreeMap<ValueId, Home>) {
    // Each pass removes a home or a whole tree, so this ends.
    while drop_unsafe_places_once(g, homes) {}
}

/// One pass of [`drop_unsafe_places`]; true when it should run again.
fn drop_unsafe_places_once(g: &Graph, homes: &mut BTreeMap<ValueId, Home>) -> bool {
    let mut retry = false;
    let groups: BTreeSet<(String, (usize, usize))> = homes
        .values()
        .filter_map(|h| match h {
            Home::Place { place, at } => Some((place.clone(), *at)),
            _ => None,
        })
        .collect();
    for (place, at) in groups {
        let root = |h: &Home| matches!(h, Home::Place { at: a, .. } if *a == at);
        // The tree: values homed in the destination, then fields of them.
        let mut members: BTreeSet<ValueId> = homes
            .iter()
            .filter(|(_, h)| root(h))
            .map(|(v, _)| *v)
            .collect();
        loop {
            let more: Vec<ValueId> = homes
                .iter()
                .filter(|(v, h)| {
                    !members.contains(v)
                        && matches!(h, Home::Field { parent, .. } if members.contains(parent))
                })
                .map(|(v, _)| *v)
                .collect();
            if more.is_empty() {
                break;
            }
            members.extend(more);
        }
        match window_ok(g, &place, at, &members) {
            Ok(()) => {}
            // A call that may fault visibly is built in a temporary of its
            // own, before the window, and the rest of the tree stays.
            // When the call reached its field through block parameters, the
            // parameters and the other values reaching them share that very
            // home: all of them lose it, or the join would skip the copy.
            Err(Some(call)) if homes.get(&call).is_some_and(|h| !root(h)) => {
                let home = homes[&call].clone();
                homes.retain(|_, h| *h != home);
                retry = true;
            }
            Err(_) => {
                let drop: Vec<ValueId> = homes
                    .iter()
                    .filter(|(_, h)| root(h))
                    .map(|(v, _)| *v)
                    .collect();
                for v in drop {
                    homes.remove(&v);
                }
            }
        }
    }
    retry
}

/// [`drop_unsafe_places`]'s rule for the window of the write at `at`.
/// `Err(Some(call))` names a member call that broke the rule (it may fault
/// visibly); `Err(None)` is any other break.
fn window_ok(
    g: &Graph,
    place: &str,
    at: (usize, usize),
    members: &BTreeSet<ValueId>,
) -> Result<(), Option<ValueId>> {
    let observable = g.observable(place);
    let fresh = |id: &ValueId| {
        members.contains(id) || !g.aggregate(*id) || g.op(*id).is_some_and(|op| g.candidate(op))
    };
    let scalar_args = |args: &[crate::ir::Argument]| {
        args.iter()
            .all(|a| a.place.is_none() && a.pass != Pass::Inout && !g.aggregate(a.value))
    };
    // The window, block by block: (block, first index, end index).
    let mut ranges: BTreeMap<usize, (usize, usize)> = BTreeMap::new();
    for id in members {
        if let Some((b, i)) = g.defs.get(id) {
            let len = g.f.blocks[*b].instructions.len();
            let end = if *b == at.0 { at.1 } else { len };
            let e = ranges.entry(*b).or_insert((*i, end));
            e.0 = e.0.min(*i);
        } else if let Some(b) = g.params.get(id) {
            // A parameter's block runs from its start to its jump (or to
            // the write itself).
            let len = g.f.blocks[*b].instructions.len();
            let end = if *b == at.0 { at.1 } else { len };
            ranges.insert(*b, (0, end));
        }
    }
    if !ranges.contains_key(&at.0) {
        return Err(None);
    }
    for (b, (from, to)) in ranges {
        for inst in &g.f.blocks[b].instructions[from..to] {
            let ok = match &inst.op {
                Op::Constant(_) | Op::Unit => true,
                // A pass-through local: the bind names the storage, and the
                // read yields it without loading.
                Op::Bind { value, .. } => members.contains(value),
                Op::Read { root, path, .. } => {
                    members.contains(&inst.dest) || (path.is_empty() && g.scalar_local(root))
                }
                Op::Data(_, fields) => fields.iter().all(|(_, v)| fresh(v)),
                Op::Tuple(ids) => ids.iter().all(fresh),
                Op::Call { callee, args, .. } if members.contains(&inst.dest) => {
                    if !(g.quiet(callee, observable) && scalar_args(args)) {
                        return Err(Some(inst.dest));
                    }
                    true
                }
                Op::Call { callee, args, .. } => {
                    // Its result, if an aggregate, is its own fresh output.
                    g.quiet(callee, observable) && scalar_args(args)
                }
                // Arithmetic that cannot raise runs anywhere; arithmetic
                // that can only where a fault hides the destination.
                Op::Binary(..) | Op::Unary(..) => !observable || !crate::ir::state_action(inst),
                _ => false,
            };
            if !ok {
                return Err(None);
            }
        }
    }
    Ok(())
}

/// True when replacing a value of `ty` drops nothing: no sequence, string,
/// box, handle or function word, so a write into it needs no drop first.
fn drop_free(world: &World, ty: &Type) -> bool {
    fn walk(layout: &Layout) -> bool {
        if crate::strings::is_text(&layout.ty) {
            return false;
        }
        match &layout.kind {
            LayoutKind::Scalar => {
                crate::numeric::is_scalar_name(layout.ty.name()) || layout.ty == Type::unit()
            }
            LayoutKind::Product(fs) => fs.iter().all(|f| walk(&f.layout)),
            LayoutKind::Enum(vs) => vs.iter().all(|v| v.fields.iter().all(|f| walk(&f.layout))),
        }
    }
    Layout::of(world, ty).is_ok_and(|l| walk(&l))
}

/// The type of field `field` of `ty` (of variant `variant` for an enum).
fn field_type(world: &World, ty: &Type, variant: Option<&str>, field: &str) -> Option<Type> {
    let layout = Layout::of(world, ty).ok()?;
    let fields = match (variant, &layout.kind) {
        (Some(name), _) => layout.variant(name)?.1.to_vec(),
        (None, LayoutKind::Product(fs)) => fs.clone(),
        _ => return None,
    };
    fields
        .into_iter()
        .find(|f| f.name == field)
        .map(|f| crate::numeric::default_type(&f.ty))
}

/// The home of field `field` of the constructor `parent` of type `ty`.
fn field_home(
    world: &World,
    parent: ValueId,
    ty: &Type,
    variant: Option<&str>,
    field: &str,
) -> Option<Home> {
    let layout = Layout::of(world, ty).ok()?;
    let fields = match (variant, &layout.kind) {
        (Some(name), _) => layout.variant(name)?.1.to_vec(),
        (None, LayoutKind::Product(fs)) => fs.clone(),
        _ => return None,
    };
    let offset = fields.iter().find(|f| f.name == field)?.offset;
    Some(Home::Field {
        parent,
        offset,
        parent_ty: ty.clone(),
    })
}

/// True when every word of `ty` is plain data: no sequence, string, box,
/// handle, function or `f32` half-word, so storage of it needs no zero.
pub(super) fn plain_data(world: &World, ty: &Type) -> bool {
    fn walk(layout: &Layout) -> bool {
        if crate::strings::is_text(&layout.ty) {
            return false;
        }
        match &layout.kind {
            // An `f32` store writes half its word; keep such storage zeroed.
            LayoutKind::Scalar => {
                (crate::numeric::is_scalar_name(layout.ty.name()) && layout.ty.name() != "f32")
                    || layout.ty == Type::unit()
            }
            LayoutKind::Product(fs) => fs.iter().all(|f| walk(&f.layout)),
            LayoutKind::Enum(vs) => vs.iter().all(|v| v.fields.iter().all(|f| walk(&f.layout))),
        }
    }
    Layout::of(world, ty).is_ok_and(|l| walk(&l))
}

/// The storage chosen for the aggregate values of the graph being lowered:
/// the values built in their home, and the stack slot of each parent
/// constructor that has no home of its own. Pointers are not cached: a home
/// reached through a block parameter is resolved again in each block that
/// builds into it, so every pointer dominates its uses.
#[derive(Default)]
pub(super) struct HomePointers {
    placed: BTreeSet<ValueId>,
    slots: BTreeMap<ValueId, ir::StackSlot>,
    /// Every pointer derived to each slot so far. Pointer metadata
    /// (`NativeInfo`: closure targets of stored fields) is keyed by SSA
    /// value, so each new pointer to a slot starts from the merged metadata
    /// of the earlier ones.
    derived: BTreeMap<ValueId, Vec<ir::Value>>,
}

impl HomePointers {
    pub(super) fn new() -> Self {
        Self::default()
    }
}

impl<M: Module> Codegen<'_, '_, M> {
    /// The storage value `id` is built in, when [`plan`] gave it a home that
    /// resolves here: the result buffer `sret`, its field of a parent
    /// constructor (whose storage is allocated now if it has no home), or
    /// the destination of its write.
    pub(super) fn home_pointer(
        &mut self,
        id: ValueId,
        homes: &BTreeMap<ValueId, Home>,
        pointers: &mut HomePointers,
        sret: Option<ir::Value>,
    ) -> Result<Option<ir::Value>, String> {
        let slot = pointers.slots.contains_key(&id);
        let ptr = self.resolve_home(id, homes, pointers, sret)?;
        if ptr.is_some() && !slot && homes.contains_key(&id) {
            pointers.placed.insert(id);
        }
        Ok(ptr)
    }
    /// A new pointer to the stack slot of parent `id`, carrying the merged
    /// metadata of every earlier pointer to it.
    fn slot_pointer(
        &mut self,
        id: ValueId,
        slot: ir::StackSlot,
        pointers: &mut HomePointers,
    ) -> ir::Value {
        let ptr = self
            .b
            .ins()
            .stack_addr(self.m.target_config().pointer_type(), slot, 0);
        let earlier = pointers.derived.entry(id).or_default();
        let mut info = NativeInfo::default();
        for old in earlier.iter() {
            info.merge(&self.info(*old));
        }
        earlier.push(ptr);
        self.infos.insert(ptr, info);
        ptr
    }
    fn resolve_home(
        &mut self,
        id: ValueId,
        homes: &BTreeMap<ValueId, Home>,
        pointers: &mut HomePointers,
        sret: Option<ir::Value>,
    ) -> Result<Option<ir::Value>, String> {
        if let Some(slot) = pointers.slots.get(&id).copied() {
            return Ok(Some(self.slot_pointer(id, slot, pointers)));
        }
        Ok(Some(match homes.get(&id) {
            None => return Ok(None),
            Some(Home::Result) => match sret {
                Some(out) => out,
                None => return Ok(None),
            },
            Some(Home::Place { place, .. }) => {
                let mut path = place.split('.');
                let p = crate::check::Place {
                    root: path.next().ok_or("empty IR place")?.into(),
                    path: path.map(str::to_owned).collect(),
                };
                if !self.vars.contains_key(&p.root) {
                    return Ok(None);
                }
                self.address(&ir_place_expr(&p, 0..0))?
            }
            Some(Home::Field {
                parent,
                offset,
                parent_ty,
            }) => {
                let base = match self.resolve_home(*parent, homes, pointers, sret)? {
                    Some(base) => base,
                    None => {
                        let size = Layout::of(self.world, parent_ty)?.size;
                        let slot = self.b.create_sized_stack_slot(ir::StackSlotData::new(
                            ir::StackSlotKind::ExplicitSlot,
                            size.max(8),
                            3,
                        ));
                        pointers.slots.insert(*parent, slot);
                        self.slot_pointer(*parent, slot, pointers)
                    }
                };
                self.offset(base, *offset)
            }
        }))
    }
}

/// True when value `id` was built in its home: its consumer skips the copy.
pub(super) fn placed(pointers: &HomePointers, id: ValueId) -> bool {
    pointers.placed.contains(&id)
}

/// Copy-struct locals in registers (P3b gaps lane 5, #1090).
///
/// A local whose type is a struct or tuple of scalar words (nested
/// structs and tuples included) is kept as one Cranelift variable per word
/// on the optimize path instead of a stack slot that every field use loads
/// and stores. Its words are written by the `let` that binds it, by writes
/// to it or its fields, and read by reads of it. A read of a scalar field
/// is its variable; a read of the whole local or of an aggregate field
/// stores the words into fresh storage (the read's home when [`plan`] gave
/// it one, such as the result buffer or a field of `Option::Some(s)`).
///
/// A local that a call lends `inout`, that a sequence, `Box` or tiled
/// operation names as a place, or that a closure or handler mentions keeps
/// its storage: those uses write through an address.
#[derive(Clone, Debug)]
pub(super) struct Split {
    pub(super) ty: Type,
    pub(super) leaves: Vec<SplitLeaf>,
}

/// One scalar word of a split local: its field path, its byte offset in
/// the local's layout, its type and its variable.
#[derive(Clone, Debug)]
pub(super) struct SplitLeaf {
    pub(super) path: Vec<String>,
    pub(super) offset: i32,
    pub(super) ty: Type,
    pub(super) var: Variable,
    /// Whether any use reads this word: a `let` loads only live words.
    pub(super) live: bool,
}

/// The most words a split local may have.
const MAX_SPLIT_WORDS: usize = 16;

/// The scalar words of `ty` (path, offset, type) when it is a struct or
/// tuple of scalars that may be split ([`Split`]).
pub(super) fn split_words(world: &World, ty: &Type) -> Option<Vec<(Vec<String>, i32, Type)>> {
    fn product(world: &World, ty: &Type) -> bool {
        (matches!(ty, Type::Tuple(_))
            || matches!(ty, Type::Named(n, _) if world.adts.contains_key(n)))
            && !crate::seq::is_slice_type(ty)
            && !crate::dyn_trait::is_object(ty)
            && !crate::strings::is_text(ty)
            && crate::vector::layout_words(ty).is_none()
    }
    fn walk(
        world: &World,
        layout: &Layout,
        path: &mut Vec<String>,
        base: i32,
        out: &mut Vec<(Vec<String>, i32, Type)>,
    ) -> bool {
        match &layout.kind {
            LayoutKind::Scalar => {
                // A unit field has no word to keep, so its reads and writes
                // would have nothing to name: such aggregates stay in memory.
                let ty = crate::numeric::default_type(&layout.ty);
                let name = ty.name();
                let word = matches!(&ty, Type::Named(_, args) if args.is_empty())
                    && (crate::numeric::is_integer_name(name)
                        || matches!(name, "f32" | "f64" | "bool"));
                if word {
                    out.push((path.clone(), base, ty));
                }
                word
            }
            LayoutKind::Product(fs) => {
                product(world, &layout.ty)
                    && fs.iter().all(|f| {
                        path.push(f.name.clone());
                        let ok = walk(world, &f.layout, path, base + f.offset, out);
                        path.pop();
                        ok
                    })
            }
            LayoutKind::Enum(_) => false,
        }
    }
    let layout = Layout::of(world, ty).ok()?;
    if !layout.aggregate() || !matches!(layout.kind, LayoutKind::Product(_)) {
        return None;
    }
    let mut out = vec![];
    (walk(world, &layout, &mut vec![], 0, &mut out)
        && !out.is_empty()
        && out.len() <= MAX_SPLIT_WORDS)
        .then_some(out)
}

/// The locals of `f` that are split into word variables ([`Split`]), with
/// the field paths their uses read (an empty path reads them whole).
///
/// A local qualifies when it is a struct or tuple of scalar words that no
/// use writes through an address. It is split when that is cheaper by a
/// simple count, weighting uses in loops by [`LOOP_WEIGHT`]: in memory,
/// each field read or write is a load or store, and a `Some(h)` binding of
/// a sequence element copies the element; split, each `let` loads the
/// words some use reads, and each read of an aggregate field or of the
/// whole local that is not built in place (a call argument, a binding of a
/// local kept in memory) stores its words into fresh storage. A local
/// that no use names a field of is never split: it is cheaper as the
/// storage it was bound to.
pub(super) fn split_locals(
    world: &World,
    f: &FunctionIr,
    generics: &BTreeMap<String, Type>,
) -> BTreeMap<String, Vec<Vec<String>>> {
    let ty = |t: &Type| crate::numeric::default_type(&substitute(t, generics));
    let mut memory = BTreeSet::new();
    let mut reads: BTreeMap<String, Vec<Vec<String>>> = BTreeMap::new();
    let mut fields = BTreeSet::new();
    for inst in f.blocks.iter().flat_map(|b| &b.instructions) {
        match &inst.op {
            Op::Read { root, path, .. } => {
                reads.entry(root.clone()).or_default().push(path.clone());
                if !path.is_empty() {
                    fields.insert(root.clone());
                }
            }
            Op::Write { place, op, .. } => {
                let mut parts = place.split('.');
                let root = parts.next().unwrap_or("").to_owned();
                let path: Vec<String> = parts.map(str::to_owned).collect();
                if !path.is_empty() {
                    fields.insert(root.clone());
                }
                // A compound assignment reads the word it writes.
                if op != "=" {
                    reads.entry(root).or_default().push(path);
                }
            }
            // A call reads an `in` or `sink` place argument as a value (a
            // direct call) or through a fresh copy (`Codegen::address`);
            // only `inout` writes through the place.
            Op::Call { args, .. } | Op::Indirect { args, .. } => {
                for a in args {
                    if a.pass == Pass::Inout
                        && let Some(p) = &a.place
                    {
                        memory.insert(p.root.clone());
                    }
                }
            }
            Op::Seq { args, .. } | Op::Fip { args, .. } | Op::Tiled { args, .. } => memory.extend(
                args.iter()
                    .filter_map(|a| a.place.as_ref())
                    .map(|p| p.root.clone()),
            ),
            Op::Closure {
                expr,
                aliases,
                moves,
            }
            | Op::Handle {
                expr,
                aliases,
                moves,
                ..
            } => {
                memory.extend(crate::check::free_names(expr));
                memory.extend(aliases.keys().cloned());
                memory.extend(aliases.values().cloned());
                memory.extend(moves.iter().map(|p| p.root.clone()));
            }
            _ => {}
        }
    }
    // An element view's binding (§13) names the element in its buffer.
    if !world.element_views.is_empty() {
        let views: BTreeSet<ValueId> = f
            .blocks
            .iter()
            .flat_map(|b| &b.instructions)
            .filter(|i| {
                matches!(i.op, Op::Seq { .. })
                    && world
                        .element_views
                        .contains_key(&(i.span.start, i.span.end))
            })
            .map(|i| i.dest)
            .collect();
        for inst in f.blocks.iter().flat_map(|b| &b.instructions) {
            if let Op::Bind { pattern, value } = &inst.op
                && views.contains(value)
            {
                memory.extend(crate::check::pattern_names(pattern));
            }
        }
    }
    let candidates: BTreeMap<String, Vec<(Vec<String>, i32, Type)>> = f
        .locals
        .iter()
        .filter(|(n, _)| {
            !memory.contains(*n)
                && fields.contains(*n)
                && !f.params.iter().any(|p| &p.name == *n)
                && !f.external_captures.contains(*n)
        })
        .filter_map(|(n, t)| Some((n.clone(), split_words(world, &ty(t))?)))
        .collect();
    let live = |n: &str| -> usize {
        let paths = reads.get(n).map(Vec::as_slice).unwrap_or_default();
        candidates[n]
            .iter()
            .filter(|(path, _, _)| {
                paths
                    .iter()
                    .any(|r| path.starts_with(r) || r.starts_with(path))
            })
            .count()
    };
    let words_under = |n: &str, path: &[String]| -> usize {
        candidates[n]
            .iter()
            .filter(|(p, _, _)| p.starts_with(path))
            .count()
    };
    let cyclic = cyclic_blocks(f);
    let elements: BTreeSet<ValueId> = f
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .filter(|i| {
            matches!(
                &i.op,
                Op::Seq {
                    op: crate::seq::SeqOp::TryGet
                        | crate::seq::SeqOp::Hoisted(crate::seq::Access::TryGet),
                    ..
                }
            )
        })
        .map(|i| i.dest)
        .collect();
    let mut in_memory: BTreeMap<&str, usize> = BTreeMap::new();
    let mut split: BTreeMap<&str, usize> = BTreeMap::new();
    for (b, block) in f.blocks.iter().enumerate() {
        let weight = if cyclic.contains(&b) { LOOP_WEIGHT } else { 1 };
        for (at, inst) in block.instructions.iter().enumerate() {
            match &inst.op {
                Op::Read { root, path, .. } if candidates.contains_key(root) => {
                    let aggregate = Layout::of(world, &ty(&inst.ty)).is_ok_and(|l| l.aggregate());
                    if !aggregate {
                        *in_memory.entry(root).or_default() += weight;
                        continue;
                    }
                    let consumer = block.instructions[at + 1..]
                        .iter()
                        .find(|later| crate::ir_inline::operands(&later.op).contains(&inst.dest));
                    let built_in_place = match consumer {
                        Some(later) => match &later.op {
                            Op::Data(..) | Op::Tuple(_) | Op::Exit { .. } => true,
                            Op::Bind { pattern, .. } => matches!(
                                &pattern.kind,
                                PatternKind::Bind(n) if candidates.contains_key(n)
                            ),
                            _ => false,
                        },
                        None => {
                            matches!(&block.terminator, Terminator::Return(v) if *v == inst.dest)
                        }
                    };
                    if !built_in_place {
                        *split.entry(root).or_default() += weight * words_under(root, path);
                    }
                }
                Op::Write { place, .. } => {
                    let root = place.split('.').next().unwrap_or("");
                    if let Some((name, _)) = candidates.get_key_value(root)
                        && place.contains('.')
                    {
                        *in_memory.entry(name).or_default() += weight;
                    }
                }
                Op::Bind { pattern, value } => {
                    let mut bound = BTreeSet::new();
                    pattern_binds(pattern, &mut bound);
                    // A sequence element bound by `Some(h)` is copied out of
                    // the buffer when `h` is kept in memory.
                    let element = elements.contains(value);
                    for n in bound {
                        if let Some((name, words)) = candidates.get_key_value(&n) {
                            *split.entry(name).or_default() += weight * live(&n);
                            if element {
                                *in_memory.entry(name).or_default() += weight * words.len();
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    candidates
        .keys()
        .filter(|n| {
            split.get(n.as_str()).copied().unwrap_or(0)
                <= in_memory.get(n.as_str()).copied().unwrap_or(0)
        })
        .map(|n| (n.clone(), reads.get(n).cloned().unwrap_or_default()))
        .collect()
}

/// How much more a use inside a loop counts than one outside
/// ([`split_locals`]).
pub(super) const LOOP_WEIGHT: usize = 8;

/// The blocks of `f` on a cycle: loop headers and bodies.
pub(super) fn cyclic_blocks(f: &FunctionIr) -> BTreeSet<usize> {
    let mut graph = petgraph::graphmap::DiGraphMap::<usize, ()>::new();
    for (b, block) in f.blocks.iter().enumerate() {
        graph.add_node(b);
        let targets: Vec<usize> = match &block.terminator {
            Terminator::Jump { block, .. } => vec![*block],
            Terminator::Branch { yes, no, .. } => vec![*yes, *no],
            Terminator::Match { arms, .. } => arms.iter().map(|(_, b)| *b).collect(),
            _ => vec![],
        };
        for t in targets {
            graph.add_edge(b, t, ());
        }
    }
    petgraph::algo::tarjan_scc(&graph)
        .into_iter()
        .filter(|c| c.len() > 1 || graph.contains_edge(c[0], c[0]))
        .flatten()
        .collect()
}

impl<M: Module> Codegen<'_, '_, M> {
    /// Declares the word variables of the split local whose own variable is
    /// `base`, of type `ty`, all zero ([`Split`]).
    pub(super) fn split_declare(
        &mut self,
        ty: &Type,
        base: Variable,
        reads: &[Vec<String>],
    ) -> Result<(), String> {
        let words = split_words(self.world, ty).ok_or("native split local lacks words")?;
        let mut leaves = Vec::with_capacity(words.len());
        for (path, offset, ty) in words {
            let live = reads
                .iter()
                .any(|r| path.starts_with(r) || r.starts_with(&path));
            let native = scalar(&ty, &[])?;
            let var = self.b.declare_var(native);
            let zero = self.scalar_zero(native);
            self.def_var(var, zero);
            leaves.push(SplitLeaf {
                path,
                offset,
                ty,
                var,
                live,
            });
        }
        self.split.insert(
            base.as_u32(),
            Split {
                ty: ty.clone(),
                leaves,
            },
        );
        Ok(())
    }
    /// The split local `name`, when the name still denotes it.
    pub(super) fn split_of(&self, name: &str) -> Option<&Split> {
        self.split.get(&self.vars.get(name)?.as_u32())
    }
    /// The root and field path of place expression `e` when its root is a
    /// split local.
    pub(super) fn split_place(&self, e: &Expr) -> Option<(String, Vec<String>)> {
        let p = crate::check::Place::from_expr(e)?;
        self.split_of(&p.root)?;
        Some((p.root, p.path))
    }
    /// The words of split local `root` under field path `path`, with their
    /// offsets relative to that field, and the field's type.
    #[allow(clippy::type_complexity)]
    fn split_subtree(
        &self,
        root: &str,
        path: &[String],
    ) -> Result<(Type, Vec<(i32, Type, Variable, bool)>), String> {
        let s = self.split_of(root).ok_or("native split local is unknown")?;
        let mut ty = s.ty.clone();
        let mut base = 0;
        for field in path {
            let layout = Layout::of(self.world, &ty)?;
            let f = layout
                .field(field)
                .ok_or_else(|| format!("unknown native field '{field}' in {ty}"))?;
            base += f.offset;
            ty = crate::numeric::default_type(&f.ty);
        }
        let words = s
            .leaves
            .iter()
            .filter(|l| l.path.starts_with(path))
            .map(|l| (l.offset - base, l.ty.clone(), l.var, l.live))
            .collect();
        Ok((ty, words))
    }
    /// Reads field `path` of split local `root`: a scalar field's variable,
    /// or the words of an aggregate field stored into `into` (fresh storage
    /// when `None`).
    pub(super) fn split_read(
        &mut self,
        root: &str,
        path: &[String],
        into: Option<ir::Value>,
    ) -> Result<ir::Value, String> {
        let (ty, words) = self.split_subtree(root, path)?;
        let layout = Layout::of(self.world, &ty)?;
        if !layout.aggregate() {
            let (_, _, var, _) = words.first().ok_or("native split field lacks a word")?;
            return Ok(self.use_var(*var));
        }
        let ptr = match into {
            Some(ptr) => ptr,
            // Every word is written when the words cover the layout with
            // full-width stores; an `f32` or narrow store writes part of
            // its word, so such storage is zeroed first.
            None if plain_data(self.world, &ty)
                && words.len() as u32 * 8 == layout.size
                && words
                    .iter()
                    .all(|(_, t, _, _)| scalar(t, &[]).is_ok_and(|n| n.bytes() == 8)) =>
            {
                self.alloc_uninit(layout.size)
            }
            None => self.alloc(layout.size),
        };
        for (offset, ty, var, _) in words {
            let value = self.use_var(var);
            let at = self.offset(ptr, offset);
            self.store(at, value, &ty)?;
        }
        Ok(ptr)
    }
    /// Writes `value` (a scalar, or the address of an aggregate) to field
    /// `path` of split local `root`.
    pub(super) fn split_write(
        &mut self,
        root: &str,
        path: &[String],
        value: ir::Value,
    ) -> Result<(), String> {
        let (ty, words) = self.split_subtree(root, path)?;
        if !Layout::of(self.world, &ty)?.aggregate() {
            let (_, ty, var, _) = words.first().ok_or("native split field lacks a word")?;
            let value = self.coerce(value, Some(ty));
            self.def_var(*var, value);
            return Ok(());
        }
        // `value` addresses a whole aggregate (a binding's value, a
        // sequence element after its bounds check, a fresh copy), so the
        // loads cannot trap. A word no use reads is not loaded: its
        // variable is only ever written before it is read again.
        for (offset, ty, var, live) in words {
            if !live {
                continue;
            }
            let word = self
                .b
                .ins()
                .load(scalar(&ty, &[])?, ir::MemFlags::trusted(), value, offset);
            self.def_var(var, word);
        }
        Ok(())
    }
    /// Binds split local `to` to a copy of split local `from` (of the same
    /// type) word by word, with no storage between them. False when the
    /// two are not both split locals of one type.
    pub(super) fn split_copy(
        &mut self,
        from: &str,
        path: &[String],
        to: &str,
    ) -> Result<bool, String> {
        let (Some(_), Some(target)) = (self.split_of(from), self.split_of(to)) else {
            return Ok(false);
        };
        let target = target.clone();
        let (ty, words) = self.split_subtree(from, path)?;
        if ty != target.ty || words.len() != target.leaves.len() {
            return Ok(false);
        }
        for ((_, _, source, _), leaf) in words.into_iter().zip(&target.leaves) {
            let value = self.use_var(source);
            self.def_var(leaf.var, value);
        }
        Ok(true)
    }
    /// The variable and type of the scalar field `path` of split local
    /// `root`, when that field is one word.
    pub(super) fn split_word(&self, root: &str, path: &[String]) -> Option<(Variable, Type)> {
        let s = self.split_of(root)?;
        s.leaves
            .iter()
            .find(|l| l.path == path)
            .map(|l| (l.var, l.ty.clone()))
    }
}

/// The aggregate reads of `f` whose value the next instruction binds to a
/// split local: the `let` loads the words at once, so the read lends the
/// source's storage instead of copying it first.
pub(super) fn split_sources(
    f: &FunctionIr,
    split: &BTreeMap<String, Vec<Vec<String>>>,
) -> BTreeMap<ValueId, String> {
    let mut uses: BTreeMap<ValueId, usize> = BTreeMap::new();
    for block in &f.blocks {
        for inst in &block.instructions {
            for id in crate::ir_inline::operands(&inst.op) {
                *uses.entry(id).or_default() += 1;
            }
        }
        for id in crate::ir_inline::terminator_operands(&block.terminator) {
            *uses.entry(id).or_default() += 1;
        }
    }
    let mut out = BTreeMap::new();
    for block in &f.blocks {
        for pair in block.instructions.windows(2) {
            if let (Op::Read { .. }, Op::Bind { pattern, value }) = (&pair[0].op, &pair[1].op)
                && *value == pair[0].dest
                && uses.get(value) == Some(&1)
                && let PatternKind::Bind(n) = &pattern.kind
                && split.contains_key(n)
            {
                out.insert(pair[0].dest, n.clone());
            }
        }
    }
    out
}

/// A small enum kept as words instead of storage (SH5 lane 7, #1330).
///
/// An `Option[u64]` built by a constructor in each arm of a `match`, joined
/// through a block parameter and matched again (the result of an inlined
/// `with_ref`, or the hand-written `let r = match .. { Some(v) => Some(..),
/// None => None }`) lives in memory without this: each arm builds the value
/// in a slot, the join passes the slot's address, a `let` copies it and the
/// next `match` loads it back. [`scalar_enums`] rewrites such values into
/// their words: the tag (the variant index, as in memory) and one word per
/// field of every variant. A constructor becomes its tag constant and its
/// field values, a block parameter becomes one parameter per word, a local
/// one local per word, and a `match` tests the tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct EnumShape {
    /// The word types: the tag (`u64`) first, then each variant's fields in
    /// layout order.
    pub(super) words: Vec<Type>,
    /// Each variant in layout order: its name and its fields' names, words
    /// and byte offsets in the enum's storage.
    pub(super) variants: Vec<(String, Vec<(String, usize, i32)>)>,
}

impl EnumShape {
    /// The index and fields of variant `name` (a `Type::Variant` or a bare
    /// variant name).
    fn variant(&self, name: &str) -> Option<(usize, &[(String, usize, i32)])> {
        let short = name.rsplit("::").next()?;
        self.variants
            .iter()
            .enumerate()
            .find(|(_, (n, _))| n == short)
            .map(|(i, (_, fs))| (i, fs.as_slice()))
    }
}

/// The most words a scalar enum may have, its tag included.
const MAX_ENUM_WORDS: usize = 4;

/// Whether `ty` is a single integer or `bool` word.
fn word_type(ty: &Type) -> bool {
    let name = ty.name();
    matches!(ty, Type::Named(_, args) if args.is_empty())
        && ((crate::numeric::is_integer_name(name) && !crate::numeric::is_wide_name(name))
            || name == "bool")
}

/// The words of enum type `ty` when every field of every variant is one
/// integer or `bool` word and there are at most [`MAX_ENUM_WORDS`] of them.
pub(super) fn enum_shape(world: &World, ty: &Type) -> Option<EnumShape> {
    let ty = crate::numeric::default_type(ty);
    if crate::fip::mentions_box(world, &ty) {
        return None;
    }
    let layout = Layout::of(world, &ty).ok()?;
    let LayoutKind::Enum(vs) = &layout.kind else {
        return None;
    };
    let mut words = vec![Type::Named("u64".into(), vec![])];
    let mut variants = Vec::with_capacity(vs.len());
    for v in vs {
        let mut fields = Vec::with_capacity(v.fields.len());
        for f in &v.fields {
            let t = crate::numeric::default_type(&f.ty);
            if !word_type(&t) {
                return None;
            }
            words.push(t);
            fields.push((f.name.clone(), words.len() - 1, f.offset));
        }
        variants.push((v.name.clone(), fields));
    }
    (words.len() <= MAX_ENUM_WORDS).then_some(EnumShape { words, variants })
}

/// A graph whose small enum values are words ([`EnumShape`]).
pub(super) struct ScalarEnums {
    pub(super) f: FunctionIr,
    /// Calls whose enum result the caller keeps as words: the shape and the
    /// values that receive the words ([`Codegen::scalar_call_words`]).
    pub(super) calls: BTreeMap<ValueId, (EnumShape, Vec<ValueId>)>,
    /// Returned values that are words, for a graph whose join takes words.
    pub(super) returns: BTreeMap<ValueId, Vec<ValueId>>,
}

#[derive(Clone, Copy)]
enum Def {
    Inst(usize, usize),
    Param(usize, usize),
}

#[derive(Clone, Copy)]
enum Use {
    Inst(usize, usize),
    Term(usize),
}

/// Whether the subpatterns of constructor pattern `fields` only bind or
/// ignore words of variant `name` of `shape`.
fn word_fields(shape: &EnumShape, name: &str, fields: &[(String, Pattern)]) -> bool {
    let Some((_, fs)) = shape.variant(name) else {
        return false;
    };
    fields.iter().all(|(n, p)| {
        fs.iter().any(|(f, _, _)| f == n)
            && matches!(p.kind, PatternKind::Bind(_) | PatternKind::Wild)
    })
}

/// Rewrites the small enum values of graph `f` into words ([`EnumShape`]),
/// or `None` when no value qualifies.
///
/// A value qualifies when it is defined by a constructor, by a whole read
/// of a qualifying local, by a call of a function the native path
/// specializes into its caller (`specialized`), or by a block parameter
/// every incoming edge passes a qualifying value; and when every use binds
/// it to a qualifying local, passes it to a qualifying parameter, matches
/// it, binds its fields, or (when `returns` gives this graph's join words of
/// the same shape) returns it. A local qualifies when it is not a
/// parameter, not shared with an enclosing graph (`shared`), and every
/// mention binds or reads it whole with a qualifying value. Anything else
/// (a field path, a write, a place argument, a closure or handler that
/// names it) keeps the old lowering for the whole connected set.
///
/// Graphs with `Box` values or FIP slots keep the old lowering: their drop
/// plans index instructions and slots.
pub(super) fn scalar_enums(
    world: &World,
    f: &FunctionIr,
    generics: &BTreeMap<String, Type>,
    returns: Option<&EnumShape>,
    specialized: &dyn Fn(&str) -> bool,
    shared: &BTreeSet<String>,
) -> Option<ScalarEnums> {
    if !f.fip_call_slots.is_empty()
        || !f.fip_join_slots.is_empty()
        || !f.box_drops_before_inst.is_empty()
        || !f.box_drops_after_inst.is_empty()
        || !f.box_drops_before_term.is_empty()
    {
        return None;
    }
    let ty = |t: &Type| crate::numeric::default_type(&substitute(t, generics));
    let mut shapes: BTreeMap<String, Option<EnumShape>> = BTreeMap::new();
    let mut shape_of = |t: &Type| -> Option<EnumShape> {
        let t = ty(t);
        shapes
            .entry(t.to_string())
            .or_insert_with(|| enum_shape(world, &t))
            .clone()
    };
    let mut cv: BTreeMap<ValueId, EnumShape> = f
        .values
        .iter()
        .filter_map(|(id, t)| Some((*id, shape_of(t)?)))
        .collect();
    if cv.is_empty() {
        return None;
    }
    if f.values
        .values()
        .chain(f.locals.values())
        .any(|t| crate::fip::mentions_box(world, &ty(t)))
    {
        return None;
    }
    let mut cl: BTreeMap<String, EnumShape> = f
        .locals
        .iter()
        .filter(|(n, _)| {
            !f.params.iter().any(|p| &p.name == *n)
                && !f.external_captures.contains(*n)
                && !shared.contains(*n)
        })
        .filter_map(|(n, t)| Some((n.clone(), shape_of(t)?)))
        .collect();
    // Locals some mention keeps in memory.
    let mut banned = BTreeSet::new();
    let mut defs = BTreeMap::new();
    let mut uses: BTreeMap<ValueId, Vec<Use>> = BTreeMap::new();
    let mut preds: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (b, block) in f.blocks.iter().enumerate() {
        for (k, p) in block.params.iter().enumerate() {
            defs.insert(*p, Def::Param(b, k));
        }
        for (i, inst) in block.instructions.iter().enumerate() {
            defs.insert(inst.dest, Def::Inst(b, i));
            for id in crate::ir_inline::operands(&inst.op) {
                uses.entry(id).or_default().push(Use::Inst(b, i));
            }
            let places = |args: &[crate::ir::Argument], banned: &mut BTreeSet<String>| {
                banned.extend(
                    args.iter()
                        .filter_map(|a| a.place.as_ref())
                        .map(|p| p.root.clone()),
                );
            };
            match &inst.op {
                Op::Read {
                    root,
                    path,
                    projections,
                    ..
                } if !path.is_empty() || !projections.is_empty() => {
                    banned.insert(root.clone());
                }
                Op::Write { place, .. } => {
                    banned.insert(place.split('.').next().unwrap_or(place).to_owned());
                }
                Op::Bind { pattern, .. } if !matches!(pattern.kind, PatternKind::Bind(_)) => {
                    pattern_binds(pattern, &mut banned);
                }
                Op::Call { args, .. }
                | Op::Indirect { args, .. }
                | Op::Seq { args, .. }
                | Op::Fip { args, .. }
                | Op::Tiled { args, .. } => places(args, &mut banned),
                Op::Closure {
                    expr,
                    aliases,
                    moves,
                }
                | Op::Handle {
                    expr,
                    aliases,
                    moves,
                    ..
                } => {
                    banned.extend(crate::check::free_names(expr));
                    banned.extend(aliases.keys().cloned());
                    banned.extend(aliases.values().cloned());
                    banned.extend(moves.iter().map(|p| p.root.clone()));
                }
                _ => {}
            }
        }
        for id in crate::ir_inline::terminator_operands(&block.terminator) {
            uses.entry(id).or_default().push(Use::Term(b));
        }
        let targets: Vec<usize> = match &block.terminator {
            Terminator::Jump { block, .. } => vec![*block],
            Terminator::Branch { yes, no, .. } => vec![*yes, *no],
            Terminator::Match { arms, .. } => arms.iter().map(|(_, b)| *b).collect(),
            _ => vec![],
        };
        for t in targets {
            preds.entry(t).or_default().push(b);
        }
    }
    cl.retain(|n, _| !banned.contains(n));
    let inst = |b: usize, i: usize| &f.blocks[b].instructions[i];
    loop {
        let mut drop_values = vec![];
        for (v, shape) in &cv {
            let def_ok = match defs.get(v) {
                Some(Def::Inst(b, i)) => match &inst(*b, *i).op {
                    Op::Data(name, fields) => shape.variant(name).is_some_and(|(_, fs)| {
                        fs.len() == fields.len()
                            && fs.iter().all(|(n, _, _)| fields.iter().any(|(m, _)| m == n))
                    }),
                    Op::Read { root, .. } => cl.get(root) == Some(shape),
                    Op::Call { callee, .. } => specialized(callee),
                    _ => false,
                },
                Some(Def::Param(b, k)) => preds.get(b).is_some_and(|ps| {
                    ps.iter().all(|p| match &f.blocks[*p].terminator {
                        Terminator::Jump { args, .. } => {
                            args.get(*k).is_some_and(|a| cv.get(a) == Some(shape))
                        }
                        _ => false,
                    })
                }),
                None => false,
            };
            let uses_ok = def_ok
                && uses.get(v).into_iter().flatten().all(|u| match u {
                    Use::Inst(b, i) => match &inst(*b, *i).op {
                        Op::Bind { pattern, .. } => match &pattern.kind {
                            PatternKind::Bind(n) => cl.get(n) == Some(shape),
                            PatternKind::Wild => true,
                            PatternKind::Constructor { name, fields, .. } => {
                                word_fields(shape, name, fields)
                            }
                            _ => false,
                        },
                        _ => false,
                    },
                    Use::Term(b) => match &f.blocks[*b].terminator {
                        Terminator::Jump { block, args } => args
                            .iter()
                            .zip(&f.blocks[*block].params)
                            .filter(|(a, _)| *a == v)
                            .all(|(_, p)| cv.get(p) == Some(shape)),
                        Terminator::Match { arms, .. } => arms.iter().all(|(p, _)| match &p.kind {
                            PatternKind::Wild => true,
                            PatternKind::Constructor { name, fields, .. } => {
                                word_fields(shape, name, fields)
                            }
                            _ => false,
                        }),
                        Terminator::Return(_) => returns == Some(shape),
                        _ => false,
                    },
                });
            if !uses_ok {
                drop_values.push(*v);
            }
        }
        let mut drop_locals = vec![];
        for (b, block) in f.blocks.iter().enumerate() {
            for (i, inst) in block.instructions.iter().enumerate() {
                let _ = (b, i);
                match &inst.op {
                    Op::Bind {
                        pattern:
                            Pattern {
                                kind: PatternKind::Bind(n),
                                ..
                            },
                        value,
                    } if cl.contains_key(n) && !cv.contains_key(value) => {
                        drop_locals.push(n.clone());
                    }
                    Op::Read { root, .. }
                        if cl.contains_key(root) && !cv.contains_key(&inst.dest) =>
                    {
                        drop_locals.push(root.clone());
                    }
                    _ => {}
                }
            }
        }
        if drop_values.is_empty() && drop_locals.is_empty() {
            break;
        }
        for v in drop_values {
            cv.remove(&v);
        }
        for n in drop_locals {
            cl.remove(&n);
        }
    }
    if cv.is_empty() {
        return None;
    }
    // Only a value that some `match` or field binding takes apart is worth
    // its words; a set that is only passed along keeps its storage.
    let taken_apart = cv.keys().any(|v| {
        uses.get(v).into_iter().flatten().any(|u| match u {
            Use::Inst(b, i) => matches!(
                &inst(*b, *i).op,
                Op::Bind { pattern, .. } if matches!(pattern.kind, PatternKind::Constructor { .. })
            ),
            Use::Term(b) => matches!(f.blocks[*b].terminator, Terminator::Match { .. }),
        })
    });
    if !taken_apart && returns.is_none() {
        return None;
    }
    let mut g = f.clone();
    let mut next = f
        .values
        .keys()
        .copied()
        .chain(
            f.blocks
                .iter()
                .flat_map(|b| b.params.iter().copied().chain(b.instructions.iter().map(|i| i.dest))),
        )
        .max()
        .map_or(0, |m| m + 1);
    let mut fresh = |t: &Type, values: &mut BTreeMap<ValueId, Type>| {
        let id = next;
        next += 1;
        values.insert(id, t.clone());
        id
    };
    let unit = Type::unit();
    // The words of each value: a constructor's tag constant and field
    // values (zero constants for the other variants' words), else fresh.
    let mut words: BTreeMap<ValueId, Vec<ValueId>> = BTreeMap::new();
    for (v, shape) in &cv {
        let ws = match defs.get(v) {
            Some(Def::Inst(b, i)) => match &inst(*b, *i).op {
                Op::Data(name, fields) => {
                    let (_, fs) = shape.variant(name)?;
                    shape
                        .words
                        .iter()
                        .enumerate()
                        .map(|(w, t)| {
                            fs.iter()
                                .find(|(_, word, _)| *word == w)
                                .and_then(|(n, _, _)| fields.iter().find(|(m, _)| m == n))
                                .map(|(_, id)| *id)
                                .unwrap_or_else(|| fresh(t, &mut g.values))
                        })
                        .collect()
                }
                _ => shape.words.iter().map(|t| fresh(t, &mut g.values)).collect(),
            },
            _ => shape.words.iter().map(|t| fresh(t, &mut g.values)).collect(),
        };
        words.insert(*v, ws);
    }
    let local = |n: &str, w: usize| format!("{n}$word{w}");
    for (n, shape) in &cl {
        g.locals.remove(n);
        for (w, t) in shape.words.iter().enumerate() {
            g.locals.insert(local(n, w), t.clone());
        }
    }
    let mut calls = BTreeMap::new();
    let mut returned = BTreeMap::new();
    for (b, block) in f.blocks.iter().enumerate() {
        let mut out = Vec::with_capacity(block.instructions.len());
        let derived = |i: &crate::ir::Instruction, dest, op, ty: &Type| crate::ir::Instruction {
            dest,
            op,
            ty: ty.clone(),
            span: 0..0,
            token: i.token,
            fallible: false,
        };
        for i in &block.instructions {
            match &i.op {
                Op::Data(name, _) if cv.contains_key(&i.dest) => {
                    let shape = &cv[&i.dest];
                    let (tag, fs) = shape.variant(name)?;
                    for (w, id) in words[&i.dest].iter().enumerate() {
                        let literal = if w == 0 {
                            Some(Literal::Int(tag as i128))
                        } else if fs.iter().any(|(_, word, _)| *word == w) {
                            None
                        } else if shape.words[w].name() == "bool" {
                            Some(Literal::Bool(false))
                        } else {
                            Some(Literal::Int(0))
                        };
                        if let Some(l) = literal {
                            out.push(derived(i, *id, Op::Constant(l), &shape.words[w]));
                        }
                    }
                }
                Op::Read { root, pass, .. } if cv.contains_key(&i.dest) => {
                    let shape = &cv[&i.dest];
                    for (w, id) in words[&i.dest].iter().enumerate() {
                        let mut read = derived(
                            i,
                            *id,
                            Op::Read {
                                root: local(root, w),
                                path: vec![],
                                pass: *pass,
                                projections: vec![],
                            },
                            &shape.words[w],
                        );
                        read.span = i.span.clone();
                        out.push(read);
                    }
                }
                Op::Call { .. } if cv.contains_key(&i.dest) => {
                    calls.insert(i.dest, (cv[&i.dest].clone(), words[&i.dest].clone()));
                    out.push(i.clone());
                }
                Op::Bind { pattern, value } if cv.contains_key(value) => {
                    let shape = &cv[value];
                    let ws = &words[value];
                    let mut binds = vec![];
                    match &pattern.kind {
                        PatternKind::Bind(n) => {
                            for (w, id) in ws.iter().enumerate() {
                                binds.push((local(n, w), *id, pattern.span.clone()));
                            }
                        }
                        PatternKind::Constructor { name, fields, .. } => {
                            let (_, fs) = shape.variant(name)?;
                            for (n, p) in fields {
                                if let PatternKind::Bind(x) = &p.kind {
                                    let (_, w, _) = fs.iter().find(|(m, _, _)| m == n)?;
                                    binds.push((x.clone(), ws[*w], p.span.clone()));
                                }
                            }
                        }
                        _ => {}
                    }
                    if binds.is_empty() {
                        out.push(derived(i, i.dest, Op::Unit, &i.ty));
                    }
                    for (k, (n, id, span)) in binds.into_iter().enumerate() {
                        let dest = if k == 0 {
                            i.dest
                        } else {
                            fresh(&unit, &mut g.values)
                        };
                        let mut bind = derived(
                            i,
                            dest,
                            Op::Bind {
                                pattern: Pattern {
                                    kind: PatternKind::Bind(n),
                                    span,
                                },
                                value: id,
                            },
                            &i.ty,
                        );
                        bind.span = i.span.clone();
                        out.push(bind);
                    }
                }
                _ => out.push(i.clone()),
            }
        }
        let nb = &mut g.blocks[b];
        nb.instructions = out;
        nb.params = block
            .params
            .iter()
            .flat_map(|p| match words.get(p) {
                Some(ws) if cv.contains_key(p) => ws.clone(),
                _ => vec![*p],
            })
            .collect();
        let expand = |args: &[ValueId]| -> Vec<ValueId> {
            args.iter()
                .flat_map(|a| match words.get(a) {
                    Some(ws) if cv.contains_key(a) => ws.clone(),
                    _ => vec![*a],
                })
                .collect()
        };
        nb.terminator = match &block.terminator {
            Terminator::Jump { block, args } => Terminator::Jump {
                block: *block,
                args: expand(args),
            },
            Terminator::Match { value, arms } if cv.contains_key(value) => {
                let shape = &cv[value];
                let last = arms.len().saturating_sub(1);
                Terminator::Match {
                    value: words[value][0],
                    arms: arms
                        .iter()
                        .enumerate()
                        .map(|(k, (p, target))| {
                            let kind = match &p.kind {
                                PatternKind::Constructor { name, .. } if k != last => {
                                    PatternKind::Literal(Literal::Int(
                                        shape.variant(name).map_or(0, |(t, _)| t) as i128,
                                    ))
                                }
                                _ => PatternKind::Wild,
                            };
                            (
                                Pattern {
                                    kind,
                                    span: p.span.clone(),
                                },
                                *target,
                            )
                        })
                        .collect(),
                }
            }
            Terminator::Return(v) if cv.contains_key(v) => {
                returned.insert(*v, words[v].clone());
                Terminator::Return(*v)
            }
            t => t.clone(),
        };
    }
    Some(ScalarEnums {
        f: g,
        calls,
        returns: returned,
    })
}

/// The words of a small enum on their way across a specialized call
/// ([`EnumShape`]).
#[derive(Default)]
pub(super) struct ScalarEnumState {
    /// Set by a caller that keeps the next call's result as words.
    pub(super) request: Option<EnumShape>,
    /// The words the specialized call passed through its join.
    pub(super) result: Option<Vec<ir::Value>>,
    /// The shape whose words the next nested graph passes to its join.
    pub(super) join: Option<EnumShape>,
    /// Variables holding the address of an aggregate that nothing can
    /// change for the whole call of the function being emitted: an `in`
    /// parameter it never rebinds, or a specialized callee's `in`
    /// parameter bound to such an address (#1330). Header words read
    /// through them are invariant ([`crate::bounds_hoist::stable_views`]).
    pub(super) invariant_roots: BTreeSet<u32>,
    /// Values read whole from an invariant root, which a specialized call
    /// may bind to its `in` parameters.
    pub(super) invariant_pointers: BTreeSet<ir::Value>,
}

impl<M: Module> Codegen<'_, '_, M> {
    /// The words of the enum of `shape` stored at `ptr`: the tag, then
    /// each field at its offset. The words of other variants than the one
    /// stored are unspecified (§16.6), as in memory.
    pub(super) fn enum_words(
        &mut self,
        ptr: ir::Value,
        shape: &EnumShape,
    ) -> Result<Vec<ir::Value>, String> {
        let mut out = vec![
            self.b
                .ins()
                .load(types::I64, ir::MemFlags::trusted(), ptr, 0),
        ];
        for (w, ty) in shape.words.iter().enumerate().skip(1) {
            let offset = shape
                .variants
                .iter()
                .flat_map(|(_, fs)| fs)
                .find(|(_, word, _)| *word == w)
                .map(|(_, _, o)| *o)
                .ok_or("native enum word lacks a field")?;
            let at = self.offset(ptr, offset);
            out.push(self.load(at, ty)?);
        }
        Ok(out)
    }
}
