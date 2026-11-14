//! A numeric admission envelope, never a substitute inferred `Type` or World.
//! The ordinary checker still decides typing, ownership and effects. This runs
//! only after the existing structural gate, and before its first Type clone.
//!
//! It covers inferred tuples and generic constructors (`Some`, `Ok`, user
//! generic ADTs). The checker infers constructor type arguments locally and
//! bidirectionally: each argument is cloned from the expected type or from
//! a sub-term of a field's actual type, refined at most once per field, and
//! never solved through global unification. So an argument is bounded by the
//! expected type's envelope plus the sum of the field envelopes, and every
//! expected type an expression is checked against is threaded down here as
//! its own envelope.
use super::AnalysisLimits;
use crate::ast::*;

#[derive(Clone, Copy)]
struct Bound {
    nodes: usize,
    depth: usize,
    bytes: usize,
}
#[derive(Clone, Copy)]
struct Value<'a> {
    bound: Bound,
    exact: Option<&'a Type>,
    function: Option<&'a Function>,
    projection_safe: bool,
    /// A handler continuation. Its arrow type carries a refined effect row
    /// this envelope does not model, so any use of the name is refused.
    opaque: bool,
}
/// A constructor as the checker resolves it: its ADT's generic arity, the
/// envelope of its field types (type parameters as leaves) and its name.
struct Ctor<'a> {
    generics: usize,
    fields: Vec<&'a Type>,
    adt: &'a str,
}
struct Admission<'a> {
    items: &'a [&'a Item],
    limits: AnalysisLimits,
    env: Vec<(&'a str, Value<'a>)>,
    work: usize,
    max_work: usize,
    retained_nodes: usize,
    retained_bytes: usize,
    snapshot_bytes: usize,
    // Every non-generic nominal field is a closed source type. A projection
    // from a conservatively bound tuple pattern may expose any such field.
    fields: Bound,
    fields_safe: bool,
    // Generic ADT field types with each type parameter a leaf; a pattern or
    // projection substitutes the scrutinee's arguments into them.
    // `None` when an inventoried type is outside this envelope; refused only
    // where it is needed, so programs that never use it are unaffected.
    generic_fields: Option<Bound>,
    // Effect operation parameter and result types, parameters as leaves.
    effect_ops: Option<Bound>,
    // Every effect instance a signature declares, with generated labels.
    effect_row: Option<Bound>,
}
const UNCERTAIN: &str = "uncertain tuple inference is unsupported before checking";
const BUDGET: &str = "inferred tuple type/metadata budget exceeded before checking";
const OPAQUE: &str = "handler continuation use is unsupported before checking";

