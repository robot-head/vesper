//! Vectorized (column-batch) execution of single-table scans, filters,
//! projections and aggregates for checked [`PreparedQuery`] plans.
//!
//! Rows are transposed into typed column batches (`Vec<u64>`, `Vec<i64>`,
//! `Vec<f64>`, byte booleans, strings, byte strings, each with a byte null
//! mask). Predicates run as whole-batch loops producing SQL three-valued
//! byte masks. Selected rows are counted by the `vesper-simd` Baseline128
//! JIT byte scanner when the host supports it (a portable count otherwise);
//! that count sizes the selection vector and answers `COUNT` aggregates.
//! Comparison loops are plain Rust over slices; whether rustc vectorizes
//! them is not claimed. Joins are rejected with `ErrorKind::Unsupported`.
use crate::execute::{check_inputs, row_bytes, rows_bytes, validate_rows, DataSet, QueryResult};
use crate::parse::Cmp;
use crate::plan::{ExprNode, PreparedQuery, RelPlan, TypedExpr};
use crate::{add, bound, Error, ErrorKind, HermeticCatalog, Result, ScalarType, Value};
use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use vesper_simd::{IsaProfileKind, JitScanner};

/// Rows per column batch unless configured otherwise.
pub const DEFAULT_BATCH: usize = 1024;
const MAX_BATCH: usize = 1 << 16;

