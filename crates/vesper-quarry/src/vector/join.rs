//! Multi-table vectorized plans: column-materialized relations, hash joins
//! (equality conjuncts of `ON` as keys, the rest as a residual predicate
//! evaluated over candidate-pair batches), vectorized nested-loop joins
//! when `ON` has no equality between the two sides, and filters, all over
//! column batches of `engine.batch()` rows with reused scratch buffers.
//! The probe side streams: candidate pairs are produced, filtered and
//! emitted one batch at a time. Chains of three or more inner joins are
//! reordered by a greedy cost model over the measured input sizes and key
//! distinct counts, and their result is put back into written order.
//!
//! Output order equals the row executor's: left-major, matches in right
//! order, an unmatched left row right after its (absent) matches, unmatched
//! right rows at the end.
use super::{as_bool, eval, Column, Ctx, Data, Operand, VectorEngine};
use crate::execute::DataSet;
use crate::parse::Cmp;
use crate::plan::{ExprNode, JoinKind, PreparedQuery, RelPlan, TypedExpr};
use crate::{add, bound, Error, ErrorKind, Result, ScalarType, Value};
use std::collections::{BTreeSet, HashMap};

/// A materialized intermediate relation: one column per bound slot.
pub(super) struct Rel {
    pub cols: Vec<Option<Column>>,
    pub n: usize,
}

impl Column {
    pub(super) fn empty(ty: ScalarType) -> Self {
        let data = match ty {
            ScalarType::U64 => Data::U64(Vec::new()),
            ScalarType::I64 => Data::I64(Vec::new()),
            ScalarType::F64 => Data::F64(Vec::new()),
            ScalarType::Bool => Data::Bool(Vec::new()),
            ScalarType::Text => Data::Text(Vec::new()),
            ScalarType::Bytes => Data::Bytes(Vec::new()),
        };
        Self {
            data,
            nulls: Vec::new(),
        }
    }
    /// Appends row `i` of `src` (same type), or NULL for `None`.
    pub(super) fn push_from(&mut self, src: &Column, i: Option<usize>) {
        let Some(i) = i else {
            self.push_null();
            return;
        };
        self.nulls.push(src.nulls[i]);
        match (&mut self.data, &src.data) {
            (Data::U64(d), Data::U64(s)) => d.push(s[i]),
            (Data::I64(d), Data::I64(s)) => d.push(s[i]),
            (Data::F64(d), Data::F64(s)) => d.push(s[i]),
            (Data::Bool(d), Data::Bool(s)) => d.push(s[i]),
            (Data::Text(d), Data::Text(s)) => d.push(s[i].clone()),
            (Data::Bytes(d), Data::Bytes(s)) => d.push(s[i].clone()),
            _ => unreachable!("slot columns keep their checked type"),
        }
    }
    pub(super) fn push_null(&mut self) {
        self.nulls.push(1);
        match &mut self.data {
            Data::U64(d) => d.push(0),
            Data::I64(d) => d.push(0),
            Data::F64(d) => d.push(0.0),
            Data::Bool(d) => d.push(0),
            Data::Text(d) => d.push(String::new()),
            Data::Bytes(d) => d.push(Vec::new()),
        }
    }
    /// Clears the column keeping its allocations (batch reuse).
    pub(super) fn clear(&mut self) {
        self.nulls.clear();
        match &mut self.data {
            Data::U64(d) => d.clear(),
            Data::I64(d) => d.clear(),
            Data::F64(d) => d.clear(),
            Data::Bool(d) => d.clear(),
            Data::Text(d) => d.clear(),
            Data::Bytes(d) => d.clear(),
        }
    }
}

/// Reusable per-query scratch: batch columns, masks and index buffers keep
/// their capacity from batch to batch.
#[derive(Default)]
pub(super) struct Scratch {
    batch: Vec<Option<Column>>,
    selected: Vec<u8>,
    left: Vec<usize>,
    right: Vec<usize>,
}

