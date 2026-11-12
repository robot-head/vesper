//! Goal terms shared by declarations (templates), environments and queries.
//!
//! One term language is used everywhere. Which variable forms are legal
//! depends on where a term appears:
//!
//! * declaration templates (impl heads, where-clauses, associated values) use
//!   only [`Var::Bound`], indexing the declaration's own parameter list;
//! * environments ([`crate::Env`]) use only [`Var::Placeholder`], indexing the
//!   environment's rigid parameters (generic parameters in scope);
//! * queries use [`Var::Placeholder`] and [`Var::Infer`], the latter indexing
//!   the query's existential binders.
//!
//! Validation in [`crate::program`] enforces those rules before solving.

use crate::ids::{AssocIdx, EffectId, ModelId, TraitId, TypeCtorId};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Substructural mode lattice `copy ⊑ affine ⊑ linear`.
///
/// Mirrors `vesper_core::ast::Mode` (same order) without depending on the
/// string-keyed surface AST.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Mode {
    Copy,
    Affine,
    Linear,
}

impl Mode {
    pub fn join(self, other: Mode) -> Mode {
        self.max(other)
    }
    pub fn is_below(self, other: Mode) -> bool {
        self <= other
    }
}

/// The sort of a variable or generic argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ParamKind {
    Type,
    Mode,
    /// An effect row (`r: eff`).
    Row,
    /// A model identity brand (`M` in `CacheBTree[K, V, M]`).
    Brand,
    /// A lexical capability label (`scratch: Region`).
    Capability,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Var {
    /// Parameter of the enclosing declaration (template position).
    Bound(u32),
    /// Rigid, universally quantified parameter of the query environment.
    Placeholder(u32),
    /// Existential inference variable of a query.
    Infer(u32),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Ty {
    Ctor {
        ctor: TypeCtorId,
        args: Vec<GenericArg>,
    },
    Var(Var),
    /// `<Self as Trait[args]>::Assoc`, resolved through `brand`.
    Projection(Box<Projection>),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ModeTerm {
    Const(Mode),
    Var(Var),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum EffectLabel {
    Effect {
        effect: EffectId,
        args: Vec<GenericArg>,
    },
    /// Lexical capability label. Inference variables are not supported here.
    Capability(Var),
}

impl EffectLabel {
    /// Labels are keyed by effect identity; each key appears at most once in a
    /// row in this fragment.
    pub fn key(&self) -> LabelKey {
        match self {
            EffectLabel::Effect { effect, .. } => LabelKey::Effect(*effect),
            EffectLabel::Capability(v) => LabelKey::Capability(*v),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LabelKey {
    Effect(EffectId),
    Capability(Var),
}

/// An effect row `[l_1, ..., l_n | tail]`. Labels are kept sorted by key.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default)]
pub struct EffectRow {
    pub labels: Vec<EffectLabel>,
    pub tail: Option<Var>,
}

impl EffectRow {
    pub fn closed(labels: Vec<EffectLabel>) -> Self {
        let mut row = EffectRow { labels, tail: None };
        row.sort();
        row
    }
    pub fn open(labels: Vec<EffectLabel>, tail: Var) -> Self {
        let mut row = EffectRow {
            labels,
            tail: Some(tail),
        };
        row.sort();
        row
    }
    pub fn empty() -> Self {
        EffectRow::default()
    }
    pub(crate) fn sort(&mut self) {
        self.labels.sort_by_key(|a| a.key());
    }
    /// Returns the first effect key that occurs twice.
    pub fn duplicate_key(&self) -> Option<LabelKey> {
        let mut keys: Vec<_> = self.labels.iter().map(|l| l.key()).collect();
        keys.sort();
        keys.windows(2).find(|w| w[0] == w[1]).map(|w| w[0])
    }
}

/// Model identity brand.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Brand {
    /// The unique anonymous canonical model (`Canonical[Trait, T]`; the trait
    /// and type are fixed by the position in which the brand occurs).
    Canonical,
    /// A named `model` declaration.
    Named(ModelId),
    Var(Var),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum GenericArg {
    Ty(Ty),
    Mode(ModeTerm),
    Row(EffectRow),
    Brand(Brand),
    Capability(Var),
}

impl GenericArg {
    pub fn kind(&self) -> ParamKind {
        match self {
            GenericArg::Ty(_) => ParamKind::Type,
            GenericArg::Mode(_) => ParamKind::Mode,
            GenericArg::Row(_) => ParamKind::Row,
            GenericArg::Brand(_) => ParamKind::Brand,
            GenericArg::Capability(_) => ParamKind::Capability,
        }
    }
    /// The bare variable of the given kind.
    pub fn var(kind: ParamKind, v: Var) -> GenericArg {
        match kind {
            ParamKind::Type => GenericArg::Ty(Ty::Var(v)),
            ParamKind::Mode => GenericArg::Mode(ModeTerm::Var(v)),
            ParamKind::Row => GenericArg::Row(EffectRow {
                labels: vec![],
                tail: Some(v),
            }),
            ParamKind::Brand => GenericArg::Brand(Brand::Var(v)),
            ParamKind::Capability => GenericArg::Capability(v),
        }
    }
}

/// `Self: Trait[args]`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TraitRef {
    pub trait_id: TraitId,
    pub self_ty: Ty,
    pub args: Vec<GenericArg>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Projection {
    pub trait_ref: TraitRef,
    pub brand: Brand,
    pub assoc: AssocIdx,
}

/// A goal of the first Pact fragment.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Goal {
    /// `self_ty: Trait[args]` through the model identified by `brand`.
    /// An unresolved inference brand means implicit resolution: lexically
    /// bound models first, otherwise the canonical impl.
    Implements { trait_ref: TraitRef, brand: Brand },
    /// Associated-type equality `<..>::Assoc == ty` (lazy normalization).
    ProjectionEq { projection: Projection, ty: Ty },
    /// Modal kinding bound `mode(ty) ⊑ bound`.
    ModeBound { ty: Ty, bound: ModeTerm },
}

impl Goal {
    pub fn implements(trait_ref: TraitRef, brand: Brand) -> Goal {
        Goal::Implements { trait_ref, brand }
    }
}

// ---------------------------------------------------------------------------
// Generic traversal

/// Callback for variables encountered by [`fold_goal`] and friends. The
/// returned argument must have the same kind as the variable.
pub trait VarFolder {
    fn fold_var(&mut self, var: Var, kind: ParamKind) -> GenericArg;
}

impl<F: FnMut(Var, ParamKind) -> GenericArg> VarFolder for F {
    fn fold_var(&mut self, var: Var, kind: ParamKind) -> GenericArg {
        self(var, kind)
    }
}

/// Error raised when a fold produces an ill-formed row (duplicate effect key).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DuplicateLabel(pub LabelKey);

pub fn fold_ty(ty: &Ty, f: &mut dyn VarFolder) -> Result<Ty, DuplicateLabel> {
    Ok(match ty {
        Ty::Ctor { ctor, args } => Ty::Ctor {
            ctor: *ctor,
            args: fold_args(args, f)?,
        },
        Ty::Var(v) => match f.fold_var(*v, ParamKind::Type) {
            GenericArg::Ty(t) => t,
            other => panic!("kind mismatch folding type variable: {other:?}"),
        },
        Ty::Projection(p) => Ty::Projection(Box::new(fold_projection(p, f)?)),
    })
}

pub fn fold_args(
    args: &[GenericArg],
    f: &mut dyn VarFolder,
) -> Result<Vec<GenericArg>, DuplicateLabel> {
    args.iter().map(|a| fold_arg(a, f)).collect()
}

pub fn fold_arg(arg: &GenericArg, f: &mut dyn VarFolder) -> Result<GenericArg, DuplicateLabel> {
    Ok(match arg {
        GenericArg::Ty(t) => GenericArg::Ty(fold_ty(t, f)?),
        GenericArg::Mode(m) => GenericArg::Mode(fold_mode(m, f)),
        GenericArg::Row(r) => GenericArg::Row(fold_row(r, f)?),
        GenericArg::Brand(b) => GenericArg::Brand(fold_brand(b, f)),
        GenericArg::Capability(v) => GenericArg::Capability(fold_cap(*v, f)),
    })
}

pub fn fold_mode(m: &ModeTerm, f: &mut dyn VarFolder) -> ModeTerm {
    match m {
        ModeTerm::Const(c) => ModeTerm::Const(*c),
        ModeTerm::Var(v) => match f.fold_var(*v, ParamKind::Mode) {
            GenericArg::Mode(m) => m,
            other => panic!("kind mismatch folding mode variable: {other:?}"),
        },
    }
}

pub fn fold_brand(b: &Brand, f: &mut dyn VarFolder) -> Brand {
    match b {
        Brand::Var(v) => match f.fold_var(*v, ParamKind::Brand) {
            GenericArg::Brand(b) => b,
            other => panic!("kind mismatch folding brand variable: {other:?}"),
        },
        other => other.clone(),
    }
}

fn fold_cap(v: Var, f: &mut dyn VarFolder) -> Var {
    match f.fold_var(v, ParamKind::Capability) {
        GenericArg::Capability(v) => v,
        other => panic!("kind mismatch folding capability variable: {other:?}"),
    }
}

pub fn fold_row(r: &EffectRow, f: &mut dyn VarFolder) -> Result<EffectRow, DuplicateLabel> {
    let mut labels = Vec::with_capacity(r.labels.len());
    for l in &r.labels {
        labels.push(match l {
            EffectLabel::Effect { effect, args } => EffectLabel::Effect {
                effect: *effect,
                args: fold_args(args, f)?,
            },
            EffectLabel::Capability(v) => EffectLabel::Capability(fold_cap(*v, f)),
        });
    }
    let mut tail = None;
    if let Some(v) = r.tail {
        match f.fold_var(v, ParamKind::Row) {
            GenericArg::Row(sub) => {
                labels.extend(sub.labels);
                tail = sub.tail;
            }
            other => panic!("kind mismatch folding row variable: {other:?}"),
        }
    }
    let mut row = EffectRow { labels, tail };
    row.sort();
    if let Some(k) = row.duplicate_key() {
        return Err(DuplicateLabel(k));
    }
    Ok(row)
}

pub fn fold_trait_ref(t: &TraitRef, f: &mut dyn VarFolder) -> Result<TraitRef, DuplicateLabel> {
    Ok(TraitRef {
        trait_id: t.trait_id,
        self_ty: fold_ty(&t.self_ty, f)?,
        args: fold_args(&t.args, f)?,
    })
}

pub fn fold_projection(
    p: &Projection,
    f: &mut dyn VarFolder,
) -> Result<Projection, DuplicateLabel> {
    Ok(Projection {
        trait_ref: fold_trait_ref(&p.trait_ref, f)?,
        brand: fold_brand(&p.brand, f),
        assoc: p.assoc,
    })
}

pub fn fold_goal(g: &Goal, f: &mut dyn VarFolder) -> Result<Goal, DuplicateLabel> {
    Ok(match g {
        Goal::Implements { trait_ref, brand } => Goal::Implements {
            trait_ref: fold_trait_ref(trait_ref, f)?,
            brand: fold_brand(brand, f),
        },
        Goal::ProjectionEq { projection, ty } => Goal::ProjectionEq {
            projection: fold_projection(projection, f)?,
            ty: fold_ty(ty, f)?,
        },
        Goal::ModeBound { ty, bound } => Goal::ModeBound {
            ty: fold_ty(ty, f)?,
            bound: fold_mode(bound, f),
        },
    })
}

/// Visits every variable occurrence in traversal order.
pub fn visit_goal_vars(g: &Goal, mut visit: impl FnMut(Var, ParamKind)) {
    let mut f = |v: Var, k: ParamKind| {
        visit(v, k);
        GenericArg::var(k, v)
    };
    // Identity fold never introduces duplicates beyond those already present.
    let _ = fold_goal(g, &mut f);
}

pub fn visit_arg_vars(a: &GenericArg, mut visit: impl FnMut(Var, ParamKind)) {
    let mut f = |v: Var, k: ParamKind| {
        visit(v, k);
        GenericArg::var(k, v)
    };
    let _ = fold_arg(a, &mut f);
}

/// Substitutes `Var::Bound(i)` with `args[i]`.
pub fn instantiate_goal(g: &Goal, args: &[GenericArg]) -> Result<Goal, DuplicateLabel> {
    fold_goal(g, &mut BoundSubst(args))
}
pub fn instantiate_ty(t: &Ty, args: &[GenericArg]) -> Result<Ty, DuplicateLabel> {
    fold_ty(t, &mut BoundSubst(args))
}
pub fn instantiate_trait_ref(
    t: &TraitRef,
    args: &[GenericArg],
) -> Result<TraitRef, DuplicateLabel> {
    fold_trait_ref(t, &mut BoundSubst(args))
}
pub fn instantiate_args(
    a: &[GenericArg],
    args: &[GenericArg],
) -> Result<Vec<GenericArg>, DuplicateLabel> {
    fold_args(a, &mut BoundSubst(args))
}
pub fn instantiate_mode(m: &ModeTerm, args: &[GenericArg]) -> ModeTerm {
    fold_mode(m, &mut BoundSubst(args))
}
pub fn instantiate_brand(b: &Brand, args: &[GenericArg]) -> Brand {
    fold_brand(b, &mut BoundSubst(args))
}

struct BoundSubst<'a>(&'a [GenericArg]);
impl VarFolder for BoundSubst<'_> {
    fn fold_var(&mut self, var: Var, kind: ParamKind) -> GenericArg {
        match var {
            Var::Bound(i) => {
                let a = self.0[i as usize].clone();
                debug_assert_eq!(a.kind(), kind);
                a
            }
            other => GenericArg::var(kind, other),
        }
    }
}

pub fn ty_has_projection(t: &Ty) -> bool {
    match t {
        Ty::Ctor { args, .. } => args.iter().any(arg_has_projection),
        Ty::Var(_) => false,
        Ty::Projection(_) => true,
    }
}
pub fn arg_has_projection(a: &GenericArg) -> bool {
    match a {
        GenericArg::Ty(t) => ty_has_projection(t),
        GenericArg::Row(r) => r.labels.iter().any(|l| match l {
            EffectLabel::Effect { args, .. } => args.iter().any(arg_has_projection),
            EffectLabel::Capability(_) => false,
        }),
        _ => false,
    }
}
pub fn trait_ref_has_projection(t: &TraitRef) -> bool {
    ty_has_projection(&t.self_ty) || t.args.iter().any(arg_has_projection)
}

// ---------------------------------------------------------------------------
// Deterministic rendering (ids only; adapters render names).

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Mode::Copy => "copy",
            Mode::Affine => "affine",
            Mode::Linear => "linear",
        })
    }
}
impl fmt::Display for Var {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Var::Bound(i) => write!(f, "^{i}"),
            Var::Placeholder(i) => write!(f, "!{i}"),
            Var::Infer(i) => write!(f, "?{i}"),
        }
    }
}
fn write_list<T: fmt::Display>(f: &mut fmt::Formatter<'_>, items: &[T]) -> fmt::Result {
    for (i, a) in items.iter().enumerate() {
        if i > 0 {
            f.write_str(", ")?;
        }
        write!(f, "{a}")?;
    }
    Ok(())
}
impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Ctor { ctor, args } => {
                write!(f, "{ctor}")?;
                if !args.is_empty() {
                    f.write_str("[")?;
                    write_list(f, args)?;
                    f.write_str("]")?;
                }
                Ok(())
            }
            Ty::Var(v) => write!(f, "{v}"),
            Ty::Projection(p) => write!(f, "{p}"),
        }
    }
}
impl fmt::Display for ModeTerm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModeTerm::Const(m) => write!(f, "{m}"),
            ModeTerm::Var(v) => write!(f, "{v}"),
        }
    }
}
impl fmt::Display for EffectLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EffectLabel::Effect { effect, args } => {
                write!(f, "{effect}")?;
                if !args.is_empty() {
                    f.write_str("[")?;
                    write_list(f, args)?;
                    f.write_str("]")?;
                }
                Ok(())
            }
            EffectLabel::Capability(v) => write!(f, "cap {v}"),
        }
    }
}
impl fmt::Display for EffectRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("eff [")?;
        write_list(f, &self.labels)?;
        if let Some(t) = self.tail {
            if !self.labels.is_empty() {
                f.write_str(" ")?;
            }
            write!(f, "| {t}")?;
        }
        f.write_str("]")
    }
}
impl fmt::Display for Brand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Brand::Canonical => f.write_str("canonical"),
            Brand::Named(m) => write!(f, "{m}"),
            Brand::Var(v) => write!(f, "{v}"),
        }
    }
}
impl fmt::Display for GenericArg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GenericArg::Ty(t) => write!(f, "{t}"),
            GenericArg::Mode(m) => write!(f, "mod {m}"),
            GenericArg::Row(r) => write!(f, "{r}"),
            GenericArg::Brand(b) => write!(f, "brand {b}"),
            GenericArg::Capability(v) => write!(f, "cap {v}"),
        }
    }
}
impl fmt::Display for TraitRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.self_ty, self.trait_id)?;
        if !self.args.is_empty() {
            f.write_str("[")?;
            write_list(f, &self.args)?;
            f.write_str("]")?;
        }
        Ok(())
    }
}
impl fmt::Display for Projection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{} via {}>::{}", self.trait_ref, self.brand, self.assoc)
    }
}
impl fmt::Display for Goal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Goal::Implements { trait_ref, brand } => write!(f, "{trait_ref} via {brand}"),
            Goal::ProjectionEq { projection, ty } => write!(f, "{projection} == {ty}"),
            Goal::ModeBound { ty, bound } => write!(f, "mode({ty}) <= {bound}"),
        }
    }
}