/// Selection-mask counter plus batch size. Not `Send` when it holds JIT
/// code (the scanner keeps its module in an `Rc`).
pub struct VectorEngine {
    scanner: Option<JitScanner>,
    batch: usize,
}
impl std::fmt::Debug for VectorEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VectorEngine")
            .field("simd", &self.simd_active())
            .field("batch", &self.batch)
            .finish()
    }
}
impl VectorEngine {
    /// Uses the Baseline128 JIT scanner when the host supports it.
    pub fn detect() -> Self {
        Self {
            scanner: JitScanner::compile_profile(IsaProfileKind::Baseline128).ok(),
            batch: DEFAULT_BATCH,
        }
    }
    /// Portable byte counting only (for differential tests and hosts
    /// without Baseline128).
    pub fn portable() -> Self {
        Self {
            scanner: None,
            batch: DEFAULT_BATCH,
        }
    }
    /// Batch size 1..=65,536 rows.
    pub fn with_batch(mut self, rows: usize) -> Result<Self> {
        if rows == 0 || rows > MAX_BATCH {
            return Err(Error::new(ErrorKind::Limit, "batch size out of range"));
        }
        self.batch = rows;
        Ok(self)
    }
    pub fn simd_active(&self) -> bool {
        self.scanner.is_some()
    }
    pub fn batch(&self) -> usize {
        self.batch
    }
    /// Number of bytes equal to 1 in a 0/1 mask.
    pub fn count_ones(&self, mask: &[u8]) -> usize {
        match &self.scanner {
            Some(s) => s.scan(mask, 1).matches as usize,
            None => mask.iter().filter(|&&b| b == 1).count(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Data {
    U64(Vec<u64>),
    I64(Vec<i64>),
    F64(Vec<f64>),
    /// 0 or 1 per row.
    Bool(Vec<u8>),
    Text(Vec<String>),
    Bytes(Vec<Vec<u8>>),
}
/// One typed column of a batch. `nulls[i]` is 1 for SQL NULL; the data
/// slot then holds a placeholder.
#[derive(Clone, Debug, PartialEq)]
struct Column {
    data: Data,
    nulls: Vec<u8>,
}
impl Column {
    fn gather(ty: ScalarType, values: impl Iterator<Item = Value>, len: usize) -> Result<Self> {
        let mut nulls = Vec::with_capacity(len);
        macro_rules! collect {
            ($variant:ident, $default:expr) => {{
                let mut out = Vec::with_capacity(len);
                for v in values {
                    match v {
                        Value::$variant(x) => {
                            out.push(x);
                            nulls.push(0);
                        }
                        Value::Null => {
                            out.push($default);
                            nulls.push(1);
                        }
                        _ => return Err(Error::new(ErrorKind::Type, "column value type")),
                    }
                }
                Data::$variant(out)
            }};
        }
        let data = match ty {
            ScalarType::U64 => collect!(U64, 0),
            ScalarType::I64 => collect!(I64, 0),
            ScalarType::F64 => collect!(F64, 0.0),
            ScalarType::Text => collect!(Text, String::new()),
            ScalarType::Bytes => collect!(Bytes, Vec::new()),
            ScalarType::Bool => {
                let mut out = Vec::with_capacity(len);
                for v in values {
                    match v {
                        Value::Bool(b) => {
                            out.push(u8::from(b));
                            nulls.push(0);
                        }
                        Value::Null => {
                            out.push(0);
                            nulls.push(1);
                        }
                        _ => return Err(Error::new(ErrorKind::Type, "column value type")),
                    }
                }
                Data::Bool(out)
            }
        };
        Ok(Self { data, nulls })
    }
    fn value(&self, i: usize) -> Value {
        if self.nulls[i] == 1 {
            return Value::Null;
        }
        match &self.data {
            Data::U64(x) => Value::U64(x[i]),
            Data::I64(x) => Value::I64(x[i]),
            Data::F64(x) => Value::F64(x[i]),
            Data::Bool(x) => Value::Bool(x[i] == 1),
            Data::Text(x) => Value::Text(x[i].clone()),
            Data::Bytes(x) => Value::Bytes(x[i].clone()),
        }
    }
    fn broadcast(v: &Value, ty: Option<ScalarType>, len: usize) -> Result<Self> {
        let ty = match (v.scalar_type(), ty) {
            (Some(t), _) | (None, Some(t)) => t,
            (None, None) => ScalarType::Bool,
        };
        Self::gather(ty, std::iter::repeat_n(v.clone(), len), len)
    }
}

/// An evaluated expression over one batch: a column (borrowed or computed)
/// or a scalar shared by every row.
enum Operand<'a> {
    Col(Cow<'a, Column>),
    Scalar(Cow<'a, Value>),
}

fn bool_column(val: Vec<u8>, nulls: Vec<u8>) -> Column {
    Column {
        data: Data::Bool(val),
        nulls,
    }
}

macro_rules! cmp_loops {
    ($op:expr, $n:expr, $out:ident, $a:expr, $b:expr) => {
        match $op {
            Cmp::Eq => (0..$n).for_each(|i| $out[i] = u8::from($a(i) == $b(i))),
            Cmp::Ne => (0..$n).for_each(|i| $out[i] = u8::from($a(i) != $b(i))),
            Cmp::Lt => (0..$n).for_each(|i| $out[i] = u8::from($a(i) < $b(i))),
            Cmp::Le => (0..$n).for_each(|i| $out[i] = u8::from($a(i) <= $b(i))),
            Cmp::Gt => (0..$n).for_each(|i| $out[i] = u8::from($a(i) > $b(i))),
            Cmp::Ge => (0..$n).for_each(|i| $out[i] = u8::from($a(i) >= $b(i))),
        }
    };
}
fn ordered(op: Cmp, o: Ordering) -> bool {
    match op {
        Cmp::Eq => o == Ordering::Equal,
        Cmp::Ne => o != Ordering::Equal,
        Cmp::Lt => o == Ordering::Less,
        Cmp::Le => o != Ordering::Greater,
        Cmp::Gt => o == Ordering::Greater,
        Cmp::Ge => o != Ordering::Less,
    }
}

/// Typed side of a comparison: a slice, or a broadcast scalar.
enum Side<'a, T> {
    Slice(&'a [T]),
    One(&'a T),
}
impl<T> Side<'_, T> {
    #[inline]
    fn get(&self, i: usize) -> &T {
        match self {
            Side::Slice(s) => &s[i],
            Side::One(x) => x,
        }
    }
}
/// Ordered comparison by reference: variable-length scalars are never
/// copied per row.
fn ord_compare<T: Ord>(op: Cmp, n: usize, a: Side<T>, b: Side<T>, nulls: &[u8]) -> Vec<u8> {
    (0..n)
        .map(|i| {
            if nulls[i] == 1 {
                0
            } else {
                u8::from(ordered(op, a.get(i).cmp(b.get(i))))
            }
        })
        .collect()
}
impl<T: Copy> Side<'_, T> {
    #[inline]
    fn at(&self, i: usize) -> T {
        match self {
            Side::Slice(s) => s[i],
            Side::One(x) => **x,
        }
    }
}
fn fixed_compare<T: Copy + PartialOrd>(op: Cmp, n: usize, a: Side<T>, b: Side<T>) -> Vec<u8> {
    let mut out = vec![0u8; n];
    // Hoist the common column-versus-constant shape into straight slice loops.
    match (&a, &b) {
        (Side::Slice(x), Side::One(y)) => {
            let y = **y;
            cmp_loops!(op, n, out, |i: usize| x[i], |_| y);
        }
        (Side::Slice(x), Side::Slice(y)) => {
            cmp_loops!(op, n, out, |i: usize| x[i], |i: usize| y[i]);
        }
        _ => cmp_loops!(op, n, out, |i| a.at(i), |i| b.at(i)),
    }
    out
}

struct Ctx<'a> {
    params: &'a BTreeMap<String, Value>,
    n: usize,
}

fn eval<'a>(expr: &TypedExpr, batch: &'a [Option<Column>], ctx: &Ctx<'a>) -> Result<Operand<'a>> {
    Ok(match &expr.node {
        ExprNode::Column { slot, .. } => {
            Operand::Col(Cow::Borrowed(batch[*slot].as_ref().ok_or_else(|| {
                Error::new(ErrorKind::Unsupported, "column of another binding")
            })?))
        }
        ExprNode::Param(name) => Operand::Scalar(Cow::Borrowed(&ctx.params[name])),
        ExprNode::Literal(v) => Operand::Scalar(Cow::Owned(v.clone())),
        ExprNode::Compare(op, a, b) => {
            let ty = a.scalar_type().or(b.scalar_type());
            let a = eval(a, batch, ctx)?;
            let b = eval(b, batch, ctx)?;
            compare(*op, a, b, ty, ctx.n)?
        }
        ExprNode::And(a, b) | ExprNode::Or(a, b) => {
            let is_and = matches!(expr.node, ExprNode::And(..));
            let a = as_bool(eval(a, batch, ctx)?, ctx.n)?;
            let b = as_bool(eval(b, batch, ctx)?, ctx.n)?;
            let (Data::Bool(av), Data::Bool(bv)) = (&a.data, &b.data) else {
                unreachable!("as_bool returns Bool data");
            };
            let mut val = vec![0u8; ctx.n];
            let mut nulls = vec![0u8; ctx.n];
            for i in 0..ctx.n {
                let (at, af) = (av[i] & !a.nulls[i] & 1, !av[i] & !a.nulls[i] & 1);
                let (bt, bf) = (bv[i] & !b.nulls[i] & 1, !bv[i] & !b.nulls[i] & 1);
                let (t, f) = if is_and {
                    (at & bt, af | bf)
                } else {
                    (at | bt, af & bf)
                };
                val[i] = t;
                nulls[i] = !(t | f) & 1;
            }
            Operand::Col(Cow::Owned(bool_column(val, nulls)))
        }
        ExprNode::Not(x) => {
            let x = as_bool(eval(x, batch, ctx)?, ctx.n)?;
            let Data::Bool(v) = &x.data else {
                unreachable!("as_bool returns Bool data");
            };
            let val = (0..ctx.n).map(|i| !v[i] & !x.nulls[i] & 1).collect();
            Operand::Col(Cow::Owned(bool_column(val, x.nulls.clone())))
        }
        ExprNode::IsNull(x, negated) => {
            let flip = u8::from(*negated);
            match eval(x, batch, ctx)? {
                Operand::Scalar(v) => {
                    Operand::Scalar(Cow::Owned(Value::Bool((*v == Value::Null) != *negated)))
                }
                Operand::Col(c) => Operand::Col(Cow::Owned(bool_column(
                    c.nulls.iter().map(|&b| b ^ flip).collect(),
                    vec![0; ctx.n],
                ))),
            }
        }
    })
}

fn as_bool(op: Operand<'_>, n: usize) -> Result<Cow<'_, Column>> {
    let col = match op {
        Operand::Col(c) => c,
        Operand::Scalar(v) => Cow::Owned(Column::broadcast(&v, Some(ScalarType::Bool), n)?),
    };
    if matches!(col.data, Data::Bool(_)) {
        Ok(col)
    } else {
        Err(Error::new(ErrorKind::Type, "non-boolean checked predicate"))
    }
}

fn compare(
    op: Cmp,
    a: Operand<'_>,
    b: Operand<'_>,
    ty: Option<ScalarType>,
    n: usize,
) -> Result<Operand<'static>> {
    if let (Operand::Scalar(x), Operand::Scalar(y)) = (&a, &b) {
        // Same rule as the row executor for constant operands.
        let v = match (&**x, &**y) {
            (Value::Null, _) | (_, Value::Null) => Value::Null,
            (x, y) => Value::Bool(ordered(op, scalar_order(x, y)?)),
        };
        return Ok(Operand::Scalar(Cow::Owned(v)));
    }
    let null_scalar = |o: &Operand<'_>| matches!(o, Operand::Scalar(v) if **v == Value::Null);
    if null_scalar(&a) || null_scalar(&b) {
        return Ok(Operand::Col(Cow::Owned(bool_column(
            vec![0; n],
            vec![1; n],
        ))));
    }
    let nulls_of = |o: &Operand<'_>| -> Vec<u8> {
        match o {
            Operand::Col(c) => c.nulls.clone(),
            Operand::Scalar(_) => vec![0; n],
        }
    };
    let mut nulls = nulls_of(&a);
    for (x, y) in nulls.iter_mut().zip(nulls_of(&b)) {
        *x |= y;
    }
    macro_rules! side {
        ($o:expr, $variant:ident) => {
            match $o {
                Operand::Col(c) => match &c.data {
                    Data::$variant(s) => Side::Slice(s.as_slice()),
                    _ => return Err(Error::new(ErrorKind::Type, "comparison mismatch")),
                },
                Operand::Scalar(v) => match &**v {
                    Value::$variant(x) => Side::One(x),
                    _ => return Err(Error::new(ErrorKind::Type, "comparison mismatch")),
                },
            }
        };
    }
    let mut val = match ty {
        Some(ScalarType::U64) => fixed_compare(op, n, side!(&a, U64), side!(&b, U64)),
        Some(ScalarType::I64) => fixed_compare(op, n, side!(&a, I64), side!(&b, I64)),
        // Finite values only, so IEEE comparison equals the row executor's
        // partial_cmp (+0 == -0).
        Some(ScalarType::F64) => fixed_compare(op, n, side!(&a, F64), side!(&b, F64)),
        Some(ScalarType::Text) => ord_compare(op, n, side!(&a, Text), side!(&b, Text), &nulls),
        Some(ScalarType::Bytes) => ord_compare(op, n, side!(&a, Bytes), side!(&b, Bytes), &nulls),
        Some(ScalarType::Bool) => {
            // Booleans are stored as 0/1 bytes; false < true matches bool's order.
            let (mut sa, mut sb) = (0u8, 0u8);
            let x = bool_side(&a, &mut sa)?;
            let y = bool_side(&b, &mut sb)?;
            fixed_compare(op, n, x, y)
        }
        None => return Err(Error::new(ErrorKind::Type, "comparison without a type")),
    };
    for (v, &z) in val.iter_mut().zip(&nulls) {
        *v &= !z & 1;
    }
    Ok(Operand::Col(Cow::Owned(bool_column(val, nulls))))
}
fn bool_side<'a>(o: &'a Operand<'_>, store: &'a mut u8) -> Result<Side<'a, u8>> {
    match o {
        Operand::Col(c) => match &c.data {
            Data::Bool(s) => Ok(Side::Slice(s.as_slice())),
            _ => Err(Error::new(ErrorKind::Type, "comparison mismatch")),
        },
        Operand::Scalar(v) => match **v {
            Value::Bool(b) => {
                *store = u8::from(b);
                Ok(Side::One(store))
            }
            _ => Err(Error::new(ErrorKind::Type, "comparison mismatch")),
        },
    }
}
fn scalar_order(a: &Value, b: &Value) -> Result<Ordering> {
    Ok(match (a, b) {
        (Value::U64(a), Value::U64(b)) => a.cmp(b),
        (Value::I64(a), Value::I64(b)) => a.cmp(b),
        (Value::F64(a), Value::F64(b)) => a
            .partial_cmp(b)
            .ok_or_else(|| Error::new(ErrorKind::Data, "non-finite float"))?,
        (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
        (Value::Text(a), Value::Text(b)) => a.cmp(b),
        (Value::Bytes(a), Value::Bytes(b)) => a.cmp(b),
        _ => return Err(Error::new(ErrorKind::Type, "comparison mismatch")),
    })
}

/// SQL aggregates over output column indexes of a query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aggregate {
    /// `COUNT(*)`.
    CountRows,
    /// `COUNT(col)`: non-null values.
    Count(usize),
    /// `SUM(col)` over U64, I64 or F64. Integer sums are exact and fail with
    /// `Overflow` iff the exact total does not fit; F64 sums add in row order.
    Sum(usize),
    /// Smallest non-null value; the first one wins among equals (+0/-0).
    Min(usize),
    Max(usize),
}

