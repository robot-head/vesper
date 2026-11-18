//! The port-backed build (port wave P3a lane 3, #1038): program validation,
//! solving, coherence, query canonicalization and derivation checking run in
//! the Vesper package `vesper/`. This module encodes the DTOs as words for
//! it and rebuilds the crate's own types from its result words. The types,
//! their `Display` and `Debug` text, the folds and the stand-alone
//! `check_orphan`, `conformance_env` and `validate_query` stay Rust.
//!
//! # Input words
//!
//! A term is its node tag followed by its fields (`vesper/src/program.ves`
//! documents the tags): `1 ctor n arg*`, `2 var` and `3 trait_ref brand
//! assoc` for types, `4 mode` and `5 var` for modes, `6 n label* tail` for
//! rows, `7`, `8 model` and `9 var` for brands, `10 var` for capability
//! arguments, `11 effect n arg*` and `12 var` for labels. A trait ref is
//! `trait self n arg*` and a goal `14 trait_ref brand`, `15 trait_ref brand
//! assoc ty` or `16 ty mode`. A variable is `kind << 32 | index` (0 bound, 1
//! placeholder, 2 inference) and an absent value is `u64::MAX`.
//!
//! A program is `major minor`, then the packages, type constructors,
//! effects, traits, impls and models, each a count and its records; a query
//! is `major minor env binders goal`, an environment `n (kind bound)* n
//! goal* n (n model*)*`. `check_derivation` reads a program, an environment
//! and a derivation `goal step n derivation*`, with the step's node tag
//! (18-26) and fields.
//!
//! # Result words
//!
//! See `vesper/src/out.ves`: outcomes, failures, derivations, environments
//! and diagnostics (whose messages are ranges of the handle's `txt`).
use crate::canon::CanonicalGoal;
use crate::derivation::{ConformanceSource, Derivation, DerivationError, Step};
use crate::diag::{DiagCode, Diagnostic, Subject};
use crate::ids::*;
use crate::port_binding as binding;
use crate::program::{ConformanceDecl, Env, PlaceholderDecl, Program, ProgramDto, QueryDto};
use crate::solver::*;
use crate::term::*;

const NONE: u64 = u64::MAX;

const CODES: [DiagCode; 12] = [
    DiagCode::SchemaVersion,
    DiagCode::IdNotDense,
    DiagCode::UnknownId,
    DiagCode::Arity,
    DiagCode::KindMismatch,
    DiagCode::VarScope,
    DiagCode::Malformed,
    DiagCode::UnconstrainedParam,
    DiagCode::Unsupported,
    DiagCode::OrphanImpl,
    DiagCode::OverlappingImpls,
    DiagCode::ImplNotWellFormed,
];

const KINDS: [ParamKind; 5] = [
    ParamKind::Type,
    ParamKind::Mode,
    ParamKind::Row,
    ParamKind::Brand,
    ParamKind::Capability,
];

const MODES: [Mode; 3] = [Mode::Copy, Mode::Affine, Mode::Linear];

// ---------------------------------------------------------------------------
// Encoding

#[derive(Default)]
struct Enc {
    w: Vec<u64>,
}

fn var_word(v: Var) -> u64 {
    match v {
        Var::Bound(i) => u64::from(i),
        Var::Placeholder(i) => 1 << 32 | u64::from(i),
        Var::Infer(i) => 2 << 32 | u64::from(i),
    }
}

