//! Ordered pomonoid resource grades (§5.3) and bounded `RefineTerm` ceiling
//! verification (§7.4).
//!
//! This is a standalone analysis primitive. Source-checker integration,
//! Bitwuzla/SMT lowering, and Stage 4 `egglog` bump-check elision remain
//! separate work.

use std::collections::BTreeMap;

/// Maximum AST nodes visited during a single `RefineTerm` evaluation.
pub const MAX_TERM_NODES: usize = 4096;

/// Maximum structural depth permitted when evaluating a `RefineTerm`.
pub const MAX_TERM_DEPTH: usize = 64;

/// Closed integer interval `[min, max]` with `min <= max`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IntervalBound {
    pub min: i64,
    pub max: i64,
}

impl IntervalBound {
    /// Constructs a validated interval `[min, max]`, rejecting inverted bounds.
    pub const fn new(min: i64, max: i64) -> Result<Self, ResourceGradeError> {
        if min > max {
            Err(ResourceGradeError::InvalidInterval { min, max })
        } else {
            Ok(Self { min, max })
        }
    }

    /// Constructs a singleton interval `[value, value]`.
    pub const fn exact(value: i64) -> Self {
        Self {
            min: value,
            max: value,
        }
    }

    /// Validates that `self.min <= self.max`.
    pub const fn validate(self) -> Result<Self, ResourceGradeError> {
        Self::new(self.min, self.max)
    }

    /// Exact overflow-checked interval addition `[a.min + b.min, a.max + b.max]`.
    pub fn checked_add(self, other: Self) -> Result<Self, ResourceGradeError> {
        self.validate()?;
        other.validate()?;
        let min = i128::from(self.min) + i128::from(other.min);
        let max = i128::from(self.max) + i128::from(other.max);
        Self::from_i128(min, max)
    }

    /// Exact overflow-checked interval subtraction `[a.min - b.max, a.max - b.min]`.
    pub fn checked_sub(self, other: Self) -> Result<Self, ResourceGradeError> {
        self.validate()?;
        other.validate()?;
        let min = i128::from(self.min) - i128::from(other.max);
        let max = i128::from(self.max) - i128::from(other.min);
        Self::from_i128(min, max)
    }

    /// Exact overflow-checked multiplication by a signed constant `k`.
    pub fn checked_mul_const(self, k: i64) -> Result<Self, ResourceGradeError> {
        self.validate()?;
        let k128 = i128::from(k);
        let a = i128::from(self.min) * k128;
        let b = i128::from(self.max) * k128;
        if k >= 0 {
            Self::from_i128(a, b)
        } else {
            Self::from_i128(b, a)
        }
    }

    /// Least upper bound of two intervals: `[min(a.min, b.min), max(a.max, b.max)]`.
    pub fn join(self, other: Self) -> Result<Self, ResourceGradeError> {
        self.validate()?;
        other.validate()?;
        Ok(Self {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        })
    }

    fn from_i128(min: i128, max: i128) -> Result<Self, ResourceGradeError> {
        let min = i64::try_from(min).map_err(|_| ResourceGradeError::ArithmeticOverflow)?;
        let max = i64::try_from(max).map_err(|_| ResourceGradeError::ArithmeticOverflow)?;
        Self::new(min, max)
    }
}

/// Linear refinement expression over `i64` constants and named index variables (§7.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefineTerm {
    Const(i64),
    Var(String),
    Add(Box<RefineTerm>, Box<RefineTerm>),
    Sub(Box<RefineTerm>, Box<RefineTerm>),
    MulConst(i64, Box<RefineTerm>),
}

impl RefineTerm {
    /// Evaluates sound `[min, max]` bounds under `env` within bounded depth and node limits.
    ///
    /// Uses exact `i128` arithmetic checked back to `i64` so overflow never silently
    /// saturates into a falsely valid ceiling proof.
    pub fn eval_bounds(
        &self,
        env: &BTreeMap<String, IntervalBound>,
    ) -> Result<IntervalBound, ResourceGradeError> {
        self.eval_bounded(env, MAX_TERM_DEPTH, MAX_TERM_NODES)
    }

