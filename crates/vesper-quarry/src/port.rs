//! The `vesper-port` build: the SQL front end, catalog DDL replay,
//! optimizer and row executor run in Vesper (`vesper/src/lib.ves`), reached
//! through the generated `port/binding.rs` (port wave P1 lane 7, #904).
//!
//! The crate's types stay in Rust (the vectorized executor and the store
//! read them), so this module only encodes inputs as flat word streams plus
//! one text pool, and rebuilds the crate's types from the streams the port
//! returns. Every text field is an `(offset, length)` range of a pool.
//!
//! Encodings (shared with the comments in `vesper/src/lib.ves`):
//! * scalar type: its index (`U64` 0 .. `Bytes` 5); a type word is 0 for
//!   none and the index plus one otherwise;
//! * value: `[tag, a, b]` with tags `Null` 0, `U64` 1, `I64` 2, `F64` 3
//!   (`a` = bits), `Bool` 4, `Text` 5 and `Bytes` 6 (`a, b` = pool range);
//! * catalog: `[ntables, (name, ncols, (name, ty, nullable)*)*]`;
//! * typed expression: `[tag, type word, nullable, payload..]`, see
//!   [`Dec::expr`];
//! * plan: `Scan [0, table, binding, nslots, slots.., npreds, preds..]`,
//!   `Filter [1, predicate, input]`, `Join [2, kind, lhs, rhs, on]`.
//!
//! `prepare` passes its catalog as slices of the Vesper build's own
//! `CTab`/`CCol` mirrors instead (export ABI v3 lends them in place; port
//! parity lane 7, #955).
//!
//! `Q` handles own their Vesper allocations and are freed on drop.
#![allow(unsafe_code)]

use crate::execute::{rows_bytes, validate_rows, DataSet, QueryResult};
use crate::parse::Cmp;
use crate::plan::{
    AccessPath, AccessPlan, BindingPath, BoundColumn, ExprNode, IndexBound, JoinKind,
    PreparedQuery, RelPlan, TypedExpr,
};
use crate::{
    bound, identifier, ColumnDef, DdlChange, Error, ErrorKind, HermeticCatalog, Limits, ParamDef,
    Result, ScalarType, Value,
};
use std::collections::BTreeMap;
use std::mem::size_of;
use vesper_port_abi::Word;

#[rustfmt::skip]
#[path = "../port/binding.rs"]
pub(crate) mod binding;

const KINDS: [ErrorKind; 10] = [
    ErrorKind::Syntax,
    ErrorKind::Unknown,
    ErrorKind::Duplicate,
    ErrorKind::Ambiguous,
    ErrorKind::Type,
    ErrorKind::Limit,
    ErrorKind::Stale,
    ErrorKind::Data,
    ErrorKind::Overflow,
    ErrorKind::Unsupported,
];
const TYPES: [ScalarType; 6] = [
    ScalarType::U64,
    ScalarType::I64,
    ScalarType::F64,
    ScalarType::Bool,
    ScalarType::Text,
    ScalarType::Bytes,
];

fn ty_index(ty: ScalarType) -> u64 {
    TYPES.iter().position(|t| *t == ty).unwrap_or(0) as u64
}
fn cmp_index(c: Cmp) -> u64 {
    match c {
        Cmp::Eq => 0,
        Cmp::Ne => 1,
        Cmp::Lt => 2,
        Cmp::Le => 3,
        Cmp::Gt => 4,
        Cmp::Ge => 5,
    }
}
const CMPS: [Cmp; 6] = [Cmp::Eq, Cmp::Ne, Cmp::Lt, Cmp::Le, Cmp::Gt, Cmp::Ge];
fn kind_index(k: JoinKind) -> u64 {
    match k {
        JoinKind::Inner => 0,
        JoinKind::Left => 1,
        JoinKind::Right => 2,
        JoinKind::Full => 3,
    }
}
const JOINS: [JoinKind; 4] = [
    JoinKind::Inner,
    JoinKind::Left,
    JoinKind::Right,
    JoinKind::Full,
];

