//! Bounded host SQL execution (row and vectorized), checked slotted pages,
//! recoverable page pins, and a durable WAL-recovered record store.
//! See the crate README for grammar, resource accounting, and integration boundaries.
#![forbid(unsafe_code)]

mod execute;
mod parse;
mod plan;

pub mod device;
pub mod durable;
pub mod page;
pub mod pool;
pub mod store;
pub mod vector;
pub mod wal;

use std::collections::BTreeMap;
use std::fmt;

pub use execute::{DataSet, QueryResult};
pub use plan::{BoundColumn, JoinKind, PreparedQuery, RelPlan, TypedExpr};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScalarType {
    U64,
    I64,
    F64,
    Bool,
    Text,
    Bytes,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    U64(u64),
    I64(i64),
    F64(f64),
    Bool(bool),
    Text(String),
    Bytes(Vec<u8>),
}
impl Value {
    pub fn scalar_type(&self) -> Option<ScalarType> {
        Some(match self {
            Self::Null => return None,
            Self::U64(_) => ScalarType::U64,
            Self::I64(_) => ScalarType::I64,
            Self::F64(_) => ScalarType::F64,
            Self::Bool(_) => ScalarType::Bool,
            Self::Text(_) => ScalarType::Text,
            Self::Bytes(_) => ScalarType::Bytes,
        })
    }
    pub(crate) fn bytes(&self) -> usize {
        match self {
            Self::Text(x) => x.len(),
            Self::Bytes(x) => x.len(),
            _ => 8,
        }
    }
    pub(crate) fn valid(&self, ty: ScalarType, nullable: bool) -> bool {
        match self {
            Self::Null => nullable,
            Self::F64(v) if !v.is_finite() => false,
            _ => self.scalar_type() == Some(ty),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnDef {
    pub name: String,
    pub ty: ScalarType,
    pub nullable: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParamDef {
    pub name: String,
    pub ty: ScalarType,
    pub nullable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Syntax,
    Unknown,
    Duplicate,
    Ambiguous,
    Type,
    Limit,
    Stale,
    Data,
    Overflow,
    /// A valid query shape this executor does not implement (for example a
    /// join in the vectorized executor).
    Unsupported,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
}
impl Error {
    pub(crate) fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

/// Limits can be lowered, but never raised above the documented hard ceilings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub sql_bytes: usize,
    pub tokens: usize,
    pub nodes: usize,
    pub nesting: usize,
    pub tables: usize,
    pub columns_per_table: usize,
    pub relations: usize,
    pub input_rows: usize,
    pub input_bytes: usize,
    pub work: usize,
    pub intermediate_rows: usize,
    pub intermediate_bytes: usize,
    pub output_rows: usize,
    pub output_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            sql_bytes: 65_536,
            tokens: 8_192,
            nodes: 512,
            nesting: 32,
            tables: 64,
            columns_per_table: 128,
            relations: 16,
            input_rows: 10_000,
            input_bytes: 4_194_304,
            work: 1_000_000,
            intermediate_rows: 10_000,
            intermediate_bytes: 8_388_608,
            output_rows: 10_000,
            output_bytes: 4_194_304,
        }
    }
}
impl Limits {
    pub(crate) fn validate(self) -> Result<()> {
        let hard = Self::default();
        let pairs = [
            (self.sql_bytes, hard.sql_bytes),
            (self.tokens, hard.tokens),
            (self.nodes, hard.nodes),
            (self.nesting, hard.nesting),
            (self.tables, hard.tables),
            (self.columns_per_table, hard.columns_per_table),
            (self.relations, hard.relations),
            (self.input_rows, hard.input_rows),
            (self.input_bytes, hard.input_bytes),
            (self.work, hard.work),
            (self.intermediate_rows, hard.intermediate_rows),
            (self.intermediate_bytes, hard.intermediate_bytes),
            (self.output_rows, hard.output_rows),
            (self.output_bytes, hard.output_bytes),
        ];
        if pairs.iter().any(|(v, h)| v > h) {
            return Err(Error::new(ErrorKind::Limit, "limit exceeds hard ceiling"));
        }
        Ok(())
    }
}
pub(crate) fn add(a: usize, b: usize) -> Result<usize> {
    a.checked_add(b)
        .ok_or_else(|| Error::new(ErrorKind::Overflow, "resource counter overflow"))
}
pub(crate) fn bound(n: usize, max: usize, what: &str) -> Result<()> {
    if n > max {
        Err(Error::new(ErrorKind::Limit, what))
    } else {
        Ok(())
    }
}
pub(crate) fn identifier(s: &str) -> Result<String> {
    let mut bytes = s.bytes();
    if !matches!(bytes.next(), Some(b'a'..=b'z' | b'A'..=b'Z' | b'_'))
        || !bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
        || s.len() > 128
    {
        return Err(Error::new(
            ErrorKind::Syntax,
            "expected ASCII identifier of at most 128 bytes",
        ));
    }
    Ok(s.to_ascii_lowercase())
}

/// One applied DDL statement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DdlChange {
    Create(String, Vec<ColumnDef>),
    Drop(String),
}

/// Ordered, private schema state. Migration replay is atomic and never performs I/O.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HermeticCatalog {
    tables: BTreeMap<String, Vec<ColumnDef>>,
    revision: u64,
}
impl HermeticCatalog {
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn tables(&self) -> &BTreeMap<String, Vec<ColumnDef>> {
        &self.tables
    }
    pub fn apply_ddl(&mut self, sql: &str, limits: Limits) -> Result<()> {
        self.apply_ddl_changes(sql, limits).map(drop)
    }
    /// `apply_ddl`, also returning each applied statement in order.
    pub(crate) fn apply_ddl_changes(&mut self, sql: &str, limits: Limits) -> Result<Vec<DdlChange>> {
        limits.validate()?;
        let statements = parse::ddl(sql, limits)?;
        let mut next = self.clone();
        let mut changes = Vec::with_capacity(statements.len());
        for statement in statements {
            match statement {
                parse::Ddl::Create(name, cols) => {
                    if next.tables.contains_key(&name) {
                        return Err(Error::new(ErrorKind::Duplicate, format!("table {name}")));
                    }
                    bound(
                        add(next.tables.len(), 1)?,
                        limits.tables,
                        "catalog table limit",
                    )?;
                    next.tables.insert(name.clone(), cols.clone());
                    changes.push(DdlChange::Create(name, cols));
                }
                parse::Ddl::Drop(name) => {
                    if next.tables.remove(&name).is_none() {
                        return Err(Error::new(ErrorKind::Unknown, format!("table {name}")));
                    }
                    changes.push(DdlChange::Drop(name));
                }
            }
            next.revision = next
                .revision
                .checked_add(1)
                .ok_or_else(|| Error::new(ErrorKind::Overflow, "catalog revision overflow"))?;
        }
        *self = next;
        Ok(changes)
    }
    /// Rebuilds a catalog persisted by the durable table store.
    pub(crate) fn from_parts(tables: BTreeMap<String, Vec<ColumnDef>>, revision: u64) -> Self {
        Self { tables, revision }
    }
    pub fn prepare(&self, sql: &str, params: &[ParamDef], limits: Limits) -> Result<PreparedQuery> {
        limits.validate()?;
        bound(self.tables.len(), limits.tables, "catalog table limit")?;
        for cols in self.tables.values() {
            bound(cols.len(), limits.columns_per_table, "catalog column limit")?;
        }
        plan::prepare(self, parse::select(sql, limits)?, params, limits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checked_counters_and_revision_overflow_fail_atomically() {
        assert_eq!(add(usize::MAX, 1).unwrap_err().kind, ErrorKind::Overflow);
        let mut catalog = HermeticCatalog {
            revision: u64::MAX,
            ..HermeticCatalog::default()
        };
        let before = catalog.clone();
        assert_eq!(
            catalog
                .apply_ddl("CREATE TABLE x (id I64)", Limits::default())
                .unwrap_err()
                .kind,
            ErrorKind::Overflow
        );
        assert_eq!(catalog, before);
    }
}
