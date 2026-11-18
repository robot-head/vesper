//! What a callee costs at one call site (port speed wave, lane 5, #1025).
//!
//! [`fold`] runs a small sparse conditional constant propagation over the
//! callee with the site's known arguments: integer and `bool` constants and
//! nullary constructors of enums that own nothing, passed by value. It
//! finds the branches and matches those arguments decide and the blocks
//! control can still reach. [`Folded::cost`] is the callee's [`cost`] over
//! those blocks, and the inlined copy drops the rest (see `splice`).
//!
//! [`split`] finds a partial inlining of a callee that is still over budget:
//! a pure head (reads, compares and binds of values that own nothing) ending in a branch whose one
//! side is the expensive part. The copy keeps the head and the cheap side
//! and calls the whole callee again on the expensive side. The head has no
//! effects, so running it twice is unobservable.
use super::{Binding, cost_parts, pattern_binds, place_of};
use crate::{
    ast::*,
    check::World,
    ir::{FunctionIr, Instruction, Op, Terminator, ValueId},
};
use std::collections::{BTreeMap, BTreeSet};

/// A value the site's arguments determine.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Known {
    Int(i128),
    Bool(bool),
    /// A nullary constructor of an enum that owns nothing, by short name.
    Tag(String),
}

fn short(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}

/// What the site's known arguments decide in a callee.
#[derive(Debug, Clone)]
pub(super) struct Folded {
    /// Blocks control can reach.
    pub live: Vec<bool>,
    /// Block -> the one successor its `Branch` or `Match` takes. A `Match`
    /// is only decided when the arm it takes binds no names.
    pub jumps: BTreeMap<usize, usize>,
    /// Values the arguments determine: their operations fold to constants.
    known: BTreeSet<ValueId>,
}

impl Folded {
    /// Nothing decided: every block live.
    pub fn none(f: &FunctionIr) -> Self {
        Self {
            live: vec![true; f.blocks.len()],
            jumps: BTreeMap::new(),
            known: BTreeSet::new(),
        }
    }

    /// Whether this folding changes anything.
    pub fn decides(&self) -> bool {
        !self.jumps.is_empty() || self.live.iter().any(|l| !l)
    }

    /// Successors of block `b` under this folding.
    pub fn successors(&self, f: &FunctionIr, b: usize) -> Vec<usize> {
        if let Some(s) = self.jumps.get(&b) {
            return vec![*s];
        }
        successors(&f.blocks[b].terminator)
    }

    /// [`cost`](super::cost) of the blocks in `blocks` under this folding.
    pub fn cost_of(&self, world: &World, f: &FunctionIr, blocks: &[bool]) -> usize {
        let free = cost_parts(world, f);
        f.blocks
            .iter()
            .enumerate()
            .filter(|(b, _)| blocks[*b])
            .map(|(b, block)| {
                let work = block
                    .instructions
                    .iter()
                    .filter(|i| {
                        !free(i)
                            && !(self.known.contains(&i.dest)
                                && matches!(
                                    i.op,
                                    Op::Binary(..) | Op::Unary(..) | Op::Read { .. } | Op::Data(..)
                                ))
                    })
                    .count();
                let jump = matches!(block.terminator, Terminator::Jump { .. })
                    || self.jumps.contains_key(&b);
                work + usize::from(!(work == 0 && jump))
            })
            .sum()
    }

    /// The callee's cost at this site.
    pub fn cost(&self, world: &World, f: &FunctionIr) -> usize {
        self.cost_of(world, f, &self.live)
    }
}

pub(super) fn successors(t: &Terminator) -> Vec<usize> {
    match t {
        Terminator::Jump { block, .. } => vec![*block],
        Terminator::Branch { yes, no, .. } => vec![*yes, *no],
        Terminator::Match { arms, .. } => arms.iter().map(|(_, b)| *b).collect(),
        Terminator::Return(_) | Terminator::Unreachable => vec![],
    }
}

