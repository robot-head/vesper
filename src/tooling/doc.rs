//! Pure checked-source adapter for the root-free documentation library.
pub mod certs;
mod links;
pub mod sources;

use crate::{
    ast::*,
    check::{self, World},
    layout::{Layout, LayoutKind},
    numeric, parser,
};
use std::collections::BTreeMap;
use vesper_doc::{
    Document, EffectDoc, FunctionDoc, Limits, NativeField, NativeLayout, NativeVariant, Parameter,
    Selector, State, TraitDoc, TypeDoc, TypestateGraph,
};
fn function_doc(
    world: &World,
    source: &str,
    f: &Function,
    parent_generics: &[Generic],
) -> FunctionDoc {
    let mut generics = parent_generics.to_vec();
    generics.extend(f.generics.clone());
    let bounds = generics
        .iter()
        .filter(|g| !g.row)
        .map(|g| (g.name.clone(), g.bound))
        .collect();
    let result = numeric::default_type(
        f.body
            .as_ref()
            .and_then(|_| world.function_results.get(&f.name))
            .unwrap_or(&f.result),
    );
    FunctionDoc {
        name: f.name.clone(),
        docs: vesper_doc::comments(source, f.span.start).1,
        span: f.span.clone(),
        generics,
        parameters: f
            .params
            .iter()
            .map(|p| Parameter {
                name: p.name.clone(),
                pass: p.pass,
                mode: world.mode(&p.ty, &bounds),
                ty: p.ty.clone(),
                selector: Selector::from_paths(&p.projections),
            })
            .collect(),
        result_mode: world.mode(&result, &bounds),
        result,
        declared_result: f.result.clone(),
        effects: f.effects.clone(),
        effect_resumptions: f
            .effects
            .instances
            .iter()
            .map(|e| {
                world
                    .effects
                    .get(&e.name)
                    .map(|d| if d.linear { Mode::Linear } else { Mode::Affine })
            })
            .collect(),
        unsupported: vec![],
    }
}
fn layout_budget(world: &World, ty: &Type, depth: usize, nodes: &mut usize) -> Result<(), String> {
    *nodes += 1;
    if *nodes > 4096 || depth > 32 {
        return Err("native layout expansion budget exceeded".into());
    }
    match ty {
        Type::Tuple(ts) => {
            for t in ts {
                layout_budget(world, t, depth + 1, nodes)?;
            }
        }
        Type::Named(n, _) if n == "String" => {
            *nodes = nodes
                .checked_add(world.string_capacity.div_ceil(8) + 1)
                .ok_or("native layout budget overflow")?;
            if *nodes > 4096 {
                return Err("native string layout expansion budget exceeded".into());
            }
        }
        Type::Named(n, args) => {
            if let Some(adt) = world.adts.get(n) {
                let vars = adt
                    .generics
                    .iter()
                    .zip(args)
                    .map(|(g, t)| (g.name.clone(), t.clone()))
                    .collect();
                for field in adt.variants.iter().flat_map(|v| &v.fields) {
                    layout_budget(world, &substitute(&field.ty, &vars), depth + 1, nodes)?;
                }
            }
        }
        _ => (),
    }
    Ok(())
}
fn native_layout(world: &World, adt: &Adt) -> NativeLayout {
    if !adt.generics.is_empty() {
        return NativeLayout::Unsupported {
            reason: "generic layout needs concrete type/row arguments".into(),
        };
    }
    let ty = Type::named(&adt.name);
    let result = (|| {
        layout_budget(world, &ty, 0, &mut 0)?;
        let layout = Layout::of(world, &ty)?;
        let fields = |fs: &[crate::layout::LayoutField]| {
            fs.iter()
                .map(|f| NativeField {
                    name: f.name.clone(),
                    ty: f.ty.clone(),
                    offset: f.offset,
                    bytes: f.layout.size,
                })
                .collect()
        };
        let (kind, variants) = match &layout.kind {
            LayoutKind::Scalar => ("scalar".into(), vec![]),
            LayoutKind::Product(fs) => (
                "product".into(),
                vec![NativeVariant {
                    name: adt.name.clone(),
                    fields: fields(fs),
                }],
            ),
            LayoutKind::Enum(vs) => (
                "enum_tag_8_bytes".into(),
                vs.iter()
                    .map(|v| NativeVariant {
                        name: v.name.clone(),
                        fields: fields(&v.fields),
                    })
                    .collect(),
            ),
        };
        Ok::<_, String>(NativeLayout::Available {
            alignment: 8,
            bytes: layout.size,
            kind,
            variants,
        })
    })();
    result.unwrap_or_else(|reason| NativeLayout::Unsupported { reason })
}
// Conservative nominal expansion budget before the unchanged checker computes
// structural modes. Recursive nominal dependencies are unsupported; repeated
// sibling expansion is charged, so a small binary DAG cannot explode work.
pub(super) fn surface_expansion(module: &Module, limit: usize) -> Result<(), String> {
    let adts: BTreeMap<_, _> = module
        .items
        .iter()
        .filter_map(|i| {
            if let Item::Adt(a) = i {
                Some((a.name.clone(), a))
            } else {
                None
            }
        })
        .collect();
    fn visit(
        ty: &Type,
        adts: &BTreeMap<String, &Adt>,
        path: &mut Vec<String>,
        remaining: &mut usize,
        depth: usize,
    ) -> Result<(), String> {
        *remaining = remaining
            .checked_sub(1)
            .ok_or("source nominal expansion budget exceeded")?;
        if depth > 48 {
            return Err("source nominal expansion depth exceeded".into());
        }
        match ty {
            Type::Named(n, args) => {
                for a in args {
                    visit(a, adts, path, remaining, depth + 1)?;
                }
                if path.contains(n) && adts.contains_key(n) {
                    return Err(
                        "recursive nominal dependencies are unsupported by documentation preflight"
                            .into(),
                    );
                }
                if let Some(adt) = adts.get(n) {
                    path.push(n.clone());
                    for field in adt.variants.iter().flat_map(|v| &v.fields) {
                        visit(&field.ty, adts, path, remaining, depth + 1)?;
                    }
                    path.pop();
                }
            }
            Type::Tuple(ts) => {
                for t in ts {
                    visit(t, adts, path, remaining, depth + 1)?;
                }
            }
            Type::Function { params, result, .. } => {
                for (_, t) in params {
                    visit(t, adts, path, remaining, depth + 1)?;
                }
                visit(result, adts, path, remaining, depth + 1)?;
            }
            _ => (),
        }
        Ok(())
    }
    let mut remaining = limit;
    for name in adts.keys() {
        visit(&Type::named(name), &adts, &mut vec![], &mut remaining, 0)?;
    }
    Ok(())
}
/// Body-level analyses of a checked source, beside its [`Document`].
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Analysis {
    /// May-use footprints and inferred effect rows (§40.1 1b).
    pub may_use: vesper_doc::footprints::MayUseReport,
    /// `[K-Adt]` mode-crossing tables (§40.1 1b).
    pub mode_crossings: vesper_doc::modes::ModeCrossings,
    /// Methods of source `impl` and Pact `model` declarations.
    pub impl_methods: Vec<vesper_doc::hoogle::ImplMethod>,
}

