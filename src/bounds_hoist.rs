//! Bounds-check hoisting for counted loops (Stage 1 performance wave, lane 3).
//!
//! A native-path Place IR pass that runs only when the optimizer is on,
//! after `ir_opt::optimize` and drop elaboration. It never runs for the
//! verified backend or the evaluator, so it cannot change a verified
//! artifact or the reference semantics.
//!
//! For a `for i in lo..hi` (or `lo..=hi`) loop whose body indexes a sequence
//! or slice `s` with the loop variable, `s[i]`, `s.set(i, v)`,
//! `s.try_get(i)` and `s.try_set(i, v)` check `i < s.len()` on every
//! iteration. When nothing in the loop can change the length of `s`, the
//! pass versions the loop:
//!
//! ```text
//! preheader: ...; if bounds_guard(lo, hi, s..) { fast loop } else { loop }
//! ```
//!
//! The guard holds when the range is empty or `hi <= s.len()` (`hi <
//! s.len()` for `..=`) for every hoisted `s`; the loop index only takes
//! values in `lo..hi`, so every hoisted access is then in bounds and the
//! fast copy uses [`SeqOp::Hoisted`] accesses without a check. When the guard
//! fails, the original, checked loop runs unchanged, so a short sequence
//! faults at the same iteration with the same prior effects.
//!
//! The pass is conservative. It skips a loop that contains a closure, a
//! handler or an indirect call, carries `@fip` reuse bookkeeping, lets
//! a value defined inside it escape, or is entered other than through its
//! header. It hoists only sequences named by a place that no instruction in
//! the loop writes, rebinds, moves or passes other than `in`, except element
//! accesses (`get`/`set`/`try_get`/`try_set`), which never resize, and
//! whose boxes no drop in the loop reclaims. A loop that owns boxes keeps
//! its drops: the fast copy reclaims them at the same points.
use crate::ast::{Pass, PatternKind, Type};
use crate::check::Place;
use crate::fip::FipDrop;
use crate::ir::{Argument, FunctionIr, Instruction, Op, Program, Terminator, ValueId};
use crate::seq::{Access, IndexForm, SeqOp};
use std::collections::{BTreeMap, BTreeSet};

mod facts;

/// Versions every eligible counted loop of `program`; returns how many.
pub fn run(program: &mut Program) -> usize {
    program.functions.values_mut().map(hoist_function).sum()
}

/// Versions the eligible loops of one function.
pub fn hoist_function(f: &mut FunctionIr) -> usize {
    // Accesses that dominating comparisons prove in bounds need no guard.
    facts::drop_compared_checks(f);
    // Inner loops first: versioning a loop copies its body, nested loops
    // and all, with the same value ids, and a loop whose values a copy
    // outside it uses is not versioned.
    let preds = reached_preds(f);
    let idom = idoms(f, &preds);
    let mut headers: Vec<(usize, usize)> = (0..f.blocks.len())
        .filter(|&h| header(f, h).is_some())
        .map(|h| (natural_loop(h, &preds, &idom).len(), h))
        .collect();
    headers.sort();
    // Every loop of the function (counted or not), to tell inner loops.
    let loops: Vec<(usize, BTreeSet<usize>)> = (0..f.blocks.len())
        .filter(|&h| preds[h].iter().any(|&p| dominates(&idom, h, p)))
        .map(|h| (h, natural_loop(h, &preds, &idom)))
        .collect();
    let versioned = headers
        .into_iter()
        .filter(|&(_, h)| {
            // The innermost loop around this one.
            let enclosing = loops
                .iter()
                .filter(|(o, l)| *o != h && l.contains(&h))
                .min_by_key(|(_, l)| l.len())
                .map(|(_, l)| l.clone());
            version(f, h, enclosing.as_ref())
        })
        .count();
    // Loops bounded by a compared, loop-invariant end the function cannot
    // relate to the length.
    let versioned = versioned + facts::version_compared_loops(f);
    drop_dominated_checks(f);
    versioned
}

/// The index of an access, for comparing accesses: the same value, or a
/// constant.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum IndexKey {
    Value(ValueId),
    Const(i128),
}

/// Drops the bounds check of each element access that a checked access
/// (`s[i]`, `s.set(i, v)`) of the same sequence dominates, at the same index
/// or, for constant indices, at a larger or equal one; returns how many.
///
/// The checked access returned, so its index was below the length then;
/// the sequence must be a place that nothing in the function can resize,
/// rebind or lend other than `in` ([`stable_place`]), so the length has not
/// changed since. An index that is the same IR value is the same number: a
/// value's definition dominates every use, so no new definition runs
/// between the dominating access and this one.
pub fn drop_dominated_checks(f: &mut FunctionIr) -> usize {
    let preds = reached_preds(f);
    let idom = idoms(f, &preds);
    let reached = reached(f);
    let all: BTreeSet<usize> = (0..f.blocks.len()).collect();
    let defs: BTreeMap<ValueId, Instruction> = f
        .blocks
        .iter()
        .flat_map(|b| b.instructions.iter())
        .map(|i| (i.dest, i.clone()))
        .collect();
    let mut bound: BTreeMap<String, ValueId> = BTreeMap::new();
    for i in f.blocks.iter().flat_map(|b| b.instructions.iter()) {
        if let Op::Bind { pattern, value } = &i.op
            && let PatternKind::Bind(name) = &pattern.kind
        {
            bound.insert(name.clone(), *value);
        }
    }
    let key = |mut v: ValueId| -> IndexKey {
        for _ in 0..MAX_DEPTH {
            match defs.get(&v).map(|i| &i.op) {
                Some(Op::Constant(crate::ast::Literal::Int(n))) if *n >= 0 => {
                    return IndexKey::Const(*n);
                }
                Some(Op::Read { root, path, .. })
                    if path.is_empty() && bound.contains_key(root) && single_binding(f, root) =>
                {
                    v = bound[root];
                }
                _ => break,
            }
        }
        IndexKey::Value(v)
    };
    // Each access: (block, instruction, place, index, checked).
    let mut accesses: BTreeMap<usize, Vec<(usize, Place, IndexKey, bool)>> = BTreeMap::new();
    let mut stable: BTreeMap<Place, bool> = BTreeMap::new();
    for &b in &reached {
        for (k, i) in f.blocks[b].instructions.iter().enumerate() {
            let Op::Seq { op, args } = &i.op else {
                continue;
            };
            let (Some(_), Some(receiver), Some(index)) =
                (hoistable(*op), args.first(), args.get(1))
            else {
                continue;
            };
            let Some(place) = &receiver.place else {
                continue;
            };
            let read_ok = matches!(defs.get(&receiver.value).map(|r| &r.op),
                Some(Op::Read { root, path, .. }) if *root == place.root && *path == place.path);
            if !read_ok
                || !*stable
                    .entry(place.clone())
                    .or_insert_with(|| stable_place(f, &all, place))
            {
                continue;
            }
            let checked = matches!(
                op,
                SeqOp::Get | SeqOp::Set | SeqOp::SliceGet | SeqOp::SliceSet
            );
            accesses
                .entry(b)
                .or_default()
                .push((k, place.clone(), key(index.value), checked));
        }
    }
    let covers = |facts: &[(Place, IndexKey)], place: &Place, at: IndexKey| {
        facts.iter().any(|(p, k)| {
            p == place
                && match (*k, at) {
                    (IndexKey::Const(a), IndexKey::Const(b)) => a >= b,
                    (a, b) => a == b,
                }
        })
    };
    let mut dropped = vec![];
    for (&b, list) in &accesses {
        // Facts of the blocks that strictly dominate `b`.
        let mut facts: Vec<(Place, IndexKey)> = vec![];
        let mut up = idom[b];
        while let Some(d) = up {
            for (_, p, k, checked) in accesses.get(&d).into_iter().flatten() {
                if *checked {
                    facts.push((p.clone(), *k));
                }
            }
            up = idom[d];
        }
        for (k, place, index, checked) in list {
            if covers(&facts, place, *index) {
                dropped.push((b, *k));
            } else if *checked {
                facts.push((place.clone(), *index));
            }
        }
    }
    for &(b, k) in &dropped {
        let i = &mut f.blocks[b].instructions[k];
        if let Op::Seq { op, .. } = &mut i.op
            && let Some(access) = hoistable(*op)
        {
            *op = SeqOp::Hoisted(access);
            i.fallible = false;
        }
    }
    dropped.len()
}