#[derive(Clone, Debug)]
enum Acc {
    Count(u64),
    IntSum(i128, ScalarType, bool),
    FloatSum(f64, bool),
    Extreme(Option<Value>, bool),
}
impl Acc {
    fn new(agg: Aggregate, output: &[crate::ColumnDef]) -> Result<Self> {
        let ty = |i: usize| {
            output
                .get(i)
                .map(|c| c.ty)
                .ok_or_else(|| Error::new(ErrorKind::Unknown, "aggregate column index"))
        };
        Ok(match agg {
            Aggregate::CountRows => Acc::Count(0),
            Aggregate::Count(i) => {
                ty(i)?;
                Acc::Count(0)
            }
            Aggregate::Sum(i) => match ty(i)? {
                t @ (ScalarType::U64 | ScalarType::I64) => Acc::IntSum(0, t, false),
                ScalarType::F64 => Acc::FloatSum(0.0, false),
                _ => return Err(Error::new(ErrorKind::Type, "SUM needs a numeric column")),
            },
            Aggregate::Min(i) => {
                ty(i)?;
                Acc::Extreme(None, false)
            }
            Aggregate::Max(i) => {
                ty(i)?;
                Acc::Extreme(None, true)
            }
        })
    }
    fn push(&mut self, agg: Aggregate, v: Option<&Value>) -> Result<()> {
        let Some(v) = v else {
            if let Acc::Count(n) = self {
                *n += 1;
            }
            return Ok(());
        };
        if *v == Value::Null {
            return Ok(());
        }
        match (self, v) {
            (Acc::Count(n), _) => *n += 1,
            (Acc::IntSum(s, _, seen), Value::U64(x)) => {
                *s += i128::from(*x);
                *seen = true;
            }
            (Acc::IntSum(s, _, seen), Value::I64(x)) => {
                *s += i128::from(*x);
                *seen = true;
            }
            (Acc::FloatSum(s, seen), Value::F64(x)) => {
                *s += x;
                *seen = true;
            }
            (Acc::Extreme(best, max), v) => {
                let replace = match best {
                    None => true,
                    Some(b) => {
                        let o = scalar_order(v, b)?;
                        if *max {
                            o == Ordering::Greater
                        } else {
                            o == Ordering::Less
                        }
                    }
                };
                if replace {
                    *best = Some(v.clone());
                }
            }
            _ => return Err(Error::new(ErrorKind::Type, format!("{agg:?} input"))),
        }
        Ok(())
    }
    fn finish(self) -> Result<Value> {
        Ok(match self {
            Acc::Count(n) => Value::U64(n),
            Acc::IntSum(_, _, false) | Acc::FloatSum(_, false) => Value::Null,
            Acc::IntSum(s, ScalarType::U64, true) => Value::U64(
                u64::try_from(s).map_err(|_| Error::new(ErrorKind::Overflow, "SUM overflow"))?,
            ),
            Acc::IntSum(s, _, true) => Value::I64(
                i64::try_from(s).map_err(|_| Error::new(ErrorKind::Overflow, "SUM overflow"))?,
            ),
            Acc::FloatSum(s, true) => Value::F64(s),
            Acc::Extreme(v, _) => v.unwrap_or(Value::Null),
        })
    }
}

