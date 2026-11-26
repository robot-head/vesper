//! Bounds facts from comparisons (self-hosting SH1 lane 3, #1146).
//!
//! A byte loop of a lexer or a span lookup reads `s.try_get(i)` (or
//! `s[i]`) after a comparison it has already made: `if i >= n { break; }`
//! with `n = s.len()`, `for i in a..e` after `if e > s.len() { fail }`,
//! `let e = min(e, s.len())`, or `if i + 1 >= s.len() { return }` before
//! reading two bytes. The checks of such accesses are redundant.
//!
//! [`drop_compared_checks`] proves `index < s.len()` from the comparisons
//! that dominate an access and makes the access unchecked. A comparison
//! `a < b` (or `a <= b`) of two `u64` values that a block branches on holds
//! in every block that its successor dominates when that successor has the
//! branching block as its only predecessor. An index is below the length
//! when such a fact says it is below a bound that is at most the length:
//! the length itself, a value another fact puts at or below the length, or
//! a join of such values (the result of an inlined `min`).
//!
//! [`version_compared_loops`] handles the loops whose bound the function
//! cannot compare with the length, such as `loop { if i >= end { break; }
//! .. s.try_get(i) .. }` with `end` read from a table: when `end` does not
//! change in the loop, the loop is versioned like a counted loop, behind a
//! guard `end <= s.len()` in its preheader, and its fast copy reads `s`
//! unchecked. When the guard fails, the original loop runs.
//!
//! Values in Place IR live in local slots, so two reads of a local hold the
//! same number only when no write, rebinding or loan of it runs between
//! them ([`Facts::unchanged`]). A local bound once (an inlined parameter, a
//! `let`) is read as the value it was bound to, and a parameter the
//! function never changes is the same number everywhere.
//!
//! Lengths are compared by place: a sequence or slice place that nothing in
//! the function can resize, rebind or lend other than `in`
//! ([`super::stable_place`]), where a local bound once to `t.as_bytes()` or
//! `s.as_slice()` has the length of `t` or `s`.
use super::*;
use std::cell::RefCell;

/// The deepest chain of facts and joins one proof follows.
const MAX_PROOF_DEPTH: usize = 4;

/// What a value is, for comparing values across reads.
#[derive(Clone, Debug, PartialEq)]
enum Term {
    Const(i128),
    /// The length of a place (canonical, see [`Facts::length_place`]),
    /// read at (block, instruction).
    Len {
        place: Place,
        at: (usize, usize),
    },
    /// A local the function never changes (a parameter, or a field path of
    /// one).
    Fixed(String),
    /// A read of a local that may change, at (block, instruction).
    Slot {
        root: String,
        at: (usize, usize),
    },
    /// `base + k`: `exact` for checked `+`, which raises rather than wraps.
    Add {
        base: Box<Term>,
        k: i128,
        exact: bool,
    },
    /// A block parameter (a join).
    Param(ValueId),
    Value(ValueId),
}

/// The length an access checks against: of `place`, at (block,
/// instruction).
struct Target {
    place: Place,
    at: (usize, usize),
}

/// Whether `i` may change the handle or the length of the sequence at
/// `place` (the per-instruction rule of [`super::stable_place`]).
fn changes_len(i: &Instruction, place: &Place) -> bool {
    let lent =
        |a: &Argument| a.pass != Pass::In && a.place.as_ref().is_some_and(|p| p.overlaps(place));
    match &i.op {
        Op::Write { place: w, .. } => parse_place(w).overlaps(place),
        Op::Bind { pattern, .. } => binds(pattern, &place.root),
        Op::Read {
            root, path, pass, ..
        } => {
            *pass == Pass::Sink
                && Place {
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
            (receiver && !keeps_length(*op) && !matches!(op, SeqOp::Hoisted(_)))
                || others.iter().any(lent)
        }
        Op::Call { args, .. } | Op::Tiled { args, .. } | Op::Fip { args, .. } => {
            args.iter().any(lent)
        }
        Op::Closure { .. } | Op::Handle { .. } | Op::Indirect { .. } => true,
        _ => false,
    }
}

/// `lhs < rhs` (`strict`) or `lhs <= rhs`, on the edge `from -> to`.
#[derive(Clone, Debug)]
struct Edge {
    from: usize,
    to: usize,
    lhs: ValueId,
    rhs: ValueId,
    strict: bool,
}

struct Facts<'a> {
    f: &'a FunctionIr,
    /// Blocks that never fall through: they call a function returning
    /// `Never` (`Fail::raise`). Facts are computed on the graph without
    /// their out-edges, so `if i >= n { Fail::raise(..) }` proves `i < n`
    /// after it; loops are found on the whole graph.
    cut: Vec<bool>,
    preds: Vec<Vec<usize>>,
    idom: Vec<Option<usize>>,
    reached: BTreeSet<usize>,
    defs: BTreeMap<ValueId, (usize, usize)>,
    params: BTreeMap<ValueId, (usize, usize)>,
    bound: BTreeMap<String, ValueId>,
    once: BTreeSet<String>,
    fixed: BTreeSet<String>,
    edges: Vec<Edge>,
    stable: RefCell<BTreeMap<Place, bool>>,
}