fn limit_words(l: Limits) -> [u64; 16] {
    [
        l.sql_bytes,
        l.tokens,
        l.nodes,
        l.nesting,
        l.tables,
        l.columns_per_table,
        l.relations,
        l.input_rows,
        l.input_bytes,
        l.work,
        l.intermediate_rows,
        l.intermediate_bytes,
        l.output_rows,
        l.output_bytes,
        size_of::<Value>(),
        size_of::<Vec<Value>>(),
    ]
    .map(|x| x as u64)
}

/// An input under construction: words plus the text pool they point into.
#[derive(Default)]
struct Enc {
    words: Vec<u64>,
    text: Vec<u8>,
}
impl Enc {
    /// An encoder with room for a typical call, so that small inputs do not
    /// grow the buffers word by word.
    fn sized(words: usize, text: usize) -> Self {
        Self {
            words: Vec::with_capacity(words),
            text: Vec::with_capacity(text),
        }
    }
    fn w(&mut self, x: u64) {
        self.words.push(x);
    }
    fn bytes(&mut self, b: &[u8]) {
        let off = self.text.len() as u64;
        self.text.extend_from_slice(b);
        self.w(off);
        self.w(b.len() as u64);
    }
    fn s(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }
    fn value(&mut self, v: &Value) {
        match v {
            Value::Null => self.words.extend([0, 0, 0]),
            Value::U64(x) => self.words.extend([1, *x, 0]),
            Value::I64(x) => self.words.extend([2, *x as u64, 0]),
            Value::F64(x) => self.words.extend([3, x.to_bits(), 0]),
            Value::Bool(x) => self.words.extend([4, u64::from(*x), 0]),
            Value::Text(s) => {
                self.w(5);
                self.s(s);
            }
            Value::Bytes(b) => {
                self.w(6);
                self.bytes(b);
            }
        }
    }
    /// A value whose text or bytes are only counted, not copied (the port
    /// reads nothing but their length).
    fn value_len(&mut self, v: &Value) {
        match v {
            Value::Text(s) => self.words.extend([5, 0, s.len() as u64]),
            Value::Bytes(b) => self.words.extend([6, 0, b.len() as u64]),
            v => self.value(v),
        }
    }
    fn rows(&mut self, rows: &[Vec<Value>], counted: bool) {
        self.w(rows.len() as u64);
        for row in rows {
            self.w(row.len() as u64);
            for v in row {
                if counted {
                    self.value_len(v);
                } else {
                    self.value(v);
                }
            }
        }
    }
    fn catalog(&mut self, c: &HermeticCatalog) {
        self.w(c.tables.len() as u64);
        for (name, cols) in &c.tables {
            self.s(name);
            self.w(cols.len() as u64);
            for col in cols {
                self.s(&col.name);
                self.w(ty_index(col.ty));
                self.w(u64::from(col.nullable));
            }
        }
    }
    /// The catalog as the Vesper build's own `CTab`/`CCol` storage (export
    /// ABI v3 lends both slices in place), with the names in the text pool.
    fn catalog_mirrors(
        &mut self,
        c: &HermeticCatalog,
    ) -> (Vec<binding::repr::CTab>, Vec<binding::repr::CCol>) {
        let mut tabs = Vec::with_capacity(c.tables.len());
        let mut cols = Vec::with_capacity(c.tables.values().map(Vec::len).sum());
        for (name, defs) in &c.tables {
            let (noff, nlen) = self.range(name.as_bytes());
            tabs.push(binding::repr::CTab {
                noff,
                nlen,
                start: cols.len() as u64,
                n: defs.len() as u64,
            });
            for col in defs {
                let (noff, nlen) = self.range(col.name.as_bytes());
                cols.push(binding::repr::CCol {
                    noff,
                    nlen,
                    ty: ty_index(col.ty),
                    nullable: Word::new(col.nullable),
                    used: Word::new(false),
                });
            }
        }
        (tabs, cols)
    }
    /// Appends `b` to the text pool and returns its range.
    fn range(&mut self, b: &[u8]) -> (u64, u64) {
        let off = self.text.len() as u64;
        self.text.extend_from_slice(b);
        (off, b.len() as u64)
    }
    fn expr(&mut self, e: &TypedExpr) {
        let tag = match &e.node {
            ExprNode::Column { .. } => 0,
            ExprNode::Param(_) => 1,
            ExprNode::Literal(_) => 2,
            ExprNode::Compare(..) => 3,
            ExprNode::And(..) => 4,
            ExprNode::Or(..) => 5,
            ExprNode::Not(_) => 6,
            ExprNode::IsNull(..) => 7,
        };
        self.w(tag);
        self.w(e.ty.map_or(0, |t| ty_index(t) + 1));
        self.w(u64::from(e.nullable));
        match &e.node {
            ExprNode::Column { slot, binding } => {
                self.w(*slot as u64);
                self.w(*binding as u64);
            }
            ExprNode::Param(name) => self.s(name),
            ExprNode::Literal(v) => self.value(v),
            ExprNode::Compare(op, a, b) => {
                self.w(cmp_index(*op));
                self.expr(a);
                self.expr(b);
            }
            ExprNode::And(a, b) | ExprNode::Or(a, b) => {
                self.expr(a);
                self.expr(b);
            }
            ExprNode::Not(x) => self.expr(x),
            ExprNode::IsNull(x, negated) => {
                self.w(u64::from(*negated));
                self.expr(x);
            }
        }
    }
    fn plan(&mut self, p: &RelPlan) {
        match p {
            RelPlan::Scan {
                table,
                binding,
                columns,
                predicates,
            } => {
                self.w(0);
                self.s(table);
                self.w(*binding as u64);
                self.w(columns.len() as u64);
                for &c in columns {
                    self.w(c as u64);
                }
                self.w(predicates.len() as u64);
                for e in predicates {
                    self.expr(e);
                }
            }
            RelPlan::Filter { predicate, input } => {
                self.w(1);
                self.expr(predicate);
                self.plan(input);
            }
            RelPlan::Join { kind, lhs, rhs, on } => {
                self.w(2);
                self.w(kind_index(*kind));
                self.plan(lhs);
                self.plan(rhs);
                self.expr(on);
            }
        }
    }
}