/// Reference row-at-a-time aggregation over a materialized result (for
/// example from the row executor). Same semantics as the vectorized path.
pub fn aggregate_rows(result: &QueryResult, aggs: &[Aggregate]) -> Result<Vec<Value>> {
    let mut accs = aggs
        .iter()
        .map(|a| Acc::new(*a, &result.columns))
        .collect::<Result<Vec<_>>>()?;
    for row in &result.rows {
        for (acc, agg) in accs.iter_mut().zip(aggs) {
            let v = match agg {
                Aggregate::CountRows => None,
                Aggregate::Count(i) | Aggregate::Sum(i) | Aggregate::Min(i) | Aggregate::Max(i) => {
                    Some(&row[*i])
                }
            };
            acc.push(*agg, v)?;
        }
    }
    accs.into_iter().map(Acc::finish).collect()
}

/// Receives each batch's projected operands and selected row indexes.
type BatchSink<'s> = dyn FnMut(&[Operand<'_>], &[usize]) -> Result<()> + 's;

/// Single-table scan with its conjunctive predicates, in evaluation order.
fn flatten(plan: &RelPlan) -> Result<(&str, &[usize], Vec<&TypedExpr>)> {
    match plan {
        RelPlan::Scan {
            table,
            columns,
            predicates,
            ..
        } => Ok((table, columns, predicates.iter().collect())),
        RelPlan::Filter { predicate, input } => {
            let (t, c, mut p) = flatten(input)?;
            p.push(predicate);
            Ok((t, c, p))
        }
        RelPlan::Join { .. } => Err(Error::new(
            ErrorKind::Unsupported,
            "vectorized execution supports single-table plans only",
        )),
    }
}

