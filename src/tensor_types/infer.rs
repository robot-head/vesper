//! Shape inference and the static tensor rules over the surface module.
use super::dim::{Dim, DimError, display_shape, elements};
use super::types::{
    AdtKinds, Arg, DType, Kind, Scope, Ty, adt_kinds, collect_kinds, collect_row_kinds,
    resolve_kinds,
};
use super::wire::{BLOCK_ELEMENTS, QuantFormat, WireStruct};
use super::{
    ACTIVE_SEQ, BYTE_ARRAY, DIMS, KV_EFFECT, KV_ERROR, LEASE, RESERVED, SHAPE_TYPE, TENSOR,
    ZERO_PAD, codes, wire_integer,
};
use crate::ast::*;
use crate::check::Checked;
use std::collections::{BTreeMap, BTreeSet};

/// A requirement on dimensions that a function places on its callers.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Obligation {
    /// The dimension is even (RoPE rotates pairs).
    Even { dim: Dim, reason: String },
    /// `divisor` divides `value` (GQA grouping, whole quantized blocks).
    Divides {
        divisor: Dim,
        value: Dim,
        reason: String,
    },
}
impl Obligation {
    fn subst(&self, env: &BTreeMap<String, Dim>) -> Result<Self, DimError> {
        Ok(match self {
            Self::Even { dim, reason } => Self::Even {
                dim: dim.subst(env)?,
                reason: reason.clone(),
            },
            Self::Divides {
                divisor,
                value,
                reason,
            } => Self::Divides {
                divisor: divisor.subst(env)?,
                value: value.subst(env)?,
                reason: reason.clone(),
            },
        })
    }
    /// `Some(true)` when it holds for every value of the variables,
    /// `Some(false)` when it fails for constants, `None` when it depends on
    /// variables.
    pub fn decide(&self) -> Option<bool> {
        match self {
            Self::Even { dim, .. } => {
                if dim.coeff.is_multiple_of(2) {
                    Some(true)
                } else {
                    dim.as_const().map(|_| false)
                }
            }
            Self::Divides { divisor, value, .. } => {
                if divisor.divides(value) {
                    Some(true)
                } else if divisor.as_const().is_some() && value.as_const().is_some() {
                    Some(false)
                } else {
                    None
                }
            }
        }
    }
    /// Evaluate with concrete dimensions.
    pub fn holds(&self, env: &BTreeMap<String, u64>) -> Option<bool> {
        match self {
            Self::Even { dim, .. } => Some(dim.eval(env)?.is_multiple_of(2)),
            Self::Divides { divisor, value, .. } => {
                let d = divisor.eval(env)?;
                Some(d != 0 && value.eval(env)?.is_multiple_of(d))
            }
        }
    }
    fn variables(&self) -> Vec<String> {
        match self {
            Self::Even { dim, .. } => dim.variables().cloned().collect(),
            Self::Divides { divisor, value, .. } => divisor
                .variables()
                .chain(value.variables())
                .cloned()
                .collect(),
        }
    }
}
impl std::fmt::Display for Obligation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Even { dim, reason } => write!(f, "{reason}: {dim} must be even"),
            Self::Divides {
                divisor,
                value,
                reason,
            } => write!(f, "{reason}: {value} must be a multiple of {divisor}"),
        }
    }
}

/// A checked function signature.
#[derive(Clone, Debug)]
pub struct FnSig {
    pub name: String,
    pub generics: Vec<(String, Kind)>,
    pub params: Vec<(Pass, Ty)>,
    pub result: Ty,
    /// `KvCachePool[KvHeads, HeadDim]` in the effect row.
    pub pool: Option<(Dim, Dim)>,
    /// Inferred preconditions over the dimension parameters.
    pub obligations: BTreeSet<Obligation>,
    pub span: Span,
}
impl FnSig {
    pub fn dims(&self) -> impl Iterator<Item = &String> {
        self.generics
            .iter()
            .filter(|(_, k)| *k == Kind::Dim)
            .map(|(n, _)| n)
    }
}

/// Per-call elaboration facts, keyed by call span.
#[derive(Clone, Debug, PartialEq)]
pub enum Elab {
    /// Dimensions passed to a user function, in its dimension order.
    Dims(Vec<Dim>),
    /// Target shape of `reshape` or `fill`.
    Shape(Vec<Dim>),
}

#[derive(Clone, Debug)]
pub struct AdtInfo {
    pub kinds: Vec<Kind>,
    pub generics: Vec<String>,
    /// Variant name to fields.
    pub variants: Vec<(String, Vec<(String, Ty)>)>,
    pub is_enum: bool,
}

/// Result of [`analyze`].
#[derive(Clone, Debug)]
pub struct Analysis {
    /// The core checker's result on the elaborated module, or the tensor
    /// diagnostics when the tensor rules fail.
    pub checked: Checked,
    /// The tensor rules' own diagnostics.
    pub diagnostics: Vec<Diagnostic>,
    pub signatures: BTreeMap<String, FnSig>,
    pub wires: BTreeMap<String, WireStruct>,
    pub adt_kinds: AdtKinds,
    /// The module the core checker ran on.
    pub elaborated: Module,
}

pub(super) struct Context {
    pub adts: BTreeMap<String, AdtInfo>,
    pub kinds: AdtKinds,
    pub sigs: BTreeMap<String, FnSig>,
    pub blocks: BTreeSet<String>,
    pub wires: BTreeMap<String, WireStruct>,
    pub elab: BTreeMap<(usize, usize), Elab>,
}

fn reserved_or_wire(n: &str) -> bool {
    RESERVED.contains(&n)
        || n == SHAPE_TYPE
        || n == BYTE_ARRAY
        || n == ZERO_PAD
        || wire_integer(n).is_some()
}
fn type_mentions(t: &Type) -> bool {
    match t {
        Type::Named(n, a) => reserved_or_wire(n) || a.iter().any(type_mentions),
        Type::Tuple(ts) => ts.iter().any(type_mentions),
        Type::Function {
            params,
            result,
            effects,
            ..
        } => {
            params.iter().any(|(_, t)| type_mentions(t))
                || type_mentions(result)
                || row_mentions(effects)
        }
        Type::Row(e) => row_mentions(e),
        _ => false,
    }
}
fn row_mentions(e: &Effects) -> bool {
    e.instances
        .iter()
        .any(|i| RESERVED.contains(&i.name.as_str()) || i.args.iter().any(type_mentions))
}
fn path_mentions(n: &str) -> bool {
    RESERVED.contains(&n)
        || n.split_once("::")
            .is_some_and(|(p, _)| RESERVED.contains(&p))
}

/// Called with each expression, type and pattern of a body.
type Visitor<'a> = dyn FnMut(Option<&Expr>, Option<&Type>, Option<&Pattern>) + 'a;

/// Visit every type and expression of a function body.
fn walk(e: &Expr, f: &mut Visitor<'_>) {
    f(Some(e), None, None);
    match &e.kind {
        ExprKind::Literal(_) | ExprKind::Name(_) => {}
        ExprKind::Tuple(es) => es.iter().for_each(|x| walk(x, f)),
        ExprKind::Struct { fields, .. } => fields.iter().for_each(|(_, x)| walk(x, f)),
        ExprKind::Block { statements, tail } => {
            for s in statements {
                match s {
                    Statement::Let { pat, ty, value } => {
                        f(None, Some(ty), Some(pat));
                        walk_pattern(pat, f);
                        walk(value, f);
                    }
                    Statement::Expr(x) => walk(x, f),
                }
            }
            if let Some(t) = tail {
                walk(t, f);
            }
        }
        ExprKind::Call {
            callee,
            types,
            args,
        } => {
            walk(callee, f);
            types.iter().for_each(|t| f(None, Some(t), None));
            args.iter().for_each(|a| walk(&a.expr, f));
        }
        ExprKind::Field(x, _) | ExprKind::Try(x) | ExprKind::Unary(_, x) => walk(x, f),
        ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Assign(_, a, b) => {
            walk(a, f);
            walk(b, f);
        }
        ExprKind::If {
            condition,
            then,
            otherwise,
        } => {
            walk(condition, f);
            walk(then, f);
            if let Some(o) = otherwise {
                walk(o, f);
            }
        }
        ExprKind::Match { value, arms } => {
            walk(value, f);
            for (p, x) in arms {
                walk_pattern(p, f);
                walk(x, f);
            }
        }
        ExprKind::Closure {
            params,
            result,
            body,
            effects,
            ..
        } => {
            params.iter().for_each(|p| f(None, Some(&p.ty), None));
            f(None, Some(result), None);
            f(None, Some(&Type::Row(effects.clone())), None);
            walk(body, f);
        }
        ExprKind::Handle {
            effect,
            state,
            body,
            ret,
            ops,
        } => {
            f(
                None,
                Some(&Type::Row(Effects {
                    instances: vec![effect.clone()],
                    tail: None,
                })),
                None,
            );
            if let Some((p, x)) = state {
                walk_pattern(p, f);
                walk(x, f);
            }
            walk(body, f);
            for c in std::iter::once(ret.as_ref()).chain(ops) {
                c.pats.iter().for_each(|p| walk_pattern(p, f));
                walk(&c.body, f);
            }
        }
    }
}
fn walk_pattern(p: &Pattern, f: &mut Visitor<'_>) {
    f(None, None, Some(p));
    match &p.kind {
        PatternKind::Tuple(ps) => ps.iter().for_each(|q| walk_pattern(q, f)),
        PatternKind::Constructor { fields, .. } => {
            fields.iter().for_each(|(_, q)| walk_pattern(q, f))
        }
        _ => {}
    }
}

