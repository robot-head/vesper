//! Lowering a checked tensor module to core syntax (see the module docs).
use super::dim::Dim;
use super::infer::{Context, Elab};
use super::types::{AdtKinds, Kind, erase, erase_row};
use super::{DIM_PARAM, DIMS, MAX_RANK};
use crate::ast::*;
use std::collections::BTreeMap;

/// The built-in declarations. Operation types are the erased ones; the
/// tensor pass has already checked shapes and dtypes.
const PRELUDE: &str = "
copy struct Tensor { id: u64 }
copy struct Dims { rank: u64, d0: u64, d1: u64, d2: u64, d3: u64 }
linear struct KvPageLease { lease: u64 }
linear struct ActiveKvSeq { seq: u64 }
copy enum KvOomError { OutOfPages, PageFull, TokenLimit }
linear effect Tensor {
    fn matmul(a: Tensor, b: Tensor) -> Tensor;
    fn matvec(a: Tensor, x: Tensor) -> Tensor;
    fn add(a: Tensor, b: Tensor) -> Tensor;
    fn mul(a: Tensor, b: Tensor) -> Tensor;
    fn swiglu(gate: Tensor, up: Tensor) -> Tensor;
    fn reshape(x: Tensor, dims: Dims) -> Tensor;
    fn transpose(x: Tensor) -> Tensor;
    fn softmax(x: Tensor) -> Tensor;
    fn silu(x: Tensor) -> Tensor;
    fn to_bf16(x: Tensor) -> Tensor;
    fn to_f32(x: Tensor) -> Tensor;
    fn argmax(x: Tensor) -> u64;
    fn fill(value: f64, dims: Dims) -> Tensor;
    fn rms_norm(x: Tensor, w: Tensor, epsilon: f64) -> Tensor;
    fn rope(x: Tensor, position: u64) -> Tensor;
    fn qmatvec(w: Tensor, x: Tensor) -> Tensor;
    fn block_scale(w: Tensor, row: u64, block: u64) -> f64;
}
linear effect KvCachePool {
    fn begin_sequence(seq_id: u64) -> Result[ActiveKvSeq, KvOomError];
    fn append_kv_step(inout seq: ActiveKvSeq, k: Tensor, v: Tensor) -> Result[Unit, KvOomError];
    fn attend(seq: ActiveKvSeq, q: Tensor) -> Tensor;
    fn tokens(seq: ActiveKvSeq) -> u64;
    fn release_sequence(sink seq: ActiveKvSeq) -> Unit;
    fn lease_page() -> Result[KvPageLease, KvOomError];
    fn write_page(inout page: KvPageLease, k: Tensor, v: Tensor) -> Result[Unit, KvOomError];
    fn page_tokens(page: KvPageLease) -> u64;
    fn release_page(sink page: KvPageLease) -> Unit;
}
";

/// The built-in declarations, parsed.
pub fn prelude() -> Vec<Item> {
    crate::parser::parse(PRELUDE).module.items
}

pub struct Elaborated {
    pub module: Module,
    /// Synthetic span start to the source span it stands for.
    synthetic: BTreeMap<usize, Span>,
    base: usize,
}
impl Elaborated {
    /// Point a core diagnostic at source text.
    pub fn remap(&self, d: &mut Diagnostic) {
        if d.span.start >= self.base
            && let Some((_, origin)) = self.synthetic.range(..=d.span.start).next_back()
        {
            d.span = origin.clone();
        }
        for r in &mut d.related {
            if r.span.start >= self.base
                && let Some((_, origin)) = self.synthetic.range(..=r.span.start).next_back()
            {
                r.span = origin.clone();
            }
        }
    }
}

struct Lower<'a> {
    cx: &'a Context,
    kinds: &'a AdtKinds,
    next: usize,
    synthetic: BTreeMap<usize, Span>,
}

fn max_end(module: &Module) -> usize {
    module
        .items
        .iter()
        .map(|i| match i {
            Item::Adt(a) => a.span.end,
            Item::Function(f) => f.span.end,
            Item::Effect(e) => e.span.end,
            Item::Impl { methods, .. } => methods.iter().map(|m| m.span.end).max().unwrap_or(0),
            Item::Trait(t) => t.span.end,
            Item::Conformance(c) => c.span.end,
        })
        .max()
        .unwrap_or(0)
}

pub fn elaborate(module: &Module, cx: &Context) -> Elaborated {
    let base = max_end(module).saturating_add(1 << 20);
    let mut l = Lower {
        cx,
        kinds: &cx.kinds,
        next: base,
        synthetic: BTreeMap::new(),
    };
    let mut items = prelude();
    for item in &module.items {
        items.push(match item {
            Item::Adt(a) => Item::Adt(l.adt(a)),
            Item::Function(f) => Item::Function(Box::new(l.function(f))),
            other => other.clone(),
        });
    }
    Elaborated {
        module: Module {
            items,
            model_uses: module.model_uses.clone(),
            trait_bounds: module.trait_bounds.clone(),
        },
        synthetic: l.synthetic,
        base,
    }
}