impl Enc {
    fn put(&mut self, x: u64) {
        self.w.push(x);
    }
    fn len(&mut self, n: usize) {
        self.put(n as u64);
    }
    fn kinds(&mut self, ks: &[ParamKind]) {
        self.len(ks.len());
        for &k in ks {
            self.put(k as u64);
        }
    }
    fn args(&mut self, args: &[GenericArg]) {
        self.len(args.len());
        for a in args {
            self.arg(a);
        }
    }
    fn arg(&mut self, a: &GenericArg) {
        match a {
            GenericArg::Ty(t) => self.ty(t),
            GenericArg::Mode(m) => self.mode(m),
            GenericArg::Row(r) => self.row(r),
            GenericArg::Brand(b) => self.brand(b),
            GenericArg::Capability(v) => {
                self.put(10);
                self.put(var_word(*v));
            }
        }
    }
    fn ty(&mut self, t: &Ty) {
        match t {
            Ty::Ctor { ctor, args } => {
                self.put(1);
                self.put(u64::from(ctor.0));
                self.args(args);
            }
            Ty::Var(v) => {
                self.put(2);
                self.put(var_word(*v));
            }
            Ty::Projection(p) => {
                self.put(3);
                self.projection(p);
            }
        }
    }
    fn projection(&mut self, p: &Projection) {
        self.trait_ref(&p.trait_ref);
        self.brand(&p.brand);
        self.put(u64::from(p.assoc.0));
    }
    fn mode(&mut self, m: &ModeTerm) {
        match m {
            ModeTerm::Const(c) => {
                self.put(4);
                self.put(*c as u64);
            }
            ModeTerm::Var(v) => {
                self.put(5);
                self.put(var_word(*v));
            }
        }
    }
    fn row(&mut self, r: &EffectRow) {
        self.put(6);
        self.len(r.labels.len());
        for l in &r.labels {
            match l {
                EffectLabel::Effect { effect, args } => {
                    self.put(11);
                    self.put(u64::from(effect.0));
                    self.args(args);
                }
                EffectLabel::Capability(v) => {
                    self.put(12);
                    self.put(var_word(*v));
                }
            }
        }
        self.put(r.tail.map_or(NONE, var_word));
    }
    fn brand(&mut self, b: &Brand) {
        match b {
            Brand::Canonical => self.put(7),
            Brand::Named(m) => {
                self.put(8);
                self.put(u64::from(m.0));
            }
            Brand::Var(v) => {
                self.put(9);
                self.put(var_word(*v));
            }
        }
    }
    fn trait_ref(&mut self, t: &TraitRef) {
        self.put(u64::from(t.trait_id.0));
        self.ty(&t.self_ty);
        self.args(&t.args);
    }
    fn goal(&mut self, g: &Goal) {
        match g {
            Goal::Implements { trait_ref, brand } => {
                self.put(14);
                self.trait_ref(trait_ref);
                self.brand(brand);
            }
            Goal::ProjectionEq { projection, ty } => {
                self.put(15);
                self.projection(projection);
                self.ty(ty);
            }
            Goal::ModeBound { ty, bound } => {
                self.put(16);
                self.ty(ty);
                self.mode(bound);
            }
        }
    }
    fn conformance(&mut self, d: &ConformanceDecl) {
        self.put(u64::from(d.package.0));
        self.kinds(&d.params);
        self.trait_ref(&d.trait_ref);
        self.len(d.where_clauses.len());
        for g in &d.where_clauses {
            self.goal(g);
        }
        self.len(d.assoc_values.len());
        for t in &d.assoc_values {
            self.ty(t);
        }
    }
    fn program(&mut self, p: &ProgramDto) {
        self.put(u64::from(p.schema.major));
        self.put(u64::from(p.schema.minor));
        self.len(p.packages.len());
        for x in &p.packages {
            self.put(u64::from(x.id.0));
        }
        self.len(p.type_ctors.len());
        for c in &p.type_ctors {
            self.put(u64::from(c.id.0));
            self.put(u64::from(c.package.0));
            self.kinds(&c.params);
            self.put(c.mode.base as u64);
            self.len(c.mode.joins.len());
            for &j in &c.mode.joins {
                self.put(u64::from(j));
            }
        }
        self.len(p.effects.len());
        for e in &p.effects {
            self.put(u64::from(e.id.0));
            self.put(u64::from(e.package.0));
            self.kinds(&e.params);
        }
        self.len(p.traits.len());
        for t in &p.traits {
            self.put(u64::from(t.id.0));
            self.put(u64::from(t.package.0));
            self.kinds(&t.params);
            self.len(t.assoc.len());
            for a in &t.assoc {
                self.mode(&a.mode_bound);
                self.len(a.bounds.len());
                for b in &a.bounds {
                    self.put(u64::from(b.trait_id.0));
                    self.args(&b.args);
                    self.brand(&b.brand);
                }
            }
            self.put(u64::from(t.coinductive));
        }
        self.len(p.impls.len());
        for i in &p.impls {
            self.put(u64::from(i.id.0));
            self.conformance(&i.decl);
        }
        self.len(p.models.len());
        for m in &p.models {
            self.put(u64::from(m.id.0));
            self.conformance(&m.decl);
        }
    }
    fn env(&mut self, e: &Env) {
        self.len(e.placeholders.len());
        for p in &e.placeholders {
            self.put(p.kind as u64);
            self.put(p.mode_bound.map_or(NONE, |m| m as u64));
        }
        self.len(e.assumptions.len());
        for g in &e.assumptions {
            self.goal(g);
        }
        self.len(e.model_scopes.len());
        for s in &e.model_scopes {
            self.len(s.len());
            for m in s {
                self.put(u64::from(m.0));
            }
        }
    }
    fn query(&mut self, q: &QueryDto) {
        self.put(u64::from(q.schema.major));
        self.put(u64::from(q.schema.minor));
        self.env(&q.env);
        self.kinds(&q.binders);
        self.goal(&q.goal);
    }
    fn source(&mut self, s: ConformanceSource) {
        self.put(match s {
            ConformanceSource::Impl(i) => 2 * u64::from(i.0),
            ConformanceSource::Model(m) => 2 * u64::from(m.0) + 1,
        });
    }
    fn derivation(&mut self, d: &Derivation) {
        self.goal(&d.goal);
        match &d.step {
            Step::Conformance { source, args } => {
                self.put(18);
                self.source(*source);
                self.args(args);
            }
            Step::Assumption { index } => {
                self.put(19);
                self.put(u64::from(*index));
            }
            Step::AliasBound { bound } => {
                self.put(20);
                self.put(u64::from(*bound));
            }
            Step::ProjectionConformance {
                source,
                args,
                value_normalizations,
            } => {
                self.put(21);
                self.source(*source);
                self.put(u64::from(*value_normalizations));
                self.args(args);
            }
            Step::ProjectionAssumption { index } => {
                self.put(22);
                self.put(u64::from(*index));
            }
            Step::RigidProjection => self.put(23),
            Step::Mode => self.put(24),
            Step::Normalize => self.put(25),
            Step::CoinductiveHypothesis => self.put(26),
        }
        self.len(d.children.len());
        for c in &d.children {
            self.derivation(c);
        }
    }
}

