//! The Rust program validator, frozen for the compiler's closure: only the seed copy
//! vesper-pact-seed builds it; the package vesper-pact runs the Vesper port.

use crate::diag::{DiagCode, Diagnostic, Subject};
use crate::ids::*;
use crate::program::{ConformanceDecl, ProgramDto, Scope, Validator, check_schema};
use crate::term::*;

/// Validates a whole program (`Program::new`).
pub(crate) fn validate_program(dto: &ProgramDto) -> Result<(), Vec<Diagnostic>> {
    let mut diags = Vec::new();
    Validator {
        dto,
        diags: &mut diags,
    }
    .run();
    if !diags.is_empty() {
        diags.sort();
        diags.dedup();
        return Err(diags);
    }
    Ok(())
}

impl Validator<'_> {
    fn run(&mut self) {
        let dto = self.dto;
        if let Some(d) = check_schema(dto.schema) {
            self.diags.push(d);
            return;
        }
        for (i, p) in dto.packages.iter().enumerate() {
            if p.id.index() != i {
                self.err(
                    DiagCode::IdNotDense,
                    Subject::Program,
                    format!("package at {i} has id {}", p.id),
                );
            }
        }
        for (i, c) in dto.type_ctors.iter().enumerate() {
            if c.id.index() != i {
                self.err(
                    DiagCode::IdNotDense,
                    Subject::Program,
                    format!("type ctor at {i} has id {}", c.id),
                );
            }
            self.package(c.package, Subject::TypeCtor(c.id));
            for &j in &c.mode.joins {
                if c.params.get(j as usize) != Some(&ParamKind::Type) {
                    self.err(
                        DiagCode::Malformed,
                        Subject::TypeCtor(c.id),
                        format!("mode join index {j} is not a type parameter"),
                    );
                }
            }
        }
        for (i, e) in dto.effects.iter().enumerate() {
            if e.id.index() != i {
                self.err(
                    DiagCode::IdNotDense,
                    Subject::Program,
                    format!("effect at {i} has id {}", e.id),
                );
            }
            self.package(e.package, Subject::Effect(e.id));
        }
        for (i, t) in dto.traits.iter().enumerate() {
            if t.id.index() != i {
                self.err(
                    DiagCode::IdNotDense,
                    Subject::Program,
                    format!("trait at {i} has id {}", t.id),
                );
            }
            self.package(t.package, Subject::Trait(t.id));
            let mut binder = vec![ParamKind::Type];
            binder.extend(&t.params);
            let scope = Scope::Template(&binder);
            for (ai, a) in t.assoc.iter().enumerate() {
                let subj = Subject::Trait(t.id);
                self.mode(&a.mode_bound, scope, subj);
                for b in &a.bounds {
                    if !self.trait_args(b.trait_id, &b.args, scope, subj) {
                        continue;
                    }
                    self.brand(&b.brand, scope, subj);
                    if matches!(b.brand, Brand::Var(_)) {
                        self.err(
                            DiagCode::Unsupported,
                            subj,
                            format!(
                                "bound on {} must name an explicit brand",
                                AssocIdx(ai as u32)
                            ),
                        );
                    }
                }
            }
        }
        for (i, im) in dto.impls.iter().enumerate() {
            if im.id.index() != i {
                self.err(
                    DiagCode::IdNotDense,
                    Subject::Program,
                    format!("impl at {i} has id {}", im.id),
                );
            }
            self.conformance(&im.decl, Subject::Impl(im.id));
        }
        for (i, m) in dto.models.iter().enumerate() {
            if m.id.index() != i {
                self.err(
                    DiagCode::IdNotDense,
                    Subject::Program,
                    format!("model at {i} has id {}", m.id),
                );
            }
            self.conformance(&m.decl, Subject::Model(m.id));
        }
    }

    fn package(&mut self, p: PackageId, subj: Subject) {
        if p.index() >= self.dto.packages.len() {
            self.err(DiagCode::UnknownId, subj, format!("unknown {p}"));
        }
    }

    fn conformance(&mut self, c: &ConformanceDecl, subj: Subject) {
        self.package(c.package, subj);
        if c.params.contains(&ParamKind::Capability) {
            // Instantiating such a parameter would need capability inference
            // variables, which row unification does not support.
            self.err(
                DiagCode::Unsupported,
                subj,
                "capability parameters on impls/models are outside this fragment".into(),
            );
            return;
        }
        let scope = Scope::Template(&c.params);
        if !self.trait_ref(&c.trait_ref, scope, subj) {
            return;
        }
        if trait_ref_has_projection(&c.trait_ref) {
            self.err(
                DiagCode::Unsupported,
                subj,
                "associated-type projections in an impl/model head are outside this fragment"
                    .into(),
            );
        }
        for w in &c.where_clauses {
            // Template scope rejects inference brands, so where-clauses always
            // name canonical, named or parameter brands explicitly: lexical
            // `use model` scopes of a use site never leak into impl bodies.
            self.goal(w, scope, subj);
        }
        let t = &self.dto.traits[c.trait_ref.trait_id.index()];
        if c.assoc_values.len() != t.assoc.len() {
            self.err(
                DiagCode::Arity,
                subj,
                format!(
                    "{} associated values supplied, {} declares {}",
                    c.assoc_values.len(),
                    t.id,
                    t.assoc.len()
                ),
            );
        }
        for v in &c.assoc_values {
            self.ty(v, scope, subj);
        }
        // Every parameter must be determined by the head (no unconstrained
        // impl parameters), otherwise a selected impl would leave free choices.
        let mut seen = vec![false; c.params.len()];
        let head = Goal::Implements {
            trait_ref: c.trait_ref.clone(),
            brand: Brand::Canonical,
        };
        visit_goal_vars(&head, |v, _| {
            if let Var::Bound(i) = v
                && let Some(s) = seen.get_mut(i as usize)
            {
                *s = true;
            }
        });
        for (i, s) in seen.iter().enumerate() {
            if !s {
                self.err(
                    DiagCode::UnconstrainedParam,
                    subj,
                    format!("parameter ^{i} does not occur in the head"),
                );
            }
        }
    }
}
