//! Compiler-derived authority, separate from syntax identity registration.
//! No caller World, graph, approved flag or deserialization constructs a bundle.
use crate::{ast::*, ir, tooling::frontend::CheckedSource};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
};
use vesper_forge::{
    Authority, AuthorityArg, AuthorityGraph, AuthorityNode, DefId, DefInfo, DefKind,
    DefinitionRegistry, EffectId, GraphEdge, PackageId, PackageInfo, Visibility,
};
use vesper_syntax::macro_source::{MacroId, MacroSource};
pub const SOURCE_PACKAGE: PackageId = PackageId(1);
pub const BUILTIN_PACKAGE: PackageId = PackageId(0);
pub const AUTHORITY_DOMAIN: &str = "vesper.forge.compiler-authority/1";
const NAME_BYTES: usize = 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthorityLimits {
    pub max_source_bytes: usize,
    pub max_definitions: usize,
    pub max_ast_nodes: usize,
    pub max_ir_nodes: usize,
    pub max_type_nodes: usize,
    pub max_depth: usize,
    pub max_metadata_bytes: usize,
    pub max_work: usize,
}
impl Default for AuthorityLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: 128 * 1024,
            max_definitions: 512,
            max_ast_nodes: 8192,
            max_ir_nodes: 8192,
            max_type_nodes: 8192,
            max_depth: 48,
            max_metadata_bytes: 2 * 1024 * 1024,
            max_work: 16 * 1024 * 1024,
        }
    }
}
impl AuthorityLimits {
    pub fn validate(self) -> Result<(), AuthorityFailure> {
        for (value, hard) in [
            (self.max_source_bytes, 1024 * 1024),
            (self.max_definitions, 2048),
            (self.max_ast_nodes, 32768),
            (self.max_ir_nodes, 32768),
            (self.max_type_nodes, 32768),
            (self.max_depth, 64),
            (self.max_metadata_bytes, 8 * 1024 * 1024),
            (self.max_work, 64 * 1024 * 1024),
        ] {
            if value == 0 || value > hard {
                return Err(AuthorityFailure::Resource("invalid_authority_limits"));
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthorityFailure {
    Resource(&'static str),
    Unsupported(&'static str),
    Mapping(&'static str),
    UnknownDefinition,
    InvalidPackage,
    ForeignSource,
}
impl std::fmt::Display for AuthorityFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resource(s) | Self::Unsupported(s) | Self::Mapping(s) => f.write_str(s),
            Self::UnknownDefinition => f.write_str("unknown_definition"),
            Self::InvalidPackage => f.write_str("invalid_source_package"),
            Self::ForeignSource => f.write_str("foreign_source_identity"),
        }
    }
}
impl std::error::Error for AuthorityFailure {}
fn resource(s: &'static str) -> AuthorityFailure {
    AuthorityFailure::Resource(s)
}
struct Budget {
    work: usize,
    bytes: usize,
    limits: AuthorityLimits,
    ast: usize,
    types: usize,
    bindings: usize,
    branches: usize,
    name_bytes: usize,
}
impl Budget {
    fn new(limits: AuthorityLimits) -> Result<Self, AuthorityFailure> {
        limits.validate()?;
        Ok(Self {
            work: limits.max_work,
            bytes: limits.max_metadata_bytes,
            limits,
            ast: 0,
            types: 0,
            bindings: 0,
            branches: 0,
            name_bytes: 0,
        })
    }
    fn charge(&mut self, n: usize) -> Result<(), AuthorityFailure> {
        self.work = self
            .work
            .checked_sub(n)
            .ok_or_else(|| resource("authority_work_limit"))?;
        Ok(())
    }
    fn name(&mut self, s: &str) -> Result<(), AuthorityFailure> {
        if s.is_empty() || s.len() > NAME_BYTES {
            return Err(resource("definition_name_limit"));
        }
        self.name_bytes = self.name_bytes.max(s.len());
        self.charge(s.len())
    }
    fn encoded<T: Serialize>(&mut self, value: &T) -> Result<Vec<u8>, AuthorityFailure> {
        // Two bounded passes: size before capacity allocation or cloned payloads.
        let mut counter = Counter {
            remaining: self.bytes,
            used: 0,
        };
        serde_json::to_writer(&mut counter, value)
            .map_err(|_| resource("authority_metadata_limit"))?;
        self.charge(
            counter
                .used
                .checked_mul(2)
                .ok_or_else(|| resource("metadata_overflow"))?,
        )?;
        self.bytes = self
            .bytes
            .checked_sub(counter.used)
            .ok_or_else(|| resource("authority_metadata_limit"))?;
        let mut out = Vec::new();
        out.try_reserve_exact(counter.used)
            .map_err(|_| resource("allocation_failed"))?;
        serde_json::to_writer(&mut out, value).map_err(|_| resource("authority_serialization"))?;
        Ok(out)
    }
    fn inspect<T: Serialize>(&mut self, value: &T) -> Result<usize, AuthorityFailure> {
        let mut counter = Counter {
            remaining: self.bytes,
            used: 0,
        };
        serde_json::to_writer(&mut counter, value)
            .map_err(|_| resource("authority_metadata_limit"))?;
        self.charge(counter.used)?;
        self.bytes = counter.remaining;
        Ok(counter.used)
    }
    fn count(&mut self, depth: usize, ty: bool) -> Result<(), AuthorityFailure> {
        self.charge(1)?;
        if depth > self.limits.max_depth {
            return Err(resource("authority_depth_limit"));
        }
        let (n, limit) = if ty {
            (&mut self.types, self.limits.max_type_nodes)
        } else {
            (&mut self.ast, self.limits.max_ast_nodes)
        };
        *n = n.checked_add(1).ok_or_else(|| resource("node_overflow"))?;
        if *n > limit {
            return Err(resource("authority_node_limit"));
        }
        Ok(())
    }
}
struct Counter {
    remaining: usize,
    used: usize,
}
impl Write for Counter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.remaining = self
            .remaining
            .checked_sub(b.len())
            .ok_or_else(|| std::io::Error::other("metadata"))?;
        self.used = self
            .used
            .checked_add(b.len())
            .ok_or_else(|| std::io::Error::other("metadata"))?;
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
#[derive(Serialize)]
struct ParameterHeader<'a> {
    name: &'a str,
    pass: Pass,
    ty: &'a Type,
    projections: &'a [Vec<String>],
}
#[derive(Serialize)]
struct FunctionHeader<'a> {
    name: &'a str,
    generics: &'a [Generic],
    params: Vec<ParameterHeader<'a>>,
    result: &'a Type,
    effects: &'a Effects,
}
fn header<'a>(
    name: &'a str,
    generics: &'a [Generic],
    params: &'a [Param],
    result: &'a Type,
    effects: &'a Effects,
    b: &mut Budget,
) -> Result<Vec<u8>, AuthorityFailure> {
    b.charge(params.len())?;
    for p in params {
        b.name(&p.name)?;
        type_walk(&p.ty, 0, b, false)?;
    }
    type_walk(result, 0, b, false)?;
    effect_walk(effects, 0, b, false)?;
    let h = FunctionHeader {
        name,
        generics,
        params: params
            .iter()
            .map(|p| ParameterHeader {
                name: &p.name,
                pass: p.pass,
                ty: &p.ty,
                projections: &p.projections,
            })
            .collect(),
        result,
        effects,
    };
    b.encoded(&h)
}
fn function_header(f: &Function, b: &mut Budget) -> Result<Vec<u8>, AuthorityFailure> {
    header(&f.name, &f.generics, &f.params, &f.result, &f.effects, b)
}
#[derive(Serialize)]
struct AdtHeader<'a> {
    name: &'a str,
    mode: Mode,
    generics: &'a [Generic],
    variants: &'a [Variant],
    is_enum: bool,
}
fn adt_header(a: &Adt, b: &mut Budget) -> Result<Vec<u8>, AuthorityFailure> {
    for f in a.variants.iter().flat_map(|v| &v.fields) {
        type_walk(&f.ty, 0, b, false)?;
    }
    b.encoded(&AdtHeader {
        name: &a.name,
        mode: a.mode,
        generics: &a.generics,
        variants: &a.variants,
        is_enum: a.is_enum,
    })
}
#[derive(Serialize)]
struct EffectHeader<'a> {
    name: &'a str,
    generics: &'a [Generic],
    linear: bool,
    ops: Vec<Vec<u8>>,
}
fn effect_header(e: &EffectDecl, b: &mut Budget) -> Result<Vec<u8>, AuthorityFailure> {
    let mut ops = Vec::new();
    if e.ops.len() > b.limits.max_definitions {
        return Err(resource("effect_operation_limit"));
    }
    for f in &e.ops {
        ops.push(function_header(f, b)?);
    }
    b.encoded(&EffectHeader {
        name: &e.name,
        generics: &e.generics,
        linear: e.linear,
        ops,
    })
}
/// Syntax-derived identity only. Constructed exclusively from sealed original
/// MacroSource; no graph, authority, World or checked-program status is stored.
#[derive(Debug)]
pub struct SourceBindings {
    package: String,
    source_hash: [u8; 32],
    identity: [u8; 32],
    registry: DefinitionRegistry,
    definitions: BTreeMap<String, DefId>,
    names: BTreeSet<String>,
    macros: Vec<DefId>,
    functions: BTreeMap<String, Vec<u8>>,
    adts: BTreeMap<String, Vec<u8>>,
    effects: BTreeMap<String, Vec<u8>>,
    type_arities: BTreeMap<String, (PackageId, usize)>,
    effect_generics: BTreeMap<String, usize>,
    work_used: usize,
    metadata_used: usize,
}
impl SourceBindings {
    pub fn registry(&self) -> &DefinitionRegistry {
        &self.registry
    }
    pub fn core_name(&self, def: DefId) -> Result<&str, AuthorityFailure> {
        let d = self
            .registry
            .definition(def)
            .ok_or(AuthorityFailure::UnknownDefinition)?;
        if !matches!(d.kind, DefKind::Function | DefKind::EffectOp) {
            return Err(AuthorityFailure::Unsupported("not_a_callable_definition"));
        }
        Ok(&d.path)
    }
    pub fn kind(&self, def: DefId) -> Result<DefKind, AuthorityFailure> {
        self.registry
            .definition(def)
            .map(|d| d.kind)
            .ok_or(AuthorityFailure::UnknownDefinition)
    }
    pub fn def_id(&self, name: &str) -> Option<DefId> {
        self.definitions.get(name).copied()
    }
    pub fn macro_def(&self, id: MacroId) -> Result<DefId, AuthorityFailure> {
        if id.source_hash() != self.source_hash {
            return Err(AuthorityFailure::ForeignSource);
        }
        self.macros
            .get(id.index())
            .copied()
            .ok_or(AuthorityFailure::UnknownDefinition)
    }
    pub fn source_package(&self) -> PackageId {
        SOURCE_PACKAGE
    }
    pub fn source_hash(&self) -> [u8; 32] {
        self.source_hash
    }
    pub fn identity(&self) -> [u8; 32] {
        self.identity
    }
    pub fn global_names(&self) -> impl Iterator<Item = &str> {
        self.names.iter().map(String::as_str)
    }
    pub fn effect_id(&self, name: &str) -> Result<EffectId, AuthorityFailure> {
        if matches!(name, "Fail" | "builtin::Fail" | "@vesper/builtin::Fail") {
            return Ok(EffectId::new(BUILTIN_PACKAGE, "Fail"));
        }
        let prefix = format!("{}::", self.package);
        let name = name.strip_prefix(&prefix).unwrap_or(name);
        if name != "Fail" && self.effect_generics.contains_key(name) {
            Ok(EffectId::new(SOURCE_PACKAGE, name))
        } else {
            Err(AuthorityFailure::UnknownDefinition)
        }
    }
}
fn builtin_types() -> BTreeMap<String, (PackageId, usize)> {
    [
        "u8",
        "u32",
        "u64",
        "i64",
        "f64",
        "bool",
        "Unit",
        "Never",
        "String",
        "RuntimeError",
        "ArithOverflow",
    ]
    .into_iter()
    .map(|n| (n.into(), (BUILTIN_PACKAGE, 0)))
    .chain([
        ("Option".into(), (BUILTIN_PACKAGE, 1)),
        ("Result".into(), (BUILTIN_PACKAGE, 2)),
    ])
    .collect()
}
fn reserved(name: &str) -> bool {
    builtin_types().contains_key(name)
        || name == "Fail"
        || crate::numeric::wrapping_intrinsic(name).is_some()
}
fn add_name(
    names: &mut BTreeSet<String>,
    name: &str,
    b: &mut Budget,
) -> Result<(), AuthorityFailure> {
    b.name(name)?;
    names.insert(name.into());
    for part in name.split("::") {
        names.insert(part.into());
    }
    Ok(())
}
fn insert_def(
    defs: &mut BTreeMap<String, DefKind>,
    name: &str,
    kind: DefKind,
    b: &mut Budget,
) -> Result<(), AuthorityFailure> {
    b.name(name)?;
    if defs.len() >= b.limits.max_definitions {
        return Err(resource("definition_limit"));
    }
    if defs.insert(name.into(), kind).is_some() {
        return Err(AuthorityFailure::Mapping("duplicate_definition"));
    }
    Ok(())
}
/// Register syntax identities before expansion. This deliberately performs no
/// checking and cannot authorize any operation; derive() is a separate stage.
pub fn bind_source(
    source: &MacroSource,
    package: &str,
    limits: AuthorityLimits,
) -> Result<SourceBindings, AuthorityFailure> {
    let mut b = Budget::new(limits)?;
    if source.source().len() > limits.max_source_bytes {
        return Err(resource("source_limit"));
    }
    if package.len() > 256 {
        return Err(AuthorityFailure::InvalidPackage);
    }
    let package = vesper_loom_calculus::PackageName::parse(package)
        .map_err(|_| AuthorityFailure::InvalidPackage)?
        .as_str()
        .to_owned();
    if package == "@vesper/builtin" {
        return Err(AuthorityFailure::InvalidPackage);
    }
    b.charge(source.source().len())?;
    let hash = *blake3::hash(source.source().as_bytes()).as_bytes();
    if hash != source.source_hash() {
        return Err(AuthorityFailure::ForeignSource);
    }
    let support = source.support_items();
    if !support.model_uses.is_empty() || !support.trait_bounds.is_empty() {
        return Err(AuthorityFailure::Unsupported("source_model_context"));
    }
    if source.functions().len() + source.definitions().len() + support.items.len()
        > limits.max_definitions
    {
        return Err(resource("definition_limit"));
    }
    let mut defs = BTreeMap::new();
    let mut names = BTreeSet::new();
    let mut functions = BTreeMap::new();
    let mut adts = BTreeMap::new();
    let mut effects = BTreeMap::new();
    let mut effect_generics = BTreeMap::from([("Fail".into(), 1)]);
    let mut type_arities = builtin_types();
    for f in source.functions() {
        let h = &f.header;
        if reserved(&h.name) {
            return Err(AuthorityFailure::Mapping("reserved_definition"));
        }
        insert_def(&mut defs, &h.name, DefKind::Function, &mut b)?;
        functions.insert(
            h.name.clone(),
            header(
                &h.name,
                &h.generics,
                &h.params,
                &h.result,
                &h.effects,
                &mut b,
            )?,
        );
    }
    for item in &support.items {
        match item {
            Item::Function(f) => {
                let parsed = source
                    .functions()
                    .iter()
                    .find(|x| x.span == f.span && x.header.name == f.name)
                    .ok_or(AuthorityFailure::Mapping(
                        "support_function_not_in_inventory",
                    ))?;
                if !parsed.invocations.is_empty()
                    || functions.get(&f.name) != Some(&function_header(f, &mut b)?)
                {
                    return Err(AuthorityFailure::Mapping("support_header_mismatch"));
                }
                ast_walk(f, &mut b, false)?;
            }
            Item::Adt(a) => {
                if reserved(&a.name) || defs.contains_key(&a.name) || adts.contains_key(&a.name) {
                    return Err(AuthorityFailure::Mapping("duplicate_or_reserved_type"));
                }
                b.name(&a.name)?;
                type_arities.insert(a.name.clone(), (SOURCE_PACKAGE, a.generics.len()));
                adts.insert(a.name.clone(), adt_header(a, &mut b)?);
                add_name(&mut names, &a.name, &mut b)?;
                for v in &a.variants {
                    add_name(&mut names, &v.name, &mut b)?;
                }
            }
            Item::Effect(e) => {
                if reserved(&e.name) || adts.contains_key(&e.name) {
                    return Err(AuthorityFailure::Mapping("reserved_effect"));
                }
                insert_def(&mut defs, &e.name, DefKind::Effect, &mut b)?;
                effect_generics.insert(e.name.clone(), e.generics.len());
                effects.insert(e.name.clone(), effect_header(e, &mut b)?);
                for op in &e.ops {
                    insert_def(
                        &mut defs,
                        &format!("{}::{}", e.name, op.name),
                        DefKind::EffectOp,
                        &mut b,
                    )?;
                }
            }
            _ => return Err(AuthorityFailure::Unsupported("source_definition_kind")),
        }
    }
    let mut macro_names = Vec::new();
    for d in source.definitions() {
        if d.id.source_hash() != hash || d.id.index() != macro_names.len() {
            return Err(AuthorityFailure::ForeignSource);
        }
        let name = source
            .source()
            .get(d.name_span.clone())
            .ok_or(AuthorityFailure::ForeignSource)?;
        if reserved(name) || adts.contains_key(name) {
            return Err(AuthorityFailure::Mapping("reserved_macro"));
        }
        insert_def(&mut defs, name, DefKind::Macro, &mut b)?;
        macro_names.push(name.to_owned());
    }
    if defs
        .len()
        .checked_add(5)
        .is_none_or(|n| n > limits.max_definitions)
    {
        return Err(resource("definition_limit"));
    }
    let mut registry = DefinitionRegistry::new();
    registry
        .add_package(PackageInfo {
            id: BUILTIN_PACKAGE,
            name: "@vesper/builtin".into(),
            dependencies: BTreeSet::new(),
        })
        .map_err(|_| AuthorityFailure::Mapping("builtin_registry"))?;
    registry
        .add_package(PackageInfo {
            id: SOURCE_PACKAGE,
            name: package.clone(),
            dependencies: BTreeSet::from([BUILTIN_PACKAGE]),
        })
        .map_err(|_| AuthorityFailure::Mapping("source_registry"))?;
    let mut definitions = BTreeMap::new();
    let builtin = BTreeMap::from([
        ("Fail", DefKind::Effect),
        ("Fail::raise", DefKind::EffectOp),
        ("wrapping_add", DefKind::Function),
        ("wrapping_sub", DefKind::Function),
        ("wrapping_mul", DefKind::Function),
    ]);
    for (i, (name, kind)) in builtin.into_iter().enumerate() {
        if defs.contains_key(name) {
            return Err(AuthorityFailure::Mapping("reserved_definition"));
        }
        let id = DefId {
            package: BUILTIN_PACKAGE,
            item: i as u32,
        };
        registry
            .add_definition(DefInfo {
                id,
                path: name.into(),
                kind,
                visibility: Visibility::Public,
            })
            .map_err(|_| AuthorityFailure::Mapping("builtin_registry"))?;
        definitions.insert(name.into(), id);
        add_name(&mut names, name, &mut b)?;
    }
    for (i, (name, kind)) in defs.into_iter().enumerate() {
        let id = DefId {
            package: SOURCE_PACKAGE,
            item: u32::try_from(i).map_err(|_| resource("definition_id_overflow"))?,
        };
        registry
            .add_definition(DefInfo {
                id,
                path: name.clone(),
                kind,
                visibility: Visibility::Public,
            })
            .map_err(|_| AuthorityFailure::Mapping("source_registry"))?;
        add_name(&mut names, &name, &mut b)?;
        definitions.insert(name, id);
    }
    for name in [
        "None",
        "Some",
        "Ok",
        "Err",
        "DivisionByZero",
        "IndexOutOfBounds",
    ] {
        add_name(&mut names, name, &mut b)?;
    }
    for name in type_arities.keys() {
        add_name(&mut names, name, &mut b)?;
    }
    let macros = macro_names.iter().map(|n| definitions[n]).collect();
    let identity = *blake3::hash(&b.encoded(&(
        AUTHORITY_DOMAIN,
        &package,
        hash,
        &registry,
        &functions,
        &adts,
        &effects,
    ))?)
    .as_bytes();
    Ok(SourceBindings {
        package,
        source_hash: hash,
        identity,
        registry,
        definitions,
        names,
        macros,
        functions,
        adts,
        effects,
        type_arities,
        effect_generics,
        work_used: limits.max_work - b.work,
        metadata_used: limits.max_metadata_bytes - b.bytes,
    })
}
fn effect_walk(
    e: &Effects,
    depth: usize,
    b: &mut Budget,
    closed: bool,
) -> Result<(), AuthorityFailure> {
    if closed && e.tail.is_some() {
        return Err(AuthorityFailure::Unsupported("open_effect_row"));
    }
    b.count(depth, false)?;
    for e in &e.instances {
        b.name(&e.name)?;
        for t in &e.args {
            type_walk(t, depth + 1, b, closed)?;
        }
    }
    Ok(())
}
fn type_walk(t: &Type, depth: usize, b: &mut Budget, closed: bool) -> Result<(), AuthorityFailure> {
    b.count(depth, true)?;
    match t {
        Type::Named(n, args) => {
            b.name(n)?;
            for t in args {
                type_walk(t, depth + 1, b, closed)?;
            }
        }
        Type::Tuple(ts) => {
            for t in ts {
                type_walk(t, depth + 1, b, closed)?;
            }
        }
        Type::Function {
            params,
            result,
            effects,
            ..
        } => {
            if closed {
                return Err(AuthorityFailure::Unsupported("higher_order_type"));
            }
            for (_, t) in params {
                type_walk(t, depth + 1, b, false)?;
            }
            type_walk(result, depth + 1, b, false)?;
            effect_walk(effects, depth + 1, b, false)?;
        }
        Type::Row(e) => {
            if closed {
                return Err(AuthorityFailure::Unsupported("row_type"));
            }
            effect_walk(e, depth + 1, b, false)?;
        }
        Type::Infer if closed => return Err(AuthorityFailure::Unsupported("unresolved_type")),
        _ => (),
    }
    Ok(())
}
fn ast_walk(
    f: &Function,
    b: &mut Budget,
    profile: bool,
) -> Result<BTreeSet<(usize, usize)>, AuthorityFailure> {
    if profile && (!f.generics.is_empty() || f.body.is_none()) {
        return Err(AuthorityFailure::Unsupported(
            "generic_or_missing_function_body",
        ));
    }
    b.bindings = b
        .bindings
        .checked_add(f.params.len())
        .ok_or_else(|| resource("binding_count_overflow"))?;
    for p in &f.params {
        type_walk(&p.ty, 0, b, profile)?;
    }
    type_walk(&f.result, 0, b, profile)?;
    effect_walk(&f.effects, 0, b, profile)?;
    let mut stack: Vec<_> = f.body.iter().map(|e| (e, 0usize)).collect();
    let mut callees = BTreeSet::new();
    while let Some((e, depth)) = stack.pop() {
        b.count(depth, false)?;
        let next = depth + 1;
        match &e.kind {
            ExprKind::Literal(_) | ExprKind::Name(_) => (),
            ExprKind::Tuple(es) => stack.extend(es.iter().map(|e| (e, next))),
            ExprKind::Struct { fields, .. } => stack.extend(fields.iter().map(|(_, e)| (e, next))),
            ExprKind::Block { statements, tail } => {
                for s in statements {
                    match s {
                        Statement::Let { ty, value, pat } => {
                            type_walk(ty, next, b, false)?;
                            pattern_walk(pat, next, b)?;
                            stack.push((value, next));
                        }
                        Statement::Expr(e) => stack.push((e, next)),
                    }
                }
                if let Some(e) = tail {
                    stack.push((e, next));
                }
            }
            ExprKind::Call {
                callee,
                types,
                args,
            } => {
                if profile && !matches!(callee.kind, ExprKind::Name(_)) {
                    return Err(AuthorityFailure::Unsupported("non_static_call_syntax"));
                }
                callees.insert((callee.span.start, callee.span.end));
                stack.push((callee, next));
                for t in types {
                    type_walk(t, next, b, profile)?;
                }
                stack.extend(args.iter().map(|a| (&a.expr, next)));
            }
            ExprKind::Field(e, _) | ExprKind::Unary(_, e) => stack.push((e, next)),
            ExprKind::Try(e) => {
                if profile {
                    return Err(AuthorityFailure::Unsupported("try_authority_not_supported"));
                }
                stack.push((e, next));
            }
            ExprKind::Index(a, c) | ExprKind::Binary(_, a, c) | ExprKind::Assign(_, a, c) => {
                stack.push((a, next));
                stack.push((c, next));
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                b.branches = b
                    .branches
                    .checked_add(1)
                    .ok_or_else(|| resource("branch_count_overflow"))?;
                stack.push((condition, next));
                stack.push((then, next));
                if let Some(e) = otherwise {
                    stack.push((e, next));
                }
            }
            ExprKind::Match { value, arms } => {
                b.branches = b
                    .branches
                    .checked_add(arms.len())
                    .ok_or_else(|| resource("branch_count_overflow"))?;
                stack.push((value, next));
                for (p, e) in arms {
                    pattern_walk(p, next, b)?;
                    stack.push((e, next));
                }
            }
            ExprKind::Closure {
                params,
                result,
                effects,
                body,
                ..
            } => {
                if profile {
                    return Err(AuthorityFailure::Unsupported(
                        "closure_authority_not_supported",
                    ));
                }
                for p in params {
                    type_walk(&p.ty, next, b, false)?;
                }
                type_walk(result, next, b, false)?;
                effect_walk(effects, next, b, false)?;
                stack.push((body, next));
            }
            ExprKind::Handle {
                effect,
                state,
                body,
                ret,
                ops,
            } => {
                if profile {
                    return Err(AuthorityFailure::Unsupported(
                        "handler_authority_not_supported",
                    ));
                }
                for t in &effect.args {
                    type_walk(t, next, b, false)?;
                }
                if let Some((p, e)) = state {
                    pattern_walk(p, next, b)?;
                    stack.push((e, next));
                }
                stack.push((body, next));
                stack.push((&ret.body, next));
                for op in ops {
                    stack.push((&op.body, next));
                }
            }
        }
    }
    Ok(callees)
}
fn pattern_walk(p: &Pattern, depth: usize, b: &mut Budget) -> Result<(), AuthorityFailure> {
    b.count(depth, false)?;
    match &p.kind {
        PatternKind::Bind(n) => {
            b.bindings = b
                .bindings
                .checked_add(1)
                .ok_or_else(|| resource("binding_count_overflow"))?;
            b.name(n)?;
        }
        PatternKind::Tuple(ps) => {
            for p in ps {
                pattern_walk(p, depth + 1, b)?;
            }
        }
        PatternKind::Constructor { name, fields, .. } => {
            b.name(name)?;
            for (_, p) in fields {
                pattern_walk(p, depth + 1, b)?;
            }
        }
        _ => (),
    };
    Ok(())
}
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum QualifiedType<'a> {
    Named {
        package: u32,
        name: &'a str,
        args: Vec<QualifiedType<'a>>,
    },
    Tuple {
        items: Vec<QualifiedType<'a>>,
    },
}
fn qualified<'a>(
    t: &'a Type,
    arities: &BTreeMap<String, (PackageId, usize)>,
    depth: usize,
    b: &mut Budget,
) -> Result<QualifiedType<'a>, AuthorityFailure> {
    b.count(depth, true)?;
    match t {
        Type::Named(name, args) => {
            let (pkg, arity) = arities
                .get(name)
                .ok_or(AuthorityFailure::Unsupported("unknown_qualified_type"))?;
            if *arity != args.len() {
                return Err(AuthorityFailure::Unsupported("qualified_type_arity"));
            }
            let mut ts = Vec::new();
            b.charge(args.len())?;
            for t in args {
                ts.push(qualified(t, arities, depth + 1, b)?);
            }
            Ok(QualifiedType::Named {
                package: pkg.0,
                name,
                args: ts,
            })
        }
        Type::Tuple(types) => {
            b.charge(types.len())?;
            let mut items = Vec::new();
            for t in types {
                items.push(qualified(t, arities, depth + 1, b)?);
            }
            Ok(QualifiedType::Tuple { items })
        }
        _ => Err(AuthorityFailure::Unsupported("non_concrete_authority_type")),
    }
}
fn concrete_authority(
    bindings: &SourceBindings,
    effect: &str,
    args: &[&Type],
    b: &mut Budget,
) -> Result<Authority, AuthorityFailure> {
    let effect_id = bindings.effect_id(effect)?;
    let arity = *bindings
        .effect_generics
        .get(&effect_id.name)
        .ok_or(AuthorityFailure::UnknownDefinition)?;
    if args.len() != arity {
        return Err(AuthorityFailure::Unsupported("effect_argument_arity"));
    }
    let mut out = Authority::new(effect_id);
    for ty in args {
        let q = qualified(ty, &bindings.type_arities, 0, b)?;
        let encoded = b.encoded(&("vesper.forge.authority-type/1", q))?;
        out.args.push(AuthorityArg::Text(
            String::from_utf8(encoded).map_err(|_| AuthorityFailure::Mapping("type_encoding"))?,
        ));
    }
    Ok(out)
}
/// Actual full-candidate compiler authority. Construction is private and always
/// derives fresh graph requirements from current checked bodies/IR.
#[derive(Debug)]
pub struct CheckedAuthorityBundle {
    registry: DefinitionRegistry,
    graph: AuthorityGraph,
    checked_hash: [u8; 32],
    bindings_identity: [u8; 32],
    identity: [u8; 32],
    unsupported: BTreeSet<DefId>,
}
impl CheckedAuthorityBundle {
    pub fn registry(&self) -> &DefinitionRegistry {
        &self.registry
    }
    pub fn graph(&self) -> &AuthorityGraph {
        &self.graph
    }
    pub fn checked_source_hash(&self) -> [u8; 32] {
        self.checked_hash
    }
    pub fn bindings_identity(&self) -> [u8; 32] {
        self.bindings_identity
    }
    pub fn identity(&self) -> [u8; 32] {
        self.identity
    }
    pub fn unsupported_emitted_def(&self, def: DefId) -> bool {
        self.registry.definition(def).is_none() || self.unsupported.contains(&def)
    }
}
fn compare_mapping(
    checked: &CheckedSource,
    bindings: &SourceBindings,
    b: &mut Budget,
) -> Result<(), AuthorityFailure> {
    let world = checked.world();
    if !world.elaboration.generated.is_empty()
        || !world.elaboration.traits.is_empty()
        || !world.elaboration.conformances.is_empty()
        || world.elaboration.package.is_some()
        || world.elaboration.revision.is_some()
        || !checked.parsed().module.model_uses.is_empty()
        || !checked.parsed().module.trait_bounds.is_empty()
    {
        return Err(AuthorityFailure::Unsupported("elaborated_or_model_context"));
    }
    if world.functions.len() != bindings.functions.len()
        || world.functions.keys().ne(bindings.functions.keys())
    {
        return Err(AuthorityFailure::Mapping("checked_function_mapping"));
    }
    for (name, f) in &world.functions {
        if Some(&function_header(f, b)?) != bindings.functions.get(name) {
            return Err(AuthorityFailure::Mapping("checked_function_header"));
        }
    }
    let user_adts = world
        .adts
        .iter()
        .filter(|(n, _)| {
            !matches!(
                n.as_str(),
                "Option" | "Result" | "RuntimeError" | "ArithOverflow"
            )
        })
        .collect::<Vec<_>>();
    if user_adts.len() != bindings.adts.len() {
        return Err(AuthorityFailure::Mapping("checked_type_mapping"));
    }
    for (name, a) in user_adts {
        if Some(&adt_header(a, b)?) != bindings.adts.get(name) {
            return Err(AuthorityFailure::Mapping("checked_type_header"));
        }
        if !a.generics.is_empty() {
            return Err(AuthorityFailure::Unsupported("generic_data_context"));
        }
    }
    if world.effects.len() != bindings.effects.len() + 1 {
        return Err(AuthorityFailure::Mapping("checked_effect_mapping"));
    }
    for (name, e) in &world.effects {
        if name == "Fail" {
            continue;
        }
        if Some(&effect_header(e, b)?) != bindings.effects.get(name) {
            return Err(AuthorityFailure::Mapping("checked_effect_header"));
        }
        if e.generics.iter().any(|g| g.row)
            || e.ops
                .iter()
                .any(|op| !op.generics.is_empty() || op.body.is_some())
        {
            return Err(AuthorityFailure::Unsupported(
                "operation_local_generic_context",
            ));
        }
    }
    for item in &checked.parsed().module.items {
        if !matches!(item, Item::Function(_) | Item::Adt(_) | Item::Effect(_)) {
            return Err(AuthorityFailure::Unsupported("unsupported_source_context"));
        }
    }
    Ok(())
}
fn failure_node(
    bindings: &SourceBindings,
    name: &str,
    b: &mut Budget,
) -> Result<Authority, AuthorityFailure> {
    let ty = Type::named(name);
    concrete_authority(bindings, "Fail", &[&ty], b)
}