/// The program's words, as every entry point reads them.
pub(crate) fn program_words(dto: &ProgramDto) -> Vec<u64> {
    let mut e = Enc::default();
    e.program(dto);
    e.w
}

// ---------------------------------------------------------------------------
// Decoding

struct Dec<'a> {
    w: &'a [u64],
    txt: &'a [u8],
    pos: usize,
}

fn u32_of(x: u64) -> u32 {
    x as u32
}

fn var_of(x: u64) -> Var {
    let i = u32_of(x);
    match x >> 32 {
        0 => Var::Bound(i),
        1 => Var::Placeholder(i),
        _ => Var::Infer(i),
    }
}

fn label_key_of(x: u64) -> LabelKey {
    if x >> 34 == 0 {
        LabelKey::Effect(EffectId(u32_of(x)))
    } else {
        LabelKey::Capability(var_of(x & !(1 << 34)))
    }
}

fn source_of(x: u64) -> ConformanceSource {
    if x.is_multiple_of(2) {
        ConformanceSource::Impl(ImplId(u32_of(x / 2)))
    } else {
        ConformanceSource::Model(ModelId(u32_of(x / 2)))
    }
}

fn candidate_of(x: u64) -> CandidateSource {
    let i = u32_of(x);
    match x >> 32 {
        0 => CandidateSource::Impl(ImplId(i)),
        1 => CandidateSource::Model(ModelId(i)),
        2 => CandidateSource::Assumption(i),
        3 => CandidateSource::AliasBound(i),
        _ => CandidateSource::Rigid,
    }
}

