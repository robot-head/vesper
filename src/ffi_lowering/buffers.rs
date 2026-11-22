//! Foreign memory viewed in place (§16.3): the rules a pinned buffer's or a
//! mapping's sequence `b.data` obeys beyond the ordinary checker's.
//!
//! `b.data` is an ordinary `ChunkSeq[T]` to the checker, so reads, writes,
//! bound views (`let v = b.data.as_slice();`) and lent views
//! (`f(inout b.data[a..c])`) follow the ordinary loan rules, and a bound view
//! keeps `b` from moving, and so from its release, while it is in scope
//! (`E1531`). What the checker would also allow, and foreign memory cannot
//! take, is anything that changes the sequence's buffer: C may hold its
//! address, so it may not grow (a reallocation), shrink, move out or be
//! replaced. This pass refuses every use of `b.data` other than an element
//! access or a view (`b.data[i]`, `b.data[a..c]`), a call of a method that
//! keeps the buffer where it is and as long as it is, and an explicit `in`
//! argument.
use super::types::{BUFFER_FIELD, Generated};
use super::{FfiError, FfiErrorKind};
use crate::ast::*;

/// The `ChunkSeq` methods that neither reallocate, nor change the length,
/// nor give the sequence away.
fn keeps_buffer(method: &str) -> bool {
    matches!(
        method,
        "len"
            | "get"
            | "set"
            | "try_get"
            | "try_set"
            | "try_get_mut"
            | "as_slice"
            | "as_slice_mut"
            | "copy_from"
            | "try_copy_from"
            | "copy_within"
            | "try_copy_within"
            | "fill"
            | "try_fill"
            | "swap"
            | "sum_be16"
            | "eq"
            | "cmp"
            | "find_byte"
            | "find_either"
            | "find_not_in"
            | "eq_ignore_ascii_case"
            | "make_ascii_lowercase"
            | "try_make_ascii_lowercase"
    ) || ["read_", "write_", "try_read_", "try_write_"]
        .iter()
        .any(|p| method.starts_with(p))
}

struct Walk<'a> {
    generated: &'a Generated,
    types: &'a TypeMap,
    /// Struct declarations by name, for the types of field paths.
    adts: std::collections::BTreeMap<&'a str, &'a Adt>,
    /// Effect declarations by name, for the types of handler-clause
    /// patterns.
    effects: std::collections::BTreeMap<&'a str, &'a EffectDecl>,
    /// Local bindings in scope, innermost last. The checker records the
    /// types of expressions it evaluates but not of place roots (`b` in
    /// `b.data.push(x)`), so the walk types places itself.
    scopes: Vec<Vec<(String, Type)>>,
    error: Option<FfiError>,
}