/// The counted-loop header shape [`crate::loops`] lowers `for` to.
struct Header {
    index: String,
    bound: String,
    inclusive: bool,
    body: usize,
}

fn header(f: &FunctionIr, h: usize) -> Option<Header> {
    let b = &f.blocks[h];
    let [first, i, n, test] = b.instructions.as_slice() else {
        return None;
    };
    if !matches!(first.op, Op::Loop) {
        return None;
    }
    let slot = |inst: &Instruction, suffix: &str| match &inst.op {
        Op::Read { root, path, .. }
            if path.is_empty()
                && root.starts_with("$loop")
                && uninlined(root).ends_with(suffix) =>
        {
            Some(root.clone())
        }
        _ => None,
    };
    let index = slot(i, "_index")?;
    let bound = slot(n, "_bound")?;
    let Op::Binary(op, a, c) = &test.op else {
        return None;
    };
    let inclusive = match op.as_str() {
        "<" => false,
        "<=" => true,
        _ => return None,
    };
    let u64_value = |v: ValueId| {
        f.values
            .get(&v)
            .is_some_and(|t| crate::numeric::default_type(t) == Type::named("u64"))
    };
    if *a != i.dest || *c != n.dest || !u64_value(i.dest) || !u64_value(n.dest) {
        return None;
    }
    let Terminator::Branch { condition, yes, .. } = b.terminator else {
        return None;
    };
    (condition == test.dest).then_some(Header {
        index,
        bound,
        inclusive,
        body: yes,
    })
}

/// `name` without the `$inl<n>` suffixes the inliner gives the locals of an
/// inlined body.
fn uninlined(mut name: &str) -> &str {
    while let Some((head, tail)) = name.rsplit_once("$inl") {
        if tail.is_empty() || !tail.bytes().all(|b| b.is_ascii_digit()) {
            break;
        }
        name = head;
    }
    name
}

fn successors(t: &Terminator) -> Vec<usize> {
    match t {
        Terminator::Jump { block, .. } => vec![*block],
        Terminator::Branch { yes, no, .. } => vec![*yes, *no],
        Terminator::Match { arms, .. } => arms.iter().map(|(_, b)| *b).collect(),
        Terminator::Return(_) | Terminator::Unreachable => vec![],
    }
}

/// The blocks the entry reaches.
fn reached(f: &FunctionIr) -> BTreeSet<usize> {
    let mut reached = BTreeSet::new();
    if f.blocks.is_empty() {
        return reached;
    }
    reached.insert(0);
    let mut stack = vec![0];
    while let Some(b) = stack.pop() {
        for s in successors(&f.blocks[b].terminator) {
            if s < f.blocks.len() && reached.insert(s) {
                stack.push(s);
            }
        }
    }
    reached
}

/// Predecessors of each block among the blocks the entry reaches (a block
/// after a `break` or `return` has none and enters nothing).
fn reached_preds(f: &FunctionIr) -> Vec<Vec<usize>> {
    let mut preds = vec![vec![]; f.blocks.len()];
    for b in reached(f) {
        for s in successors(&f.blocks[b].terminator) {
            if let Some(p) = preds.get_mut(s) {
                p.push(b);
            }
        }
    }
    preds
}

/// The value operands of `op`, mutably.
fn arguments(args: &mut [Argument]) -> Vec<&mut ValueId> {
    args.iter_mut().map(|a| &mut a.value).collect()
}

fn operands(op: &mut Op) -> Vec<&mut ValueId> {
    match op {
        Op::Constant(_) | Op::Unit | Op::Read { .. } | Op::Loop | Op::Closure { .. } => vec![],
        Op::Bind { value, .. }
        | Op::Write { value, .. }
        | Op::Unary(_, value)
        | Op::Try(value)
        | Op::Project(value, _)
        | Op::Exit { value, .. } => vec![value],
        Op::Binary(_, a, b) | Op::Index(a, b) => vec![a, b],
        Op::Tuple(vs) | Op::Vector { args: vs, .. } => vs.iter_mut().collect(),
        Op::Data(_, fields) => fields.iter_mut().map(|(_, v)| v).collect(),
        Op::Call { args: a, .. }
        | Op::Tiled { args: a, .. }
        | Op::Seq { args: a, .. }
        | Op::Fip { args: a, .. } => arguments(a),
        Op::Indirect { callee, args: a } => {
            let mut out: Vec<&mut ValueId> = vec![callee];
            out.extend(a.iter_mut().map(|a| &mut a.value));
            out
        }
        Op::Handle { state_value, .. } => state_value.iter_mut().collect(),
    }
}

fn terminator_operands(t: &mut Terminator) -> Vec<&mut ValueId> {
    match t {
        Terminator::Return(v) => vec![v],
        Terminator::Jump { args, .. } => args.iter_mut().collect(),
        Terminator::Branch { condition, .. } => vec![condition],
        Terminator::Match { value, .. } => vec![value],
        Terminator::Unreachable => vec![],
    }
}

fn uses(op: &Op) -> Vec<ValueId> {
    operands(&mut op.clone()).into_iter().map(|v| *v).collect()
}

fn terminator_uses(t: &Terminator) -> Vec<ValueId> {
    terminator_operands(&mut t.clone())
        .into_iter()
        .map(|v| *v)
        .collect()
}

/// The immediate dominator of each block the entry reaches (`None` for the
/// entry and for unreached blocks), from `preds` of [`reached_preds`].
fn idoms(f: &FunctionIr, preds: &[Vec<usize>]) -> Vec<Option<usize>> {
    let n = f.blocks.len();
    let mut idom = vec![None; n];
    if n == 0 {
        return idom;
    }
    // Reverse postorder from the entry.
    let mut post = vec![];
    let mut seen = vec![false; n];
    let mut stack = vec![(0usize, 0usize)];
    seen[0] = true;
    while let Some((b, k)) = stack.pop() {
        let succ = successors(&f.blocks[b].terminator);
        if let Some(&s) = succ.get(k) {
            stack.push((b, k + 1));
            if s < n && !seen[s] {
                seen[s] = true;
                stack.push((s, 0));
            }
        } else {
            post.push(b);
        }
    }
    let mut order = vec![usize::MAX; n];
    for (k, &b) in post.iter().enumerate() {
        order[b] = k;
    }
    idom[0] = Some(0);
    let intersect = |idom: &[Option<usize>], mut a: usize, mut b: usize| {
        while a != b {
            while order[a] < order[b] {
                a = idom[a].unwrap_or(0);
            }
            while order[b] < order[a] {
                b = idom[b].unwrap_or(0);
            }
        }
        a
    };
    let mut changed = true;
    while changed {
        changed = false;
        for &b in post.iter().rev().skip(1) {
            let mut new: Option<usize> = None;
            for &p in &preds[b] {
                if idom[p].is_none() {
                    continue;
                }
                new = Some(match new {
                    None => p,
                    Some(q) => intersect(&idom, p, q),
                });
            }
            if new.is_some() && idom[b] != new {
                idom[b] = new;
                changed = true;
            }
        }
    }
    idom[0] = None;
    idom
}

/// Whether block `a` dominates block `b` (both reached).
fn dominates(idom: &[Option<usize>], a: usize, mut b: usize) -> bool {
    loop {
        if a == b {
            return true;
        }
        match idom.get(b).copied().flatten() {
            Some(up) => b = up,
            None => return false,
        }
    }
}

/// The blocks of the natural loop of header `h`, with `h`: the blocks that
/// reach a back edge into `h` (from a block `h` dominates) without passing
/// through `h`. An enclosing loop's blocks are not part of it.
fn natural_loop(h: usize, preds: &[Vec<usize>], idom: &[Option<usize>]) -> BTreeSet<usize> {
    let mut blocks = BTreeSet::from([h]);
    let mut stack: Vec<usize> = preds[h]
        .iter()
        .copied()
        .filter(|&p| dominates(idom, h, p))
        .collect();
    while let Some(b) = stack.pop() {
        if blocks.insert(b) {
            stack.extend(preds[b].iter().copied());
        }
    }
    blocks
}