fn subject_of(v: u64, a: u64, b: u64) -> Subject {
    let (a, b) = (u32_of(a), u32_of(b));
    match v {
        0 => Subject::Program,
        1 => Subject::TypeCtor(TypeCtorId(a)),
        2 => Subject::Effect(EffectId(a)),
        3 => Subject::Trait(TraitId(a)),
        4 => Subject::Impl(ImplId(a)),
        5 => Subject::ImplPair(ImplId(a), ImplId(b)),
        6 => Subject::Model(ModelId(a)),
        _ => Subject::Query,
    }
}

impl<'a> Dec<'a> {
    fn new(w: &'a [u64], txt: &'a [u8]) -> Self {
        Dec { w, txt, pos: 0 }
    }
    fn next(&mut self) -> u64 {
        let x = self.w[self.pos];
        self.pos += 1;
        x
    }
    fn count(&mut self) -> usize {
        self.next() as usize
    }
    fn text(&self, off: u64, len: u64) -> String {
        let (off, len) = (off as usize, len as usize);
        String::from_utf8_lossy(&self.txt[off..off + len]).into_owned()
    }
    fn kinds(&mut self) -> Vec<ParamKind> {
        (0..self.count())
            .map(|_| KINDS[self.next() as usize])
            .collect()
    }
    fn args(&mut self) -> Vec<GenericArg> {
        (0..self.count()).map(|_| self.arg()).collect()
    }
    fn arg(&mut self) -> GenericArg {
        match self.w[self.pos] {
            1..=3 => GenericArg::Ty(self.ty()),
            4 | 5 => GenericArg::Mode(self.mode()),
            6 => GenericArg::Row(self.row()),
            7..=9 => GenericArg::Brand(self.brand()),
            _ => {
                self.pos += 1;
                GenericArg::Capability(var_of(self.next()))
            }
        }
    }
    fn ty(&mut self) -> Ty {
        match self.next() {
            1 => {
                let ctor = TypeCtorId(u32_of(self.next()));
                Ty::Ctor {
                    ctor,
                    args: self.args(),
                }
            }
            2 => Ty::Var(var_of(self.next())),
            _ => Ty::Projection(Box::new(self.projection())),
        }
    }
    fn projection(&mut self) -> Projection {
        let trait_ref = self.trait_ref();
        let brand = self.brand();
        Projection {
            trait_ref,
            brand,
            assoc: AssocIdx(u32_of(self.next())),
        }
    }
    fn mode(&mut self) -> ModeTerm {
        match self.next() {
            4 => ModeTerm::Const(MODES[self.next() as usize]),
            _ => ModeTerm::Var(var_of(self.next())),
        }
    }
    fn row(&mut self) -> EffectRow {
        self.pos += 1;
        let labels = (0..self.count())
            .map(|_| match self.next() {
                11 => {
                    let effect = EffectId(u32_of(self.next()));
                    EffectLabel::Effect {
                        effect,
                        args: self.args(),
                    }
                }
                _ => EffectLabel::Capability(var_of(self.next())),
            })
            .collect();
        let tail = match self.next() {
            NONE => None,
            v => Some(var_of(v)),
        };
        EffectRow { labels, tail }
    }
    fn brand(&mut self) -> Brand {
        match self.next() {
            7 => Brand::Canonical,
            8 => Brand::Named(ModelId(u32_of(self.next()))),
            _ => Brand::Var(var_of(self.next())),
        }
    }
    fn trait_ref(&mut self) -> TraitRef {
        let trait_id = TraitId(u32_of(self.next()));
        let self_ty = self.ty();
        TraitRef {
            trait_id,
            self_ty,
            args: self.args(),
        }
    }
    fn goal(&mut self) -> Goal {
        match self.next() {
            14 => {
                let trait_ref = self.trait_ref();
                Goal::Implements {
                    trait_ref,
                    brand: self.brand(),
                }
            }
            15 => {
                let projection = self.projection();
                Goal::ProjectionEq {
                    projection,
                    ty: self.ty(),
                }
            }
            _ => {
                let ty = self.ty();
                Goal::ModeBound {
                    ty,
                    bound: self.mode(),
                }
            }
        }
    }
    fn derivation(&mut self) -> Derivation {
        let goal = self.goal();
        let step = match self.next() {
            0 => {
                let source = source_of(self.next());
                Step::Conformance {
                    source,
                    args: self.args(),
                }
            }
            1 => Step::Assumption {
                index: u32_of(self.next()),
            },
            2 => Step::AliasBound {
                bound: u32_of(self.next()),
            },
            3 => {
                let source = source_of(self.next());
                let value_normalizations = u32_of(self.next());
                Step::ProjectionConformance {
                    source,
                    args: self.args(),
                    value_normalizations,
                }
            }
            4 => Step::ProjectionAssumption {
                index: u32_of(self.next()),
            },
            5 => Step::RigidProjection,
            6 => Step::Mode,
            7 => Step::Normalize,
            _ => Step::CoinductiveHypothesis,
        };
        let children = (0..self.count()).map(|_| self.derivation()).collect();
        Derivation {
            goal,
            step,
            children,
        }
    }
    fn failure(&mut self) -> Failure {
        let goal = self.goal();
        let reason = match self.next() {
            0 => FailureReason::NoCandidates,
            1 => FailureReason::CandidatesFailed(
                (0..self.count())
                    .map(|_| {
                        let candidate = candidate_of(self.next());
                        let cause = match self.next() {
                            0 => CandidateCause::Nested(Box::new(self.failure())),
                            _ => CandidateCause::ValueMismatch,
                        };
                        CandidateFailure { candidate, cause }
                    })
                    .collect(),
            ),
            2 => {
                let atom = self.mode();
                FailureReason::ModeExceeds {
                    atom,
                    bound: self.mode(),
                }
            }
            3 => FailureReason::ModelTraitMismatch {
                model: ModelId(u32_of(self.next())),
            },
            _ => FailureReason::InductiveCycle,
        };
        Failure { goal, reason }
    }
    fn outcome(&mut self) -> Outcome {
        match self.next() {
            0 => {
                let binders = self.kinds();
                let var_values = self.args();
                let derivation = self.derivation();
                let proof_kind = if self.next() == 1 {
                    ProofKind::Coinductive
                } else {
                    ProofKind::Finite
                };
                Outcome::Proven(Solution {
                    binders,
                    var_values,
                    derivation,
                    proof_kind,
                })
            }
            1 => Outcome::Ambiguous(match self.next() {
                0 => Ambiguity::SelfTypeUnknown,
                1 => Ambiguity::MultipleCandidates(
                    (0..self.count())
                        .map(|_| candidate_of(self.next()))
                        .collect(),
                ),
                2 => Ambiguity::ConflictingScopedModels(
                    (0..self.count())
                        .map(|_| ModelId(u32_of(self.next())))
                        .collect(),
                ),
                3 => Ambiguity::ModeUnknown,
                4 => Ambiguity::NonInjectiveProjection,
                _ => Ambiguity::Stalled(Box::new(self.goal())),
            }),
            2 => Outcome::NoSolution(self.failure()),
            3 => Outcome::Unsupported(match self.next() {
                0 => UnsupportedReason::DuplicateEffectLabel(label_key_of(self.next())),
                1 => UnsupportedReason::MixedCycle(Box::new(self.goal())),
                _ => {
                    let code = self.next();
                    let goal = (self.next() == 1).then(|| self.goal());
                    UnsupportedReason::Internal(match (code, goal) {
                        (0, Some(g)) => format!("answer for `{g}` does not apply to its query"),
                        (1, _) => "bound brand in a goal".into(),
                        _ => {
                            "assumption on a non-rigid self type without an equality binding".into()
                        }
                    })
                }
            }),
            _ => Outcome::Overflow(match self.next() {
                0 => OverflowKind::Depth,
                1 => OverflowKind::Steps,
                _ => OverflowKind::FixpointIterations,
            }),
        }
    }
    fn diagnostics(&mut self) -> Vec<Diagnostic> {
        (0..self.count())
            .map(|_| {
                let code = CODES[self.next() as usize];
                let (v, a, b) = (self.next(), self.next(), self.next());
                let (off, len) = (self.next(), self.next());
                Diagnostic::new(code, subject_of(v, a, b), self.text(off, len))
            })
            .collect()
    }
    fn env(&mut self) -> Env {
        let placeholders = (0..self.count())
            .map(|_| {
                let kind = KINDS[self.next() as usize];
                let mode_bound = match self.next() {
                    NONE => None,
                    m => Some(MODES[m as usize]),
                };
                PlaceholderDecl { kind, mode_bound }
            })
            .collect();
        let assumptions = (0..self.count()).map(|_| self.goal()).collect();
        let model_scopes = (0..self.count())
            .map(|_| {
                (0..self.count())
                    .map(|_| ModelId(u32_of(self.next())))
                    .collect()
            })
            .collect();
        Env {
            placeholders,
            assumptions,
            model_scopes,
        }
    }
}

