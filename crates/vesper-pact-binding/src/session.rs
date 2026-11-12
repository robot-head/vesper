//! Coherence-gated bridge sessions, queries and evidence consumption.

use crate::binding::*;
use crate::error::{BridgeDiagnostic, BridgeError, ErrorKind};
use crate::identity::{DefPath, PackageIdentity};
use crate::lower::{
    Cx, Input, Position, R, fail_error, lower, pact_kind, resolve_model, well_formed_goal,
};
use crate::modes::{from_pact, to_pact};
use crate::source::{SourcePackage, SourceWorld};
use crate::tables::IdTables;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::sync::Arc;
use vesper_core::ast::Mode;
use vesper_pact::{
    Ambiguity, ConformanceSource, Derivation, Env, Failure, GenericArg, Goal, OverflowKind,
    ParamKind, PlaceholderDecl, Program, ProgramDto, ProofKind, QueryDto, Solution, Solver,
    SolverConfig, Step, Ty, UnsupportedReason, Var, VarFolder, check_coherence, check_derivation,
    fold_goal, validate_query, visit_goal_vars,
};

/// Content identity of one declaration revision: SHA-256 of the lowered
/// `ProgramDto` together with the external identity of every table entry.
/// It identifies a declaration set; it is not provenance.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Revision(pub String);

impl fmt::Display for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Collects checked packages and their declaration bindings.
#[derive(Clone, Debug)]
pub struct SessionBuilder {
    prelude: Arc<dyn SourceWorld>,
    packages: Vec<(SourcePackage, BindingsDto)>,
    config: Option<SolverConfig>,
}

/// A lowered declaration set whose DTO shape may still be invalid and whose
/// coherence has NOT been checked. It offers no query or evidence API; only
/// [`SessionBuilder::build`] produces a session that can answer queries.
#[derive(Clone, Debug)]
pub struct LoweredProgram {
    pub dto: ProgramDto,
    pub tables: IdTables,
}

impl SessionBuilder {
    /// `prelude` supplies the checker's builtin ADTs and effects, which
    /// belong to the builtin package.
    pub fn new(prelude: Arc<dyn SourceWorld>) -> Self {
        SessionBuilder {
            prelude,
            packages: vec![],
            config: None,
        }
    }

    pub fn package(mut self, package: SourcePackage, bindings: BindingsDto) -> Self {
        self.packages.push((package, bindings));
        self
    }