fn function_mentions(func: &Function) -> bool {
    let mut found = func.params.iter().any(|p| type_mentions(&p.ty))
        || type_mentions(&func.result)
        || row_mentions(&func.effects);
    if let Some(body) = &func.body {
        walk(body, &mut |e, t, p| {
            if let Some(e) = e {
                match &e.kind {
                    ExprKind::Name(n) | ExprKind::Struct { name: n, .. } if path_mentions(n) => {
                        found = true
                    }
                    _ => {}
                }
            }
            if t.is_some_and(type_mentions) {
                found = true;
            }
            if let Some(Pattern {
                kind: PatternKind::Constructor { name, .. },
                ..
            }) = p
                && path_mentions(name)
            {
                found = true;
            }
        });
    }
    found
}

/// Whether any item mentions tensor syntax.
pub fn mentions_tensors(module: &Module) -> bool {
    module.items.iter().any(|item| match item {
        Item::Adt(a) => a
            .variants
            .iter()
            .flat_map(|v| &v.fields)
            .any(|f| type_mentions(&f.ty)),
        Item::Function(f) => function_mentions(f),
        Item::Effect(e) => e.ops.iter().any(function_mentions),
        Item::Impl { methods, ty, .. } => {
            type_mentions(ty) || methods.iter().any(function_mentions)
        }
        Item::Conformance(c) => {
            c.methods.iter().any(function_mentions) || type_mentions(&c.self_ty)
        }
        Item::Trait(t) => t.methods.iter().any(function_mentions),
    })
}

fn diag(code: &str, message: impl Into<String>, span: Span) -> Diagnostic {
    Diagnostic::new(code, message, span)
}

/// Build the signature of a function (kinds from its signature).
fn signature(
    func: &Function,
    kinds: &AdtKinds,
    blocks: &BTreeSet<String>,
) -> Result<FnSig, Diagnostic> {
    let names: BTreeSet<String> = func
        .generics
        .iter()
        .filter(|g| !g.row)
        .map(|g| g.name.clone())
        .collect();
    let mut uses = BTreeMap::new();
    for p in &func.params {
        collect_kinds(&p.ty, &names, kinds, &mut uses);
    }
    collect_kinds(&func.result, &names, kinds, &mut uses);
    collect_row_kinds(&func.effects, &names, kinds, &mut uses);
    let generics: Vec<Generic> = func.generics.iter().filter(|g| !g.row).cloned().collect();
    let resolved =
        resolve_kinds(&generics, &uses).map_err(|e| diag(codes::KIND, e, func.span.clone()))?;
    let scope = Scope {
        kinds: generics
            .iter()
            .map(|g| g.name.clone())
            .zip(resolved.iter().copied())
            .collect(),
        adts: kinds,
        blocks,
    };
    let mut params = vec![];
    for p in &func.params {
        let t = scope.ty(&p.ty).map_err(|e| {
            diag(
                super::types::error_code(&e),
                format!("parameter '{}': {e}", p.name),
                p.span.clone(),
            )
        })?;
        params.push((p.pass, t));
    }
    let result = scope.ty(&func.result).map_err(|e| {
        diag(
            super::types::error_code(&e),
            format!("result of '{}': {e}", func.name),
            func.span.clone(),
        )
    })?;
    let mut pool = None;
    for e in &func.effects.instances {
        if e.name == KV_EFFECT && !e.args.is_empty() {
            let [kv, hd] = e.args.as_slice() else {
                return Err(diag(
                    codes::KIND,
                    "KvCachePool takes two dimensions: KvCachePool[KvHeads, HeadDim]",
                    func.span.clone(),
                ));
            };
            let d = |t| {
                scope
                    .dim(t)
                    .map_err(|e| diag(codes::KIND, e, func.span.clone()))
            };
            pool = Some((d(kv)?, d(hd)?));
        }
    }
    Ok(FnSig {
        name: func.name.clone(),
        generics: generics
            .iter()
            .map(|g| g.name.clone())
            .zip(resolved)
            .collect(),
        params,
        result,
        pool,
        obligations: BTreeSet::new(),
        span: func.span.clone(),
    })
}

/// Check the whole module, elaborate it and run the core checker.
pub fn analyze(module: &Module) -> Analysis {
    let mut diagnostics = vec![];
    let (wires, wire_errors) = super::wire::check(module);
    diagnostics.extend(wire_errors);
    let blocks: BTreeSet<String> = super::wire::wire_struct_names(module);
    let (kinds, kind_errors) = adt_kinds(module);
    for (e, s) in kind_errors {
        diagnostics.push(diag(codes::KIND, e, s));
    }
    // Unsupported item forms.
    for item in &module.items {
        match item {
            Item::Effect(e) => {
                if e.ops.iter().any(function_mentions)
                    || e.generics.iter().any(|g| kinds.contains_key(&g.name))
                {
                    diagnostics.push(diag(
                        codes::UNSUPPORTED,
                        format!(
                            "effect '{}' mentions tensor types; only top-level functions and type declarations may (this slice)",
                            e.name
                        ),
                        e.span.clone(),
                    ));
                }
            }
            Item::Impl { methods, ty, .. }
                if type_mentions(ty) || methods.iter().any(function_mentions) =>
            {
                let span = methods.first().map(|m| m.span.clone()).unwrap_or(0..0);
                diagnostics.push(diag(
                    codes::UNSUPPORTED,
                    "impl blocks may not mention tensor types; use top-level functions (this slice)",
                    span,
                ));
            }
            Item::Conformance(c)
                if c.methods.iter().any(function_mentions) || type_mentions(&c.self_ty) =>
            {
                diagnostics.push(diag(
                    codes::UNSUPPORTED,
                    "trait implementations may not mention tensor types (this slice)",
                    c.span.clone(),
                ));
            }
            Item::Trait(t) if t.methods.iter().any(function_mentions) => {
                diagnostics.push(diag(
                    codes::UNSUPPORTED,
                    "traits may not mention tensor types (this slice)",
                    t.span.clone(),
                ));
            }
            _ => {}
        }
    }
    // ADT field types.
    let mut adts = BTreeMap::new();
    for item in &module.items {
        let Item::Adt(a) = item else { continue };
        let k = kinds.get(&a.name).cloned().unwrap_or_default();
        let scope = Scope {
            kinds: a
                .generics
                .iter()
                .map(|g| g.name.clone())
                .zip(k.iter().copied())
                .collect(),
            adts: &kinds,
            blocks: &blocks,
        };
        let mut variants = vec![];
        for v in &a.variants {
            let mut fields = vec![];
            for f in &v.fields {
                if blocks.contains(&a.name) {
                    fields.push((f.name.clone(), Ty::Base("wire".into())));
                    continue;
                }
                match scope.ty(&f.ty) {
                    Ok(t) => {
                        if a.mode != Mode::Linear && matches!(t, Ty::Lease { .. } | Ty::Seq { .. })
                        {
                            diagnostics.push(diag(
                                codes::OPAQUE,
                                format!(
                                    "field '{}' holds a linear pool lease; '{}' must be a linear type",
                                    f.name, a.name
                                ),
                                a.span.clone(),
                            ));
                        }
                        fields.push((f.name.clone(), t))
                    }
                    Err(e) => {
                        diagnostics.push(diag(
                            super::types::error_code(&e),
                            format!("field '{}' of '{}': {e}", f.name, a.name),
                            a.span.clone(),
                        ));
                        fields.push((f.name.clone(), Ty::Unknown));
                    }
                }
            }
            variants.push((v.name.clone(), fields));
        }
        adts.insert(
            a.name.clone(),
            AdtInfo {
                kinds: k,
                generics: a.generics.iter().map(|g| g.name.clone()).collect(),
                variants,
                is_enum: a.is_enum,
            },
        );
    }
    // Signatures.
    let mut sigs = BTreeMap::new();
    for item in &module.items {
        let Item::Function(f) = item else { continue };
        match signature(f, &kinds, &blocks) {
            Ok(s) => {
                sigs.insert(f.name.clone(), s);
            }
            Err(d) => diagnostics.push(d),
        }
    }
    let mut cx = Context {
        adts,
        kinds,
        sigs,
        blocks,
        wires,
        elab: BTreeMap::new(),
    };
    // Bodies, to a fixed point of the inferred obligations.
    let functions: Vec<&Function> = module
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Function(f) if cx.sigs.contains_key(&f.name) => Some(f.as_ref()),
            _ => None,
        })
        .collect();
    let mut body_errors = vec![];
    for round in 0..=16 {
        body_errors.clear();
        cx.elab.clear();
        let mut changed = false;
        for f in &functions {
            let (obligations, errors) = check_body(&mut cx, f);
            body_errors.extend(errors);
            let sig = cx.sigs.get_mut(&f.name).expect("signature exists");
            if obligations.len() > 64 {
                body_errors.push(diag(
                    codes::OBLIGATION,
                    format!("'{}' infers more than 64 dimension preconditions", f.name),
                    f.span.clone(),
                ));
                continue;
            }
            if sig.obligations != obligations {
                sig.obligations = obligations;
                changed = true;
            }
        }
        if !changed {
            break;
        }
        if round == 16 {
            body_errors.push(diag(
                codes::OBLIGATION,
                "dimension preconditions did not converge across recursive calls",
                functions.first().map(|f| f.span.clone()).unwrap_or(0..0),
            ));
        }
    }
    diagnostics.extend(body_errors);
    let mut seen = BTreeSet::new();
    diagnostics
        .retain(|d| seen.insert((d.code.clone(), d.span.start, d.span.end, d.message.clone())));
    let elaborated = super::elaborate::elaborate(module, &cx);
    let checked = if diagnostics.is_empty() {
        let mut checked = crate::pact_frontend::check_module(&elaborated.module);
        for d in &mut checked.diagnostics {
            elaborated.remap(d);
        }
        checked
    } else {
        Checked {
            world: Default::default(),
            diagnostics: diagnostics.clone(),
            expression_types: Default::default(),
        }
    };
    Analysis {
        checked,
        diagnostics,
        signatures: cx.sigs,
        wires: cx.wires,
        adt_kinds: cx.kinds,
        elaborated: elaborated.module,
    }
}

