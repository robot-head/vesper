//! Filesystem/CLI adapter for pure documentation tooling.
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
};
use vesper::tooling::doc::certs;
pub use vesper::tooling::doc::document_source;
use vesper_doc::{
    Document, Limits,
    badges::{self, Badge, CertificateLink},
    heatmap::{self, Heatmaps},
    hoogle::{self, SearchIndex, TextHit},
    visual::{self, PageExtras, Visuals},
};
pub const HELP: &str = "Usage: vesper doc FILE [--json] [-o OUTPUT | --bundle DIR] [--query QUERY.json] [--search TEXT]\n                [--wire LAYOUTS.json] [--proof-audit REPORT.json ...] [--olean-store DIR|default]\n       vesper doc store-olean --proof-audit REPORT.json --root LEAN_BUILD_DIR --store DIR|default [--json]\n\nCheck source and generate deterministic local API documentation.\nDefault: escaped static HTML on stdout. --json: typed JSON metadata and search results.\n-o creates a new local output file; existing files are preserved. --bundle creates a new directory holding index.html,\nsearch.html (a self-contained signature search page), search-index.json and linked .olean certificates.\n--query reads a full structural JSON query (no wildcard/generic inference). --search takes a text query, e.g.\n'inout Tx.{total}, u64 -> ()', 'sink linear Tx -> u64 eff []' or a name; `_` matches any type and omitted passes,\nmodes and eff [...] are unconstrained (needs --json; the search page runs the same matcher).\nSource is limited to 128 KiB and bounded tokens/nesting/items/types; query to 32 KiB; output to 2 MiB.\nLocal intra-doc references such as [Type::method] resolve against documented checked declarations; unknown, ambiguous or unsupported references fail generation.\nGenerated private Pact helpers are excluded. Struct byte rulers describe the native eight-byte ABI, not wire formats.\nFieldTree heatmaps show each parameter's checked footprint (inout write, in read, sink consume, free siblings).\n--wire compiles vesper-wire-layouts JSON declarations with the Bridge layout checker (explicit endianness, no implicit\npadding) into 16-byte field-span records and SVG cache-line rulers. Linear typestate graphs are rendered as SVG.\n--proof-audit (repeatable, up to 16) reads `vesper proof-audit --json` reports. An item is proved/audited only when\nevery @proof(Name) anchor in its docs names a theorem in an accepted kernel-replayed audit; otherwise unverified.\n--olean-store links each audited .olean from a Loom content-addressed store (`default` is ~/.vesper/store), bound to\nthe exact audit report and rehashed on read; store-olean records them there. A link is content identity, not provenance.\nBadges describe Lean theorems about models, not the Rust or Vesper implementation. No network, JIT or doctest execution.\n";

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
fn generate(args: Vec<String>) -> Result<Option<Rendered>, String> {
    let mut input = None;
    let mut output = None;
    let mut bundle = None;
    let mut query = None;
    let mut text_query = None;
    let mut wire = None;
    let mut store = None;
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
                    return Err("doc accepts one source file".into());
                }
            }
        }
    }
    let input = input.ok_or("doc requires a source file; see doc --help")?;
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
    let wire = match wire {
        Some(path) => visual::compile_wire_input(&read_file(&path, 64 * 1024)?)
            .map_err(|e| format!("{path}: {e}"))?,
        None => Vec::new(),
    };
    let visuals = visual::visuals(&document.types, &document.graph, &document.functions, &wire)?;
    let mut parsed = Vec::new();
    for path in &audits {
        let text = read_file(path, 8 * 1024 * 1024)?;
        let audit = badges::parse_audit(&text).map_err(|e| format!("{path}: {e}"))?;
        parsed.push((text.into_bytes(), audit));
    }
    let just_audits: Vec<_> = parsed.iter().map(|(_, a)| a.clone()).collect();
    let mut badges = badges::badges(&document, &just_audits)?;
    let mut extra = Vec::new();
    let mut certificates = Vec::new();
    if let Some(store) = &store {
        for found in certs::link_certificates(&store_root(store)?, &parsed)? {
            let mut link = found.link;
            if let (Some(bytes), Some(_)) = (found.bytes, &bundle) {
                let name = format!("certs/{}.olean", link.blake3);
                link.href = Some(name.clone());
                if !extra.iter().any(|(n, _)| *n == name) {
                    extra.push((name, bytes));
                }
            }
            certificates.push(link);
        }
        badges::attach_certificates(&mut badges, &certificates);
    }
    let heatmaps = heatmap::heatmaps(&document.types, &document.functions, limits)?;
    let search_index = hoogle::build_index(&document.functions, &badges, limits)?;
    let text_search = match text_query {
        Some(text) => {
            let query = hoogle::parse_text_query(&text)
                .map_err(|e| format!("invalid search query: {e}"))?;
            Some(TextSearch {
                hits: hoogle::search(&query, &search_index),
                query: text,
            })
        }
        None => None,
    };
    let report = Report {
        schema_version: 1,
        status: "valid",
        document,
        search,
        visuals,
        badges,
        proof_audits: parsed.iter().map(|(_, a)| a.module.clone()).collect(),
        heatmaps,
        search_index,
        text_search,
        certificates,
    };
    let bytes = if json {
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
                search_href: bundle.as_ref().map(|_| "search.html"),
            },
            limits,
        )?
    };
    if bundle.is_some() {
        extra.push((
            "search.html".into(),
            hoogle::search_page(&report.search_index, "index.html")?.into_bytes(),
        ));
        extra.push((
            "search-index.json".into(),
            vesper_doc::to_json(&report.search_index, limits.max_output_bytes)?,
        ));
    }
    let output = match (output, bundle) {
        (Some(path), _) => Output::File(path),
        (_, Some(dir)) => Output::Bundle(PathBuf::from(dir)),
        _ => Output::Stdout,
    };
    Ok(Some(Rendered {
        bytes,
        output,
        extra,
    }))
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