fn unsigned_word(f: &FunctionIr, v: ValueId) -> bool {
    f.values.get(&v).is_some_and(|t| {
        let t = crate::numeric::default_type(t);
        t == Type::named("u64") || t == Type::named("usize")
    })
}

/// Whether `i` may change the local `root`: writes, binds or lends it
/// other than `in` (or by value), or runs code (a closure or handled body)
/// this pass does not look into.
fn touches(i: &Instruction, root: &str) -> bool {
    let lent = |args: &[Argument]| {
        args.iter().any(|a| {
            a.pass != Pass::In
                && a.pass != Pass::Sink
                && a.place.as_ref().is_some_and(|p| p.root == root)
        })
    };
    match &i.op {
        Op::Write { place, .. } => place_root(place) == root,
        Op::Bind { pattern, .. } => binds(pattern, root),
        Op::Call { args, .. }
        | Op::Indirect { args, .. }
        | Op::Seq { args, .. }
        | Op::Tiled { args, .. }
        | Op::Fip { args, .. } => lent(args),
        Op::Closure { .. } | Op::Handle { .. } => true,
        _ => false,
    }
}

impl<'a> Facts<'a> {
    fn new(f: &'a FunctionIr) -> Self {
        let never = Type::named("Never");
        let cut: Vec<bool> = f
            .blocks
            .iter()
            .map(|b| {
                b.instructions
                    .iter()
                    .any(|i| matches!(i.op, Op::Call { .. }) && i.ty == never)
            })
            .collect();
        let (preds, idom, reached) = if cut.iter().any(|c| *c) {
            let mut g = f.clone();
            for (b, c) in g.blocks.iter_mut().zip(&cut) {
                if *c {
                    b.terminator = Terminator::Unreachable;
                }
            }
            let preds = reached_preds(&g);
            (preds.clone(), idoms(&g, &preds), reached(&g))
        } else {
            let preds = reached_preds(f);
            (preds.clone(), idoms(f, &preds), reached(f))
        };
        let mut defs = BTreeMap::new();
        let mut params = BTreeMap::new();
        let mut binds_of: BTreeMap<String, Vec<ValueId>> = BTreeMap::new();
        for (b, block) in f.blocks.iter().enumerate() {
            for (k, p) in block.params.iter().enumerate() {
                params.insert(*p, (b, k));
            }
            for (k, i) in block.instructions.iter().enumerate() {
                defs.insert(i.dest, (b, k));
                if let Op::Bind { pattern, value } = &i.op
                    && let PatternKind::Bind(name) = &pattern.kind
                {
                    binds_of.entry(name.clone()).or_default().push(*value);
                }
            }
        }
        let mut bound = BTreeMap::new();
        let mut once = BTreeSet::new();
        let single = single_bindings(f);
        for (name, values) in &binds_of {
            if let [v] = values.as_slice()
                && single.contains(name)
            {
                bound.insert(name.clone(), *v);
                once.insert(name.clone());
            }
        }
        // Parameters nothing in the function writes, rebinds or lends
        // other than `in`.
        let insts = || f.blocks.iter().flat_map(|b| &b.instructions);
        let fixed = f
            .params
            .iter()
            .map(|p| p.name.clone())
            .filter(|name| !insts().any(|i| touches(i, name)))
            .collect();
        let mut facts = Self {
            f,
            cut,
            preds,
            idom,
            reached,
            defs,
            params,
            bound,
            once,
            fixed,
            edges: vec![],
            stable: RefCell::new(BTreeMap::new()),
        };
        facts.edges = facts.find_edges();
        facts
    }

    /// The successors of `b` on the graph facts are computed on.
    fn succ(&self, b: usize) -> Vec<usize> {
        if self.cut[b] {
            vec![]
        } else {
            successors(&self.f.blocks[b].terminator)
        }
    }