/// Every named type a signature mentions.
fn mentioned(ty: &Type, out: &mut std::collections::BTreeSet<String>) {
    match ty {
        Type::Named(n, args) => {
            out.insert(n.clone());
            args.iter().for_each(|a| mentioned(a, out));
        }
        Type::Tuple(ts) => ts.iter().for_each(|t| mentioned(t, out)),
        Type::Function { params, result, .. } => {
            params.iter().for_each(|(_, t)| mentioned(t, out));
            mentioned(result, out);
        }
        _ => (),
    }
}

fn mode_crossings(
    module: &Module,
    world: &World,
    document: &Document,
) -> vesper_doc::modes::ModeCrossings {
    use vesper_doc::modes::{self, ModeCrossings, TypeParam};
    let mut out = ModeCrossings::default();
    let local: std::collections::BTreeSet<&str> = module
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Adt(a) => Some(a.name.as_str()),
            _ => None,
        })
        .collect();
    let mut used = std::collections::BTreeSet::new();
    for f in &document.functions {
        for p in &f.parameters {
            mentioned(&p.ty, &mut used);
        }
        mentioned(&f.result, &mut used);
    }
    for t in &document.types {
        for field in t.declaration.variants.iter().flat_map(|v| &v.fields) {
            mentioned(&field.ty, &mut used);
        }
    }
    for (name, adt) in &world.adts {
        if adt.generics.is_empty() {
            continue;
        }
        let origin = if local.contains(name.as_str()) {
            "documented"
        } else if used.contains(name) {
            "builtin"
        } else {
            continue;
        };
        if adt.generics.iter().any(|g| g.row) {
            out.unsupported
                .push((name.clone(), "row parameters have no mode".into()));
            continue;
        }
        let params: Vec<TypeParam> = adt
            .generics
            .iter()
            .map(|g| TypeParam {
                name: g.name.clone(),
                bound: g.bound,
            })
            .collect();
        let ty = Type::Named(
            name.clone(),
            adt.generics.iter().map(|g| Type::named(&g.name)).collect(),
        );
        out.tables
            .push(modes::table(name, origin, adt.mode, params, |args| {
                let bounds = adt
                    .generics
                    .iter()
                    .zip(args)
                    .map(|(g, m)| (g.name.clone(), *m))
                    .collect();
                world.mode(&ty, &bounds)
            }));
    }
    out.tables.sort_by(|a, b| a.ty.cmp(&b.ty));
    out.unsupported.sort();
    out
}