impl PreparedQuery {
    fn check_call(
        &self,
        catalog: &HermeticCatalog,
        data: &DataSet,
        params: &BTreeMap<String, Value>,
    ) -> Result<()> {
        if catalog != &self.catalog || data.catalog != self.catalog {
            return Err(Error::new(
                ErrorKind::Stale,
                "prepared query or data schema snapshot differs in revision/content",
            ));
        }
        if params.len() != self.params.len() {
            return Err(Error::new(
                ErrorKind::Data,
                "parameter set differs from declarations",
            ));
        }
        let mut bytes = 0;
        for (name, def) in &self.params {
            let v = params
                .get(name)
                .ok_or_else(|| Error::new(ErrorKind::Data, format!("missing parameter :{name}")))?;
            if !v.valid(def.ty, def.nullable) {
                return Err(Error::new(ErrorKind::Type, format!("parameter :{name}")));
            }
            bytes = add(bytes, add(size_of::<Value>(), v.bytes())?)?;
        }
        bound(bytes, self.limits.input_bytes, "parameter byte limit")?;
        let mut count = 0;
        for (table, rows) in &data.tables {
            count = add(count, rows.len())?;
            bytes = add(bytes, rows_bytes(rows)?)?;
            bound(count, self.limits.input_rows, "input row limit")?;
            bound(bytes, self.limits.input_bytes, "input byte limit")?;
            validate_rows(catalog, table, rows)?;
        }
        check_inputs(&self.plan, data)
    }