/// Hashable form of a non-null join key value. Floats are finite here;
/// -0.0 is normalized so it equals +0.0 as SQL comparison requires.
#[derive(Clone, PartialEq, Eq, Hash)]
enum KeyPart {
    Word(u64),
    Bytes(Vec<u8>),
}
fn key_part(c: &Column, i: usize) -> Option<KeyPart> {
    if c.nulls[i] == 1 {
        return None;
    }
    Some(match &c.data {
        Data::U64(x) => KeyPart::Word(x[i]),
        Data::I64(x) => KeyPart::Word(x[i] as u64),
        Data::F64(x) => KeyPart::Word(if x[i] == 0.0 { 0 } else { x[i].to_bits() }),
        Data::Bool(x) => KeyPart::Word(u64::from(x[i])),
        Data::Text(x) => KeyPart::Bytes(x[i].as_bytes().to_vec()),
        Data::Bytes(x) => KeyPart::Bytes(x[i].clone()),
    })
}
fn row_key(cols: &[&Column], i: usize) -> Option<Vec<KeyPart>> {
    cols.iter().map(|c| key_part(c, i)).collect()
}

fn conjuncts<'e>(e: &'e TypedExpr, out: &mut Vec<&'e TypedExpr>) {
    if let ExprNode::And(a, b) = &e.node {
        conjuncts(a, out);
        conjuncts(b, out);
    } else {
        out.push(e);
    }
}
/// Splits `ON` into equality keys `(left slot, right slot)` and residual
/// conjuncts.
fn equi_keys<'e>(
    on: &'e TypedExpr,
    lhs: &RelPlan,
    rhs: &RelPlan,
) -> (Vec<(usize, usize)>, Vec<&'e TypedExpr>) {
    let (lb, rb) = (lhs.bindings(), rhs.bindings());
    let mut parts = Vec::new();
    conjuncts(on, &mut parts);
    let mut keys = Vec::new();
    let mut residual = Vec::new();
    for p in parts {
        if let ExprNode::Compare(Cmp::Eq, a, b) = &p.node {
            if let (
                ExprNode::Column {
                    slot: sa,
                    binding: ba,
                },
                ExprNode::Column {
                    slot: sb,
                    binding: bb,
                },
            ) = (&a.node, &b.node)
            {
                if lb.contains(ba) && rb.contains(bb) {
                    keys.push((*sa, *sb));
                    continue;
                }
                if lb.contains(bb) && rb.contains(ba) {
                    keys.push((*sb, *sa));
                    continue;
                }
            }
        }
        residual.push(p);
    }
    (keys, residual)
}

pub(super) struct Run<'a> {
    pub query: &'a PreparedQuery,
    pub data: &'a DataSet,
    pub params: &'a std::collections::BTreeMap<String, Value>,
    pub engine: &'a VectorEngine,
    pub work: usize,
    pub rows: usize,
    /// Cumulative bytes of materialized intermediate rows, accounted like
    /// the row executor's (`Vec<Value>` per row, `Value` per cell, payload).
    pub bytes: usize,
    pub scratch: Scratch,
    /// Input orders chosen for inner-join chains (table names in execution
    /// order), for inspection.
    pub orders: Vec<Vec<String>>,
}

