use crate::{add, Error, ErrorKind, HermeticCatalog, Limits, PreparedQuery, Result, Value};
use std::collections::BTreeMap;
use std::mem::size_of;
/// Data is explicitly tied to a complete schema snapshot; mutations validate before replacement.
#[derive(Clone, Debug)]
pub struct DataSet {
    pub(crate) catalog: HermeticCatalog,
    pub(crate) tables: BTreeMap<String, Vec<Vec<Value>>>,
}
impl DataSet {
    pub fn new(catalog: &HermeticCatalog) -> Self {
        Self {
            catalog: catalog.clone(),
            tables: BTreeMap::new(),
        }
    }
    pub fn tables(&self) -> &BTreeMap<String, Vec<Vec<Value>>> {
        &self.tables
    }
    /// Borrows incoming rows so every validation failure preserves the caller's data.
    /// Clones into the dataset only after the complete preflight succeeds.
    pub fn set_rows(&mut self, table: &str, rows: &[Vec<Value>], limits: Limits) -> Result<()> {
        let table = crate::port::set_rows(self, table, rows, limits)?;
        self.tables.insert(table, rows.to_vec());
        Ok(())
    }
}
pub(crate) fn validate_rows(
    catalog: &HermeticCatalog,
    table: &str,
    rows: &[Vec<Value>],
) -> Result<()> {
    let cols = catalog
        .tables
        .get(table)
        .ok_or_else(|| Error::new(ErrorKind::Unknown, format!("data table {table}")))?;
    for row in rows {
        if row.len() != cols.len() {
            return Err(Error::new(
                ErrorKind::Data,
                format!("row width for {table}"),
            ));
        }
        for (value, col) in row.iter().zip(cols) {
            if !value.valid(col.ty, col.nullable) {
                return Err(Error::new(
                    ErrorKind::Data,
                    format!("invalid value for {table}.{}", col.name),
                ));
            }
        }
    }
    Ok(())
}
pub(crate) fn row_bytes(row: &[Value]) -> Result<usize> {
    row.iter().try_fold(0, |n, v| {
        add(
            add(n, size_of::<Value>())?,
            match v {
                Value::Text(s) => s.len(),
                Value::Bytes(b) => b.len(),
                _ => 0,
            },
        )
    })
}
pub(crate) fn rows_bytes(rows: &[Vec<Value>]) -> Result<usize> {
    rows.iter().try_fold(0, |n, r| {
        add(n, add(size_of::<Vec<Value>>(), row_bytes(r)?)?)
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueryResult {
    pub columns: Vec<crate::ColumnDef>,
    pub rows: Vec<Vec<Value>>,
    pub work: usize,
}
impl PreparedQuery {
    pub fn execute(
        &self,
        catalog: &HermeticCatalog,
        data: &DataSet,
        params: &BTreeMap<String, Value>,
    ) -> Result<QueryResult> {
        if catalog != &self.catalog || data.catalog != self.catalog {
            return Err(Error::new(
                ErrorKind::Stale,
                "prepared query or data schema snapshot differs in revision/content",
            ));
        }
        crate::port::execute(self, catalog, data, params)
    }
}