/// Validate declaration identity against the ACTUAL full checked expanded
/// candidate, then lower/traverse its bounded current bodies. Header equality
/// alone is never used to reuse an older authority graph.
pub fn derive(
    checked: &CheckedSource,
    bindings: &SourceBindings,
    limits: AuthorityLimits,
) -> Result<CheckedAuthorityBundle, AuthorityFailure> {
    let mut b = Budget::new(limits)?;
    b.charge(bindings.work_used)?;
    b.bytes = b
        .bytes
        .checked_sub(bindings.metadata_used)
        .ok_or_else(|| resource("authority_metadata_limit"))?;
    if checked.source().len() > limits.max_source_bytes {
        return Err(resource("checked_source_limit"));
    }
    if bindings.definitions.len() > limits.max_definitions {
        return Err(resource("definition_limit"));
    }
    b.charge(checked.source().len())?;
    compare_mapping(checked, bindings, &mut b)?;
    let world = checked.world();
    let mut static_callees = BTreeSet::new();
    for f in world.functions.values() {
        static_callees.extend(ast_walk(f, &mut b, true)?);
    }
    let mut max_type_bytes = 1usize;
    for (key, ty) in &checked.checked().expression_types {
        type_walk(ty, 0, &mut b, !static_callees.contains(key))?;
        max_type_bytes = max_type_bytes.max(b.inspect(ty)?);
    }
    for (key, ty) in &world.types {
        type_walk(ty, 0, &mut b, !static_callees.contains(key))?;
        max_type_bytes = max_type_bytes.max(b.inspect(ty)?);
    }
    for ty in world.function_results.values() {
        type_walk(ty, 0, &mut b, true)?;
        max_type_bytes = max_type_bytes.max(b.inspect(ty)?);
    }
    for vars in world.calls.values() {
        for ty in vars.values() {
            type_walk(ty, 0, &mut b, true)?;
        }
    }
    // Preflight every owned body and current typed map before ir::lower clones.
    b.inspect(&world.functions)?;
    b.inspect(&world.function_results)?;
    let allowance = b
        .ast
        .checked_mul(16)
        .and_then(|n| {
            b.bindings
                .checked_mul(b.branches)
                .and_then(|p| p.checked_mul(8))
                .and_then(|p| n.checked_add(p))
        })
        .and_then(|n| {
            world
                .functions
                .len()
                .checked_mul(8)
                .and_then(|p| n.checked_add(p))
        })
        .ok_or_else(|| resource("lowering_allowance_overflow"))?;
    if allowance > limits.max_ir_nodes {
        return Err(resource("ir_preflight_limit"));
    }
    let type_allowance = allowance
        .checked_mul(max_type_bytes)
        .ok_or_else(|| resource("lowering_allowance_overflow"))?;
    b.charge(type_allowance)?;
    b.bytes = b
        .bytes
        .checked_sub(type_allowance)
        .ok_or_else(|| resource("ir_type_preflight_limit"))?;
    // Lower snapshots lexical name maps at blocks/match arms. Charge a
    // conservative all-bindings/all-AST envelope before any of those clones.
    let scope_allowance = b
        .bindings
        .checked_mul(b.ast)
        .and_then(|n| {
            b.name_bytes
                .checked_mul(2)
                .and_then(|v| v.checked_add(32))
                .and_then(|v| n.checked_mul(v))
        })
        .ok_or_else(|| resource("lowering_scope_allowance_overflow"))?;
    b.charge(scope_allowance)?;
    b.bytes = b
        .bytes
        .checked_sub(scope_allowance)
        .ok_or_else(|| resource("ir_scope_preflight_limit"))?;
    let program = ir::lower(checked.checked());
    if program.functions.len() != world.functions.len() {
        return Err(AuthorityFailure::Unsupported("synthetic_function_ir"));
    }
    let mut nodes = Vec::new();
    let mut ir_nodes = 0usize;
    let mut seen_failures = BTreeSet::new();
    let mut unsupported = bindings
        .definitions
        .values()
        .copied()
        .filter(|d| !matches!(bindings.kind(*d), Ok(DefKind::Function | DefKind::EffectOp)))
        .collect::<BTreeSet<_>>();
    for (name, generics) in &bindings.effect_generics {
        if *generics > 0 {
            for (path, def) in &bindings.definitions {
                if path.strip_prefix(name).is_some_and(|p| p.starts_with("::")) {
                    unsupported.insert(*def);
                }
            }
        }
    }
    // Compiler-known wrapping intrinsics are pure, with no user-provided aliases.
    for (name, _) in crate::numeric::WRAPPING_INTRINSICS {
        let def = bindings
            .def_id(name)
            .ok_or(AuthorityFailure::UnknownDefinition)?;
        nodes.push(AuthorityNode::new(def));
    }
    for (name, effect) in &world.effects {
        if !effect.generics.is_empty() {
            continue;
        }
        for op in &effect.ops {
            let def = bindings
                .def_id(&format!("{name}::{}", op.name))
                .ok_or(AuthorityFailure::UnknownDefinition)?;
            nodes.push(AuthorityNode::new(def).direct(concrete_authority(
                bindings,
                name,
                &[],
                &mut b,
            )?));
        }
    }
    for (name, function) in &program.functions {
        let source = world
            .functions
            .get(name)
            .ok_or(AuthorityFailure::Mapping("ir_function_mapping"))?;
        let mut node = AuthorityNode::new(
            bindings
                .def_id(name)
                .ok_or(AuthorityFailure::UnknownDefinition)?,
        );
        for block in &function.blocks {
            ir_nodes = ir_nodes
                .checked_add(1)
                .ok_or_else(|| resource("ir_count_overflow"))?;
            if ir_nodes > limits.max_ir_nodes {
                return Err(resource("ir_node_limit"));
            }
            for instruction in &block.instructions {
                ir_nodes = ir_nodes
                    .checked_add(1)
                    .ok_or_else(|| resource("ir_count_overflow"))?;
                if ir_nodes > limits.max_ir_nodes {
                    return Err(resource("ir_node_limit"));
                }
                b.charge(1)?;
                let key = (instruction.span.start, instruction.span.end);
                let runtime = world.partial_ops.contains(&key);
                let overflow = world.overflow_ops.contains(&key);
                if runtime {
                    node.direct
                        .insert(failure_node(bindings, "RuntimeError", &mut b)?);
                    seen_failures.insert(key);
                }
                if overflow {
                    node.direct
                        .insert(failure_node(bindings, "ArithOverflow", &mut b)?);
                    seen_failures.insert(key);
                }
                if instruction.fallible
                    && !runtime
                    && !overflow
                    && !matches!(instruction.op, ir::Op::Call { .. })
                {
                    return Err(AuthorityFailure::Unsupported(
                        "unknown_implicit_failure_kind",
                    ));
                }
                match &instruction.op {
                    ir::Op::Call { callee, .. } => {
                        if world.functions.contains_key(callee)
                            || crate::numeric::wrapping_intrinsic(callee).is_some()
                        {
                            node.edges.push(GraphEdge::Call(
                                bindings
                                    .def_id(callee)
                                    .ok_or(AuthorityFailure::UnknownDefinition)?,
                            ));
                        } else {
                            let (effect, op) = callee
                                .split_once("::")
                                .ok_or(AuthorityFailure::Unsupported("unknown_ir_callee"))?;
                            let decl = world
                                .effects
                                .get(effect)
                                .ok_or(AuthorityFailure::Unsupported("unknown_ir_effect"))?;
                            if !decl.ops.iter().any(|f| f.name == op) {
                                return Err(AuthorityFailure::Unsupported("unknown_ir_operation"));
                            }
                            let vars = world.calls.get(&key);
                            let mut args = Vec::new();
                            for generic in &decl.generics {
                                if generic.row {
                                    return Err(AuthorityFailure::Unsupported(
                                        "row_effect_argument",
                                    ));
                                }
                                args.push(vars.and_then(|v| v.get(&generic.name)).ok_or(
                                    AuthorityFailure::Unsupported("unresolved_effect_argument"),
                                )?);
                            }
                            node.direct
                                .insert(concrete_authority(bindings, effect, &args, &mut b)?);
                        }
                    }
                    ir::Op::Indirect { .. }
                    | ir::Op::Closure { .. }
                    | ir::Op::Handle { .. }
                    | ir::Op::Try(_) => {
                        return Err(AuthorityFailure::Unsupported("dynamic_or_residual_ir"));
                    }
                    _ => (),
                }
            }
        }
        // Any implicit marker must be an actual instruction in this original body.
        let body = source
            .body
            .as_ref()
            .ok_or(AuthorityFailure::Unsupported("missing_body"))?;
        for key in world.partial_ops.iter().chain(&world.overflow_ops) {
            if body.span.start <= key.0 && key.1 <= body.span.end && !seen_failures.contains(key) {
                return Err(AuthorityFailure::Mapping("implicit_failure_missing_ir"));
            }
        }
        nodes.push(node);
    }
    if world
        .partial_ops
        .iter()
        .chain(&world.overflow_ops)
        .any(|key| !seen_failures.contains(key))
    {
        return Err(AuthorityFailure::Mapping("unowned_implicit_failure"));
    }
    b.inspect(&program)?;
    let checked_hash = checked.source_hash();
    let identity = *blake3::hash(&b.encoded(&(
        AUTHORITY_DOMAIN,
        &bindings.package,
        bindings.identity,
        checked_hash,
        &nodes,
    ))?)
    .as_bytes();
    let mut graph = AuthorityGraph::new(format!(
        "{AUTHORITY_DOMAIN}:{}",
        blake3::Hash::from_bytes(identity).to_hex()
    ));
    for node in nodes {
        graph
            .add_node(node)
            .map_err(|_| AuthorityFailure::Mapping("duplicate_graph_node"))?;
    }
    // Registry cloning is admitted only after the bounded source/mapping pass.
    b.inspect(&bindings.registry)?;
    Ok(CheckedAuthorityBundle {
        registry: bindings.registry.clone(),
        graph,
        checked_hash,
        bindings_identity: bindings.identity,
        identity,
        unsupported,
    })
}