    fn inst(&self, v: ValueId) -> Option<&'a Instruction> {
        let (b, k) = *self.defs.get(&v)?;
        self.f.blocks.get(b)?.instructions.get(k)
    }

    /// The comparisons blocks branch on, as facts on their edges.
    fn find_edges(&self) -> Vec<Edge> {
        let mut out = vec![];
        for &d in &self.reached {
            let Terminator::Branch { condition, yes, no } = self.f.blocks[d].terminator else {
                continue;
            };
            if yes == no {
                continue;
            }
            let Some((op, a, c)) = self.comparison(condition) else {
                continue;
            };
            // (lhs, rhs, strict) on the yes edge, then on the no edge.
            let (on_yes, on_no) = match op {
                "<" => ((a, c, true), (c, a, false)),
                "<=" => ((a, c, false), (c, a, true)),
                ">" => ((c, a, true), (a, c, false)),
                ">=" => ((c, a, false), (a, c, true)),
                _ => continue,
            };
            for (to, (lhs, rhs, strict)) in [(yes, on_yes), (no, on_no)] {
                if to != d && self.preds.get(to).is_some_and(|p| p.as_slice() == [d]) {
                    out.push(Edge {
                        from: d,
                        to,
                        lhs,
                        rhs,
                        strict,
                    });
                }
            }
        }
        out
    }

    /// The unsigned comparison `condition` computes, when it is one.
    fn comparison(&self, condition: ValueId) -> Option<(&'a str, ValueId, ValueId)> {
        let mut v = condition;
        for _ in 0..MAX_DEPTH {
            match &self.inst(v)?.op {
                Op::Binary(op, a, c)
                    if matches!(op.as_str(), "<" | "<=" | ">" | ">=")
                        && unsigned_word(self.f, *a)
                        && unsigned_word(self.f, *c) =>
                {
                    return Some((op.as_str(), *a, *c));
                }
                Op::Read { root, path, .. } if path.is_empty() && self.once.contains(root) => {
                    v = self.bound[root];
                }
                _ => return None,
            }
        }
        None
    }

    fn is_stable(&self, place: &Place) -> bool {
        let all: BTreeSet<usize> = (0..self.f.blocks.len()).collect();
        // A slice local bound once (`let b = t.as_bytes()`) is a
        // second-class view: nothing can resize or reassign it.
        let slice_once = place.path.is_empty()
            && self.once.contains(&place.root)
            && self
                .f
                .values
                .get(&self.bound[&place.root])
                .is_some_and(|t| crate::seq::is_slice_type(&crate::numeric::default_type(t)));
        // A borrowed `in` parameter (or an `in` or `inout` slice
        // parameter) and its fields: the place calculus keeps them from
        // changing while the function runs (§5).
        let borrowed = self.f.params.iter().any(|p| {
            p.name == place.root
                && (p.pass == Pass::In
                    || (p.pass == Pass::Inout
                        && place.path.is_empty()
                        && crate::seq::is_slice_type(&crate::numeric::default_type(&p.ty))))
        }) && !self
            .f
            .blocks
            .iter()
            .flat_map(|b| &b.instructions)
            .any(|i| matches!(&i.op, Op::Bind { pattern, .. } if binds(pattern, &place.root)));
        *self
            .stable
            .borrow_mut()
            .entry(place.clone())
            .or_insert_with(|| slice_once || borrowed || stable_place(self.f, &all, place))
    }

    /// The place whose length `place`'s length is: the place a local bound
    /// once to a whole view of a stable place views, else `place`.
    fn length_place(&self, place: &Place) -> Place {
        let mut place = place.clone();
        for _ in 0..MAX_DEPTH {
            if !place.path.is_empty() || !self.once.contains(&place.root) {
                break;
            }
            let Some(Op::Seq { op, args }) = self.inst(self.bound[&place.root]).map(|i| &i.op)
            else {
                break;
            };
            if !matches!(
                op,
                SeqOp::AsSlice | SeqOp::Str(crate::strings::StrOp::AsBytes)
            ) {
                break;
            }
            match args.first().and_then(|a| a.place.clone()) {
                Some(viewed) if self.receiver_read(args, &viewed) && self.is_stable(&viewed) => {
                    place = viewed;
                }
                _ => break,
            }
        }
        place
    }

    /// Whether the first argument is a read of `place`.
    fn receiver_read(&self, args: &[Argument], place: &Place) -> bool {
        args.first().is_some_and(|a| {
            matches!(self.inst(a.value).map(|i| &i.op),
                Some(Op::Read { root, path, .. }) if *root == place.root && *path == place.path)
        })
    }

    fn term(&self, v: ValueId) -> Term {
        self.term_at(v, 0)
    }

    fn term_at(&self, v: ValueId, depth: usize) -> Term {
        if self.params.contains_key(&v) {
            return Term::Param(v);
        }
        let (Some(&at), Some(i)) = (self.defs.get(&v), self.inst(v)) else {
            return Term::Value(v);
        };
        if depth >= MAX_DEPTH {
            return Term::Value(v);
        }
        match &i.op {
            Op::Constant(crate::ast::Literal::Int(n)) if *n >= 0 => Term::Const(*n),
            Op::Read { root, path, .. } => {
                if path.is_empty() && self.once.contains(root) {
                    self.term_at(self.bound[root], depth + 1)
                } else if self.fixed.contains(root) {
                    let mut name = root.clone();
                    for p in path {
                        name.push('.');
                        name.push_str(p);
                    }
                    Term::Fixed(name)
                } else if path.is_empty() {
                    Term::Slot {
                        root: root.clone(),
                        at,
                    }
                } else {
                    Term::Value(v)
                }
            }
            Op::Seq {
                op: SeqOp::Len | SeqOp::SliceLen | SeqOp::Str(crate::strings::StrOp::Len),
                args,
            } => match args.first().and_then(|a| a.place.as_ref()) {
                Some(place) if self.receiver_read(args, place) => Term::Len {
                    place: self.length_place(place),
                    at,
                },
                _ => Term::Value(v),
            },
            Op::Binary(op, a, c) if matches!(op.as_str(), "+" | "wrapping_add") => {
                let exact = op == "+";
                match (self.term_at(*a, depth + 1), self.term_at(*c, depth + 1)) {
                    (base, Term::Const(k)) | (Term::Const(k), base) => Term::Add {
                        base: Box::new(base),
                        k,
                        exact,
                    },
                    _ => Term::Value(v),
                }
            }
            _ => Term::Value(v),
        }
    }

    /// Whether the point `p` (block, instruction) dominates the point `q`.
    fn dominates_point(&self, p: (usize, usize), q: (usize, usize)) -> bool {
        if p.0 == q.0 {
            p.1 <= q.1
        } else {
            dominates(&self.idom, p.0, q.0)
        }
    }

    /// Whether the local `root` holds the same value at the points `p` and
    /// `q`. One of them dominates the other, so the later one runs after the
    /// latest run of the earlier one; no path between them may change it.
    fn unchanged(&self, root: &str, p: (usize, usize), q: (usize, usize)) -> bool {
        self.unchanged_by(p, q, &|i| touches(i, root), &|_| false)
    }

    /// Whether no instruction `changes` (and no block `block_changes`) runs
    /// between the points `p` and `q`, one of which dominates the other.
    fn unchanged_by(
        &self,
        p: (usize, usize),
        q: (usize, usize),
        changes: &dyn Fn(&Instruction) -> bool,
        block_changes: &dyn Fn(usize) -> bool,
    ) -> bool {
        let (p, q) = if self.dominates_point(p, q) {
            (p, q)
        } else if self.dominates_point(q, p) {
            (q, p)
        } else {
            return false;
        };
        let insts = |b: usize| &self.f.blocks[b].instructions;
        if p.0 == q.0 && p.1 <= q.1 {
            // Entering the block again runs `p` again before `q`.
            return !block_changes(p.0) && !insts(p.0)[(p.1 + 1).min(q.1)..q.1].iter().any(changes);
        }
        // The blocks a path from `p` to `q` passes, never through `p`'s
        // block again: forward from `p`'s successors, back from `q`.
        let mut forward = BTreeSet::new();
        let mut stack: Vec<usize> = self.succ(p.0);
        while let Some(b) = stack.pop() {
            if b != p.0 && self.reached.contains(&b) && forward.insert(b) {
                stack.extend(self.succ(b));
            }
        }
        if !forward.contains(&q.0) {
            return false;
        }
        let mut back = BTreeSet::new();
        let mut stack: Vec<usize> = self.preds[q.0].clone();
        while let Some(b) = stack.pop() {
            if b != p.0 && back.insert(b) {
                stack.extend(self.preds[b].iter().copied());
            }
        }
        if block_changes(p.0) || block_changes(q.0) {
            return false;
        }
        if insts(p.0).iter().skip(p.1 + 1).any(changes) {
            return false;
        }
        // `q`'s block counts whole when a path from it returns to it.
        let upto = if back.contains(&q.0) {
            insts(q.0).len()
        } else {
            q.1
        };
        if insts(q.0).iter().take(upto).any(changes) {
            return false;
        }
        forward
            .intersection(&back)
            .filter(|&&b| b != q.0)
            .all(|&b| !block_changes(b) && !insts(b).iter().any(changes))
    }

    /// Whether the length of `place` is the same at the points `p` and `q`:
    /// nothing in the function can change it, or nothing between them does.
    fn length_kept(&self, place: &Place, p: (usize, usize), q: (usize, usize)) -> bool {
        self.is_stable(place)
            || self.unchanged_by(p, q, &|i| changes_len(i, place), &|b| {
                box_drops(self.f, &BTreeSet::from([b])).any(|d| d.overlaps(place))
            })
    }

    /// Whether `a` and `b` are the same number.
    fn same(&self, a: &Term, b: &Term) -> bool {
        match (a, b) {
            (Term::Slot { root: r, at: p }, Term::Slot { root: s, at: q, .. }) => {
                r == s && self.unchanged(r, *p, *q)
            }
            (
                Term::Add {
                    base: x,
                    k: j,
                    exact: e,
                },
                Term::Add {
                    base: y,
                    k,
                    exact: f,
                },
            ) => j == k && e == f && self.same(x, y),
            (Term::Len { place: x, at: p }, Term::Len { place: y, at: q }) => {
                x == y && self.length_kept(x, *p, *q)
            }
            _ => a == b,
        }
    }

    /// The facts that hold at the start of block `at`.
    fn holding(&self, at: usize) -> impl Iterator<Item = &Edge> {
        self.edges
            .iter()
            .filter(move |e| dominates(&self.idom, e.to, at))
    }

    /// Whether `t` is below the length of `place` wherever block `at` runs.
    fn below_len(&self, t: &Term, place: &Target, at: usize, depth: usize) -> bool {
        if depth > MAX_PROOF_DEPTH {
            return false;
        }
        self.holding(at).any(|e| {
            let (l, r) = (self.term(e.lhs), self.term(e.rhs));
            let index_is = |l: &Term| {
                self.same(l, t)
                    || matches!(l, Term::Add { base, k, exact: true } if *k >= 0 && self.same(base, t))
            };
            if e.strict {
                index_is(&l) && self.at_most_len(&r, place, e.from, depth + 1)
            } else {
                self.same(&l, t) && self.below_len(&r, place, e.from, depth + 1)
            }
        })
    }

    /// Whether `t` is at most the length of `place` wherever block `at`
    /// runs.
    fn at_most_len(&self, t: &Term, place: &Target, at: usize, depth: usize) -> bool {
        if depth > MAX_PROOF_DEPTH {
            return false;
        }
        if let Term::Len { place: p, at: read } = t
            && *p == place.place
            && self.length_kept(p, *read, place.at)
        {
            return true;
        }
        // A join of values that are each at most the length (`min`).
        if let Term::Param(v) = t {
            let (b, k) = self.params[v];
            let preds = &self.preds[b];
            if !preds.is_empty()
                && preds.iter().all(|&p| match &self.f.blocks[p].terminator {
                    Terminator::Jump { block, args } if *block == b => args
                        .get(k)
                        .is_some_and(|&a| self.at_most_len(&self.term(a), place, p, depth + 1)),
                    _ => false,
                })
            {
                return true;
            }
        }
        self.holding(at).any(|e| {
            let (l, r) = (self.term(e.lhs), self.term(e.rhs));
            // `l < r` or `l <= r`, and `r <= len`.
            self.same(&l, t) && self.at_most_len(&r, place, e.from, depth + 1)
        })
    }

    /// The element accesses of block `b`: (instruction, access, receiver
    /// place, index value), for receivers read from a stable place.
    fn accesses(&self, b: usize) -> Vec<(usize, Access, Place, ValueId)> {
        let mut out = vec![];
        for (k, i) in self.f.blocks[b].instructions.iter().enumerate() {
            let Op::Seq { op, args } = &i.op else {
                continue;
            };
            let (Some(access), Some(index)) = (hoistable(*op), args.get(1)) else {
                continue;
            };
            let Some(place) = args.first().and_then(|a| a.place.as_ref()) else {
                continue;
            };
            if self.receiver_read(args, place) {
                out.push((k, access, place.clone(), index.value));
            }
        }
        out
    }
}