/// A type whose values own nothing: copying one is the same as moving it,
/// and dropping one is a no-op. `Ordering`, field-less enums and structs of
/// integers are; sequences, strings, boxes and functions are not.
pub(super) fn plain(world: &World, t: &Type) -> bool {
    !crate::fip::may_need_drop(world, t)
        && !crate::fip::mentions_box(world, t)
        && !crate::fip::type_contains_box(world, t)
        && !crate::native::contains_function(world, t)
        && crate::layout::Layout::of(world, t).is_ok()
}

/// Locals of `f` that hold one value for the whole call: bound once (or a
/// parameter never re-bound), never written, never lent `inout`, and no
/// handle in `f` could reach them through its aliases.
fn stable_locals(f: &FunctionIr) -> BTreeMap<String, usize> {
    let mut binds = BTreeMap::<String, usize>::new();
    let mut unstable = BTreeSet::new();
    for p in &f.params {
        binds.insert(p.name.clone(), 1);
    }
    for b in &f.blocks {
        for i in &b.instructions {
            match &i.op {
                Op::Bind { pattern, .. } => {
                    let mut names = BTreeSet::new();
                    pattern_binds(pattern, &mut names);
                    for n in names {
                        *binds.entry(n).or_default() += 1;
                    }
                }
                Op::Write { place, .. } => {
                    unstable.insert(place_of(place).root);
                }
                Op::Call { args, .. }
                | Op::Seq { args, .. }
                | Op::Tiled { args, .. }
                | Op::Fip { args, .. }
                | Op::Indirect { args, .. } => {
                    for a in args {
                        if a.pass == Pass::Inout
                            && let Some(p) = &a.place
                        {
                            unstable.insert(p.root.clone());
                        }
                    }
                }
                Op::Handle { .. } | Op::Closure { .. } => return BTreeMap::new(),
                _ => {}
            }
        }
        if let Terminator::Match { arms, .. } = &b.terminator {
            for (p, _) in arms {
                let mut names = BTreeSet::new();
                pattern_binds(p, &mut names);
                for n in names {
                    *binds.entry(n).or_default() += 1;
                }
            }
        }
    }
    binds.retain(|n, count| *count == 1 && !unstable.contains(n));
    binds
}

/// The known by-value arguments of a call: the caller's constants and
/// nullary constructors, keyed by the callee's parameter name.
fn seeds(
    world: &World,
    caller: &FunctionIr,
    callee: &FunctionIr,
    bindings: &[Binding],
) -> BTreeMap<String, Known> {
    let wanted = bindings
        .iter()
        .filter_map(|b| match b {
            Binding::Bind(v) => Some(*v),
            Binding::Place(_) => None,
        })
        .collect::<BTreeSet<_>>();
    if wanted.is_empty() {
        return BTreeMap::new();
    }
    let mut defs = BTreeMap::new();
    for i in caller.blocks.iter().flat_map(|b| &b.instructions) {
        if wanted.contains(&i.dest) {
            defs.insert(i.dest, i);
        }
    }
    let mut out = BTreeMap::new();
    for (p, b) in callee.params.iter().zip(bindings) {
        let Binding::Bind(v) = b else { continue };
        if let Some(k) = defs.get(v).and_then(|i| constant(world, i)) {
            out.insert(p.name.clone(), k);
        }
    }
    out
}

fn constant(world: &World, i: &Instruction) -> Option<Known> {
    match &i.op {
        Op::Constant(Literal::Int(n)) => Some(Known::Int(*n)),
        Op::Constant(Literal::Bool(b)) => Some(Known::Bool(*b)),
        Op::Data(name, fields) if fields.is_empty() && plain(world, &i.ty) => {
            Some(Known::Tag(short(name).to_owned()))
        }
        _ => None,
    }
}