/// A reader over a returned word stream and its pool.
struct Dec<'a> {
    words: &'a [u64],
    pool: &'a [u8],
    at: usize,
}
impl Dec<'_> {
    fn w(&mut self) -> u64 {
        let x = self.words.get(self.at).copied().unwrap_or(0);
        self.at += 1;
        x
    }
    fn n(&mut self) -> usize {
        self.w() as usize
    }
    fn b(&mut self) -> bool {
        self.w() != 0
    }
    fn bytes(&mut self) -> &[u8] {
        let off = self.n();
        let len = self.n();
        self.pool.get(off..off + len).unwrap_or_default()
    }
    fn raw(&mut self) -> Vec<u8> {
        self.bytes().to_vec()
    }
    /// One allocation per string: the port only returns valid UTF-8, so
    /// the lossy conversion borrows and `into_owned` copies once.
    fn s(&mut self) -> String {
        String::from_utf8_lossy(self.bytes()).into_owned()
    }
    fn ty(&mut self) -> ScalarType {
        TYPES[self.n().min(5)]
    }
    fn value(&mut self) -> Value {
        match self.w() {
            1 => {
                let x = self.w();
                self.w();
                Value::U64(x)
            }
            2 => {
                let x = self.w();
                self.w();
                Value::I64(x as i64)
            }
            3 => {
                let x = self.w();
                self.w();
                Value::F64(f64::from_bits(x))
            }
            4 => {
                let x = self.b();
                self.w();
                Value::Bool(x)
            }
            5 => Value::Text(self.s()),
            6 => Value::Bytes(self.raw()),
            _ => {
                self.w();
                self.w();
                Value::Null
            }
        }
    }
    /// `[tag, type word, nullable, payload..]`: Column 0 `slot, binding`,
    /// Param 1 `name`, Literal 2 `value`, Compare 3 `cmp, lhs, rhs`, And 4
    /// and Or 5 `lhs, rhs`, Not 6 `operand`, IsNull 7 `negated, operand`.
    fn expr(&mut self) -> TypedExpr {
        let tag = self.w();
        let ty = match self.n() {
            0 => None,
            t => Some(TYPES[(t - 1).min(5)]),
        };
        let nullable = self.b();
        let node = match tag {
            0 => {
                let slot = self.n();
                let binding = self.n();
                ExprNode::Column { slot, binding }
            }
            1 => ExprNode::Param(self.s()),
            2 => ExprNode::Literal(self.value()),
            3 => {
                let op = CMPS[self.n().min(5)];
                let a = self.expr();
                let b = self.expr();
                ExprNode::Compare(op, Box::new(a), Box::new(b))
            }
            4 => {
                let a = self.expr();
                ExprNode::And(Box::new(a), Box::new(self.expr()))
            }
            5 => {
                let a = self.expr();
                ExprNode::Or(Box::new(a), Box::new(self.expr()))
            }
            6 => ExprNode::Not(Box::new(self.expr())),
            _ => {
                let negated = self.b();
                ExprNode::IsNull(Box::new(self.expr()), negated)
            }
        };
        TypedExpr { node, ty, nullable }
    }
    fn plan(&mut self) -> RelPlan {
        match self.w() {
            0 => {
                let table = self.s();
                let binding = self.n();
                let columns = (0..self.n()).map(|_| self.n()).collect();
                let predicates = (0..self.n()).map(|_| self.expr()).collect();
                RelPlan::Scan {
                    table,
                    binding,
                    columns,
                    predicates,
                }
            }
            1 => {
                let predicate = self.expr();
                RelPlan::Filter {
                    predicate,
                    input: Box::new(self.plan()),
                }
            }
            _ => {
                let kind = JOINS[self.n().min(3)];
                let lhs = Box::new(self.plan());
                let rhs = Box::new(self.plan());
                RelPlan::Join {
                    kind,
                    lhs,
                    rhs,
                    on: self.expr(),
                }
            }
        }
    }
}