fn binds(pattern: &crate::ast::Pattern, name: &str) -> bool {
    match &pattern.kind {
        PatternKind::Bind(n) => n == name,
        PatternKind::Tuple(ps) => ps.iter().any(|p| binds(p, name)),
        PatternKind::Constructor { fields, .. } => fields.iter().any(|(_, p)| binds(p, name)),
        PatternKind::Wild | PatternKind::Literal(_) => false,
    }
}

fn place_root(place: &str) -> &str {
    place.split('.').next().unwrap_or(place)
}

/// The access a checked element operation becomes in the fast copy.
fn hoistable(op: SeqOp) -> Option<Access> {
    Some(match op {
        SeqOp::Get | SeqOp::SliceGet => Access::Get,
        SeqOp::Set | SeqOp::SliceSet => Access::Set,
        SeqOp::TryGet => Access::TryGet,
        SeqOp::TrySet => Access::TrySet,
        _ => None?,
    })
}

/// Operations on a sequence that never change its length.
fn keeps_length(op: SeqOp) -> bool {
    hoistable(op).is_some()
        || matches!(
            op,
            SeqOp::Len | SeqOp::SliceLen | SeqOp::AsSlice | SeqOp::SubSlice
        )
}

/// Versions the loop headed by `h` when it is eligible.
fn version(f: &mut FunctionIr, h: usize, enclosing: Option<&BTreeSet<usize>>) -> bool {
    let Some(head) = header(f, h) else {
        return false;
    };
    // Predecessors among the blocks the entry reaches: the inliner and
    // `break` leave unreachable blocks whose edges enter nothing.
    let preds = reached_preds(f);
    if preds[h].is_empty() {
        return false;
    }
    let idom = idoms(f, &preds);
    let blocks = natural_loop(h, &preds, &idom);
    if !blocks.contains(&head.body) {
        return false;
    }
    // One preheader, entering through the header only.
    let outside: Vec<usize> = preds[h]
        .iter()
        .copied()
        .filter(|p| !blocks.contains(p))
        .collect();
    let [pre] = outside.as_slice() else {
        return false;
    };
    let pre = *pre;
    if !matches!(&f.blocks[pre].terminator, Terminator::Jump { block, args } if *block == h && args.is_empty())
    {
        return false;
    }
    for &b in &blocks {
        if b != h && preds[b].iter().any(|p| !blocks.contains(p)) {
            return false;
        }
        if f.fip_join_slots.contains_key(&b) {
            return false;
        }
    }
    let insts = || {
        blocks
            .iter()
            .flat_map(|&b| f.blocks[b].instructions.iter().map(move |i| (b, i)))
    };
    if insts().any(|(_, i)| {
        matches!(
            i.op,
            Op::Closure { .. } | Op::Handle { .. } | Op::Indirect { .. }
        ) || f.fip_call_slots.contains_key(&i.dest)
    }) {
        return false;
    }
    // No value defined in the loop is used outside it.
    let mut defined: BTreeSet<ValueId> = insts().map(|(_, i)| i.dest).collect();
    for &b in &blocks {
        defined.extend(f.blocks[b].params.iter().copied());
    }
    for (b, block) in f.blocks.iter().enumerate() {
        if blocks.contains(&b) {
            continue;
        }
        let used = block
            .instructions
            .iter()
            .flat_map(|i| uses(&i.op))
            .chain(terminator_uses(&block.terminator));
        if used.into_iter().any(|v| defined.contains(&v)) {
            return false;
        }
    }
    // The bound never changes; the index changes only in latch blocks that
    // jump straight back to the header.
    let mut latches = BTreeSet::new();
    for (b, i) in insts() {
        match &i.op {
            Op::Write { place, .. } if place_root(place) == head.bound => return false,
            Op::Write { place, .. } if place_root(place) == head.index => {
                if !matches!(f.blocks[b].terminator, Terminator::Jump { block, .. } if block == h) {
                    return false;
                }
                latches.insert(b);
            }
            Op::Bind { pattern, .. }
                if binds(pattern, &head.index) || binds(pattern, &head.bound) =>
            {
                return false;
            }
            _ => {}
        }
    }
    // Values that hold an index of the range: reads of the index slot after
    // the header test, and reads of a local bound once from one.
    let mut induction: BTreeSet<ValueId> = BTreeSet::new();
    for (b, i) in insts() {
        if let Op::Read { root, path, .. } = &i.op
            && b != h
            && path.is_empty()
            && *root == head.index
        {
            induction.insert(i.dest);
        }
    }
    let mut locals = BTreeSet::new();
    for (_, i) in insts() {
        if let Op::Bind { pattern, value } = &i.op
            && let PatternKind::Bind(name) = &pattern.kind
            && induction.contains(value)
            && single_binding(f, name)
        {
            locals.insert(name.clone());
        }
    }
    for (b, i) in insts() {
        if let Op::Read { root, path, .. } = &i.op
            && b != h
            && path.is_empty()
            && locals.contains(root)
        {
            induction.insert(i.dest);
        }
    }
    // Candidate accesses, by receiver place and index form.
    let reads: BTreeMap<ValueId, &Instruction> = insts()
        .filter(|(_, i)| matches!(i.op, Op::Read { .. }))
        .map(|(_, i)| (i.dest, i))
        .collect();
    let defs: BTreeMap<ValueId, &Instruction> = insts().map(|(_, i)| (i.dest, i)).collect();
    let is_u64 = |v: ValueId| {
        f.values
            .get(&v)
            .is_some_and(|t| crate::numeric::default_type(t) == Type::named("u64"))
    };
    // Locals bound once in the loop to an index of the loop variable's form
    // (`let s = max - j`), read like the value they hold.
    let mut bound_locals: BTreeMap<String, ValueId> = BTreeMap::new();
    for (_, i) in insts() {
        if let Op::Bind { pattern, value } = &i.op
            && let PatternKind::Bind(name) = &pattern.kind
            && !locals.contains(name)
            && is_u64(*value)
            && single_binding(f, name)
        {
            bound_locals.insert(name.clone(), *value);
        }
    }
    let analysis = Affine {
        f,
        blocks: &blocks,
        defined: &defined,
        induction: &induction,
        reads: &reads,
        defs: &defs,
        bound_locals: &bound_locals,
    };
    let mut receivers: BTreeMap<String, Receiver> = BTreeMap::new();
    let mut candidates: BTreeMap<ValueId, (Access, String)> = BTreeMap::new();
    for (b, i) in insts() {
        let Op::Seq { op, args } = &i.op else {
            continue;
        };
        let (Some(access), Some(receiver), Some(index)) =
            (hoistable(*op), args.first(), args.get(1))
        else {
            continue;
        };
        let (Some(place), Some(read)) = (&receiver.place, reads.get(&receiver.value)) else {
            continue;
        };
        if latches.contains(&b) {
            continue;
        }
        let Some(form) = analysis.linear(index.value, 0) else {
            continue;
        };
        if form.sign == 0 || form.plus.len() + form.minus.len() > MAX_TERMS {
            continue;
        }
        let Op::Read { root, path, .. } = &read.op else {
            continue;
        };
        if root != &place.root || path != &place.path {
            continue;
        }
        let key = format!("{}@{}", place.display(), form.key());
        receivers.entry(key.clone()).or_insert_with(|| Receiver {
            place: place.clone(),
            read: (*read).clone(),
            form,
        });
        candidates.insert(i.dest, (access, key));
    }
    // Keep only receivers whose length the loop cannot change.
    receivers.retain(|_, r| stable(f, &blocks, &r.place.root));
    candidates.retain(|_, (_, key)| receivers.contains_key(key));
    if candidates.is_empty() {
        return false;
    }
    // A loop with a short constant trip count saves less than its guards
    // cost: each guard checks both range ends with overflow-checked sums
    // (about 20 instructions), and each unchecked access saves a compare
    // and a branch (about 3) per iteration.
    match trip_count(f, &head) {
        Some(trips)
            if trips.saturating_mul(candidates.len() as u64)
                < GUARD_COST * receivers.len() as u64 =>
        {
            return false;
        }
        // An inner loop runs its guard on every iteration of the loop
        // around it. When its range changes with that loop's iteration it
        // is often short (a pass's few resource uses in Ignis's planner),
        // so it pays only when it drops more checks per iteration than it
        // adds guards; a length fixed for the outer loop (a matrix row) is
        // guarded as a top-level loop is.
        None if enclosing.is_some_and(|outer| !bound_invariant(f, &head, outer))
            && candidates.len() <= receivers.len() =>
        {
            return false;
        }
        _ => {}
    }
    let receivers: Vec<Receiver> = receivers.into_values().collect();
    clone_loop(f, h, pre, &head, &blocks, &receivers, &candidates);
    true
}