/// The element accesses whose index the dominating comparisons put below
/// their receiver's length: (block, instruction, access) each.
pub(super) fn compared_checks(f: &FunctionIr) -> Vec<(usize, usize, Access)> {
    let facts = Facts::new(f);
    if facts.edges.is_empty() {
        return vec![];
    }
    let mut found = vec![];
    for &b in &facts.reached {
        for (k, access, place, index) in facts.accesses(b) {
            let target = Target {
                place: facts.length_place(&place),
                at: (b, k),
            };
            if facts.below_len(&facts.term(index), &target, b, 0) {
                found.push((b, k, access));
            }
        }
    }
    found
}

/// Makes the accesses of [`compared_checks`] unchecked.
pub(super) fn make_unchecked(f: &mut FunctionIr, found: &[(usize, usize, Access)]) {
    for &(b, k, access) in found {
        let i = &mut f.blocks[b].instructions[k];
        if let Op::Seq { op, .. } = &mut i.op {
            *op = SeqOp::Hoisted(access);
            i.fallible = false;
        }
    }
}

/// A loop-invariant bound, readable in the preheader.
#[derive(Clone, Debug, PartialEq)]
enum Bound {
    /// A local the loop does not change.
    Local(String),
    Const(i128),
    /// A value defined before the loop.
    Value(ValueId),
}