#[derive(Clone, Default)]
struct Subst {
    dims: BTreeMap<String, Dim>,
    dtypes: BTreeMap<String, DType>,
    tys: BTreeMap<String, Ty>,
    pending: Vec<(Dim, Dim)>,
}
fn flexible(v: &str) -> bool {
    v.starts_with('?')
}
impl Subst {
    fn dim(&self, d: &Dim) -> Result<Dim, String> {
        let mut d = d.clone();
        for _ in 0..32 {
            let next = d.subst(&self.dims).map_err(|e| e.to_string())?;
            if next == d {
                return Ok(d);
            }
            d = next;
        }
        Err("dimension substitution does not terminate".into())
    }
    fn dtype(&self, d: &DType) -> DType {
        match d {
            DType::Var(v) => self.dtypes.get(v).cloned().unwrap_or_else(|| d.clone()),
            _ => d.clone(),
        }
    }
    fn ty(&self, t: &Ty) -> Result<Ty, String> {
        Ok(match t {
            Ty::Tensor { dtype, shape } => Ty::Tensor {
                dtype: self.dtype(dtype),
                shape: shape
                    .iter()
                    .map(|d| self.dim(d))
                    .collect::<Result<_, _>>()?,
            },
            Ty::Lease { kv, hd } => Ty::Lease {
                kv: self.dim(kv)?,
                hd: self.dim(hd)?,
            },
            Ty::Seq { kv, hd } => Ty::Seq {
                kv: self.dim(kv)?,
                hd: self.dim(hd)?,
            },
            Ty::Adt(n, args) => Ty::Adt(
                n.clone(),
                args.iter()
                    .map(|a| {
                        Ok(match a {
                            Arg::Ty(t) => Arg::Ty(self.ty(t)?),
                            Arg::Dim(d) => Arg::Dim(self.dim(d)?),
                            Arg::DType(d) => Arg::DType(self.dtype(d)),
                        })
                    })
                    .collect::<Result<_, String>>()?,
            ),
            Ty::Tuple(ts) => Ty::Tuple(ts.iter().map(|t| self.ty(t)).collect::<Result<_, _>>()?),
            Ty::Fn(ps, r) => Ty::Fn(
                ps.iter().map(|t| self.ty(t)).collect::<Result<_, _>>()?,
                Box::new(self.ty(r)?),
            ),
            Ty::Var(v) => match self.tys.get(v) {
                Some(Ty::Var(w)) if w == v => t.clone(),
                Some(t) => self.ty(t)?,
                None => t.clone(),
            },
            other => other.clone(),
        })
    }
    fn unify_dtype(&mut self, a: &DType, b: &DType) -> Result<(), (&'static str, String)> {
        let (a, b) = (self.dtype(a), self.dtype(b));
        match (&a, &b) {
            _ if a == b => Ok(()),
            (DType::Var(v), _) if flexible(v) => {
                self.dtypes.insert(v.clone(), b);
                Ok(())
            }
            (_, DType::Var(v)) if flexible(v) => {
                self.dtypes.insert(v.clone(), a);
                Ok(())
            }
            _ => Err((codes::DTYPE, format!("element type {a} differs from {b}"))),
        }
    }
    fn unify_dim(&mut self, a: &Dim, b: &Dim) -> Result<(), (&'static str, String)> {
        let e = |m: String| (codes::DIM, m);
        let (a, b) = (self.dim(a).map_err(e)?, self.dim(b).map_err(e)?);
        if a == b {
            return Ok(());
        }
        if self.solve(&a, &b) || self.solve(&b, &a) {
            return Ok(());
        }
        if a.variables().chain(b.variables()).any(|v| flexible(v)) {
            self.pending.push((a, b));
            return Ok(());
        }
        Err((codes::SHAPE, format!("{a} differs from {b}")))
    }
    /// Bind the single flexible variable of `a` so that `a == b`.
    fn solve(&mut self, a: &Dim, b: &Dim) -> bool {
        let flex: Vec<(&String, &u32)> = a.vars.iter().filter(|(v, _)| flexible(v)).collect();
        let [(v, 1)] = flex.as_slice() else {
            return false;
        };
        if b.mentions(v) {
            return false;
        }
        let mut rest = a.clone();
        rest.vars.remove(*v);
        if !rest.divides(b) {
            return false;
        }
        let mut value = b.clone();
        value.coeff /= rest.coeff;
        for (x, e) in &rest.vars {
            let entry = value.vars.get_mut(x).expect("divides checked the factor");
            *entry -= e;
            if *entry == 0 {
                value.vars.remove(x);
            }
        }
        self.dims.insert((*v).clone(), value);
        true
    }
    fn unify(&mut self, exp: &Ty, act: &Ty) -> Result<(), (&'static str, String)> {
        let e = |m: String| (codes::DIM, m);
        let (exp, act) = (self.ty(exp).map_err(e)?, self.ty(act).map_err(e)?);
        match (&exp, &act) {
            (Ty::Unknown, _) | (_, Ty::Unknown) => Ok(()),
            (Ty::Var(v), _) if flexible(v) => {
                self.tys.insert(v.clone(), act.clone());
                Ok(())
            }
            (_, Ty::Var(v)) if flexible(v) => {
                self.tys.insert(v.clone(), exp.clone());
                Ok(())
            }
            (
                Ty::Tensor {
                    dtype: d1,
                    shape: s1,
                },
                Ty::Tensor {
                    dtype: d2,
                    shape: s2,
                },
            ) => {
                self.unify_dtype(d1, d2)?;
                if s1.len() != s2.len() {
                    return Err((
                        codes::SHAPE,
                        format!("rank {} differs from rank {}", s1.len(), s2.len()),
                    ));
                }
                for (i, (a, b)) in s1.iter().zip(s2).enumerate() {
                    self.unify_dim(a, b)
                        .map_err(|(c, m)| (c, format!("axis {i}: {m}")))?;
                }
                Ok(())
            }
            (Ty::Lease { kv: a, hd: b }, Ty::Lease { kv: c, hd: d })
            | (Ty::Seq { kv: a, hd: b }, Ty::Seq { kv: c, hd: d }) => {
                self.unify_dim(a, c)
                    .map_err(|(c, m)| (c, format!("KvHeads: {m}")))?;
                self.unify_dim(b, d)
                    .map_err(|(c, m)| (c, format!("HeadDim: {m}")))
            }
            (Ty::Adt(n1, a1), Ty::Adt(n2, a2)) if n1 == n2 && a1.len() == a2.len() => {
                for (x, y) in a1.iter().zip(a2) {
                    match (x, y) {
                        (Arg::Ty(x), Arg::Ty(y)) => self.unify(x, y)?,
                        (Arg::Dim(x), Arg::Dim(y)) => self.unify_dim(x, y)?,
                        (Arg::DType(x), Arg::DType(y)) => self.unify_dtype(x, y)?,
                        _ => {}
                    }
                }
                Ok(())
            }
            (Ty::Tuple(a), Ty::Tuple(b)) if a.len() == b.len() => {
                for (x, y) in a.iter().zip(b) {
                    self.unify(x, y)?;
                }
                Ok(())
            }
            (Ty::Fn(a, r1), Ty::Fn(b, r2)) if a.len() == b.len() => {
                for (x, y) in a.iter().zip(b) {
                    self.unify(x, y)?;
                }
                self.unify(r1, r2)
            }
            (Ty::Var(a), Ty::Var(b)) if a == b => Ok(()),
            (x, y) if !x.has_tensor() && !y.has_tensor() => Ok(()),
            _ => Err((codes::SHAPE, String::new())),
        }
    }
    /// Retry pending equations; the first unsolved one is returned.
    fn settle(&mut self) -> Result<(), (&'static str, String)> {
        for _ in 0..16 {
            let pending = std::mem::take(&mut self.pending);
            if pending.is_empty() {
                return Ok(());
            }
            let before = pending.len();
            for (a, b) in pending {
                self.unify_dim(&a, &b)?;
            }
            if self.pending.len() >= before {
                break;
            }
        }
        match self.pending.first() {
            None => Ok(()),
            Some((a, b)) => Err((
                codes::INFER,
                format!(
                    "cannot solve {} = {}",
                    self.dim(a).unwrap_or(a.clone()),
                    self.dim(b).unwrap_or(b.clone())
                ),
            )),
        }
    }
}

struct Body<'a> {
    cx: &'a mut Context,
    sig: FnSig,
    scope_kinds: BTreeMap<String, Kind>,
    env: Vec<BTreeMap<String, Ty>>,
    obligations: BTreeSet<Obligation>,
    errors: Vec<Diagnostic>,
    fresh: usize,
}

fn check_body(cx: &mut Context, f: &Function) -> (BTreeSet<Obligation>, Vec<Diagnostic>) {
    let sig = cx.sigs[&f.name].clone();
    let scope_kinds = sig.generics.iter().cloned().collect();
    let mut body = Body {
        cx,
        sig: sig.clone(),
        scope_kinds,
        env: vec![BTreeMap::new()],
        obligations: BTreeSet::new(),
        errors: vec![],
        fresh: 0,
    };
    for (p, (_, t)) in f.params.iter().zip(&sig.params) {
        body.env[0].insert(p.name.clone(), t.clone());
    }
    if let Some(e) = &f.body {
        let t = body.expr(e, Some(&sig.result));
        body.expect(
            &sig.result,
            &t,
            e.span.clone(),
            &format!("body of '{}'", f.name),
        );
    }
    (body.obligations, body.errors)
}