fn analysis(
    module: &Module,
    world: &World,
    source: &str,
    document: &Document,
) -> Result<Analysis, String> {
    let bodies: Vec<&Function> = document
        .functions
        .iter()
        .filter_map(|f| world.functions.get(&f.name))
        .collect();
    let operations: Vec<(String, FunctionDoc)> = document
        .effects
        .iter()
        .flat_map(|e| e.operations.iter().map(|o| (e.name.clone(), o.clone())))
        .collect();
    let effect_names = world.effects.keys().cloned().collect();
    let may_use =
        vesper_doc::footprints::may_use(&bodies, &document.functions, &operations, &effect_names)?;
    let conformances: BTreeMap<&str, &vesper_doc::ConformanceInfo> = world
        .elaboration
        .conformances
        .iter()
        .map(|c| (c.path.as_str(), c))
        .collect();
    let mut impl_methods = Vec::new();
    for (generated, origin) in &world.elaboration.generated {
        let (Some(f), Some(c)) = (
            world.functions.get(generated),
            conformances.get(origin.conformance.as_str()),
        ) else {
            continue;
        };
        let mut doc = function_doc(world, source, f, &[]);
        doc.docs.clear();
        impl_methods.push(vesper_doc::hoogle::ImplMethod {
            conformance: origin.conformance.clone(),
            trait_name: origin.trait_name.clone(),
            trait_args: trait_args(module, &origin.conformance),
            method: origin.method.clone(),
            self_ty: c.self_ty.clone(),
            model: c.model.clone(),
            public: c.public,
            function: doc,
        });
    }
    impl_methods.sort_by_key(|m| m.path());
    Ok(Analysis {
        may_use,
        mode_crossings: mode_crossings(module, world, document),
        impl_methods,
    })
}

/// The `impl` key segment of a type, as the Pact frontend spells it in a
/// conformance path (`Convert[i64]` is `Convert-0-i64-2`).
fn key_segment(t: &Type) -> String {
    let join = |ts: Vec<String>| ts.join("-1-");
    match t {
        Type::Named(n, args) if args.is_empty() => n.clone(),
        Type::Named(n, args) => format!("{n}-0-{}-2", join(args.iter().map(key_segment).collect())),
        Type::Tuple(ts) if ts.is_empty() => "Unit".into(),
        Type::Tuple(ts) => format!("3-0-{}-2", join(ts.iter().map(key_segment).collect())),
        Type::Row(e) => format!(
            "4-0-{}-2",
            join(
                e.instances
                    .iter()
                    .map(|i| key_segment(&Type::Named(i.name.clone(), i.args.clone())))
                    .chain(e.tail.clone())
                    .collect()
            )
        ),
        _ => "5".into(),
    }
}