impl Lower<'_> {
    fn span(&mut self, origin: &Span) -> Span {
        let s = self.next;
        self.next += 2;
        self.synthetic.insert(s, origin.clone());
        s..s + 1
    }
    fn ty(&self, t: &Type) -> Type {
        erase(t, self.kinds)
    }
    fn adt(&self, a: &Adt) -> Adt {
        let kinds = self.kinds.get(&a.name).cloned().unwrap_or_default();
        Adt {
            name: a.name.clone(),
            mode: a.mode,
            generics: a
                .generics
                .iter()
                .zip(kinds.iter().copied().chain(std::iter::repeat(Kind::Type)))
                .filter(|(_, k)| !k.erased())
                .map(|(g, _)| g.clone())
                .collect(),
            variants: a
                .variants
                .iter()
                .map(|v| Variant {
                    name: v.name.clone(),
                    fields: v
                        .fields
                        .iter()
                        .map(|f| Field {
                            name: f.name.clone(),
                            ty: self.ty(&f.ty),
                            modality: f.modality,
                        })
                        .collect(),
                    named: v.named,
                })
                .collect(),
            is_enum: a.is_enum,
            span: a.span.clone(),
        }
    }
    fn function(&mut self, f: &Function) -> Function {
        let Some(sig) = self.cx.sigs.get(&f.name) else {
            return f.clone();
        };
        let kind = |g: &Generic| {
            sig.generics
                .iter()
                .find(|(n, _)| *n == g.name)
                .map(|(_, k)| *k)
                .unwrap_or(Kind::Type)
        };
        let mut params = vec![];
        for d in sig.dims() {
            let span = self.span(&f.span);
            params.push(Param {
                name: format!("{DIM_PARAM}{d}"),
                pass: Pass::In,
                projections: vec![],
                ty: Type::named("u64"),
                span,
            });
        }
        params.extend(f.params.iter().map(|p| Param {
            name: p.name.clone(),
            pass: p.pass,
            projections: p.projections.clone(),
            ty: self.ty(&p.ty),
            span: p.span.clone(),
        }));
        Function {
            name: f.name.clone(),
            generics: f
                .generics
                .iter()
                .filter(|g| g.row || !kind(g).erased())
                .cloned()
                .collect(),
            params,
            result: self.ty(&f.result),
            effects: erase_row(&f.effects, self.kinds),
            body: f.body.as_ref().map(|b| self.expr(b)),
            span: f.span.clone(),
        }
    }
    fn dim(&mut self, d: &Dim, origin: &Span) -> Expr {
        let mut factors: Vec<Expr> = vec![];
        if d.coeff != 1 || d.vars.is_empty() {
            let span = self.span(origin);
            factors.push(Expr {
                kind: ExprKind::Literal(Literal::Int(i128::from(d.coeff))),
                span,
            });
        }
        for (v, e) in &d.vars {
            for _ in 0..*e {
                let span = self.span(origin);
                factors.push(Expr {
                    kind: ExprKind::Name(format!("{DIM_PARAM}{v}")),
                    span,
                });
            }
        }
        let mut it = factors.into_iter();
        let mut acc = it.next().expect("a dimension has a factor");
        for x in it {
            let callee_span = self.span(origin);
            let span = self.span(origin);
            acc = Expr {
                kind: ExprKind::Call {
                    callee: Box::new(Expr {
                        kind: ExprKind::Name("wrapping_mul".into()),
                        span: callee_span,
                    }),
                    types: vec![],
                    args: vec![arg(acc), arg(x)],
                },
                span,
            };
        }
        acc
    }
    fn dims_value(&mut self, shape: &[Dim], origin: &Span) -> Expr {
        let mut fields = vec![];
        let span = self.span(origin);
        fields.push((
            "rank".to_string(),
            Expr {
                kind: ExprKind::Literal(Literal::Int(shape.len() as i128)),
                span,
            },
        ));
        for i in 0..MAX_RANK {
            let value = match shape.get(i) {
                Some(d) => self.dim(d, origin),
                None => {
                    let span = self.span(origin);
                    Expr {
                        kind: ExprKind::Literal(Literal::Int(1)),
                        span,
                    }
                }
            };
            fields.push((format!("d{i}"), value));
        }
        let span = self.span(origin);
        Expr {
            kind: ExprKind::Struct {
                name: DIMS.into(),
                fields,
            },
            span,
        }
    }
    fn expr(&mut self, e: &Expr) -> Expr {
        let span = e.span.clone();
        let kind = match &e.kind {
            ExprKind::Literal(_) | ExprKind::Name(_) => e.kind.clone(),
            ExprKind::Tuple(es) => ExprKind::Tuple(es.iter().map(|x| self.expr(x)).collect()),
            ExprKind::Struct { name, fields } => ExprKind::Struct {
                name: name.clone(),
                fields: fields
                    .iter()
                    .map(|(n, x)| (n.clone(), self.expr(x)))
                    .collect(),
            },
            ExprKind::Block { statements, tail } => ExprKind::Block {
                statements: statements
                    .iter()
                    .map(|s| match s {
                        Statement::Let { pat, ty, value } => Statement::Let {
                            pat: pat.clone(),
                            ty: self.ty(ty),
                            value: self.expr(value),
                        },
                        Statement::Expr(x) => Statement::Expr(self.expr(x)),
                    })
                    .collect(),
                tail: tail.as_ref().map(|t| Box::new(self.expr(t))),
            },
            ExprKind::Call {
                callee,
                types,
                args,
            } => {
                let mut new_args: Vec<crate::ast::Arg> = vec![];
                let mut new_types: Vec<Type> = types.iter().map(|t| self.ty(t)).collect();
                match self.cx.elab.get(&(span.start, span.end)).cloned() {
                    Some(Elab::Dims(dims)) => {
                        for d in &dims {
                            new_args.push(arg(self.dim(d, &span)));
                        }
                        if let ExprKind::Name(n) = &callee.kind
                            && let Some(sig) = self.cx.sigs.get(n)
                            && types.len() == sig.generics.len()
                        {
                            new_types = types
                                .iter()
                                .zip(&sig.generics)
                                .filter(|(_, (_, k))| !k.erased())
                                .map(|(t, _)| self.ty(t))
                                .collect();
                        }
                        new_args.extend(args.iter().map(|a| self.arg(a)));
                    }
                    Some(Elab::Shape(shape)) => {
                        new_types.clear();
                        new_args.extend(args.iter().map(|a| self.arg(a)));
                        new_args.push(arg(self.dims_value(&shape, &span)));
                    }
                    None => new_args.extend(args.iter().map(|a| self.arg(a))),
                }
                ExprKind::Call {
                    callee: Box::new(self.expr(callee)),
                    types: new_types,
                    args: new_args,
                }
            }
            ExprKind::Field(x, f) => ExprKind::Field(Box::new(self.expr(x)), f.clone()),
            ExprKind::Index(a, b) => {
                ExprKind::Index(Box::new(self.expr(a)), Box::new(self.expr(b)))
            }
            ExprKind::Try(x) => ExprKind::Try(Box::new(self.expr(x))),
            ExprKind::Unary(op, x) => ExprKind::Unary(op.clone(), Box::new(self.expr(x))),
            ExprKind::Binary(op, a, b) => {
                ExprKind::Binary(op.clone(), Box::new(self.expr(a)), Box::new(self.expr(b)))
            }
            ExprKind::Assign(op, a, b) => {
                ExprKind::Assign(op.clone(), Box::new(self.expr(a)), Box::new(self.expr(b)))
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => ExprKind::If {
                condition: Box::new(self.expr(condition)),
                then: Box::new(self.expr(then)),
                otherwise: otherwise.as_ref().map(|o| Box::new(self.expr(o))),
            },
            ExprKind::Match { value, arms } => ExprKind::Match {
                value: Box::new(self.expr(value)),
                arms: arms
                    .iter()
                    .map(|(p, x)| (p.clone(), self.expr(x)))
                    .collect(),
            },
            ExprKind::Closure {
                params,
                mode,
                result,
                effects,
                infer_effects,
                body,
            } => ExprKind::Closure {
                params: params
                    .iter()
                    .map(|p| Param {
                        ty: self.ty(&p.ty),
                        ..p.clone()
                    })
                    .collect(),
                mode: *mode,
                result: self.ty(result),
                effects: erase_row(effects, self.kinds),
                infer_effects: *infer_effects,
                body: Box::new(self.expr(body)),
            },
            ExprKind::Handle {
                effect,
                state,
                body,
                ret,
                ops,
            } => ExprKind::Handle {
                effect: erase_row(
                    &Effects {
                        instances: vec![effect.clone()],
                        tail: None,
                    },
                    self.kinds,
                )
                .instances
                .remove(0),
                state: state
                    .as_ref()
                    .map(|(p, x)| (p.clone(), Box::new(self.expr(x)))),
                body: Box::new(self.expr(body)),
                ret: Box::new(self.clause(ret)),
                ops: ops.iter().map(|c| self.clause(c)).collect(),
            },
        };
        Expr { kind, span }
    }
    fn clause(&mut self, c: &Clause) -> Clause {
        Clause {
            name: c.name.clone(),
            pats: c.pats.clone(),
            body: self.expr(&c.body),
        }
    }
    fn arg(&mut self, a: &crate::ast::Arg) -> crate::ast::Arg {
        crate::ast::Arg {
            pass: a.pass,
            explicit: a.explicit,
            expr: self.expr(&a.expr),
        }
    }
}

fn arg(expr: Expr) -> crate::ast::Arg {
    crate::ast::Arg {
        pass: Pass::In,
        explicit: false,
        expr,
    }
}
