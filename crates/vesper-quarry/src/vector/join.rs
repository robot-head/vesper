//! Multi-table vectorized plans: column-materialized relations, hash joins
//! (equality conjuncts of `ON` as keys, the rest as a residual predicate
//! evaluated over candidate-pair batches), vectorized nested-loop joins
//! when `ON` has no equality between the two sides, and filters, all over
//! column batches of `engine.batch()` rows with reused scratch buffers.
//!
//! Output order equals the row executor's: left-major, matches in right
//! order, an unmatched left row right after its (absent) matches, unmatched
//! right rows at the end.
use super::{as_bool, eval, Column, Ctx, Data, Operand, VectorEngine};
use crate::execute::DataSet;
use crate::parse::Cmp;
use crate::plan::{ExprNode, JoinKind, PreparedQuery, RelPlan, TypedExpr};
use crate::{add, bound, Error, ErrorKind, Result, ScalarType, Value};
use std::collections::HashMap;

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
        // Candidate pairs, left-major and right-ascending.
        let mut cand_l: Vec<usize> = Vec::new();
        let mut cand_r: Vec<usize> = Vec::new();
        if keys.is_empty() {
            let pairs =
                l.n.checked_mul(r.n)
                    .ok_or_else(|| Error::new(ErrorKind::Overflow, "resource counter overflow"))?;
            self.charge(pairs)?;
            bound(
                pairs,
                self.query.limits.intermediate_rows,
                "cumulative intermediate row limit",
            )?;
            for i in 0..l.n {
                for j in 0..r.n {
                    cand_l.push(i);
                    cand_r.push(j);
                }
            }
        } else {
            let lcols: Vec<&Column> = keys
                .iter()
                .map(|&(s, _)| l.cols[s].as_ref().expect("left key slot"))
                .collect();
            let rcols: Vec<&Column> = keys
                .iter()
                .map(|&(_, s)| r.cols[s].as_ref().expect("right key slot"))
                .collect();
            let mut table: HashMap<Vec<KeyPart>, Vec<usize>> = HashMap::new();
            for j in 0..r.n {
                if let Some(k) = row_key(&rcols, j) {
                    table.entry(k).or_default().push(j);
                }
            }
            self.charge(r.n.saturating_mul(keys.len()))?;
            for i in 0..l.n {
                if let Some(k) = row_key(&lcols, i) {
                    if let Some(js) = table.get(&k) {
                        self.charge(js.len())?;
                        self.produced(js.len())?;
                        cand_l.extend(std::iter::repeat_n(i, js.len()));
                        cand_r.extend(js.iter().copied());
                    }
                }
            }
            self.charge(l.n.saturating_mul(keys.len()))?;
        }
        // Residual predicate over candidate-pair batches.
        let lslots = lhs.slots();
        let rslots = rhs.slots();
        let mut matched_l: Vec<usize> = Vec::with_capacity(cand_l.len());
        let mut matched_r: Vec<usize> = Vec::with_capacity(cand_r.len());
        if residual.is_empty() {
            matched_l = cand_l;
            matched_r = cand_r;
        } else {
            let batch = self.engine.batch;
            let mut start = 0;
            while start < cand_l.len() {
                let end = (start + batch).min(cand_l.len());
                let pair = self.pairs(
                    &l,
                    &r,
                    &lslots,
                    &rslots,
                    &cand_l[start..end],
                    &cand_r[start..end],
                )?;
                self.charge((end - start).saturating_mul(residual.len() + self.width()))?;
                self.mask(&pair, 0, end - start, &residual)?;
                for k in 0..end - start {
                    if self.scratch.selected[k] == 1 {
                        matched_l.push(cand_l[start + k]);
                        matched_r.push(cand_r[start + k]);
                    }
                }
                start = end;
            }
        }
        // Assemble in row-executor order, null-extending outer rows.
        let mut out_l = std::mem::take(&mut self.scratch.left);
        let mut out_r = std::mem::take(&mut self.scratch.right);
        out_l.clear();
        out_r.clear();
        let mut right_hit = vec![false; r.n];
        let mut m = 0;
        for i in 0..l.n {
            let mut any = false;
            while m < matched_l.len() && matched_l[m] == i {
                out_l.push(i);
                out_r.push(matched_r[m]);
                right_hit[matched_r[m]] = true;
                any = true;
                m += 1;
            }
            if !any && matches!(kind, JoinKind::Left | JoinKind::Full) {
                out_l.push(i);
                out_r.push(usize::MAX);
            }
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

pub(super) fn has_join(plan: &RelPlan) -> bool {
    match plan {
        RelPlan::Scan { .. } => false,
        RelPlan::Filter { input, .. } => has_join(input),
        RelPlan::Join { .. } => true,
    }
}