pub(super) fn guard(items: &[&Item], limits: AnalysisLimits) -> Result<(), String> {
    let mut a = Admission {
        items,
        limits,
        env: Vec::new(),
        work: 0,
        max_work: limits
            .max_ast_nodes
            .checked_mul(limits.max_ast_nodes)
            .and_then(|n| n.checked_mul(8))
            .ok_or(BUDGET)?,
        retained_nodes: 0,
        retained_bytes: 0,
        snapshot_bytes: 0,
        fields: Bound {
            nodes: 1,
            depth: 1,
            bytes: 128,
        },
        fields_safe: true,
        generic_fields: Some(Bound {
            nodes: 1,
            depth: 1,
            bytes: 128,
        }),
        effect_ops: Some(Bound {
            nodes: 1,
            depth: 1,
            bytes: 128,
        }),
        effect_row: Some(Bound {
            nodes: 0,
            depth: 0,
            bytes: 0,
        }),
    };
    for (index, item) in items.iter().enumerate() {
        a.step()?;
        // Pact substitutes Self/associated types into generated methods before
        // checking. Implicit inherent self types are also replaced by collect.
        // These are not the closed headers this numeric envelope reasons over.
        match item {
            Item::Trait(_) | Item::Conformance(_) => return Err(UNCERTAIN.into()),
            Item::Impl { methods, .. } => {
                for f in methods {
                    a.step()?;
                    for p in &f.params {
                        a.step()?;
                        if p.name == "self" && p.ty == Type::Infer {
                            return Err(UNCERTAIN.into());
                        }
                    }
                }
            }
            _ => (),
        }
        if let Item::Adt(adt) = item {
            a.adt_inventory(adt, index)?;
            if !adt.generics.is_empty() {
                for field in adt.variants.iter().flat_map(|v| &v.fields) {
                    a.generic_fields = a.fold_max(a.generic_fields, &field.ty);
                }
                continue;
            }
            for field in adt.variants.iter().flat_map(|v| &v.fields) {
                let v = a.ty(&field.ty)?;
                a.fields = a.maximum(a.fields, v.bound)?;
                a.fields_safe &= v.projection_safe;
            }
        }
        let mut signatures: Vec<&Function> = Vec::new();
        match item {
            Item::Function(f) => signatures.push(f),
            Item::Effect(e) => {
                for f in &e.ops {
                    for p in &f.params {
                        a.effect_ops = a.fold_max(a.effect_ops, &p.ty);
                    }
                    a.effect_ops = a.fold_max(a.effect_ops, &f.result);
                    signatures.push(f);
                }
            }
            Item::Impl { methods, .. } => signatures.extend(methods.iter()),
            _ => (),
        }
        for f in signatures {
            for e in &f.effects.instances {
                a.step()?;
                a.effect_row = a.effect_row.and_then(|total| a.row_entry(total, f, e).ok());
            }
        }
    }
    for item in items {
        a.step()?;
        match item {
            Item::Function(f) => a.function(f)?,
            Item::Effect(e) => {
                for f in &e.ops {
                    a.function(f)?;
                }
            }
            Item::Impl { methods, .. } => {
                for f in methods {
                    a.function(f)?;
                }
            }
            Item::Trait(t) => {
                for f in &t.methods {
                    a.function(f)?;
                }
            }
            Item::Conformance(c) => {
                for f in &c.methods {
                    a.function(f)?;
                }
            }
            Item::Adt(_) => (),
        }
    }
    Ok(())
}
impl<'a> Admission<'a> {
    fn fold_max(&mut self, total: Option<Bound>, t: &'a Type) -> Option<Bound> {
        let total = total?;
        let v = self.ty(t).ok()?;
        self.maximum(total, v.bound).ok()
    }
    fn row_entry(&mut self, total: Bound, f: &Function, e: &'a Effect) -> Result<Bound, String> {
        let mut row = self.leaf().bound;
        row.bytes = f
            .name
            .len()
            .checked_add(e.name.len())
            .and_then(|n| n.checked_mul(4))
            .and_then(|n| n.checked_add(1024))
            .ok_or(BUDGET)?;
        for t in &e.args {
            let c = self.ty(t)?;
            row = self.add(row, c.bound, true)?;
        }
        self.add(total, row, false)
    }
    /// The envelope of a type with every type-parameter leaf replaced by a
    /// type within `arg`: each of its nodes may become a whole `arg`.
    fn substitute(&self, template: Bound, arg: Bound) -> Result<Bound, String> {
        self.bounded(Bound {
            nodes: template.nodes.checked_mul(arg.nodes.max(1)).ok_or(BUDGET)?,
            depth: template.depth.checked_add(arg.depth).ok_or(BUDGET)?,
            bytes: template
                .nodes
                .checked_mul(arg.bytes)
                .and_then(|n| n.checked_add(template.bytes))
                .ok_or(BUDGET)?,
        })
    }
    /// Any field a pattern or projection can expose from a value within `v`.
    fn field_of(&self, v: Bound) -> Result<Bound, String> {
        let generic = self.generic_fields.ok_or(UNCERTAIN)?;
        let generic = self.substitute(generic, v)?;
        let closed = self.maximum(v, self.fields)?;
        self.maximum(closed, generic)
    }
    fn step(&mut self) -> Result<(), String> {
        self.work = self.work.checked_add(1).ok_or(BUDGET)?;
        if self.work > self.max_work {
            return Err("tuple admission work budget exceeded".into());
        }
        Ok(())
    }
    fn bounded(&self, b: Bound) -> Result<Bound, String> {
        if b.nodes > self.limits.max_type_nodes
            || b.depth > self.limits.max_depth
            || b.bytes > self.limits.max_metadata_bytes
        {
            return Err(BUDGET.into());
        }
        Ok(b)
    }
    fn leaf(&self) -> Value<'a> {
        Value {
            bound: Bound {
                nodes: 1,
                depth: 1,
                bytes: 128 + self.limits.max_identifier_bytes,
            },
            exact: None,
            function: None,
            projection_safe: true,
            opaque: false,
        }
    }
    fn maximum(&self, a: Bound, b: Bound) -> Result<Bound, String> {
        self.bounded(Bound {
            nodes: a.nodes.max(b.nodes),
            depth: a.depth.max(b.depth),
            bytes: a.bytes.max(b.bytes),
        })
    }
    fn add(&self, a: Bound, b: Bound, child: bool) -> Result<Bound, String> {
        self.bounded(Bound {
            nodes: a.nodes.checked_add(b.nodes).ok_or(BUDGET)?,
            depth: a
                .depth
                .max(b.depth.checked_add(usize::from(child)).ok_or(BUDGET)?),
            bytes: a.bytes.checked_add(b.bytes).ok_or(BUDGET)?,
        })
    }
    fn adt_inventory(&mut self, adt: &Adt, index: usize) -> Result<(), String> {
        // World::collect keeps the last ADT of a name, including malformed
        // declarations. Never resolve a projection using an earlier header.
        if matches!(
            adt.name.as_str(),
            "Option" | "Result" | "RuntimeError" | "ArithOverflow" | "Unit" | "Never" | "String"
        ) {
            return Err(UNCERTAIN.into());
        }
        for item in &self.items[..index] {
            self.step()?;
            if matches!(item, Item::Adt(other) if other.name == adt.name) {
                return Err(UNCERTAIN.into());
            }
        }
        for (i, variant) in adt.variants.iter().enumerate() {
            for earlier in &adt.variants[..i] {
                self.step()?;
                if earlier.name == variant.name {
                    return Err(UNCERTAIN.into());
                }
            }
            for (j, field) in variant.fields.iter().enumerate() {
                for earlier in &variant.fields[..j] {
                    self.step()?;
                    if earlier.name == field.name {
                        return Err(UNCERTAIN.into());
                    }
                }
            }
        }
        Ok(())
    }
    fn nominal_arity(&mut self, name: &str, arity: usize) -> Result<(), String> {
        // A nullary constructor can clone its complete expected nominal type
        // even after the checker diagnoses malformed type-argument arity.
        // Refuse those contexts before interpreting any expression as a leaf.
        let mut expected = match name {
            "Option" => Some(1),
            "Result" => Some(2),
            _ => None,
        };
        for item in self.items {
            self.step()?;
            if let Item::Adt(adt) = item
                && adt.name == name
            {
                expected = Some(adt.generics.len());
            }
        }
        if expected.is_some_and(|expected| expected != arity) || (expected.is_none() && arity != 0)
        {
            return Err(UNCERTAIN.into());
        }
        Ok(())
    }
    fn ty(&mut self, t: &'a Type) -> Result<Value<'a>, String> {
        self.step()?;
        let mut v = self.leaf();
        v.exact = Some(t);
        match t {
            Type::Named(_, ts) | Type::Tuple(ts) => {
                if let Type::Named(name, args) = t {
                    // Explicit source `_` is retained as a named placeholder
                    // by the current parser; omitted annotations use Infer.
                    if name == "_" {
                        return Err(UNCERTAIN.into());
                    }
                    self.nominal_arity(name, args.len())?;
                    v.bound.bytes = 128usize.checked_add(name.len()).ok_or(BUDGET)?;
                }
                // Generic nominal fields are bounded by `field_of`, which
                // substitutes this value's arguments into them.
                for t in ts {
                    let c = self.ty(t)?;
                    v.bound = self.add(v.bound, c.bound, true)?;
                    v.projection_safe &= c.projection_safe;
                }
            }
            // Latent-effect refinement can union rows across checker passes.
            // Supporting that needs a separate envelope; do not assume a
            // source arrow is the checker-refined result in tuple programs.
            Type::Function { .. } | Type::Row(_) => return Err(UNCERTAIN.into()),
            // Header/annotation holes are not closed source evidence. The
            // checker may refine them or use a refined function result on a
            // later pass; a one-node placeholder cannot bound those clones.
            Type::Infer => return Err(UNCERTAIN.into()),
            Type::IntLiteral(_) => (),
        }
        self.bounded(v.bound)?;
        Ok(v)
    }
    fn function_value(&mut self, f: &'a Function) -> Result<Value<'a>, String> {
        if !f.generics.is_empty() {
            return Err(UNCERTAIN.into());
        }
        let mut v = self.leaf();
        for p in &f.params {
            let c = self.ty(&p.ty)?;
            v.bound = self.add(v.bound, c.bound, true)?;
            v.projection_safe &= c.projection_safe;
        }
        let c = self.ty(&f.result)?;
        v.bound = self.add(v.bound, c.bound, true)?;
        v.projection_safe &= c.projection_safe;
        for e in &f.effects.instances {
            // Include effect records, generated capability labels and argument
            // types, rather than bounding function values by their result.
            let label_bytes = f
                .name
                .len()
                .checked_mul(4)
                .and_then(|n| n.checked_add(1024))
                .ok_or(BUDGET)?;
            v.bound.bytes = v.bound.bytes.checked_add(label_bytes).ok_or(BUDGET)?;
            self.bounded(v.bound)?;
            for t in &e.args {
                let c = self.ty(t)?;
                v.bound = self.add(v.bound, c.bound, true)?;
            }
        }
        if f.effects.tail.is_some() {
            return Err(UNCERTAIN.into());
        }
        v.function = Some(f);
        Ok(v)
    }
    fn env_bytes(&mut self) -> Result<usize, String> {
        self.work = self.work.checked_add(self.env.len()).ok_or(BUDGET)?;
        if self.work > self.max_work {
            return Err("tuple admission work budget exceeded".into());
        }
        self.env.iter().try_fold(0usize, |n, (_, v)| {
            n.checked_add(v.bound.bytes)
                .and_then(|n| n.checked_add(core::mem::size_of::<(&str, Value<'_>)>()))
                .ok_or_else(|| BUDGET.into())
        })
    }
    fn memory(&mut self) -> Result<(), String> {
        let env = self.env_bytes()?;
        let bytes = self
            .retained_bytes
            .checked_add(self.snapshot_bytes)
            .and_then(|n| n.checked_add(env))
            .ok_or(BUDGET)?;
        if bytes > self.limits.max_metadata_bytes {
            return Err(BUDGET.into());
        }
        Ok(())
    }
    fn snapshot(&mut self, copies: usize) -> Result<usize, String> {
        let previous = self.snapshot_bytes;
        self.snapshot_bytes = previous
            .checked_add(self.env_bytes()?.checked_mul(copies).ok_or(BUDGET)?)
            .ok_or(BUDGET)?;
        self.memory()?;
        Ok(previous)
    }
    fn push(&mut self, name: &'a str, value: Value<'a>) -> Result<(), String> {
        self.step()?;
        let count = self.env.len().checked_add(1).ok_or(BUDGET)?;
        if count > self.limits.max_ast_nodes
            || count
                .checked_mul(core::mem::size_of::<(&str, Value<'_>)>())
                .ok_or(BUDGET)?
                > self.limits.max_metadata_bytes
        {
            return Err(BUDGET.into());
        }
        // Borrowed names and Copy numeric envelopes only; reserve after limits.
        let pending = self
            .env_bytes()?
            .checked_add(value.bound.bytes)
            .and_then(|n| n.checked_add(core::mem::size_of::<(&str, Value<'_>)>()))
            .and_then(|n| n.checked_add(self.retained_bytes))
            .and_then(|n| n.checked_add(self.snapshot_bytes))
            .ok_or(BUDGET)?;
        if pending > self.limits.max_metadata_bytes {
            return Err(BUDGET.into());
        }
        self.env.try_reserve_exact(1).map_err(|_| BUDGET)?;
        self.env.push((name, value));
        self.memory()
    }
    fn local(&mut self, name: &str) -> Result<Option<Value<'a>>, String> {
        for i in (0..self.env.len()).rev() {
            self.step()?;
            if self.env[i].0 == name {
                return Ok(Some(self.env[i].1));
            }
        }
        Ok(None)
    }
    fn known(&mut self, name: &str) -> Result<Option<&'a Function>, String> {
        let mut found = None;
        for item in self.items {
            self.step()?;
            match item {
                Item::Function(f) if f.name == name => {
                    if found.replace(f.as_ref()).is_some() {
                        return Err(UNCERTAIN.into());
                    }
                }
                Item::Effect(e) => {
                    if let Some((prefix, op)) = name.split_once("::")
                        && prefix == e.name
                    {
                        if !e.generics.is_empty() {
                            return Err(UNCERTAIN.into());
                        }
                        for f in &e.ops {
                            self.step()?;
                            if f.name == op && found.replace(f).is_some() {
                                return Err(UNCERTAIN.into());
                            }
                        }
                    }
                }
                Item::Impl {
                    ty,
                    methods,
                    generics,
                } => {
                    if let Some((prefix, method)) = name.rsplit_once("::")
                        && prefix == ty.name()
                    {
                        if !generics.is_empty() {
                            return Err(UNCERTAIN.into());
                        }
                        for f in methods {
                            self.step()?;
                            if f.name == method && found.replace(f).is_some() {
                                return Err(UNCERTAIN.into());
                            }
                        }
                    }
                }
                _ => (),
            }
        }
        Ok(found)
    }
    fn constructor(&mut self, name: &str) -> Result<Option<Ctor<'a>>, String> {
        let short = name.rsplit("::").next().ok_or(UNCERTAIN)?;
        let builtin = match short {
            "Some" | "None" if name == short || name.starts_with("Option::") => Some(("Option", 1)),
            "Ok" | "Err" if name == short || name.starts_with("Result::") => Some(("Result", 2)),
            _ => None,
        };
        let mut found: Option<Ctor<'a>> = None;
        let mut candidates = 0usize;
        for item in self.items {
            self.step()?;
            if let Item::Adt(a) = item {
                for variant in &a.variants {
                    self.step()?;
                    if short == variant.name
                        && (!name.contains("::")
                            || name
                                .strip_prefix(&a.name)
                                .is_some_and(|suffix| suffix.starts_with("::")))
                    {
                        candidates += 1;
                        // Closed alternatives: any one may be the resolution,
                        // so every candidate's field types are expected types.
                        let found = found.get_or_insert(Ctor {
                            generics: a.generics.len(),
                            fields: Vec::new(),
                            adt: &a.name,
                        });
                        found.generics = found.generics.max(a.generics.len());
                        if a.name.len() > found.adt.len() {
                            found.adt = &a.name;
                        }
                        found.fields.extend(variant.fields.iter().map(|f| &f.ty));
                    }
                }
            }
        }
        // A user variant spelled like a builtin one, or one spelling that
        // names several generic variants, resolves by the World's order.
        if (builtin.is_some() && candidates > 0)
            || (candidates > 1 && found.as_ref().is_some_and(|c| c.generics > 0))
        {
            return Err(UNCERTAIN.into());
        }
        if let Some((adt, generics)) = builtin {
            return Ok(Some(Ctor {
                generics,
                fields: Vec::new(),
                adt,
            }));
        }
        Ok(found)
    }
    /// `Named(adt, args)` where every argument is within `inner`.
    fn nominal(&self, ctor: &Ctor<'_>, inner: Bound) -> Result<Value<'a>, String> {
        let mut v = self.leaf();
        v.bound.bytes = 128usize.checked_add(ctor.adt.len()).ok_or(BUDGET)?;
        v.bound = self.bounded(v.bound)?;
        for _ in 0..ctor.generics {
            v.bound = self.add(v.bound, inner, true)?;
        }
        Ok(v)
    }
    /// A constructor application, as `construct` checks it: each field is
    /// checked against its field type with the expected arguments substituted,
    /// and each type argument is the expected argument refined by the fields.
    fn construct(
        &mut self,
        ctor: Ctor<'a>,
        fields: &[&'a Expr],
        expected: Bound,
    ) -> Result<Value<'a>, String> {
        let mut template = self.leaf().bound;
        for t in ctor.fields.iter().copied() {
            let c = self.ty(t)?;
            template = self.maximum(template, c.bound)?;
        }
        let target = if ctor.generics == 0 {
            template
        } else {
            self.substitute(template, expected)?
        };
        let mut inner = expected;
        let mut safe = true;
        for e in fields {
            let c = self.expr_in(e, target)?;
            inner = self.add(inner, c.bound, false)?;
            safe &= c.projection_safe;
        }
        let mut v = self.nominal(&ctor, inner)?;
        v.projection_safe = safe;
        Ok(v)
    }
    fn function(&mut self, f: &'a Function) -> Result<(), String> {
        self.env.clear();
        for p in &f.params {
            let mut v = self.ty(&p.ty)?;
            // An uninstantiated generic parameter cannot justify projections.
            if !f.generics.is_empty() {
                v.projection_safe = false;
            }
            self.push(&p.name, v)?;
        }
        let result = self.ty(&f.result)?;
        if let Some(body) = &f.body {
            self.expr_in(body, result.bound)?;
        }
        Ok(())
    }
    fn pattern(&mut self, p: &'a Pattern, v: Value<'a>) -> Result<(), String> {
        self.step()?;
        match &p.kind {
            PatternKind::Bind(n) => self.push(n, v)?,
            PatternKind::Tuple(ps) => {
                for p in ps {
                    self.pattern(p, v)?;
                }
            }
            PatternKind::Constructor { name, fields, .. } => {
                if self.constructor(name)?.is_none() {
                    return Err(UNCERTAIN.into());
                }
                // Actual closed fields can exceed the nominal root's size, and
                // generic fields substitute the scrutinee's type arguments.
                let mut field = v;
                field.bound = self.field_of(v.bound)?;
                field.exact = None;
                field.function = None;
                field.projection_safe &= self.fields_safe;
                for (_, p) in fields {
                    self.pattern(p, field)?;
                }
            }
            _ => (),
        }
        Ok(())
    }
    fn project(&mut self, v: Value<'a>, name: &str) -> Result<Value<'a>, String> {
        if !v.projection_safe {
            return Err(UNCERTAIN.into());
        }
        if let Some(Type::Tuple(ts)) = v.exact
            && let Some(t) = name.parse::<usize>().ok().and_then(|i| ts.get(i))
        {
            return self.ty(t);
        }
        if let Some(Type::Named(n, _)) = v.exact {
            for item in self.items {
                self.step()?;
                if let Item::Adt(a) = item
                    && &a.name == n
                    && a.generics.is_empty()
                {
                    for f in a.variants.iter().flat_map(|v| &v.fields) {
                        self.step()?;
                        if f.name == name {
                            return self.ty(&f.ty);
                        }
                    }
                }
            }
        }
        let mut result = v;
        result.bound = self.field_of(v.bound)?;
        result.exact = None;
        result.function = None;
        result.projection_safe &= self.fields_safe;
        Ok(result)
    }
    fn expr(&mut self, e: &'a Expr) -> Result<Value<'a>, String> {
        let expected = self.leaf().bound;
        self.expr_in(e, expected)
    }
    /// `expected` bounds the type the checker checks `e` against (a leaf
    /// for an inferred context). Results cloned from it stay within it.
    fn expr_in(&mut self, e: &'a Expr, expected: Bound) -> Result<Value<'a>, String> {
        self.step()?;
        let mut v = self.leaf();
        match &e.kind {
            ExprKind::Literal(_) => (),
            ExprKind::Name(n) => {
                v = if let Some(local) = self.local(n)? {
                    if local.opaque {
                        return Err(OPAQUE.into());
                    }
                    local
                } else if let Some(f) = self.known(n)? {
                    self.function_value(f)?
                } else if let Some(ctor) = self.constructor(n)? {
                    // A nullary constructor clones a matching expected type,
                    // otherwise its ADT applied to inference holes.
                    let leaf = self.leaf().bound;
                    let mut c = self.nominal(&ctor, leaf)?;
                    c.bound = self.maximum(c.bound, expected)?;
                    c
                } else {
                    v
                };
            }
            ExprKind::Tuple(es) => {
                for e in es {
                    let c = self.expr(e)?;
                    v.bound = self.add(v.bound, c.bound, true)?;
                    v.projection_safe &= c.projection_safe;
                }
            }
            ExprKind::Struct { name, fields } => {
                let Some(ctor) = self.constructor(name)? else {
                    return Err(UNCERTAIN.into());
                };
                let fields = fields.iter().map(|(_, e)| e).collect::<Vec<_>>();
                v = self.construct(ctor, &fields, expected)?;
            }
            ExprKind::Block { statements, tail } => {
                let snapshot = self.snapshot(2)?;
                let mark = self.env.len();
                for s in statements {
                    match s {
                        Statement::Let { pat, ty, value } => {
                            // Initializer sees the previous binding, never itself.
                            let value = if *ty == Type::Infer {
                                self.expr(value)?
                            } else {
                                let mut declared = self.ty(ty)?;
                                let actual = self.expr_in(value, declared.bound)?;
                                declared.bound = self.maximum(declared.bound, actual.bound)?;
                                declared.projection_safe &= actual.projection_safe;
                                declared
                            };
                            self.pattern(pat, value)?;
                        }
                        Statement::Expr(e) => {
                            self.expr(e)?;
                        }
                    }
                }
                if let Some(tail) = tail {
                    v = self.expr_in(tail, expected)?;
                }
                self.env.truncate(mark);
                self.snapshot_bytes = snapshot;
            }
            ExprKind::Call {
                callee,
                types,
                args,
            } => {
                let local = if let ExprKind::Name(n) = &callee.kind {
                    self.local(n)?
                } else {
                    None
                };
                if local.is_some_and(|l| l.opaque) {
                    return Err(OPAQUE.into());
                }
                let name = if let ExprKind::Name(n) = &callee.kind {
                    Some(n.as_str())
                } else {
                    None
                };
                // The core gives constructors priority over functions,
                // even on ambiguous or unusually qualified spellings.
                let ctor = if local.is_none()
                    && let Some(name) = name
                {
                    self.constructor(name)?
                } else {
                    None
                };
                let function = if ctor.is_some() {
                    None
                } else if let Some(local) = local {
                    local.function
                } else if let Some(n) = name {
                    self.known(n)?
                } else {
                    None
                };
                if let Some(ctor) = ctor {
                    // Explicit arguments replace the expected type with
                    // `Adt[types]`, which is also the result.
                    let expected = if types.is_empty() {
                        expected
                    } else {
                        let mut arg = self.leaf().bound;
                        for t in types {
                            let c = self.ty(t)?;
                            arg = self.maximum(arg, c.bound)?;
                        }
                        self.nominal(&ctor, arg)?.bound
                    };
                    let fields = args.iter().map(|a| &a.expr).collect::<Vec<_>>();
                    v = self.construct(ctor, &fields, expected)?;
                    v.bound = self.maximum(v.bound, expected)?;
                } else if let Some(f) = function {
                    if local.is_some() {
                        self.expr(callee)?;
                    }
                    if !f.generics.is_empty() || f.effects.tail.is_some() {
                        return Err(UNCERTAIN.into());
                    }
                    // The result is fixed by this actual closed header. Each
                    // argument is checked against a closed parameter type.
                    let mut params = self.leaf().bound;
                    for p in &f.params {
                        let c = self.ty(&p.ty)?;
                        params = self.maximum(params, c.bound)?;
                    }
                    for a in args {
                        self.expr_in(&a.expr, params)?;
                    }
                    v = self.ty(&f.result)?;
                } else if local.is_none()
                    && name.is_some_and(|n| crate::numeric::wrapping_intrinsic(n).is_some())
                {
                    for a in args {
                        let c = self.expr_in(&a.expr, expected)?;
                        v.bound = self.maximum(v.bound, c.bound)?;
                        v.projection_safe &= c.projection_safe;
                    }
                } else {
                    return Err(UNCERTAIN.into());
                }
            }
            ExprKind::Field(base, name) => {
                let base = self.expr(base)?;
                v = self.project(base, name)?;
            }
            ExprKind::Try(value) => {
                // Checked against `Result[expected, _]`; yields its first
                // argument, which lies within the operand's envelope.
                let result = Ctor {
                    generics: 2,
                    fields: Vec::new(),
                    adt: "Result",
                };
                let target = self.nominal(&result, expected)?;
                v = self.expr_in(value, target.bound)?;
                v.exact = None;
                v.function = None;
            }
            ExprKind::Handle {
                effect,
                state,
                body,
                ret,
                ops,
            } => v = self.handle(effect, state, body, ret, ops, expected)?,
            // Dynamic indexing can synthesize a repeated joined element type
            // even for ill-typed heterogeneous tuples. Its envelope is separate.
            ExprKind::Index(_, _) | ExprKind::Closure { .. } => return Err(UNCERTAIN.into()),
            ExprKind::Unary(_, e) => {
                v = self.expr_in(e, expected)?;
            }
            ExprKind::Binary(op, a, b) => {
                let a = self.expr_in(a, expected)?;
                // The checker retains before/after environment clones for
                // short-circuit RHS checking, including statically dead RHSs.
                let snapshot = if matches!(op.as_str(), "&&" | "||") {
                    self.snapshot(2)?
                } else {
                    self.snapshot_bytes
                };
                // The right operand is checked against the left one's type.
                let goal = self.maximum(expected, a.bound)?;
                let b = self.expr_in(b, goal)?;
                v.bound = self.maximum(a.bound, b.bound)?;
                v.projection_safe = a.projection_safe && b.projection_safe;
                self.snapshot_bytes = snapshot;
            }
            ExprKind::Assign(_, a, b) => {
                let place = self.expr(a)?;
                self.expr_in(b, place.bound)?;
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                self.expr(condition)?;
                let snapshot = self.snapshot(3)?;
                let mark = self.env.len();
                let a = self.expr_in(then, expected)?;
                self.env.truncate(mark);
                let b = if let Some(e) = otherwise {
                    self.expr_in(e, expected)?
                } else {
                    self.leaf()
                };
                self.env.truncate(mark);
                // Joins may combine disjoint inference holes; max alone is not
                // a bound. Sum is safe even for semantically invalid branches.
                v.bound = self.add(a.bound, b.bound, false)?;
                v.projection_safe = a.projection_safe && b.projection_safe;
                self.snapshot_bytes = snapshot;
            }
            ExprKind::Match { value, arms } => {
                let input = self.expr(value)?;
                let snapshot = self.snapshot(arms.len().checked_add(3).ok_or(BUDGET)?)?;
                for (p, e) in arms {
                    let mark = self.env.len();
                    self.pattern(p, input)?;
                    let c = self.expr_in(e, expected)?;
                    self.env.truncate(mark);
                    v.bound = self.add(v.bound, c.bound, false)?;
                    v.projection_safe &= c.projection_safe;
                }
                self.snapshot_bytes = snapshot;
            }
        }
        self.bounded(v.bound)?;
        // The checker records every expression type, even on semantic errors.
        // Charge all retained inferred envelopes before allowing checker entry.
        self.retained_nodes = self
            .retained_nodes
            .checked_add(v.bound.nodes)
            .ok_or(BUDGET)?;
        self.retained_bytes = self
            .retained_bytes
            .checked_add(v.bound.bytes)
            .ok_or(BUDGET)?;
        if self.retained_nodes > self.limits.max_type_nodes
            || self.retained_bytes > self.limits.max_metadata_bytes
        {
            return Err(BUDGET.into());
        }
        self.memory()?;
        Ok(v)
    }
    /// `handle E[A] in body with { return p => r, op(params, resume) => c }`
    /// as `Checker::handle` types it: the body is inferred; the return clause
    /// binds the body's type (paired with the state) and is checked against
    /// `expected`; the result `out` is the return clause's type or `expected`;
    /// each operation clause binds the operation's parameters with the
    /// effect's arguments substituted, then a continuation, and is checked
    /// against `out`.
    #[allow(clippy::too_many_arguments)]
    fn handle(
        &mut self,
        effect: &'a Effect,
        state: &'a Option<(Pattern, Box<Expr>)>,
        body: &'a Expr,
        ret: &'a Clause,
        ops: &'a [Clause],
        expected: Bound,
    ) -> Result<Value<'a>, String> {
        let mut args = self.leaf().bound;
        for t in &effect.args {
            let c = self.ty(t)?;
            args = self.add(args, c.bound, true)?;
        }
        let state = match state {
            Some((_, e)) => Some(self.expr(e)?),
            None => None,
        };
        // outer, clause, return and one environment per operation clause.
        let snapshot = self.snapshot(ops.len().checked_add(3).ok_or(BUDGET)?)?;
        let mark = self.env.len();
        let body = self.expr(body)?;
        let mut returned = body;
        returned.exact = None;
        returned.function = None;
        if let Some(s) = state {
            returned.bound = self.add(returned.bound, s.bound, true)?;
            returned.projection_safe &= s.projection_safe;
        }
        if let Some(p) = ret.pats.first() {
            self.pattern(p, returned)?;
        }
        let r = self.expr_in(&ret.body, expected)?;
        self.env.truncate(mark);
        let mut out = r;
        out.bound = self.maximum(r.bound, expected)?;
        out.exact = None;
        out.function = None;
        let op_types = self.effect_ops.ok_or(UNCERTAIN)?;
        let mut param = self.substitute(op_types, args)?;
        param = self.maximum(param, self.leaf().bound)?;
        if let Some(s) = state {
            param = self.maximum(param, s.bound)?;
        }
        let row = self.effect_row.ok_or(UNCERTAIN)?;
        let mut resume = self.leaf();
        resume.bound = self.add(resume.bound, param, true)?;
        resume.bound = self.add(resume.bound, out.bound, true)?;
        resume.bound = self.add(resume.bound, row, true)?;
        resume.opaque = true;
        resume.projection_safe = false;
        for op in ops {
            let mark = self.env.len();
            let arguments = op.pats.len().saturating_sub(1);
            for (i, p) in op.pats.iter().enumerate() {
                let mut value = self.leaf();
                value.bound = param;
                value.projection_safe = state.is_none_or(|s| s.projection_safe);
                self.pattern(p, if i < arguments { value } else { resume })?;
            }
            self.expr_in(&op.body, out.bound)?;
            self.env.truncate(mark);
        }
        self.snapshot_bytes = snapshot;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{AnalysisFailure, AnalysisLimits, check_source, editor_ast_guard};

    fn lens() -> AnalysisLimits {
        AnalysisLimits {
            max_ast_nodes: 8192,
            max_type_nodes: 16_384,
            max_depth: 64,
            max_diagnostics: 256,
            max_diagnostic_bytes: 262_144,
            max_metadata_bytes: 2_097_152,
            ..AnalysisLimits::default()
        }
    }
    fn guard(source: &str) -> Result<(), String> {
        let parsed = crate::parser::parse(source);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        editor_ast_guard(&parsed.module, AnalysisLimits::default())
    }

    #[test]
    fn generic_constructors_try_and_handlers_reach_the_checker() {
        for source in [
            "fn f(x: u64) -> Option[u64] { if x > 1 { Some(x) } else { None } }",
            "fn f(r: Result[u64, u32]) -> u64 { match r { Ok(n) => n, Err(_) => 0 } }",
            "fn f() -> Result[u64, u32] { Result::Ok[u64, u32](4) }",
            "copy struct Pair[A, B] { left: A, right: B }
             fn f(x: u64) -> u64 { let p = Pair { left: x, right: Some(x) }; p.left }",
            "copy enum E { Bad(u64) }
             fn g(x: u64) -> Result[u64, E] { if x > 9 { Err(Bad(x)) } else { Ok(x) } }
             fn f(x: u64) -> Result[u64, E] eff [Fail[E]] { let y = g(x)?; Ok(y) }
             fn h(x: u64) -> Result[u64, E] {
                 handle Fail[E] in { f(x) } with {
                     return value => value,
                     raise(error, resume) => Err(error),
                 }
             }",
        ] {
            let checked = check_source(source, AnalysisLimits::default());
            assert!(checked.is_ok(), "{source}: {:?}", checked.err());
        }
        // A semantic error in an admitted program is a diagnostic, not a guard.
        assert!(matches!(
            check_source(
                "fn f(x: u64) -> Option[bool] { Some(x) }",
                AnalysisLimits::default()
            ),
            Err(AnalysisFailure::Semantic { .. })
        ));
    }

    #[test]
    fn doubling_through_generic_arguments_is_refused_before_checking() {
        // Joining `Ok(a)` and `Err(a)` gives Result[A, A]: the type doubles
        // per binding, and twenty-five bindings would need 2^25 nodes.
        let mut join = "fn f(c: bool) -> bool { let a = 0;".to_owned();
        for _ in 0..25 {
            join.push_str("let a = if c { Ok(a) } else { Err(a) };");
        }
        join.push_str("c }");
        let mut pair = "copy struct P[A, B] { l: A, r: B } fn f() -> u64 { let a = 0;".to_owned();
        for _ in 0..25 {
            pair.push_str("let a = P { l: a, r: a };");
        }
        pair.push_str("0 }");
        // A nullary constructor clones its closed expected type; the
        // annotation itself is bounded by the type budget.
        let mut nested = "copy struct P[A, B] { l: A, r: B } copy struct D[T] { x: P[T, T] }
            fn f() -> u64 { let v: Option["
            .to_owned();
        for _ in 0..12 {
            nested.push_str("D[");
        }
        nested.push_str("u64");
        nested.push_str(&"]".repeat(12));
        nested.push_str("] = None; 0 }");
        for source in [join, pair] {
            let refused = guard(&source).unwrap_err();
            assert!(refused.contains("budget"), "{refused}");
            assert!(matches!(
                check_source(&source, lens()),
                Err(AnalysisFailure::Resource { .. })
            ));
        }
        assert!(guard(&nested).is_ok());
        assert!(check_source(&nested, AnalysisLimits::default()).is_ok());
        // Linear growth stays admitted: Some(Some(...)) adds one node per level.
        let mut chain = "fn f() -> u64 { let a = 0;".to_owned();
        for _ in 0..25 {
            chain.push_str("let a = Some(a);");
        }
        chain.push_str("0 }");
        assert!(guard(&chain).is_ok());
        assert!(check_source(&chain, AnalysisLimits::default()).is_ok());
    }

    #[test]
    fn continuations_and_shadowed_builtins_stay_uncertain() {
        let resume = "copy enum E { Bad(u64) }
             fn f(x: u64) -> Result[u64, E] eff [Fail[E]] { Ok(x) }
             fn h(x: u64) -> Result[u64, E] {
                 handle Fail[E] in { f(x) } with {
                     return value => value,
                     raise(error, resume) => { let k = resume; Err(error) },
                 }
             }";
        assert!(guard(resume).unwrap_err().contains("continuation"));
        let shadow = "copy enum Option[T] { Some(T), None } fn f() -> Option[u64] { Some(1) }";
        assert!(guard(shadow).unwrap_err().contains("uncertain"));
        let spelled = "copy enum M { Some(u64) } fn f() -> M { Some(1) }";
        assert!(guard(spelled).unwrap_err().contains("uncertain"));
    }

    #[test]
    fn lens_profile_checks_the_large_simulation_programs() {
        for (name, source, default) in [
            ("kv", include_str!("../../lsp/fixtures/kv.ves"), true),
            (
                "warp_ack_full",
                include_str!("../../lsp/fixtures/warp_ack_full.ves"),
                false,
            ),
        ] {
            let checked = check_source(source, lens());
            assert!(checked.is_ok(), "{name}: {:?}", checked.err());
            assert_eq!(
                check_source(source, AnalysisLimits::default()).is_ok(),
                default,
                "{name}"
            );
        }
    }
}