    /// Evaluates sound `[min, max]` bounds with caller-specified depth and node budgets.
    pub fn eval_bounded(
        &self,
        env: &BTreeMap<String, IntervalBound>,
        max_depth: usize,
        max_nodes: usize,
    ) -> Result<IntervalBound, ResourceGradeError> {
        if max_depth == 0 || max_nodes == 0 {
            return Err(ResourceGradeError::TermLimitExceeded);
        }
        enum Frame<'a> {
            Enter(&'a RefineTerm, usize),
            CombineAdd,
            CombineSub,
            CombineMul(i64),
        }

        let mut work = Vec::new();
        work.push(Frame::Enter(self, 1));
        let mut values: Vec<IntervalBound> = Vec::new();
        let mut visited = 0usize;

        while let Some(frame) = work.pop() {
            match frame {
                Frame::Enter(term, depth) => {
                    if depth > max_depth {
                        return Err(ResourceGradeError::TermLimitExceeded);
                    }
                    visited = visited
                        .checked_add(1)
                        .filter(|&n| n <= max_nodes)
                        .ok_or(ResourceGradeError::TermLimitExceeded)?;
                    match term {
                        Self::Const(c) => values.push(IntervalBound::exact(*c)),
                        Self::Var(name) => {
                            let bound = env.get(name).copied().ok_or_else(|| {
                                ResourceGradeError::UnresolvedVariable {
                                    variable: name.clone(),
                                }
                            })?;
                            values.push(bound.validate()?);
                        }
                        Self::Add(lhs, rhs) => {
                            let next_depth = depth + 1;
                            work.push(Frame::CombineAdd);
                            work.push(Frame::Enter(rhs, next_depth));
                            work.push(Frame::Enter(lhs, next_depth));
                        }
                        Self::Sub(lhs, rhs) => {
                            let next_depth = depth + 1;
                            work.push(Frame::CombineSub);
                            work.push(Frame::Enter(rhs, next_depth));
                            work.push(Frame::Enter(lhs, next_depth));
                        }
                        Self::MulConst(k, inner) => {
                            let next_depth = depth + 1;
                            work.push(Frame::CombineMul(*k));
                            work.push(Frame::Enter(inner, next_depth));
                        }
                    }
                }
                Frame::CombineAdd => {
                    let rhs = values.pop().expect("CombineAdd rhs");
                    let lhs = values.pop().expect("CombineAdd lhs");
                    values.push(lhs.checked_add(rhs)?);
                }
                Frame::CombineSub => {
                    let rhs = values.pop().expect("CombineSub rhs");
                    let lhs = values.pop().expect("CombineSub lhs");
                    values.push(lhs.checked_sub(rhs)?);
                }
                Frame::CombineMul(k) => {
                    let inner = values.pop().expect("CombineMul inner");
                    values.push(inner.checked_mul_const(k)?);
                }
            }
        }

        debug_assert_eq!(values.len(), 1);
        Ok(values.pop().expect("root evaluation value"))
    }
}

/// Ordered pomonoid semantics for graded effects `eff [E @ g]` (§5.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GradeKind {
    /// Additive budgets (`Region`, `Fuel`): `seq = +`, `branch = max`, `loop(k) = k * body`, `<=`.
    Additive,
    /// Tropical call-stack depth (`Stack`): `seq = max`, `branch = max`, `frame + callee_max`, `<=`.
    TropicalStack,
    /// Constant-time execution (`ConstTime`): `seq = +`, `branch` requires exact equality, `==`.
    ExactEquality,
}

impl GradeKind {
    /// Pomonoid identity element `0`.
    pub const fn identity(self) -> u64 {
        0
    }

    /// Sequential composition `g1 ⊕ g2`.
    pub const fn seq(self, lhs: u64, rhs: u64) -> Result<u64, ResourceGradeError> {
        match self {
            Self::Additive | Self::ExactEquality => match lhs.checked_add(rhs) {
                Some(sum) => Ok(sum),
                None => Err(ResourceGradeError::ArithmeticOverflow),
            },
            Self::TropicalStack => Ok(if lhs >= rhs { lhs } else { rhs }),
        }
    }

