//! Symbolic dimensions in normal form: a positive constant times a product of
//! dimension variables (§28.2). `M * 2 * K` and `K * M * 2` normalize to the
//! same value, so shape agreement is decided by structural equality.
use std::collections::BTreeMap;
use std::fmt;

/// Largest constant a dimension may take (and the largest element count of a
/// constant shape). Larger values are rejected as overflow.
pub const MAX_DIM: u64 = 1 << 40;
/// Largest number of variable factors in one dimension.
pub const MAX_FACTORS: u32 = 16;

/// `coeff * Π var^exp`. Variables are rigid dimension parameters of the item
/// being checked, or `?n` unification variables during a call.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Dim {
    pub coeff: u64,
    pub vars: BTreeMap<String, u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DimError {
    /// A constant or product exceeds [`MAX_DIM`].
    Overflow,
    /// A dimension is zero.
    Zero,
    /// Too many variable factors.
    Factors,
}
impl fmt::Display for DimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Overflow => write!(f, "dimension exceeds {MAX_DIM}"),
            Self::Zero => write!(f, "dimensions must be positive"),
            Self::Factors => write!(f, "dimension has more than {MAX_FACTORS} factors"),
        }
    }
}

impl Dim {
    pub fn constant(n: u64) -> Result<Self, DimError> {
        if n == 0 {
            return Err(DimError::Zero);
        }
        if n > MAX_DIM {
            return Err(DimError::Overflow);
        }
        Ok(Self {
            coeff: n,
            vars: BTreeMap::new(),
        })
    }
    pub fn var(name: &str) -> Self {
        Self {
            coeff: 1,
            vars: BTreeMap::from([(name.to_string(), 1)]),
        }
    }
    pub fn one() -> Self {
        Self {
            coeff: 1,
            vars: BTreeMap::new(),
        }
    }
    pub fn as_const(&self) -> Option<u64> {
        self.vars.is_empty().then_some(self.coeff)
    }
    /// The single variable `v` when the dimension is exactly `v`.
    pub fn as_var(&self) -> Option<&str> {
        match (self.coeff, self.vars.iter().next(), self.vars.len()) {
            (1, Some((v, 1)), 1) => Some(v),
            _ => None,
        }
    }
    pub fn mul(&self, other: &Self) -> Result<Self, DimError> {
        let coeff = self
            .coeff
            .checked_mul(other.coeff)
            .filter(|c| *c <= MAX_DIM)
            .ok_or(DimError::Overflow)?;
        let mut vars = self.vars.clone();
        for (v, e) in &other.vars {
            *vars.entry(v.clone()).or_default() += e;
        }
        if vars.values().sum::<u32>() > MAX_FACTORS {
            return Err(DimError::Factors);
        }
        Ok(Self { coeff, vars })
    }
    /// Replace variables bound in `env`. Unbound variables stay symbolic.
    pub fn subst(&self, env: &BTreeMap<String, Dim>) -> Result<Self, DimError> {
        let mut out = Self {
            coeff: self.coeff,
            vars: BTreeMap::new(),
        };
        for (v, e) in &self.vars {
            let factor = env.get(v).cloned().unwrap_or_else(|| Self::var(v));
            for _ in 0..*e {
                out = out.mul(&factor)?;
            }
        }
        Ok(out)
    }
    pub fn mentions(&self, var: &str) -> bool {
        self.vars.contains_key(var)
    }
    pub fn variables(&self) -> impl Iterator<Item = &String> {
        self.vars.keys()
    }
    /// Whether `self` divides `other` for every value of the variables: the
    /// constant divides and every factor of `self` occurs in `other`.
    pub fn divides(&self, other: &Self) -> bool {
        other.coeff.is_multiple_of(self.coeff)
            && self
                .vars
                .iter()
                .all(|(v, e)| other.vars.get(v).is_some_and(|o| o >= e))
    }
    /// Evaluate with concrete values for every variable.
    pub fn eval(&self, env: &BTreeMap<String, u64>) -> Option<u64> {
        let mut n = self.coeff;
        for (v, e) in &self.vars {
            for _ in 0..*e {
                n = n.checked_mul(*env.get(v)?)?;
            }
        }
        Some(n)
    }
}
impl fmt::Display for Dim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = vec![];
        if self.coeff != 1 || self.vars.is_empty() {
            parts.push(self.coeff.to_string());
        }
        for (v, e) in &self.vars {
            for _ in 0..*e {
                parts.push(v.clone());
            }
        }
        write!(f, "{}", parts.join(" * "))
    }
}

/// Product of a shape's dimensions (its element count).
pub fn elements(shape: &[Dim]) -> Result<Dim, DimError> {
    shape.iter().try_fold(Dim::one(), |acc, d| acc.mul(d))
}

pub fn display_shape(shape: &[Dim]) -> String {
    format!(
        "[{}]",
        shape
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    )
}