/// The trait type arguments of the conformance at `path` (an `impl` key
/// `Trait[args].Self` or a model name). Unknown conformances have none.
fn trait_args(module: &Module, path: &str) -> Vec<String> {
    let key = path.rsplit("::").next().unwrap_or(path);
    module
        .items
        .iter()
        .find_map(|item| {
            let Item::Conformance(c) = item else {
                return None;
            };
            let this = match &c.kind {
                ConformanceKind::Model { name, .. } => name.clone(),
                ConformanceKind::Canonical => {
                    let mut t = c.trait_ref.name.clone();
                    if !c.trait_ref.args.is_empty() {
                        t = key_segment(&Type::Named(t, c.trait_ref.args.clone()));
                    }
                    format!("{t}.{}", key_segment(&c.self_ty))
                }
            };
            (this == key).then(|| c.trait_ref.args.iter().map(ToString::to_string).collect())
        })
        .unwrap_or_default()
}

/// [`document_source`] plus the body-level [`Analysis`] of the same checked
/// snapshot.
pub fn document_with_analysis(
    source: &str,
    limits: Limits,
) -> Result<(Document, Analysis), String> {
    let (parsed, checked) = check_source(source, limits)?;
    let document = build_document(source, &parsed.module, &checked.world, limits)?;
    let analysis = analysis(&parsed.module, &checked.world, source, &document)?;
    Ok((document, analysis))
}

fn check_source(source: &str, limits: Limits) -> Result<(parser::Parse, check::Checked), String> {
    vesper_doc::source_preflight(source, limits)?;
    let parsed = parser::parse(source);
    if !parsed.diagnostics.is_empty() {
        return Err(format!(
            "source parse failed: {}: {}",
            parsed.diagnostics[0].code, parsed.diagnostics[0].message
        ));
    }
    if parsed.module.items.len() > limits.max_items {
        return Err("source item budget exceeded".into());
    }
    vesper_doc::ast_preflight(&parsed.module)?;
    surface_expansion(&parsed.module, limits.max_type_nodes)?;
    let checked = check::check(&parsed.module);
    if !checked.diagnostics.is_empty() {
        return Err(format!(
            "source check failed: {}: {}",
            checked.diagnostics[0].code, checked.diagnostics[0].message
        ));
    }
    Ok((parsed, checked))
}

/// Checks the complete current source before extracting metadata. Diagnostics
/// prevent any document from being returned. Callers cannot provide a boolean
/// to promote unchecked source or proof evidence.
pub fn document_source(source: &str, limits: Limits) -> Result<Document, String> {
    let (parsed, checked) = check_source(source, limits)?;
    build_document(source, &parsed.module, &checked.world, limits)
}

/// Extract metadata from an actual validated snapshot without parsing or checking again.
pub fn document_checked(
    snapshot: &super::frontend::CheckedSource,
    limits: Limits,
) -> Result<Document, String> {
    limits.validate()?;
    if snapshot.source().len() > limits.max_source_bytes
        || snapshot.parsed().module.items.len() > limits.max_items
    {
        return Err("documentation source/item budget exceeded".into());
    }
    build_document(
        snapshot.source(),
        &snapshot.parsed().module,
        snapshot.world(),
        limits,
    )
}