    /// Runs the plan batch by batch; `sink` receives the projected operands
    /// and the selected row indexes of each batch.
    fn drive(
        &self,
        catalog: &HermeticCatalog,
        data: &DataSet,
        params: &BTreeMap<String, Value>,
        engine: &VectorEngine,
        sink: &mut BatchSink<'_>,
    ) -> Result<usize> {
        self.check_call(catalog, data, params)?;
        let (table, slots, predicates) = flatten(&self.plan)?;
        let rows = &data.tables[table];
        let types: Vec<ScalarType> = catalog.tables[table].iter().map(|c| c.ty).collect();
        let nodes = predicates
            .iter()
            .chain(self.projection.iter().collect::<Vec<_>>().iter())
            .map(|e| expr_nodes(e))
            .sum::<usize>()
            .max(1);
        let mut work = 0usize;
        for chunk in rows.chunks(engine.batch) {
            let n = chunk.len();
            work = add(
                work,
                n.checked_mul(add(nodes, slots.len())?)
                    .ok_or_else(|| Error::new(ErrorKind::Overflow, "resource counter overflow"))?,
            )?;
            bound(work, self.limits.work, "execution work limit")?;
            let mut batch: Vec<Option<Column>> = vec![None; self.columns.len()];
            for (i, &slot) in slots.iter().enumerate() {
                batch[slot] = Some(Column::gather(
                    types[i],
                    chunk.iter().map(|r| r[i].clone()),
                    n,
                )?);
            }
            let ctx = Ctx { params, n };
            let mut selected = vec![1u8; n];
            for p in &predicates {
                let mask = as_bool(eval(p, &batch, &ctx)?, n)?;
                let Data::Bool(v) = &mask.data else {
                    unreachable!("as_bool returns Bool data");
                };
                for i in 0..n {
                    selected[i] &= v[i] & !mask.nulls[i] & 1;
                }
            }
            let count = engine.count_ones(&selected);
            if count == 0 {
                continue;
            }
            let mut sel = Vec::with_capacity(count);
            sel.extend((0..n).filter(|&i| selected[i] == 1));
            let projected = self
                .projection
                .iter()
                .map(|e| eval(e, &batch, &ctx))
                .collect::<Result<Vec<_>>>()?;
            sink(&projected, &sel)?;
        }
        Ok(work)
    }