fn binary(op: &str, a: &Known, b: &Known) -> Option<Known> {
    use std::cmp::Ordering::*;
    let order = match (a, b) {
        (Known::Int(x), Known::Int(y)) => x.cmp(y),
        (Known::Bool(x), Known::Bool(y)) if matches!(op, "==" | "!=") => x.cmp(y),
        (Known::Tag(x), Known::Tag(y)) if matches!(op, "==" | "!=") => {
            if x == y {
                Equal
            } else {
                Less
            }
        }
        _ => return None,
    };
    Some(Known::Bool(match op {
        "==" => order == Equal,
        "!=" => order != Equal,
        "<" => order == Less,
        "<=" => order != Greater,
        ">" => order == Greater,
        ">=" => order != Less,
        _ => return None,
    }))
}

/// The arm of a `match` that `value` takes, if the arms decide it.
fn arm(arms: &[(Pattern, usize)], value: &Known) -> Option<usize> {
    for (index, (p, _)) in arms.iter().enumerate() {
        let hit = match (&p.kind, value) {
            (PatternKind::Wild | PatternKind::Bind(_), _) => true,
            (PatternKind::Constructor { name, .. }, Known::Tag(t)) => short(name) == t,
            (PatternKind::Literal(Literal::Int(n)), Known::Int(v)) => n == v,
            (PatternKind::Literal(Literal::Bool(n)), Known::Bool(v)) => n == v,
            // A pattern this folding does not understand: undecided.
            _ => return None,
        };
        if hit {
            return Some(index);
        }
    }
    None
}

/// One round of [`fold`]: live blocks, decided terminators, known values.
type Round = (Vec<bool>, BTreeMap<usize, usize>, BTreeMap<ValueId, Known>);

