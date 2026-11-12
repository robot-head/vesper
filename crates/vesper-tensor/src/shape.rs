//! Bounded, nonrecursive dimension arena. Products are canonicalized only in
//! the nonnegative integer dimension algebra; never for floating-point data.
//! Equality is conservative: no cancellation or inferred bindings. Distinct
//! unresolved expressions become obligations that must be checked after binding.

use std::collections::BTreeMap;

use crate::{valid_name, Error};

pub type Environment = BTreeMap<String, u64>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DimId(usize, u64);

#[derive(Clone, Debug)]
enum Expr {
    Const(u64),
    Symbol(String),
    Mul(DimId, DimId),
}

#[derive(Debug)]
pub struct Dimensions {
    owner: u64,
    nodes: Vec<(Expr, usize)>,
    max_nodes: usize,
    max_depth: usize,
    max_factors: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalDim {
    pub coefficient: u64,
    /// Sorted factors, retaining multiplicity.
    pub symbols: Vec<String>,
}
impl CanonicalDim {
    pub fn resolved(&self) -> Option<u64> {
        self.symbols.is_empty().then_some(self.coefficient)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Obligation {
    Equal(DimId, DimId),
    Positive(DimId),
    Divisible(DimId, u64),
    GqaRatio {
        query: DimId,
        kv: DimId,
    },
    /// Even a canonically equal product may overflow after future binding.
    Fits(DimId),
    ProductFits(DimId, DimId),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CheckedShape {
    obligations: Vec<Obligation>,
    // Recheck proven predicates too: callers may supply a changed environment.
    checks: Vec<Obligation>,
}
impl CheckedShape {
    pub fn obligations(&self) -> &[Obligation] {
        &self.obligations
    }

    /// Whether all predicates were resolved under the planning environment.
    /// `discharge` still rechecks proven predicates against its final bindings.
    pub fn is_resolved(&self) -> bool {
        self.obligations.is_empty()
    }

    /// Rechecks all obligations using the same arena and final bindings.
    /// Fails closed when any dimension still has unbound symbols.
    pub fn discharge(&self, dims: &Dimensions, env: &Environment) -> Result<(), Error> {
        for obligation in &self.checks {
            match *obligation {
                Obligation::Equal(a, b) => {
                    if dims.value(a, env)? != dims.value(b, env)? {
                        return Err(Error::Shape("contraction mismatch"));
                    }
                }
                Obligation::Positive(a) => {
                    if dims.value(a, env)? == 0 {
                        return Err(Error::Shape("zero dimension"));
                    }
                }
                Obligation::Divisible(a, block) => {
                    if block == 0 || dims.value(a, env)? % block != 0 {
                        return Err(Error::Shape("block alignment"));
                    }
                }
                Obligation::GqaRatio { query, kv } => {
                    gqa_ratio(dims.value(query, env)?, dims.value(kv, env)?)?;
                }
                Obligation::ProductFits(a, b) => {
                    dims.value(a, env)?
                        .checked_mul(dims.value(b, env)?)
                        .ok_or(Error::Overflow)?;
                }
                Obligation::Fits(a) => {
                    dims.value(a, env)?;
                }
            }
        }
        Ok(())
    }
}

impl Dimensions {
    /// Hard ceilings prevent caller budgets from enabling unbounded tree work.
    pub fn new(max_nodes: usize, max_depth: usize, max_factors: usize) -> Result<Self, Error> {
        if max_nodes == 0
            || max_nodes > 4096
            || max_depth == 0
            || max_depth > 128
            || max_factors == 0
            || max_factors > 128
        {
            return Err(Error::Limit("dimension configuration"));
        }
        Ok(Self {
            owner: crate::next_owner()?,
            nodes: Vec::new(),
            max_nodes,
            max_depth,
            max_factors,
        })
    }

    fn insert(&mut self, expr: Expr, depth: usize) -> Result<DimId, Error> {
        if self.nodes.len() >= self.max_nodes || depth > self.max_depth {
            return Err(Error::Limit("dimension nodes/depth"));
        }
        let id = DimId(self.nodes.len(), self.owner);
        self.nodes.push((expr, depth));
        Ok(id)
    }

    pub fn constant(&mut self, value: u64) -> Result<DimId, Error> {
        self.insert(Expr::Const(value), 1)
    }

    pub fn symbol(&mut self, name: &str) -> Result<DimId, Error> {
        if !valid_name(name) {
            return Err(Error::InvalidName);
        }
        self.insert(Expr::Symbol(name.into()), 1)
    }

    pub fn multiply(&mut self, a: DimId, b: DimId) -> Result<DimId, Error> {
        if a.1 != self.owner || b.1 != self.owner {
            return Err(Error::InvalidId);
        }
        let da = self.nodes.get(a.0).ok_or(Error::InvalidId)?.1;
        let db = self.nodes.get(b.0).ok_or(Error::InvalidId)?.1;
        self.insert(Expr::Mul(a, b), da.max(db) + 1)
    }

    pub fn normalize(&self, id: DimId, env: &Environment) -> Result<CanonicalDim, Error> {
        if id.1 != self.owner || id.0 >= self.nodes.len() {
            return Err(Error::InvalidId);
        }
        // Evaluate each DAG node once. Unreachable expressions cannot reject a
        // requested expression, but are never recursively traversed.
        let mut needed = vec![false; id.0 + 1];
        needed[id.0] = true;
        for i in (0..=id.0).rev() {
            if needed[i] {
                if let Expr::Mul(a, b) = self.nodes[i].0 {
                    needed[a.0] = true;
                    needed[b.0] = true;
                }
            }
        }
        let mut out: Vec<Option<CanonicalDim>> = vec![None; id.0 + 1];
        for i in 0..=id.0 {
            if !needed[i] {
                continue;
            }
            let value = match &self.nodes[i].0 {
                Expr::Const(n) => CanonicalDim {
                    coefficient: *n,
                    symbols: vec![],
                },
                Expr::Symbol(s) => match env.get(s) {
                    Some(n) => CanonicalDim {
                        coefficient: *n,
                        symbols: vec![],
                    },
                    None => CanonicalDim {
                        coefficient: 1,
                        symbols: vec![s.clone()],
                    },
                },
                Expr::Mul(a, b) => {
                    let a = out[a.0].as_ref().ok_or(Error::InvalidId)?;
                    let b = out[b.0].as_ref().ok_or(Error::InvalidId)?;
                    let coefficient = a
                        .coefficient
                        .checked_mul(b.coefficient)
                        .ok_or(Error::Overflow)?;
                    let count = a
                        .symbols
                        .len()
                        .checked_add(b.symbols.len())
                        .ok_or(Error::Overflow)?;
                    if count > self.max_factors {
                        return Err(Error::Limit("symbol factors"));
                    }
                    let mut symbols = a.symbols.clone();
                    symbols.extend(b.symbols.iter().cloned());
                    symbols.sort();
                    // Keep zero's symbolic factors: binding can still make a
                    // nested subexpression overflow. Do not erase that domain.
                    CanonicalDim {
                        coefficient,
                        symbols,
                    }
                }
            };
            out[i] = Some(value);
        }
        out[id.0].take().ok_or(Error::InvalidId)
    }

    pub fn value(&self, id: DimId, env: &Environment) -> Result<u64, Error> {
        self.normalize(id, env)?.resolved().ok_or(Error::Unresolved)
    }

    pub fn unify(&self, a: DimId, b: DimId, env: &Environment) -> Result<CheckedShape, Error> {
        let a_value = self.normalize(a, env)?;
        let b_value = self.normalize(b, env)?;
        let mut result = CheckedShape::default();
        result.checks.push(Obligation::Equal(a, b));
        match (a_value.resolved(), b_value.resolved()) {
            (Some(a), Some(b)) if a != b => return Err(Error::Shape("contraction mismatch")),
            (Some(_), Some(_)) => (),
            _ if a_value == b_value => {
                result
                    .obligations
                    .extend([Obligation::Fits(a), Obligation::Fits(b)]);
            }
            _ => result.obligations.push(Obligation::Equal(a, b)),
        }
        Ok(result)
    }

    fn positive(
        &self,
        id: DimId,
        env: &Environment,
        result: &mut CheckedShape,
    ) -> Result<(), Error> {
        result.checks.push(Obligation::Positive(id));
        match self.normalize(id, env)?.resolved() {
            Some(0) => Err(Error::Shape("zero dimension")),
            Some(_) => Ok(()),
            None => {
                result.obligations.push(Obligation::Positive(id));
                Ok(())
            }
        }
    }

    fn divisible(
        &self,
        id: DimId,
        block: u64,
        env: &Environment,
        result: &mut CheckedShape,
    ) -> Result<(), Error> {
        if block == 0 {
            return Err(Error::Shape("zero block size"));
        }
        result.checks.push(Obligation::Divisible(id, block));
        match self.normalize(id, env)?.resolved() {
            Some(n) if n % block != 0 => Err(Error::Shape("block alignment")),
            Some(_) => Ok(()),
            None => {
                result.obligations.push(Obligation::Divisible(id, block));
                Ok(())
            }
        }
    }

    /// Checks `[M,K] × [K,N]`; supported planning shapes must be positive.
    /// Input/output element products must fit u64; `resolved_matmul` also
    /// applies the caller's output element budget.
    pub fn matmul(
        &self,
        axes: [DimId; 4],
        block: Option<u64>,
        env: &Environment,
    ) -> Result<CheckedShape, Error> {
        let [m, left_k, right_k, n] = axes;
        let mut result = self.unify(left_k, right_k, env)?;
        for axis in axes {
            self.positive(axis, env, &mut result)?;
        }
        if let Some(block) = block {
            self.divisible(left_k, block, env, &mut result)?;
        }
        for (a, b) in [(m, left_k), (right_k, n), (m, n)] {
            result.checks.push(Obligation::ProductFits(a, b));
            match (
                self.normalize(a, env)?.resolved(),
                self.normalize(b, env)?.resolved(),
            ) {
                (Some(a), Some(b)) => {
                    a.checked_mul(b).ok_or(Error::Overflow)?;
                }
                _ => result.obligations.push(Obligation::ProductFits(a, b)),
            }
        }
        Ok(result)
    }

    pub fn resolved_matmul(
        &self,
        axes: [DimId; 4],
        block: Option<u64>,
        env: &Environment,
        max_elements: u64,
    ) -> Result<[u64; 2], Error> {
        self.matmul(axes, block, env)?.discharge(self, env)?;
        let [m, _, _, n] = axes;
        let m = self.value(m, env)?;
        let n = self.value(n, env)?;
        if m.checked_mul(n).ok_or(Error::Overflow)? > max_elements {
            return Err(Error::Limit("matmul output"));
        }
        Ok([m, n])
    }

    pub fn gqa_rope(
        &self,
        query: DimId,
        kv: DimId,
        width: DimId,
        env: &Environment,
    ) -> Result<CheckedShape, Error> {
        let mut result = CheckedShape::default();
        result.checks.push(Obligation::GqaRatio { query, kv });
        for axis in [query, kv, width] {
            self.positive(axis, env, &mut result)?;
        }
        self.divisible(width, 2, env, &mut result)?;
        match (
            self.normalize(query, env)?.resolved(),
            self.normalize(kv, env)?.resolved(),
        ) {
            (Some(q), Some(k)) => {
                gqa_ratio(q, k)?;
            }
            _ => result.obligations.push(Obligation::GqaRatio { query, kv }),
        }
        Ok(result)
    }
}

pub fn gqa_ratio(query_heads: u64, kv_heads: u64) -> Result<u64, Error> {
    if query_heads == 0 || kv_heads == 0 || !query_heads.is_multiple_of(kv_heads) {
        return Err(Error::Shape("invalid GQA ratio"));
    }
    Ok(query_heads / kv_heads)
}

pub fn gqa_kv_head(query_heads: u64, kv_heads: u64, query_index: u64) -> Result<u64, Error> {
    let group = gqa_ratio(query_heads, kv_heads)?;
    if query_index >= query_heads {
        return Err(Error::Shape("query head out of bounds"));
    }
    Ok(query_index / group)
}