    /// Vectorized execution of a single-table plan. Rows, order and column
    /// definitions equal [`PreparedQuery::execute`]; `work` counts batch
    /// cells and is not comparable with the row executor's counter.
    pub fn execute_vectorized(
        &self,
        catalog: &HermeticCatalog,
        data: &DataSet,
        params: &BTreeMap<String, Value>,
        engine: &VectorEngine,
    ) -> Result<QueryResult> {
        let mut out: Vec<Vec<Value>> = Vec::new();
        let mut bytes = 0usize;
        let limits = self.limits;
        let work = self.drive(catalog, data, params, engine, &mut |cols, sel| {
            for &i in sel {
                bound(add(out.len(), 1)?, limits.output_rows, "output row limit")?;
                let row: Vec<Value> = cols
                    .iter()
                    .map(|c| match c {
                        Operand::Col(c) => c.value(i),
                        Operand::Scalar(v) => (**v).clone(),
                    })
                    .collect();
                bytes = add(bytes, add(size_of::<Vec<Value>>(), row_bytes(&row)?)?)?;
                bound(bytes, limits.output_bytes, "output byte limit")?;
                out.push(row);
            }
            Ok(())
        })?;
        Ok(QueryResult {
            columns: self.output.clone(),
            rows: out,
            work,
        })
    }