    /// Solver budgets. Raising `max_depth` also needs a larger thread stack
    /// (see the Pact report).
    pub fn solver_config(mut self, config: SolverConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Lowers sources and bindings to Pact without validation or coherence.
    pub fn lower(&self) -> Result<LoweredProgram, BridgeError> {
        let inputs: Vec<Input<'_>> = self
            .packages
            .iter()
            .map(|(package, bindings)| Input { package, bindings })
            .collect();
        let l = lower(self.prelude.as_ref(), &inputs)?;
        Ok(LoweredProgram {
            dto: l.dto,
            tables: l.tables,
        })
    }

    /// Lowers, validates (`Program::new`) and runs `check_coherence`. Any
    /// diagnostic blocks the gate: an incoherent declaration set never yields
    /// a session, even where the solver would report `Proven`.
    pub fn build(&self) -> Result<CoherentSession, BridgeError> {
        let LoweredProgram { dto, tables } = self.lower()?;
        let mut fingerprints: Vec<(&PackageIdentity, &str)> = self
            .packages
            .iter()
            .filter_map(|(p, _)| p.fingerprint().map(|f| (p.identity(), f)))
            .collect();
        fingerprints.sort();
        let revision = revision_of(&dto, &tables, &fingerprints);
        let program =
            Program::new(dto).map_err(|d| pact_error(&tables, ErrorKind::Incoherent, d))?;
        let coherence = check_coherence(&program);
        if !coherence.is_empty() {
            return Err(pact_error(&tables, ErrorKind::Incoherent, coherence));
        }
        Ok(CoherentSession {
            program,
            tables,
            revision,
            config: self.config.unwrap_or_default(),
        })
    }
}

type TypeMeta<'a> = (Vec<(&'a str, ParamKind, Mode)>, Vec<(&'a str, u32)>);

fn revision_of(
    dto: &ProgramDto,
    tables: &IdTables,
    fingerprints: &[(&PackageIdentity, &str)],
) -> Revision {
    let mut paths: Vec<&DefPath> = vec![];
    paths.extend(tables.types.iter().map(|t| &t.path));
    paths.extend(tables.effects.iter().map(|t| &t.path));
    paths.extend(tables.traits.iter().map(|t| &t.path));
    paths.extend(tables.impls.iter().map(|t| &t.path));
    paths.extend(tables.models.iter().map(|t| &t.path));
    // Bridge-only constraints (source parameter bounds, brand traits and
    // effect bounds) are not in the ProgramDto but gate evidence, so they
    // are part of the revision too.
    let type_meta: Vec<TypeMeta<'_>> = tables
        .types
        .iter()
        .map(|t| {
            (
                t.params
                    .iter()
                    .map(|p| (p.name.as_str(), p.kind, p.bound))
                    .collect(),
                t.brands.iter().map(|(n, tr)| (n.as_str(), tr.0)).collect(),
            )
        })
        .collect();
    let effect_meta: Vec<&[Mode]> = tables.effects.iter().map(|e| e.bounds.as_slice()).collect();
    let mut h = Sha256::new();
    h.update(b"vesper-pact-bridge/revision/v2\0");
    h.update(
        serde_json::to_vec(&(dto, &tables.packages, paths, type_meta, effect_meta))
            .expect("DTOs serialize"),
    );
    // Source declaration fingerprints (when supplied) bind evidence to the
    // declarations it was derived from, beyond their Pact shapes.
    if !fingerprints.is_empty() {
        h.update(b"\0fingerprints\0");
        h.update(serde_json::to_vec(fingerprints).expect("fingerprints serialize"));
    }
    Revision(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn pact_error(
    tables: &IdTables,
    kind: ErrorKind,
    diags: Vec<vesper_pact::Diagnostic>,
) -> BridgeError {
    BridgeError::new(
        kind,
        diags
            .into_iter()
            .map(|d| BridgeDiagnostic {
                code: d.code.as_str().into(),
                subject: subject_path(tables, d.subject),
                span: None,
                message: tables.rename_ids(&d.message),
            })
            .collect(),
    )
}

fn subject_path(tables: &IdTables, s: vesper_pact::Subject) -> Option<DefPath> {
    use vesper_pact::Subject;
    match s {
        Subject::TypeCtor(i) => tables.types.get(i.index()).map(|e| e.path.clone()),
        Subject::Effect(i) => tables.effects.get(i.index()).map(|e| e.path.clone()),
        Subject::Trait(i) => tables.traits.get(i.index()).map(|e| e.path.clone()),
        Subject::Impl(i) | Subject::ImplPair(i, _) => tables.impl_path(i).cloned(),
        Subject::Model(i) => tables.model_path(i).cloned(),
        Subject::Program | Subject::Query => None,
    }
}

/// A validated, coherent declaration set. Only [`SessionBuilder::build`]
/// creates one; sessions are immutable, so a declaration change means
/// building a new session (with new tables and a new [`Revision`]).
#[derive(Debug)]
pub struct CoherentSession {
    program: Program,
    tables: IdTables,
    revision: Revision,
    config: SolverConfig,
}

/// A query translated to Pact, with the names of its variables.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslatedQuery {
    query: QueryDto,
    placeholder_names: Vec<String>,
    binder_names: Vec<String>,
}

impl TranslatedQuery {
    pub fn query(&self) -> &QueryDto {
        &self.query
    }
    pub fn placeholder_names(&self) -> &[String] {
        &self.placeholder_names
    }
    /// Names of the query binders; implicit brands are named `$brandN`.
    pub fn binder_names(&self) -> &[String] {
        &self.binder_names
    }
}

/// The five solver outcomes. Only `Proven` carries accepted evidence, and
/// only after the derivation re-checked against this session.
#[derive(Clone, Debug)]
pub enum BridgeOutcome {
    Proven(Box<Evidence>),
    Ambiguous {
        ambiguity: Ambiguity,
        explanation: String,
    },
    NoSolution {
        failure: Failure,
        explanation: String,
    },
    /// `pact` is `None` when the bridge itself rejected a solver proof (a
    /// solution that violates a declared parameter bound).
    Unsupported {
        pact: Option<UnsupportedReason>,
        explanation: String,
    },
    Overflow {
        kind: OverflowKind,
    },
}

impl BridgeOutcome {
    pub fn verdict(&self) -> &'static str {
        match self {
            BridgeOutcome::Proven(_) => "proven",
            BridgeOutcome::Ambiguous { .. } => "ambiguous",
            BridgeOutcome::NoSolution { .. } => "no-solution",
            BridgeOutcome::Unsupported { .. } => "unsupported",
            BridgeOutcome::Overflow { .. } => "overflow",
        }
    }
    pub fn evidence(&self) -> Option<&Evidence> {
        match self {
            BridgeOutcome::Proven(e) => Some(e.as_ref()),
            _ => None,
        }
    }
}

/// Accepted, re-checked evidence. Only a session creates it; its fields are
/// private so callers cannot forge accepted evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evidence {
    revision: Revision,
    translated: TranslatedQuery,
    binders: Vec<ParamKind>,
    var_values: Vec<GenericArg>,
    derivation: Derivation,
    proof_kind: ProofKind,
}