/// The error of a failed call, or the handle of a successful one.
fn checked(q: binding::Q) -> Result<binding::Q> {
    let r = q.as_ref();
    match r.err() {
        0 => Ok(q),
        k => Err(Error::new(
            KINDS[(k as usize - 1).min(9)],
            String::from_utf8_lossy(&r.msg().to_vec()).into_owned(),
        )),
    }
}

/// `HermeticCatalog::apply_ddl_changes` through the port.
pub(crate) fn apply_ddl(
    catalog: &mut HermeticCatalog,
    sql: &str,
    limits: Limits,
) -> Result<Vec<DdlChange>> {
    let mut e = Enc::default();
    e.catalog(catalog);
    let q = checked(binding::ddl_apply(
        &e.words,
        &e.text,
        catalog.revision,
        sql.as_bytes(),
        &limit_words(limits),
    ))?;
    let r = q.as_ref();
    let (words, pool) = (r.words().to_vec(), r.pool().to_vec());
    let mut d = Dec {
        words: &words,
        pool: &pool,
        at: 0,
    };
    let mut next = catalog.clone();
    let mut changes = Vec::new();
    for _ in 0..d.n() {
        if d.w() == 0 {
            let name = d.s();
            let cols: Vec<ColumnDef> = (0..d.n())
                .map(|_| ColumnDef {
                    name: d.s(),
                    ty: d.ty(),
                    nullable: d.b(),
                })
                .collect();
            next.tables.insert(name.clone(), cols.clone());
            changes.push(DdlChange::Create(name, cols));
        } else {
            let name = d.s();
            next.tables.remove(&name);
            changes.push(DdlChange::Drop(name));
        }
        // The port has already rejected a revision overflow.
        next.revision = next.revision.wrapping_add(1);
    }
    *catalog = next;
    Ok(changes)
}

