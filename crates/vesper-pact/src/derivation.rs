//! Proof trees produced by the solver and an independent structural checker.
//!
//! A derivation is the elaboration evidence the compiler needs (which impl or
//! model was selected, with which arguments) and a certificate that can be
//! re-checked without search. [`check_derivation`] re-validates each step
//! against the declarations and environment; it does not trust the solver.

use crate::ids::*;
use crate::mode::{ModeVerdict, decide, mode_atoms};
use crate::program::{ConformanceDecl, Env, Program, QueryDto, Scope, Validator, validate_query};
use crate::term::*;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ConformanceSource {
    Impl(ImplId),
    Model(ModelId),
}

impl fmt::Display for ConformanceSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConformanceSource::Impl(i) => write!(f, "{i}"),
            ConformanceSource::Model(m) => write!(f, "{m}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Step {
    /// `Implements` by an impl/model; children prove its where-clauses.
    Conformance {
        source: ConformanceSource,
        args: Vec<GenericArg>,
    },
    /// `Implements` by environment assumption `index`.
    Assumption { index: u32 },
    /// `Implements` for a rigid projection self type, by bound `bound` of its
    /// associated type; the single child proves the projection's trait ref.
    AliasBound { bound: u32 },
    /// `ProjectionEq` by an impl/model's associated value. Children prove the
    /// where-clauses, followed by `value_normalizations` `ProjectionEq`
    /// proofs normalizing projections inside the associated value.
    ProjectionConformance {
        source: ConformanceSource,
        args: Vec<GenericArg>,
        value_normalizations: u32,
    },
    /// `ProjectionEq` by environment assumption `index`.
    ProjectionAssumption { index: u32 },
    /// `ProjectionEq(p, p)` for a rigid projection; the child proves p's trait ref.
    RigidProjection,
    /// `ModeBound` computed structurally.
    Mode,
    /// A goal containing projections: children are the `ProjectionEq`
    /// proofs (innermost-first, left-to-right), then the normalized goal.
    Normalize,
    /// Leaf closing a cycle whose goals are all coinductive.
    CoinductiveHypothesis,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Derivation {
    pub goal: Goal,
    pub step: Step,
    pub children: Vec<Derivation>,
}

impl Derivation {
    pub fn leaf(goal: Goal, step: Step) -> Self {
        Derivation {
            goal,
            step,
            children: vec![],
        }
    }

    pub(crate) fn fold(&self, f: &mut dyn VarFolder) -> Result<Derivation, DuplicateLabel> {
        let step = match &self.step {
            Step::Conformance { source, args } => Step::Conformance {
                source: *source,
                args: fold_args(args, f)?,
            },
            Step::ProjectionConformance {
                source,
                args,
                value_normalizations,
            } => Step::ProjectionConformance {
                source: *source,
                args: fold_args(args, f)?,
                value_normalizations: *value_normalizations,
            },
            other => other.clone(),
        };
        Ok(Derivation {
            goal: fold_goal(&self.goal, f)?,
            step,
            children: self
                .children
                .iter()
                .map(|c| c.fold(f))
                .collect::<Result<_, _>>()?,
        })
    }

    /// Whether any leaf closes a coinductive cycle.
    pub fn uses_coinduction(&self) -> bool {
        self.step == Step::CoinductiveHypothesis
            || self.children.iter().any(|c| c.uses_coinduction())
    }

    /// Number of nodes.
    pub fn size(&self) -> usize {
        1 + self.children.iter().map(|c| c.size()).sum::<usize>()
    }

    /// Deterministic indented rendering.
    pub fn render(&self) -> String {
        let mut s = String::new();
        self.render_into(0, &mut s);
        s
    }

    fn render_into(&self, depth: usize, s: &mut String) {
        use std::fmt::Write;
        let step = match &self.step {
            Step::Conformance { source, .. } => format!("by {source}"),
            Step::Assumption { index } => format!("by assumption {index}"),
            Step::AliasBound { bound } => format!("by alias bound {bound}"),
            Step::ProjectionConformance { source, .. } => format!("by value of {source}"),
            Step::ProjectionAssumption { index } => format!("by assumption {index}"),
            Step::RigidProjection => "rigid".into(),
            Step::Mode => "by modal kinding".into(),
            Step::Normalize => "after normalization".into(),
            Step::CoinductiveHypothesis => "by coinductive hypothesis".into(),
        };
        let _ = writeln!(s, "{}{}  [{}]", "  ".repeat(depth), self.goal, step);
        for c in &self.children {
            c.render_into(depth + 1, s);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DerivationError {
    /// Child indices from the root to the offending node.
    pub path: Vec<usize>,
    pub message: String,
}

impl fmt::Display for DerivationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "at {:?}: {}", self.path, self.message)
    }
}

/// Re-checks a derivation against the program and environment.
///
/// Checked per node: impl/model heads and where-clauses instantiate exactly
/// to the recorded goals (with recorded argument kinds), assumptions exist
/// in the environment, associated values match after replaying the recorded
/// normalizations, mode bounds recompute, rigid projections have a
/// placeholder or projection self type, and every coinductive hypothesis
/// closes a cycle (up to renaming of inference variables) whose goals are
/// all `Implements` goals of coinductive traits.
pub fn check_derivation(
    program: &Program,
    env: &Env,
    d: &Derivation,
) -> Result<(), DerivationError> {
    // Certificates are public DTOs, not trusted solver internals. Rename
    // sparse inference ids densely before validation to avoid allocating by
    // an attacker-controlled id, while retaining sharing and kind checks.
    let mut canonicalizer = crate::canon::Canonicalizer::new(None);
    let d = d.fold(&mut canonicalizer).map_err(|_| DerivationError {
        path: vec![],
        message: "duplicate effect label in derivation".into(),
    })?;
    validate_query(
        program,
        &QueryDto::new(env.clone(), canonicalizer.binders.clone(), d.goal.clone()),
    )
    .map_err(|diags| DerivationError {
        path: vec![],
        message: crate::diag::render(&diags),
    })?;
    validate_nodes(program, env, &canonicalizer.binders, &d, &mut vec![])?;
    Checker {
        program,
        env,
        ancestors: vec![],
        path: vec![],
    }
    .check(&d)
}

// Validate every goal and recorded instantiation before any declaration lookup
// or modal computation in the structural checker.
fn validate_nodes(
    program: &Program,
    env: &Env,
    binders: &[ParamKind],
    d: &Derivation,
    path: &mut Vec<usize>,
) -> Result<(), DerivationError> {
    let mut diags = vec![];
    let mut validator = Validator {
        dto: program.dto(),
        diags: &mut diags,
    };
    let scope = Scope::Query {
        placeholders: &env.placeholders,
        binders,
    };
    validator.goal(&d.goal, scope, crate::diag::Subject::Query);
    if let Step::Conformance { args, .. } | Step::ProjectionConformance { args, .. } = &d.step {
        for arg in args {
            validator.arg(arg, scope, crate::diag::Subject::Query);
        }
    }
    if !diags.is_empty() {
        return Err(DerivationError {
            path: path.clone(),
            message: crate::diag::render(&diags),
        });
    }
    for (i, child) in d.children.iter().enumerate() {
        path.push(i);
        validate_nodes(program, env, binders, child, path)?;
        path.pop();
    }
    Ok(())
}

struct Checker<'a> {
    program: &'a Program,
    env: &'a Env,
    ancestors: Vec<&'a Goal>,
    path: Vec<usize>,
}