fn build_document(
    source: &str,
    module: &Module,
    world: &World,
    limits: Limits,
) -> Result<Document, String> {
    let limitations = vec![
        "Checked source metadata; proof badges come only from supplied proof-audit reports, otherwise unverified".into(),
        "Search matches generic functions by instantiation; row binders, open rows, unresolved and function-typed contracts are excluded".into(),
        "Struct layouts describe the native eight-byte ABI; wire layouts come only from compiled Bridge declarations; no tiled layouts".into(),
        "A terminal path is not an all-path ownership or termination proof".into(),
        "Local intra-doc links cover documented source declarations; package, generic, Self and trait-method links are unsupported".into(),
        "No doctest, JIT, WASM or semver-diff execution; .olean certificates are linked only from an explicit Loom store and prove content identity, not provenance".into(),
        "FieldTree heatmaps show declared, checker-enforced footprints; may-use footprints over-approximate body uses through parameter names (no alias analysis)".into(),
    ];
    let mut document = Document {
        schema_version: 1,
        source_bytes: source.len(),
        module_docs: vesper_doc::comments(source, source.len() + 1).0,
        doc_comments: vesper_doc::doc_comments(source),
        intra_doc_links: Default::default(),
        functions: vec![],
        types: vec![],
        effects: vec![],
        traits: vec![],
        conformances: world.elaboration.conformances.clone(),
        source_conformances: vec![],
        graph: TypestateGraph::default(),
        reachability: vec![],
        limitations,
    };
    for f in world.functions.values() {
        if !world.elaboration.generated.contains_key(&f.name) {
            document.functions.push(function_doc(world, source, f, &[]));
        }
    }
    for item in &module.items {
        match item {
            Item::Adt(a) => {
                let adt = world
                    .adts
                    .get(&a.name)
                    .ok_or("checked type metadata missing")?;
                document.types.push(TypeDoc {
                    declaration: adt.clone(),
                    concrete_mode:if adt.generics.is_empty(){Some(world.mode(&Type::named(&adt.name),&BTreeMap::new()))}else{None},
                    docs: vesper_doc::comments(source, a.span.start).1,
                    layout: native_layout(world, adt),
                });
            }
            Item::Effect(e) => {
                let resolved = world
                    .effects
                    .get(&e.name)
                    .ok_or("checked effect metadata missing")?;
                document.effects.push(EffectDoc {
                    name: resolved.name.clone(),
                    docs: vesper_doc::comments(source, e.span.start).1,
                    generics: resolved.generics.clone(),
                    linear: resolved.linear,
                    operations: resolved
                        .ops
                        .iter()
                        .map(|f| function_doc(world, source, f, &resolved.generics))
                        .collect(),
                });
            }
            Item::Trait(t) => document.traits.push(TraitDoc {
                declaration: t.clone(),
                docs: vesper_doc::comments(source, t.span.start).1,
                limitation: "Trait/model dictionary contracts are outside signature-search support"
                    .into(),
            }),
            Item::Conformance(c)=>document.source_conformances.push(vesper_doc::ConformanceDeclaration {
                kind:c.kind.clone(),generics:c.generics.clone(),self_type:c.self_ty.clone(),trait_reference:c.trait_ref.clone(),associated:c.assoc.clone(),where_clauses:c.where_clauses.clone(),docs:vesper_doc::comments(source,c.span.start).1,span:c.span.clone(),unsupported:"Source conformance head retained; dictionary instantiation is outside signature search".into(),
            }),
            _ => (),
        }
    }
    document.validate(limits)?;
    document.graph = TypestateGraph::from_functions(&document.functions, limits)?;
    // Include even unused source linear types: no API path is a visible dead end.
    for ty in &document.types {
        if ty.declaration.generics.is_empty() {
            let ty = Type::named(&ty.declaration.name);
            if world.mode(&ty, &BTreeMap::new()) == Mode::Linear {
                let identity = vesper_doc::type_identity(&ty)?;
                if !document.graph.states.iter().any(|s| s.identity == identity) {
                    document.graph.states.push(State { identity, ty });
                }
            }
        }
    }
    document
        .graph
        .states
        .sort_by(|a, b| a.identity.cmp(&b.identity));
    links::populate(&mut document, source, limits)?;
    document.validate(limits)?;
    Ok(document)
}