/// One receiver of a versioned loop: its place, a read of it to clone, and
/// the bound its accesses' indices are below.
struct Guarded {
    place: Place,
    read: Instruction,
    bound: Bound,
}

/// Versions each loop whose accesses a comparison in the loop puts below a
/// loop-invariant bound that the function cannot compare with the length:
/// a guard `bound <= len` in the preheader enters a copy with unchecked
/// accesses. Returns how many loops it versioned.
pub(super) fn version_compared_loops(
    f: &mut FunctionIr,
    trace: &mut Option<super::Trace<'_>>,
) -> usize {
    let mut count = 0;
    // One loop at a time: versioning adds blocks and changes dominators, so
    // the facts are recomputed only after a loop is versioned. Each loop is
    // tried once, by its header, innermost first.
    let mut tried = BTreeSet::new();
    loop {
        let before = tried.clone();
        let next = next_loop(f, &mut tried);
        if let Some(t) = trace.as_mut() {
            let decision = match &next {
                None => "none".to_string(),
                Some(c) => candidate_text(c),
            };
            t(
                f,
                Step {
                    tried: before,
                    decision: format!("{decision} tried={}", blocks_text(&tried)),
                    ..step(StepKind::ComparedLoop, String::new())
                },
            );
        }
        let Some((h, pre, blocks, guarded, convert)) = next else {
            return count;
        };
        let base = f.blocks.len();
        guard_and_copy(f, h, pre, &blocks, &guarded, &convert);
        // The copy's blocks are versioned already.
        tried.extend(base..f.blocks.len());
        count += 1;
    }
}