impl<'a> Checker<'a> {
    fn fail<T>(&self, message: impl Into<String>) -> Result<T, DerivationError> {
        Err(DerivationError {
            path: self.path.clone(),
            message: message.into(),
        })
    }

    fn check(&mut self, d: &'a Derivation) -> Result<(), DerivationError> {
        self.node(d)?;
        if d.step == Step::CoinductiveHypothesis {
            return Ok(());
        }
        self.ancestors.push(&d.goal);
        for (i, c) in d.children.iter().enumerate() {
            self.path.push(i);
            self.check(c)?;
            self.path.pop();
        }
        self.ancestors.pop();
        Ok(())
    }

    fn conformance(
        &self,
        source: ConformanceSource,
    ) -> Result<&'a ConformanceDecl, DerivationError> {
        let decl = match source {
            ConformanceSource::Impl(i) => self.program.dto().impls.get(i.index()).map(|d| &d.decl),
            ConformanceSource::Model(m) => {
                self.program.dto().models.get(m.index()).map(|d| &d.decl)
            }
        };
        decl.ok_or_else(|| DerivationError {
            path: self.path.clone(),
            message: format!("unknown conformance {source}"),
        })
    }

    fn kinds_match(
        &self,
        decl: &ConformanceDecl,
        args: &[GenericArg],
    ) -> Result<(), DerivationError> {
        if decl.params.len() != args.len()
            || decl.params.iter().zip(args).any(|(k, a)| *k != a.kind())
        {
            return self.fail("conformance arguments do not match parameter kinds");
        }
        Ok(())
    }

    fn where_children(
        &self,
        decl: &ConformanceDecl,
        args: &[GenericArg],
        children: &[Derivation],
    ) -> Result<(), DerivationError> {
        for (w, c) in decl.where_clauses.iter().zip(children) {
            let expected =
                instantiate_goal(w, args).or_else(|_| self.fail("ill-formed where-clause"))?;
            if expected != c.goal {
                return self.fail(format!(
                    "where-clause child proves `{}`, expected `{expected}`",
                    c.goal
                ));
            }
        }
        Ok(())
    }

    fn node(&self, d: &Derivation) -> Result<(), DerivationError> {
        match (&d.step, &d.goal) {
            (Step::Conformance { source, args }, Goal::Implements { trait_ref, brand }) => {
                let decl = self.conformance(*source)?;
                self.kinds_match(decl, args)?;
                let expected_brand = match source {
                    ConformanceSource::Impl(_) => Brand::Canonical,
                    ConformanceSource::Model(m) => Brand::Named(*m),
                };
                if *brand != expected_brand {
                    return self.fail(format!("brand {brand} does not match {source}"));
                }
                let head = instantiate_trait_ref(&decl.trait_ref, args)
                    .or_else(|_| self.fail("ill-formed head"))?;
                if head != *trait_ref {
                    return self.fail(format!("head `{head}` does not match goal"));
                }
                if d.children.len() != decl.where_clauses.len() {
                    return self.fail("wrong number of where-clause proofs");
                }
                self.where_children(decl, args, &d.children)
            }
            (Step::Assumption { index }, g @ Goal::Implements { .. })
            | (Step::ProjectionAssumption { index }, g @ Goal::ProjectionEq { .. }) => {
                match self.env.assumptions.get(*index as usize) {
                    Some(a) if a == g => {}
                    _ => return self.fail(format!("assumption {index} does not state the goal")),
                }
                if !d.children.is_empty() {
                    return self.fail("assumption has children");
                }
                Ok(())
            }
            (Step::AliasBound { bound }, Goal::Implements { trait_ref, brand }) => {
                let Ty::Projection(p) = &trait_ref.self_ty else {
                    return self.fail("alias bound on a non-projection self type");
                };
                let t = self.program.trait_decl(p.trait_ref.trait_id);
                let Some(b) = t.assoc[p.assoc.0 as usize].bounds.get(*bound as usize) else {
                    return self.fail("unknown alias bound");
                };
                let mut binder = vec![GenericArg::Ty(p.trait_ref.self_ty.clone())];
                binder.extend(p.trait_ref.args.iter().cloned());
                let args = instantiate_args(&b.args, &binder)
                    .or_else(|_| self.fail("ill-formed bound"))?;
                if b.trait_id != trait_ref.trait_id
                    || args != trait_ref.args
                    || instantiate_brand(&b.brand, &binder) != *brand
                {
                    return self.fail("alias bound does not state the goal");
                }
                self.single_trait_child(d, p)
            }
            (
                Step::ProjectionConformance {
                    source,
                    args,
                    value_normalizations,
                },
                Goal::ProjectionEq { projection, ty },
            ) => {
                let decl = self.conformance(*source)?;
                self.kinds_match(decl, args)?;
                let expected_brand = match source {
                    ConformanceSource::Impl(_) => Brand::Canonical,
                    ConformanceSource::Model(m) => Brand::Named(*m),
                };
                if projection.brand != expected_brand {
                    return self.fail("projection brand does not match source");
                }
                let head = instantiate_trait_ref(&decl.trait_ref, args)
                    .or_else(|_| self.fail("ill-formed head"))?;
                if head != projection.trait_ref {
                    return self.fail("head does not match projection");
                }
                let n = decl.where_clauses.len();
                let k = *value_normalizations as usize;
                if d.children.len() != n + k {
                    return self.fail("wrong number of children");
                }
                self.where_children(decl, args, &d.children[..n])?;
                let value = decl
                    .assoc_values
                    .get(projection.assoc.0 as usize)
                    .ok_or(())
                    .or_else(|_| self.fail("missing associated value"))?;
                let value =
                    instantiate_ty(value, args).or_else(|_| self.fail("ill-formed value"))?;
                let replayed = self.replay_ty(&value, &d.children[n..])?;
                if replayed != *ty {
                    return self.fail(format!(
                        "associated value `{replayed}` does not match `{ty}`"
                    ));
                }
                Ok(())
            }
            (Step::RigidProjection, Goal::ProjectionEq { projection, ty }) => {
                if *ty != Ty::Projection(Box::new(projection.clone())) {
                    return self.fail("rigid projection must equal itself");
                }
                match &projection.trait_ref.self_ty {
                    Ty::Var(Var::Placeholder(_)) | Ty::Projection(_) => {}
                    _ => return self.fail("rigid projection needs a rigid self type"),
                }
                self.single_trait_child(d, projection)
            }
            (Step::Mode, Goal::ModeBound { ty, bound }) => {
                let mut atoms = vec![];
                mode_atoms(self.program, self.env, ty, &mut atoms)
                    .or_else(|_| self.fail("ill-formed type"))?;
                match decide(&atoms, bound) {
                    ModeVerdict::Holds => {}
                    _ => return self.fail("mode bound does not hold"),
                }
                if !d.children.is_empty() {
                    return self.fail("mode step has children");
                }
                Ok(())
            }
            (Step::Normalize, g) => {
                let Some((last, norms)) = d.children.split_last() else {
                    return self.fail("normalization without a normalized goal");
                };
                let mut it = NormReplay {
                    norms,
                    next: 0,
                    ok: true,
                };
                let replayed = it.goal(g);
                if !it.ok || it.next != norms.len() {
                    return self.fail("normalizations do not replay");
                }
                if replayed != last.goal {
                    return self.fail(format!(
                        "normalized goal `{}` expected `{replayed}`",
                        last.goal
                    ));
                }
                Ok(())
            }
            (Step::CoinductiveHypothesis, g) => {
                if !d.children.is_empty() {
                    return self.fail("coinductive hypothesis has children");
                }
                let key = alpha(g);
                let Some(pos) = self.ancestors.iter().rposition(|a| alpha(a) == key) else {
                    return self.fail("coinductive hypothesis without a matching ancestor");
                };
                for a in self.ancestors[pos..]
                    .iter()
                    .copied()
                    .chain(std::iter::once(g))
                {
                    match a {
                        Goal::Implements { trait_ref, .. }
                            if self.program.trait_decl(trait_ref.trait_id).coinductive => {}
                        _ => {
                            return self
                                .fail(format!("cycle passes through non-coinductive goal `{a}`"));
                        }
                    }
                }
                Ok(())
            }
            (step, goal) => self.fail(format!("step {step:?} cannot prove `{goal}`")),
        }
    }

    fn single_trait_child(&self, d: &Derivation, p: &Projection) -> Result<(), DerivationError> {
        let expected = Goal::Implements {
            trait_ref: p.trait_ref.clone(),
            brand: p.brand.clone(),
        };
        match d.children.as_slice() {
            [c] if c.goal == expected => Ok(()),
            _ => self.fail("expected one child proving the projection's trait"),
        }
    }

    fn replay_ty(&self, t: &Ty, norms: &[Derivation]) -> Result<Ty, DerivationError> {
        let mut it = NormReplay {
            norms,
            next: 0,
            ok: true,
        };
        let out = it.ty(t);
        if !it.ok || it.next != norms.len() {
            return self.fail("value normalizations do not replay");
        }
        Ok(out)
    }
}

