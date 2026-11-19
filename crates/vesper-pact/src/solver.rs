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

use crate::derivation::{ConformanceSource, Derivation};
use crate::diag::Diagnostic;
use crate::ids::*;
use crate::program::{Program, QueryDto};
use crate::term::*;
use std::fmt;

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

/// The tabled trait solver over one program.
pub struct Solver<'p> {
    #[cfg(vesper_port)]
    port: crate::port::PortSolver,
    #[cfg(vesper_port)]
    program: std::marker::PhantomData<&'p Program>,
    #[cfg(not(vesper_port))]
    rust: crate::engine::solve::RustSolver<'p>,
}

impl<'p> Solver<'p> {
    pub fn new(program: &'p Program) -> Self {
        Self::with_config(program, SolverConfig::default())
    }

    pub fn with_config(program: &'p Program, config: SolverConfig) -> Self {
        Solver {
            #[cfg(vesper_port)]
            port: crate::port::PortSolver::new(program, config),
            #[cfg(vesper_port)]
            program: std::marker::PhantomData,
            #[cfg(not(vesper_port))]
            rust: crate::engine::solve::RustSolver::new(program, config),
        }
    }

    pub fn stats(&self) -> SolverStats {
        #[cfg(vesper_port)]
        return self.port.stats();
        #[cfg(not(vesper_port))]
        self.rust.stats()
    }

    /// Number of tabled canonical answers.
    pub fn table_len(&self) -> usize {
        #[cfg(vesper_port)]
        return self.port.table_len();
        #[cfg(not(vesper_port))]
        self.rust.table_len()
    }

    /// Solves one query. Invalid queries are rejected with sorted diagnostics.
    pub fn solve(&mut self, query: &QueryDto) -> Result<Outcome, Vec<Diagnostic>> {
        #[cfg(vesper_port)]
        return self.port.solve(query);
        #[cfg(not(vesper_port))]
        self.rust.solve(query)
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
