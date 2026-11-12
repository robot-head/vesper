//! Inference context: substitution, resolution and first-order unification
//! of types, modes, effect rows and brands.

use crate::term::*;

#[derive(Clone, Debug)]
struct Slot {
    kind: ParamKind,
    value: Option<GenericArg>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UnifyError {
    Mismatch,
    /// Cannot be decided without more information (e.g. relating two
    /// non-identical rigid projections that still contain inference
    /// variables: projections are not injective).
    Ambiguous,
    /// Merging rows produced a repeated effect label.
    DuplicateLabel(LabelKey),
}

impl From<DuplicateLabel> for UnifyError {
    fn from(d: DuplicateLabel) -> Self {
        UnifyError::DuplicateLabel(d.0)
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct InferCtxt {
    slots: Vec<Slot>,
}

impl InferCtxt {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn new_var(&mut self, kind: ParamKind) -> Var {
        self.slots.push(Slot { kind, value: None });
        Var::Infer((self.slots.len() - 1) as u32)
    }

    pub fn fresh_args(&mut self, kinds: &[ParamKind]) -> Vec<GenericArg> {
        kinds
            .iter()
            .map(|&k| {
                let v = self.new_var(k);
                GenericArg::var(k, v)
            })
            .collect()
    }

    pub fn kind_of(&self, i: u32) -> ParamKind {
        self.slots[i as usize].kind
    }

    fn value(&self, v: Var) -> Option<&GenericArg> {
        match v {
            Var::Infer(i) => self.slots[i as usize].value.as_ref(),
            _ => None,
        }
    }

    fn bind(&mut self, i: u32, value: GenericArg) {
        debug_assert_eq!(self.slots[i as usize].kind, value.kind());
        debug_assert!(self.slots[i as usize].value.is_none());
        self.slots[i as usize].value = Some(value);
    }

    // -- resolution ---------------------------------------------------------

    pub fn resolve_goal(&self, g: &Goal) -> Result<Goal, DuplicateLabel> {
        Resolver::run(self, |r| fold_goal(g, r))
    }
    pub fn resolve_ty(&self, t: &Ty) -> Result<Ty, DuplicateLabel> {
        Resolver::run(self, |r| fold_ty(t, r))
    }
    pub fn resolve_arg(&self, a: &GenericArg) -> Result<GenericArg, DuplicateLabel> {
        Resolver::run(self, |r| fold_arg(a, r))
    }
    pub fn resolve_mode(&self, m: &ModeTerm) -> ModeTerm {
        fold_mode(m, &mut Resolver::new(self))
    }
    pub fn resolve_brand(&self, b: &Brand) -> Brand {
        fold_brand(b, &mut Resolver::new(self))
    }

    // -- unification --------------------------------------------------------

    pub fn unify_args(&mut self, a: &[GenericArg], b: &[GenericArg]) -> Result<(), UnifyError> {
        if a.len() != b.len() {
            return Err(UnifyError::Mismatch);
        }
        for (x, y) in a.iter().zip(b) {
            self.unify_arg(x, y)?;
        }
        Ok(())
    }

    pub fn unify_arg(&mut self, a: &GenericArg, b: &GenericArg) -> Result<(), UnifyError> {
        match (a, b) {
            (GenericArg::Ty(x), GenericArg::Ty(y)) => self.unify_ty(x, y),
            (GenericArg::Mode(x), GenericArg::Mode(y)) => self.unify_mode(x, y),
            (GenericArg::Row(x), GenericArg::Row(y)) => self.unify_row(x, y),
            (GenericArg::Brand(x), GenericArg::Brand(y)) => self.unify_brand(x, y),
            (GenericArg::Capability(x), GenericArg::Capability(y)) => {
                if x == y {
                    Ok(())
                } else {
                    Err(UnifyError::Mismatch)
                }
            }
            _ => Err(UnifyError::Mismatch),
        }
    }

    pub fn unify_trait_ref(&mut self, a: &TraitRef, b: &TraitRef) -> Result<(), UnifyError> {
        if a.trait_id != b.trait_id {
            return Err(UnifyError::Mismatch);
        }
        self.unify_ty(&a.self_ty, &b.self_ty)?;
        self.unify_args(&a.args, &b.args)
    }

    pub fn unify_ty(&mut self, a: &Ty, b: &Ty) -> Result<(), UnifyError> {
        let a = self.shallow_ty(a);
        let b = self.shallow_ty(b);
        match (&a, &b) {
            (Ty::Var(Var::Infer(x)), Ty::Var(Var::Infer(y))) if x == y => Ok(()),
            (Ty::Var(Var::Infer(x)), other) | (other, Ty::Var(Var::Infer(x))) => {
                let value = self.resolve_ty(other)?;
                if self.occurs_ty(*x, &value) {
                    return Err(UnifyError::Mismatch);
                }
                self.bind(*x, GenericArg::Ty(value));
                Ok(())
            }
            (Ty::Var(x), Ty::Var(y)) => {
                if x == y {
                    Ok(())
                } else {
                    Err(UnifyError::Mismatch)
                }
            }
            (Ty::Ctor { ctor: c1, args: a1 }, Ty::Ctor { ctor: c2, args: a2 }) => {
                if c1 != c2 {
                    return Err(UnifyError::Mismatch);
                }
                self.unify_args(a1, a2)
            }
            (Ty::Projection(p1), Ty::Projection(p2)) => {
                // Goals are normalized eagerly, so projections reaching
                // unification are rigid. Rigid projections are not
                // injective: only syntactic identity is a success.
                let p1 = Resolver::run(self, |r| fold_projection(p1, r))?;
                let p2 = Resolver::run(self, |r| fold_projection(p2, r))?;
                if p1 == p2 {
                    Ok(())
                } else if projection_has_infer(&p1) || projection_has_infer(&p2) {
                    Err(UnifyError::Ambiguous)
                } else {
                    Err(UnifyError::Mismatch)
                }
            }
            _ => Err(UnifyError::Mismatch),
        }
    }

    fn shallow_ty(&self, t: &Ty) -> Ty {
        let mut t = t.clone();
        while let Ty::Var(v) = t {
            match self.value(v) {
                Some(GenericArg::Ty(next)) => t = next.clone(),
                _ => break,
            }
        }
        t
    }

    pub fn unify_mode(&mut self, a: &ModeTerm, b: &ModeTerm) -> Result<(), UnifyError> {
        let a = self.resolve_mode(a);
        let b = self.resolve_mode(b);
        match (&a, &b) {
            _ if a == b => Ok(()),
            (ModeTerm::Var(Var::Infer(x)), other) | (other, ModeTerm::Var(Var::Infer(x))) => {
                self.bind(*x, GenericArg::Mode(other.clone()));
                Ok(())
            }
            _ => Err(UnifyError::Mismatch),
        }
    }

    pub fn unify_brand(&mut self, a: &Brand, b: &Brand) -> Result<(), UnifyError> {
        let a = self.resolve_brand(a);
        let b = self.resolve_brand(b);
        match (&a, &b) {
            _ if a == b => Ok(()),
            (Brand::Var(Var::Infer(x)), other) | (other, Brand::Var(Var::Infer(x))) => {
                self.bind(*x, GenericArg::Brand(other.clone()));
                Ok(())
            }
            _ => Err(UnifyError::Mismatch),
        }
    }

    /// Row unification keyed by effect identity (each key occurs at most once
    /// per row in this fragment). Open rows are solved by binding inference
    /// tails, introducing a shared fresh tail when both sides need extension.
    pub fn unify_row(&mut self, a: &EffectRow, b: &EffectRow) -> Result<(), UnifyError> {
        let a = Resolver::run(self, |r| fold_row(a, r))?;
        let b = Resolver::run(self, |r| fold_row(b, r))?;
        let mut only_a = Vec::new();
        let mut matched = Vec::new();
        for la in &a.labels {
            match b.labels.iter().find(|lb| lb.key() == la.key()) {
                Some(lb) => matched.push((la.clone(), lb.clone())),
                None => only_a.push(la.clone()),
            }
        }
        let only_b: Vec<EffectLabel> = b
            .labels
            .iter()
            .filter(|lb| !a.labels.iter().any(|la| la.key() == lb.key()))
            .cloned()
            .collect();
        for (la, lb) in matched {
            if let (EffectLabel::Effect { args: x, .. }, EffectLabel::Effect { args: y, .. }) =
                (&la, &lb)
            {
                self.unify_args(x, y)?;
            }
        }
        let infer = |t: Option<Var>| matches!(t, Some(Var::Infer(_)));
        match (a.tail, b.tail) {
            (None, None) => {
                if only_a.is_empty() && only_b.is_empty() {
                    Ok(())
                } else {
                    Err(UnifyError::Mismatch)
                }
            }
            (Some(ta), tb) if ta == tb.unwrap_or(Var::Bound(u32::MAX)) => {
                if only_a.is_empty() && only_b.is_empty() {
                    Ok(())
                } else {
                    Err(UnifyError::Mismatch)
                }
            }
            (Some(ta), tb) if infer(Some(ta)) && (only_a.is_empty() || infer(tb)) => {
                let Var::Infer(x) = ta else { unreachable!() };
                if only_a.is_empty() {
                    self.bind_row(
                        x,
                        EffectRow {
                            labels: only_b,
                            tail: tb,
                        },
                    )
                } else {
                    let Some(Var::Infer(y)) = tb else {
                        unreachable!()
                    };
                    if only_b.is_empty() {
                        return self.bind_row(
                            y,
                            EffectRow {
                                labels: only_a,
                                tail: Some(ta),
                            },
                        );
                    }
                    let fresh = self.new_var(ParamKind::Row);
                    self.bind_row(
                        x,
                        EffectRow {
                            labels: only_b,
                            tail: Some(fresh),
                        },
                    )?;
                    self.bind_row(
                        y,
                        EffectRow {
                            labels: only_a,
                            tail: Some(fresh),
                        },
                    )
                }
            }
            (ta, Some(Var::Infer(y))) if only_b.is_empty() => self.bind_row(
                y,
                EffectRow {
                    labels: only_a,
                    tail: ta,
                },
            ),
            _ => Err(UnifyError::Mismatch),
        }
    }

    fn bind_row(&mut self, x: u32, mut row: EffectRow) -> Result<(), UnifyError> {
        row.sort();
        let row = Resolver::run(self, |r| fold_row(&row, r))?;
        if self.occurs_arg(x, &GenericArg::Row(row.clone())) {
            return Err(UnifyError::Mismatch);
        }
        self.bind(x, GenericArg::Row(row));
        Ok(())
    }

    fn occurs_ty(&self, x: u32, t: &Ty) -> bool {
        self.occurs_arg(x, &GenericArg::Ty(t.clone()))
    }

    fn occurs_arg(&self, x: u32, a: &GenericArg) -> bool {
        let mut found = false;
        visit_arg_vars(a, |v, _| {
            if v == Var::Infer(x) {
                found = true;
            }
        });
        found
    }
}

pub(crate) fn projection_has_infer(p: &Projection) -> bool {
    let mut found = false;
    visit_goal_vars(
        &Goal::Implements {
            trait_ref: p.trait_ref.clone(),
            brand: p.brand.clone(),
        },
        |v, _| found |= matches!(v, Var::Infer(_)),
    );
    found
}

/// Follows bindings. A duplicate label discovered while re-resolving a
/// nested bound value is recorded and surfaced by [`Resolver::run`].
struct Resolver<'a> {
    cx: &'a InferCtxt,
    err: Option<DuplicateLabel>,
}

impl<'a> Resolver<'a> {
    fn new(cx: &'a InferCtxt) -> Self {
        Resolver { cx, err: None }
    }
    fn run<T>(
        cx: &'a InferCtxt,
        f: impl FnOnce(&mut Resolver<'a>) -> Result<T, DuplicateLabel>,
    ) -> Result<T, DuplicateLabel> {
        let mut r = Resolver::new(cx);
        let out = f(&mut r)?;
        match r.err {
            Some(e) => Err(e),
            None => Ok(out),
        }
    }
}

impl VarFolder for Resolver<'_> {
    fn fold_var(&mut self, var: Var, kind: ParamKind) -> GenericArg {
        match self.cx.value(var) {
            Some(v) => {
                let v = v.clone();
                match fold_arg(&v, self) {
                    Ok(r) => r,
                    Err(e) => {
                        self.err.get_or_insert(e);
                        v
                    }
                }
            }
            None => GenericArg::var(kind, var),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::EffectId;

    fn l(e: u32) -> EffectLabel {
        EffectLabel::Effect {
            effect: EffectId(e),
            args: vec![],
        }
    }

    #[test]
    fn open_rows_share_a_fresh_tail() {
        let mut cx = InferCtxt::new();
        let a = cx.new_var(ParamKind::Row);
        let b = cx.new_var(ParamKind::Row);
        // [0 | a] = [1 | b]  ⇒  a = [1 | t], b = [0 | t]
        cx.unify_row(
            &EffectRow::open(vec![l(0)], a),
            &EffectRow::open(vec![l(1)], b),
        )
        .unwrap();
        let ra = cx.resolve_arg(&GenericArg::var(ParamKind::Row, a)).unwrap();
        let rb = cx.resolve_arg(&GenericArg::var(ParamKind::Row, b)).unwrap();
        let (GenericArg::Row(ra), GenericArg::Row(rb)) = (ra, rb) else {
            panic!()
        };
        assert_eq!(ra.labels, vec![l(1)]);
        assert_eq!(rb.labels, vec![l(0)]);
        assert_eq!(ra.tail, rb.tail);
        assert!(ra.tail.is_some());
    }

    #[test]
    fn rigid_and_closed_rows() {
        let mut cx = InferCtxt::new();
        let p = Var::Placeholder(0);
        assert_eq!(
            cx.unify_row(&EffectRow::open(vec![], p), &EffectRow::closed(vec![])),
            Err(UnifyError::Mismatch)
        );
        assert_eq!(
            cx.unify_row(
                &EffectRow::open(vec![l(0)], p),
                &EffectRow::open(vec![l(0)], p)
            ),
            Ok(())
        );
        assert_eq!(
            cx.unify_row(
                &EffectRow::open(vec![l(0)], p),
                &EffectRow::open(vec![l(1)], p)
            ),
            Err(UnifyError::Mismatch)
        );
        let v = cx.new_var(ParamKind::Row);
        // [0 | v] = [0, 1 | p]  ⇒  v = [1 | p]
        cx.unify_row(
            &EffectRow::open(vec![l(0)], v),
            &EffectRow::open(vec![l(0), l(1)], p),
        )
        .unwrap();
        assert_eq!(
            cx.resolve_arg(&GenericArg::var(ParamKind::Row, v)).unwrap(),
            GenericArg::Row(EffectRow::open(vec![l(1)], p))
        );
        // A closed row cannot absorb an extra label.
        let w = cx.new_var(ParamKind::Row);
        assert_eq!(
            cx.unify_row(
                &EffectRow::open(vec![l(2)], w),
                &EffectRow::closed(vec![l(0)])
            ),
            Err(UnifyError::Mismatch)
        );
    }

    #[test]
    fn occurs_check_rejects_infinite_types() {
        let mut cx = InferCtxt::new();
        let v = cx.new_var(ParamKind::Type);
        let looped = Ty::Ctor {
            ctor: crate::ids::TypeCtorId(0),
            args: vec![GenericArg::Ty(Ty::Var(v))],
        };
        assert_eq!(cx.unify_ty(&Ty::Var(v), &looped), Err(UnifyError::Mismatch));
    }
}