/// Whether every binding of `head`'s range slots (its start and its bound)
/// takes a value `outer` (the loop around it) does not change: one defined
/// outside it, a constant, or a read of a local `outer` neither binds nor
/// writes. A triangle loop (`for j in i + 1..n`) changes its start.
fn bound_invariant(f: &FunctionIr, head: &Header, outer: &BTreeSet<usize>) -> bool {
    let in_outer = || outer.iter().flat_map(|&b| f.blocks[b].instructions.iter());
    let changed = |root: &str| {
        in_outer().any(|i| match &i.op {
            Op::Bind { pattern, .. } => binds(pattern, root),
            Op::Write { place, .. } => place_root(place) == root,
            _ => false,
        })
    };
    in_outer().all(|i| match &i.op {
        Op::Bind { pattern, value }
            if binds(pattern, &head.bound) || binds(pattern, &head.index) =>
        {
            match in_outer().find(|d| d.dest == *value).map(|d| &d.op) {
                None => !outer.iter().any(|&b| f.blocks[b].params.contains(value)),
                Some(Op::Constant(_)) => true,
                Some(Op::Read { root, .. }) => !changed(root),
                Some(_) => false,
            }
        }
        _ => true,
    })
}

/// How many iterations' worth of one unchecked access pay for one guard
/// ([`version`]).
const GUARD_COST: u64 = 7;

/// The trip count of the counted loop `head` when both its range ends are
/// constants.
fn trip_count(f: &FunctionIr, head: &Header) -> Option<u64> {
    let lo = slot_constant(f, &head.index)?;
    let hi = slot_constant(f, &head.bound)?;
    let hi = if head.inclusive {
        hi.checked_add(1)?
    } else {
        hi
    };
    Some(u64::try_from(hi.saturating_sub(lo).max(0)).unwrap_or(u64::MAX))
}

/// The constant every binding of the loop slot `slot` gives it (the index
/// slot's writes are the loop's own increments).
fn slot_constant(f: &FunctionIr, slot: &str) -> Option<i128> {
    let mut found = None;
    for i in f.blocks.iter().flat_map(|b| &b.instructions) {
        match &i.op {
            Op::Bind { pattern, value } if binds(pattern, slot) => {
                let c = constant_value(f, *value, 0)?;
                if found.is_some_and(|k| k != c) {
                    return None;
                }
                found = Some(c);
            }
            _ => {}
        }
    }
    found
}

/// The integer constant `v` always holds: a literal, a read of a local
/// bound once to one, or a block parameter every incoming jump passes the
/// same constant (an inlined callee's result).
fn constant_value(f: &FunctionIr, v: ValueId, depth: usize) -> Option<i128> {
    if depth > MAX_DEPTH {
        return None;
    }
    for (b, block) in f.blocks.iter().enumerate() {
        if let Some(i) = block.instructions.iter().find(|i| i.dest == v) {
            return match &i.op {
                Op::Constant(crate::ast::Literal::Int(n)) => Some(*n),
                Op::Read { root, path, .. } if path.is_empty() && single_binding(f, root) => f
                    .blocks
                    .iter()
                    .flat_map(|b| &b.instructions)
                    .find_map(|i| match &i.op {
                        Op::Bind { pattern, value } if binds(pattern, root) => {
                            Some(constant_value(f, *value, depth + 1))
                        }
                        _ => None,
                    })?,
                _ => None,
            };
        }
        if let Some(k) = block.params.iter().position(|p| *p == v) {
            let mut found = None;
            for pred in &f.blocks {
                let args = match &pred.terminator {
                    Terminator::Jump { block: t, args } if *t == b => args,
                    Terminator::Branch { yes, no, .. } if *yes == b || *no == b => return None,
                    Terminator::Match { arms, .. } if arms.iter().any(|(_, t)| *t == b) => {
                        return None;
                    }
                    _ => continue,
                };
                let c = constant_value(f, *args.get(k)?, depth + 1)?;
                if found.is_some_and(|x| x != c) {
                    return None;
                }
                found = Some(c);
            }
            return found;
        }
    }
    None
}

/// The most loop-invariant terms one guarded index may have.
const MAX_TERMS: usize = 4;

/// How deep [`Affine::linear`] follows `+` and `-` operands.
const MAX_DEPTH: usize = 6;

/// A loop-invariant `u64` term of an index.
#[derive(Clone)]
enum Offset {
    /// A value defined before the loop.
    Value(ValueId),
    /// A read of a local the loop never changes, repeated in the preheader.
    Local(Box<Instruction>),
    /// An integer constant, repeated in the preheader.
    Const(Box<Instruction>),
}

impl Offset {
    fn key(&self) -> String {
        match self {
            Self::Value(v) => format!("%{v}"),
            Self::Local(read) => match &read.op {
                Op::Read { root, .. } => root.clone(),
                _ => String::new(),
            },
            Self::Const(c) => match &c.op {
                Op::Constant(crate::ast::Literal::Int(n)) => format!("#{n}"),
                _ => String::new(),
            },
        }
    }
}

/// An index `sign * i + plus.. - minus..` in the loop index `i` (`sign` is
/// -1, 0 or 1) with loop-invariant terms.
#[derive(Clone)]
struct Linear {
    sign: i8,
    plus: Vec<Offset>,
    minus: Vec<Offset>,
}

impl Linear {
    fn term(offset: Offset) -> Self {
        Self {
            sign: 0,
            plus: vec![offset],
            minus: vec![],
        }
    }

    /// `self + other` (`self - other` when `negate`), when the loop index
    /// keeps a coefficient of -1, 0 or 1.
    fn combine(mut self, other: Self, negate: bool) -> Option<Self> {
        let sign = if negate { -other.sign } else { other.sign };
        if self.sign != 0 && sign != 0 {
            return None;
        }
        self.sign += sign;
        let (plus, minus) = if negate {
            (other.minus, other.plus)
        } else {
            (other.plus, other.minus)
        };
        self.plus.extend(plus);
        self.minus.extend(minus);
        Some(self)
    }

    /// A key that tells apart the forms one guard can cover.
    fn key(&self) -> String {
        let mut plus: Vec<String> = self.plus.iter().map(Offset::key).collect();
        let mut minus: Vec<String> = self.minus.iter().map(Offset::key).collect();
        plus.sort();
        minus.sort();
        format!("{}i+{}-{}", self.sign, plus.join("+"), minus.join("-"))
    }
}

/// The index analysis of one loop.
///
/// The fast copy keeps every arithmetic instruction of the loop, checked or
/// wrapping, and only drops the bounds checks. Its guard evaluates the
/// index exactly at both ends of the range: an index linear in the loop
/// index takes every value between those, so when both ends are in `0..len`
/// every index the loop computes is too, and the loop's own `u64` result
/// (exact, or the exact value modulo 2^64 for a wrapping operation) is that
/// exact value. A checked operation that would overflow still raises in the
/// fast copy as in the checked one.
struct Affine<'a> {
    f: &'a FunctionIr,
    blocks: &'a BTreeSet<usize>,
    defined: &'a BTreeSet<ValueId>,
    induction: &'a BTreeSet<ValueId>,
    reads: &'a BTreeMap<ValueId, &'a Instruction>,
    defs: &'a BTreeMap<ValueId, &'a Instruction>,
    bound_locals: &'a BTreeMap<String, ValueId>,
}