    /// Control-flow branch join `g_then ⊔ g_else`.
    pub const fn branch(self, then_grade: u64, else_grade: u64) -> Result<u64, ResourceGradeError> {
        match self {
            Self::Additive | Self::TropicalStack => Ok(if then_grade >= else_grade {
                then_grade
            } else {
                else_grade
            }),
            Self::ExactEquality => {
                if then_grade == else_grade {
                    Ok(then_grade)
                } else {
                    Err(ResourceGradeError::BranchGradeMismatch {
                        then_grade,
                        else_grade,
                    })
                }
            }
        }
    }

    /// Bounded loop `0..trips` scaling of a loop body's grade.
    pub const fn loop_scale(self, body_grade: u64, trips: u64) -> Result<u64, ResourceGradeError> {
        match self {
            Self::Additive | Self::ExactEquality => match body_grade.checked_mul(trips) {
                Some(total) => Ok(total),
                None => Err(ResourceGradeError::ArithmeticOverflow),
            },
            Self::TropicalStack => Ok(if trips == 0 { 0 } else { body_grade }),
        }
    }

    /// Tropical caller stack depth `frame_self + max_i callee_i` (§5.3).
    pub fn caller_stack_depth(self_frame: u64, callees: &[u64]) -> Result<u64, ResourceGradeError> {
        let max_callee = callees.iter().copied().max().unwrap_or(0);
        self_frame
            .checked_add(max_callee)
            .ok_or(ResourceGradeError::ArithmeticOverflow)
    }

    /// Pomonoid order `lhs ⊑ rhs`: whether `lhs` satisfies a declared ceiling `rhs`.
    pub const fn satisfies_ceiling(self, observed: u64, ceiling: u64) -> bool {
        match self {
            Self::Additive | Self::TropicalStack => observed <= ceiling,
            Self::ExactEquality => observed == ceiling,
        }
    }
}

/// Errors produced by ordered pomonoid operations or `RefineTerm` ceiling checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResourceGradeError {
    InvalidInterval {
        min: i64,
        max: i64,
    },
    UnresolvedVariable {
        variable: String,
    },
    ArithmeticOverflow,
    TermLimitExceeded,
    NegativeUsageBound {
        effect: String,
        inferred_min: i64,
        inferred_max: i64,
    },
    ResourceGradeExceeded {
        effect: String,
        inferred_max: u64,
        ceiling: u64,
    },
    BranchGradeMismatch {
        then_grade: u64,
        else_grade: u64,
    },
}

