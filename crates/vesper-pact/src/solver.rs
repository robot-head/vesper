//! Tabled canonical goal solver.
//!
//! Every nested goal is canonicalized and answered through a table keyed by
//! `(environment, canonical goal)`. Cycles are detected on the active stack:
//! a cycle made only of coinductive `Implements` goals starts from the
//! provisional answer "holds" (greatest fixed point); any other cycle starts
//! from "no solution" (least fixed point). Cycle heads are re-evaluated until
//! their answer stops changing; results that depended on a provisional answer
//! of a still-active goal are not tabled.
//!
//! Outcomes are five-valued and fail closed: only [`Outcome::Proven`] is a
//! success. Ambiguity (more inference needed), unsupported reasoning and
//! overflow are never reported as success.

use crate::canon::{CanonicalGoal, Canonicalizer, canonicalize_goal, canonicalize_placeholders};
use crate::derivation::{ConformanceSource, Derivation, Step};
use crate::diag::Diagnostic;
use crate::ids::*;
use crate::infer::{InferCtxt, UnifyError};
use crate::mode::{ModeVerdict, decide, mode_atoms};
use crate::program::{ConformanceDecl, Env, Program, QueryDto, validate_query};
use crate::term::*;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

// ---------------------------------------------------------------------------
// Public results

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProofKind {
    /// Well-founded derivation.
    Finite,
    /// The derivation closes at least one coinductive cycle.
    Coinductive,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Solution {
    /// Kinds of free variables remaining in `var_values` / `derivation`
    /// (referenced as `Var::Infer(i)`, numbered independently of the query).
    pub binders: Vec<ParamKind>,
    /// Value of each query binder.
    pub var_values: Vec<GenericArg>,
    pub derivation: Derivation,
    pub proof_kind: ProofKind,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CandidateSource {
    Impl(ImplId),
    Model(ModelId),
    Assumption(u32),
    AliasBound(u32),
    Rigid,
}

impl From<ConformanceSource> for CandidateSource {
    fn from(s: ConformanceSource) -> Self {
        match s {
            ConformanceSource::Impl(i) => CandidateSource::Impl(i),
            ConformanceSource::Model(m) => CandidateSource::Model(m),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ambiguity {
    /// The self type is an unresolved inference variable.
    SelfTypeUnknown,
    /// Several candidates give different answers.
    MultipleCandidates(Vec<CandidateSource>),
    /// More than one model of the innermost matching `use model` scope applies.
    ConflictingScopedModels(Vec<ModelId>),
    /// Mode of an unresolved type or against an unresolved bound.
    ModeUnknown,
    /// Two distinct rigid projections containing inference variables.
    NonInjectiveProjection,
    /// Nested goals stopped making progress.
    Stalled(Box<Goal>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnsupportedReason {
    /// Merging effect rows repeated a label key (scoped/duplicate labels are
    /// outside this fragment).
    DuplicateEffectLabel(LabelKey),
    /// A goal was reached through both a coinductive and an inductive cycle
    /// in one evaluation.
    MixedCycle(Box<Goal>),
    /// Internal invariant violation; reported instead of guessing.
    Internal(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverflowKind {
    Depth,
    Steps,
    /// A cycle head did not reach a fixed point within the iteration budget.
    FixpointIterations,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// The failing goal, in canonical form.
    pub goal: Goal,
    pub reason: FailureReason,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FailureReason {
    /// No impl, model or assumption head unifies with the goal.
    NoCandidates,
    /// Every applicable candidate failed.
    CandidatesFailed(Vec<CandidateFailure>),
    ModeExceeds {
        atom: ModeTerm,
        bound: ModeTerm,
    },
    ModelTraitMismatch {
        model: ModelId,
    },
    /// The goal depends on itself through an inductive cycle and has no
    /// well-founded proof.
    InductiveCycle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateFailure {
    pub candidate: CandidateSource,
    pub cause: CandidateCause,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CandidateCause {
    Nested(Box<Failure>),
    /// The associated value does not unify with the expected type.
    ValueMismatch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Proven(Solution),
    Ambiguous(Ambiguity),
    NoSolution(Failure),
    Unsupported(UnsupportedReason),
    Overflow(OverflowKind),
}

impl Outcome {
    pub fn is_proven(&self) -> bool {
        matches!(self, Outcome::Proven(_))
    }
    /// Stable one-word classification.
    pub fn verdict(&self) -> &'static str {
        match self {
            Outcome::Proven(s) if s.proof_kind == ProofKind::Coinductive => "proven-coinductive",
            Outcome::Proven(_) => "proven",
            Outcome::Ambiguous(_) => "ambiguous",
            Outcome::NoSolution(_) => "no-solution",
            Outcome::Unsupported(_) => "unsupported",
            Outcome::Overflow(_) => "overflow",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SolverConfig {
    /// Maximum nesting of canonical goals. The solver recurses on the native
    /// stack; the default (48) stays within a 2 MiB thread stack in debug
    /// builds of the test suite. Raise it only together with the stack size.
    pub max_depth: usize,
    pub max_steps: u64,
    pub max_fixpoint_iterations: u32,
}

impl Default for SolverConfig {
    fn default() -> Self {
        SolverConfig {
            max_depth: 48,
            max_steps: 100_000,
            max_fixpoint_iterations: 16,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SolverStats {
    /// Canonical goal evaluations (including fixpoint re-runs).
    pub evaluations: u64,
    pub cache_hits: u64,
    pub coinductive_cycles: u64,
    pub inductive_cycles: u64,
    pub fixpoint_reruns: u64,
}

// ---------------------------------------------------------------------------
// Internal results

#[derive(Clone, Debug, PartialEq, Eq)]
struct Response {
    binders: Vec<ParamKind>,
    var_values: Vec<GenericArg>,
    derivation: Derivation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum QResult {
    Proven(Response),
    Ambiguous(Ambiguity),
    NoSolution(Failure),
    Unsupported(UnsupportedReason),
    Overflow(OverflowKind),
}

impl QResult {
    fn same_answer(&self, other: &QResult) -> bool {
        match (self, other) {
            (QResult::Proven(a), QResult::Proven(b)) => {
                a.binders == b.binders && a.var_values == b.var_values
            }
            _ => std::mem::discriminant(self) == std::mem::discriminant(other),
        }
    }
}

/// Why a candidate did not produce a proof.
enum CandErr {
    HeadMismatch,
    NoSolution(CandidateCause),
    Ambiguous(Ambiguity),
    Unsupported(UnsupportedReason),
    Overflow(OverflowKind),
}

impl From<DuplicateLabel> for CandErr {
    fn from(d: DuplicateLabel) -> Self {
        CandErr::Unsupported(UnsupportedReason::DuplicateEffectLabel(d.0))
    }
}

fn unify_err(e: UnifyError) -> CandErr {
    match e {
        UnifyError::Mismatch => CandErr::HeadMismatch,
        UnifyError::Ambiguous => CandErr::Ambiguous(Ambiguity::NonInjectiveProjection),
        UnifyError::DuplicateLabel(k) => {
            CandErr::Unsupported(UnsupportedReason::DuplicateEffectLabel(k))
        }
    }
}

/// Failure of a nested goal inside a candidate.
enum NestedErr {
    NoSolution(Box<Failure>),
    Ambiguous(Ambiguity),
    Unsupported(UnsupportedReason),
    Overflow(OverflowKind),
}

impl From<NestedErr> for CandErr {
    fn from(e: NestedErr) -> Self {
        match e {
            NestedErr::NoSolution(f) => CandErr::NoSolution(CandidateCause::Nested(f)),
            NestedErr::Ambiguous(a) => CandErr::Ambiguous(a),
            NestedErr::Unsupported(u) => CandErr::Unsupported(u),
            NestedErr::Overflow(o) => CandErr::Overflow(o),
        }
    }
}

impl From<DuplicateLabel> for NestedErr {
    fn from(d: DuplicateLabel) -> Self {
        NestedErr::Unsupported(UnsupportedReason::DuplicateEffectLabel(d.0))
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Key {
    env: u32,
    goal: CanonicalGoal,
}

struct StackEntry {
    key: Key,
    coinductive: bool,
    prov_co: QResult,
    prov_ind: QResult,
    used_co: bool,
    used_ind: bool,
    /// Lowest stack index whose provisional answer this evaluation used.
    min_dep: usize,
}

// ---------------------------------------------------------------------------

pub struct Solver<'p> {
    program: &'p Program,
    config: SolverConfig,
    envs: Vec<Rc<Env>>,
    env_ids: HashMap<Rc<Env>, u32>,
    cache: HashMap<Key, QResult>,
    stack: Vec<StackEntry>,
    steps: u64,
    stats: SolverStats,
}

impl<'p> Solver<'p> {
    pub fn new(program: &'p Program) -> Self {
        Self::with_config(program, SolverConfig::default())
    }

    pub fn with_config(program: &'p Program, config: SolverConfig) -> Self {
        Solver {
            program,
            config,
            envs: vec![],
            env_ids: HashMap::new(),
            cache: HashMap::new(),
            stack: vec![],
            steps: 0,
            stats: SolverStats::default(),
        }
    }

    pub fn stats(&self) -> SolverStats {
        self.stats
    }

    /// Number of tabled canonical answers.
    pub fn table_len(&self) -> usize {
        self.cache.len()
    }

    /// Solves one query. Invalid queries are rejected with sorted diagnostics.
    pub fn solve(&mut self, query: &QueryDto) -> Result<Outcome, Vec<Diagnostic>> {
        validate_query(self.program, query)?;
        self.steps = 0;
        let (env, goal, origin) = match canonicalize_placeholders(&query.env, &query.goal) {
            Ok(x) => x,
            Err(d) => {
                return Ok(Outcome::Unsupported(
                    UnsupportedReason::DuplicateEffectLabel(d.0),
                ));
            }
        };
        let env_id = self.intern_env(env);
        let mut cx = InferCtxt::new();
        let vars = cx.fresh_args(&query.binders);
        let outcome = match self.drive(env_id, &mut cx, vec![goal]) {
            Ok(mut proofs) => match make_response(&cx, &vars, proofs.remove(0)) {
                Ok(r) => {
                    let proof_kind = if r.derivation.uses_coinduction() {
                        ProofKind::Coinductive
                    } else {
                        ProofKind::Finite
                    };
                    Outcome::Proven(Solution {
                        binders: r.binders,
                        var_values: r.var_values,
                        derivation: r.derivation,
                        proof_kind,
                    })
                }
                Err(d) => Outcome::Unsupported(UnsupportedReason::DuplicateEffectLabel(d.0)),
            },
            Err(NestedErr::NoSolution(f)) => Outcome::NoSolution(*f),
            Err(NestedErr::Ambiguous(a)) => Outcome::Ambiguous(a),
            Err(NestedErr::Unsupported(u)) => Outcome::Unsupported(u),
            Err(NestedErr::Overflow(o)) => Outcome::Overflow(o),
        };
        Ok(restore_placeholders(outcome, &origin))
    }

    fn intern_env(&mut self, env: Env) -> u32 {
        let env = Rc::new(env);
        if let Some(&id) = self.env_ids.get(&env) {
            return id;
        }
        let id = self.envs.len() as u32;
        self.envs.push(env.clone());
        self.env_ids.insert(env, id);
        id
    }

    // -- obligation driver ----------------------------------------------------

    /// Proves `goals` in `cx`, returning one derivation per goal.
    fn drive(
        &mut self,
        env: u32,
        cx: &mut InferCtxt,
        goals: Vec<Goal>,
    ) -> Result<Vec<Derivation>, NestedErr> {
        struct Root {
            original: Goal,
            norms: Vec<usize>,
            main: usize,
        }
        let mut obligations: Vec<Goal> = vec![];
        let mut roots = vec![];
        for g in goals {
            let mut norms = vec![];
            let normalized = normalize_goal(cx, &g, &mut norms)?;
            let mut idxs = vec![];
            for n in norms {
                idxs.push(obligations.len());
                obligations.push(n);
            }
            roots.push(Root {
                original: g,
                norms: idxs,
                main: obligations.len(),
            });
            obligations.push(normalized);
        }
        let mut proofs: Vec<Option<Derivation>> = vec![None; obligations.len()];
        let mut pending: Vec<usize> = (0..obligations.len()).collect();
        loop {
            let mut progress = false;
            let mut still = vec![];
            let mut first_ambiguity = None;
            for idx in pending {
                match self.solve_nested(env, cx, &obligations[idx]) {
                    Ok(d) => {
                        proofs[idx] = Some(d);
                        progress = true;
                    }
                    Err(NestedErr::Ambiguous(a)) => {
                        first_ambiguity.get_or_insert(a);
                        still.push(idx);
                    }
                    Err(e) => return Err(e),
                }
            }
            if still.is_empty() {
                break;
            }
            if !progress {
                let g = cx.resolve_goal(&obligations[still[0]])?;
                return Err(NestedErr::Ambiguous(match first_ambiguity {
                    Some(Ambiguity::Stalled(inner)) => Ambiguity::Stalled(inner),
                    Some(a) if still.len() == 1 => a,
                    _ => Ambiguity::Stalled(Box::new(g)),
                }));
            }
            pending = still;
        }
        let mut out = vec![];
        for r in roots {
            let main = proofs[r.main].take().expect("proved");
            if r.norms.is_empty() {
                out.push(main);
            } else {
                let mut children: Vec<Derivation> = r
                    .norms
                    .iter()
                    .map(|&i| proofs[i].take().expect("proved"))
                    .collect();
                children.push(main);
                out.push(Derivation {
                    goal: r.original,
                    step: Step::Normalize,
                    children,
                });
            }
        }
        Ok(out)
    }

    /// Canonicalizes a goal of `cx`, answers it through the table, and applies
    /// the answer back to `cx`.
    fn solve_nested(
        &mut self,
        env: u32,
        cx: &mut InferCtxt,
        goal: &Goal,
    ) -> Result<Derivation, NestedErr> {
        let (canonical, origins) = canonicalize_goal(cx, goal)?;
        let result = self.solve_key(Key {
            env,
            goal: canonical,
        });
        match result {
            QResult::Proven(resp) => {
                let fresh = cx.fresh_args(&resp.binders);
                let mut subst = InferSubst(&fresh);
                for (i, value) in resp.var_values.iter().enumerate() {
                    let value = fold_arg(value, &mut subst)?;
                    let var = GenericArg::var(value.kind(), Var::Infer(origins[i]));
                    if let Err(e) = cx.unify_arg(&var, &value) {
                        return Err(match e {
                            UnifyError::DuplicateLabel(k) => {
                                NestedErr::Unsupported(UnsupportedReason::DuplicateEffectLabel(k))
                            }
                            _ => NestedErr::Unsupported(UnsupportedReason::Internal(format!(
                                "answer for `{goal}` does not apply to its query"
                            ))),
                        });
                    }
                }
                Ok(resp.derivation.fold(&mut subst)?)
            }
            QResult::Ambiguous(a) => Err(NestedErr::Ambiguous(a)),
            QResult::NoSolution(f) => Err(NestedErr::NoSolution(Box::new(f))),
            QResult::Unsupported(u) => Err(NestedErr::Unsupported(u)),
            QResult::Overflow(o) => Err(NestedErr::Overflow(o)),
        }
    }

    // -- tabling and cycles -----------------------------------------------------

    fn solve_key(&mut self, key: Key) -> QResult {
        if let Some(r) = self.cache.get(&key) {
            self.stats.cache_hits += 1;
            return r.clone();
        }
        if let Some(i) = self.stack.iter().position(|e| e.key == key) {
            let coinductive = self.stack[i..].iter().all(|e| e.coinductive);
            let top = self.stack.len() - 1;
            self.stack[top].min_dep = self.stack[top].min_dep.min(i);
            let entry = &mut self.stack[i];
            if coinductive {
                entry.used_co = true;
                self.stats.coinductive_cycles += 1;
            } else {
                entry.used_ind = true;
                self.stats.inductive_cycles += 1;
            }
            if entry.used_co && entry.used_ind {
                return QResult::Unsupported(UnsupportedReason::MixedCycle(Box::new(
                    key.goal.goal.clone(),
                )));
            }
            return if coinductive {
                entry.prov_co.clone()
            } else {
                entry.prov_ind.clone()
            };
        }
        if self.stack.len() >= self.config.max_depth {
            return QResult::Overflow(OverflowKind::Depth);
        }
        let coinductive = match &key.goal.goal {
            Goal::Implements { trait_ref, .. } => {
                self.program.trait_decl(trait_ref.trait_id).coinductive
            }
            _ => false,
        };
        let identity = Response {
            binders: key.goal.binders.clone(),
            var_values: key
                .goal
                .binders
                .iter()
                .enumerate()
                .map(|(i, &k)| GenericArg::var(k, Var::Infer(i as u32)))
                .collect(),
            derivation: Derivation::leaf(key.goal.goal.clone(), Step::CoinductiveHypothesis),
        };
        let index = self.stack.len();
        self.stack.push(StackEntry {
            key: key.clone(),
            coinductive,
            prov_co: QResult::Proven(identity),
            prov_ind: QResult::NoSolution(Failure {
                goal: key.goal.goal.clone(),
                reason: FailureReason::InductiveCycle,
            }),
            used_co: false,
            used_ind: false,
            min_dep: usize::MAX,
        });
        let mut iterations = 0;
        let result = loop {
            {
                let e = &mut self.stack[index];
                e.used_co = false;
                e.used_ind = false;
                e.min_dep = usize::MAX;
            }
            let result = self.evaluate(&key);
            let e = &mut self.stack[index];
            if matches!(
                result,
                QResult::Unsupported(UnsupportedReason::MixedCycle(_))
            ) || (e.used_co && e.used_ind)
            {
                break QResult::Unsupported(UnsupportedReason::MixedCycle(Box::new(
                    key.goal.goal.clone(),
                )));
            }
            let stable = (!e.used_co || result.same_answer(&e.prov_co))
                && (!e.used_ind || result.same_answer(&e.prov_ind));
            if stable {
                break result;
            }
            iterations += 1;
            if iterations >= self.config.max_fixpoint_iterations {
                break QResult::Overflow(OverflowKind::FixpointIterations);
            }
            self.stats.fixpoint_reruns += 1;
            if e.used_co {
                e.prov_co = match &result {
                    QResult::Proven(r) => QResult::Proven(Response {
                        derivation: Derivation::leaf(
                            key.goal.goal.clone(),
                            Step::CoinductiveHypothesis,
                        ),
                        ..r.clone()
                    }),
                    other => other.clone(),
                };
            }
            if e.used_ind {
                e.prov_ind = result;
            }
        };
        let entry = self.stack.pop().expect("pushed");
        if entry.min_dep < index {
            if let Some(parent) = self.stack.last_mut() {
                parent.min_dep = parent.min_dep.min(entry.min_dep);
            }
        } else if !matches!(result, QResult::Overflow(_)) {
            self.cache.insert(key, result.clone());
        }
        result
    }

    fn bump(&mut self) -> Result<(), CandErr> {
        self.steps += 1;
        if self.steps > self.config.max_steps {
            return Err(CandErr::Overflow(OverflowKind::Steps));
        }
        Ok(())
    }

    // -- evaluation -------------------------------------------------------------

    fn evaluate(&mut self, key: &Key) -> QResult {
        self.stats.evaluations += 1;
        if let Err(CandErr::Overflow(o)) = self.bump() {
            return QResult::Overflow(o);
        }
        let env = self.envs[key.env as usize].clone();
        let mut cx = InferCtxt::new();
        let vars = cx.fresh_args(&key.goal.binders);
        let goal = key.goal.goal.clone();
        let candidates = match &goal {
            Goal::Implements { trait_ref, brand } => {
                self.implements(key.env, &env, &cx, trait_ref, brand)
            }
            Goal::ProjectionEq { projection, ty } => {
                self.projection_eq(key.env, &env, &cx, projection, ty)
            }
            Goal::ModeBound { ty, bound } => {
                return self.mode_bound(&env, &cx, &goal, ty, bound, &vars);
            }
        };
        let phase = match candidates {
            Ok(p) => p,
            Err(CandErr::Ambiguous(a)) => return QResult::Ambiguous(a),
            Err(CandErr::Unsupported(u)) => return QResult::Unsupported(u),
            Err(CandErr::Overflow(o)) => return QResult::Overflow(o),
            Err(CandErr::NoSolution(_)) | Err(CandErr::HeadMismatch) => {
                return QResult::NoSolution(Failure {
                    goal,
                    reason: FailureReason::NoCandidates,
                });
            }
        };
        if let Some(model) = phase.model_trait_mismatch {
            return QResult::NoSolution(Failure {
                goal,
                reason: FailureReason::ModelTraitMismatch { model },
            });
        }
        combine(goal, &vars, phase.results)
    }

    /// Selects how a brand is resolved, binding implicit brands.
    fn brand_mode(
        &mut self,
        env: &Env,
        cx: &mut InferCtxt,
        trait_ref: &TraitRef,
        brand: &Brand,
    ) -> Result<BrandMode, CandErr> {
        match cx.resolve_brand(brand) {
            Brand::Canonical => Ok(BrandMode::Canonical),
            Brand::Named(m) => Ok(BrandMode::Named(m)),
            Brand::Var(Var::Placeholder(p)) => Ok(BrandMode::Placeholder(p)),
            Brand::Var(Var::Infer(b)) => {
                for scope in env.model_scopes.iter().rev() {
                    let mut matching = vec![];
                    for &m in scope {
                        let decl = &self.program.model(m).decl;
                        if decl.trait_ref.trait_id != trait_ref.trait_id {
                            continue;
                        }
                        let mut probe = cx.clone();
                        let args = probe.fresh_args(&decl.params);
                        let head = instantiate_trait_ref(&decl.trait_ref, &args)?;
                        match probe.unify_trait_ref(&head, trait_ref) {
                            Ok(()) | Err(UnifyError::Ambiguous) => matching.push(m),
                            Err(UnifyError::Mismatch) => {}
                            Err(UnifyError::DuplicateLabel(k)) => {
                                return Err(CandErr::Unsupported(
                                    UnsupportedReason::DuplicateEffectLabel(k),
                                ));
                            }
                        }
                    }
                    matching.sort();
                    matching.dedup();
                    match matching.as_slice() {
                        [] => continue,
                        [m] => {
                            cx.unify_brand(&Brand::Var(Var::Infer(b)), &Brand::Named(*m))
                                .map_err(unify_err)?;
                            return Ok(BrandMode::Named(*m));
                        }
                        _ => {
                            return Err(CandErr::Ambiguous(Ambiguity::ConflictingScopedModels(
                                matching,
                            )));
                        }
                    }
                }
                cx.unify_brand(&Brand::Var(Var::Infer(b)), &Brand::Canonical)
                    .map_err(unify_err)?;
                Ok(BrandMode::Canonical)
            }
            Brand::Var(Var::Bound(_)) => Err(CandErr::Unsupported(UnsupportedReason::Internal(
                "bound brand in a goal".into(),
            ))),
        }
    }

    fn implements(
        &mut self,
        env_id: u32,
        env: &Env,
        cx0: &InferCtxt,
        trait_ref: &TraitRef,
        brand: &Brand,
    ) -> Result<Phase, CandErr> {
        if is_unresolved_infer(cx0, &trait_ref.self_ty) {
            return Err(CandErr::Ambiguous(Ambiguity::SelfTypeUnknown));
        }
        let mut cx = cx0.clone();
        let mode = self.brand_mode(env, &mut cx, trait_ref, brand)?;
        let resolved_brand = cx.resolve_brand(brand);
        let goal_of = |cx: &InferCtxt| Goal::Implements {
            trait_ref: trait_ref.clone(),
            brand: cx.resolve_brand(brand),
        };
        match mode {
            BrandMode::Named(m) => {
                let decl = &self.program.model(m).decl;
                if decl.trait_ref.trait_id != trait_ref.trait_id {
                    return Ok(Phase::mismatch(m));
                }
                let r = self.try_conformance(
                    env_id,
                    &cx,
                    decl,
                    ConformanceSource::Model(m),
                    trait_ref,
                    &goal_of,
                );
                Ok(Phase::of(vec![(CandidateSource::Model(m), r)]))
            }
            BrandMode::Placeholder(_) | BrandMode::Canonical => {
                let env_results =
                    self.env_implements(env_id, env, &cx, trait_ref, &resolved_brand, &goal_of);
                if env_results.iter().any(|(_, r)| applies(r))
                    || matches!(mode, BrandMode::Placeholder(_))
                {
                    return Ok(Phase::of(env_results));
                }
                let mut results = env_results;
                for &i in self.program.impls_of(trait_ref.trait_id) {
                    let decl = &self.program.impl_decl(i).decl;
                    let r = self.try_conformance(
                        env_id,
                        &cx,
                        decl,
                        ConformanceSource::Impl(i),
                        trait_ref,
                        &goal_of,
                    );
                    results.push((CandidateSource::Impl(i), r));
                }
                Ok(Phase::of(results))
            }
        }
    }

    /// Environment candidates: assumptions and, for rigid projection self
    /// types, bounds declared on the associated type.
    #[allow(clippy::type_complexity)]
    fn env_implements(
        &mut self,
        env_id: u32,
        env: &Env,
        cx0: &InferCtxt,
        trait_ref: &TraitRef,
        brand: &Brand,
        goal_of: &dyn Fn(&InferCtxt) -> Goal,
    ) -> Vec<(CandidateSource, Result<(InferCtxt, Derivation), CandErr>)> {
        let mut out = vec![];
        for (k, a) in env.assumptions.iter().enumerate() {
            let Goal::Implements {
                trait_ref: atr,
                brand: ab,
            } = a
            else {
                continue;
            };
            if atr.trait_id != trait_ref.trait_id {
                continue;
            }
            let mut cx = cx0.clone();
            let r = cx
                .unify_trait_ref(atr, trait_ref)
                .and_then(|_| cx.unify_brand(ab, brand))
                .map_err(unify_err)
                .map(|_| {
                    (
                        cx.clone(),
                        Derivation::leaf(goal_of(&cx), Step::Assumption { index: k as u32 }),
                    )
                });
            out.push((CandidateSource::Assumption(k as u32), r));
        }
        if let Ty::Projection(p) = &trait_ref.self_ty {
            let t = self.program.trait_decl(p.trait_ref.trait_id);
            let mut binder = vec![GenericArg::Ty(p.trait_ref.self_ty.clone())];
            binder.extend(p.trait_ref.args.iter().cloned());
            for (bi, b) in t.assoc[p.assoc.0 as usize].bounds.iter().enumerate() {
                if b.trait_id != trait_ref.trait_id {
                    continue;
                }
                let r = (|| {
                    self.bump()?;
                    let mut cx = cx0.clone();
                    let args = instantiate_args(&b.args, &binder)?;
                    cx.unify_args(&args, &trait_ref.args).map_err(unify_err)?;
                    cx.unify_brand(&instantiate_brand(&b.brand, &binder), brand)
                        .map_err(unify_err)?;
                    let parent = Goal::Implements {
                        trait_ref: p.trait_ref.clone(),
                        brand: p.brand.clone(),
                    };
                    let children = self.drive(env_id, &mut cx, vec![parent])?;
                    let d = Derivation {
                        goal: goal_of(&cx),
                        step: Step::AliasBound { bound: bi as u32 },
                        children,
                    };
                    Ok((cx, d))
                })();
                out.push((CandidateSource::AliasBound(bi as u32), r));
            }
        }
        out
    }

    fn try_conformance(
        &mut self,
        env_id: u32,
        cx0: &InferCtxt,
        decl: &ConformanceDecl,
        source: ConformanceSource,
        trait_ref: &TraitRef,
        goal_of: &dyn Fn(&InferCtxt) -> Goal,
    ) -> Result<(InferCtxt, Derivation), CandErr> {
        self.bump()?;
        let mut cx = cx0.clone();
        let args = cx.fresh_args(&decl.params);
        let head = instantiate_trait_ref(&decl.trait_ref, &args)?;
        cx.unify_trait_ref(&head, trait_ref).map_err(unify_err)?;
        let wcs = decl
            .where_clauses
            .iter()
            .map(|w| instantiate_goal(w, &args))
            .collect::<Result<Vec<_>, _>>()?;
        let children = self.drive(env_id, &mut cx, wcs)?;
        let d = Derivation {
            goal: goal_of(&cx),
            step: Step::Conformance { source, args },
            children,
        };
        Ok((cx, d))
    }

    fn projection_eq(
        &mut self,
        env_id: u32,
        env: &Env,
        cx0: &InferCtxt,
        projection: &Projection,
        ty: &Ty,
    ) -> Result<Phase, CandErr> {
        let tr = &projection.trait_ref;
        if is_unresolved_infer(cx0, &tr.self_ty) {
            return Err(CandErr::Ambiguous(Ambiguity::SelfTypeUnknown));
        }
        let mut cx = cx0.clone();
        let mode = self.brand_mode(env, &mut cx, tr, &projection.brand)?;
        let goal_of = |cx: &InferCtxt| Goal::ProjectionEq {
            projection: Projection {
                trait_ref: tr.clone(),
                brand: cx.resolve_brand(&projection.brand),
                assoc: projection.assoc,
            },
            ty: ty.clone(),
        };
        let conformance =
            |s: &mut Self, cx: &InferCtxt, decl: &ConformanceDecl, source: ConformanceSource| {
                s.try_projection_conformance(env_id, cx, decl, source, projection, ty, &goal_of)
            };
        match mode {
            BrandMode::Named(m) => {
                let decl = &self.program.model(m).decl;
                if decl.trait_ref.trait_id != tr.trait_id {
                    return Ok(Phase::mismatch(m));
                }
                let r = conformance(self, &cx, decl, ConformanceSource::Model(m));
                Ok(Phase::of(vec![(CandidateSource::Model(m), r)]))
            }
            BrandMode::Placeholder(_) | BrandMode::Canonical => {
                let brand = cx.resolve_brand(&projection.brand);
                // 1. Explicit equality assumptions.
                let mut results = vec![];
                for (k, a) in env.assumptions.iter().enumerate() {
                    let Goal::ProjectionEq {
                        projection: ap,
                        ty: aty,
                    } = a
                    else {
                        continue;
                    };
                    if ap.trait_ref.trait_id != tr.trait_id || ap.assoc != projection.assoc {
                        continue;
                    }
                    let mut c = cx.clone();
                    let r = c
                        .unify_trait_ref(&ap.trait_ref, tr)
                        .and_then(|_| c.unify_brand(&ap.brand, &brand))
                        .map_err(unify_err)
                        .and_then(|_| match c.unify_ty(aty, ty) {
                            Ok(()) => Ok(()),
                            Err(UnifyError::Mismatch) => {
                                Err(CandErr::NoSolution(CandidateCause::ValueMismatch))
                            }
                            Err(e) => Err(unify_err(e)),
                        })
                        .map(|_| {
                            let d = Derivation::leaf(
                                goal_of(&c),
                                Step::ProjectionAssumption { index: k as u32 },
                            );
                            (c, d)
                        });
                    results.push((CandidateSource::Assumption(k as u32), r));
                }
                if results
                    .iter()
                    .any(|(_, r)| applies(r) || matches!(r, Err(CandErr::NoSolution(_))))
                {
                    return Ok(Phase::of(results));
                }
                // 2. The trait holds through the environment: rigid projection.
                let trait_goal_of = |c: &InferCtxt| Goal::Implements {
                    trait_ref: tr.clone(),
                    brand: c.resolve_brand(&brand),
                };
                let env_proofs = self.env_implements(env_id, env, &cx, tr, &brand, &trait_goal_of);
                let env_applies = env_proofs
                    .iter()
                    .any(|(_, r)| !matches!(r, Err(CandErr::HeadMismatch)));
                if env_applies || matches!(mode, BrandMode::Placeholder(_)) {
                    let rigid =
                        matches!(tr.self_ty, Ty::Var(Var::Placeholder(_)) | Ty::Projection(_));
                    let r = if !rigid {
                        Err(CandErr::Unsupported(UnsupportedReason::Internal(
                            "assumption on a non-rigid self type without an equality binding"
                                .into(),
                        )))
                    } else {
                        self.rigid_projection(env_id, &cx, projection, &brand, ty, &goal_of)
                    };
                    return Ok(Phase::of(vec![(CandidateSource::Rigid, r)]));
                }
                // 3. Canonical impls.
                for &i in self.program.impls_of(tr.trait_id) {
                    let decl = &self.program.impl_decl(i).decl;
                    let r = conformance(self, &cx, decl, ConformanceSource::Impl(i));
                    results.push((CandidateSource::Impl(i), r));
                }
                Ok(Phase::of(results))
            }
        }
    }

    fn rigid_projection(
        &mut self,
        env_id: u32,
        cx0: &InferCtxt,
        projection: &Projection,
        brand: &Brand,
        ty: &Ty,
        goal_of: &dyn Fn(&InferCtxt) -> Goal,
    ) -> Result<(InferCtxt, Derivation), CandErr> {
        self.bump()?;
        let mut cx = cx0.clone();
        let p = Projection {
            trait_ref: projection.trait_ref.clone(),
            brand: brand.clone(),
            assoc: projection.assoc,
        };
        let parent = Goal::Implements {
            trait_ref: p.trait_ref.clone(),
            brand: brand.clone(),
        };
        let children = self.drive(env_id, &mut cx, vec![parent])?;
        match cx.unify_ty(&Ty::Projection(Box::new(p)), ty) {
            Ok(()) => {}
            Err(UnifyError::Mismatch) => {
                return Err(CandErr::NoSolution(CandidateCause::ValueMismatch));
            }
            Err(e) => return Err(unify_err(e)),
        }
        Ok((
            cx.clone(),
            Derivation {
                goal: goal_of(&cx),
                step: Step::RigidProjection,
                children,
            },
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn try_projection_conformance(
        &mut self,
        env_id: u32,
        cx0: &InferCtxt,
        decl: &ConformanceDecl,
        source: ConformanceSource,
        projection: &Projection,
        ty: &Ty,
        goal_of: &dyn Fn(&InferCtxt) -> Goal,
    ) -> Result<(InferCtxt, Derivation), CandErr> {
        self.bump()?;
        let mut cx = cx0.clone();
        let args = cx.fresh_args(&decl.params);
        let head = instantiate_trait_ref(&decl.trait_ref, &args)?;
        cx.unify_trait_ref(&head, &projection.trait_ref)
            .map_err(unify_err)?;
        let value = instantiate_ty(&decl.assoc_values[projection.assoc.0 as usize], &args)?;
        let mut norms = vec![];
        let value = normalize_ty(&mut cx, &value, &mut norms)?;
        match cx.unify_ty(&value, ty) {
            Ok(()) => {}
            Err(UnifyError::Mismatch) => {
                return Err(CandErr::NoSolution(CandidateCause::ValueMismatch));
            }
            Err(e) => return Err(unify_err(e)),
        }
        let mut goals = decl
            .where_clauses
            .iter()
            .map(|w| instantiate_goal(w, &args))
            .collect::<Result<Vec<_>, _>>()?;
        let k = norms.len() as u32;
        goals.extend(norms);
        let children = self.drive(env_id, &mut cx, goals)?;
        let d = Derivation {
            goal: goal_of(&cx),
            step: Step::ProjectionConformance {
                source,
                args,
                value_normalizations: k,
            },
            children,
        };
        Ok((cx, d))
    }

    fn mode_bound(
        &mut self,
        env: &Env,
        cx: &InferCtxt,
        goal: &Goal,
        ty: &Ty,
        bound: &ModeTerm,
        vars: &[GenericArg],
    ) -> QResult {
        let mut atoms = vec![];
        let ty = match cx.resolve_ty(ty) {
            Ok(t) => t,
            Err(d) => return QResult::Unsupported(UnsupportedReason::DuplicateEffectLabel(d.0)),
        };
        if let Err(d) = mode_atoms(self.program, env, &ty, &mut atoms) {
            return QResult::Unsupported(UnsupportedReason::DuplicateEffectLabel(d.0));
        }
        let bound = cx.resolve_mode(bound);
        match decide(&atoms, &bound) {
            ModeVerdict::Holds => {
                match make_response(cx, vars, Derivation::leaf(goal.clone(), Step::Mode)) {
                    Ok(r) => QResult::Proven(r),
                    Err(d) => QResult::Unsupported(UnsupportedReason::DuplicateEffectLabel(d.0)),
                }
            }
            ModeVerdict::Fails { atom } => QResult::NoSolution(Failure {
                goal: goal.clone(),
                reason: FailureReason::ModeExceeds { atom, bound },
            }),
            ModeVerdict::Unknown => QResult::Ambiguous(Ambiguity::ModeUnknown),
        }
    }
}

enum BrandMode {
    Canonical,
    Named(ModelId),
    Placeholder(#[allow(dead_code)] u32),
}

type CandResult = Result<(InferCtxt, Derivation), CandErr>;

struct Phase {
    results: Vec<(CandidateSource, CandResult)>,
    model_trait_mismatch: Option<ModelId>,
}

impl Phase {
    fn of(results: Vec<(CandidateSource, CandResult)>) -> Self {
        Phase {
            results,
            model_trait_mismatch: None,
        }
    }
    fn mismatch(m: ModelId) -> Self {
        Phase {
            results: vec![],
            model_trait_mismatch: Some(m),
        }
    }
}

/// A candidate applies when its head matched and it did not definitely fail.
fn applies(r: &CandResult) -> bool {
    !matches!(r, Err(CandErr::HeadMismatch) | Err(CandErr::NoSolution(_)))
}

fn is_unresolved_infer(cx: &InferCtxt, t: &Ty) -> bool {
    matches!(cx.resolve_ty(t), Ok(Ty::Var(Var::Infer(_))))
}

/// Combines candidate results: any unsupported or overflowing candidate makes
/// the goal unsupported/overflowing (its answer could differ); several
/// distinct answers or any remaining ambiguity is ambiguous.
fn combine(
    goal: Goal,
    vars: &[GenericArg],
    results: Vec<(CandidateSource, CandResult)>,
) -> QResult {
    let mut proven: Vec<(CandidateSource, Response)> = vec![];
    let mut ambiguous: Vec<(CandidateSource, Ambiguity)> = vec![];
    let mut failures = vec![];
    for (source, r) in results {
        match r {
            Ok((cx, d)) => match make_response(&cx, vars, d) {
                Ok(resp) => proven.push((source, resp)),
                Err(dl) => {
                    return QResult::Unsupported(UnsupportedReason::DuplicateEffectLabel(dl.0));
                }
            },
            Err(CandErr::HeadMismatch) => {}
            Err(CandErr::NoSolution(cause)) => failures.push(CandidateFailure {
                candidate: source,
                cause,
            }),
            Err(CandErr::Ambiguous(a)) => ambiguous.push((source, a)),
            Err(CandErr::Unsupported(u)) => return QResult::Unsupported(u),
            Err(CandErr::Overflow(o)) => return QResult::Overflow(o),
        }
    }
    let mut distinct: Vec<&(CandidateSource, Response)> = vec![];
    for p in &proven {
        if !distinct
            .iter()
            .any(|q| q.1.binders == p.1.binders && q.1.var_values == p.1.var_values)
        {
            distinct.push(p);
        }
    }
    if distinct.len() > 1 || (!distinct.is_empty() && !ambiguous.is_empty()) {
        let mut sources: Vec<CandidateSource> = proven
            .iter()
            .map(|p| p.0.clone())
            .chain(ambiguous.iter().map(|a| a.0.clone()))
            .collect();
        sources.sort();
        return QResult::Ambiguous(Ambiguity::MultipleCandidates(sources));
    }
    if let Some((_, resp)) = proven.into_iter().next() {
        return QResult::Proven(resp);
    }
    if let Some((_, a)) = ambiguous.into_iter().next() {
        return QResult::Ambiguous(a);
    }
    QResult::NoSolution(Failure {
        goal,
        reason: if failures.is_empty() {
            FailureReason::NoCandidates
        } else {
            FailureReason::CandidatesFailed(failures)
        },
    })
}

/// Builds a canonical response: query variable values first, then the
/// derivation, sharing one renaming of free inference variables.
fn make_response(
    cx: &InferCtxt,
    vars: &[GenericArg],
    d: Derivation,
) -> Result<Response, DuplicateLabel> {
    let mut c = Canonicalizer::new(Some(cx));
    let mut values = vec![];
    for v in vars {
        let r = cx.resolve_arg(v)?;
        values.push(fold_arg(&r, &mut c)?);
    }
    let resolved = d.fold(&mut ResolveIn(cx))?;
    let derivation = resolved.fold(&mut c)?;
    Ok(Response {
        binders: c.binders,
        var_values: values,
        derivation,
    })
}

struct ResolveIn<'a>(&'a InferCtxt);
impl VarFolder for ResolveIn<'_> {
    fn fold_var(&mut self, var: Var, kind: ParamKind) -> GenericArg {
        self.0
            .resolve_arg(&GenericArg::var(kind, var))
            .unwrap_or_else(|_| GenericArg::var(kind, var))
    }
}

struct InferSubst<'a>(&'a [GenericArg]);
impl VarFolder for InferSubst<'_> {
    fn fold_var(&mut self, var: Var, kind: ParamKind) -> GenericArg {
        match var {
            Var::Infer(i) => self.0[i as usize].clone(),
            other => GenericArg::var(kind, other),
        }
    }
}

// -- eager normalization --------------------------------------------------------

/// Replaces every projection (innermost first, left to right) with a fresh
/// inference variable `?v` and records `ProjectionEq(p, ?v)`. For a
/// `ProjectionEq` goal the outermost projection itself is kept.
fn normalize_goal(
    cx: &mut InferCtxt,
    g: &Goal,
    out: &mut Vec<Goal>,
) -> Result<Goal, DuplicateLabel> {
    let g = cx.resolve_goal(g)?;
    Ok(match g {
        Goal::Implements { trait_ref, brand } => Goal::Implements {
            trait_ref: normalize_trait_ref(cx, &trait_ref, out)?,
            brand,
        },
        Goal::ProjectionEq { projection, ty } => Goal::ProjectionEq {
            projection: Projection {
                trait_ref: normalize_trait_ref(cx, &projection.trait_ref, out)?,
                brand: projection.brand,
                assoc: projection.assoc,
            },
            ty: normalize_ty(cx, &ty, out)?,
        },
        Goal::ModeBound { ty, bound } => Goal::ModeBound {
            ty: normalize_ty(cx, &ty, out)?,
            bound,
        },
    })
}

fn normalize_trait_ref(
    cx: &mut InferCtxt,
    t: &TraitRef,
    out: &mut Vec<Goal>,
) -> Result<TraitRef, DuplicateLabel> {
    Ok(TraitRef {
        trait_id: t.trait_id,
        self_ty: normalize_ty(cx, &t.self_ty, out)?,
        args: t
            .args
            .iter()
            .map(|a| normalize_arg(cx, a, out))
            .collect::<Result<_, _>>()?,
    })
}

fn normalize_arg(
    cx: &mut InferCtxt,
    a: &GenericArg,
    out: &mut Vec<Goal>,
) -> Result<GenericArg, DuplicateLabel> {
    Ok(match a {
        GenericArg::Ty(t) => GenericArg::Ty(normalize_ty(cx, t, out)?),
        GenericArg::Row(r) => {
            let mut row = r.clone();
            for l in &mut row.labels {
                if let EffectLabel::Effect { args, .. } = l {
                    *args = args
                        .iter()
                        .map(|a| normalize_arg(cx, a, out))
                        .collect::<Result<_, _>>()?;
                }
            }
            GenericArg::Row(row)
        }
        other => other.clone(),
    })
}

fn normalize_ty(cx: &mut InferCtxt, t: &Ty, out: &mut Vec<Goal>) -> Result<Ty, DuplicateLabel> {
    Ok(match t {
        Ty::Ctor { ctor, args } => Ty::Ctor {
            ctor: *ctor,
            args: args
                .iter()
                .map(|a| normalize_arg(cx, a, out))
                .collect::<Result<_, _>>()?,
        },
        Ty::Var(v) => Ty::Var(*v),
        Ty::Projection(p) => {
            let inner = Projection {
                trait_ref: normalize_trait_ref(cx, &p.trait_ref, out)?,
                brand: p.brand.clone(),
                assoc: p.assoc,
            };
            let v = cx.new_var(ParamKind::Type);
            out.push(Goal::ProjectionEq {
                projection: inner,
                ty: Ty::Var(v),
            });
            Ty::Var(v)
        }
    })
}

// -- mapping results back to the caller's placeholders ------------------------------

fn restore_placeholders(o: Outcome, origin: &[u32]) -> Outcome {
    let mut f = |v: Var, k: ParamKind| match v {
        Var::Placeholder(i) => GenericArg::var(k, Var::Placeholder(origin[i as usize])),
        other => GenericArg::var(k, other),
    };
    let restore_goal =
        |g: &Goal, f: &mut dyn VarFolder| fold_goal(g, f).unwrap_or_else(|_| g.clone());
    match o {
        Outcome::Proven(s) => Outcome::Proven(Solution {
            var_values: s
                .var_values
                .iter()
                .map(|a| fold_arg(a, &mut f).unwrap_or_else(|_| a.clone()))
                .collect(),
            derivation: s.derivation.fold(&mut f).unwrap_or(s.derivation.clone()),
            ..s
        }),
        Outcome::NoSolution(fail) => {
            Outcome::NoSolution(restore_failure(&fail, &mut f, &restore_goal))
        }
        Outcome::Ambiguous(Ambiguity::Stalled(g)) => {
            Outcome::Ambiguous(Ambiguity::Stalled(Box::new(restore_goal(&g, &mut f))))
        }
        Outcome::Unsupported(UnsupportedReason::MixedCycle(g)) => Outcome::Unsupported(
            UnsupportedReason::MixedCycle(Box::new(restore_goal(&g, &mut f))),
        ),
        other => other,
    }
}

fn restore_failure(
    fail: &Failure,
    f: &mut dyn VarFolder,
    rg: &dyn Fn(&Goal, &mut dyn VarFolder) -> Goal,
) -> Failure {
    Failure {
        goal: rg(&fail.goal, f),
        reason: match &fail.reason {
            FailureReason::CandidatesFailed(cs) => FailureReason::CandidatesFailed(
                cs.iter()
                    .map(|c| CandidateFailure {
                        candidate: c.candidate.clone(),
                        cause: match &c.cause {
                            CandidateCause::Nested(inner) => {
                                CandidateCause::Nested(Box::new(restore_failure(inner, f, rg)))
                            }
                            other => other.clone(),
                        },
                    })
                    .collect(),
            ),
            FailureReason::ModeExceeds { atom, bound } => FailureReason::ModeExceeds {
                atom: fold_mode(atom, f),
                bound: fold_mode(bound, f),
            },
            other => other.clone(),
        },
    }
}

// -- rendering ------------------------------------------------------------------

impl fmt::Display for CandidateSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CandidateSource::Impl(i) => write!(f, "{i}"),
            CandidateSource::Model(m) => write!(f, "{m}"),
            CandidateSource::Assumption(k) => write!(f, "assumption {k}"),
            CandidateSource::AliasBound(b) => write!(f, "alias bound {b}"),
            CandidateSource::Rigid => f.write_str("rigid projection"),
        }
    }
}

impl Failure {
    /// Deterministic multi-line explanation.
    pub fn render(&self) -> String {
        let mut s = String::new();
        self.render_into(0, &mut s);
        s
    }

    fn render_into(&self, depth: usize, s: &mut String) {
        use std::fmt::Write;
        let pad = "  ".repeat(depth);
        let _ = match &self.reason {
            FailureReason::NoCandidates => writeln!(
                s,
                "{pad}`{}`: no matching impl, model or assumption",
                self.goal
            ),
            FailureReason::ModeExceeds { atom, bound } => {
                writeln!(s, "{pad}`{}`: mode {atom} is not below {bound}", self.goal)
            }
            FailureReason::ModelTraitMismatch { model } => {
                writeln!(s, "{pad}`{}`: {model} does not model this trait", self.goal)
            }
            FailureReason::InductiveCycle => writeln!(
                s,
                "{pad}`{}`: inductive cycle without a base case",
                self.goal
            ),
            FailureReason::CandidatesFailed(cs) => {
                let _ = writeln!(s, "{pad}`{}`: every candidate failed", self.goal);
                for c in cs {
                    match &c.cause {
                        CandidateCause::Nested(inner) => {
                            let _ = writeln!(s, "{pad}  {} requires", c.candidate);
                            inner.render_into(depth + 2, s);
                        }
                        CandidateCause::ValueMismatch => {
                            let _ = writeln!(s, "{pad}  {}: associated type differs", c.candidate);
                        }
                    }
                }
                Ok(())
            }
        };
    }
}