impl Affine<'_> {
    fn is_u64(&self, v: ValueId) -> bool {
        self.f
            .values
            .get(&v)
            .is_some_and(|t| crate::numeric::default_type(t) == Type::named("u64"))
    }

    /// The linear form of the `u64` value `v`, if it has one.
    fn linear(&self, v: ValueId, depth: usize) -> Option<Linear> {
        if depth > MAX_DEPTH || !self.is_u64(v) {
            return None;
        }
        if self.induction.contains(&v) {
            return Some(Linear {
                sign: 1,
                plus: vec![],
                minus: vec![],
            });
        }
        if !self.defined.contains(&v) {
            return Some(Linear::term(Offset::Value(v)));
        }
        let def = self.defs.get(&v)?;
        match &def.op {
            Op::Constant(crate::ast::Literal::Int(n)) if *n >= 0 => {
                Some(Linear::term(Offset::Const(Box::new((*def).clone()))))
            }
            Op::Read { root, path, .. } if path.is_empty() => {
                if let Some(bound) = self.bound_locals.get(root) {
                    return self.linear(*bound, depth + 1);
                }
                let read = self.reads.get(&v)?;
                invariant_local(self.f, self.blocks, root)
                    .then(|| Linear::term(Offset::Local(Box::new((*read).clone()))))
            }
            Op::Binary(op, a, b) => {
                let negate = match op.as_str() {
                    "+" | "wrapping_add" => false,
                    "-" | "wrapping_sub" => true,
                    _ => return None,
                };
                let a = self.linear(*a, depth + 1)?;
                let b = self.linear(*b, depth + 1)?;
                a.combine(b, negate)
            }
            _ => None,
        }
    }
}

/// A hoisted receiver: its place, the read of it, and the index form.
struct Receiver {
    place: Place,
    read: Instruction,
    form: Linear,
}

/// Whether the loop never writes, rebinds or lends `inout` the local `root`.
fn invariant_local(f: &FunctionIr, blocks: &BTreeSet<usize>, root: &str) -> bool {
    blocks.iter().all(|&b| {
        f.blocks[b].instructions.iter().all(|i| {
            let lent = |args: &[Argument]| {
                args.iter().any(|a| {
                    a.pass != Pass::In
                        && a.pass != Pass::Sink
                        && a.place.as_ref().is_some_and(|p| p.root == root)
                })
            };
            match &i.op {
                Op::Write { place, .. } => place_root(place) != root,
                Op::Bind { pattern, .. } => !binds(pattern, root),
                Op::Call { args, .. }
                | Op::Indirect { args, .. }
                | Op::Seq { args, .. }
                | Op::Tiled { args, .. }
                | Op::Fip { args, .. } => !lent(args),
                _ => true,
            }
        })
    })
}

/// Whether `name` is bound by exactly one `Bind`, never written, and never
/// lent to an operation other than as `in` or by value.
fn single_binding(f: &FunctionIr, name: &str) -> bool {
    let lent = |args: &[Argument]| {
        args.iter().any(|a| {
            a.pass != Pass::In
                && a.pass != Pass::Sink
                && a.place.as_ref().is_some_and(|p| p.root == name)
        })
    };
    let mut count = 0;
    for i in f.blocks.iter().flat_map(|b| &b.instructions) {
        match &i.op {
            Op::Bind { pattern, .. } if binds(pattern, name) => count += 1,
            Op::Write { place, .. } if place_root(place) == name => return false,
            Op::Call { args, .. }
            | Op::Indirect { args, .. }
            | Op::Seq { args, .. }
            | Op::Tiled { args, .. }
            | Op::Fip { args, .. }
                if lent(args) =>
            {
                return false;
            }
            Op::Closure { .. } | Op::Handle { .. } => return false,
            _ => {}
        }
    }
    count == 1
}

/// Whether nothing in `blocks` can change the length of a sequence rooted
/// at `root`.
fn stable(f: &FunctionIr, blocks: &BTreeSet<usize>, root: &str) -> bool {
    if box_drops(f, blocks).any(|p| p.root == root) {
        return false;
    }
    for &b in blocks {
        for i in &f.blocks[b].instructions {
            let rooted = |a: &Argument| a.place.as_ref().is_some_and(|p| p.root == root);
            let ok = match &i.op {
                Op::Write { place, .. } => place_root(place) != root,
                Op::Bind { pattern, .. } => !binds(pattern, root),
                Op::Read { root: r, pass, .. } => r != root || *pass != Pass::Sink,
                Op::Seq { op, args } => {
                    let receiver = args.first().is_some_and(rooted) && op.has_receiver();
                    let others = if op.has_receiver() {
                        &args[1..]
                    } else {
                        &args[..]
                    };
                    (!receiver || keeps_length(*op))
                        && others.iter().all(|a| !rooted(a) || a.pass == Pass::In)
                }
                Op::Call { args, .. } | Op::Tiled { args, .. } | Op::Fip { args, .. } => {
                    args.iter().all(|a| !rooted(a) || a.pass == Pass::In)
                }
                _ => true,
            };
            if !ok {
                return false;
            }
        }
    }
    true
}

/// The places whose boxes the drop bookkeeping of `blocks` reclaims (a
/// replaced field, a destructured or dead local). A sequence at or inside
/// such a place may be freed there, so it is never hoisted.
fn box_drops<'a>(
    f: &'a FunctionIr,
    blocks: &'a BTreeSet<usize>,
) -> impl Iterator<Item = &'a Place> {
    blocks.iter().flat_map(move |b| {
        let inst = |m: &'a BTreeMap<usize, Vec<(usize, FipDrop)>>| {
            m.get(b).into_iter().flatten().map(|(_, d)| d)
        };
        inst(&f.box_drops_before_inst)
            .chain(inst(&f.box_drops_after_inst))
            .chain(f.box_drops_before_term.get(b).into_iter().flatten())
            .filter_map(|d| match d {
                FipDrop::Place(p, _) => Some(p),
                _ => None,
            })
    })
}

/// Adds the guard to `pre` and a fast copy of the loop `blocks`.
fn clone_loop(
    f: &mut FunctionIr,
    h: usize,
    pre: usize,
    head: &Header,
    blocks: &BTreeSet<usize>,
    receivers: &[Receiver],
    candidates: &BTreeMap<ValueId, (Access, String)>,
) {
    let mut fresh = Fresh::new(f);
    // The guard, at the end of the preheader.
    let span = f.blocks[h].instructions[0].span.clone();
    let before = f.blocks[pre].outgoing_token;
    let guard_token = fresh.token();
    let u64_ty = Type::named("u64");
    let read_slot = |dest, root: &str| Instruction {
        dest,
        op: Op::Read {
            root: root.into(),
            path: vec![],
            pass: Pass::Sink,
            projections: vec![],
        },
        ty: u64_ty.clone(),
        span: span.clone(),
        token: Some((before, before)),
        fallible: false,
    };
    let lo = fresh.value(f, u64_ty.clone());
    let hi = fresh.value(f, u64_ty.clone());
    let mut guard_insts = vec![read_slot(lo, &head.index), read_slot(hi, &head.bound)];
    let sink = |value| Argument {
        pass: Pass::Sink,
        value,
        place: None,
    };
    // One guard per receiver and index form, each taking the previous one's
    // result.
    let mut previous: Option<ValueId> = None;
    let mut guards = vec![];
    let mut token = before;
    for (k, r) in receivers.iter().enumerate() {
        let v = fresh.value(f, r.read.ty.clone());
        guard_insts.push(Instruction {
            dest: v,
            span: span.clone(),
            token: Some((before, before)),
            fallible: false,
            ..r.read.clone()
        });
        let mut args = vec![
            sink(lo),
            sink(hi),
            Argument {
                pass: Pass::In,
                value: v,
                place: Some(r.place.clone()),
            },
        ];
        for term in r.form.plus.iter().chain(&r.form.minus) {
            let value = match term {
                Offset::Value(c) => *c,
                Offset::Local(read) => {
                    let c = fresh.value(f, u64_ty.clone());
                    guard_insts.push(Instruction {
                        dest: c,
                        span: span.clone(),
                        token: Some((before, before)),
                        fallible: false,
                        ..(**read).clone()
                    });
                    c
                }
                Offset::Const(constant) => {
                    let c = fresh.value(f, u64_ty.clone());
                    guard_insts.push(Instruction {
                        dest: c,
                        token: constant.token.map(|_| (before, before)),
                        fallible: false,
                        ..(**constant).clone()
                    });
                    c
                }
            };
            args.push(sink(value));
        }
        if let Some(ok) = previous {
            args.push(sink(ok));
        }
        let last = k + 1 == receivers.len();
        let guard = fresh.value(f, Type::named("bool"));
        // Each guard is a state action of its own: the token chain runs
        // from the preheader's token through every guard to `guard_token`.
        let input = token;
        token = if last { guard_token } else { fresh.token() };
        guards.push(Instruction {
            dest: guard,
            op: Op::Seq {
                op: SeqOp::BoundsGuard {
                    inclusive: head.inclusive,
                    form: IndexForm {
                        negated: r.form.sign < 0,
                        plus: r.form.plus.len() as u8,
                        minus: r.form.minus.len() as u8,
                    },
                },
                args,
            },
            ty: Type::named("bool"),
            span: span.clone(),
            token: Some((input, token)),
            fallible: false,
        });
        previous = Some(guard);
    }
    // The reads and constants first, then the guards.
    guard_insts.extend(guards);
    let guard = previous.expect("a versioned loop hoists a receiver");
    copy_loop(
        f,
        h,
        pre,
        blocks,
        LoopGuard {
            insts: guard_insts,
            guard,
            token: guard_token,
        },
        &mut fresh,
        |_, _, i| candidates.get(&i.dest).map(|(access, _)| *access),
    );
}