impl Body<'_> {
    fn error(&mut self, code: &str, message: impl Into<String>, span: Span) {
        self.errors.push(diag(code, message, span));
    }
    fn scope(&self) -> Scope<'_> {
        Scope {
            kinds: self.scope_kinds.clone(),
            adts: &self.cx.kinds,
            blocks: &self.cx.blocks,
        }
    }
    fn lookup(&self, n: &str) -> Option<&Ty> {
        self.env.iter().rev().find_map(|s| s.get(n))
    }
    fn bind(&mut self, n: &str, t: Ty) {
        if let Some(s) = self.env.last_mut() {
            s.insert(n.to_string(), t);
        }
    }
    /// Report when `actual` cannot be used where `expected` is required.
    fn expect(&mut self, expected: &Ty, actual: &Ty, span: Span, context: &str) {
        let mut s = Subst::default();
        if let Err((code, reason)) = s.unify(expected, actual).and_then(|_| s.settle()) {
            let detail = if reason.is_empty() {
                String::new()
            } else {
                format!(" ({reason})")
            };
            self.error(
                code,
                format!("{context}: expected {expected}, found {actual}{detail}"),
                span,
            );
        }
    }
    fn require(&mut self, ob: Obligation, span: Span) {
        match ob.decide() {
            Some(true) => {}
            Some(false) => self.error(codes::OBLIGATION, ob.to_string(), span),
            None => {
                if ob
                    .variables()
                    .iter()
                    .all(|v| self.scope_kinds.get(v) == Some(&Kind::Dim))
                {
                    self.obligations.insert(ob);
                } else {
                    self.error(codes::INFER, format!("cannot decide: {ob}"), span);
                }
            }
        }
    }
    fn tensor(&mut self, t: &Ty, span: &Span, what: &str) -> Option<(DType, Vec<Dim>)> {
        match t {
            Ty::Tensor { dtype, shape } => Some((dtype.clone(), shape.clone())),
            Ty::Unknown => {
                self.error(
                    codes::INFER,
                    format!("cannot determine the shape of {what}; annotate it with a tensor type"),
                    span.clone(),
                );
                None
            }
            other => {
                self.error(
                    codes::SHAPE,
                    format!("{what} must be a tensor, found {other}"),
                    span.clone(),
                );
                None
            }
        }
    }
    fn f32(&mut self, d: &DType, span: &Span, what: &str) -> bool {
        if *d == DType::F32 {
            return true;
        }
        self.error(
            codes::DTYPE,
            format!("{what} must have element type F32, found {d}"),
            span.clone(),
        );
        false
    }
    fn expr(&mut self, e: &Expr, expected: Option<&Ty>) -> Ty {
        match &e.kind {
            ExprKind::Literal(l) => Ty::Base(
                match l {
                    Literal::Int(_) => "int",
                    Literal::Float(_) => "f64",
                    Literal::Bool(_) => "bool",
                    Literal::String(_) => "String",
                }
                .into(),
            ),
            ExprKind::Name(n) => match self.lookup(n) {
                Some(t) => t.clone(),
                None => self.function_value(n, e),
            },
            ExprKind::Tuple(es) => {
                let exp: Vec<Option<Ty>> = match expected {
                    Some(Ty::Tuple(ts)) if ts.len() == es.len() => {
                        ts.iter().cloned().map(Some).collect()
                    }
                    _ => vec![None; es.len()],
                };
                Ty::Tuple(
                    es.iter()
                        .zip(exp)
                        .map(|(x, t)| self.expr(x, t.as_ref()))
                        .collect(),
                )
            }
            ExprKind::Struct { name, fields } => self.struct_literal(e, name, fields, expected),
            ExprKind::Block { statements, tail } => {
                self.env.push(BTreeMap::new());
                for s in statements {
                    match s {
                        Statement::Let { pat, ty, value } => self.let_statement(pat, ty, value),
                        Statement::Expr(x) => {
                            self.expr(x, None);
                        }
                    }
                }
                let t = match tail {
                    Some(t) => self.expr(t, expected),
                    None => Ty::unit(),
                };
                self.env.pop();
                t
            }
            ExprKind::Call {
                callee,
                types,
                args,
            } => self.call(e, callee, types, args, expected),
            ExprKind::Field(x, f) => {
                let t = self.expr(x, None);
                self.field(&t, f, e.span.clone())
            }
            ExprKind::Index(a, b) => {
                let t = self.expr(a, None);
                self.expr(b, None);
                if t.has_tensor() {
                    self.error(
                        codes::UNSUPPORTED,
                        "tensors are not indexed with []: use Tensor operations",
                        e.span.clone(),
                    );
                }
                Ty::Unknown
            }
            ExprKind::Try(x) => match self.expr(x, None) {
                Ty::Adt(n, args) if (n == "Result" || n == "Option") && !args.is_empty() => {
                    match &args[0] {
                        Arg::Ty(t) => t.clone(),
                        _ => Ty::Unknown,
                    }
                }
                _ => Ty::Unknown,
            },
            ExprKind::Unary(_, x) => {
                let t = self.expr(x, None);
                self.no_operator(&t, e.span.clone());
                Ty::Unknown
            }
            ExprKind::Binary(op, a, b) => {
                let (ta, tb) = (self.expr(a, None), self.expr(b, None));
                self.no_operator(&ta, e.span.clone());
                self.no_operator(&tb, e.span.clone());
                if matches!(
                    op.as_str(),
                    "==" | "!=" | "<" | "<=" | ">" | ">=" | "&&" | "||"
                ) {
                    Ty::Base("bool".into())
                } else {
                    Ty::Unknown
                }
            }
            ExprKind::Assign(_, target, value) => {
                let t = self.expr(target, None);
                let v = self.expr(value, Some(&t));
                self.expect(&t, &v, value.span.clone(), "assignment");
                Ty::unit()
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                self.expr(condition, None);
                let a = self.expr(then, expected);
                match otherwise {
                    Some(o) => {
                        let b = self.expr(o, expected.or(Some(&a)));
                        self.join(a, b, o.span.clone(), "the else branch")
                    }
                    None => Ty::unit(),
                }
            }
            ExprKind::Match { value, arms } => {
                let scrutinee = self.expr(value, None);
                let mut result: Option<Ty> = None;
                for (p, body) in arms {
                    self.env.push(BTreeMap::new());
                    self.pattern(p, &scrutinee);
                    let t = self.expr(body, expected.or(result.as_ref()));
                    self.env.pop();
                    result = Some(match result {
                        None => t,
                        Some(prev) => self.join(prev, t, body.span.clone(), "this match arm"),
                    });
                }
                result.unwrap_or(Ty::Unknown)
            }
            ExprKind::Closure {
                params,
                result,
                body,
                ..
            } => {
                self.env.push(BTreeMap::new());
                let mut ps = vec![];
                for p in params {
                    let t = match self.scope().ty(&p.ty) {
                        Ok(t) => t,
                        Err(m) => {
                            self.error(codes::KIND, m, p.span.clone());
                            Ty::Unknown
                        }
                    };
                    self.bind(&p.name, t.clone());
                    ps.push(t);
                }
                let r = self.scope().ty(result).unwrap_or(Ty::Unknown);
                let b = self.expr(body, Some(&r));
                self.expect(&r, &b, body.span.clone(), "closure body");
                self.env.pop();
                let r = if r == Ty::Unknown { b } else { r };
                Ty::Fn(ps, Box::new(r))
            }
            ExprKind::Handle {
                effect,
                state,
                body,
                ret,
                ops,
            } => {
                if effect.name == TENSOR || effect.name == KV_EFFECT {
                    self.error(
                        codes::HANDLER,
                        format!(
                            "'{}' is implemented by the host; a source handler could forge or drop tensors and leases",
                            effect.name
                        ),
                        e.span.clone(),
                    );
                }
                self.env.push(BTreeMap::new());
                if let Some((p, x)) = state {
                    let t = self.expr(x, None);
                    self.pattern(p, &t);
                }
                let body_ty = self.expr(body, None);
                for (i, c) in std::iter::once(ret.as_ref()).chain(ops).enumerate() {
                    self.env.push(BTreeMap::new());
                    for p in &c.pats {
                        self.pattern(p, if i == 0 { &body_ty } else { &Ty::Unknown });
                    }
                    self.expr(&c.body, None);
                    self.env.pop();
                }
                self.env.pop();
                Ty::Unknown
            }
        }
    }
    fn no_operator(&mut self, t: &Ty, span: Span) {
        if matches!(t, Ty::Tensor { .. }) {
            self.error(
                codes::UNSUPPORTED,
                "tensor arithmetic uses Tensor::add, Tensor::mul and Tensor::matmul, not operators",
                span,
            );
        }
    }
    fn join(&mut self, a: Ty, b: Ty, span: Span, what: &str) -> Ty {
        if a == Ty::Unknown {
            return b;
        }
        self.expect(&a, &b, span, what);
        a
    }
    fn let_statement(&mut self, pat: &Pattern, ty: &Type, value: &Expr) {
        let annotated = if *ty == Type::Infer {
            None
        } else {
            match self.scope().ty(ty) {
                Ok(t) => Some(t),
                Err(m) => {
                    self.error(super::types::error_code(&m), m, pat.span.clone());
                    Some(Ty::Unknown)
                }
            }
        };
        let v = self.expr(value, annotated.as_ref());
        let t = match annotated {
            Some(a) => {
                self.expect(&a, &v, value.span.clone(), "let binding");
                if a == Ty::Unknown { v } else { a }
            }
            None => v,
        };
        self.pattern(pat, &t);
    }
    fn opaque(&mut self, name: &str, span: Span, what: &str) -> bool {
        let base = name.rsplit("::").next().unwrap_or(name);
        if [TENSOR, LEASE, ACTIVE_SEQ, DIMS].contains(&base) {
            self.error(
                codes::OPAQUE,
                format!(
                    "'{base}' values are created and consumed only by Tensor and KvCachePool operations; a source program cannot {what} one"
                ),
                span,
            );
            return true;
        }
        false
    }
    fn pattern(&mut self, p: &Pattern, t: &Ty) {
        match &p.kind {
            PatternKind::Wild | PatternKind::Literal(_) => {}
            PatternKind::Bind(n) => self.bind(n, t.clone()),
            PatternKind::Tuple(ps) => {
                for (i, q) in ps.iter().enumerate() {
                    let ti = match t {
                        Ty::Tuple(ts) => ts.get(i).cloned().unwrap_or(Ty::Unknown),
                        _ => Ty::Unknown,
                    };
                    self.pattern(q, &ti);
                }
            }
            PatternKind::Constructor { name, fields, .. } => {
                if self.opaque(name, p.span.clone(), "destructure") {
                    return;
                }
                let fields_ty = self.variant_fields(name, t);
                for (f, q) in fields {
                    let ft = fields_ty
                        .iter()
                        .find(|(n, _)| n == f)
                        .map(|(_, t)| t.clone())
                        .unwrap_or(Ty::Unknown);
                    self.pattern(q, &ft);
                }
            }
        }
    }
    /// Field types of variant `name` of the scrutinee type `t`.
    fn variant_fields(&self, name: &str, t: &Ty) -> Vec<(String, Ty)> {
        let variant = name.rsplit("::").next().unwrap_or(name);
        let arg = |i: usize| match t {
            Ty::Adt(_, a) => match a.get(i) {
                Some(Arg::Ty(t)) => t.clone(),
                _ => Ty::Unknown,
            },
            _ => Ty::Unknown,
        };
        match (t, variant) {
            (Ty::Adt(n, _), "Some") if n == "Option" => vec![("0".into(), arg(0))],
            (Ty::Adt(n, _), "Ok") if n == "Result" => vec![("0".into(), arg(0))],
            (Ty::Adt(n, _), "Err") if n == "Result" => vec![("0".into(), arg(1))],
            (Ty::Adt(n, args), _) => {
                let Some(info) = self.cx.adts.get(n) else {
                    return vec![];
                };
                let Some((_, fields)) = info.variants.iter().find(|(v, _)| v == variant) else {
                    return vec![];
                };
                let s = adt_subst(info, args);
                fields
                    .iter()
                    .map(|(f, ft)| (f.clone(), s.ty(ft).unwrap_or(Ty::Unknown)))
                    .collect()
            }
            _ => vec![],
        }
    }
    fn field(&mut self, t: &Ty, f: &str, span: Span) -> Ty {
        match t {
            Ty::Tensor { .. } | Ty::Lease { .. } | Ty::Seq { .. } => {
                let name = match t {
                    Ty::Tensor { .. } => TENSOR,
                    Ty::Lease { .. } => LEASE,
                    _ => ACTIVE_SEQ,
                };
                self.opaque(name, span, "read a field of");
                Ty::Unknown
            }
            Ty::Tuple(ts) => f
                .parse::<usize>()
                .ok()
                .and_then(|i| ts.get(i).cloned())
                .unwrap_or(Ty::Unknown),
            Ty::Adt(n, args) => {
                let Some(info) = self.cx.adts.get(n) else {
                    return Ty::Unknown;
                };
                if info.is_enum {
                    return Ty::Unknown;
                }
                let s = adt_subst(info, args);
                info.variants
                    .first()
                    .and_then(|(_, fs)| fs.iter().find(|(x, _)| x == f))
                    .map(|(_, ft)| s.ty(ft).unwrap_or(Ty::Unknown))
                    .unwrap_or(Ty::Unknown)
            }
            _ => Ty::Unknown,
        }
    }
    fn fresh_generics(&mut self, generics: &[(String, Kind)]) -> (Subst, Vec<(String, String)>) {
        let mut s = Subst::default();
        let mut names = vec![];
        for (g, k) in generics {
            self.fresh += 1;
            let v = format!("?{}", self.fresh);
            match k {
                Kind::Dim => {
                    s.dims.insert(g.clone(), Dim::var(&v));
                }
                Kind::DType => {
                    s.dtypes.insert(g.clone(), DType::Var(v.clone()));
                }
                Kind::Type => {
                    s.tys.insert(g.clone(), Ty::Var(v.clone()));
                }
            }
            names.push((g.clone(), v));
        }
        (s, names)
    }
    fn struct_literal(
        &mut self,
        e: &Expr,
        name: &str,
        fields: &[(String, Expr)],
        expected: Option<&Ty>,
    ) -> Ty {
        if self.opaque(name, e.span.clone(), "construct") {
            for (_, x) in fields {
                self.expr(x, None);
            }
            return Ty::Unknown;
        }
        let (adt, variant) = match name.split_once("::") {
            Some((a, v)) => (a.to_string(), v.to_string()),
            None => (name.to_string(), name.to_string()),
        };
        let adt = if self.cx.adts.contains_key(&adt) {
            adt
        } else {
            match self
                .cx
                .adts
                .iter()
                .find(|(_, i)| i.variants.iter().any(|(v, _)| *v == variant))
            {
                Some((n, _)) => n.clone(),
                None => {
                    for (_, x) in fields {
                        self.expr(x, None);
                    }
                    return Ty::Unknown;
                }
            }
        };
        let info = self.cx.adts[&adt].clone();
        let generics: Vec<(String, Kind)> = info
            .generics
            .iter()
            .cloned()
            .zip(info.kinds.iter().copied())
            .collect();
        let (renaming, fresh) = self.fresh_generics(&generics);
        let declared = Ty::Adt(
            adt.clone(),
            generics
                .iter()
                .map(|(g, k)| match k {
                    Kind::Dim => Arg::Dim(renaming.dims[g].clone()),
                    Kind::DType => Arg::DType(renaming.dtypes[g].clone()),
                    Kind::Type => Arg::Ty(renaming.tys[g].clone()),
                })
                .collect(),
        );
        let mut s = Subst::default();
        if let Some(exp) = expected {
            let mut probe = s.clone();
            if probe.unify(&declared, exp).is_ok() {
                s = probe;
            }
        }
        let field_types = info
            .variants
            .iter()
            .find(|(v, _)| *v == variant)
            .map(|(_, f)| f.clone())
            .unwrap_or_default();
        for (f, x) in fields {
            let ft = field_types
                .iter()
                .find(|(n, _)| n == f)
                .map(|(_, t)| renaming.ty(t).unwrap_or(Ty::Unknown))
                .unwrap_or(Ty::Unknown);
            let want = s.ty(&ft).unwrap_or(Ty::Unknown);
            let got = self.expr(x, Some(&want));
            if let Err((code, reason)) = s.unify(&ft, &got) {
                let want = s.ty(&ft).unwrap_or(ft.clone());
                self.error(
                    code,
                    format!("field '{f}' of '{adt}': expected {want}, found {got} ({reason})"),
                    x.span.clone(),
                );
            }
        }
        if let Err((code, reason)) = s.settle() {
            self.error(code, format!("'{adt}' literal: {reason}"), e.span.clone());
        }
        for (g, v) in &fresh {
            if generics.iter().any(|(n, k)| n == g && *k == Kind::Dim)
                && !s.dims.contains_key(v)
                && field_types.iter().any(|(_, t)| t.has_tensor())
            {
                self.error(
                    codes::INFER,
                    format!("cannot infer dimension '{g}' of '{adt}'; annotate the binding"),
                    e.span.clone(),
                );
            }
        }
        s.ty(&declared).unwrap_or(Ty::Unknown)
    }
    fn args(&mut self, args: &[Arg2<'_>]) -> Vec<Ty> {
        args.iter().map(|a| self.expr(a.0, None)).collect()
    }
    /// A call through a function value. Its tensor shapes are checked
    /// against the value's `fn(..) -> ..` type; a callee whose type is not
    /// known cannot take or return tensors, because erasure would hide a
    /// mismatch from the core checker.
    /// A top-level function named as a value: its `fn(..) -> ..` type. A
    /// generic function is instantiated (and receives hidden dimension
    /// arguments) at each direct call, so it cannot be a value.
    fn function_value(&mut self, n: &str, e: &Expr) -> Ty {
        let Some(sig) = self.cx.sigs.get(n) else {
            return Ty::Unknown;
        };
        let tensorish = sig.params.iter().any(|(_, t)| t.has_tensor()) || sig.result.has_tensor();
        if !tensorish {
            return Ty::Unknown;
        }
        if !sig.generics.is_empty() {
            self.error(
                codes::UNSUPPORTED,
                format!(
                    "'{n}' has generic parameters and cannot be used as a value; \
                     call it directly"
                ),
                e.span.clone(),
            );
            return Ty::Unknown;
        }
        Ty::Fn(
            sig.params.iter().map(|(_, t)| t.clone()).collect(),
            Box::new(sig.result.clone()),
        )
    }
    fn indirect(
        &mut self,
        e: &Expr,
        f: &Ty,
        args: &[crate::ast::Arg],
        expected: Option<&Ty>,
    ) -> Ty {
        if let Ty::Fn(ps, r) = f
            && ps.len() == args.len()
        {
            for (p, a) in ps.iter().zip(args) {
                let t = self.expr(&a.expr, Some(p));
                self.expect(p, &t, a.expr.span.clone(), "argument");
            }
            return (**r).clone();
        }
        let a: Vec<Arg2> = args.iter().map(|a| Arg2(&a.expr)).collect();
        let tys = self.args(&a);
        if tys.iter().any(Ty::has_tensor) || expected.is_some_and(Ty::has_tensor) {
            self.error(
                codes::UNSUPPORTED,
                "a call through a function value whose type is not known cannot pass or \
                 return tensors; annotate the function value with a `fn(..) -> ..` type"
                    .to_string(),
                e.span.clone(),
            );
        }
        Ty::Unknown
    }
    fn call(
        &mut self,
        e: &Expr,
        callee: &Expr,
        types: &[Type],
        args: &[crate::ast::Arg],
        expected: Option<&Ty>,
    ) -> Ty {
        let ExprKind::Name(name) = &callee.kind else {
            let f = self.expr(callee, None);
            return self.indirect(e, &f, args, expected);
        };
        if let Some(op) = name.strip_prefix("Tensor::") {
            return self.tensor_op(e, op, types, args, expected);
        }
        if let Some(op) = name.strip_prefix("KvCachePool::") {
            return self.pool_op(e, op, args, expected);
        }
        if let Some(f) = self.lookup(name).cloned() {
            return self.indirect(e, &f, args, expected);
        }
        match name.as_str() {
            "Some" | "Ok" | "Err" if args.len() == 1 => {
                let inner_expected = match (name.as_str(), expected) {
                    ("Some" | "Ok", Some(Ty::Adt(_, a))) => match a.first() {
                        Some(Arg::Ty(t)) => Some(t.clone()),
                        _ => None,
                    },
                    ("Err", Some(Ty::Adt(_, a))) => match a.get(1) {
                        Some(Arg::Ty(t)) => Some(t.clone()),
                        _ => None,
                    },
                    _ => None,
                };
                let t = self.expr(&args[0].expr, inner_expected.as_ref());
                let other = |i: usize| match expected {
                    Some(Ty::Adt(_, a)) => match a.get(i) {
                        Some(Arg::Ty(t)) => t.clone(),
                        _ => Ty::Unknown,
                    },
                    _ => Ty::Unknown,
                };
                return match name.as_str() {
                    "Some" => Ty::Adt("Option".into(), vec![Arg::Ty(t)]),
                    "Ok" => Ty::Adt("Result".into(), vec![Arg::Ty(t), Arg::Ty(other(1))]),
                    _ => Ty::Adt("Result".into(), vec![Arg::Ty(other(0)), Arg::Ty(t)]),
                };
            }
            _ => {}
        }
        if let Some(sig) = self.cx.sigs.get(name).cloned() {
            return self.apply(e, &sig, types, args, expected);
        }
        // Tuple-variant constructor of a user enum.
        let variant = name.rsplit("::").next().unwrap_or(name);
        if let Some((adt, info)) = self
            .cx
            .adts
            .iter()
            .find(|(_, i)| i.is_enum && i.variants.iter().any(|(v, _)| v == variant))
            .map(|(n, i)| (n.clone(), i.clone()))
        {
            let fields = info
                .variants
                .iter()
                .find(|(v, _)| v == variant)
                .map(|(_, f)| f.clone())
                .unwrap_or_default();
            let generics: Vec<(String, Kind)> = info
                .generics
                .iter()
                .cloned()
                .zip(info.kinds.iter().copied())
                .collect();
            let sig = FnSig {
                name: name.clone(),
                generics: generics.clone(),
                params: fields.into_iter().map(|(_, t)| (Pass::In, t)).collect(),
                result: Ty::Adt(
                    adt,
                    generics
                        .iter()
                        .map(|(g, k)| match k {
                            Kind::Dim => Arg::Dim(Dim::var(g)),
                            Kind::DType => Arg::DType(DType::Var(g.clone())),
                            Kind::Type => Arg::Ty(Ty::Var(g.clone())),
                        })
                        .collect(),
                ),
                pool: None,
                obligations: BTreeSet::new(),
                span: e.span.clone(),
            };
            let t = self.apply(e, &sig, &[], args, expected);
            // Constructors take no hidden dimensions.
            self.cx.elab.remove(&(e.span.start, e.span.end));
            return t;
        }
        let a: Vec<Arg2> = args.iter().map(|a| Arg2(&a.expr)).collect();
        self.args(&a);
        Ty::Unknown
    }
    /// Instantiate `sig` at a call.
    fn apply(
        &mut self,
        e: &Expr,
        sig: &FnSig,
        types: &[Type],
        args: &[crate::ast::Arg],
        expected: Option<&Ty>,
    ) -> Ty {
        let (renaming, fresh) = self.fresh_generics(&sig.generics);
        let mut s = Subst::default();
        if !types.is_empty() && types.len() == sig.generics.len() {
            for ((g, k), t) in sig.generics.iter().zip(types) {
                let v = &fresh.iter().find(|(n, _)| n == g).expect("fresh").1;
                let scope = self.scope();
                let bound = match k {
                    Kind::Dim => scope.dim(t).map(|d| {
                        s.dims.insert(v.clone(), d);
                    }),
                    Kind::DType => scope.dtype(t).map(|d| {
                        s.dtypes.insert(v.clone(), d);
                    }),
                    Kind::Type => scope.ty(t).map(|d| {
                        s.tys.insert(v.clone(), d);
                    }),
                };
                if let Err(m) = bound {
                    self.error(codes::KIND, m, e.span.clone());
                }
            }
        }
        let result = renaming.ty(&sig.result).unwrap_or(Ty::Unknown);
        if let Some(exp) = expected {
            let mut probe = s.clone();
            if probe.unify(&result, exp).is_ok() {
                s = probe;
            }
        }
        if let (Some((h, d)), Some((ch, cd))) = (&sig.pool, &self.sig.pool) {
            let h = renaming.dim(h).unwrap_or(h.clone());
            let d = renaming.dim(d).unwrap_or(d.clone());
            if s.unify_dim(&h, ch)
                .and_then(|_| s.unify_dim(&d, cd))
                .is_err()
            {
                self.error(
                    codes::POOL,
                    format!(
                        "'{}' uses KvCachePool[{}, {}] but the caller's row has KvCachePool[{ch}, {cd}]",
                        sig.name,
                        s.dim(&h).unwrap_or(h.clone()),
                        s.dim(&d).unwrap_or(d.clone())
                    ),
                    e.span.clone(),
                );
            }
        }
        for (i, a) in args.iter().enumerate() {
            let Some((_, pt)) = sig.params.get(i) else {
                self.expr(&a.expr, None);
                continue;
            };
            let pt = renaming.ty(pt).unwrap_or(Ty::Unknown);
            let want = s.ty(&pt).unwrap_or(Ty::Unknown);
            let got = self.expr(&a.expr, Some(&want));
            if pt.has_tensor() && got == Ty::Unknown {
                self.error(
                    codes::INFER,
                    format!(
                        "cannot determine the shape of argument {} of '{}'; annotate it",
                        i + 1,
                        sig.name
                    ),
                    a.expr.span.clone(),
                );
                continue;
            }
            if let Err((code, reason)) = s.unify(&pt, &got) {
                let want = s.ty(&pt).unwrap_or(pt.clone());
                let detail = if reason.is_empty() {
                    String::new()
                } else {
                    format!(" ({reason})")
                };
                self.error(
                    code,
                    format!(
                        "argument {} of '{}': expected {want}, found {got}{detail}",
                        i + 1,
                        sig.name
                    ),
                    a.expr.span.clone(),
                );
            }
        }
        if let Err((code, reason)) = s.settle() {
            self.error(
                code,
                format!("call of '{}': {reason}", sig.name),
                e.span.clone(),
            );
        }
        // Instantiated dimensions, in the callee's order.
        let mut dims = vec![];
        let mut env = BTreeMap::new();
        for (g, k) in &sig.generics {
            if *k != Kind::Dim {
                continue;
            }
            let v = &fresh.iter().find(|(n, _)| n == g).expect("fresh").1;
            match s.dim(&Dim::var(v)) {
                Ok(d) if !d.variables().any(|x| flexible(x)) => {
                    env.insert(g.clone(), d.clone());
                    dims.push(d);
                }
                _ => {
                    self.error(
                        codes::INFER,
                        format!(
                            "cannot infer dimension '{g}' of '{}'; annotate an argument or the result",
                            sig.name
                        ),
                        e.span.clone(),
                    );
                    return Ty::Unknown;
                }
            }
        }
        for ob in &sig.obligations {
            match ob.subst(&env) {
                Ok(ob) => {
                    let span = e.span.clone();
                    if ob.decide() == Some(false) {
                        self.error(
                            codes::OBLIGATION,
                            format!("call of '{}' violates its precondition: {ob}", sig.name),
                            span,
                        );
                    } else {
                        self.require(ob, span);
                    }
                }
                Err(m) => self.error(codes::DIM, m.to_string(), e.span.clone()),
            }
        }
        self.cx
            .elab
            .insert((e.span.start, e.span.end), Elab::Dims(dims));
        let mut out = s.ty(&result).unwrap_or(Ty::Unknown);
        // Unsolved ordinary type parameters are left to the core checker.
        strip_flexible(&mut out);
        out
    }
    fn arity(&mut self, e: &Expr, op: &str, args: &[crate::ast::Arg], n: usize) -> bool {
        if args.len() == n {
            return true;
        }
        for a in args {
            self.expr(&a.expr, None);
        }
        self.error(
            codes::SHAPE,
            format!("Tensor::{op} takes {n} argument(s), found {}", args.len()),
            e.span.clone(),
        );
        false
    }
    fn target_shape(
        &mut self,
        e: &Expr,
        op: &str,
        types: &[Type],
        expected: Option<&Ty>,
    ) -> Option<Vec<Dim>> {
        match types {
            [t] => match self.scope().shape(t) {
                Ok(s) => Some(s),
                Err(m) => {
                    self.error(codes::KIND, m, e.span.clone());
                    None
                }
            },
            [] => match expected {
                Some(Ty::Tensor { shape, .. }) => Some(shape.clone()),
                _ => {
                    self.error(
                        codes::INFER,
                        format!("Tensor::{op} needs a target shape: Tensor::{op}[[d, ...]](...)"),
                        e.span.clone(),
                    );
                    None
                }
            },
            _ => {
                self.error(
                    codes::KIND,
                    format!("Tensor::{op} takes one shape argument"),
                    e.span.clone(),
                );
                None
            }
        }
    }
    fn tensor_op(
        &mut self,
        e: &Expr,
        op: &str,
        types: &[Type],
        args: &[crate::ast::Arg],
        expected: Option<&Ty>,
    ) -> Ty {
        let span = e.span.clone();
        let n = match op {
            "matmul" | "matvec" | "add" | "mul" | "swiglu" | "qmatvec" | "rope" => 2,
            "reshape" | "transpose" | "softmax" | "silu" | "to_bf16" | "to_f32" | "argmax"
            | "fill" => 1,
            "rms_norm" | "block_scale" => 3,
            _ => {
                for a in args {
                    self.expr(&a.expr, None);
                }
                self.error(
                    codes::UNSUPPORTED,
                    format!("unknown tensor operation 'Tensor::{op}'"),
                    span,
                );
                return Ty::Unknown;
            }
        };
        if op != "reshape" && op != "fill" && !types.is_empty() {
            self.error(
                codes::KIND,
                format!("Tensor::{op} takes no type arguments"),
                span.clone(),
            );
        }
        if !self.arity(e, op, args, n) {
            return Ty::Unknown;
        }
        let a: Vec<Arg2> = args.iter().map(|a| Arg2(&a.expr)).collect();
        if op == "fill" {
            self.expr(a[0].0, None);
            let Some(shape) = self.target_shape(e, op, types, expected) else {
                return Ty::Unknown;
            };
            self.cx
                .elab
                .insert((span.start, span.end), Elab::Shape(shape.clone()));
            return Ty::Tensor {
                dtype: DType::F32,
                shape,
            };
        }
        let tys = self.args(&a);
        let sp = |i: usize| a[i].0.span.clone();
        let tensor = |b: &mut Self, i: usize| {
            let what = format!("argument {} of Tensor::{op}", i + 1);
            b.tensor(&tys[i], &sp(i), &what)
        };
        let f32 = |b: &mut Self, i: usize, d: &DType| {
            b.f32(d, &sp(i), &format!("argument {} of Tensor::{op}", i + 1))
        };
        let mismatch = |b: &mut Self, m: String| {
            b.error(codes::SHAPE, format!("Tensor::{op}: {m}"), span.clone());
            Ty::Unknown
        };
        match op {
            "matmul" | "matvec" => {
                let (Some((da, sa)), Some((db, sb))) = (tensor(self, 0), tensor(self, 1)) else {
                    return Ty::Unknown;
                };
                if !f32(self, 0, &da) || !f32(self, 1, &db) {
                    return Ty::Unknown;
                }
                let rb = if op == "matmul" { 2 } else { 1 };
                if sa.len() != 2 || sb.len() != rb {
                    return mismatch(
                        self,
                        format!(
                            "expected ranks 2 and {rb}, found {} and {}",
                            display_shape(&sa),
                            display_shape(&sb)
                        ),
                    );
                }
                if sa[1] != sb[0] {
                    return mismatch(
                        self,
                        format!(
                            "inner dimensions differ: {} has {} columns but {} has {} rows",
                            display_shape(&sa),
                            sa[1],
                            display_shape(&sb),
                            sb[0]
                        ),
                    );
                }
                let mut shape = vec![sa[0].clone()];
                if op == "matmul" {
                    shape.push(sb[1].clone());
                }
                Ty::Tensor {
                    dtype: DType::F32,
                    shape,
                }
            }
            "add" | "mul" => {
                let (Some((da, sa)), Some((db, sb))) = (tensor(self, 0), tensor(self, 1)) else {
                    return Ty::Unknown;
                };
                if !f32(self, 0, &da) || !f32(self, 1, &db) {
                    return Ty::Unknown;
                }
                match broadcast(&sa, &sb) {
                    Ok(shape) => Ty::Tensor {
                        dtype: DType::F32,
                        shape,
                    },
                    Err(m) => mismatch(self, m),
                }
            }
            "swiglu" => {
                let (Some((da, sa)), Some((db, sb))) = (tensor(self, 0), tensor(self, 1)) else {
                    return Ty::Unknown;
                };
                if !f32(self, 0, &da) || !f32(self, 1, &db) {
                    return Ty::Unknown;
                }
                if sa != sb {
                    return mismatch(
                        self,
                        format!(
                            "gate {} and up {} must have the same shape",
                            display_shape(&sa),
                            display_shape(&sb)
                        ),
                    );
                }
                Ty::Tensor {
                    dtype: DType::F32,
                    shape: sa,
                }
            }
            "reshape" => {
                let Some((d, s)) = tensor(self, 0) else {
                    return Ty::Unknown;
                };
                let Some(target) = self.target_shape(e, op, types, expected) else {
                    return Ty::Unknown;
                };
                match (elements(&s), elements(&target)) {
                    (Ok(a), Ok(b)) if a == b => {}
                    (Ok(a), Ok(b)) => {
                        return mismatch(
                            self,
                            format!(
                                "{} has {a} elements but {} has {b}",
                                display_shape(&s),
                                display_shape(&target)
                            ),
                        );
                    }
                    (Err(m), _) | (_, Err(m)) => return mismatch(self, m.to_string()),
                }
                self.cx
                    .elab
                    .insert((span.start, span.end), Elab::Shape(target.clone()));
                Ty::Tensor {
                    dtype: d,
                    shape: target,
                }
            }
            "transpose" => {
                let Some((d, s)) = tensor(self, 0) else {
                    return Ty::Unknown;
                };
                if !f32(self, 0, &d) {
                    return Ty::Unknown;
                }
                if s.len() != 2 {
                    return mismatch(
                        self,
                        format!("expected rank 2, found {}", display_shape(&s)),
                    );
                }
                Ty::Tensor {
                    dtype: d,
                    shape: vec![s[1].clone(), s[0].clone()],
                }
            }
            "softmax" | "argmax" => {
                let Some((d, s)) = tensor(self, 0) else {
                    return Ty::Unknown;
                };
                if !f32(self, 0, &d) {
                    return Ty::Unknown;
                }
                if s.len() != 1 {
                    return mismatch(
                        self,
                        format!("expected rank 1, found {}", display_shape(&s)),
                    );
                }
                if op == "argmax" {
                    Ty::Base("u64".into())
                } else {
                    Ty::Tensor { dtype: d, shape: s }
                }
            }
            "silu" => {
                let Some((d, s)) = tensor(self, 0) else {
                    return Ty::Unknown;
                };
                if !f32(self, 0, &d) {
                    return Ty::Unknown;
                }
                Ty::Tensor { dtype: d, shape: s }
            }
            "to_bf16" | "to_f32" => {
                let Some((d, s)) = tensor(self, 0) else {
                    return Ty::Unknown;
                };
                let (from, to) = if op == "to_bf16" {
                    (DType::F32, DType::Bf16)
                } else {
                    (DType::Bf16, DType::F32)
                };
                if d != from {
                    self.error(
                        codes::DTYPE,
                        format!("Tensor::{op} converts {from} tensors, found {d}"),
                        sp(0),
                    );
                    return Ty::Unknown;
                }
                Ty::Tensor {
                    dtype: to,
                    shape: s,
                }
            }
            "rms_norm" => {
                let (Some((dx, sx)), Some((dw, sw))) = (tensor(self, 0), tensor(self, 1)) else {
                    return Ty::Unknown;
                };
                if !f32(self, 0, &dx) || !f32(self, 1, &dw) {
                    return Ty::Unknown;
                }
                if sx.len() != 1 || sx != sw {
                    return mismatch(
                        self,
                        format!(
                            "x {} and weight {} must be the same rank-1 shape",
                            display_shape(&sx),
                            display_shape(&sw)
                        ),
                    );
                }
                Ty::Tensor {
                    dtype: dx,
                    shape: sx,
                }
            }
            "rope" => {
                let Some((d, s)) = tensor(self, 0) else {
                    return Ty::Unknown;
                };
                if !f32(self, 0, &d) {
                    return Ty::Unknown;
                }
                if s.len() != 2 {
                    return mismatch(
                        self,
                        format!("expected [Heads, HeadDim], found {}", display_shape(&s)),
                    );
                }
                self.require(
                    Obligation::Even {
                        dim: s[1].clone(),
                        reason: "RoPE head width".into(),
                    },
                    span.clone(),
                );
                Ty::Tensor { dtype: d, shape: s }
            }
            "qmatvec" | "block_scale" => {
                let Some((dw, sw)) = tensor(self, 0) else {
                    return Ty::Unknown;
                };
                let DType::Block(block) = &dw else {
                    self.error(
                        codes::DTYPE,
                        format!("argument 1 of Tensor::{op} must be a quantized block tensor, found {dw}"),
                        sp(0),
                    );
                    return Ty::Unknown;
                };
                let Some(_format) = self.block_format(block, &sp(0)) else {
                    return Ty::Unknown;
                };
                if sw.len() != 2 {
                    return mismatch(
                        self,
                        format!("expected [Rows, Cols], found {}", display_shape(&sw)),
                    );
                }
                self.require(
                    Obligation::Divides {
                        divisor: Dim::constant(BLOCK_ELEMENTS).expect("block size"),
                        value: sw[1].clone(),
                        reason: format!("whole {BLOCK_ELEMENTS}-element {block} blocks per row"),
                    },
                    span.clone(),
                );
                if op == "block_scale" {
                    return Ty::Base("f64".into());
                }
                let Some((dx, sx)) = tensor(self, 1) else {
                    return Ty::Unknown;
                };
                if !f32(self, 1, &dx) {
                    return Ty::Unknown;
                }
                if sx.len() != 1 || sx[0] != sw[1] {
                    return mismatch(
                        self,
                        format!(
                            "weights {} need an input of shape [{}], found {}",
                            display_shape(&sw),
                            sw[1],
                            display_shape(&sx)
                        ),
                    );
                }
                Ty::Tensor {
                    dtype: DType::F32,
                    shape: vec![sw[0].clone()],
                }
            }
            _ => Ty::Unknown,
        }
    }
    fn block_format(&mut self, block: &str, span: &Span) -> Option<QuantFormat> {
        let wire = self.cx.wires.get(block)?;
        match wire.format {
            Some(f) => Some(f),
            None => {
                let m = super::wire::format_mismatch(block, wire);
                self.error(codes::WIRE, m, span.clone());
                None
            }
        }
    }
    fn pool_dims(&mut self, kv: &Dim, hd: &Dim, span: &Span) {
        if let Some((h, d)) = self.sig.pool.clone()
            && (h != *kv || d != *hd)
        {
            self.error(
                codes::POOL,
                format!(
                    "this lease belongs to KvCachePool[{kv}, {hd}] but the function's row has KvCachePool[{h}, {d}]"
                ),
                span.clone(),
            );
        }
    }
    fn pool_op(
        &mut self,
        e: &Expr,
        op: &str,
        args: &[crate::ast::Arg],
        expected: Option<&Ty>,
    ) -> Ty {
        let span = e.span.clone();
        let n = match op {
            "begin_sequence" => 1,
            "lease_page" => 0,
            "append_kv_step" | "write_page" => 3,
            "attend" => 2,
            "tokens" | "page_tokens" | "release_sequence" | "release_page" => 1,
            _ => {
                for a in args {
                    self.expr(&a.expr, None);
                }
                self.error(
                    codes::UNSUPPORTED,
                    format!("unknown pool operation 'KvCachePool::{op}'"),
                    span,
                );
                return Ty::Unknown;
            }
        };
        if args.len() != n {
            for a in args {
                self.expr(&a.expr, None);
            }
            self.error(
                codes::SHAPE,
                format!(
                    "KvCachePool::{op} takes {n} argument(s), found {}",
                    args.len()
                ),
                span,
            );
            return Ty::Unknown;
        }
        let a: Vec<Arg2> = args.iter().map(|a| Arg2(&a.expr)).collect();
        let tys = self.args(&a);
        let result_of = |t: Ty| {
            Ty::Adt(
                "Result".into(),
                vec![Arg::Ty(t), Arg::Ty(Ty::Adt(KV_ERROR.into(), vec![]))],
            )
        };
        match op {
            "begin_sequence" | "lease_page" => {
                let from_expected = match expected {
                    Some(Ty::Adt(n, a)) if n == "Result" => match a.first() {
                        Some(Arg::Ty(Ty::Seq { kv, hd })) if op == "begin_sequence" => {
                            Some((kv.clone(), hd.clone()))
                        }
                        Some(Arg::Ty(Ty::Lease { kv, hd })) if op == "lease_page" => {
                            Some((kv.clone(), hd.clone()))
                        }
                        _ => None,
                    },
                    _ => None,
                };
                let Some((kv, hd)) = self.sig.pool.clone().or(from_expected.clone()) else {
                    self.error(
                        codes::INFER,
                        format!(
                            "KvCachePool::{op} needs the pool's dimensions: write KvCachePool[KvHeads, HeadDim] in the effect row or annotate the result"
                        ),
                        span,
                    );
                    return Ty::Unknown;
                };
                if let Some((ekv, ehd)) = from_expected {
                    self.pool_dims(&ekv, &ehd, &span);
                }
                result_of(if op == "begin_sequence" {
                    Ty::Seq { kv, hd }
                } else {
                    Ty::Lease { kv, hd }
                })
            }
            "append_kv_step" | "write_page" | "attend" | "tokens" | "page_tokens"
            | "release_sequence" | "release_page" => {
                let page = matches!(op, "write_page" | "page_tokens" | "release_page");
                let (kv, hd) = match &tys[0] {
                    Ty::Seq { kv, hd } if !page => (kv.clone(), hd.clone()),
                    Ty::Lease { kv, hd } if page => (kv.clone(), hd.clone()),
                    Ty::Unknown => {
                        self.error(
                            codes::INFER,
                            format!("cannot determine the pool instance of argument 1 of KvCachePool::{op}"),
                            a[0].0.span.clone(),
                        );
                        return Ty::Unknown;
                    }
                    other => {
                        let want = if page { LEASE } else { ACTIVE_SEQ };
                        self.error(
                            codes::SHAPE,
                            format!(
                                "argument 1 of KvCachePool::{op} must be a {want}, found {other}"
                            ),
                            a[0].0.span.clone(),
                        );
                        return Ty::Unknown;
                    }
                };
                self.pool_dims(&kv, &hd, &a[0].0.span);
                match op {
                    "append_kv_step" | "write_page" => {
                        for i in [1, 2] {
                            let what = format!("argument {} of KvCachePool::{op}", i + 1);
                            let Some((d, s)) = self.tensor(&tys[i], &a[i].0.span, &what) else {
                                return Ty::Unknown;
                            };
                            if d != DType::Bf16 {
                                self.error(
                                    codes::DTYPE,
                                    format!("{what} must be a Bf16 K/V step, found {d}"),
                                    a[i].0.span.clone(),
                                );
                            }
                            let want = vec![kv.clone(), hd.clone()];
                            if s != want {
                                self.error(
                                    codes::SHAPE,
                                    format!(
                                        "{what}: the pool stores {} steps, found {}",
                                        display_shape(&want),
                                        display_shape(&s)
                                    ),
                                    a[i].0.span.clone(),
                                );
                            }
                        }
                        result_of(Ty::unit())
                    }
                    "attend" => {
                        let what = "argument 2 of KvCachePool::attend";
                        let Some((d, s)) = self.tensor(&tys[1], &a[1].0.span, what) else {
                            return Ty::Unknown;
                        };
                        if !self.f32(&d, &a[1].0.span, what) {
                            return Ty::Unknown;
                        }
                        if s.len() != 2 || s[1] != hd {
                            self.error(
                                codes::SHAPE,
                                format!(
                                    "KvCachePool::attend: queries must be [QHeads, {hd}], found {}",
                                    display_shape(&s)
                                ),
                                a[1].0.span.clone(),
                            );
                            return Ty::Unknown;
                        }
                        self.require(
                            Obligation::Divides {
                                divisor: kv.clone(),
                                value: s[0].clone(),
                                reason: "grouped-query attention (QHeads % KvHeads == 0)".into(),
                            },
                            span,
                        );
                        Ty::Tensor {
                            dtype: DType::F32,
                            shape: s,
                        }
                    }
                    "tokens" | "page_tokens" => Ty::Base("u64".into()),
                    _ => Ty::unit(),
                }
            }
            _ => Ty::Unknown,
        }
    }
}

