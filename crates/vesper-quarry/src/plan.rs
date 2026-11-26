use crate::{ColumnDef, HermeticCatalog, Limits, ParamDef, ScalarType, Value};
use std::collections::{BTreeMap, BTreeSet};

/// A comparison operator of a checked predicate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cmp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

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
    pub(crate) ty: Option<ScalarType>,
    pub(crate) nullable: bool,
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
        p.plan = crate::port::optimize(&p.plan);
        p
    }
}
/// How an index path bounds its column (spec/planner-ddl.md, section 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexBound {
    Eq,
    Range,
    Lower,
    Upper,
}

/// The access path the planner chose for one FROM binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccessPath {
    /// Every row the snapshot sees.
    Scan,
    /// A secondary index with sargable bounds.
    Index { index: String, bound: IndexBound },
    /// Index nested-loop join: probe `index` with the distinct non-null
    /// values of `outer.column`.
    IndexJoin {
        index: String,
        outer: String,
        column: String,
    },
}

/// One FROM binding's chosen path, with the cost model's row estimate and
/// cost (integer milli-units).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindingPath {
    pub alias: String,
    pub path: AccessPath,
    pub rows: u64,
    pub cost: u64,
}

/// The access paths of a query, in FROM order. `Display` renders the
/// notation of `tests/fixtures/planner/cases.txt`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessPlan {
    pub bindings: Vec<BindingPath>,
}

impl std::fmt::Display for AccessPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, b) in self.bindings.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{}=", b.alias)?;
            match &b.path {
                AccessPath::Scan => f.write_str("scan")?,
                AccessPath::Index { index, bound } => {
                    let bound = match bound {
                        IndexBound::Eq => "eq",
                        IndexBound::Range => "range",
                        IndexBound::Lower => "lower",
                        IndexBound::Upper => "upper",
                    };
                    write!(f, "index({index},{bound})")?;
                }
                AccessPath::IndexJoin {
                    index,
                    outer,
                    column,
                } => write!(f, "index_join({index},{outer}.{column})")?,
            }
        }
        Ok(())
    }
}

impl PreparedQuery {
    /// Chooses an access path for every FROM binding of the optimized plan
    /// with the cost model of `spec/planner-ddl.md`. `data` holds every
    /// table the query reads (its exact statistics are ANALYZE's), `indexes`
    /// maps index names to `(table, column)` as `Store::indexes` returns
    /// them, and `params` are the parameter values known at planning (a
    /// parameter without a value is estimated).
    ///
    /// Only the Vesper port implements access selection; the Rust build
    /// returns `Unsupported`.
    pub fn access_plan(
        &self,
        data: &crate::DataSet,
        indexes: &BTreeMap<String, (String, String)>,
        params: &BTreeMap<String, Value>,
    ) -> crate::Result<AccessPlan> {
        if data.catalog != self.catalog {
            return Err(crate::Error::new(
                crate::ErrorKind::Stale,
                "prepared query or data schema snapshot differs in revision/content",
            ));
        }
        crate::port::access_plan(self, data, indexes, params)
    }
}
