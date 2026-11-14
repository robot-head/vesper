//! Filesystem/CLI adapter for pure documentation tooling.
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
};
/// Re-exported for the documentation tests that include this adapter.
#[allow(unused_imports)]
pub use vesper::tooling::doc::document_source;
use vesper::tooling::doc::{certs, document_with_analysis, sources};
use vesper_doc::{
    Document, Limits,
    badges::{self, Badge, CertificateLink},
    capabilities::{self, Capabilities, PackageCapabilities},
    footprints::{self, MayUseReport},
    heatmap::{self, Heatmaps},
    hoogle::{self, ImplMethod, SearchIndex, TextHit},
    modes::{self, ModeCrossings},
    package,
    visual::{self, PageExtras, Visuals},
};
pub const HELP: &str = "Usage: vesper doc FILE [--json] [-o OUTPUT | --bundle DIR] [--query QUERY.json] [--search TEXT]\n                [--wire LAYOUTS.json] [--proof-audit REPORT.json ...] [--olean-store DIR|default [--olean-root DIR]]\n       vesper doc PACKAGE_DIR (--json | --bundle DIR) [--search TEXT] [--proof-audit REPORT.json ...] [--olean-store ...]\n       vesper doc store-olean --proof-audit REPORT.json --root LEAN_BUILD_DIR --store DIR|default [--json]\n\nCheck source and generate deterministic local API documentation.\nDefault: escaped static HTML on stdout. --json: typed JSON metadata and search results.\n-o creates a new local output file; existing files are preserved. --bundle creates a new directory holding index.html,\nsearch.html (a self-contained signature search page), search-index.json and linked .olean certificates.\n--query reads a full structural JSON query (no wildcard/generic inference). --search takes a text query, e.g.\n'inout Tx.{total}, u64 -> ()', 'sink linear Tx -> u64 eff []' or a name; `_` matches any type and omitted passes,\nmodes and eff [...] are unconstrained (needs --json; the search page runs the same matcher).\nSource is limited to 128 KiB and bounded tokens/nesting/items/types; query to 32 KiB; output to 2 MiB.\nLocal intra-doc references such as [Type::method] resolve against documented checked declarations; unknown, ambiguous or unsupported references fail generation.\nGenerated private Pact helpers are excluded. Struct byte rulers describe the native eight-byte ABI, not wire formats.\nFieldTree heatmaps show each parameter's checked footprint (inout write, in read, sink consume, free siblings); a may-use\nanalysis of each body marks declared leaves it never reaches and infers the effects it may perform. Mode-crossing tables\ngive the [K-Adt] mode of each generic type per parameter mode; capability badges (Pure, Zero-Alloc, Uncancellable\nLinear I/O, Abortable Checkpoint, Constant-Time, Hard-Realtime, Deterministic Float) come from checked effect rows.\nA PACKAGE_DIR documents every .ves file of the package (Loom.toml discovery as for path-less lint) with one page per\nfile, an overview with package capabilities against the manifest's capability_ceiling, and ONE package-wide search\nindex over file-qualified functions and impl/model methods; generic functions match by instantiation.\n--wire compiles vesper-wire-layouts JSON declarations with the Bridge layout checker (explicit endianness, no implicit\npadding) into 16-byte field-span records and SVG cache-line rulers. Linear typestate graphs are rendered as SVG.\n--proof-audit (repeatable, up to 16) reads `vesper proof-audit --json` reports. An item is proved/audited only when\nevery @proof(Name) anchor in its docs names a theorem in an accepted kernel-replayed audit; otherwise unverified.\n--olean-store links each audited .olean from a Loom content-addressed store (`default` is ~/.vesper/store), bound to\nthe exact audit report and rehashed on read; store-olean records them there, or add --olean-root LEAN_BUILD_DIR to\npublish them from the proof-audit output in the same run (the bytes must match the audited size and BLAKE3). A link is content identity, not provenance.\nBadges describe Lean theorems about models, not the Rust or Vesper implementation. No network, JIT or doctest execution.\n";

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
    visuals: Visuals,
    badges: Vec<Badge>,
    proof_audits: Vec<String>,
    heatmaps: Heatmaps,
    may_use: MayUseReport,
    mode_crossings: ModeCrossings,
    capabilities: Capabilities,
    impl_methods: Vec<ImplMethod>,
    search_index: SearchIndex,
    #[serde(skip_serializing_if = "Option::is_none")]
    text_search: Option<TextSearch>,
    certificates: Vec<CertificateLink>,
}
#[derive(Serialize)]
struct TextSearch {
    query: String,
    hits: Vec<TextHit>,
}
/// One file of a package report: everything but the search index, which is
/// package-wide.
#[derive(Serialize)]
struct FileReport {
    file: String,
    href: String,
    document: Document,
    visuals: Visuals,
    badges: Vec<Badge>,
    heatmaps: Heatmaps,
    may_use: MayUseReport,
    mode_crossings: ModeCrossings,
    capabilities: Capabilities,
    impl_methods: Vec<ImplMethod>,
}
#[derive(Serialize)]
struct PackageInfo {
    name: String,
    manifest: bool,
    capability_ceiling: Option<Vec<String>>,
}
#[derive(Serialize)]
struct PackageReport {
    schema_version: u32,
    status: &'static str,
    package: PackageInfo,
    files: Vec<FileReport>,
    capabilities: PackageCapabilities,
    proof_audits: Vec<String>,
    search_index: SearchIndex,
    #[serde(skip_serializing_if = "Option::is_none")]
    text_search: Option<TextSearch>,
    certificates: Vec<CertificateLink>,
}
enum Output {
    Stdout,
    File(String),
    Bundle(PathBuf),
}
struct Rendered {
    bytes: Vec<u8>,
    output: Output,
    /// Bundle-only files: (relative path, bytes).
    extra: Vec<(String, Vec<u8>)>,
}
fn value(
    args: &mut impl Iterator<Item = String>,
    flag: &str,
    what: &str,
) -> Result<String, String> {
    args.next()
        .filter(|p| !p.starts_with('-'))
        .ok_or_else(|| format!("{flag} requires {what}"))
}
fn store_root(value: &str) -> Result<PathBuf, String> {
    if value == "default" {
        certs::default_store()
    } else {
        Ok(PathBuf::from(value))
    }
}
struct Options {
    input: String,
    output: Option<String>,
    bundle: Option<String>,
    query: Option<String>,
    text_query: Option<String>,
    wire: Option<String>,
    store: Option<String>,
    olean_root: Option<String>,
    audits: Vec<String>,
    json: bool,
}
fn options(args: Vec<String>) -> Result<Option<Options>, String> {
    let mut input = None;
    let mut output = None;
    let mut bundle = None;
    let mut query = None;
    let mut text_query = None;
    let mut wire = None;
    let mut store = None;
    let mut olean_root = None;
    let mut audits = Vec::new();
    let mut json = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let once = |slot: &Option<String>| {
            if slot.is_some() {
                Err(format!("duplicate {arg}"))
            } else {
                Ok(())
            }
        };
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{HELP}");
                return Ok(None);
            }
            "--json" => json = true,
            "-o" => {
                once(&output)?;
                output = Some(value(&mut args, "-o", "a new output path")?);
            }
            "--bundle" => {
                once(&bundle)?;
                bundle = Some(value(&mut args, "--bundle", "a new output directory")?);
            }
            "--query" => {
                once(&query)?;
                query = Some(value(&mut args, "--query", "a query JSON file")?);
            }
            "--search" => {
                once(&text_query)?;
                // Text queries may start with '-' only as '->'.
                text_query = Some(
                    args.next()
                        .filter(|p| !p.starts_with('-') || p.starts_with("->"))
                        .ok_or("--search requires a text query")?,
                );
            }
            "--wire" => {
                once(&wire)?;
                wire = Some(value(&mut args, "--wire", "a wire layout JSON file")?);
            }
            "--olean-store" => {
                once(&store)?;
                store = Some(value(
                    &mut args,
                    "--olean-store",
                    "a store directory or 'default'",
                )?);
            }
            "--olean-root" => {
                once(&olean_root)?;
                olean_root = Some(value(
                    &mut args,
                    "--olean-root",
                    "the Lean build directory",
                )?);
            }
            "--proof-audit" => {
                if audits.len() == 16 {
                    return Err("at most 16 --proof-audit reports".into());
                }
                audits.push(value(
                    &mut args,
                    "--proof-audit",
                    "a proof-audit JSON report",
                )?);
            }
            _ if arg.starts_with('-') => return Err(format!("unknown doc option '{arg}'")),
            _ => {
                if input.replace(arg).is_some() {
                    return Err("doc accepts one source file or package directory".into());
                }
            }
        }
    }
    let input = input.ok_or("doc requires a source file or package directory; see doc --help")?;
    if output.is_some() && bundle.is_some() {
        return Err("-o and --bundle are exclusive".into());
    }
    if bundle.is_some() && json {
        return Err("--bundle writes HTML and JSON files; drop --json".into());
    }
    if text_query.is_some() && !json {
        return Err("--search needs --json (the bundle's search.html runs the same query)".into());
    }
    if store.is_some() && audits.is_empty() {
        return Err("--olean-store needs at least one --proof-audit report".into());
    }
    if olean_root.is_some() && store.is_none() {
        return Err(
            "--olean-root needs --olean-store (where the certificates are published)".into(),
        );
    }
    Ok(Some(Options {
        input,
        output,
        bundle,
        query,
        text_query,
        wire,
        store,
        olean_root,
        audits,
        json,
    }))
}
type ParsedAudits = Vec<(Vec<u8>, badges::ProofAudit)>;
/// Bundle-relative path and bytes of each extra bundle file.
type BundleFiles = Vec<(String, Vec<u8>)>;
/// Read the audits and, with `--olean-root`, publish their `.olean` files to
/// the store first (the one-step form of `store-olean`), then link them.
fn audits_and_certificates(
    opts: &Options,
    bundled: bool,
) -> Result<(ParsedAudits, Vec<CertificateLink>, BundleFiles), String> {
    let mut parsed = Vec::new();
    for path in &opts.audits {
        let text = read_file(path, 8 * 1024 * 1024)?;
        let audit = badges::parse_audit(&text).map_err(|e| format!("{path}: {e}"))?;
        parsed.push((text.into_bytes(), audit));
    }
    let mut extra = Vec::new();
    let mut certificates = Vec::new();
    if let Some(store) = &opts.store {
        let root = store_root(store)?;
        if let Some(build) = &opts.olean_root {
            let cas = vesper_loom_cas::Store::open(&root).map_err(|e| e.to_string())?;
            for (bytes, audit) in &parsed {
                // Only accepted, replayed audits that inventory .olean files
                // have certificates; the rest link nothing, as without a root.
                if audit.accepted && audit.kernel_replayed && !badges::olean_files(audit).is_empty()
                {
                    certs::store_certificates(&cas, bytes, Path::new(build))?;
                }
            }
        }
        for found in certs::link_certificates(&root, &parsed)? {
            let mut link = found.link;
            if let (Some(bytes), true) = (found.bytes, bundled) {
                let name = format!("certs/{}.olean", link.blake3);
                link.href = Some(name.clone());
                if !extra.iter().any(|(n, _)| *n == name) {
                    extra.push((name, bytes));
                }
            }
            certificates.push(link);
        }
    }
    Ok((parsed, certificates, extra))
}
/// Everything computed for one checked file.
struct FileParts {
    document: Document,
    visuals: Visuals,
    badges: Vec<Badge>,
    heatmaps: Heatmaps,
    may_use: MayUseReport,
    mode_crossings: ModeCrossings,
    capabilities: Capabilities,
    impl_methods: Vec<ImplMethod>,
}
fn file_parts(
    source: &str,
    wire: &[vesper_ffi::WireLayout],
    audits: &ParsedAudits,
    certificates: &[CertificateLink],
    limits: Limits,
) -> Result<FileParts, String> {
    let (document, analysis) = document_with_analysis(source, limits)?;
    let visuals = visual::visuals(&document.types, &document.graph, &document.functions, wire)?;
    let just_audits: Vec<_> = audits.iter().map(|(_, a)| a.clone()).collect();
    let mut badges = badges::badges(&document, &just_audits)?;
    if !certificates.is_empty() {
        badges::attach_certificates(&mut badges, certificates);
    }
    let mut heatmaps = heatmap::heatmaps(&document.types, &document.functions, limits)?;
    footprints::attach(&mut heatmaps, &analysis.may_use);
    let mut capabilities = capabilities::capabilities(&document.functions, Some(&analysis.may_use));
    // Impl and model methods are callable API too: their checked rows join
    // the file's (and so the package's) capabilities under their
    // conformance-qualified path. Their bodies are not in the may-use walk.
    capabilities
        .items
        .extend(analysis.impl_methods.iter().map(|m| {
            let mut i = capabilities::item(&m.function, None);
            i.item = m.path();
            i
        }));
    capabilities.items.sort_by(|a, b| a.item.cmp(&b.item));
    Ok(FileParts {
        document,
        visuals,
        badges,
        heatmaps,
        may_use: analysis.may_use,
        mode_crossings: analysis.mode_crossings,
        capabilities,
        impl_methods: analysis.impl_methods,
    })
}
fn sections(parts: &FileParts) -> String {
    let mut out = footprints::render(&parts.may_use);
    out.push_str(&modes::render(&parts.mode_crossings));
    out.push_str(&capabilities::render(&parts.capabilities, None));
    out.push_str(&package::render_impls(&parts.impl_methods));
    out
}
fn text_search(text: Option<String>, index: &SearchIndex) -> Result<Option<TextSearch>, String> {
    match text {
        Some(text) => {
            let query = hoogle::parse_text_query(&text)
                .map_err(|e| format!("invalid search query: {e}"))?;
            Ok(Some(TextSearch {
                hits: hoogle::search(&query, index),
                query: text,
            }))
        }
        None => Ok(None),
    }
}
fn generate(args: Vec<String>) -> Result<Option<Rendered>, String> {
    let Some(opts) = options(args)? else {
        return Ok(None);
    };
    if fs::symlink_metadata(&opts.input).is_ok_and(|m| m.is_dir()) {
        return generate_package(opts).map(Some);
    }
    let limits = Limits::default();
    let source = read_file(&opts.input, limits.max_source_bytes)?;
    let wire = match &opts.wire {
        Some(path) => visual::compile_wire_input(&read_file(path, 64 * 1024)?)
            .map_err(|e| format!("{path}: {e}"))?,
        None => Vec::new(),
    };
    let (parsed, certificates, mut extra) = audits_and_certificates(&opts, opts.bundle.is_some())?;
    let parts = file_parts(&source, &wire, &parsed, &certificates, limits)?;
    let search = if let Some(path) = &opts.query {
        let text = read_file(path, 32 * 1024)?;
        let query = vesper_doc::parse_query_json(&text, limits)
            .map_err(|e| format!("invalid structural query: {e}"))?;
        Some(vesper_doc::search(
            &query,
            &parts.document.functions,
            limits,
        )?)
    } else {
        None
    };
    let search_index = hoogle::build_index(&parts.document.functions, &parts.badges, limits)?;
    let text_search = text_search(opts.text_query.clone(), &search_index)?;
    let html_sections = sections(&parts);
    let report = Report {
        schema_version: 1,
        status: "valid",
        document: parts.document,
        search,
        visuals: parts.visuals,
        badges: parts.badges,
        proof_audits: parsed.iter().map(|(_, a)| a.module.clone()).collect(),
        heatmaps: parts.heatmaps,
        may_use: parts.may_use,
        mode_crossings: parts.mode_crossings,
        capabilities: parts.capabilities,
        impl_methods: parts.impl_methods,
        search_index,
        text_search,
        certificates,
    };
    let bytes = if opts.json {
        vesper_doc::to_json(&report, limits.max_output_bytes)?
    } else {
        visual::render_codex_page(
            &report.document,
            &report,
            &report.visuals,
            &report.badges,
            parsed.len(),
            &PageExtras {
                heatmaps: Some(&report.heatmaps),
                search_href: opts.bundle.as_ref().map(|_| "search.html"),
                sections: Some(&html_sections),
            },
            limits,
        )?
    };
    if opts.bundle.is_some() {
        extra.push((
            "search.html".into(),
            hoogle::search_page(&report.search_index, "index.html")?.into_bytes(),
        ));
        extra.push((
            "search-index.json".into(),
            vesper_doc::to_json(&report.search_index, limits.max_output_bytes)?,
        ));
    }
    let output = match (opts.output, opts.bundle) {
        (Some(path), _) => Output::File(path),
        (_, Some(dir)) => Output::Bundle(PathBuf::from(dir)),
        _ => Output::Stdout,
    };
    Ok(Rendered {
        bytes,
        output,
        extra,
    }
    .into())
}
/// `vesper doc DIR`: every `.ves` file of the package (the same discovery as
/// path-less `vesper lint`), one page per file, a package overview and one
/// package-wide search index. Any file that does not check is an error.
fn generate_package(opts: Options) -> Result<Rendered, String> {
    if opts.output.is_some() || opts.query.is_some() || opts.wire.is_some() {
        return Err(
            "a package directory takes --json or --bundle (not -o, --query or --wire)".into(),
        );
    }
    if !opts.json && opts.bundle.is_none() {
        return Err("a package directory needs --json or --bundle DIR".into());
    }
    let limits = Limits::default();
    let package = sources::discover(Path::new(&opts.input), sources::SourceLimits::default())?;
    let manifest = match &package.manifest {
        Some(path) => {
            let text = read_file(&path.display().to_string(), 64 * 1024)?;
            Some(vesper_loom_calculus::parse_manifest(&text).map_err(|d| {
                format!(
                    "Loom.toml: {}",
                    d.first()
                        .map_or("invalid manifest".into(), |d| d.message.clone())
                )
            })?)
        }
        None => None,
    };
    let info = PackageInfo {
        name: manifest
            .as_ref()
            .map(|m| m.package.name.as_str().to_string())
            .unwrap_or_else(|| "(no Loom.toml)".into()),
        manifest: manifest.is_some(),
        capability_ceiling: manifest
            .as_ref()
            .map(|m| m.package.capability_ceiling.iter().cloned().collect()),
    };
    if package.files.is_empty() {
        return Err("package has no .ves sources".into());
    }
    let (parsed, certificates, mut extra) = audits_and_certificates(&opts, opts.bundle.is_some())?;
    let mut files = Vec::new();
    let mut total = 0usize;
    for path in &package.files {
        let name = package.display(path);
        let source = read_file(&path.display().to_string(), limits.max_source_bytes)
            .map_err(|e| format!("{name}: {e}"))?;
        total = total
            .checked_add(source.len())
            .filter(|n| *n <= 16 * limits.max_source_bytes)
            .ok_or("package source byte budget exceeded")?;
        let parts = file_parts(&source, &[], &parsed, &certificates, limits)
            .map_err(|e| format!("{name}: {e}"))?;
        files.push((name.clone(), url_path(&format!("files/{name}.html")), parts));
    }
    let index_files: Vec<hoogle::PackageFile<'_>> = files
        .iter()
        .map(|(file, href, p)| hoogle::PackageFile {
            file,
            href,
            functions: &p.document.functions,
            badges: &p.badges,
            impls: &p.impl_methods,
        })
        .collect();
    let search_index = hoogle::build_package_index(&index_files, limits)?;
    let items: Vec<capabilities::ItemCapabilities> = files
        .iter()
        .flat_map(|(file, _, p)| {
            p.capabilities.items.iter().map(move |i| {
                let mut i = i.clone();
                i.item = format!("{file}::{}", i.item);
                i
            })
        })
        .collect();
    let package_caps = capabilities::package(&items, info.capability_ceiling.as_deref());
    let text_search = text_search(opts.text_query.clone(), &search_index)?;
    if opts.json {
        let report = PackageReport {
            schema_version: 1,
            status: "valid",
            package: info,
            files: files
                .into_iter()
                .map(|(file, href, p)| FileReport {
                    file,
                    href,
                    document: p.document,
                    visuals: p.visuals,
                    badges: p.badges,
                    heatmaps: p.heatmaps,
                    may_use: p.may_use,
                    mode_crossings: p.mode_crossings,
                    capabilities: p.capabilities,
                    impl_methods: p.impl_methods,
                })
                .collect(),
            capabilities: package_caps,
            proof_audits: parsed.iter().map(|(_, a)| a.module.clone()).collect(),
            search_index,
            text_search,
            certificates,
        };
        return Ok(Rendered {
            bytes: vesper_doc::to_json(&report, 4 * limits.max_output_bytes)?,
            output: Output::Stdout,
            extra: vec![],
        });
    }
    let mut summaries = Vec::new();
    for (file, href, p) in &files {
        let depth = href.matches('/').count();
        let up = "../".repeat(depth);
        // Certificate links are bundle-root relative.
        let mut badges = p.badges.clone();
        for b in &mut badges {
            for c in &mut b.certificates {
                if let Some(h) = &mut c.href {
                    *h = format!("{up}{h}");
                }
            }
        }
        let search_href = format!("{up}search.html");
        let html_sections = sections(p);
        let page = visual::render_codex_page(
            &p.document,
            &serde_json::json!({"file": file}),
            &p.visuals,
            &badges,
            parsed.len(),
            &PageExtras {
                heatmaps: Some(&p.heatmaps),
                search_href: Some(&search_href),
                sections: Some(&html_sections),
            },
            limits,
        )?;
        // The page is written at the literal file name; links use its
        // percent-encoded form.
        extra.push((format!("files/{file}.html"), page));
        summaries.push(vesper_doc::package::FileSummary {
            file: file.clone(),
            href: href.clone(),
            module_docs: p.document.module_docs.clone(),
            functions: p.document.functions.len(),
            types: p.document.types.len(),
            impl_methods: p.impl_methods.len(),
        });
    }
    extra.push((
        "search.html".into(),
        hoogle::search_page(&search_index, "index.html")?.into_bytes(),
    ));
    extra.push((
        "search-index.json".into(),
        vesper_doc::to_json(&search_index, 4 * limits.max_output_bytes)?,
    ));
    let overview =
        vesper_doc::package::render_overview(&info.name, &summaries, &package_caps, "search.html");
    Ok(Rendered {
        bytes: overview.into_bytes(),
        output: Output::Bundle(PathBuf::from(opts.bundle.expect("bundle checked"))),
        extra,
    })
}
/// Percent-encode a relative path for use in an href: `/` separates
/// segments and every byte outside the unreserved set is escaped, so file
/// names holding `#`, `?`, `%` or spaces still link to their page.
fn url_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
fn create_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("cannot create new output {}: {e}", path.display()))?;
    if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(error.to_string());
    }
    Ok(())
}
/// Write a bundle into a directory that must not exist yet. On failure the
/// partial directory is removed.
fn write_bundle(dir: &Path, index: &[u8], extra: &[(String, Vec<u8>)]) -> Result<(), String> {
    fs::create_dir(dir).map_err(|e| format!("cannot create new bundle directory: {e}"))?;
    let result = (|| {
        create_new(&dir.join("index.html"), index)?;
        for (name, bytes) in extra {
            let path = dir.join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            create_new(&path, bytes)?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(dir);
    }
    result
}
fn store_olean(args: Vec<String>) -> Result<(), String> {
    let (mut audit, mut root, mut store, mut json) = (None, None, None, false);
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{HELP}");
                return Ok(());
            }
            "--json" => json = true,
            "--proof-audit" if audit.is_none() => {
                audit = Some(value(
                    &mut args,
                    "--proof-audit",
                    "a proof-audit JSON report",
                )?)
            }
            "--root" if root.is_none() => {
                root = Some(value(&mut args, "--root", "the Lean build directory")?)
            }
            "--store" if store.is_none() => {
                store = Some(value(
                    &mut args,
                    "--store",
                    "a store directory or 'default'",
                )?)
            }
            _ => return Err(format!("unexpected store-olean argument '{arg}'")),
        }
    }
    let (Some(audit), Some(root), Some(store)) = (audit, root, store) else {
        return Err("store-olean needs --proof-audit, --root and --store".into());
    };
    let report = read_file(&audit, 8 * 1024 * 1024)?;
    let store = vesper_loom_cas::Store::open(store_root(&store)?).map_err(|e| e.to_string())?;
    let stored = certs::store_certificates(&store, report.as_bytes(), Path::new(&root))?;
    if json {
        println!(
            "{}",
            serde_json::json!({"schema_version":1,"status":"stored","certificates":stored})
        );
    } else {
        for s in &stored {
            println!(
                "{} {} {}{}",
                s.module,
                s.path,
                s.object_id,
                if s.newly_recorded {
                    ""
                } else {
                    " (already recorded)"
                }
            );
        }
    }
    Ok(())
}
pub fn run(args: Vec<String>) -> Result<(), String> {
    let json = args.iter().any(|a| a == "--json");
    let result = (|| {
        if args.first().is_some_and(|a| a == "store-olean") {
            return store_olean(args[1..].to_vec());
        }
        if let Some(Rendered {
            bytes,
            output,
            extra,
        }) = generate(args)?
        {
            match output {
                Output::File(path) => create_new(Path::new(&path), &bytes)?,
                Output::Bundle(dir) => write_bundle(&dir, &bytes, &extra)?,
                Output::Stdout => std::io::stdout()
                    .lock()
                    .write_all(&bytes)
                    .map_err(|e| e.to_string())?,
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