struct Arg2<'a>(&'a Expr);

fn strip_flexible(t: &mut Ty) {
    match t {
        Ty::Var(v) if flexible(v) => *t = Ty::Unknown,
        Ty::Adt(_, args) => {
            for a in args {
                if let Arg::Ty(t) = a {
                    strip_flexible(t);
                }
            }
        }
        Ty::Tuple(ts) => ts.iter_mut().for_each(strip_flexible),
        Ty::Fn(ps, r) => {
            ps.iter_mut().for_each(strip_flexible);
            strip_flexible(r);
        }
        _ => {}
    }
}

fn adt_subst(info: &AdtInfo, args: &[Arg]) -> Subst {
    let mut s = Subst::default();
    for (g, a) in info.generics.iter().zip(args) {
        match a {
            Arg::Dim(d) => {
                s.dims.insert(g.clone(), d.clone());
            }
            Arg::DType(d) => {
                s.dtypes.insert(g.clone(), d.clone());
            }
            Arg::Ty(t) => {
                s.tys.insert(g.clone(), t.clone());
            }
        }
    }
    s
}

/// Right-aligned broadcasting: equal dimensions, or a constant 1.
pub fn broadcast(a: &[Dim], b: &[Dim]) -> Result<Vec<Dim>, String> {
    let n = a.len().max(b.len());
    let one = Dim::one();
    let mut out = vec![];
    for i in 0..n {
        let x = (i + a.len()).checked_sub(n).map(|j| &a[j]).unwrap_or(&one);
        let y = (i + b.len()).checked_sub(n).map(|j| &b[j]).unwrap_or(&one);
        out.push(if x == y || *y == one {
            x.clone()
        } else if *x == one {
            y.clone()
        } else {
            return Err(format!(
                "{} and {} do not broadcast: axis {i} has {x} and {y}",
                display_shape(a),
                display_shape(b)
            ));
        });
    }
    Ok(out)
}
