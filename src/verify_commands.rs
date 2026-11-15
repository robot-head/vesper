//! Explicit Lean verification for formatter document models (`--format`) and
//! extracted Vesper source programs (`--extract`).
use crate::proof_commands::{self, LeanToolchain, Stage};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
use vesper::{check, lean, parser};
use vesper_fmt::{Limits, ProofLimits, format_for_verification};
use vesper_loom_cas::{DerivationInputs, Domain, Store};

const MODEL: &str = include_str!("../lean/StylusAndLintSafety.lean");
const SCHEMA: &str = "vesper.format-proof/1";
const EXTRACT_SCHEMA: &str = "vesper.verify-extract/1";
const EXTRACT_BUILDER: &str = "vesper-verify-extract";
const EXTRACT_BUILDER_VERSION: &str = "1";
const MAX_INPUT: usize = 128 * 1024;
const MAX_JSON: usize = 2 * 1024 * 1024;
const MAX_OLEAN_BYTES: u64 = 256 * 1024 * 1024;
const HELP: &str = "Usage: vesper verify SOURCE --format [--json | --emit-lean] [--lean PATH] [--timeout SECONDS]\n       vesper verify SOURCE --extract [--proofs FILE.lean] [--theorem Name ...] [--allow-axiom Name ...] [--olean-store DIR|default] [--json | --emit-lean] [--lean PATH] [--timeout SECONDS]\n\n--format: Kernel-check a formatter document-algebra comment-conservation model with Lean 4.35.0.\n--extract: Extract Lean 4 models from a checked .ves source, compile and replay through the Lean 4.35.0 kernel, and audit axioms (sorryAx is always forbidden).\n--proofs joins an explicit Lean proof script containing theorem proofs for the extracted model.\n--theorem selects theorem names to audit (default: theorems discovered in --proofs, or an internal model-compilation witness).\n--allow-axiom permits named axioms such as propext, Quot.sound, or Classical.choice.\n--olean-store caches and remounts audited .olean certificates in a Loom CAS directory (`default` is ~/.vesper/store).\n--emit-lean prints the generated Lean source only after compilation and kernel replay.\n--timeout bounds each Lean process (1 to 300 seconds; default 30).\nThe selected Lean toolchain and artifact parser remain trusted.\n";