/// The guard a versioned loop's preheader ends with: its instructions, its
/// `bool` result and the state token it leaves.
pub(crate) struct LoopGuard {
    pub(crate) insts: Vec<Instruction>,
    pub(crate) guard: ValueId,
    pub(crate) token: u32,
}

/// Fresh value ids and state tokens of a function.
pub(crate) struct Fresh {
    next_value: ValueId,
    next_token: u32,
}

impl Fresh {
    pub(crate) fn new(f: &FunctionIr) -> Self {
        let next_value = f
            .values
            .keys()
            .copied()
            .chain(f.blocks.iter().flat_map(|b| {
                b.instructions
                    .iter()
                    .map(|i| i.dest)
                    .chain(b.params.iter().copied())
            }))
            .max()
            .map_or(0, |v| v + 1);
        let next_token = f
            .blocks
            .iter()
            .flat_map(|b| {
                [b.entry_token, b.outgoing_token]
                    .into_iter()
                    .chain(b.incoming_tokens.iter().map(|(_, t)| *t))
                    .chain(
                        b.instructions
                            .iter()
                            .filter_map(|i| i.token)
                            .flat_map(|(a, b)| [a, b]),
                    )
            })
            .max()
            .map_or(0, |t| t + 1);
        Self {
            next_value,
            next_token,
        }
    }

    pub(crate) fn value(&mut self, f: &mut FunctionIr, ty: Type) -> ValueId {
        let v = self.next_value;
        self.next_value += 1;
        f.values.insert(v, ty);
        v
    }

    pub(crate) fn token(&mut self) -> u32 {
        self.next_token += 1;
        self.next_token - 1
    }
}

/// Ends `pre` with `guard` and adds a fast copy of the loop `blocks` (headed
/// by `h`) that the guard enters; the copy turns each access `convert` names
/// (by original block, instruction index and instruction) into an unchecked
/// one.
pub(crate) fn copy_loop(
    f: &mut FunctionIr,
    h: usize,
    pre: usize,
    blocks: &BTreeSet<usize>,
    guard: LoopGuard,
    fresh: &mut Fresh,
    convert: impl Fn(usize, usize, &Instruction) -> Option<Access>,
) {
    let LoopGuard {
        insts: guard_insts,
        guard,
        token: guard_token,
    } = guard;
    // Fresh blocks, block parameters and tokens for the copy. Instructions
    // keep their value ids: the native backend keeps each IR value in one
    // frontend variable, so a fresh id per copied value would add a
    // loop-carried variable per value to every block of the function. The
    // two copies are disjoint paths and no loop value is used outside the
    // loop, so every use still sees the definition of its own copy.
    let base = f.blocks.len();
    let block_map: BTreeMap<usize, usize> = blocks
        .iter()
        .enumerate()
        .map(|(k, &b)| (b, base + k))
        .collect();
    let mut value_map: BTreeMap<ValueId, ValueId> = BTreeMap::new();
    let mut token_map: BTreeMap<u32, u32> = BTreeMap::new();
    for &b in blocks {
        let block = &f.blocks[b];
        let defs: Vec<ValueId> = block.params.clone();
        let mut tokens = vec![block.entry_token];
        tokens.extend(
            block
                .instructions
                .iter()
                .filter_map(|i| i.token)
                .filter(|(a, b)| a != b)
                .map(|(_, b)| b),
        );
        for v in defs {
            let ty = f.values.get(&v).cloned().unwrap_or(Type::Infer);
            let n = fresh.value(f, ty);
            value_map.insert(v, n);
        }
        for t in tokens {
            token_map.insert(t, fresh.token());
        }
    }
    let map_value = |v: &mut ValueId| {
        if let Some(n) = value_map.get(v) {
            *v = *n;
        }
    };
    let map_token = |t: u32| token_map.get(&t).copied().unwrap_or(t);
    let mut copies = vec![];
    for &b in blocks {
        let mut block = f.blocks[b].clone();
        block.params.iter_mut().for_each(map_value);
        block.entry_token = map_token(block.entry_token);
        block.outgoing_token = map_token(block.outgoing_token);
        block.incoming_tokens = block
            .incoming_tokens
            .iter()
            .filter(|(p, _)| blocks.contains(p))
            .map(|(p, t)| (block_map[p], map_token(*t)))
            .collect();
        if b == h {
            block.incoming_tokens.push((pre, guard_token));
        }
        for (k, i) in block.instructions.iter_mut().enumerate() {
            if let Some(access) = convert(b, k, i)
                && let Op::Seq { op, .. } = &mut i.op
            {
                *op = SeqOp::Hoisted(access);
                i.fallible = false;
            }
            map_value(&mut i.dest);
            operands(&mut i.op).into_iter().for_each(map_value);
            i.token = i.token.map(|(a, b)| (map_token(a), map_token(b)));
        }
        terminator_operands(&mut block.terminator)
            .into_iter()
            .for_each(map_value);
        match &mut block.terminator {
            Terminator::Jump { block: t, .. } => *t = block_map.get(t).copied().unwrap_or(*t),
            Terminator::Branch { yes, no, .. } => {
                *yes = block_map.get(yes).copied().unwrap_or(*yes);
                *no = block_map.get(no).copied().unwrap_or(*no);
            }
            Terminator::Match { arms, .. } => {
                for (_, t) in arms {
                    *t = block_map.get(t).copied().unwrap_or(*t);
                }
            }
            Terminator::Return(_) | Terminator::Unreachable => {}
        }
        copies.push(block);
    }
    // Exits gain the copy's edges.
    let mut exit_edges: Vec<(usize, usize, u32)> = vec![];
    for (&b, copy) in blocks.iter().zip(&copies) {
        for s in successors(&copy.terminator) {
            if s < base {
                exit_edges.push((s, block_map[&b], copy.outgoing_token));
            }
        }
    }
    f.blocks.extend(copies);
    // The copy reclaims the same boxes at the same points: its instructions
    // keep their order, and only block parameters get fresh values.
    let map_drop = |d: &FipDrop| {
        let mut d = d.clone();
        match &mut d {
            FipDrop::Value(v, _) | FipDrop::OtherFields { value: v, .. } => map_value(v),
            FipDrop::Place(..) => {}
        }
        d
    };
    for (&b, &copy) in &block_map {
        for m in [&mut f.box_drops_before_inst, &mut f.box_drops_after_inst] {
            if let Some(drops) = m.get(&b) {
                let drops = drops.iter().map(|(k, d)| (*k, map_drop(d))).collect();
                m.insert(copy, drops);
            }
        }
        if let Some(drops) = f.box_drops_before_term.get(&b) {
            let drops = drops.iter().map(map_drop).collect();
            f.box_drops_before_term.insert(copy, drops);
        }
    }
    for (s, from, token) in exit_edges {
        f.blocks[s].incoming_tokens.push((from, token));
    }
    for entry in &mut f.blocks[h].incoming_tokens {
        if entry.0 == pre {
            entry.1 = guard_token;
        }
    }
    let pre_block = &mut f.blocks[pre];
    pre_block.instructions.extend(guard_insts);
    pre_block.outgoing_token = guard_token;
    pre_block.terminator = Terminator::Branch {
        condition: guard,
        yes: block_map[&h],
        no: h,
    };
}