/// `HermeticCatalog::prepare` through the port.
pub(crate) fn prepare(
    catalog: &HermeticCatalog,
    sql: &str,
    params: &[ParamDef],
    limits: Limits,
) -> Result<PreparedQuery> {
    let mut e = Enc::sized(16, 64);
    let (tabs, cols) = e.catalog_mirrors(catalog);
    e.w(params.len() as u64);
    for p in params {
        e.s(&p.name);
        e.w(ty_index(p.ty));
        e.w(u64::from(p.nullable));
    }
    let q = checked(binding::prepare(
        &tabs,
        &cols,
        &e.words,
        &e.text,
        sql.as_bytes(),
        &limit_words(limits),
    ))?;
    let r = q.as_ref();
    let (words, pool) = (r.words().to_vec(), r.pool().to_vec());
    let mut d = Dec {
        words: &words,
        pool: &pool,
        at: 0,
    };
    let plan = d.plan();
    let columns = (0..d.n())
        .map(|_| BoundColumn {
            alias: d.s(),
            column: d.s(),
            binding: d.n(),
            slot: d.n(),
            ty: d.ty(),
            nullable: d.b(),
        })
        .collect();
    let projection = (0..d.n()).map(|_| d.expr()).collect();
    let output = (0..d.n())
        .map(|_| ColumnDef {
            name: d.s(),
            ty: d.ty(),
            nullable: d.b(),
        })
        .collect();
    let params = (0..d.n())
        .map(|_| {
            let def = ParamDef {
                name: d.s(),
                ty: d.ty(),
                nullable: d.b(),
            };
            (def.name.clone(), def)
        })
        .collect();
    Ok(PreparedQuery {
        catalog: catalog.clone(),
        limits,
        plan,
        columns,
        projection,
        output,
        params,
    })
}

/// `RelPlan::optimize` through the port.
pub(crate) fn optimize(plan: &RelPlan) -> RelPlan {
    let mut e = Enc::sized(64, 0);
    e.plan(plan);
    let q = binding::optimize(&e.words);
    let words = q.as_ref().words().to_vec();
    Dec {
        words: &words,
        pool: &e.text,
        at: 0,
    }
    .plan()
}

/// `DataSet::set_rows` checks through the port; returns the normalized
/// table name.
pub(crate) fn set_rows(
    data: &DataSet,
    table: &str,
    rows: &[Vec<Value>],
    limits: Limits,
) -> Result<String> {
    // Input over its own budget is rejected on borrowed data before
    // anything is copied into the ABI, with the Rust build's checks in its
    // order (every path below returns an error for such input).
    let own_bytes = rows_bytes(rows);
    if rows.len() > limits.input_rows
        || own_bytes.as_ref().map_or(true, |&b| b > limits.input_bytes)
    {
        limits.validate()?;
        let table = identifier(table)?;
        bound(rows.len(), limits.input_rows, "input row limit")?;
        validate_rows(&data.catalog, &table, rows)?;
        bound(own_bytes?, limits.input_bytes, "input byte limit")?;
    }
    set_rows_encoded(data, table, rows, limits)
}

fn set_rows_encoded(
    data: &DataSet,
    table: &str,
    rows: &[Vec<Value>],
    limits: Limits,
) -> Result<String> {
    let mut e = Enc::default();
    e.words.extend(limit_words(limits));
    e.s(table);
    e.catalog(&data.catalog);
    e.rows(rows, false);
    e.w(data.tables.len() as u64);
    for (name, existing) in &data.tables {
        e.s(name);
        e.rows(existing, true);
    }
    let q = checked(binding::set_rows(&e.words, &e.text))?;
    let r = q.as_ref();
    let (words, pool) = (r.words().to_vec(), r.pool().to_vec());
    Ok(Dec {
        words: &words,
        pool: &pool,
        at: 0,
    }
    .s())
}