/// The decision text of a [`StepKind::ComparedLoop`] step.
fn candidate_text((h, pre, blocks, guarded, convert): &Candidate) -> String {
    let guards = guarded
        .iter()
        .map(|g| {
            let bound = match &g.bound {
                Bound::Local(n) => format!("local:{n}"),
                Bound::Const(c) => format!("const:{c}"),
                Bound::Value(v) => format!("value:{v}"),
            };
            format!("{}<{bound}:{}", g.place.display(), g.read.dest)
        })
        .collect::<Vec<_>>()
        .join(" ");
    let accesses = convert
        .iter()
        .map(|((b, k), a)| format!("{b}:{k}:{}", access_text(*a)))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "header={h} pre={pre} blocks={} guards=[{guards}] accesses=[{accesses}]",
        blocks_text(blocks)
    )
}

type Candidate = (
    usize,
    usize,
    BTreeSet<usize>,
    Vec<Guarded>,
    BTreeMap<(usize, usize), Access>,
);

/// The next untried loop with something to guard; marks every loop it
/// tries.
fn next_loop(f: &FunctionIr, tried: &mut BTreeSet<usize>) -> Option<Candidate> {
    // Loops and their entries on the whole graph.
    let preds = reached_preds(f);
    let idom = idoms(f, &preds);
    let mut loops: Vec<(usize, BTreeSet<usize>)> = reached(f)
        .into_iter()
        .filter(|&h| {
            // Counted loops are the counted-loop pass's, with its cost
            // model (a triangle loop is not worth a guard).
            !tried.contains(&h)
                && matches!(
                    f.blocks[h].instructions.first().map(|i| &i.op),
                    Some(Op::Loop)
                )
                && preds[h].iter().any(|&p| dominates(&idom, h, p))
                && header(f, h).is_none()
        })
        .map(|h| (h, natural_loop(h, &preds, &idom)))
        .collect();
    if loops.is_empty() {
        return None;
    }
    loops.sort_by_key(|(h, l)| (l.len(), *h));
    let facts = Facts::new(f);
    if facts.edges.is_empty() {
        return None;
    }
    for (h, blocks) in loops {
        tried.insert(h);
        if let Some(c) = loop_candidate(f, &facts, &preds, h, blocks) {
            return Some(c);
        }
    }
    None
}