/// Fold `callee` with the known arguments of this site. `None` when the
/// propagation does not settle (it always does on the graphs the IR
/// produces; the bound is a guard).
pub(super) fn fold(
    world: &World,
    caller: &FunctionIr,
    callee: &FunctionIr,
    bindings: &[Binding],
) -> Option<Folded> {
    let seeds = seeds(world, caller, callee, bindings);
    if seeds.is_empty() {
        return None;
    }
    let stable = stable_locals(callee);
    let seeds = seeds
        .into_iter()
        .filter(|(n, _)| stable.contains_key(n))
        .collect::<BTreeMap<_, _>>();
    if seeds.is_empty() {
        return None;
    }
    let n = callee.blocks.len();
    // Values flowing into block parameters, from the previous round.
    let mut params = BTreeMap::<ValueId, Known>::new();
    let mut previous: Option<Round> = None;
    for _round in 0..32 {
        let mut locals = seeds.clone();
        let mut values = params.clone();
        let mut live = vec![false; n];
        let mut jumps = BTreeMap::new();
        let mut incoming = BTreeMap::<usize, Vec<Vec<ValueId>>>::new();
        live[0] = true;
        let mut work = vec![0usize];
        let mut seen = BTreeSet::new();
        // Visit live blocks; a block's instructions are evaluated once per
        // round, in discovery order. Values used before their block was
        // evaluated in this round are simply unknown (pessimistic), and the
        // next round sees them.
        while let Some(b) = work.pop() {
            if !seen.insert(b) {
                continue;
            }
            let block = &callee.blocks[b];
            for i in &block.instructions {
                let k = match &i.op {
                    Op::Constant(_) | Op::Data(..) => constant(world, i),
                    Op::Read { root, path, .. } if path.is_empty() => locals.get(root).cloned(),
                    Op::Unary(op, v) if op == "!" => match values.get(v) {
                        Some(Known::Bool(b)) => Some(Known::Bool(!b)),
                        _ => None,
                    },
                    Op::Binary(op, a, b) if !i.fallible => match (values.get(a), values.get(b)) {
                        (Some(x), Some(y)) => binary(op, x, y),
                        _ => None,
                    },
                    Op::Bind { pattern, value } => {
                        if let PatternKind::Bind(name) = &pattern.kind
                            && stable.contains_key(name)
                            && let Some(k) = values.get(value)
                        {
                            locals.insert(name.clone(), k.clone());
                        }
                        None
                    }
                    _ => None,
                };
                if let Some(k) = k {
                    values.insert(i.dest, k);
                }
                if matches!(i.op, Op::Exit { .. }) {
                    break;
                }
            }
            let ends = block
                .instructions
                .iter()
                .any(|i| matches!(i.op, Op::Exit { .. }));
            let next = if ends {
                vec![]
            } else {
                match &block.terminator {
                    Terminator::Branch { condition, yes, no } => match values.get(condition) {
                        Some(Known::Bool(true)) => {
                            jumps.insert(b, *yes);
                            vec![*yes]
                        }
                        Some(Known::Bool(false)) => {
                            jumps.insert(b, *no);
                            vec![*no]
                        }
                        _ => vec![*yes, *no],
                    },
                    Terminator::Match { value, arms } => {
                        let taken = values.get(value).and_then(|k| arm(arms, k)).filter(|a| {
                            let mut names = BTreeSet::new();
                            pattern_binds(&arms[*a].0, &mut names);
                            names.is_empty()
                        });
                        match taken {
                            Some(a) => {
                                jumps.insert(b, arms[a].1);
                                vec![arms[a].1]
                            }
                            None => arms.iter().map(|(_, t)| *t).collect(),
                        }
                    }
                    Terminator::Jump { block: t, args } => {
                        incoming.entry(*t).or_default().push(args.clone());
                        vec![*t]
                    }
                    Terminator::Return(_) | Terminator::Unreachable => vec![],
                }
            };
            for s in next {
                live[s] = true;
                work.push(s);
            }
        }
        // Block parameters: known when every live predecessor passes the
        // same known value.
        let mut next_params = BTreeMap::new();
        for (b, block) in callee.blocks.iter().enumerate() {
            if !live[b] || block.params.is_empty() {
                continue;
            }
            // A parameter block reached by a `Branch`/`Match` edge is not a
            // shape the IR builds; leave such a block undecided.
            let Some(edges) = incoming.get(&b) else {
                continue;
            };
            for (index, p) in block.params.iter().enumerate() {
                let mut agreed: Option<Known> = None;
                let mut ok = true;
                for args in edges {
                    match args.get(index).and_then(|v| values.get(v)) {
                        Some(k) if agreed.as_ref().is_none_or(|a| a == k) => {
                            agreed = Some(k.clone());
                        }
                        _ => ok = false,
                    }
                }
                if ok && let Some(k) = agreed {
                    next_params.insert(*p, k);
                }
            }
        }
        let state = (live, jumps, values);
        if previous.as_ref() == Some(&state) && next_params == params {
            let (live, jumps, values) = state;
            return Some(Folded {
                live,
                jumps,
                known: values.into_keys().collect(),
            });
        }
        previous = Some(state);
        params = next_params;
    }
    None
}

/// A partial inlining of a callee: inline everything but the blocks only
/// `cold` reaches, and call the whole callee there instead.
#[derive(Debug, Clone)]
pub(super) struct Split {
    /// The block whose terminator leads to the cold side.
    pub block: usize,
    /// The cold successor of `block`.
    pub cold: usize,
    /// Blocks the inlined copy keeps.
    pub hot: Vec<bool>,
    /// Cost of the copy: the hot blocks and the call.
    pub hot_cost: usize,
    /// Cost of the blocks left to the call.
    pub cold_cost: usize,
}

/// Instructions a head may run twice (once inlined, once in the call).
fn pure(world: &World, f: &FunctionIr, i: &Instruction) -> bool {
    if i.fallible {
        return false;
    }
    match &i.op {
        Op::Constant(_) | Op::Unit => true,
        Op::Read { .. }
        | Op::Binary(..)
        | Op::Unary(..)
        | Op::Project(..)
        | Op::Data(..)
        | Op::Tuple(_) => plain(world, &i.ty),
        // Any pattern over a plain value only binds or tests it (a derived
        // `cmp` binds `Equal` on each field compare).
        Op::Bind { value, .. } => f.values.get(value).is_some_and(|t| plain(world, t)),
        _ => false,
    }
}

/// Smallest cold side worth a call.
const MIN_COLD: usize = 16;
/// What the call and its continuation jump add to the copy.
const CALL_COST: usize = 2;