/// `PreparedQuery::execute` (after the snapshot check) through the port.
pub(crate) fn execute(
    query: &PreparedQuery,
    catalog: &HermeticCatalog,
    data: &DataSet,
    params: &BTreeMap<String, Value>,
) -> Result<QueryResult> {
    let mut e = Enc::default();
    e.words.extend(limit_words(query.limits));
    e.w(query.columns.len() as u64);
    e.plan(&query.plan);
    e.w(query.projection.len() as u64);
    for p in &query.projection {
        e.expr(p);
    }
    e.w(query.params.len() as u64);
    for def in query.params.values() {
        e.s(&def.name);
        e.w(ty_index(def.ty));
        e.w(u64::from(def.nullable));
    }
    e.w(params.len() as u64);
    for (name, v) in params {
        e.s(name);
        e.value(v);
    }
    e.catalog(catalog);
    e.w(data.tables.len() as u64);
    for (name, rows) in &data.tables {
        e.s(name);
        e.rows(rows, false);
    }
    let q = checked(binding::execute(&e.words, &e.text))?;
    let words = q.as_ref().words().to_vec();
    let mut d = Dec {
        words: &words,
        pool: &e.text,
        at: 0,
    };
    let work = d.n();
    let width = query.projection.len();
    let rows = (0..d.n())
        .map(|_| (0..width).map(|_| d.value()).collect())
        .collect();
    Ok(QueryResult {
        columns: query.output.clone(),
        rows,
        work,
    })
}

/// `PreparedQuery::access_plan` through the port.
pub(crate) fn access_plan(
    query: &PreparedQuery,
    data: &DataSet,
    indexes: &BTreeMap<String, (String, String)>,
    params: &BTreeMap<String, Value>,
) -> Result<AccessPlan> {
    let mut e = Enc::default();
    e.plan(&optimize(&query.plan));
    e.w(params.len() as u64);
    for (name, v) in params {
        e.s(name);
        e.value(v);
    }
    e.catalog(&query.catalog);
    e.w(data.tables.len() as u64);
    for (name, rows) in &data.tables {
        e.s(name);
        e.rows(rows, false);
    }
    e.w(indexes.len() as u64);
    for (name, (table, column)) in indexes {
        e.s(name);
        e.s(table);
        e.s(column);
    }
    let q = checked(binding::access_paths(&e.words, &e.text))?;
    let words = q.as_ref().words().to_vec();
    let mut d = Dec {
        words: &words,
        pool: &e.text,
        at: 0,
    };
    let alias = |binding: usize| {
        query
            .columns
            .iter()
            .find(|c| c.binding == binding)
            .map_or_else(String::new, |c| c.alias.clone())
    };
    let mut bindings = Vec::new();
    for b in 0..words.len() / 8 {
        let kind = d.w();
        let index = d.s();
        let bound = [
            IndexBound::Eq,
            IndexBound::Range,
            IndexBound::Lower,
            IndexBound::Upper,
        ][d.n().min(3)];
        let outer = d.n();
        let slot = d.n();
        let path = match kind {
            0 => AccessPath::Scan,
            1 => AccessPath::Index { index, bound },
            _ => AccessPath::IndexJoin {
                index,
                outer: alias(outer),
                column: query
                    .columns
                    .get(slot)
                    .map_or_else(String::new, |c| c.column.clone()),
            },
        };
        bindings.push(BindingPath {
            alias: alias(b),
            path,
            rows: d.w(),
            cost: d.w(),
        });
    }
    Ok(AccessPlan { bindings })
}

/// Live Vesper allocations (for leak tests).
pub(crate) fn live_allocations() -> i64 {
    binding::live_allocations()
}
