use crate::parse::Cmp;
use crate::plan::ExprNode;
use crate::{
    add, bound, identifier, Error, ErrorKind, HermeticCatalog, JoinKind, Limits, PreparedQuery,
    RelPlan, Result, TypedExpr, Value,
};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::mem::size_of;

/// Data is explicitly tied to a complete schema snapshot; mutations validate before replacement.
#[derive(Clone, Debug)]
pub struct DataSet {
    catalog: HermeticCatalog,
    tables: BTreeMap<String, Vec<Vec<Value>>>,
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
        limits.validate()?;
        let table = identifier(table)?;
        bound(rows.len(), limits.input_rows, "input row limit")?;
        validate_rows(&self.catalog, &table, rows)?;
        let mut count = rows.len();
        let mut bytes = rows_bytes(rows)?;
        for (name, existing) in &self.tables {
            if name != &table {
                count = add(count, existing.len())?;
                bytes = add(bytes, rows_bytes(existing)?)?;
            }
        }
        bound(count, limits.input_rows, "input row limit")?;
        bound(bytes, limits.input_bytes, "input byte limit")?;
        self.tables.insert(table, rows.to_vec());
        Ok(())
    }
}
fn validate_rows(catalog: &HermeticCatalog, table: &str, rows: &[Vec<Value>]) -> Result<()> {
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
fn row_bytes(row: &[Value]) -> Result<usize> {
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
fn rows_bytes(rows: &[Vec<Value>]) -> Result<usize> {
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
struct Budget {
    limits: Limits,
    work: usize,
    rows: usize,
    bytes: usize,
}
impl Budget {
    fn tick(&mut self, n: usize) -> Result<()> {
        self.work = add(self.work, n)?;
        bound(self.work, self.limits.work, "execution work limit")
    }
    fn row(&mut self, row: &[Value]) -> Result<()> {
        self.rows = add(self.rows, 1)?;
        self.bytes = add(self.bytes, add(size_of::<Vec<Value>>(), row_bytes(row)?)?)?;
        bound(
            self.rows,
            self.limits.intermediate_rows,
            "cumulative intermediate row limit",
        )?;
        bound(
            self.bytes,
            self.limits.intermediate_bytes,
            "cumulative intermediate byte limit",
        )?;
        self.tick(add(
            row.len(),
            row.iter().try_fold(0, |n, v| add(n, v.bytes()))?,
        )?)
    }
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
        if params.len() != self.params.len() {
            return Err(Error::new(
                ErrorKind::Data,
                "parameter set differs from declarations",
            ));
        }
        let mut budget = Budget {
            limits: self.limits,
            work: 0,
            rows: 0,
            bytes: 0,
        };
        let mut parameter_bytes = 0;
        for (name, def) in &self.params {
            let v = params
                .get(name)
                .ok_or_else(|| Error::new(ErrorKind::Data, format!("missing parameter :{name}")))?;
            if !v.valid(def.ty, def.nullable) {
                return Err(Error::new(ErrorKind::Type, format!("parameter :{name}")));
            }
            parameter_bytes = add(parameter_bytes, add(size_of::<Value>(), v.bytes())?)?;
        }
        bound(
            parameter_bytes,
            self.limits.input_bytes,
            "parameter byte limit",
        )?;
        let mut count = 0;
        let mut bytes = parameter_bytes;
        for (table, rows) in &data.tables {
            count = add(count, rows.len())?;
            bytes = add(bytes, rows_bytes(rows)?)?;
            bound(count, self.limits.input_rows, "input row limit")?;
            bound(bytes, self.limits.input_bytes, "input byte limit")?;
            validate_rows(catalog, table, rows)?;
            for row in rows {
                budget.tick(add(1, row.len())?)?;
            }
        }
        // Missing input must fail even if an optimizer could eliminate its rows.
        check_inputs(&self.plan, data)?;
        let rows = run(&self.plan, data, params, self.columns.len(), &mut budget)?;
        let mut out = Vec::new();
        let mut output_bytes = 0;
        for row in rows {
            bound(
                add(out.len(), 1)?,
                self.limits.output_rows,
                "output row limit",
            )?;
            let projected: Vec<Value> = self
                .projection
                .iter()
                .map(|expr| eval(expr, &row, params, &mut budget))
                .collect::<Result<_>>()?;
            output_bytes = add(
                output_bytes,
                add(size_of::<Vec<Value>>(), row_bytes(&projected)?)?,
            )?;
            bound(output_bytes, self.limits.output_bytes, "output byte limit")?;
            out.push(projected);
        }
        Ok(QueryResult {
            columns: self.output.clone(),
            rows: out,
            work: budget.work,
        })
    }
}
fn check_inputs(plan: &RelPlan, data: &DataSet) -> Result<()> {
    match plan {
        RelPlan::Scan { table, .. } => {
            if data.tables.contains_key(table) {
                Ok(())
            } else {
                Err(Error::new(
                    ErrorKind::Data,
                    format!("missing data for {table}"),
                ))
            }
        }
        RelPlan::Filter { input, .. } => check_inputs(input, data),
        RelPlan::Join { lhs, rhs, .. } => {
            check_inputs(lhs, data)?;
            check_inputs(rhs, data)
        }
    }
}
fn run(
    plan: &RelPlan,
    data: &DataSet,
    params: &BTreeMap<String, Value>,
    width: usize,
    budget: &mut Budget,
) -> Result<Vec<Vec<Value>>> {
    match plan {
        RelPlan::Scan {
            table,
            columns,
            predicates,
            ..
        } => {
            let mut out = Vec::new();
            for source in &data.tables[table] {
                let mut row = vec![Value::Null; width];
                for (&slot, value) in columns.iter().zip(source) {
                    row[slot] = value.clone();
                }
                // Charge rejected rows too: scanning and expression evaluation remain bounded.
                budget.row(&row)?;
                let mut keep = true;
                for predicate in predicates {
                    if truth(eval(predicate, &row, params, budget)?)? != Some(true) {
                        keep = false;
                        break;
                    }
                }
                if keep {
                    out.push(row);
                }
            }
            Ok(out)
        }
        RelPlan::Filter { predicate, input } => {
            let mut out = Vec::new();
            for row in run(input, data, params, width, budget)? {
                if truth(eval(predicate, &row, params, budget)?)? == Some(true) {
                    out.push(row);
                }
            }
            Ok(out)
        }
        RelPlan::Join { kind, lhs, rhs, on } => {
            let left = run(lhs, data, params, width, budget)?;
            let right = run(rhs, data, params, width, budget)?;
            let slots = rhs.slots();
            let mut matched_right = vec![false; right.len()];
            let mut out = Vec::new();
            for l in &left {
                let mut matched_left = false;
                for (j, r) in right.iter().enumerate() {
                    budget.tick(add(1, width)?)?;
                    let mut row = l.clone();
                    for &slot in &slots {
                        row[slot] = r[slot].clone();
                    }
                    budget.row(&row)?;
                    if truth(eval(on, &row, params, budget)?)? == Some(true) {
                        matched_left = true;
                        matched_right[j] = true;
                        out.push(row);
                    }
                }
                if !matched_left && matches!(kind, JoinKind::Left | JoinKind::Full) {
                    budget.row(l)?;
                    out.push(l.clone());
                }
            }
            if matches!(kind, JoinKind::Right | JoinKind::Full) {
                for (j, r) in right.iter().enumerate() {
                    if !matched_right[j] {
                        budget.row(r)?;
                        out.push(r.clone());
                    }
                }
            }
            Ok(out)
        }
    }
}
fn truth(v: Value) -> Result<Option<bool>> {
    match v {
        Value::Null => Ok(None),
        Value::Bool(x) => Ok(Some(x)),
        _ => Err(Error::new(ErrorKind::Type, "non-boolean checked predicate")),
    }
}
fn boolean(v: Option<bool>) -> Value {
    v.map_or(Value::Null, Value::Bool)
}
fn eval(
    expr: &TypedExpr,
    row: &[Value],
    params: &BTreeMap<String, Value>,
    budget: &mut Budget,
) -> Result<Value> {
    budget.tick(1)?;
    Ok(match &expr.node {
        ExprNode::Column { slot, .. } => {
            budget.tick(row[*slot].bytes())?;
            row[*slot].clone()
        }
        ExprNode::Param(name) => {
            budget.tick(params[name].bytes())?;
            params[name].clone()
        }
        ExprNode::Literal(v) => {
            budget.tick(v.bytes())?;
            v.clone()
        }
        ExprNode::Compare(op, a, b) => {
            let a = eval(a, row, params, budget)?;
            let b = eval(b, row, params, budget)?;
            if a == Value::Null || b == Value::Null {
                Value::Null
            } else {
                let ordering = match (&a, &b) {
                    (Value::U64(a), Value::U64(b)) => a.cmp(b),
                    (Value::I64(a), Value::I64(b)) => a.cmp(b),
                    (Value::F64(a), Value::F64(b)) => a
                        .partial_cmp(b)
                        .ok_or_else(|| Error::new(ErrorKind::Data, "non-finite float"))?,
                    (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
                    (Value::Text(a), Value::Text(b)) => a.cmp(b),
                    (Value::Bytes(a), Value::Bytes(b)) => a.cmp(b),
                    _ => return Err(Error::new(ErrorKind::Type, "comparison mismatch")),
                };
                Value::Bool(match op {
                    Cmp::Eq => ordering == Ordering::Equal,
                    Cmp::Ne => ordering != Ordering::Equal,
                    Cmp::Lt => ordering == Ordering::Less,
                    Cmp::Le => ordering != Ordering::Greater,
                    Cmp::Gt => ordering == Ordering::Greater,
                    Cmp::Ge => ordering != Ordering::Less,
                })
            }
        }
        ExprNode::And(a, b) => {
            let a = truth(eval(a, row, params, budget)?)?;
            let b = truth(eval(b, row, params, budget)?)?;
            boolean(match (a, b) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            })
        }
        ExprNode::Or(a, b) => {
            let a = truth(eval(a, row, params, budget)?)?;
            let b = truth(eval(b, row, params, budget)?)?;
            boolean(match (a, b) {
                (Some(true), _) | (_, Some(true)) => Some(true),
                (Some(false), Some(false)) => Some(false),
                _ => None,
            })
        }
        ExprNode::Not(x) => boolean(truth(eval(x, row, params, budget)?)?.map(|b| !b)),
        ExprNode::IsNull(x, negated) => {
            Value::Bool((eval(x, row, params, budget)? == Value::Null) != *negated)
        }
    })
}