/// A sequence header read once before a loop (port gaps lane 7, #990).
///
/// Element accesses in a loop read the receiver's handle and its `{len,
/// data}` words on every iteration, even when the loop cannot change them.
/// [`header_hoists`] finds, for each loop (one receiver at most when it
/// calls), the receiver places that no
/// instruction of the loop writes, rebinds, moves, resizes or lends other
/// than `in`; the native backend reads such a receiver and its header words
/// once at the end of `preheader` and gives the loop's `reads` of it those
/// values. Unlike [`run`] this needs no counted loop and no guard: element
/// accesses still check their bounds, against the hoisted length.
#[derive(Clone, Debug)]
pub struct HeaderHoist {
    /// The block that enters the loop (it dominates every loop block).
    pub preheader: usize,
    /// The receiver place.
    pub place: Place,
    /// The receiver's sequence or slice type.
    pub ty: Type,
    /// The reads of `place` in the loop that only receivers of accesses use.
    pub reads: BTreeSet<ValueId>,
    /// A borrowed parameter that nothing in the function can change
    /// ([`stable_views`]): read once at function entry, its header words
    /// are invariant for the whole call, so the native backend loads them
    /// as read-only memory that the code generator may share and move.
    pub entry: bool,
}

/// The borrowed (`in` or `inout`) sequence and slice parameters of `f` whose
/// handle and header nothing in the function can change, with their reads
/// that only receivers of length-keeping accesses use.
///
/// A borrowed view's header lives in the caller and its buffer cannot be
/// resized while it is lent, and the place calculus keeps distinct
/// parameters from overlapping (§5), so only the function itself can change
/// it. A slice view is second-class and never changes; a sequence handle
/// changes when the function writes, rebinds or moves the parameter, lends
/// it other than `in`, or from a closure, handler, effect operation or
/// indirect call. When it does none of these, a store through another
/// sequence cannot change this one's header, and neither can a call.
pub fn stable_views(f: &FunctionIr) -> Vec<HeaderHoist> {
    if f.blocks.is_empty() {
        return vec![];
    }
    let all: BTreeSet<usize> = (0..f.blocks.len()).collect();
    let insts = || f.blocks.iter().flat_map(|b| b.instructions.iter());
    // Code this graph cannot see runs: a closure or handler body, an
    // indirect call, an effect operation.
    let opaque = insts().any(|i| {
        matches!(
            i.op,
            Op::Closure { .. } | Op::Handle { .. } | Op::Indirect { .. }
        ) || matches!(&i.op, Op::Call { callee, .. }
            if callee.contains("::") && callee != "Fail::raise")
    });
    let users = value_users(f);
    let mut out = vec![];
    for param in f.params.iter().filter(|_| !opaque) {
        if !matches!(param.pass, Pass::In | Pass::Inout) {
            continue;
        }
        let ty = crate::numeric::default_type(&param.ty);
        if !(crate::seq::is_seq_type(&ty) || crate::seq::is_slice_type(&ty))
            || crate::strings::is_text(&ty)
        {
            continue;
        }
        let place = Place {
            root: param.name.clone(),
            path: vec![],
        };
        // A slice is a second-class borrow (§13): it is never assigned,
        // moved or resized, so even a callee it is lent `inout` to keeps its
        // view. A sequence must be stable here.
        if !crate::seq::is_slice_type(&ty) && !stable_place(f, &all, &place) {
            continue;
        }
        let mut reads = BTreeSet::new();
        for i in insts() {
            let Op::Read {
                root, path, pass, ..
            } = &i.op
            else {
                continue;
            };
            if root != &param.name || !path.is_empty() {
                continue;
            }
            // Only a growing operation writes a handle back to its place,
            // and a stable place is never grown.
            let pass_ok = matches!(pass, Pass::In | Pass::Inout);
            if pass_ok && only_access_receivers(&users, i.dest, *pass, &place, None) {
                reads.insert(i.dest);
            }
        }
        if !reads.is_empty() {
            out.push(HeaderHoist {
                preheader: 0,
                place,
                ty,
                reads,
                entry: true,
            });
        }
    }
    // A sequence field of an `in` parameter (`self.nodes` of `in self`):
    // nothing can write, move or resize a place under an `in` borrow, so
    // its handle and header are fixed for the call too, whatever code the
    // graph cannot see runs (a lent closure, a callback, a method call;
    // #1330). Only a handler shares the graph's names, so a graph with one
    // keeps the old rule.
    let handles = insts().any(|i| matches!(i.op, Op::Handle { .. }));
    let mut fields: BTreeMap<Vec<String>, HeaderHoist> = BTreeMap::new();
    for param in f.params.iter().filter(|p| p.pass == Pass::In && !handles) {
        if insts().any(|i| matches!(&i.op, Op::Bind { pattern, .. } if binds(pattern, &param.name)))
        {
            continue;
        }
        for i in insts() {
            let Op::Read {
                root, path, pass, ..
            } = &i.op
            else {
                continue;
            };
            if root != &param.name || path.is_empty() || *pass != Pass::In {
                continue;
            }
            let ty = crate::numeric::default_type(&i.ty);
            if !crate::seq::is_seq_type(&ty) || crate::strings::is_text(&ty) {
                continue;
            }
            let place = Place {
                root: root.clone(),
                path: path.clone(),
            };
            if only_access_receivers(&users, i.dest, *pass, &place, None) {
                let mut key = vec![root.clone()];
                key.extend(path.iter().cloned());
                fields
                    .entry(key)
                    .or_insert_with(|| HeaderHoist {
                        preheader: 0,
                        place,
                        ty,
                        reads: BTreeSet::new(),
                        entry: true,
                    })
                    .reads
                    .insert(i.dest);
            }
        }
    }
    out.extend(fields.into_values());
    out
}

/// Uses of each value, by block (`None` for the terminator).
fn value_users(f: &FunctionIr) -> BTreeMap<ValueId, Vec<(usize, Option<&Instruction>)>> {
    let mut users: BTreeMap<ValueId, Vec<(usize, Option<&Instruction>)>> = BTreeMap::new();
    for (b, block) in f.blocks.iter().enumerate() {
        for i in &block.instructions {
            for v in uses(&i.op) {
                users.entry(v).or_default().push((b, Some(i)));
            }
        }
        for v in terminator_uses(&block.terminator) {
            users.entry(v).or_default().push((b, None));
        }
    }
    users
}

/// Whether every use of `read` is the receiver, lent as `pass`, of a
/// length-keeping access of `place` (or the sequence of a bounds guard), in
/// `blocks` when given.
fn only_access_receivers(
    users: &BTreeMap<ValueId, Vec<(usize, Option<&Instruction>)>>,
    read: ValueId,
    pass: Pass,
    place: &Place,
    blocks: Option<&BTreeSet<usize>>,
) -> bool {
    users.get(&read).is_some_and(|us| {
        us.iter().all(|(b, user)| {
            blocks.is_none_or(|blocks| blocks.contains(b))
                && user.is_some_and(|u| match &u.op {
                    Op::Seq {
                        op: SeqOp::BoundsGuard { .. },
                        args,
                    } => args.iter().all(|a| {
                        a.value != read || (a.pass == Pass::In && a.place.as_ref() == Some(place))
                    }),
                    Op::Seq { op, args } => {
                        op.has_receiver()
                            && (keeps_length(*op) || matches!(op, SeqOp::Hoisted(_)))
                            && args.first().is_some_and(|a| {
                                a.value == read && a.pass == pass && a.place.as_ref() == Some(place)
                            })
                            && args[1..].iter().all(|a| a.value != read)
                    }
                    _ => false,
                })
        })
    })
}