impl Evidence {
    pub fn revision(&self) -> &Revision {
        &self.revision
    }
    pub fn query(&self) -> &QueryDto {
        &self.translated.query
    }
    pub fn var_values(&self) -> &[GenericArg] {
        &self.var_values
    }
    pub fn derivation(&self) -> &Derivation {
        &self.derivation
    }
    pub fn proof_kind(&self) -> ProofKind {
        self.proof_kind
    }
    /// Public, serializable certificate form.
    pub fn certificate(&self) -> Certificate {
        Certificate {
            schema: BINDING_SCHEMA,
            revision: self.revision.clone(),
            query: self.translated.query.clone(),
            placeholder_names: self.translated.placeholder_names.clone(),
            binder_names: self.translated.binder_names.clone(),
            binders: self.binders.clone(),
            var_values: self.var_values.clone(),
            derivation: self.derivation.clone(),
        }
    }
}

/// Untrusted serialized evidence. [`CoherentSession::accept_certificate`]
/// re-validates every part before turning it into [`Evidence`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Certificate {
    pub schema: BindingSchema,
    pub revision: Revision,
    pub query: QueryDto,
    pub placeholder_names: Vec<String>,
    pub binder_names: Vec<String>,
    /// Kinds of the solution's remaining free variables.
    pub binders: Vec<ParamKind>,
    pub var_values: Vec<GenericArg>,
    pub derivation: Derivation,
}

/// What consumed evidence selected for the root goal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    Impl { path: DefPath, args: Vec<String> },
    Model { path: DefPath, args: Vec<String> },
    Assumption(u32),
    AliasBound(u32),
    Rigid,
    Mode,
    Coinductive,
}

impl fmt::Display for Selection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let with_args = |f: &mut fmt::Formatter<'_>, p: &DefPath, args: &[String]| {
            write!(f, "{p}")?;
            if !args.is_empty() {
                write!(f, "[{}]", args.join(", "))?;
            }
            Ok(())
        };
        match self {
            Selection::Impl { path, args } | Selection::Model { path, args } => {
                with_args(f, path, args)
            }
            Selection::Assumption(i) => write!(f, "assumption {i}"),
            Selection::AliasBound(i) => write!(f, "alias bound {i}"),
            Selection::Rigid => f.write_str("rigid projection"),
            Selection::Mode => f.write_str("modal kinding"),
            Selection::Coinductive => f.write_str("coinductive hypothesis"),
        }
    }
}