/// Bytes one copied cell of `src` adds, accounted like a row `Value`.
fn cell_bytes(src: &Column, i: Option<usize>) -> usize {
    let payload = match (i, &src.data) {
        (Some(i), Data::Text(x)) if src.nulls[i] == 0 => x[i].len(),
        (Some(i), Data::Bytes(x)) if src.nulls[i] == 0 => x[i].len(),
        _ => 0,
    };
    size_of::<Value>() + payload
}
impl Run<'_> {
    fn charge(&mut self, cells: usize) -> Result<()> {
        self.work = add(self.work, cells)?;
        bound(self.work, self.query.limits.work, "execution work limit")
    }
    fn produced(&mut self, rows: usize) -> Result<()> {
        self.rows = add(self.rows, rows)?;
        bound(
            self.rows,
            self.query.limits.intermediate_rows,
            "cumulative intermediate row limit",
        )
    }
    /// Charges `rows` rows of `cells` bytes (from [`cell_bytes`]) against
    /// the cumulative intermediate byte budget, before they are copied.
    fn materialize(&mut self, rows: usize, cells: usize) -> Result<()> {
        let row = rows.saturating_mul(size_of::<Vec<Value>>());
        self.bytes = add(self.bytes, add(row, cells)?)?;
        bound(
            self.bytes,
            self.query.limits.intermediate_bytes,
            "cumulative intermediate byte limit",
        )
    }
    fn ty(&self, slot: usize) -> ScalarType {
        self.query.columns[slot].ty
    }
    fn width(&self) -> usize {
        self.query.columns.len()
    }

    /// Evaluates the conjunction `preds` over rows `[start, end)` of `rel`
    /// into `self.scratch.selected` (1 = TRUE).
    fn mask(&mut self, rel: &Rel, start: usize, end: usize, preds: &[&TypedExpr]) -> Result<()> {
        let n = end - start;
        let mut batch = std::mem::take(&mut self.scratch.batch);
        batch.resize_with(self.width(), || None);
        for (slot, col) in rel.cols.iter().enumerate() {
            match (col, &mut batch[slot]) {
                (Some(src), Some(dst))
                    if std::mem::discriminant(&src.data) == std::mem::discriminant(&dst.data) =>
                {
                    dst.clear();
                    for i in start..end {
                        dst.push_from(src, Some(i));
                    }
                }
                (Some(src), dst) => {
                    let mut c = Column::empty(self.ty(slot));
                    for i in start..end {
                        c.push_from(src, Some(i));
                    }
                    *dst = Some(c);
                }
                (None, dst) => *dst = None,
            }
        }
        self.scratch.selected.clear();
        self.scratch.selected.resize(n, 1);
        let ctx = Ctx {
            params: self.params,
            n,
        };
        for p in preds {
            let m = as_bool(eval(p, &batch, &ctx)?, n)?;
            let Data::Bool(v) = &m.data else {
                unreachable!("as_bool returns Bool data");
            };
            let sel = self.scratch.selected[..n].iter_mut();
            for ((s, &b), &null) in sel.zip(&v[..n]).zip(&m.nulls[..n]) {
                *s &= b & !null & 1;
            }
        }
        self.scratch.batch = batch;
        Ok(())
    }

    pub(super) fn rel(&mut self, plan: &RelPlan) -> Result<Rel> {
        match plan {
            RelPlan::Scan {
                table,
                columns,
                predicates,
                ..
            } => {
                let rows = &self.data.tables[table];
                let types: Vec<ScalarType> = columns.iter().map(|&s| self.ty(s)).collect();
                let mut all: Vec<Option<Column>> = vec![None; self.width()];
                for (i, &slot) in columns.iter().enumerate() {
                    all[slot] = Some(Column::gather(
                        types[i],
                        rows.iter().map(|r| r[i].clone()),
                        rows.len(),
                    )?);
                }
                let src = Rel {
                    cols: all,
                    n: rows.len(),
                };
                self.charge(rows.len().saturating_mul(columns.len().max(1)))?;
                let preds: Vec<&TypedExpr> = predicates.iter().collect();
                self.filter(src, &preds)
            }
            RelPlan::Filter { predicate, input } => {
                let input = self.rel(input)?;
                self.filter(input, &[predicate])
            }
            RelPlan::Join { kind, lhs, rhs, on } => {
                let mut leaves = Vec::new();
                let mut on_all = Vec::new();
                flatten_inner(plan, &mut leaves, &mut on_all);
                if leaves.len() >= 3 {
                    return self.inner_chain(&leaves, &on_all);
                }
                let l = self.rel(lhs)?;
                let r = self.rel(rhs)?;
                self.join(*kind, lhs, rhs, on, l, r)
            }
        }
    }

    fn filter(&mut self, rel: Rel, preds: &[&TypedExpr]) -> Result<Rel> {
        if preds.is_empty() {
            self.produced(rel.n)?;
            return Ok(rel);
        }
        let mut out: Vec<Option<Column>> = rel
            .cols
            .iter()
            .enumerate()
            .map(|(slot, c)| c.as_ref().map(|_| Column::empty(self.ty(slot))))
            .collect();
        let mut kept = 0;
        let batch = self.engine.batch;
        let mut start = 0;
        while start < rel.n {
            let end = (start + batch).min(rel.n);
            self.charge((end - start).saturating_mul(preds.len()))?;
            self.mask(&rel, start, end, preds)?;
            if self.engine.count_ones(&self.scratch.selected) > 0 {
                let selected = std::mem::take(&mut self.scratch.selected);
                for (i, _) in (0..end - start)
                    .zip(&selected)
                    .filter(|&(_, &bit)| bit == 1)
                {
                    kept += 1;
                    let cells = rel
                        .cols
                        .iter()
                        .flatten()
                        .try_fold(0, |n, src| add(n, cell_bytes(src, Some(start + i))))?;
                    self.materialize(1, cells)?;
                    for (slot, c) in out.iter_mut().enumerate() {
                        if let (Some(dst), Some(src)) = (c, &rel.cols[slot]) {
                            dst.push_from(src, Some(start + i));
                        }
                    }
                }
                self.scratch.selected = selected;
            }
            start = end;
        }
        self.produced(kept)?;
        Ok(Rel { cols: out, n: kept })
    }

    /// A two-input join with a streaming probe: the hash table is built on
    /// the right input, then left rows are probed in order and their
    /// candidate pairs are produced, filtered by the residual and emitted
    /// one batch of `engine.batch()` candidates at a time, so no more than
    /// one batch of candidate pairs is ever held. Outer rows are
    /// null-extended as the probe passes them.
    fn join(
        &mut self,
        kind: JoinKind,
        lhs: &RelPlan,
        rhs: &RelPlan,
        on: &TypedExpr,
        l: Rel,
        r: Rel,
    ) -> Result<Rel> {
        let (keys, residual) = equi_keys(on, lhs, rhs);
        let lslots = lhs.slots();
        let rslots = rhs.slots();
        let mut table: HashMap<Vec<KeyPart>, Vec<usize>> = HashMap::new();
        let lcols: Vec<&Column> = keys
            .iter()
            .map(|&(s, _)| l.cols[s].as_ref().expect("left key slot"))
            .collect();
        let every_right: Vec<usize> = if keys.is_empty() {
            let pairs =
                l.n.checked_mul(r.n)
                    .ok_or_else(|| Error::new(ErrorKind::Overflow, "resource counter overflow"))?;
            self.charge(pairs)?;
            bound(
                pairs,
                self.query.limits.intermediate_rows,
                "cumulative intermediate row limit",
            )?;
            (0..r.n).collect()
        } else {
            let rcols: Vec<&Column> = keys
                .iter()
                .map(|&(_, s)| r.cols[s].as_ref().expect("right key slot"))
                .collect();
            for j in 0..r.n {
                if let Some(k) = row_key(&rcols, j) {
                    table.entry(k).or_default().push(j);
                }
            }
            self.charge(r.n.saturating_mul(keys.len()))?;
            self.charge(l.n.saturating_mul(keys.len()))?;
            Vec::new()
        };
        let outer_left = matches!(kind, JoinKind::Left | JoinKind::Full);
        let batch = self.engine.batch;
        let mut out_l = std::mem::take(&mut self.scratch.left);
        let mut out_r = std::mem::take(&mut self.scratch.right);
        out_l.clear();
        out_r.clear();
        let mut right_hit = vec![false; r.n];
        let mut left_hit = vec![false; l.n];
        let mut cand_l: Vec<usize> = Vec::with_capacity(batch);
        let mut cand_r: Vec<usize> = Vec::with_capacity(batch);
        // Probe position: left row `i`, its match list from offset `k`.
        let (mut i, mut k) = (0usize, 0usize);
        let mut current: &[usize] = &[];
        // Left rows below `closed` have all their output emitted.
        let mut closed = 0usize;
        loop {
            cand_l.clear();
            cand_r.clear();
            while cand_l.len() < batch && i < l.n {
                if k == 0 {
                    current = if keys.is_empty() {
                        &every_right
                    } else {
                        let js = row_key(&lcols, i)
                            .and_then(|key| table.get(&key))
                            .map_or(&[][..], Vec::as_slice);
                        self.charge(js.len())?;
                        self.produced(js.len())?;
                        js
                    };
                }
                let take = (batch - cand_l.len()).min(current.len() - k);
                cand_l.extend(std::iter::repeat_n(i, take));
                cand_r.extend_from_slice(&current[k..k + take]);
                k += take;
                if k == current.len() {
                    i += 1;
                    k = 0;
                }
            }
            if cand_l.is_empty() {
                break;
            }
            let n = cand_l.len();
            if residual.is_empty() {
                self.scratch.selected.clear();
                self.scratch.selected.resize(n, 1);
            } else {
                let pair = self.pairs(&l, &r, &lslots, &rslots, &cand_l, &cand_r)?;
                self.charge(n.saturating_mul(residual.len() + self.width()))?;
                self.mask(&pair, 0, n, &residual)?;
            }
            for c in 0..n {
                let (li, rj) = (cand_l[c], cand_r[c]);
                while closed < li {
                    if outer_left && !left_hit[closed] {
                        out_l.push(closed);
                        out_r.push(usize::MAX);
                    }
                    closed += 1;
                }
                if self.scratch.selected[c] == 1 {
                    out_l.push(li);
                    out_r.push(rj);
                    left_hit[li] = true;
                    right_hit[rj] = true;
                }
            }
        }
        while closed < l.n {
            if outer_left && !left_hit[closed] {
                out_l.push(closed);
                out_r.push(usize::MAX);
            }
            closed += 1;
        }
        if matches!(kind, JoinKind::Right | JoinKind::Full) {
            for (j, hit) in right_hit.iter().enumerate() {
                if !hit {
                    out_l.push(usize::MAX);
                    out_r.push(j);
                }
            }
        }
        self.produced(out_l.len())?;
        self.charge(out_l.len().saturating_mul(self.width()))?;
        let opt = |x: usize| (x != usize::MAX).then_some(x);
        let mut cells = 0usize;
        for (slots, side, idx) in [(&lslots, &l, &out_l), (&rslots, &r, &out_r)] {
            for &s in slots.iter() {
                let src = side.cols[s].as_ref().expect("bound slot");
                for &x in idx.iter() {
                    cells = add(cells, cell_bytes(src, opt(x)))?;
                }
            }
        }
        self.materialize(out_l.len(), cells)?;
        let mut cols: Vec<Option<Column>> = vec![None; self.width()];
        for (slots, side, idx) in [(&lslots, &l, &out_l), (&rslots, &r, &out_r)] {
            for &s in slots.iter() {
                let src = side.cols[s].as_ref().expect("bound slot");
                let mut c = Column::empty(self.ty(s));
                for &x in idx.iter() {
                    c.push_from(src, opt(x));
                }
                cols[s] = Some(c);
            }
        }
        let n = out_l.len();
        self.scratch.left = out_l;
        self.scratch.right = out_r;
        Ok(Rel { cols, n })
    }

    /// A chain of three or more inner joins, reordered by cost: every input
    /// is materialized (filters included), then a greedy order starts from
    /// the smallest input and repeatedly joins the input with the smallest
    /// estimated result, preferring inputs connected by an equality
    /// (estimate `|current| * |input| / distinct keys of the input`; a
    /// cross product is `|current| * |input|`). Each step is a hash join
    /// on the equalities between the joined set and the new input, with
    /// every conjunct whose inputs are now all present as a residual.
    /// Intermediate results are tuples of input row indexes; the final
    /// tuples are sorted in the written order's left-major order, so rows
    /// and order equal the row executor's.
    fn inner_chain(&mut self, leaves: &[&RelPlan], on: &[&TypedExpr]) -> Result<Rel> {
        let rels = leaves
            .iter()
            .map(|p| self.rel(p))
            .collect::<Result<Vec<Rel>>>()?;
        let binds: Vec<BTreeSet<usize>> = leaves.iter().map(|p| p.bindings()).collect();
        let slots: Vec<Vec<usize>> = leaves.iter().map(|p| p.slots()).collect();
        let leaf_of = |binding: usize| binds.iter().position(|b| b.contains(&binding));
        let order = self.join_order(&rels, &binds, on)?;
        self.orders
            .push(order.iter().map(|&i| leaf_name(leaves[i])).collect());
        let m = leaves.len();
        let mut applied = vec![false; on.len()];
        let mut joined: BTreeSet<usize> = BTreeSet::new();
        let mut tuples: Vec<Vec<usize>> = Vec::new();
        for (step, &c) in order.iter().enumerate() {
            let mut present: BTreeSet<usize> =
                joined.iter().flat_map(|&x| binds[x].clone()).collect();
            // Equalities between the joined inputs and `c` become hash keys.
            let mut keys: Vec<((usize, usize), usize)> = Vec::new();
            if step > 0 {
                for (q, e) in on.iter().enumerate() {
                    if applied[q] {
                        continue;
                    }
                    if let Some(((sa, ba), (sb, bb))) = column_equality(e) {
                        match (leaf_of(ba), leaf_of(bb)) {
                            (Some(la), Some(lb)) if joined.contains(&la) && lb == c => {
                                keys.push(((la, sa), sb));
                                applied[q] = true;
                            }
                            (Some(la), Some(lb)) if joined.contains(&lb) && la == c => {
                                keys.push(((lb, sb), sa));
                                applied[q] = true;
                            }
                            _ => {}
                        }
                    }
                }
            }
            present.extend(binds[c].iter().copied());
            let residual: Vec<&TypedExpr> = on
                .iter()
                .enumerate()
                .filter(|(q, e)| !applied[*q] && e.dependencies().is_subset(&present))
                .map(|(_, e)| *e)
                .collect();
            for (q, e) in on.iter().enumerate() {
                if !applied[q] && e.dependencies().is_subset(&present) {
                    applied[q] = true;
                }
            }
            // Candidates: the first input's rows, or probes of `c`.
            let mut table: HashMap<Vec<KeyPart>, Vec<usize>> = HashMap::new();
            let rc = &rels[c];
            if step > 0 && !keys.is_empty() {
                let rcols: Vec<&Column> = keys
                    .iter()
                    .map(|&(_, s)| rc.cols[s].as_ref().expect("key slot"))
                    .collect();
                for j in 0..rc.n {
                    if let Some(k) = row_key(&rcols, j) {
                        table.entry(k).or_default().push(j);
                    }
                }
                self.charge(rc.n.saturating_mul(keys.len()))?;
            }
            let every: Vec<usize> = (0..rc.n).collect();
            let mut next: Vec<Vec<usize>> = Vec::new();
            let mut cand: Vec<Vec<usize>> = Vec::new();
            let base: Vec<Vec<usize>> = if step == 0 {
                vec![vec![usize::MAX; m]]
            } else {
                std::mem::take(&mut tuples)
            };
            let batch = self.engine.batch;
            let mut members: Vec<usize> = joined.iter().copied().collect();
            members.push(c);
            for t in &base {
                let js: &[usize] = if step == 0 || keys.is_empty() {
                    if step > 0 {
                        self.charge(rc.n)?;
                        self.produced(rc.n)?;
                    }
                    &every
                } else {
                    let parts: Option<Vec<KeyPart>> = keys
                        .iter()
                        .map(|&((leaf, slot), _)| {
                            key_part(rels[leaf].cols[slot].as_ref().expect("key slot"), t[leaf])
                        })
                        .collect();
                    let js = parts
                        .and_then(|k| table.get(&k))
                        .map_or(&[][..], Vec::as_slice);
                    self.charge(js.len().max(1))?;
                    self.produced(js.len())?;
                    js
                };
                for &j in js {
                    let mut nt = t.clone();
                    nt[c] = j;
                    cand.push(nt);
                    if cand.len() >= batch {
                        self.keep(&rels, &slots, &members, &residual, &mut cand, &mut next)?;
                    }
                }
            }
            self.keep(&rels, &slots, &members, &residual, &mut cand, &mut next)?;
            self.produced(next.len())?;
            tuples = next;
            joined.insert(c);
        }
        // Constant conjuncts (no inputs) were applied at the first step.
        debug_assert!(applied.iter().all(|&a| a));
        tuples.sort_unstable();
        let n = tuples.len();
        self.charge(n.saturating_mul(self.width()))?;
        let mut cells = 0usize;
        for (leaf, ss) in slots.iter().enumerate() {
            for &sl in ss {
                let src = rels[leaf].cols[sl].as_ref().expect("bound slot");
                for t in &tuples {
                    cells = add(cells, cell_bytes(src, Some(t[leaf])))?;
                }
            }
        }
        self.materialize(n, cells)?;
        let mut cols: Vec<Option<Column>> = vec![None; self.width()];
        for (leaf, ss) in slots.iter().enumerate() {
            for &sl in ss {
                let src = rels[leaf].cols[sl].as_ref().expect("bound slot");
                let mut col = Column::empty(self.ty(sl));
                for t in &tuples {
                    col.push_from(src, Some(t[leaf]));
                }
                cols[sl] = Some(col);
            }
        }
        Ok(Rel { cols, n })
    }
    /// Filters the candidate tuples by `residual` (a batch gathered from
    /// the inputs in `members`) and moves the survivors to `out`.
    fn keep(
        &mut self,
        rels: &[Rel],
        slots: &[Vec<usize>],
        members: &[usize],
        residual: &[&TypedExpr],
        cand: &mut Vec<Vec<usize>>,
        out: &mut Vec<Vec<usize>>,
    ) -> Result<()> {
        if cand.is_empty() {
            return Ok(());
        }
        if residual.is_empty() {
            out.append(cand);
            return Ok(());
        }
        let n = cand.len();
        let mut bytes = n.saturating_mul(size_of::<Vec<Value>>());
        let mut cols: Vec<Option<Column>> = vec![None; self.width()];
        for &leaf in members {
            for &sl in &slots[leaf] {
                let src = rels[leaf].cols[sl].as_ref().expect("bound slot");
                let mut col = Column::empty(self.ty(sl));
                for t in cand.iter() {
                    bytes = add(bytes, cell_bytes(src, Some(t[leaf])))?;
                    col.push_from(src, Some(t[leaf]));
                }
                cols[sl] = Some(col);
            }
        }
        bound(
            bytes,
            self.query.limits.intermediate_bytes,
            "intermediate byte limit of a candidate-tuple batch",
        )?;
        let rel = Rel { cols, n };
        self.charge(n.saturating_mul(residual.len() + self.width()))?;
        self.mask(&rel, 0, n, residual)?;
        for (t, &bit) in cand.drain(..).zip(&self.scratch.selected) {
            if bit == 1 {
                out.push(t);
            }
        }
        Ok(())
    }
    /// The greedy order of [`Run::inner_chain`] (input positions).
    fn join_order(
        &mut self,
        rels: &[Rel],
        binds: &[BTreeSet<usize>],
        on: &[&TypedExpr],
    ) -> Result<Vec<usize>> {
        let m = rels.len();
        let leaf_of = |binding: usize| binds.iter().position(|b| b.contains(&binding));
        let start = (0..m).min_by_key(|&i| (rels[i].n, i)).expect("inputs");
        let mut order = vec![start];
        let mut est = rels[start].n as f64;
        while order.len() < m {
            let mut best: Option<(bool, f64, usize)> = None;
            for c in (0..m).filter(|c| !order.contains(c)) {
                // Key slots of `c` equal to a column of the joined inputs.
                let mut kslots = Vec::new();
                for e in on {
                    if let Some(((sa, ba), (sb, bb))) = column_equality(e) {
                        match (leaf_of(ba), leaf_of(bb)) {
                            (Some(la), Some(lb)) if order.contains(&la) && lb == c => {
                                kslots.push(sb)
                            }
                            (Some(la), Some(lb)) if order.contains(&lb) && la == c => {
                                kslots.push(sa)
                            }
                            _ => {}
                        }
                    }
                }
                let size = rels[c].n as f64;
                let connected = !kslots.is_empty();
                let e = if connected {
                    let cols: Vec<&Column> = kslots
                        .iter()
                        .map(|&s| rels[c].cols[s].as_ref().expect("key slot"))
                        .collect();
                    let mut distinct = std::collections::HashSet::new();
                    for j in 0..rels[c].n {
                        if let Some(k) = row_key(&cols, j) {
                            distinct.insert(k);
                        }
                    }
                    self.charge(rels[c].n.saturating_mul(cols.len()))?;
                    est * size / (distinct.len().max(1) as f64)
                } else {
                    est * size
                };
                let better = match best {
                    None => true,
                    Some((bc, be, _)) => (connected && !bc) || (connected == bc && e < be),
                };
                if better {
                    best = Some((connected, e, c));
                }
            }
            let (_, e, c) = best.expect("a remaining input");
            order.push(c);
            est = e;
        }
        Ok(order)
    }

    /// The candidate-pair batch: left slots gathered by `li`, right by `ri`.
    fn pairs(
        &self,
        l: &Rel,
        r: &Rel,
        lslots: &[usize],
        rslots: &[usize],
        li: &[usize],
        ri: &[usize],
    ) -> Result<Rel> {
        // A transient batch, reused per batch: bounded on its own by the
        // intermediate byte budget, since repeated wide cells multiply.
        let mut bytes = li.len().saturating_mul(size_of::<Vec<Value>>());
        for (slots, side, idx) in [(lslots, l, li), (rslots, r, ri)] {
            for &s in slots {
                let src = side.cols[s].as_ref().expect("bound slot");
                for &x in idx {
                    bytes = add(bytes, cell_bytes(src, Some(x)))?;
                }
            }
        }
        bound(
            bytes,
            self.query.limits.intermediate_bytes,
            "intermediate byte limit of a candidate-pair batch",
        )?;
        let mut cols: Vec<Option<Column>> = vec![None; self.width()];
        for (slots, side, idx) in [(lslots, l, li), (rslots, r, ri)] {
            for &s in slots {
                let src = side.cols[s].as_ref().expect("bound slot");
                let mut c = Column::empty(self.ty(s));
                for &x in idx {
                    c.push_from(src, Some(x));
                }
                cols[s] = Some(c);
            }
        }
        Ok(Rel { cols, n: li.len() })
    }

    /// Streams projected batches of the materialized plan to `sink`.
    pub(super) fn project(&mut self, rel: &Rel, sink: &mut super::BatchSink<'_>) -> Result<()> {
        let batch = self.engine.batch;
        let mut start = 0;
        let mut sel: Vec<usize> = Vec::with_capacity(batch);
        while start < rel.n {
            let end = (start + batch).min(rel.n);
            self.mask(rel, start, end, &[])?;
            let n = end - start;
            self.charge(n.saturating_mul(self.query.projection.len().max(1)))?;
            let batch_cols = std::mem::take(&mut self.scratch.batch);
            let ctx = Ctx {
                params: self.params,
                n,
            };
            let projected = self
                .query
                .projection
                .iter()
                .map(|e| eval(e, &batch_cols, &ctx))
                .collect::<Result<Vec<Operand<'_>>>>();
            sel.clear();
            sel.extend(0..n);
            let r = projected.and_then(|p| sink(&p, &sel));
            self.scratch.batch = batch_cols;
            r?;
            start = end;
        }
        Ok(())
    }
}