fn read<T>(s: &binding::S, f: impl FnOnce(&mut Dec<'_>) -> T) -> T {
    let r = s.as_ref();
    let out = r.out();
    let txt = r.txt();
    let mut d = Dec::new(out.as_slice(), txt.as_slice());
    f(&mut d)
}

// ---------------------------------------------------------------------------
// Entry points

/// `Program::new`: the sorted diagnostics (empty when valid).
pub(crate) fn validate_program(words: &[u64]) -> Vec<Diagnostic> {
    let s = binding::program_check(words);
    read(&s, |d| d.diagnostics())
}

/// `check_coherence`.
pub(crate) fn check_coherence(program: &Program) -> Vec<Diagnostic> {
    let s = binding::coherence(program.port_words());
    read(&s, |d| d.diagnostics())
}

/// The solver state of a `Solver`: the program, its table and its stack.
pub(crate) struct PortSolver {
    s: binding::S,
}

impl PortSolver {
    pub(crate) fn new(program: &Program, config: SolverConfig) -> Self {
        let s = binding::solver_new(
            program.port_words(),
            config.max_depth as u64,
            config.max_steps,
            u64::from(config.max_fixpoint_iterations),
        );
        PortSolver { s }
    }

    pub(crate) fn solve(&mut self, query: &QueryDto) -> Result<Outcome, Vec<Diagnostic>> {
        let mut e = Enc::default();
        e.query(query);
        self.s.solve(&e.w);
        read(&self.s, |d| match d.next() {
            0 => Ok(d.outcome()),
            1 => Err(d.diagnostics()),
            _ => panic!("query names a placeholder its environment does not declare"),
        })
    }

    pub(crate) fn stats(&self) -> SolverStats {
        let r = self.s.as_ref();
        SolverStats {
            evaluations: r.st_evals(),
            cache_hits: r.st_hits(),
            coinductive_cycles: r.st_cocyc(),
            inductive_cycles: r.st_indcyc(),
            fixpoint_reruns: r.st_reruns(),
        }
    }

    pub(crate) fn table_len(&self) -> usize {
        self.s.as_ref().ck_count() as usize
    }
}

/// `canonicalize_query`.
pub(crate) fn canonicalize_query(query: &QueryDto) -> Result<(Env, CanonicalGoal), DuplicateLabel> {
    let mut e = Enc::default();
    e.query(query);
    let s = binding::canonicalize(&e.w);
    read(&s, |d| match d.next() {
        0 => {
            let env = d.env();
            let binders = d.kinds();
            let goal = d.goal();
            Ok((env, CanonicalGoal { binders, goal }))
        }
        1 => Err(DuplicateLabel(label_key_of(d.next()))),
        _ => panic!("query names a placeholder its environment does not declare"),
    })
}

/// `check_derivation`.
pub(crate) fn check_derivation(
    program: &Program,
    env: &Env,
    derivation: &Derivation,
) -> Result<(), DerivationError> {
    let mut e = Enc {
        w: program.port_words().to_vec(),
    };
    e.env(env);
    e.derivation(derivation);
    let s = binding::derivation(&e.w);
    read(&s, |d| match d.next() {
        0 => Ok(()),
        _ => {
            let path = (0..d.count()).map(|_| d.next() as usize).collect();
            let off = d.next() as usize;
            let message = String::from_utf8_lossy(&d.txt[off..]).into_owned();
            Err(DerivationError { path, message })
        }
    })
}

/// Live sequence and box allocations of the port (for leak checks).
pub fn live_allocations() -> i64 {
    binding::live_allocations()
}
