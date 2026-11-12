use crate::parse::{Cmp, Expr, Select, Table};
use crate::{
    add, bound, identifier, ColumnDef, Error, ErrorKind, HermeticCatalog, Limits, ParamDef, Result,
    ScalarType, Value,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinKind {
    Inner,
    Left,
    Right,
    Full,
}

/// Identity belongs to a FROM binding, so self-joins have distinct slots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundColumn {
    pub alias: String,
    pub column: String,
    pub binding: usize,
    pub slot: usize,
    pub ty: ScalarType,
    pub nullable: bool,
}
#[derive(Clone, Debug, PartialEq)]
pub struct TypedExpr {
    pub(crate) node: ExprNode,
    ty: Option<ScalarType>,
    nullable: bool,
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ExprNode {
    Column { slot: usize, binding: usize },
    Param(String),
    Literal(Value),
    Compare(Cmp, Box<TypedExpr>, Box<TypedExpr>),
    And(Box<TypedExpr>, Box<TypedExpr>),
    Or(Box<TypedExpr>, Box<TypedExpr>),
    Not(Box<TypedExpr>),
    IsNull(Box<TypedExpr>, bool),
}
impl TypedExpr {
    pub fn scalar_type(&self) -> Option<ScalarType> {
        self.ty
    }
    pub fn nullable(&self) -> bool {
        self.nullable
    }
    /// Recomputed from checked column references; never accepted from the caller.
    pub fn dependencies(&self) -> BTreeSet<usize> {
        let mut out = BTreeSet::new();
        self.collect_dependencies(&mut out);
        out
    }
    fn collect_dependencies(&self, out: &mut BTreeSet<usize>) {
        match &self.node {
            ExprNode::Column { binding, .. } => {
                out.insert(*binding);
            }
            ExprNode::Compare(_, a, b) | ExprNode::And(a, b) | ExprNode::Or(a, b) => {
                a.collect_dependencies(out);
                b.collect_dependencies(out);
            }
            ExprNode::Not(x) | ExprNode::IsNull(x, _) => x.collect_dependencies(out),
            ExprNode::Param(_) | ExprNode::Literal(_) => {}
        }
    }
}
/// Readable typed logical plan. Execution is available only through a private checked PreparedQuery.
#[derive(Clone, Debug, PartialEq)]
pub enum RelPlan {
    Scan {
        table: String,
        binding: usize,
        columns: Vec<usize>,
        predicates: Vec<TypedExpr>,
    },
    Filter {
        predicate: TypedExpr,
        input: Box<RelPlan>,
    },
    Join {
        kind: JoinKind,
        lhs: Box<RelPlan>,
        rhs: Box<RelPlan>,
        on: TypedExpr,
    },
}
impl RelPlan {
    pub fn bindings(&self) -> BTreeSet<usize> {
        match self {
            Self::Scan { binding, .. } => BTreeSet::from([*binding]),
            Self::Filter { input, .. } => input.bindings(),
            Self::Join { lhs, rhs, .. } => {
                let mut x = lhs.bindings();
                x.extend(rhs.bindings());
                x
            }
        }
    }
    pub(crate) fn slots(&self) -> Vec<usize> {
        match self {
            Self::Scan { columns, .. } => columns.clone(),
            Self::Filter { input, .. } => input.slots(),
            Self::Join { lhs, rhs, .. } => {
                let mut x = lhs.slots();
                x.extend(rhs.slots());
                x
            }
        }
    }
    fn optimize(self) -> Self {
        match self {
            Self::Filter { predicate, input } => {
                let input = input.optimize();
                // Conjunction splitting is valid under SQL three-valued WHERE semantics.
                if let ExprNode::And(a, b) = predicate.node {
                    place(*b, place(*a, input))
                } else {
                    place(predicate, input)
                }
            }
            Self::Join { kind, lhs, rhs, on } => Self::Join {
                kind,
                lhs: Box::new(lhs.optimize()),
                rhs: Box::new(rhs.optimize()),
                on,
            },
            scan => scan,
        }
    }
}
fn place(predicate: TypedExpr, input: RelPlan) -> RelPlan {
    if let ExprNode::And(a, b) = predicate.node {
        return place(*b, place(*a, input));
    }
    let deps = predicate.dependencies();
    match input {
        RelPlan::Scan {
            table,
            binding,
            columns,
            mut predicates,
        } => {
            predicates.push(predicate);
            RelPlan::Scan {
                table,
                binding,
                columns,
                predicates,
            }
        }
        RelPlan::Join { kind, lhs, rhs, on } => {
            // A WHERE predicate can cross only a preserved side of an outer join.
            // Even IS NULL on the nullable side must remain above the join.
            if matches!(kind, JoinKind::Inner | JoinKind::Left) && deps.is_subset(&lhs.bindings()) {
                RelPlan::Join {
                    kind,
                    lhs: Box::new(place(predicate, *lhs)),
                    rhs,
                    on,
                }
            } else if matches!(kind, JoinKind::Inner | JoinKind::Right)
                && deps.is_subset(&rhs.bindings())
            {
                RelPlan::Join {
                    kind,
                    lhs,
                    rhs: Box::new(place(predicate, *rhs)),
                    on,
                }
            } else {
                RelPlan::Filter {
                    predicate,
                    input: Box::new(RelPlan::Join { kind, lhs, rhs, on }),
                }
            }
        }
        RelPlan::Filter {
            predicate: existing,
            input,
        } => RelPlan::Filter {
            predicate: existing,
            input: Box::new(place(predicate, *input)),
        },
    }
}

#[derive(Clone, Debug)]
pub struct PreparedQuery {
    pub(crate) catalog: HermeticCatalog,
    pub(crate) limits: Limits,
    pub(crate) plan: RelPlan,
    pub(crate) columns: Vec<BoundColumn>,
    pub(crate) projection: Vec<TypedExpr>,
    pub(crate) output: Vec<ColumnDef>,
    pub(crate) params: BTreeMap<String, ParamDef>,
}
impl PreparedQuery {
    pub fn plan(&self) -> &RelPlan {
        &self.plan
    }
    pub fn columns(&self) -> &[BoundColumn] {
        &self.columns
    }
    pub fn output(&self) -> &[ColumnDef] {
        &self.output
    }
    pub fn parameters(&self) -> &BTreeMap<String, ParamDef> {
        &self.params
    }
    pub fn catalog_revision(&self) -> u64 {
        self.catalog.revision()
    }
    pub fn optimized(&self) -> Self {
        let mut p = self.clone();
        p.plan = p.plan.optimize();
        p
    }
}
struct Checker {
    columns: Vec<BoundColumn>,
    aliases: BTreeSet<String>,
    params: BTreeMap<String, ParamDef>,
    used_params: BTreeSet<String>,
    bindings: usize,
}
impl Checker {
    fn table(&mut self, t: Table, catalog: &HermeticCatalog) -> Result<RelPlan> {
        if !self.aliases.insert(t.alias.clone()) {
            return Err(Error::new(
                ErrorKind::Duplicate,
                format!("table alias {}", t.alias),
            ));
        }
        let cols = catalog
            .tables
            .get(&t.name)
            .ok_or_else(|| Error::new(ErrorKind::Unknown, format!("table {}", t.name)))?;
        let binding = self.bindings;
        self.bindings += 1;
        let mut slots = Vec::new();
        for c in cols {
            let slot = self.columns.len();
            slots.push(slot);
            self.columns.push(BoundColumn {
                alias: t.alias.clone(),
                column: c.name.clone(),
                binding,
                slot,
                ty: c.ty,
                nullable: c.nullable,
            });
        }
        Ok(RelPlan::Scan {
            table: t.name,
            binding,
            columns: slots,
            predicates: Vec::new(),
        })
    }
    fn column(&self, qualifier: Option<&str>, name: &str) -> Result<&BoundColumn> {
        let mut matches = self
            .columns
            .iter()
            .filter(|c| c.column == name && qualifier.is_none_or(|q| q == c.alias));
        let col = matches.next().ok_or_else(|| {
            Error::new(
                ErrorKind::Unknown,
                format!(
                    "column {}{name}",
                    qualifier.map_or(String::new(), |q| format!("{q}."))
                ),
            )
        })?;
        if matches.next().is_some() {
            return Err(Error::new(ErrorKind::Ambiguous, format!("column {name}")));
        }
        Ok(col)
    }
    fn expr(&mut self, raw: Expr, expected: Option<ScalarType>) -> Result<TypedExpr> {
        let mut expr = match raw {
            Expr::Column(q, name) => {
                let c = self.column(q.as_deref(), &name)?;
                TypedExpr {
                    node: ExprNode::Column {
                        slot: c.slot,
                        binding: c.binding,
                    },
                    ty: Some(c.ty),
                    nullable: c.nullable,
                }
            }
            Expr::Param(name) => {
                let p = self
                    .params
                    .get(&name)
                    .ok_or_else(|| Error::new(ErrorKind::Unknown, format!("parameter :{name}")))?;
                self.used_params.insert(name.clone());
                TypedExpr {
                    node: ExprNode::Param(name),
                    ty: Some(p.ty),
                    nullable: p.nullable,
                }
            }
            Expr::Literal(v) => {
                let ty = v.scalar_type();
                let nullable = v == Value::Null;
                TypedExpr {
                    node: ExprNode::Literal(v),
                    ty,
                    nullable,
                }
            }
            Expr::Compare(op, a, b) => {
                let mut a = self.expr(*a, None)?;
                let mut b = self.expr(*b, None)?;
                let ty = a.ty.or(b.ty).unwrap_or(ScalarType::Bool);
                if a.ty.is_some_and(|x| x != ty) || b.ty.is_some_and(|x| x != ty) {
                    return Err(Error::new(
                        ErrorKind::Type,
                        "comparison operands must have identical types",
                    ));
                }
                a.ty = Some(ty);
                b.ty = Some(ty);
                let nullable = a.nullable || b.nullable;
                TypedExpr {
                    node: ExprNode::Compare(op, Box::new(a), Box::new(b)),
                    ty: Some(ScalarType::Bool),
                    nullable,
                }
            }
            Expr::And(a, b) => return self.boolean_binary(*a, *b, expected, false),
            Expr::Or(a, b) => return self.boolean_binary(*a, *b, expected, true),
            Expr::Not(x) => {
                let x = self.expr(*x, Some(ScalarType::Bool))?;
                let nullable = x.nullable;
                TypedExpr {
                    node: ExprNode::Not(Box::new(x)),
                    ty: Some(ScalarType::Bool),
                    nullable,
                }
            }
            Expr::IsNull(x, negated) => {
                let x = self.expr(*x, None)?;
                TypedExpr {
                    node: ExprNode::IsNull(Box::new(x), negated),
                    ty: Some(ScalarType::Bool),
                    nullable: false,
                }
            }
        };
        if let Some(ty) = expected {
            if expr.ty.is_some_and(|actual| actual != ty) {
                return Err(Error::new(ErrorKind::Type, "expected boolean expression"));
            }
            expr.ty = Some(ty);
        }
        Ok(expr)
    }
    fn boolean_binary(
        &mut self,
        a: Expr,
        b: Expr,
        expected: Option<ScalarType>,
        or: bool,
    ) -> Result<TypedExpr> {
        if expected.is_some_and(|ty| ty != ScalarType::Bool) {
            return Err(Error::new(ErrorKind::Type, "boolean type mismatch"));
        }
        let a = self.expr(a, Some(ScalarType::Bool))?;
        let b = self.expr(b, Some(ScalarType::Bool))?;
        let nullable = a.nullable || b.nullable;
        let node = if or {
            ExprNode::Or(Box::new(a), Box::new(b))
        } else {
            ExprNode::And(Box::new(a), Box::new(b))
        };
        Ok(TypedExpr {
            node,
            ty: Some(ScalarType::Bool),
            nullable,
        })
    }
}
pub(crate) fn prepare(
    catalog: &HermeticCatalog,
    raw: Select,
    defs: &[ParamDef],
    limits: Limits,
) -> Result<PreparedQuery> {
    bound(defs.len(), limits.nodes, "parameter count limit")?;
    let mut params = BTreeMap::new();
    for def in defs {
        let name = identifier(&def.name)?;
        let mut def = def.clone();
        def.name = name.clone();
        if params.insert(name.clone(), def).is_some() {
            return Err(Error::new(
                ErrorKind::Duplicate,
                format!("parameter {name}"),
            ));
        }
    }
    let mut c = Checker {
        columns: Vec::new(),
        aliases: BTreeSet::new(),
        params,
        used_params: BTreeSet::new(),
        bindings: 0,
    };
    let mut plan = c.table(raw.from, catalog)?;
    for join in raw.joins {
        let rhs = c.table(join.rhs, catalog)?;
        let on = c.expr(join.on, Some(ScalarType::Bool))?;
        let left_slots = plan.slots();
        let right_slots = rhs.slots();
        if matches!(join.kind, JoinKind::Right | JoinKind::Full) {
            for i in left_slots {
                c.columns[i].nullable = true;
            }
        }
        if matches!(join.kind, JoinKind::Left | JoinKind::Full) {
            for i in right_slots {
                c.columns[i].nullable = true;
            }
        }
        plan = RelPlan::Join {
            kind: join.kind,
            lhs: Box::new(plan),
            rhs: Box::new(rhs),
            on,
        };
    }
    if let Some(raw) = raw.filter {
        plan = RelPlan::Filter {
            predicate: c.expr(raw, Some(ScalarType::Bool))?,
            input: Box::new(plan),
        };
    }
    let mut projection = Vec::new();
    let mut output = Vec::new();
    let mut names = BTreeSet::new();
    for selection in raw.projection {
        let entries = if let Some(expr) = selection.expr {
            let name = selection
                .alias
                .or_else(|| match &expr {
                    Expr::Column(_, name) => Some(name.clone()),
                    _ => None,
                })
                .ok_or_else(|| {
                    Error::new(ErrorKind::Syntax, "non-column projection requires AS name")
                })?;
            vec![(expr, name)]
        } else {
            c.columns
                .iter()
                .map(|col| {
                    (
                        Expr::Column(Some(col.alias.clone()), col.column.clone()),
                        col.column.clone(),
                    )
                })
                .collect()
        };
        for (expr, name) in entries {
            bound(
                add(projection.len(), 1)?,
                limits.nodes,
                "expanded projection limit",
            )?;
            if !names.insert(name.clone()) {
                return Err(Error::new(
                    ErrorKind::Duplicate,
                    format!("output name {name}; use AS"),
                ));
            }
            let expr = c.expr(expr, None)?;
            let ty = expr.ty.ok_or_else(|| {
                Error::new(ErrorKind::Type, "NULL projection has no inferable type")
            })?;
            output.push(ColumnDef {
                name,
                ty,
                nullable: expr.nullable,
            });
            projection.push(expr);
        }
    }
    if c.params.len() != c.used_params.len() {
        return Err(Error::new(
            ErrorKind::Unknown,
            "unused parameter declaration",
        ));
    }
    Ok(PreparedQuery {
        catalog: catalog.clone(),
        limits,
        plan,
        columns: c.columns,
        projection,
        output,
        params: c.params,
    })
}
