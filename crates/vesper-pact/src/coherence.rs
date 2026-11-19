//! Stratified coherence (design §16.3).
//!
//! * Tier 1 canonical impls must satisfy the covered-first orphan rule and
//!   must not overlap pairwise (Global Model Uniqueness).
//! * Tier 2 named models are exempt from both checks: they are never found
//!   by canonical search, so they cannot collide.
//! * Every impl and model must be well formed: associated values satisfy
//!   their declared modal and trait bounds under the declaration's
//!   where-clauses.

use crate::diag::Diagnostic;
use crate::program::{ConformanceDecl, Env, PlaceholderDecl, Program};
use crate::term::*;

/// Runs orphan, overlap and well-formedness checks. The result is sorted.
pub fn check_coherence(program: &Program) -> Vec<Diagnostic> {
    #[cfg(vesper_port)]
    return crate::port::check_coherence(program);
    #[cfg(not(vesper_port))]
    crate::engine::coherence::check_coherence(program)
}

/// Covered-first orphan rule. A canonical impl declared in package `P` is
/// legal when the trait is defined in `P`, or when, scanning the ordered type
/// arguments `(T_0 = Self, T_1, ..., T_n)`, a type constructor defined in `P`
/// is reached before any uncovered impl parameter. Parameters nested inside a
/// foreign constructor are covered. Non-type arguments are skipped.
pub fn check_orphan(program: &Program, decl: &ConformanceDecl) -> Result<(), String> {
    let t = program.trait_decl(decl.trait_ref.trait_id);
    if t.package == decl.package {
        return Ok(());
    }
    let seq = std::iter::once(&decl.trait_ref.self_ty).chain(
        decl.trait_ref.args.iter().filter_map(|a| match a {
            GenericArg::Ty(t) => Some(t),
            _ => None,
        }),
    );
    for (i, ty) in seq.enumerate() {
        match ty {
            Ty::Ctor { ctor, .. } if program.ctor(*ctor).package == decl.package => return Ok(()),
            Ty::Ctor { .. } => continue,
            Ty::Var(v) => {
                return Err(format!(
                    "orphan canonical impl: uncovered parameter {v} at type position {i} precedes any local type; \
                     declare a named `model` instead"
                ));
            }
            Ty::Projection(_) => return Err("projection in impl head".into()),
        }
    }
    Err(format!(
        "orphan canonical impl: neither {} nor any type argument is defined in {}; declare a named `model` instead",
        t.id, decl.package
    ))
}

/// The environment under which an impl/model body is checked: parameters as
/// placeholders, where-clauses as assumptions. `ModeBound(^i, c)` clauses
/// become placeholder mode bounds; other mode clauses are not assumed.
pub fn conformance_env(decl: &ConformanceDecl) -> Env {
    let to_ph = |v: Var, k: ParamKind| match v {
        Var::Bound(i) => GenericArg::var(k, Var::Placeholder(i)),
        other => GenericArg::var(k, other),
    };
    let mut placeholders: Vec<PlaceholderDecl> = decl
        .params
        .iter()
        .map(|&k| PlaceholderDecl::of(k))
        .collect();
    let mut assumptions = vec![];
    for w in &decl.where_clauses {
        match w {
            Goal::ModeBound {
                ty: Ty::Var(Var::Bound(i)),
                bound: ModeTerm::Const(m),
            } => {
                let p = &mut placeholders[*i as usize];
                p.mode_bound = Some(p.mode_bound.map_or(*m, |old| old.min(*m)));
            }
            Goal::ModeBound { .. } => {}
            other => {
                if let Ok(g) = fold_goal(other, &mut { to_ph }) {
                    assumptions.push(g);
                }
            }
        }
    }
    Env {
        placeholders,
        assumptions,
        model_scopes: vec![],
    }
}