/// The result of consuming evidence: the elaboration-relevant facts with
/// source-facing names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Consumed {
    pub selection: Selection,
    /// Query variable name and rendered value, in binder order.
    pub substitution: Vec<(String, String)>,
    /// The derivation rendered with external names.
    pub explanation: String,
    pub proof_kind: ProofKind,
}

impl CoherentSession {
    pub fn tables(&self) -> &IdTables {
        &self.tables
    }
    pub fn revision(&self) -> &Revision {
        &self.revision
    }
    pub fn program_dto(&self) -> &ProgramDto {
        self.program.dto()
    }

    /// A solving engine that shares its answer table across queries.
    pub fn engine(&self) -> Engine<'_> {
        Engine {
            session: self,
            solver: Solver::with_config(&self.program, self.config),
        }
    }

    /// Solves one query with a fresh table.
    pub fn solve(&self, q: &QueryBindingDto) -> Result<BridgeOutcome, BridgeError> {
        self.engine().solve(q)
    }

    /// Translates a query binding to a validated Pact query.
    pub fn translate_query(&self, q: &QueryBindingDto) -> Result<TranslatedQuery, BridgeError> {
        if !BINDING_SCHEMA.accepts(q.schema) {
            return Err(BridgeError::single(
                ErrorKind::InvalidQuery,
                None,
                format!(
                    "query schema {}.{} is not accepted",
                    q.schema.major, q.schema.minor
                ),
            ));
        }
        if q.package.is_builtin() || self.tables.package_id(&q.package).is_none() {
            return Err(BridgeError::single(
                ErrorKind::InvalidQuery,
                None,
                format!("query site {} is not a package of this session", q.package),
            ));
        }
        self.translate_inner(q)
            .map_err(|f| fail_error(ErrorKind::InvalidQuery, None, f))
            .and_then(|t| {
                validate_query(&self.program, &t.query)
                    .map_err(|d| pact_error(&self.tables, ErrorKind::InvalidQuery, d))?;
                Ok(t)
            })
    }

    fn translate_inner(&self, q: &QueryBindingDto) -> R<TranslatedQuery> {
        let mut cx = Cx::new(&self.tables, q.package.clone(), Position::Env);
        let mut placeholders = vec![];
        for (i, p) in q.env.params.iter().enumerate() {
            cx.declare(p, Var::Placeholder(i as u32))?;
            placeholders.push(PlaceholderDecl {
                kind: pact_kind(p.kind),
                mode_bound: p.bound.map(to_pact),
            });
        }
        let mut assumptions = vec![];
        for a in &q.env.assumptions {
            if matches!(a, GoalDto::ModeBound { .. }) {
                return Err(crate::lower::Fail::Invalid(
                    "mode assumptions are expressed as parameter bounds".into(),
                ));
            }
            assumptions.push(cx.goal(a)?);
        }
        let mut model_scopes = vec![];
        for scope in &q.env.use_models {
            let mut s = vec![];
            for m in scope {
                s.push(resolve_model(&self.tables, &q.package, m)?);
            }
            model_scopes.push(s);
        }
        let mut binder_names = vec![];
        for (i, p) in q.infer.iter().enumerate() {
            if p.kind == ParamKindDto::Capability {
                return Err(crate::lower::Fail::Unsupported(
                    "capability inference variables are outside Pact's fragment".into(),
                ));
            }
            if p.bound.is_some() {
                return Err(crate::lower::Fail::Invalid(format!(
                    "inference variable `{}` cannot carry a bound; add a mode-bound goal",
                    p.name
                )));
            }
            cx.declare(p, Var::Infer(i as u32))?;
            cx.binders.push(pact_kind(p.kind));
            binder_names.push(p.name.clone());
        }
        cx.position = Position::Goal;
        let goal = cx.goal(&q.goal)?;
        for i in binder_names.len()..cx.binders.len() {
            binder_names.push(format!("$brand{}", i - q.infer.len()));
        }
        let mut used = vec![false; cx.binders.len()];
        visit_goal_vars(&goal, |v, _| {
            if let Var::Infer(i) = v
                && let Some(u) = used.get_mut(i as usize)
            {
                *u = true;
            }
        });
        if let Some(i) = used.iter().position(|u| !u) {
            return Err(crate::lower::Fail::Invalid(format!(
                "inference variable `{}` does not occur in the goal",
                binder_names[i]
            )));
        }
        let env = Env {
            placeholders,
            assumptions,
            model_scopes,
        };
        Ok(TranslatedQuery {
            query: QueryDto::new(env, cx.binders, goal),
            placeholder_names: q.env.params.iter().map(|p| p.name.clone()).collect(),
            binder_names,
        })
    }

    /// Re-checks evidence against this session before use. Evidence from a
    /// different declaration revision is rejected as stale.
    pub fn consume(&self, e: &Evidence) -> Result<Consumed, BridgeError> {
        self.verify(
            &e.revision,
            &e.translated.query,
            &e.binders,
            &e.var_values,
            &e.derivation,
        )?;
        Ok(Consumed {
            selection: self.selection(&e.derivation, &e.translated),
            substitution: e
                .translated
                .binder_names
                .iter()
                .zip(&e.var_values)
                .map(|(n, v)| (n.clone(), self.render(&e.translated, &v.to_string())))
                .collect(),
            explanation: self.render(&e.translated, &e.derivation.render()),
            proof_kind: e.proof_kind,
        })
    }

    /// Validates an untrusted certificate completely and returns evidence.
    /// Malformed certificates produce diagnostics, never panics.
    pub fn accept_certificate(&self, c: &Certificate) -> Result<Evidence, BridgeError> {
        if !BINDING_SCHEMA.accepts(c.schema) {
            return Err(BridgeError::single(
                ErrorKind::InvalidEvidence,
                None,
                "certificate schema is not accepted".into(),
            ));
        }
        if c.placeholder_names.len() != c.query.env.placeholders.len()
            || c.binder_names.len() != c.query.binders.len()
        {
            return Err(BridgeError::single(
                ErrorKind::InvalidEvidence,
                None,
                "certificate variable names do not match the query".into(),
            ));
        }
        self.verify(
            &c.revision,
            &c.query,
            &c.binders,
            &c.var_values,
            &c.derivation,
        )?;
        Ok(Evidence {
            revision: c.revision.clone(),
            translated: TranslatedQuery {
                query: c.query.clone(),
                placeholder_names: c.placeholder_names.clone(),
                binder_names: c.binder_names.clone(),
            },
            binders: c.binders.clone(),
            var_values: c.var_values.clone(),
            proof_kind: if c.derivation.uses_coinduction() {
                ProofKind::Coinductive
            } else {
                ProofKind::Finite
            },
            derivation: c.derivation.clone(),
        })
    }

    fn verify(
        &self,
        revision: &Revision,
        query: &QueryDto,
        binders: &[ParamKind],
        var_values: &[GenericArg],
        derivation: &Derivation,
    ) -> Result<(), BridgeError> {
        let bad = |m: String| BridgeError::single(ErrorKind::InvalidEvidence, None, m);
        if revision != &self.revision {
            return Err(BridgeError::single(
                ErrorKind::StaleEvidence,
                None,
                format!(
                    "evidence was produced for revision {revision}, this session is {}",
                    self.revision
                ),
            ));
        }
        validate_query(&self.program, query)
            .map_err(|d| pact_error(&self.tables, ErrorKind::InvalidEvidence, d))?;
        // Every query binder occurs in the goal, so substituting validates
        // every value (below) through the substituted goal.
        let mut used = vec![false; query.binders.len()];
        visit_goal_vars(&query.goal, |v, _| {
            if let Var::Infer(i) = v
                && let Some(u) = used.get_mut(i as usize)
            {
                *u = true;
            }
        });
        if used.iter().any(|u| !u) {
            return Err(bad("a query binder does not occur in the goal".into()));
        }
        if var_values.len() != query.binders.len()
            || var_values
                .iter()
                .zip(&query.binders)
                .any(|(v, k)| v.kind() != *k)
        {
            return Err(bad("solution values do not match the query binders".into()));
        }
        let substituted = fold_goal(&query.goal, &mut InferSubst(var_values))
            .map_err(|_| bad("substituted goal repeats an effect label".into()))?;
        validate_query(
            &self.program,
            &QueryDto::new(query.env.clone(), binders.to_vec(), substituted.clone()),
        )
        .map_err(|d| pact_error(&self.tables, ErrorKind::InvalidEvidence, d))?;
        if substituted != derivation.goal {
            return Err(bad(
                "the derivation does not prove the query goal under the solution".into(),
            ));
        }
        check_derivation(&self.program, &query.env, derivation)
            .map_err(|e| bad(self.tables.rename_ids(&e.to_string())))?;
        let env = &query.env;
        well_formed_goal(&self.tables, &substituted, &|v| match v {
            Var::Placeholder(i) => env
                .placeholders
                .get(i as usize)
                .map(|p| from_pact(p.mode_bound.unwrap_or(vesper_pact::Mode::Linear))),
            _ => None,
        })
        .map_err(|m| {
            BridgeError::single(
                ErrorKind::Unsupported,
                None,
                format!(
                    "solution is not a well-formed instantiation: {}",
                    self.tables.rename_ids(&m)
                ),
            )
        })?;
        Ok(())
    }

    fn selection(&self, d: &Derivation, t: &TranslatedQuery) -> Selection {
        let mut d = d;
        while d.step == Step::Normalize {
            match d.children.last() {
                Some(c) => d = c,
                None => break,
            }
        }
        let args = |args: &[GenericArg]| -> Vec<String> {
            args.iter()
                .map(|a| self.render(t, &a.to_string()))
                .collect()
        };
        match &d.step {
            Step::Conformance { source, args: a }
            | Step::ProjectionConformance {
                source, args: a, ..
            } => match source {
                ConformanceSource::Impl(i) => Selection::Impl {
                    path: self.tables.impl_path(*i).cloned().expect("validated id"),
                    args: args(a),
                },
                ConformanceSource::Model(m) => Selection::Model {
                    path: self.tables.model_path(*m).cloned().expect("validated id"),
                    args: args(a),
                },
            },
            Step::Assumption { index } | Step::ProjectionAssumption { index } => {
                Selection::Assumption(*index)
            }
            Step::AliasBound { bound } => Selection::AliasBound(*bound),
            Step::RigidProjection | Step::Normalize => Selection::Rigid,
            Step::Mode => Selection::Mode,
            Step::CoinductiveHypothesis => Selection::Coinductive,
        }
    }

    /// Renders Pact output with external names and placeholder names.
    fn render(&self, t: &TranslatedQuery, s: &str) -> String {
        let s = self.tables.rename_ids(s);
        let mut out = String::with_capacity(s.len());
        let mut chars = s.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            if c == '!' {
                let digits: String = s[i + 1..]
                    .chars()
                    .take_while(|d| d.is_ascii_digit())
                    .collect();
                if let Ok(n) = digits.parse::<usize>()
                    && let Some(name) = t.placeholder_names.get(n)
                {
                    out.push_str(name);
                    for _ in 0..digits.len() {
                        chars.next();
                    }
                    continue;
                }
            }
            out.push(c);
        }
        out
    }

    /// Renders a goal of a translated query with external names.
    pub fn render_goal(&self, t: &TranslatedQuery, g: &Goal) -> String {
        self.render(t, &g.to_string())
    }

    /// Mode of a ground Pact type computed from the session's constructor
    /// schemes (the same summary the solver uses).
    pub fn ground_mode(&self, ty: &Ty) -> Option<Mode> {
        match crate::lower::upper_mode(&self.tables, ty, &|_| None) {
            crate::lower::ModeKnowledge::Known(m) => Some(m),
            crate::lower::ModeKnowledge::Unknown => None,
        }
    }

    /// Translates a type at `site` with no parameters in scope.
    pub fn translate_type(&self, site: &PackageIdentity, t: &TypeDto) -> Result<Ty, BridgeError> {
        let mut cx = Cx::new(&self.tables, site.clone(), Position::Env);
        cx.ty(t)
            .map_err(|f| fail_error(ErrorKind::Invalid, None, f))
    }

    fn outcome(
        &self,
        t: TranslatedQuery,
        o: vesper_pact::Outcome,
    ) -> Result<BridgeOutcome, BridgeError> {
        use vesper_pact::Outcome;
        Ok(match o {
            Outcome::Proven(Solution {
                binders,
                var_values,
                derivation,
                proof_kind,
            }) => {
                match self.verify(&self.revision, &t.query, &binders, &var_values, &derivation) {
                    Ok(()) => {}
                    Err(e) if e.kind == ErrorKind::Unsupported => {
                        return Ok(BridgeOutcome::Unsupported {
                            pact: None,
                            explanation: e.to_string(),
                        });
                    }
                    Err(e) => return Err(e),
                }
                BridgeOutcome::Proven(Box::new(Evidence {
                    revision: self.revision.clone(),
                    translated: t,
                    binders,
                    var_values,
                    derivation,
                    proof_kind,
                }))
            }
            Outcome::Ambiguous(a) => BridgeOutcome::Ambiguous {
                explanation: self.render(&t, &self.ambiguity_text(&a)),
                ambiguity: a,
            },
            Outcome::NoSolution(f) => BridgeOutcome::NoSolution {
                explanation: self.render(&t, &f.render()),
                failure: f,
            },
            Outcome::Unsupported(r) => BridgeOutcome::Unsupported {
                explanation: self.render(
                    &t,
                    &match &r {
                        UnsupportedReason::DuplicateEffectLabel(k) => format!(
                            "merging effect rows repeats {}; duplicate labels are outside Pact's fragment",
                            match k {
                                vesper_pact::LabelKey::Effect(e) => e.to_string(),
                                vesper_pact::LabelKey::Capability(v) => format!("capability {v}"),
                            }
                        ),
                        UnsupportedReason::MixedCycle(g) => {
                            format!("{g} is reached through mixed coinductive/inductive cycles")
                        }
                        UnsupportedReason::Internal(s) => format!("internal: {s}"),
                    },
                ),
                pact: Some(r),
            },
            Outcome::Overflow(kind) => BridgeOutcome::Overflow { kind },
        })
    }

    fn ambiguity_text(&self, a: &Ambiguity) -> String {
        match a {
            Ambiguity::SelfTypeUnknown => "the self type is not yet known".into(),
            Ambiguity::MultipleCandidates(c) => format!(
                "several candidates apply: {}",
                c.iter()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Ambiguity::ConflictingScopedModels(ms) => format!(
                "conflicting `use model` scope: {}",
                ms.iter()
                    .map(|m| m.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Ambiguity::ModeUnknown => "a mode is not yet known".into(),
            Ambiguity::NonInjectiveProjection => "two rigid projections cannot be equated".into(),
            Ambiguity::Stalled(g) => format!("no progress on {g}"),
        }
    }
}

/// Substitutes query binders `?i` by their solution values.
struct InferSubst<'a>(&'a [GenericArg]);
impl VarFolder for InferSubst<'_> {
    fn fold_var(&mut self, var: Var, kind: ParamKind) -> GenericArg {
        match var {
            Var::Infer(i) => self.0[i as usize].clone(),
            other => GenericArg::var(kind, other),
        }
    }
}

/// A solving engine bound to one session; answers are tabled across queries.
pub struct Engine<'s> {
    session: &'s CoherentSession,
    solver: Solver<'s>,
}

impl Engine<'_> {
    pub fn solve(&mut self, q: &QueryBindingDto) -> Result<BridgeOutcome, BridgeError> {
        let t = self.session.translate_query(q)?;
        let o = self
            .solver
            .solve(&t.query)
            .map_err(|d| pact_error(&self.session.tables, ErrorKind::InvalidQuery, d))?;
        self.session.outcome(t, o)
    }

    pub fn table_len(&self) -> usize {
        self.solver.table_len()
    }
}