/// The hoistable sequence headers of `f` ([`HeaderHoist`]); each read is
/// hoisted out of the outermost loop that keeps it stable.
pub fn header_hoists(f: &FunctionIr) -> Vec<HeaderHoist> {
    if !f.fip_join_slots.is_empty() || !f.fip_call_slots.is_empty() {
        return vec![];
    }
    let reached = reached(f);
    let preds = reached_preds(f);
    let idom = idoms(f, &preds);
    let users = value_users(f);
    // Parameters read once for the whole function need no loop hoist.
    let views = stable_views(f);
    let viewed: BTreeSet<Place> = views.iter().map(|v| v.place.clone()).collect();
    let mut found: Vec<(usize, HeaderHoist)> = vec![];
    for &h in &reached {
        let blocks = natural_loop(h, &preds, &idom);
        if !preds[h].iter().any(|p| blocks.contains(p)) {
            continue;
        }
        let outside: Vec<usize> = preds[h]
            .iter()
            .copied()
            .filter(|p| !blocks.contains(p))
            .collect();
        let [pre] = outside.as_slice() else {
            continue;
        };
        if blocks
            .iter()
            .any(|&b| b != h && preds[b].iter().any(|p| !blocks.contains(p)))
        {
            continue;
        }
        // A versioned loop and its fast copy share their preheader (the
        // guard picks one) and their value ids, so a read's users span both:
        // the hoisted words, read at the end of the shared preheader, serve
        // both copies.
        let twin = match f.blocks[*pre].terminator {
            Terminator::Branch { yes, no, .. } if yes == h || no == h => {
                let other = if yes == h { no } else { yes };
                let twin = natural_loop(other, &preds, &idom);
                let disjoint = twin.is_disjoint(&blocks)
                    && preds[other].iter().filter(|p| !twin.contains(p)).eq([pre]);
                if disjoint { twin } else { BTreeSet::new() }
            }
            _ => BTreeSet::new(),
        };
        let both: BTreeSet<usize> = blocks.union(&twin).copied().collect();
        let insts = || blocks.iter().flat_map(|&b| f.blocks[b].instructions.iter());
        let all_insts = || both.iter().flat_map(|&b| f.blocks[b].instructions.iter());
        // A call returning `Never` (`Fail::raise`) leaves the loop for good:
        // no handler it runs comes back to the loop's hoisted words.
        let never = Type::named("Never");
        if all_insts().any(|i| {
            matches!(
                i.op,
                Op::Closure { .. } | Op::Handle { .. } | Op::Indirect { .. }
            ) || matches!(&i.op, Op::Call { callee, .. } if callee.contains("::") && i.ty != never)
        }) {
            // An effect operation runs a handler outside the loop's view.
            continue;
        }
        // A call (or a sequence operation that may call the allocator)
        // clobbers the caller-saved registers, so in such a loop the hoisted
        // words are spilled and reloaded anyway, and many of them push other
        // values out of registers: such a loop hoists one receiver at most
        // (Ignis's pass loops ran 4% more instructions with no cap).
        let calls = all_insts().any(|i| calls_out(&i.op) && i.ty != never);
        let mut places: BTreeMap<String, HeaderHoist> = BTreeMap::new();
        for i in insts() {
            let Op::Read {
                root, path, pass, ..
            } = &i.op
            else {
                continue;
            };
            // Element writes (`set`, `try_set`) read their receiver `inout`
            // but never move or grow it; a loop that calls keeps to `in`
            // receivers (the cap below).
            if !(*pass == Pass::In || (*pass == Pass::Inout && !calls)) {
                continue;
            }
            let ty = crate::numeric::default_type(&i.ty);
            if !(crate::seq::is_seq_type(&ty) || crate::seq::is_slice_type(&ty))
                || crate::strings::is_text(&ty)
            {
                continue;
            }
            let place = Place {
                root: root.clone(),
                path: path.clone(),
            };
            if viewed.contains(&place) {
                continue;
            }
            // Every use is the receiver of an access in the loop.
            let only_receivers = only_access_receivers(&users, i.dest, *pass, &place, Some(&both));
            if !only_receivers || !stable_place(f, &both, &place) {
                continue;
            }
            places
                .entry(place.display())
                .or_insert_with(|| HeaderHoist {
                    preheader: *pre,
                    place: place.clone(),
                    ty: ty.clone(),
                    reads: BTreeSet::new(),
                    entry: false,
                })
                .reads
                .insert(i.dest);
        }
        // More than one receiver in a loop that calls costs more than it
        // saves, whichever receivers are picked (#1023: by read count, by
        // reads against calls, or only in loops of at most 8-32 blocks;
        // docs/parallel-results/port-speed-params-and-hoisting.md).
        if calls && places.len() > 1 {
            continue;
        }
        found.extend(places.into_values().map(|hoist| (blocks.len(), hoist)));
    }
    // Outermost loop first; a read claimed by an enclosing loop is skipped.
    found.sort_by_key(|f| std::cmp::Reverse(f.0));
    let mut claimed = BTreeSet::new();
    let mut out: Vec<HeaderHoist> = views;
    for (_, mut hoist) in found {
        hoist.reads.retain(|r| !claimed.contains(r));
        if hoist.reads.is_empty() {
            continue;
        }
        claimed.extend(hoist.reads.iter().copied());
        match out
            .iter_mut()
            .find(|o| o.preheader == hoist.preheader && o.place == hoist.place)
        {
            Some(o) => o.reads.extend(hoist.reads),
            None => out.push(hoist),
        }
    }
    out
}

/// Whether `op` may call out of the function (a call, or a sequence
/// operation that may reach the allocator), clobbering the caller-saved
/// registers.
pub(crate) fn calls_out(op: &Op) -> bool {
    match op {
        Op::Call { .. }
        | Op::Tiled { .. }
        | Op::Fip { .. }
        | Op::Indirect { .. }
        | Op::Closure { .. }
        | Op::Handle { .. } => true,
        Op::Seq { op, .. } => {
            !(keeps_length(*op) || matches!(op, SeqOp::Hoisted(_) | SeqOp::BoundsGuard { .. }))
        }
        _ => false,
    }
}

fn parse_place(place: &str) -> Place {
    let mut parts = place.split('.');
    Place {
        root: parts.next().unwrap_or_default().into(),
        path: parts.map(str::to_owned).collect(),
    }
}

/// Whether nothing in `blocks` can change the handle or the `{len, data}`
/// header of the sequence at `place`: no write, binding or move of a place
/// that overlaps it, no operation that resizes it, and no loan of it other
/// than `in`. Writing a disjoint field of the same root is allowed.
fn stable_place(f: &FunctionIr, blocks: &BTreeSet<usize>, place: &Place) -> bool {
    if box_drops(f, blocks).any(|p| p.overlaps(place)) {
        return false;
    }
    let lent =
        |a: &Argument| a.pass != Pass::In && a.place.as_ref().is_some_and(|p| p.overlaps(place));
    blocks.iter().all(|&b| {
        f.blocks[b].instructions.iter().all(|i| match &i.op {
            Op::Write { place: w, .. } => !parse_place(w).overlaps(place),
            Op::Bind { pattern, .. } => !binds(pattern, &place.root),
            Op::Read {
                root, path, pass, ..
            } => {
                *pass != Pass::Sink
                    || !Place {
                        root: root.clone(),
                        path: path.clone(),
                    }
                    .overlaps(place)
            }
            Op::Seq { op, args } => {
                let receiver = op.has_receiver()
                    && args
                        .first()
                        .is_some_and(|a| a.place.as_ref().is_some_and(|p| p.overlaps(place)));
                let others = if op.has_receiver() && !args.is_empty() {
                    &args[1..]
                } else {
                    &args[..]
                };
                (!receiver || keeps_length(*op) || matches!(op, SeqOp::Hoisted(_)))
                    && !others.iter().any(lent)
            }
            Op::Call { args, .. } | Op::Tiled { args, .. } | Op::Fip { args, .. } => {
                !args.iter().any(lent)
            }
            Op::Closure { .. } | Op::Handle { .. } | Op::Indirect { .. } => false,
            _ => true,
        })
    })
}
