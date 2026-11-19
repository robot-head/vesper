//! The Rust coherence checker, frozen for the compiler's closure: only the seed copy
//! vesper-pact-seed builds it; the package vesper-pact runs the Vesper port.

use crate::coherence::{check_orphan, conformance_env};
use crate::diag::{DiagCode, Diagnostic, Subject};
use crate::engine::infer::{InferCtxt, UnifyError};
use crate::ids::AssocIdx;
use crate::program::{ConformanceDecl, Program, QueryDto};
use crate::solver::{Outcome, Solver};
use crate::term::*;

pub(crate) fn check_coherence(program: &Program) -> Vec<Diagnostic> {
    let mut diags = vec![];
    for im in &program.dto().impls {
        if let Err(msg) = check_orphan(program, &im.decl) {
            diags.push(Diagnostic::new(
                DiagCode::OrphanImpl,
                Subject::Impl(im.id),
                msg,
            ));
        }
    }
    for t in &program.dto().traits {
        let impls = program.impls_of(t.id);
        for (x, &a) in impls.iter().enumerate() {
            for &b in &impls[x + 1..] {
                match heads_unify(&program.impl_decl(a).decl, &program.impl_decl(b).decl) {
                    Ok(false) => {}
                    Ok(true) => diags.push(Diagnostic::new(
                        DiagCode::OverlappingImpls,
                        Subject::ImplPair(a, b),
                        format!(
                            "canonical impls of {} overlap; declare one as a named `model`",
                            t.id
                        ),
                    )),
                    Err(msg) => diags.push(Diagnostic::new(
                        DiagCode::Unsupported,
                        Subject::ImplPair(a, b),
                        msg,
                    )),
                }
            }
        }
    }
    for im in &program.dto().impls {
        well_formed(program, &im.decl, Subject::Impl(im.id), &mut diags);
    }
    for m in &program.dto().models {
        well_formed(program, &m.decl, Subject::Model(m.id), &mut diags);
    }
    diags.sort();
    diags.dedup();
    diags
}

/// Whether two impl heads have a common instance (`Err` when undecidable in
/// this fragment). No negative reasoning over where-clauses is performed:
/// unifiable heads overlap.
fn heads_unify(a: &ConformanceDecl, b: &ConformanceDecl) -> Result<bool, String> {
    let mut cx = InferCtxt::new();
    let aa = cx.fresh_args(&a.params);
    let ba = cx.fresh_args(&b.params);
    let ha = instantiate_trait_ref(&a.trait_ref, &aa)
        .map_err(|d| format!("duplicate label {:?}", d.0))?;
    let hb = instantiate_trait_ref(&b.trait_ref, &ba)
        .map_err(|d| format!("duplicate label {:?}", d.0))?;
    match cx.unify_trait_ref(&ha, &hb) {
        Ok(()) => Ok(true),
        Err(UnifyError::Mismatch) => Ok(false),
        Err(UnifyError::Ambiguous) => Err("cannot decide overlap of projection heads".into()),
        Err(UnifyError::DuplicateLabel(k)) => Err(format!(
            "cannot decide overlap: rows merge duplicate label {k:?}"
        )),
    }
}

fn well_formed(
    program: &Program,
    decl: &ConformanceDecl,
    subject: Subject,
    diags: &mut Vec<Diagnostic>,
) {
    let env = conformance_env(decl);
    let to_ph = |v: Var, k: ParamKind| match v {
        Var::Bound(i) => GenericArg::var(k, Var::Placeholder(i)),
        other => GenericArg::var(k, other),
    };
    let unsupported = |diags: &mut Vec<Diagnostic>, what: String| {
        diags.push(Diagnostic::new(
            DiagCode::Unsupported,
            subject,
            format!("cannot check well-formedness: {what}; rejected rather than skipped"),
        ))
    };
    let Ok(tr) = fold_trait_ref(&decl.trait_ref, &mut { to_ph }) else {
        unsupported(diags, "head repeats an effect label".into());
        return;
    };
    let t = program.trait_decl(tr.trait_id);
    let mut binder = vec![GenericArg::Ty(tr.self_ty.clone())];
    binder.extend(tr.args.iter().cloned());
    let mut solver = Solver::new(program);
    for (ai, (assoc, value)) in t.assoc.iter().zip(&decl.assoc_values).enumerate() {
        let Ok(value) = fold_ty(value, &mut { to_ph }) else {
            unsupported(
                diags,
                format!("value of {} repeats an effect label", AssocIdx(ai as u32)),
            );
            continue;
        };
        let mut goals = vec![Goal::ModeBound {
            ty: value.clone(),
            bound: instantiate_mode(&assoc.mode_bound, &binder),
        }];
        for (bi, b) in assoc.bounds.iter().enumerate() {
            let Ok(args) = instantiate_args(&b.args, &binder) else {
                unsupported(
                    diags,
                    format!(
                        "bound {bi} of {} repeats an effect label after substitution",
                        AssocIdx(ai as u32)
                    ),
                );
                continue;
            };
            goals.push(Goal::Implements {
                trait_ref: TraitRef {
                    trait_id: b.trait_id,
                    self_ty: value.clone(),
                    args,
                },
                brand: instantiate_brand(&b.brand, &binder),
            });
        }
        for g in goals {
            let q = QueryDto::new(env.clone(), vec![], g.clone());
            let outcome = solver.solve(&q);
            let detail = match outcome {
                Ok(Outcome::Proven(_)) => continue,
                Ok(Outcome::NoSolution(f)) => format!("no-solution\n{}", f.render().trim_end()),
                Ok(o) => o.verdict().to_string(),
                Err(d) => crate::diag::render(&d).trim_end().to_string(),
            };
            diags.push(Diagnostic::new(
                DiagCode::ImplNotWellFormed,
                subject,
                format!(
                    "associated value {} = {value} must satisfy `{g}`: {detail}",
                    AssocIdx(ai as u32)
                ),
            ));
        }
    }
}