/// What to guard in loop `blocks` headed by `h`, if it is worth a guard.
fn loop_candidate(
    f: &FunctionIr,
    facts: &Facts,
    preds: &[Vec<usize>],
    h: usize,
    blocks: BTreeSet<usize>,
) -> Option<Candidate> {
    let pre = loop_entry(f, preds, h, &blocks)?;
    // A loop the counted-loop pass versioned already has its checked copy
    // here; its guard failed, so this one would too.
    if matches!(&f.blocks[pre].terminator, Terminator::Branch { condition, .. }
        if matches!(facts.inst(*condition).map(|i| &i.op),
            Some(Op::Seq { op: SeqOp::BoundsGuard { .. }, .. })))
    {
        return None;
    }
    let mut guarded: Vec<Guarded> = vec![];
    let mut convert = BTreeMap::new();
    for &b in &blocks {
        for (k, access, place, index) in facts.accesses(b) {
            if !stable_place(f, &blocks, &place) {
                continue;
            }
            let len = facts.length_place(&place);
            let t = facts.term(index);
            let bound = facts.holding(b).find_map(|e| {
                if !e.strict || !blocks.contains(&e.from) {
                    return None;
                }
                let l = facts.term(e.lhs);
                let index_is = facts.same(&l, &t)
                    || matches!(&l, Term::Add { base, k, exact: true } if *k >= 0 && facts.same(base, &t));
                if !index_is {
                    return None;
                }
                match facts.term(e.rhs) {
                    Term::Const(c) => Some(Bound::Const(c)),
                    Term::Fixed(name) if !name.contains('.') => Some(Bound::Local(name)),
                    // The guard reads the local in the preheader, so a read
                    // before the loop must still hold there.
                    Term::Slot { root, at }
                        if invariant_local(f, &blocks, &root)
                            && (blocks.contains(&at.0)
                                || facts.unchanged(
                                    &root,
                                    at,
                                    (pre, f.blocks[pre].instructions.len()),
                                )) =>
                    {
                        Some(Bound::Local(root))
                    }
                    // A value (a join of an inlined call) defined outside
                    // the loop dominates its header, so its preheader too.
                    Term::Param(v) | Term::Value(v)
                        if facts
                            .params
                            .get(&v)
                            .or_else(|| facts.defs.get(&v))
                            .is_some_and(|(d, _)| !blocks.contains(d)) =>
                    {
                        Some(Bound::Value(v))
                    }
                    _ => None,
                }
            });
            let Some(bound) = bound else {
                continue;
            };
            let receiver = match &f.blocks[b].instructions[k].op {
                Op::Seq { args, .. } => args.first().and_then(|a| facts.inst(a.value)),
                _ => None,
            };
            let Some(receiver) = receiver else {
                continue;
            };
            if !guarded.iter().any(|g| g.place == len && g.bound == bound) {
                // The guard reads the length of the place the receiver's
                // length is; a whole view has its viewed place's length, so
                // reading the receiver itself is enough.
                guarded.push(Guarded {
                    place: len.clone(),
                    read: receiver.clone(),
                    bound,
                });
            }
            convert.insert((b, k), access);
        }
    }
    // Guards are about as cheap as two checked accesses; a loop must drop
    // at least as many checks as it adds guards.
    if guarded.is_empty() || convert.len() < guarded.len() || guarded.len() > MAX_TERMS {
        return None;
    }
    Some((h, pre, blocks, guarded, convert))
}

/// The preheader of loop `blocks` (headed by `h`) when it can be versioned:
/// entered only through the header, from one block that jumps there with no
/// arguments, with no closure, handler, indirect call or `@fip` slot, and
/// no value of it used outside it.
/// Locals bound by exactly one `Bind` and never written, lent other than
/// `in` or by value, or captured: [`super::single_binding`] for every name
/// at once.
fn single_bindings(f: &FunctionIr) -> BTreeSet<String> {
    fn names(pattern: &crate::ast::Pattern, out: &mut BTreeSet<String>) {
        match &pattern.kind {
            PatternKind::Bind(n) => {
                out.insert(n.clone());
            }
            PatternKind::Tuple(ps) => ps.iter().for_each(|p| names(p, out)),
            PatternKind::Constructor { fields, .. } => {
                fields.iter().for_each(|(_, p)| names(p, out));
            }
            PatternKind::Wild | PatternKind::Literal(_) => {}
        }
    }
    let mut count: BTreeMap<String, usize> = BTreeMap::new();
    let mut spoiled = BTreeSet::new();
    for i in f.blocks.iter().flat_map(|b| &b.instructions) {
        match &i.op {
            Op::Bind { pattern, .. } => {
                let mut bound = BTreeSet::new();
                names(pattern, &mut bound);
                for n in bound {
                    *count.entry(n).or_default() += 1;
                }
            }
            Op::Write { place, .. } => {
                spoiled.insert(place_root(place).to_string());
            }
            Op::Call { args, .. }
            | Op::Indirect { args, .. }
            | Op::Seq { args, .. }
            | Op::Tiled { args, .. }
            | Op::Fip { args, .. } => {
                for a in args {
                    if a.pass != Pass::In
                        && a.pass != Pass::Sink
                        && let Some(p) = &a.place
                    {
                        spoiled.insert(p.root.clone());
                    }
                }
            }
            Op::Closure { .. } | Op::Handle { .. } => return BTreeSet::new(),
            _ => {}
        }
    }
    count
        .into_iter()
        .filter(|(n, c)| *c == 1 && !spoiled.contains(n))
        .map(|(n, _)| n)
        .collect()
}