/// Replays eager normalization: each projection (innermost first) must equal
/// the next recorded `ProjectionEq` goal's projection and is replaced by its
/// right-hand side.
struct NormReplay<'a> {
    norms: &'a [Derivation],
    next: usize,
    ok: bool,
}

impl NormReplay<'_> {
    fn goal(&mut self, g: &Goal) -> Goal {
        match g {
            Goal::Implements { trait_ref, brand } => Goal::Implements {
                trait_ref: self.trait_ref(trait_ref),
                brand: brand.clone(),
            },
            Goal::ProjectionEq { projection, ty } => Goal::ProjectionEq {
                projection: Projection {
                    trait_ref: self.trait_ref(&projection.trait_ref),
                    brand: projection.brand.clone(),
                    assoc: projection.assoc,
                },
                ty: self.ty(ty),
            },
            Goal::ModeBound { ty, bound } => Goal::ModeBound {
                ty: self.ty(ty),
                bound: bound.clone(),
            },
        }
    }
    fn trait_ref(&mut self, t: &TraitRef) -> TraitRef {
        TraitRef {
            trait_id: t.trait_id,
            self_ty: self.ty(&t.self_ty),
            args: t.args.iter().map(|a| self.arg(a)).collect(),
        }
    }
    fn arg(&mut self, a: &GenericArg) -> GenericArg {
        match a {
            GenericArg::Ty(t) => GenericArg::Ty(self.ty(t)),
            GenericArg::Row(r) => {
                let mut row = r.clone();
                for l in &mut row.labels {
                    if let EffectLabel::Effect { args, .. } = l {
                        *args = args.iter().map(|a| self.arg(a)).collect();
                    }
                }
                GenericArg::Row(row)
            }
            other => other.clone(),
        }
    }
    fn ty(&mut self, t: &Ty) -> Ty {
        match t {
            Ty::Ctor { ctor, args } => Ty::Ctor {
                ctor: *ctor,
                args: args.iter().map(|a| self.arg(a)).collect(),
            },
            Ty::Var(v) => Ty::Var(*v),
            Ty::Projection(p) => {
                let inner = Projection {
                    trait_ref: self.trait_ref(&p.trait_ref),
                    brand: p.brand.clone(),
                    assoc: p.assoc,
                };
                match self.norms.get(self.next).map(|d| &d.goal) {
                    Some(Goal::ProjectionEq { projection, ty }) if *projection == inner => {
                        self.next += 1;
                        ty.clone()
                    }
                    _ => {
                        self.ok = false;
                        Ty::Projection(Box::new(inner))
                    }
                }
            }
        }
    }
}

/// Alpha-normal form: inference variables renumbered by first occurrence.
fn alpha(g: &Goal) -> Goal {
    let mut c = crate::canon::Canonicalizer::new(None);
    fold_goal(g, &mut c).unwrap_or_else(|_| g.clone())
}