#[derive(Debug)]
struct Failure {
    stage: &'static str,
    message: String,
}
impl Failure {
    fn new(stage: &'static str, message: impl std::fmt::Display) -> Self {
        let message = message.to_string();
        let mut end = message.len().min(4096);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        Self {
            stage,
            message: message[..end].to_owned(),
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Format,
    Extract,
}
struct Options {
    mode: Mode,
    source: PathBuf,
    proofs: Option<PathBuf>,
    theorems: BTreeSet<String>,
    allowed_axioms: BTreeSet<String>,
    olean_store: Option<String>,
    lean: String,
    timeout: u64,
    json: bool,
    emit_lean: bool,
}
fn options(args: Vec<String>) -> Result<Options, Failure> {
    let size = args
        .iter()
        .try_fold(0usize, |n, arg| n.checked_add(arg.len()));
    if args.len() > 300 || size.is_none_or(|n| n > 32 * 1024) {
        return Err(Failure::new(
            "config",
            "verification argument limit exceeded",
        ));
    }
    let mut source = None;
    let mut format = false;
    let mut extract = false;
    let mut proofs = None;
    let mut theorems = BTreeSet::new();
    let mut allowed_axioms = BTreeSet::new();
    let mut olean_store = None;
    let mut json = false;
    let mut emit_lean = false;
    let mut lean = None;
    let mut timeout = None;
    let mut positional = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if !positional {
            match arg.as_str() {
                "--format" => {
                    if format {
                        return Err(Failure::new("config", "duplicate --format"));
                    }
                    format = true;
                    continue;
                }
                "--extract" => {
                    if extract {
                        return Err(Failure::new("config", "duplicate --extract"));
                    }
                    extract = true;
                    continue;
                }
                "--proofs" => {
                    if proofs.is_some() {
                        return Err(Failure::new("config", "duplicate --proofs"));
                    }
                    let path = args
                        .next()
                        .ok_or_else(|| Failure::new("config", "--proofs requires a path"))?;
                    if path.len() > 4096 {
                        return Err(Failure::new("config", "invalid --proofs path"));
                    }
                    proofs = Some(PathBuf::from(path));
                    continue;
                }
                "--theorem" => {
                    let name = args
                        .next()
                        .ok_or_else(|| Failure::new("config", "--theorem requires a name"))?;
                    if !proof_commands::name_valid(&name) {
                        return Err(Failure::new(
                            "config",
                            format!("invalid theorem name '{name}'"),
                        ));
                    }
                    theorems.insert(name);
                    if theorems.len() > 128 {
                        return Err(Failure::new("config", "at most 128 theorems are supported"));
                    }
                    continue;
                }
                "--allow-axiom" => {
                    let name = args
                        .next()
                        .ok_or_else(|| Failure::new("config", "--allow-axiom requires a name"))?;
                    if !proof_commands::name_valid(&name) {
                        return Err(Failure::new(
                            "config",
                            format!("invalid axiom name '{name}'"),
                        ));
                    }
                    if matches!(
                        name.as_str(),
                        "sorryAx" | "Lean.ofReduceBool" | "Lean.trustCompiler"
                    ) {
                        return Err(Failure::new(
                            "config",
                            format!("{name} cannot be allowed by the audit policy"),
                        ));
                    }
                    allowed_axioms.insert(name);
                    continue;
                }
                "--olean-store" => {
                    if olean_store.is_some() {
                        return Err(Failure::new("config", "duplicate --olean-store"));
                    }
                    let dir = args
                        .next()
                        .ok_or_else(|| Failure::new("config", "--olean-store requires a path"))?;
                    if dir.is_empty() || dir.len() > 4096 {
                        return Err(Failure::new("config", "invalid --olean-store path"));
                    }
                    olean_store = Some(dir);
                    continue;
                }
                "--json" => {
                    json = true;
                    continue;
                }
                "--emit-lean" => {
                    emit_lean = true;
                    continue;
                }
                "--lean" => {
                    if lean.is_some() {
                        return Err(Failure::new("config", "duplicate --lean"));
                    }
                    lean = Some(
                        args.next()
                            .ok_or_else(|| Failure::new("config", "--lean requires a path"))?,
                    );
                    continue;
                }
                "--timeout" => {
                    if timeout.is_some() {
                        return Err(Failure::new("config", "duplicate --timeout"));
                    }
                    let value = args
                        .next()
                        .ok_or_else(|| Failure::new("config", "--timeout requires seconds"))?;
                    let seconds = value
                        .parse::<u64>()
                        .ok()
                        .filter(|n| (1..=300).contains(n))
                        .ok_or_else(|| {
                            Failure::new("config", "--timeout must be between 1 and 300 seconds")
                        })?;
                    timeout = Some(seconds);
                    continue;
                }
                "--" => {
                    positional = true;
                    continue;
                }
                _ if arg.starts_with('-') => {
                    return Err(Failure::new("config", "unknown verification option"));
                }
                _ => {}
            }
        }
        if arg.len() > 4096 || source.replace(PathBuf::from(arg)).is_some() {
            return Err(Failure::new(
                "config",
                "verify accepts one bounded source path",
            ));
        }
    }
    if format && extract {
        return Err(Failure::new(
            "config",
            "--format and --extract cannot be combined",
        ));
    }
    if !format && !extract {
        return Err(Failure::new(
            "config",
            "--format or --extract is required; see vesper verify --help",
        ));
    }
    if format
        && (proofs.is_some()
            || !theorems.is_empty()
            || !allowed_axioms.is_empty()
            || olean_store.is_some())
    {
        return Err(Failure::new(
            "config",
            "--proofs, --theorem, --allow-axiom, and --olean-store require --extract",
        ));
    }
    if json && emit_lean {
        return Err(Failure::new(
            "config",
            "--json and --emit-lean cannot be combined",
        ));
    }
    let lean = lean
        .or_else(|| env::var("VESPER_LEAN").ok())
        .unwrap_or_else(|| "lean".into());
    if lean.is_empty() || lean.len() > 4096 {
        return Err(Failure::new("config", "invalid Lean executable path"));
    }
    Ok(Options {
        mode: if extract { Mode::Extract } else { Mode::Format },
        source: source.ok_or_else(|| Failure::new("config", "verify requires a source file"))?,
        proofs,
        theorems,
        allowed_axioms,
        olean_store,
        lean,
        timeout: timeout.unwrap_or(30),
        json,
        emit_lean,
    })
}
fn read_source(path: &Path) -> Result<String, Failure> {
    let meta = fs::symlink_metadata(path).map_err(|e| Failure::new("io", e))?;
    if !meta.is_file() {
        return Err(Failure::new(
            "io",
            "verification requires a regular file without final symlinks",
        ));
    }
    let mut open = OpenOptions::new();
    open.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        open.custom_flags(0x0020_0000);
    }
    let file = open.open(path).map_err(|e| Failure::new("io", e))?;
    let meta = file.metadata().map_err(|e| Failure::new("io", e))?;
    if !meta.is_file() || meta.len() > MAX_INPUT as u64 {
        return Err(Failure::new(
            "resource",
            "verification requires a regular file of at most 128 KiB",
        ));
    }
    let mut bytes = Vec::new();
    file.take((MAX_INPUT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| Failure::new("io", e))?;
    if bytes.len() > MAX_INPUT {
        return Err(Failure::new(
            "resource",
            "verification source exceeds 128 KiB",
        ));
    }
    String::from_utf8(bytes).map_err(|_| Failure::new("parse", "verification source must be UTF-8"))
}
fn compile(
    toolchain: &LeanToolchain,
    stage: &Path,
    name: &str,
    timeout: u64,
) -> Result<(), Failure> {
    let search = env::join_paths([stage.to_path_buf(), toolchain.prefix.join("lib/lean")])
        .map_err(|e| Failure::new("toolchain", e))?;
    let mut command = Command::new(&toolchain.binary);
    command
        .env_clear()
        .env("LEAN_PATH", search)
        .env("LEAN_ABORT_ON_PANIC", "1")
        .current_dir(stage);
    #[cfg(windows)]
    for key in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = env::var_os(key) {
            command.env(key, value);
        }
    }
    proof_commands::execute(
        command
            .args(["-j1", "-M1024", "-o"])
            .arg(format!("{name}.olean"))
            .arg(format!("{name}.lean")),
        Duration::from_secs(timeout),
    )
    .map_err(|e| Failure::new("compile", e))?;
    Ok(())
}
#[derive(Serialize)]
struct Success {
    schema: &'static str,
    status: &'static str,
    scope: &'static str,
    source_blake3: String,
    output_blake3: String,
    input_bytes: usize,
    output_bytes: usize,
    document_nodes: usize,
    comment_tokens: usize,
    lean_version: String,
    audit: serde_json::Value,
}
#[derive(Serialize)]
struct OleanCertReport {
    store: String,
    object_id: String,
    cache_hit: bool,
    newly_recorded: bool,
}
#[derive(Serialize)]
struct ExtractSuccess {
    schema: &'static str,
    status: &'static str,
    scope: &'static str,
    source_blake3: String,
    lean_model_blake3: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    proofs_blake3: Option<String>,
    input_bytes: usize,
    totality: BTreeMap<String, String>,
    contracts: Vec<String>,
    lean_version: String,
    audit: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    olean_certificate: Option<OleanCertReport>,
}
struct Bounded(Vec<u8>);
impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self
            .0
            .len()
            .checked_add(bytes.len())
            .is_none_or(|n| n > MAX_JSON)
        {
            return Err(io::Error::other("verification JSON exceeds 2 MiB"));
        }
        self.0.try_reserve(bytes.len()).map_err(io::Error::other)?;
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn totality_label(totality: &lean::Totality) -> String {
    match totality {
        lean::Totality::Total => "total".to_owned(),
        lean::Totality::Fuel => "fuel".to_owned(),
        lean::Totality::Partial(reason) => format!("partial: {reason}"),
    }
}
fn discover_theorems(proofs: &str) -> Result<Vec<String>, Failure> {
    let mut ns_stack: Vec<String> = Vec::new();
    let mut discovered = Vec::new();
    let mut seen = BTreeSet::new();
    let mut block_comment_depth = 0usize;
    for line in proofs.lines() {
        let mut stripped = String::with_capacity(line.len());
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'-' {
                block_comment_depth += 1;
                i += 2;
                continue;
            }
            if block_comment_depth > 0
                && i + 1 < bytes.len()
                && bytes[i] == b'-'
                && bytes[i + 1] == b'/'
            {
                block_comment_depth -= 1;
                i += 2;
                continue;
            }
            if block_comment_depth == 0 {
                if i + 1 < bytes.len() && bytes[i] == b'-' && bytes[i + 1] == b'-' {
                    break;
                }
                let ch = line[i..].chars().next().unwrap();
                stripped.push(ch);
                i += ch.len_utf8();
            } else {
                let ch = line[i..].chars().next().unwrap();
                i += ch.len_utf8();
            }
        }
        let trimmed = stripped.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("namespace ") {
            let name = rest.split_whitespace().next().unwrap_or("");
            if proof_commands::name_valid(name) {
                ns_stack.push(name.to_owned());
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("end ") {
            let name = rest.split_whitespace().next().unwrap_or("");
            if ns_stack.last().is_some_and(|top| top == name) {
                ns_stack.pop();
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("theorem ") {
            let ident = rest
                .split(|c: char| c.is_whitespace() || matches!(c, ':' | '(' | '{' | '['))
                .next()
                .unwrap_or("");
            if !proof_commands::name_valid(ident) {
                return Err(Failure::new(
                    "proofs",
                    format!("unsupported theorem identifier '{ident}' in proofs file"),
                ));
            }
            let full = if ns_stack.is_empty() {
                ident.to_owned()
            } else {
                format!("{}.{}", ns_stack.join("."), ident)
            };
            if seen.insert(full.clone()) {
                discovered.push(full);
                if discovered.len() > 128 {
                    return Err(Failure::new(
                        "proofs",
                        "proofs file declares more than 128 theorems",
                    ));
                }
            }
        }
    }
    Ok(discovered)
}
fn resolve_store_dir(spec: &str) -> Result<PathBuf, Failure> {
    if spec == "default" {
        vesper::tooling::doc::certs::default_store().map_err(|e| Failure::new("store", e))
    } else {
        Ok(PathBuf::from(spec))
    }
}
fn execute_extract(options: &Options) -> Result<Vec<u8>, Failure> {
    let source = read_source(&options.source)?;
    let proofs = options.proofs.as_deref().map(read_source).transpose()?;
    let parsed = parser::parse(&source);
    if let Some(diag) = parsed.diagnostics.first() {
        return Err(Failure::new(
            "parse",
            format!(
                "{} syntax error(s); first: {} {}",
                parsed.diagnostics.len(),
                diag.code,
                diag.message
            ),
        ));
    }
    let checked = check::check(&parsed.module);
    if let Some(diag) = checked.diagnostics.first() {
        return Err(Failure::new(
            "check",
            format!(
                "{} semantic error(s); first: {} {}",
                checked.diagnostics.len(),
                diag.code,
                diag.message
            ),
        ));
    }
    let report = lean::extract_report(&checked.world).map_err(|e| Failure::new("extract", e))?;
    let extracted = report
        .module
        .render()
        .map_err(|e| Failure::new("emit", e))?;
    let source_blake3 = blake3::hash(source.as_bytes()).to_hex().to_string();
    let lean_model_blake3 = blake3::hash(extracted.as_bytes()).to_hex().to_string();
    let proofs_blake3 = proofs
        .as_deref()
        .map(|p| blake3::hash(p.as_bytes()).to_hex().to_string());
    let (bundle_text, audit_theorems) = if let Some(proof_text) = &proofs {
        let thms: Vec<String> = if options.theorems.is_empty() {
            let found = discover_theorems(proof_text)?;
            if found.is_empty() {
                return Err(Failure::new("proofs", "proofs file declares no theorems"));
            }
            found
        } else {
            options.theorems.iter().cloned().collect()
        };
        (format!("{extracted}\n{proof_text}\n"), thms)
    } else if !options.theorems.is_empty() {
        (
            extracted.clone(),
            options.theorems.iter().cloned().collect(),
        )
    } else {
        let with_witness = format!(
            "{extracted}\nnamespace VesperVerify\ntheorem model_verified : True := trivial\nend VesperVerify\n"
        );
        (with_witness, vec!["VesperVerify.model_verified".to_owned()])
    };
    let toolchain =
        proof_commands::lean_toolchain(&options.lean).map_err(|e| Failure::new("toolchain", e))?;
    let stage = Stage::new().map_err(|e| Failure::new("io", e))?;
    fs::write(stage.0.join("VesperExtract.lean"), &bundle_text)
        .map_err(|e| Failure::new("io", e))?;

    let theorems_csv = audit_theorems.join(",");
    let axioms_csv: String = options
        .allowed_axioms
        .iter()
        .cloned()
        .collect::<Vec<_>>()
        .join(",");
    let store_dir = options
        .olean_store
        .as_deref()
        .map(resolve_store_dir)
        .transpose()?;
    let opened_store = store_dir
        .as_deref()
        .map(Store::open)
        .transpose()
        .map_err(|e| Failure::new("store", e))?;
    let deriv_inputs = opened_store.as_ref().map(|_| {
        DerivationInputs::new(Domain::Artifact, EXTRACT_BUILDER, EXTRACT_BUILDER_VERSION)
            .with_option("source_blake3", source_blake3.clone())
            .with_option("lean_model_blake3", lean_model_blake3.clone())
            .with_option("proofs_blake3", proofs_blake3.as_deref().unwrap_or(""))
            .with_option("lean_version", toolchain.version.clone())
            .with_option("theorems", theorems_csv.clone())
            .with_option("allowed_axioms", axioms_csv.clone())
    });

    let mut cert_report = None;
    if let (Some(store), Some(inputs), Some(dir)) = (&opened_store, &deriv_inputs, &store_dir) {
        if let Some(cached_id) = store
            .lookup_derivation(inputs)
            .map_err(|e| Failure::new("store", e))?
        {
            let bytes = store
                .read_blob(&cached_id)
                .map_err(|e| Failure::new("store", e))?;
            if bytes.len() as u64 > MAX_OLEAN_BYTES {
                return Err(Failure::new("store", "cached .olean exceeds size limit"));
            }
            fs::write(stage.0.join("VesperExtract.olean"), &bytes)
                .map_err(|e| Failure::new("io", e))?;
            cert_report = Some(OleanCertReport {
                store: dir.display().to_string(),
                object_id: cached_id.to_string(),
                cache_hit: true,
                newly_recorded: false,
            });
        } else {
            compile(&toolchain, &stage.0, "VesperExtract", options.timeout)?;
        }
    } else {
        compile(&toolchain, &stage.0, "VesperExtract", options.timeout)?;
    }

    let theorem_refs: Vec<&str> = audit_theorems.iter().map(String::as_str).collect();
    let allowed_refs: Vec<&str> = options.allowed_axioms.iter().map(String::as_str).collect();
    let audit = proof_commands::audit_generated_with_policy(
        &stage.0,
        "VesperExtract",
        &theorem_refs,
        &allowed_refs,
        &options.lean,
        options.timeout,
    )
    .map_err(|e| Failure::new("kernel", e))?;

    if cert_report.is_none()
        && let (Some(store), Some(inputs), Some(dir)) = (&opened_store, &deriv_inputs, &store_dir)
    {
        let olean_path = stage.0.join("VesperExtract.olean");
        let meta = fs::symlink_metadata(&olean_path).map_err(|e| Failure::new("io", e))?;
        if meta.len() > MAX_OLEAN_BYTES {
            return Err(Failure::new("resource", ".olean exceeds size limit"));
        }
        let olean_bytes = fs::read(&olean_path).map_err(|e| Failure::new("io", e))?;
        let blob = store
            .put_blob(Domain::Artifact, &olean_bytes)
            .map_err(|e| Failure::new("store", e))?;
        let outcome = store
            .publish_derivation(inputs, blob.id)
            .map_err(|e| Failure::new("store", e))?;
        cert_report = Some(OleanCertReport {
            store: dir.display().to_string(),
            object_id: blob.id.to_string(),
            cache_hit: false,
            newly_recorded: outcome.newly_recorded,
        });
    }

    if options.emit_lean {
        return Ok(extracted.into_bytes());
    }
    if !options.json {
        return Ok(format!(
            "Lean extraction model: kernel checked ({} function(s), {} contract(s), {} theorem(s)).\n",
            report.totality.len(),
            report.contracts.len(),
            audit_theorems.len(),
        )
        .into_bytes());
    }
    let totality = report
        .totality
        .iter()
        .map(|(k, v)| (k.clone(), totality_label(v)))
        .collect();
    let report = ExtractSuccess {
        schema: EXTRACT_SCHEMA,
        status: "kernel_checked",
        scope: "source_lean_extraction",
        source_blake3,
        lean_model_blake3,
        proofs_blake3,
        input_bytes: source.len(),
        totality,
        contracts: report.contracts,
        lean_version: audit["lean_version"]
            .as_str()
            .ok_or_else(|| Failure::new("kernel", "audit omitted its actual Lean version"))?
            .to_owned(),
        audit,
        olean_certificate: cert_report,
    };
    let mut output = Bounded(Vec::new());
    serde_json::to_writer(&mut output, &report).map_err(|e| Failure::new("resource", e))?;
    output
        .write_all(b"\n")
        .map_err(|e| Failure::new("resource", e))?;
    Ok(output.0)
}
fn execute(options: &Options) -> Result<Vec<u8>, Failure> {
    if options.mode == Mode::Extract {
        return execute_extract(options);
    }
    let source = read_source(&options.source)?;
    let verification = format_for_verification(&source, Limits::default(), ProofLimits::default())
        .map_err(|e| Failure::new("format_model", e))?;
    let witness = verification
        .emit_lean(ProofLimits::default())
        .map_err(|e| Failure::new("emit", e))?;
    let toolchain =
        proof_commands::lean_toolchain(&options.lean).map_err(|e| Failure::new("toolchain", e))?;
    let stage = Stage::new().map_err(|e| Failure::new("io", e))?;
    fs::write(stage.0.join("StylusAndLintSafety.lean"), MODEL)
        .map_err(|e| Failure::new("io", e))?;
    fs::write(stage.0.join("FormatWitness.lean"), &witness).map_err(|e| Failure::new("io", e))?;
    compile(&toolchain, &stage.0, "StylusAndLintSafety", options.timeout)?;
    compile(&toolchain, &stage.0, "FormatWitness", options.timeout)?;
    let audit = proof_commands::audit_generated(
        &stage.0,
        "FormatWitness",
        &[
            "Vesper.StylusGenerated.comments_preserved",
            "Vesper.StylusSafety.projection_narrowing_preserves_disjointness",
        ],
        &options.lean,
        options.timeout,
    )
    .map_err(|e| Failure::new("kernel", e))?;
    if options.emit_lean {
        return Ok(witness.into_bytes());
    }
    if !options.json {
        return Ok(b"Formatter document model: kernel checked; no program-semantics or Rust-refinement claim.\n".to_vec());
    }
    let report = Success {
        schema: SCHEMA,
        status: "kernel_checked",
        scope: "formatter_document_model",
        source_blake3: blake3::Hash::from_bytes(verification.source_hash())
            .to_hex()
            .to_string(),
        output_blake3: blake3::Hash::from_bytes(verification.output_hash())
            .to_hex()
            .to_string(),
        input_bytes: source.len(),
        output_bytes: verification.output().len(),
        document_nodes: verification.model().node_count(),
        comment_tokens: verification.model().comments().len(),
        lean_version: audit["lean_version"]
            .as_str()
            .ok_or_else(|| Failure::new("kernel", "audit omitted its actual Lean version"))?
            .to_owned(),
        audit,
    };
    let mut output = Bounded(Vec::new());
    serde_json::to_writer(&mut output, &report).map_err(|e| Failure::new("resource", e))?;
    output
        .write_all(b"\n")
        .map_err(|e| Failure::new("resource", e))?;
    Ok(output.0)
}
pub fn run(args: Vec<String>) -> Result<(), String> {
    if args
        .iter()
        .take_while(|a| a.as_str() != "--")
        .any(|a| matches!(a.as_str(), "--help" | "-h"))
    {
        print!("{HELP}");
        return Ok(());
    }
    let json = args
        .iter()
        .take_while(|a| a.as_str() != "--")
        .any(|a| a == "--json");
    let extract = args
        .iter()
        .take_while(|a| a.as_str() != "--")
        .any(|a| a == "--extract");
    let result = options(args).and_then(|options| execute(&options));
    match result {
        Ok(output) => io::stdout()
            .lock()
            .write_all(&output)
            .map_err(|e| e.to_string()),
        Err(error) => {
            if json {
                let schema = if extract { EXTRACT_SCHEMA } else { SCHEMA };
                println!(
                    "{}",
                    serde_json::json!({"schema":schema,"status":"rejected","stage":error.stage,"message":error.message})
                );
            }
            let prefix = if extract {
                "Lean extraction verification failed"
            } else {
                "formatter model verification failed"
            };
            Err(format!("{prefix}: {}", error.message))
        }
    }
}