/// Verifies `eff [Region @ <= ceiling]` and returns `Ok(true)` when the inferred
/// usage interval is provably within `0..=ceiling` (§7.4).
pub fn verify_graded_resource_ceiling(
    effect_name: &str,
    usage_expr: &RefineTerm,
    ceiling: u64,
    env: &BTreeMap<String, IntervalBound>,
) -> Result<bool, ResourceGradeError> {
    let bounds = usage_expr.eval_bounds(env)?;
    if bounds.min < 0 {
        return Err(ResourceGradeError::NegativeUsageBound {
            effect: effect_name.into(),
            inferred_min: bounds.min,
            inferred_max: bounds.max,
        });
    }
    let inferred_max = bounds.max as u64;
    if inferred_max <= ceiling {
        Ok(true)
    } else {
        Err(ResourceGradeError::ResourceGradeExceeded {
            effect: effect_name.into(),
            inferred_max,
            ceiling,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn box_add(a: RefineTerm, b: RefineTerm) -> RefineTerm {
        RefineTerm::Add(Box::new(a), Box::new(b))
    }

    fn box_sub(a: RefineTerm, b: RefineTerm) -> RefineTerm {
        RefineTerm::Sub(Box::new(a), Box::new(b))
    }

    fn box_mul(k: i64, inner: RefineTerm) -> RefineTerm {
        RefineTerm::MulConst(k, Box::new(inner))
    }

    #[test]
    fn manuscript_region_scoped_exact_example_succeeds_and_overflow_fails() {
        // Chapter 4.6: `64 * count <= 2048` where `0 <= count <= 32`.
        let usage = box_mul(64, RefineTerm::Var("count".into()));
        let mut env = BTreeMap::new();
        env.insert("count".into(), IntervalBound::new(0, 32).unwrap());
        assert_eq!(
            verify_graded_resource_ceiling("Region", &usage, 2048, &env),
            Ok(true)
        );

        // Raising upper bound to 33 exceeds 2048 (`64 * 33 = 2112`).
        env.insert("count".into(), IntervalBound::new(0, 33).unwrap());
        assert_eq!(
            verify_graded_resource_ceiling("Region", &usage, 2048, &env),
            Err(ResourceGradeError::ResourceGradeExceeded {
                effect: "Region".into(),
                inferred_max: 2112,
                ceiling: 2048,
            })
        );
    }

    #[test]
    fn negative_bounds_and_unresolved_variables_are_rejected() {
        let term = box_sub(
            RefineTerm::Var("n".into()),
            RefineTerm::Var("offset".into()),
        );
        let mut env = BTreeMap::new();
        env.insert("n".into(), IntervalBound::new(1, 10).unwrap());
        assert_eq!(
            verify_graded_resource_ceiling("Fuel", &term, 100, &env),
            Err(ResourceGradeError::UnresolvedVariable {
                variable: "offset".into(),
            })
        );

        env.insert("offset".into(), IntervalBound::new(2, 5).unwrap());
        // [1 - 5, 10 - 2] = [-4, 8] -> negative lower bound is rejected.
        assert_eq!(
            verify_graded_resource_ceiling("Fuel", &term, 100, &env),
            Err(ResourceGradeError::NegativeUsageBound {
                effect: "Fuel".into(),
                inferred_min: -4,
                inferred_max: 8,
            })
        );
    }

    #[test]
    fn interval_arithmetic_never_saturates_past_i64_bounds() {
        let mut env = BTreeMap::new();
        env.insert(
            "x".into(),
            IntervalBound::new(i64::MAX - 1, i64::MAX).unwrap(),
        );
        let sum = box_add(RefineTerm::Var("x".into()), RefineTerm::Const(2));
        assert_eq!(
            verify_graded_resource_ceiling("Region", &sum, u64::MAX, &env),
            Err(ResourceGradeError::ArithmeticOverflow)
        );

        // Negating `i64::MIN` overflows `i64` and must not silently saturate.
        env.insert("y".into(), IntervalBound::exact(i64::MIN));
        let neg = box_mul(-1, RefineTerm::Var("y".into()));
        assert_eq!(
            neg.eval_bounds(&env),
            Err(ResourceGradeError::ArithmeticOverflow)
        );

        // Inverted environment intervals are rejected even if constructed directly.
        env.insert("bad".into(), IntervalBound { min: 5, max: 3 });
        assert_eq!(
            RefineTerm::Var("bad".into()).eval_bounds(&env),
            Err(ResourceGradeError::InvalidInterval { min: 5, max: 3 })
        );
    }

    #[test]
    fn bounded_depth_and_node_budgets_prevent_stack_overflow() {
        let mut term = RefineTerm::Const(1);
        for _ in 0..80 {
            term = box_add(term, RefineTerm::Const(1));
        }
        let env = BTreeMap::new();
        assert_eq!(
            term.eval_bounds(&env),
            Err(ResourceGradeError::TermLimitExceeded)
        );
        assert_eq!(
            term.eval_bounded(&env, 100, 10),
            Err(ResourceGradeError::TermLimitExceeded)
        );
        assert_eq!(
            term.eval_bounded(&env, 100, 500),
            Ok(IntervalBound::exact(81))
        );
    }

    #[test]
    fn exhaustive_small_domain_interval_oracle() {
        let env_ranges = [(-3, -1), (-2, 2), (0, 3), (1, 1)];
        let coeffs = [-2i64, 0, 3];
        for &(ax0, ax1) in &env_ranges {
            for &(bx0, bx1) in &env_ranges {
                let mut env = BTreeMap::new();
                env.insert("a".into(), IntervalBound::new(ax0, ax1).unwrap());
                env.insert("b".into(), IntervalBound::new(bx0, bx1).unwrap());
                for &k in &coeffs {
                    let term = box_sub(
                        box_mul(k, RefineTerm::Var("a".into())),
                        RefineTerm::Var("b".into()),
                    );
                    let got = term.eval_bounds(&env).unwrap();
                    let mut exact_min = i64::MAX;
                    let mut exact_max = i64::MIN;
                    for a in ax0..=ax1 {
                        for b in bx0..=bx1 {
                            let val = k * a - b;
                            exact_min = exact_min.min(val);
                            exact_max = exact_max.max(val);
                        }
                    }
                    assert_eq!(got, IntervalBound::new(exact_min, exact_max).unwrap());
                }
            }
        }
    }

    #[test]
    fn pomonoid_laws_and_effect_grade_semantics() {
        let kinds = [
            GradeKind::Additive,
            GradeKind::TropicalStack,
            GradeKind::ExactEquality,
        ];
        let vals = [0u64, 1, 4, 16];

        for kind in kinds {
            let id = kind.identity();
            for &a in &vals {
                // Identity
                assert_eq!(kind.seq(id, a), Ok(a));
                assert_eq!(kind.seq(a, id), Ok(a));
                // 0-trip loop yields identity
                assert_eq!(kind.loop_scale(a, 0), Ok(id));
                // 1-trip loop yields body grade
                assert_eq!(kind.loop_scale(a, 1), Ok(a));
                // Branching identical grades succeeds
                assert_eq!(kind.branch(a, a), Ok(a));

                for &b in &vals {
                    // Associativity on small values
                    for &c in &vals {
                        let lhs = kind.seq(kind.seq(a, b).unwrap(), c).unwrap();
                        let rhs = kind.seq(a, kind.seq(b, c).unwrap()).unwrap();
                        assert_eq!(lhs, rhs);
                    }
                    // Monotonicity with respect to `satisfies_ceiling`
                    for &a2 in &vals {
                        for &b2 in &vals {
                            if kind.satisfies_ceiling(a, a2) && kind.satisfies_ceiling(b, b2) {
                                let s1 = kind.seq(a, b).unwrap();
                                let s2 = kind.seq(a2, b2).unwrap();
                                assert!(kind.satisfies_ceiling(s1, s2));
                            }
                        }
                    }
                }
            }
        }

        // Tropical stack depth: `frame_self + max(callees)`
        assert_eq!(GradeKind::caller_stack_depth(64, &[]), Ok(64));
        assert_eq!(GradeKind::caller_stack_depth(64, &[128, 320, 96]), Ok(384));
        assert_eq!(
            GradeKind::TropicalStack.seq(128, 320),
            Ok(320),
            "sequential calls reuse caller stack depth"
        );
        assert_eq!(
            GradeKind::caller_stack_depth(u64::MAX, &[1]),
            Err(ResourceGradeError::ArithmeticOverflow)
        );

        // Exact-equality timing rejects divergent branches
        assert_eq!(
            GradeKind::ExactEquality.branch(12, 13),
            Err(ResourceGradeError::BranchGradeMismatch {
                then_grade: 12,
                else_grade: 13,
            })
        );
        assert!(!GradeKind::ExactEquality.satisfies_ceiling(11, 12));
        assert!(GradeKind::ExactEquality.satisfies_ceiling(12, 12));

        // Additive overflow checks
        assert_eq!(
            GradeKind::Additive.seq(u64::MAX, 1),
            Err(ResourceGradeError::ArithmeticOverflow)
        );
        assert_eq!(
            GradeKind::Additive.loop_scale(u64::MAX / 2 + 1, 2),
            Err(ResourceGradeError::ArithmeticOverflow)
        );
    }
}