/// The inputs and `ON` conjuncts of a maximal tree of inner joins, inputs
/// in written (left-to-right) order.
fn flatten_inner<'p>(
    plan: &'p RelPlan,
    leaves: &mut Vec<&'p RelPlan>,
    on: &mut Vec<&'p TypedExpr>,
) {
    match plan {
        RelPlan::Join {
            kind: JoinKind::Inner,
            lhs,
            rhs,
            on: e,
        } => {
            flatten_inner(lhs, leaves, on);
            flatten_inner(rhs, leaves, on);
            conjuncts(e, on);
        }
        other => leaves.push(other),
    }
}
/// A readable name of a join input: its table, or its tables in
/// parentheses.
fn leaf_name(plan: &RelPlan) -> String {
    match plan {
        RelPlan::Scan { table, .. } => table.clone(),
        RelPlan::Filter { input, .. } => leaf_name(input),
        RelPlan::Join { lhs, rhs, .. } => format!("({} {})", leaf_name(lhs), leaf_name(rhs)),
    }
}
/// `a = b` between two columns: ((slot, binding), (slot, binding)).
fn column_equality(e: &TypedExpr) -> Option<((usize, usize), (usize, usize))> {
    if let ExprNode::Compare(Cmp::Eq, a, b) = &e.node {
        if let (
            ExprNode::Column {
                slot: sa,
                binding: ba,
            },
            ExprNode::Column {
                slot: sb,
                binding: bb,
            },
        ) = (&a.node, &b.node)
        {
            return Some(((*sa, *ba), (*sb, *bb)));
        }
    }
    None
}

pub(super) fn has_join(plan: &RelPlan) -> bool {
    match plan {
        RelPlan::Scan { .. } => false,
        RelPlan::Filter { input, .. } => has_join(input),
        RelPlan::Join { .. } => true,
    }
}
