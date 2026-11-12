//! Checked-World adapter for the root-free documentation/search library.
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{Read, Write},
};
use vesper::{
    ast::*,
    check::{self, World},
    layout::{Layout, LayoutKind},
    numeric, parser,
};
use vesper_doc::{
    Document, EffectDoc, FunctionDoc, Limits, NativeField, NativeLayout, NativeVariant, Parameter,
    Selector, State, TraitDoc, TypeDoc, TypestateGraph,
};

pub const HELP: &str = "Usage: vesper doc FILE [--json] [-o OUTPUT] [--query QUERY.json]\n\nCheck source and generate deterministic local API documentation.\nDefault: escaped static HTML on stdout. --json: typed JSON metadata and search results.\n-o creates a new local output file; existing files are preserved. --query reads a full structural JSON query (no wildcard/generic inference).\nSource is limited to 128 KiB and bounded tokens/nesting/items/types; query to 32 KiB; output to 2 MiB.\nGenerated private Pact helpers are excluded. Layouts describe the native eight-byte ABI, not wire formats.\nNo audited proof evidence is accepted by this command; documentation is unverified. No network, JIT or doctest execution.\n";

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
        let fields = |fs: &[vesper::layout::LayoutField]| {
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
fn surface_expansion(module: &Module, limit: usize) -> Result<(), String> {
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
/// Checks the complete current source before extracting metadata. Diagnostics
/// prevent any document from being returned. Callers cannot provide a boolean
/// to promote unchecked source or proof evidence.
pub fn document_source(source: &str, limits: Limits) -> Result<Document, String> {
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
    let world = &checked.world;
    let limitations = vec![
        "Checked source metadata; no audited artifact-bound evidence: unverified".into(),
        "Search excludes generic, open-row, unresolved and effectful-arrow binder contracts".into(),
        "Layouts describe the native eight-byte ABI, not wire or tiled layouts".into(),
        "A terminal path is not an all-path ownership or termination proof".into(),
        "No doctest, JIT, link resolution, WASM, proof certificate, or semver-diff execution"
            .into(),
    ];
    let mut document = Document {
        schema_version: 1,
        source_bytes: source.len(),
        module_docs: vesper_doc::comments(source, source.len() + 1).0,
        doc_comments: vesper_doc::doc_comments(source),
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
    for item in &parsed.module.items {
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
    document.validate(limits)?;
    Ok(document)
}
fn read_file(path: &str, limit: usize) -> Result<String, String> {
    if !fs::symlink_metadata(path)
        .map_err(|e| e.to_string())?
        .is_file()
    {
        return Err("documentation input must be a regular file without final symlinks".into());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000);
    }
    let file = options.open(path).map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return Err("documentation input byte/file limit exceeded".into());
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("documentation input byte limit exceeded".into());
    }
    String::from_utf8(bytes).map_err(|_| "documentation input is not UTF-8".into())
}
#[derive(Serialize)]
struct Report {
    schema_version: u32,
    status: &'static str,
    document: Document,
    search: Option<vesper_doc::SearchResults>,
}
struct Rendered {
    bytes: Vec<u8>,
    output: Option<String>,
}
fn generate(args: Vec<String>) -> Result<Option<Rendered>, String> {
    let mut input = None;
    let mut output = None;
    let mut query = None;
    let mut json = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{HELP}");
                return Ok(None);
            }
            "--json" => json = true,
            "-o" => {
                if output.is_some() {
                    return Err("duplicate -o".into());
                }
                output = Some(
                    args.next()
                        .filter(|p| !p.starts_with('-'))
                        .ok_or("-o requires a new output path")?,
                );
            }
            "--query" => {
                if query.is_some() {
                    return Err("duplicate --query".into());
                }
                query = Some(
                    args.next()
                        .filter(|p| !p.starts_with('-'))
                        .ok_or("--query requires a query JSON file")?,
                );
            }
            _ if arg.starts_with('-') => return Err(format!("unknown doc option '{arg}'")),
            _ => {
                if input.replace(arg).is_some() {
                    return Err("doc accepts one source file".into());
                }
            }
        }
    }
    let input = input.ok_or("doc requires a source file; see doc --help")?;
    let limits = Limits::default();
    let source = read_file(&input, limits.max_source_bytes)?;
    let document = document_source(&source, limits)?;
    let search = if let Some(path) = query {
        let text = read_file(&path, 32 * 1024)?;
        let query = vesper_doc::parse_query_json(&text, limits)
            .map_err(|e| format!("invalid structural query: {e}"))?;
        Some(vesper_doc::search(&query, &document.functions, limits)?)
    } else {
        None
    };
    let report = Report {
        schema_version: 1,
        status: "valid",
        document,
        search,
    };
    let bytes = if json {
        vesper_doc::to_json(&report, limits.max_output_bytes)?
    } else {
        vesper_doc::render_html(&report, limits)?
    };
    Ok(Some(Rendered { bytes, output }))
}
pub fn run(args: Vec<String>) -> Result<(), String> {
    let json = args.iter().any(|a| a == "--json");
    let result = (|| {
        if let Some(Rendered { bytes, output }) = generate(args)? {
            if let Some(path) = output {
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                    .map_err(|e| format!("cannot create new output: {e}"))?;
                if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
                    drop(file);
                    let _ = fs::remove_file(&path);
                    return Err(error.to_string());
                }
            } else {
                std::io::stdout()
                    .lock()
                    .write_all(&bytes)
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    })()
    .map_err(|mut message: String| {
        if message.len() > 4096 {
            let mut end = 4096;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
            message.push_str(" [truncated]");
        }
        message
    });
    if json && let Err(message) = &result {
        println!(
            "{}",
            serde_json::json!({"schema_version":1,"status":"invalid","error":message})
        );
    }
    result
}