impl<'a> Walk<'a> {
    fn recorded(&self, e: &Expr) -> Option<Type> {
        self.types.get(&(e.span.start, e.span.end)).cloned()
    }
    fn lookup(&self, name: &str) -> Option<Type> {
        self.scopes
            .iter()
            .rev()
            .flat_map(|s| s.iter().rev())
            .find(|(n, _)| n == name)
            .map(|(_, t)| t.clone())
    }
    /// The type of the field `field` of a value of type `ty`.
    fn field_type(&self, ty: &Type, field: &str) -> Option<Type> {
        let Type::Named(name, args) = ty else {
            return None;
        };
        let adt = self.adts.get(name.as_str())?;
        let variant = adt.variants.first().filter(|_| !adt.is_enum)?;
        let f = variant.fields.iter().find(|f| f.name == field)?;
        let vars = adt
            .generics
            .iter()
            .map(|g| g.name.clone())
            .zip(args.iter().cloned())
            .collect();
        Some(substitute(&f.ty, &vars))
    }
    /// The type of the place or value `e`.
    fn type_of(&self, e: &Expr) -> Option<Type> {
        match &e.kind {
            ExprKind::Name(n) => self.lookup(n).or_else(|| self.recorded(e)),
            ExprKind::Field(base, field) => self
                .type_of(base)
                .and_then(|t| self.field_type(&t, field))
                .or_else(|| self.recorded(e)),
            _ => self.recorded(e),
        }
    }
    /// Bind the names of `p`, which matches a value of type `ty`.
    fn bind(&mut self, p: &Pattern, ty: Option<Type>) {
        match &p.kind {
            PatternKind::Bind(n) => {
                if let Some(t) = ty {
                    self.scopes
                        .last_mut()
                        .expect("a scope")
                        .push((n.clone(), t));
                } else {
                    // Shadow an outer binding of the same name.
                    self.scopes
                        .last_mut()
                        .expect("a scope")
                        .push((n.clone(), Type::Infer));
                }
            }
            PatternKind::Tuple(ps) => {
                let ts = match ty {
                    Some(Type::Tuple(ts)) if ts.len() == ps.len() => {
                        ts.into_iter().map(Some).collect()
                    }
                    _ => vec![None; ps.len()],
                };
                for (p, t) in ps.iter().zip(ts) {
                    self.bind(p, t);
                }
            }
            PatternKind::Constructor { name, fields, .. } => {
                for (i, (field, p)) in fields.iter().enumerate() {
                    let t = ty.as_ref().and_then(|t| self.payload(t, name, field, i));
                    self.bind(p, t);
                }
            }
            PatternKind::Wild | PatternKind::Literal(_) => {}
        }
    }
    /// The type of field `field` (position `i`) of constructor `name` of a
    /// value of type `ty`.
    fn payload(&self, ty: &Type, name: &str, field: &str, i: usize) -> Option<Type> {
        let Type::Named(n, args) = ty else {
            return None;
        };
        match (n.as_str(), name) {
            ("Option", "Some") | ("Result", "Ok") => return args.first().cloned(),
            ("Result", "Err") => return args.get(1).cloned(),
            _ => {}
        }
        let adt = self.adts.get(n.as_str())?;
        let variant = adt.variants.iter().find(|v| v.name == name)?;
        let f = variant
            .fields
            .iter()
            .find(|f| f.name == field)
            .or_else(|| variant.fields.get(i))?;
        let vars = adt
            .generics
            .iter()
            .map(|g| g.name.clone())
            .zip(args.iter().cloned())
            .collect();
        Some(substitute(&f.ty, &vars))
    }
    /// The buffer type of `e` when `e` is `b.data` of a pinned buffer or a
    /// mapping `b`.
    fn buffer_of(&self, e: &Expr) -> Option<String> {
        let ExprKind::Field(base, field) = &e.kind else {
            return None;
        };
        if field != BUFFER_FIELD {
            return None;
        }
        match self.type_of(base) {
            Some(Type::Named(n, _)) if self.generated.is_buffer(&n) => Some(n),
            _ => None,
        }
    }
    fn refuse(&mut self, ty: &str, e: &Expr) {
        if self.error.is_none() {
            self.error = Some(FfiError::new(
                FfiErrorKind::BufferMisuse,
                format!(
                    "the memory of {ty} stays where it is while C may hold its address: `.data` is used only as `.data[i]`, `.data[a..b]`, an `in` argument, or the receiver of a method that keeps its length (not growing, shrinking, moving or replacing it)"
                ),
                Some(e.span.clone()),
            ));
        }
    }
    fn scoped(&mut self, f: impl FnOnce(&mut Self)) {
        self.scopes.push(vec![]);
        f(self);
        self.scopes.pop();
    }
    fn function(&mut self, f: &Function) {
        let Some(body) = &f.body else {
            return;
        };
        self.scopes.push(
            f.params
                .iter()
                .map(|p| (p.name.clone(), p.ty.clone()))
                .collect(),
        );
        self.expr(body, false);
        self.scopes.pop();
    }
    /// `e` in a position that may hold `b.data` when `ok`.
    fn expr(&mut self, e: &Expr, ok: bool) {
        if let Some(ty) = self.buffer_of(e) {
            if !ok {
                self.refuse(&ty, e);
            }
            if let ExprKind::Field(base, _) = &e.kind {
                self.expr(base, false);
            }
            return;
        }
        match &e.kind {
            ExprKind::Literal(_) | ExprKind::Name(_) | ExprKind::Continue => {}
            ExprKind::Tuple(es) => es.iter().for_each(|e| self.expr(e, false)),
            ExprKind::Struct { fields, .. } => fields.iter().for_each(|(_, e)| self.expr(e, false)),
            ExprKind::Block { statements, tail } => self.scoped(|w| {
                for s in statements {
                    match s {
                        Statement::Let { pat, ty, value, .. } => {
                            w.expr(value, false);
                            let t = match ty {
                                Type::Infer => w.type_of(value),
                                t => Some(t.clone()),
                            };
                            w.bind(pat, t);
                        }
                        Statement::Expr(e) => w.expr(e, false),
                    }
                }
                if let Some(t) = tail {
                    w.expr(t, false);
                }
            }),
            ExprKind::Call { callee, args, .. } => {
                match &callee.kind {
                    ExprKind::Field(receiver, method) => {
                        self.expr(receiver, keeps_buffer(method));
                    }
                    _ => self.expr(callee, false),
                }
                for a in args {
                    self.expr(&a.expr, a.explicit && a.pass == Pass::In);
                }
            }
            ExprKind::Field(base, _) | ExprKind::Try(base) | ExprKind::Unary(_, base) => {
                self.expr(base, false)
            }
            ExprKind::Index(base, index) => {
                self.expr(base, true);
                self.expr(index, false);
            }
            ExprKind::Binary(_, a, b) | ExprKind::Assign(_, a, b) => {
                self.expr(a, false);
                self.expr(b, false);
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                self.expr(condition, false);
                self.expr(then, false);
                if let Some(o) = otherwise {
                    self.expr(o, false);
                }
            }
            ExprKind::Match { value, arms } => {
                self.expr(value, false);
                let ty = self.type_of(value);
                for (p, e) in arms {
                    self.scoped(|w| {
                        w.bind(p, ty.clone());
                        w.expr(e, false);
                    });
                }
            }
            ExprKind::Closure { params, body, .. } => {
                self.scopes.push(
                    params
                        .iter()
                        .map(|p| (p.name.clone(), p.ty.clone()))
                        .collect(),
                );
                self.expr(body, false);
                self.scopes.pop();
            }
            ExprKind::Handle {
                effect,
                state,
                body,
                ret,
                ops,
            } => {
                // The clause patterns are typed as the checker types them:
                // the return clause binds the body's value (with the state
                // after it), and an operation clause binds the operation's
                // arguments, then the state, then the continuation.
                let state_ty = state.as_ref().map(|(_, s)| {
                    self.expr(s, false);
                    self.type_of(s)
                });
                self.expr(body, false);
                let body_ty = self.type_of(body);
                let ret_ty = match &state_ty {
                    Some(st) => match (body_ty, st) {
                        (Some(b), Some(st)) => Some(Type::Tuple(vec![b, st.clone()])),
                        _ => None,
                    },
                    None => body_ty,
                };
                self.scoped(|w| {
                    if let Some(p) = ret.pats.first() {
                        w.bind(p, ret_ty);
                    }
                    ret.pats.iter().skip(1).for_each(|p| w.bind(p, None));
                    w.expr(&ret.body, false);
                });
                let decl = self.effects.get(effect.name.as_str()).copied();
                let vars: std::collections::BTreeMap<String, Type> = decl
                    .map(|d| {
                        d.generics
                            .iter()
                            .map(|g| g.name.clone())
                            .zip(effect.args.iter().cloned())
                            .collect()
                    })
                    .unwrap_or_default();
                for c in ops {
                    let sig = decl.and_then(|d| d.ops.iter().find(|f| f.name == c.name));
                    let mut tys: Vec<Option<Type>> = sig
                        .map(|f| {
                            f.params
                                .iter()
                                .map(|p| Some(substitute(&p.ty, &vars)))
                                .collect()
                        })
                        .unwrap_or_default();
                    if let Some(st) = &state_ty {
                        tys.push(st.clone());
                    }
                    self.scoped(|w| {
                        for (i, p) in c.pats.iter().enumerate() {
                            w.bind(p, tys.get(i).cloned().flatten());
                        }
                        w.expr(&c.body, false);
                    });
                }
            }
            ExprKind::For { pat, iter, body } => {
                self.expr(iter, false);
                self.scoped(|w| {
                    w.bind(pat, None);
                    w.expr(body, false);
                });
            }
            k => {
                for c in k.control_children().into_iter().flatten() {
                    self.expr(c, false);
                }
            }
        }
    }
}

/// Refuse every use of a pinned buffer's or a mapping's `.data` that could
/// move its memory. `types` are the checked module's expression types.
pub(super) fn check(
    module: &Module,
    types: &TypeMap,
    generated: &Generated,
) -> Result<(), FfiError> {
    if !generated.has_buffers() {
        return Ok(());
    }
    let mut walk = Walk {
        generated,
        types,
        adts: module
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Adt(a) => Some((a.name.as_str(), a)),
                _ => None,
            })
            .collect(),
        effects: module
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Effect(e) => Some((e.name.as_str(), e)),
                _ => None,
            })
            .collect(),
        scopes: vec![],
        error: None,
    };
    for item in &module.items {
        match item {
            Item::Function(f) => walk.function(f),
            Item::Impl { methods, .. } | Item::Conformance(Conformance { methods, .. }) => {
                for f in methods {
                    walk.function(f);
                }
            }
            _ => {}
        }
    }
    walk.error.map_or(Ok(()), Err)
}
