//! A numeric admission envelope, never a substitute inferred `Type` or World.
//! The ordinary checker still decides typing, ownership and effects. This runs
//! only after the existing structural gate, and before its first Type clone.
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
}
const UNCERTAIN: &str = "uncertain tuple inference is unsupported before checking";
const BUDGET: &str = "inferred tuple type/metadata budget exceeded before checking";

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
                continue;
            }
            for field in adt.variants.iter().flat_map(|v| &v.fields) {
                let v = a.ty(&field.ty)?;
                a.fields = a.maximum(a.fields, v.bound)?;
                a.fields_safe &= v.projection_safe;
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
                    self.nominal_arity(name, args.len())?;
                    v.bound.bytes = 128usize.checked_add(name.len()).ok_or(BUDGET)?;
                }
                v.projection_safe = !matches!(t, Type::Named(_, ts) if !ts.is_empty());
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
            Type::Infer | Type::IntLiteral(_) => (),
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
    fn constructor(&mut self, name: &str) -> Result<bool, String> {
        let short = name.rsplit("::").next().ok_or(UNCERTAIN)?;
        if (matches!(short, "Some" | "None") && (name == short || name.starts_with("Option::")))
            || (matches!(short, "Ok" | "Err") && (name == short || name.starts_with("Result::")))
        {
            return Err(UNCERTAIN.into());
        }
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
                        if !a.generics.is_empty() {
                            return Err(UNCERTAIN.into());
                        }
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
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
        self.ty(&f.result)?;
        if let Some(body) = &f.body {
            self.expr(body)?;
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
                if !self.constructor(name)? {
                    return Err(UNCERTAIN.into());
                }
                // Actual closed fields can exceed the nominal root's size.
                let mut field = v;
                field.bound = self.maximum(v.bound, self.fields)?;
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
                {
                    if !a.generics.is_empty() {
                        return Err(UNCERTAIN.into());
                    }
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
        result.bound = self.maximum(v.bound, self.fields)?;
        result.exact = None;
        result.function = None;
        result.projection_safe &= self.fields_safe;
        Ok(result)
    }
    fn expr(&mut self, e: &'a Expr) -> Result<Value<'a>, String> {
        self.step()?;
        let mut v = self.leaf();
        match &e.kind {
            ExprKind::Literal(_) => (),
            ExprKind::Name(n) => {
                v = if let Some(local) = self.local(n)? {
                    local
                } else if let Some(f) = self.known(n)? {
                    self.function_value(f)?
                } else {
                    self.constructor(n)?;
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
                if !self.constructor(name)? {
                    return Err(UNCERTAIN.into());
                }
                for (_, e) in fields {
                    self.expr(e)?;
                }
            }
            ExprKind::Block { statements, tail } => {
                let snapshot = self.snapshot(2)?;
                let mark = self.env.len();
                for s in statements {
                    match s {
                        Statement::Let { pat, ty, value } => {
                            // Initializer sees the previous binding, never itself.
                            let actual = self.expr(value)?;
                            let value = if *ty == Type::Infer {
                                actual
                            } else {
                                let mut declared = self.ty(ty)?;
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
                    v = self.expr(tail)?;
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
                let name = if let ExprKind::Name(n) = &callee.kind {
                    Some(n.as_str())
                } else {
                    None
                };
                if local.is_none()
                    && let Some(name) = name
                {
                    // The core gives constructors priority over functions,
                    // even on ambiguous or unusually qualified spellings.
                    self.constructor(name)?;
                }
                let function = if let Some(local) = local {
                    local.function
                } else if let Some(n) = name {
                    self.known(n)?
                } else {
                    None
                };
                if let Some(f) = function {
                    if local.is_some() {
                        self.expr(callee)?;
                    }
                    if !f.generics.is_empty() || f.effects.tail.is_some() {
                        return Err(UNCERTAIN.into());
                    }
                    // The result is fixed by this actual closed header. Arguments
                    // may be ill-typed; they still receive their own envelopes.
                    for a in args {
                        self.expr(&a.expr)?;
                    }
                    v = self.ty(&f.result)?;
                } else if local.is_none()
                    && name.is_some_and(|n| crate::numeric::wrapping_intrinsic(n).is_some())
                {
                    for a in args {
                        let c = self.expr(&a.expr)?;
                        v.bound = self.maximum(v.bound, c.bound)?;
                        v.projection_safe &= c.projection_safe;
                    }
                } else if local.is_none() && name.is_some() && self.constructor(name.unwrap())? {
                    if !types.is_empty() {
                        return Err(UNCERTAIN.into());
                    }
                    for a in args {
                        self.expr(&a.expr)?;
                    }
                } else {
                    return Err(UNCERTAIN.into());
                }
            }
            ExprKind::Field(base, name) => {
                let base = self.expr(base)?;
                v = self.project(base, name)?;
            }
            // Dynamic indexing can synthesize a repeated joined element type
            // even for ill-typed heterogeneous tuples. Its envelope is separate.
            ExprKind::Index(_, _)
            | ExprKind::Try(_)
            | ExprKind::Closure { .. }
            | ExprKind::Handle { .. } => return Err(UNCERTAIN.into()),
            ExprKind::Unary(_, e) => {
                v = self.expr(e)?;
            }
            ExprKind::Binary(op, a, b) => {
                let a = self.expr(a)?;
                // The checker retains before/after environment clones for
                // short-circuit RHS checking, including statically dead RHSs.
                let snapshot = if matches!(op.as_str(), "&&" | "||") {
                    self.snapshot(2)?
                } else {
                    self.snapshot_bytes
                };
                let b = self.expr(b)?;
                v.bound = self.maximum(a.bound, b.bound)?;
                v.projection_safe = a.projection_safe && b.projection_safe;
                self.snapshot_bytes = snapshot;
            }
            ExprKind::Assign(_, a, b) => {
                self.expr(a)?;
                self.expr(b)?;
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                self.expr(condition)?;
                let snapshot = self.snapshot(3)?;
                let mark = self.env.len();
                let a = self.expr(then)?;
                self.env.truncate(mark);
                let b = if let Some(e) = otherwise {
                    self.expr(e)?
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
                    let c = self.expr(e)?;
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
}