fn loop_entry(
    f: &FunctionIr,
    preds: &[Vec<usize>],
    h: usize,
    blocks: &BTreeSet<usize>,
) -> Option<usize> {
    let outside: Vec<usize> = preds[h]
        .iter()
        .copied()
        .filter(|p| !blocks.contains(p))
        .collect();
    let [pre] = outside.as_slice() else {
        return None;
    };
    let pre = *pre;
    if !matches!(&f.blocks[pre].terminator, Terminator::Jump { block, args } if *block == h && args.is_empty())
    {
        return None;
    }
    for &b in blocks {
        if b != h && preds[b].iter().any(|p| !blocks.contains(p)) {
            return None;
        }
        if f.fip_join_slots.contains_key(&b) {
            return None;
        }
    }
    let insts = || blocks.iter().flat_map(|&b| f.blocks[b].instructions.iter());
    if insts().any(|i| {
        matches!(
            i.op,
            Op::Closure { .. } | Op::Handle { .. } | Op::Indirect { .. }
        ) || f.fip_call_slots.contains_key(&i.dest)
    }) {
        return None;
    }
    let mut defined: BTreeSet<ValueId> = insts().map(|i| i.dest).collect();
    for &b in blocks {
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
            return None;
        }
    }
    Some(pre)
}

/// Ends `pre` with the guards of `guarded`, chained, and copies the loop.
fn guard_and_copy(
    f: &mut FunctionIr,
    h: usize,
    pre: usize,
    blocks: &BTreeSet<usize>,
    guarded: &[Guarded],
    convert: &BTreeMap<(usize, usize), Access>,
) {
    let mut fresh = Fresh::new(f);
    let span = f.blocks[h].instructions[0].span.clone();
    let before = f.blocks[pre].outgoing_token;
    let guard_token = fresh.token();
    let u64_ty = Type::named("u64");
    let sink = |value| Argument {
        pass: Pass::Sink,
        value,
        place: None,
    };
    let plain = |op, dest, ty: &Type| Instruction {
        dest,
        op,
        ty: ty.clone(),
        span: span.clone(),
        token: None,
        fallible: false,
    };
    let mut insts = vec![];
    let lo = fresh.value(f, u64_ty.clone());
    insts.push(plain(
        Op::Constant(crate::ast::Literal::Int(0)),
        lo,
        &u64_ty,
    ));
    let mut previous: Option<ValueId> = None;
    let mut token = before;
    let mut guards = vec![];
    for (k, g) in guarded.iter().enumerate() {
        let hi = match &g.bound {
            Bound::Value(v) => *v,
            _ => fresh.value(f, u64_ty.clone()),
        };
        insts.extend(match &g.bound {
            Bound::Value(_) => None,
            Bound::Const(c) => Some(plain(
                Op::Constant(crate::ast::Literal::Int(*c)),
                hi,
                &u64_ty,
            )),
            Bound::Local(root) => Some(Instruction {
                token: Some((before, before)),
                ..plain(
                    Op::Read {
                        root: root.clone(),
                        path: vec![],
                        pass: Pass::Sink,
                        projections: vec![],
                    },
                    hi,
                    &u64_ty,
                )
            }),
        });
        let s = fresh.value(f, g.read.ty.clone());
        insts.push(Instruction {
            dest: s,
            span: span.clone(),
            token: Some((before, before)),
            fallible: false,
            ..g.read.clone()
        });
        let receiver_place = match &g.read.op {
            Op::Read { root, path, .. } => Place {
                root: root.clone(),
                path: path.clone(),
            },
            _ => g.place.clone(),
        };
        let mut args = vec![
            sink(lo),
            sink(hi),
            Argument {
                pass: Pass::In,
                value: s,
                place: Some(receiver_place),
            },
        ];
        if let Some(ok) = previous {
            args.push(sink(ok));
        }
        let guard = fresh.value(f, Type::named("bool"));
        let input = token;
        token = if k + 1 == guarded.len() {
            guard_token
        } else {
            fresh.token()
        };
        guards.push(Instruction {
            dest: guard,
            op: Op::Seq {
                op: SeqOp::BoundsGuard {
                    inclusive: false,
                    form: IndexForm {
                        negated: false,
                        plus: 0,
                        minus: 0,
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
    insts.extend(guards);
    let guard = previous.expect("a guarded loop has a receiver");
    copy_loop(
        f,
        h,
        pre,
        blocks,
        LoopGuard {
            insts,
            guard,
            token: guard_token,
        },
        &mut fresh,
        |b, k, _| convert.get(&(b, k)).copied(),
    );
}