    /// Vectorized aggregates over the query's output columns, without
    /// materializing output rows. `COUNT` uses the SIMD mask count.
    pub fn aggregate_vectorized(
        &self,
        catalog: &HermeticCatalog,
        data: &DataSet,
        params: &BTreeMap<String, Value>,
        engine: &VectorEngine,
        aggs: &[Aggregate],
    ) -> Result<Vec<Value>> {
        let mut accs = aggs
            .iter()
            .map(|a| Acc::new(*a, &self.output))
            .collect::<Result<Vec<_>>>()?;
        self.drive(catalog, data, params, engine, &mut |cols, sel| {
            for (acc, agg) in accs.iter_mut().zip(aggs) {
                match (agg, &mut *acc) {
                    (Aggregate::CountRows, Acc::Count(n)) => *n += sel.len() as u64,
                    (Aggregate::Count(i), Acc::Count(n)) => match &cols[*i] {
                        Operand::Scalar(v) => {
                            if **v != Value::Null {
                                *n += sel.len() as u64;
                            }
                        }
                        Operand::Col(c) => {
                            let live: Vec<u8> = sel.iter().map(|&r| !c.nulls[r] & 1).collect();
                            *n += engine.count_ones(&live) as u64;
                        }
                    },
                    (Aggregate::Sum(i), Acc::IntSum(s, _, seen)) => {
                        if let Operand::Col(c) = &cols[*i] {
                            let mut total = 0i128;
                            match &c.data {
                                Data::U64(x) => sel
                                    .iter()
                                    .filter(|&&r| c.nulls[r] == 0)
                                    .for_each(|&r| total += i128::from(x[r])),
                                Data::I64(x) => sel
                                    .iter()
                                    .filter(|&&r| c.nulls[r] == 0)
                                    .for_each(|&r| total += i128::from(x[r])),
                                _ => return Err(Error::new(ErrorKind::Type, "SUM input")),
                            }
                            *seen |= sel.iter().any(|&r| c.nulls[r] == 0);
                            *s += total;
                        } else {
                            for _ in sel {
                                acc.push(*agg, Some(&scalar_of(&cols[*i])))?;
                            }
                        }
                    }
                    (Aggregate::Sum(i), Acc::FloatSum(s, seen)) => {
                        if let Operand::Col(c) = &cols[*i] {
                            let Data::F64(x) = &c.data else {
                                return Err(Error::new(ErrorKind::Type, "SUM input"));
                            };
                            for &r in sel {
                                if c.nulls[r] == 0 {
                                    *s += x[r];
                                    *seen = true;
                                }
                            }
                        } else {
                            for _ in sel {
                                acc.push(*agg, Some(&scalar_of(&cols[*i])))?;
                            }
                        }
                    }
                    (Aggregate::Min(i) | Aggregate::Max(i), _) => {
                        for &r in sel {
                            let v = match &cols[*i] {
                                Operand::Col(c) => c.value(r),
                                Operand::Scalar(v) => (**v).clone(),
                            };
                            acc.push(*agg, Some(&v))?;
                        }
                    }
                    _ => unreachable!("accumulator matches its aggregate"),
                }
            }
            Ok(())
        })?;
        accs.into_iter().map(Acc::finish).collect()
    }
}
fn scalar_of(o: &Operand<'_>) -> Value {
    match o {
        Operand::Scalar(v) => (**v).clone(),
        Operand::Col(_) => Value::Null,
    }
}
fn expr_nodes(e: &TypedExpr) -> usize {
    match &e.node {
        ExprNode::Compare(_, a, b) | ExprNode::And(a, b) | ExprNode::Or(a, b) => {
            1 + expr_nodes(a) + expr_nodes(b)
        }
        ExprNode::Not(x) | ExprNode::IsNull(x, _) => 1 + expr_nodes(x),
        _ => 1,
    }
}