fn reach(f: &FunctionIr, folded: &Folded, from: usize, cut: Option<(usize, usize)>) -> Vec<bool> {
    let mut seen = vec![false; f.blocks.len()];
    let mut work = vec![from];
    while let Some(b) = work.pop() {
        if seen[b] || !folded.live[b] {
            continue;
        }
        seen[b] = true;
        for s in folded.successors(f, b) {
            if cut != Some((b, s)) {
                work.push(s);
            }
        }
    }
    seen
}

/// The best partial inlining of `f` at a site folded as `folded` whose
/// copy costs at most `budget`, if any.
pub(super) fn split(
    world: &World,
    f: &FunctionIr,
    folded: &Folded,
    budget: usize,
) -> Option<Split> {
    if f.blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .any(|i| matches!(i.op, Op::Handle { .. }))
    {
        return None;
    }
    let n = f.blocks.len();
    let pure_block = (0..n)
        .map(|b| f.blocks[b].instructions.iter().all(|i| pure(world, f, i)))
        .collect::<Vec<_>>();
    // Blocks every path to which runs only pure blocks: the entry if pure,
    // then blocks all of whose live predecessors are such blocks. A loop
    // header never joins: its back edge comes from a block not yet in.
    let mut preds = vec![Vec::new(); n];
    for b in 0..n {
        if folded.live[b] {
            for s in folded.successors(f, b) {
                preds[s].push(b);
            }
        }
    }
    let mut head = vec![false; n];
    head[0] = pure_block[0] && folded.live[0];
    let mut changed = true;
    while changed {
        changed = false;
        for b in 1..n {
            if !head[b]
                && folded.live[b]
                && pure_block[b]
                && !preds[b].is_empty()
                && preds[b].iter().all(|p| head[*p])
            {
                head[b] = true;
                changed = true;
            }
        }
    }
    let heavy = |blocks: &[bool]| {
        blocks.iter().enumerate().any(|(b, on)| {
            *on && f.blocks[b]
                .instructions
                .iter()
                .any(|i| matches!(i.op, Op::Loop | Op::Call { .. } | Op::Seq { .. }))
        })
    };
    let mut best: Option<(bool, usize, Split)> = None;
    for b in 0..n {
        if !head[b] || folded.jumps.contains_key(&b) {
            continue;
        }
        let targets = match &f.blocks[b].terminator {
            Terminator::Branch { yes, no, .. } if yes != no => vec![*yes, *no],
            Terminator::Match { arms, .. } => {
                let mut ts = arms.iter().map(|(_, t)| *t).collect::<Vec<_>>();
                ts.sort_unstable();
                ts.dedup();
                // Each arm target must be reached by exactly one arm, so
                // cutting the edge cuts one arm.
                if ts.len() != arms.len() {
                    continue;
                }
                ts
            }
            _ => continue,
        };
        for cold in targets {
            if !f.blocks[cold].params.is_empty() {
                continue;
            }
            let hot = reach(f, folded, 0, Some((b, cold)));
            if hot[cold] {
                continue;
            }
            let from_cold = reach(f, folded, cold, None);
            let cold_blocks = from_cold
                .iter()
                .zip(&hot)
                .map(|(c, h)| *c && !h)
                .collect::<Vec<_>>();
            let hot_cost = folded.cost_of(world, f, &hot) + CALL_COST;
            let cold_cost = folded.cost_of(world, f, &cold_blocks);
            if hot_cost > budget || cold_cost < MIN_COLD || cold_cost < hot_cost {
                continue;
            }
            let key = (heavy(&cold_blocks) && !heavy(&hot), hot_cost);
            if best.as_ref().is_none_or(|(h, c, _)| key > (*h, *c)) {
                best = Some((
                    key.0,
                    key.1,
                    Split {
                        block: b,
                        cold,
                        hot,
                        hot_cost,
                        cold_cost,
                    },
                ));
            }
        }
    }
    best.map(|(_, _, s)| s)
}
