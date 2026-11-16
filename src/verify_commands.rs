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
use vesper_loom_cas::{DerivationInputs, Domain, Store, blob_id};

const MODEL: &str = include_str!("../lean/StylusAndLintSafety.lean");
const SCHEMA: &str = "vesper.format-proof/1";
const EXTRACT_SCHEMA: &str = "vesper.verify-extract/1";
const EXTRACT_BUILDER: &str = "vesper-verify-extract";
const EXTRACT_BUILDER_VERSION: &str = "1";
const MAX_INPUT: usize = 128 * 1024;
const MAX_JSON: usize = 2 * 1024 * 1024;
const MAX_OLEAN_BYTES: u64 = 256 * 1024 * 1024;
const HELP: &str = "Usage: vesper verify SOURCE --format [--json | --emit-lean] [--lean PATH] [--timeout SECONDS]\n       vesper verify SOURCE --extract [--proofs FILE.lean] [--theorem Name ...] [--contract-proof Function=Theorem --contract-witness Function=Theorem ...] [--allow-axiom Name ...] [--olean-store DIR|default] [--json | --emit-lean] [--lean PATH] [--timeout SECONDS]\n\n--format: Kernel-check a formatter document-algebra comment-conservation model with Lean 4.35.0.\n--extract: Extract Lean 4 models from a checked .ves source, compile and replay through the Lean 4.35.0 kernel, and audit axioms (sorryAx is always forbidden).\n--proofs joins an explicit Lean proof script containing theorem proofs for the extracted model.\n--theorem selects theorem names to audit (default: theorems discovered in --proofs, or an internal model-compilation witness).\n--contract-proof binds an emitted function contract to a theorem; when used, every emitted contract must be covered. Each binding requires --contract-witness for the generated existential input precondition. This checks model partial correctness and inhabited input preconditions, not termination or native-code correspondence. Explicit contract admission permits only the three standard Lean axioms.\n--allow-axiom permits named axioms such as propext, Quot.sound, or Classical.choice.\n--olean-store stores audited .olean artifacts and compares cache hits with a fresh compilation in a Loom CAS directory (`default` is ~/.vesper/store).\n--emit-lean prints the generated Lean source only after compilation and kernel replay.\n--timeout bounds each Lean process (1 to 300 seconds; default 30).\nThe selected Lean toolchain and artifact parser remain trusted.\n";

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
    contract_proofs: BTreeMap<String, String>,
    contract_witnesses: BTreeMap<String, String>,
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
    let mut contract_proofs = BTreeMap::new();
    let mut contract_witnesses = BTreeMap::new();
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
                "--contract-proof" | "--contract-witness" => {
                    let bindings = if arg == "--contract-proof" {
                        &mut contract_proofs
                    } else {
                        &mut contract_witnesses
                    };
                    let binding = args.next().ok_or_else(|| {
                        Failure::new("config", format!("{arg} requires Function=Theorem"))
                    })?;
                    let (function, theorem) = binding.split_once('=').ok_or_else(|| {
                        Failure::new("config", format!("{arg} requires Function=Theorem"))
                    })?;
                    if !proof_commands::name_valid(function)
                        || function.contains('.')
                        || !proof_commands::name_valid(theorem)
                    {
                        return Err(Failure::new("config", "invalid contract proof binding"));
                    }
                    if bindings
                        .insert(function.to_owned(), theorem.to_owned())
                        .is_some()
                    {
                        return Err(Failure::new("config", "duplicate contract proof binding"));
                    }
                    if bindings.len() > 128 {
                        return Err(Failure::new(
                            "config",
                            "at most 128 contract proofs are supported",
                        ));
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
                    if proof_commands::forbidden_axiom(&name) {
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
            || !contract_proofs.is_empty()
            || !contract_witnesses.is_empty()
            || !allowed_axioms.is_empty()
            || olean_store.is_some())
    {
        return Err(Failure::new(
            "config",
            "--proofs, --theorem, --contract-proof, --contract-witness, --allow-axiom, and --olean-store require --extract",
        ));
    }
    if (!contract_proofs.is_empty() || !contract_witnesses.is_empty())
        && allowed_axioms
            .iter()
            .any(|name| !matches!(name.as_str(), "propext" | "Classical.choice" | "Quot.sound"))
    {
        return Err(Failure::new(
            "config",
            "explicit contract admission only permits propext, Classical.choice, and Quot.sound",
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
        contract_proofs,
        contract_witnesses,
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
fn read_olean(path: &Path) -> Result<Vec<u8>, Failure> {
    let meta = fs::symlink_metadata(path).map_err(|e| Failure::new("io", e))?;
    if !meta.is_file() || meta.len() > MAX_OLEAN_BYTES {
        return Err(Failure::new(
            "resource",
            ".olean must be a bounded regular file",
        ));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| Failure::new("io", e))?
        .take(MAX_OLEAN_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| Failure::new("io", e))?;
    if bytes.len() as u64 > MAX_OLEAN_BYTES {
        return Err(Failure::new("resource", ".olean exceeds size limit"));
    }
    Ok(bytes)
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
    checked_contracts: BTreeMap<String, String>,
    checked_preconditions: BTreeMap<String, String>,
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
#[derive(Clone, Debug, PartialEq, Eq)]
enum Scope {
    Namespace(String),
    Section(Option<String>),
    Mutual,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TokenKind {
    Word(String),
    Guillemet(String),
    Attr,
    Dot,
    Open(char),
    Close(char),
    Other,
}

#[derive(Clone, Debug)]
struct LeanToken {
    kind: TokenKind,
    line: usize,
    dot_prefixed: bool,
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'\''
}

fn is_lean_keyword(word: &str) -> bool {
    matches!(
        word,
        "theorem"
            | "lemma"
            | "def"
            | "abbrev"
            | "opaque"
            | "axiom"
            | "example"
            | "inductive"
            | "structure"
            | "class"
            | "instance"
            | "namespace"
            | "section"
            | "end"
            | "mutual"
            | "open"
            | "variable"
            | "universe"
            | "attribute"
            | "private"
            | "protected"
            | "noncomputable"
            | "unsafe"
            | "partial"
            | "scoped"
            | "local"
            | "where"
            | "by"
            | "let"
            | "in"
            | "if"
            | "then"
            | "else"
            | "match"
            | "with"
            | "import"
            | "set_option"
    )
}

fn skip_block_comment(bytes: &[u8], i: &mut usize, line: &mut usize) -> Result<(), Failure> {
    let mut depth = 1usize;
    *i += 2;
    while *i < bytes.len() && depth > 0 {
        if bytes[*i] == b'\n' {
            *line += 1;
            *i += 1;
            continue;
        }
        if *i + 1 < bytes.len() && bytes[*i] == b'/' && bytes[*i + 1] == b'-' {
            depth += 1;
            *i += 2;
            continue;
        }
        if *i + 1 < bytes.len() && bytes[*i] == b'-' && bytes[*i + 1] == b'/' {
            depth -= 1;
            *i += 2;
            continue;
        }
        *i += 1;
    }
    if depth != 0 {
        return Err(Failure::new(
            "proofs",
            "unterminated block comment in proofs file",
        ));
    }
    Ok(())
}

fn skip_string_lit(bytes: &[u8], i: &mut usize, line: &mut usize) -> Result<(), Failure> {
    *i += 1;
    while *i < bytes.len() {
        match bytes[*i] {
            b'\\' => {
                *i += 1;
                if *i < bytes.len() {
                    if bytes[*i] == b'\n' {
                        *line += 1;
                    }
                    *i += 1;
                }
            }
            b'"' => {
                *i += 1;
                return Ok(());
            }
            b'\n' => {
                *line += 1;
                *i += 1;
            }
            _ => *i += 1,
        }
    }
    Err(Failure::new(
        "proofs",
        "unterminated string literal in proofs file",
    ))
}

fn try_skip_char_lit(bytes: &[u8], i: &mut usize) -> bool {
    if *i + 2 >= bytes.len() || bytes[*i] != b'\'' {
        return false;
    }
    if bytes[*i + 1] == b'\\' {
        if *i + 3 < bytes.len() && bytes[*i + 3] == b'\'' {
            *i += 4;
            return true;
        }
        return false;
    }
    if bytes[*i + 1] != b'\'' && bytes[*i + 1] != b'\n' && bytes[*i + 2] == b'\'' {
        *i += 3;
        return true;
    }
    false
}

fn tokenize_lean_proofs(proofs: &str) -> Result<Vec<LeanToken>, Failure> {
    let bytes = proofs.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0usize;
    let mut line = 1usize;
    let mut dot_pending = false;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\n' {
            line += 1;
            i += 1;
            dot_pending = false;
            continue;
        }
        if b.is_ascii_whitespace() {
            i += 1;
            dot_pending = false;
            continue;
        }
        if i + 1 < bytes.len() && b == b'-' && bytes[i + 1] == b'-' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            dot_pending = false;
            continue;
        }
        if i + 1 < bytes.len() && b == b'/' && bytes[i + 1] == b'-' {
            skip_block_comment(bytes, &mut i, &mut line)?;
            dot_pending = false;
            continue;
        }
        if b == b'"' {
            let tok_line = line;
            skip_string_lit(bytes, &mut i, &mut line)?;
            tokens.push(LeanToken {
                kind: TokenKind::Other,
                line: tok_line,
                dot_prefixed: false,
            });
            dot_pending = false;
            continue;
        }
        if try_skip_char_lit(bytes, &mut i) {
            tokens.push(LeanToken {
                kind: TokenKind::Other,
                line,
                dot_prefixed: false,
            });
            dot_pending = false;
            continue;
        }
        if i + 1 < bytes.len() && b == b'@' && bytes[i + 1] == b'[' {
            let tok_line = line;
            i += 2;
            let mut bracket_depth = 1usize;
            while i < bytes.len() && bracket_depth > 0 {
                match bytes[i] {
                    b'\n' => {
                        line += 1;
                        i += 1;
                    }
                    b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                        while i < bytes.len() && bytes[i] != b'\n' {
                            i += 1;
                        }
                    }
                    b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                        skip_block_comment(bytes, &mut i, &mut line)?;
                    }
                    b'"' => skip_string_lit(bytes, &mut i, &mut line)?,
                    b'\'' if try_skip_char_lit(bytes, &mut i) => {}
                    b'[' => {
                        bracket_depth += 1;
                        i += 1;
                    }
                    b']' => {
                        bracket_depth -= 1;
                        i += 1;
                    }
                    _ => i += 1,
                }
            }
            if bracket_depth != 0 {
                return Err(Failure::new(
                    "proofs",
                    "unterminated attribute block '@[' in proofs file",
                ));
            }
            tokens.push(LeanToken {
                kind: TokenKind::Attr,
                line: tok_line,
                dot_prefixed: false,
            });
            dot_pending = false;
            continue;
        }
        if proofs[i..].starts_with('«') {
            let start = i;
            let tok_line = line;
            i += '«'.len_utf8();
            let mut closed = false;
            while i < bytes.len() {
                let ch = proofs[i..].chars().next().unwrap();
                if ch == '\n' {
                    line += 1;
                }
                i += ch.len_utf8();
                if ch == '»' {
                    closed = true;
                    break;
                }
            }
            if !closed {
                return Err(Failure::new(
                    "proofs",
                    "unterminated guillemet identifier in proofs file",
                ));
            }
            tokens.push(LeanToken {
                kind: TokenKind::Guillemet(proofs[start..i].to_owned()),
                line: tok_line,
                dot_prefixed: dot_pending,
            });
            dot_pending = false;
            continue;
        }
        if is_ident_start(b) {
            let start = i;
            i += 1;
            while i < bytes.len() {
                if is_ident_continue(bytes[i])
                    || (bytes[i] == b'.' && i + 1 < bytes.len() && is_ident_start(bytes[i + 1]))
                {
                    i += 1;
                } else {
                    break;
                }
            }
            tokens.push(LeanToken {
                kind: TokenKind::Word(proofs[start..i].to_owned()),
                line,
                dot_prefixed: dot_pending,
            });
            dot_pending = false;
            continue;
        }
        if b == b'.' {
            tokens.push(LeanToken {
                kind: TokenKind::Dot,
                line,
                dot_prefixed: false,
            });
            dot_pending = true;
            i += 1;
            continue;
        }
        dot_pending = false;
        if matches!(b, b'(' | b'[' | b'{') {
            tokens.push(LeanToken {
                kind: TokenKind::Open(b as char),
                line,
                dot_prefixed: false,
            });
            i += 1;
            continue;
        }
        if matches!(b, b')' | b']' | b'}') {
            tokens.push(LeanToken {
                kind: TokenKind::Close(b as char),
                line,
                dot_prefixed: false,
            });
            i += 1;
            continue;
        }
        let ch = proofs[i..].chars().next().unwrap();
        i += ch.len_utf8();
        tokens.push(LeanToken {
            kind: TokenKind::Other,
            line,
            dot_prefixed: false,
        });
    }
    Ok(tokens)
}

fn discover_theorems(proofs: &str) -> Result<Vec<String>, Failure> {
    let tokens = tokenize_lean_proofs(proofs)?;
    let mut scope_stack: Vec<Scope> = Vec::new();
    let mut discovered = Vec::new();
    let mut seen = BTreeSet::new();
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut brace_depth = 0usize;
    let mut is_private = false;
    let mut idx = 0usize;

    while idx < tokens.len() {
        let tok = &tokens[idx];
        match tok.kind {
            TokenKind::Open('(') => {
                paren_depth += 1;
                is_private = false;
                idx += 1;
                continue;
            }
            TokenKind::Open('[') => {
                bracket_depth += 1;
                is_private = false;
                idx += 1;
                continue;
            }
            TokenKind::Open('{') => {
                brace_depth += 1;
                is_private = false;
                idx += 1;
                continue;
            }
            TokenKind::Close(')') => {
                paren_depth = paren_depth.saturating_sub(1);
                is_private = false;
                idx += 1;
                continue;
            }
            TokenKind::Close(']') => {
                bracket_depth = bracket_depth.saturating_sub(1);
                is_private = false;
                idx += 1;
                continue;
            }
            TokenKind::Close('}') => {
                brace_depth = brace_depth.saturating_sub(1);
                is_private = false;
                idx += 1;
                continue;
            }
            _ => {}
        }

        if paren_depth > 0 || bracket_depth > 0 || brace_depth > 0 || tok.dot_prefixed {
            idx += 1;
            continue;
        }

        match &tok.kind {
            TokenKind::Attr => {
                idx += 1;
            }
            TokenKind::Word(w)
                if matches!(
                    w.as_str(),
                    "protected" | "noncomputable" | "unsafe" | "partial" | "scoped" | "local"
                ) =>
            {
                idx += 1;
            }
            TokenKind::Word(w) if w == "private" => {
                is_private = true;
                idx += 1;
            }
            TokenKind::Word(w) if w == "namespace" => {
                is_private = false;
                let Some(next) = tokens.get(idx + 1) else {
                    return Err(Failure::new(
                        "proofs",
                        "expected namespace name in proofs file",
                    ));
                };
                if next.line != tok.line {
                    return Err(Failure::new(
                        "proofs",
                        "expected namespace name on same line in proofs file",
                    ));
                }
                match &next.kind {
                    TokenKind::Word(name)
                        if !next.dot_prefixed
                            && !is_lean_keyword(name)
                            && proof_commands::name_valid(name) =>
                    {
                        for part in name.split('.') {
                            scope_stack.push(Scope::Namespace(part.to_owned()));
                        }
                        idx += 2;
                    }
                    TokenKind::Word(name) => {
                        return Err(Failure::new(
                            "proofs",
                            format!("unsupported namespace identifier '{name}' in proofs file"),
                        ));
                    }
                    TokenKind::Guillemet(raw) => {
                        return Err(Failure::new(
                            "proofs",
                            format!("unsupported namespace identifier '{raw}' in proofs file"),
                        ));
                    }
                    _ => {
                        return Err(Failure::new(
                            "proofs",
                            "expected namespace name in proofs file",
                        ));
                    }
                }
            }
            TokenKind::Word(w) if w == "section" => {
                is_private = false;
                if let Some(next) = tokens.get(idx + 1)
                    && next.line == tok.line
                    && !next.dot_prefixed
                {
                    match &next.kind {
                        TokenKind::Word(name) if !is_lean_keyword(name) => {
                            if !proof_commands::name_valid(name) {
                                return Err(Failure::new(
                                    "proofs",
                                    format!(
                                        "unsupported section identifier '{name}' in proofs file"
                                    ),
                                ));
                            }
                            for part in name.split('.') {
                                scope_stack.push(Scope::Section(Some(part.to_owned())));
                            }
                            idx += 2;
                            continue;
                        }
                        TokenKind::Guillemet(raw) => {
                            return Err(Failure::new(
                                "proofs",
                                format!("unsupported section identifier '{raw}' in proofs file"),
                            ));
                        }
                        _ => {}
                    }
                }
                scope_stack.push(Scope::Section(None));
                idx += 1;
            }
            TokenKind::Word(w) if w == "mutual" => {
                is_private = false;
                scope_stack.push(Scope::Mutual);
                idx += 1;
            }
            TokenKind::Word(w) if w == "end" => {
                is_private = false;
                if let Some(next) = tokens.get(idx + 1)
                    && next.line == tok.line
                    && !next.dot_prefixed
                {
                    match &next.kind {
                        TokenKind::Word(name) if !is_lean_keyword(name) => {
                            if !proof_commands::name_valid(name) {
                                return Err(Failure::new(
                                    "proofs",
                                    format!("unsupported 'end {name}' identifier in proofs file"),
                                ));
                            }
                            for part in name.split('.').rev() {
                                match scope_stack.pop() {
                                    Some(Scope::Namespace(n)) if n == part => {}
                                    Some(Scope::Section(Some(n))) if n == part => {}
                                    _ => {
                                        return Err(Failure::new(
                                            "proofs",
                                            format!("mismatched 'end {name}' in proofs file"),
                                        ));
                                    }
                                }
                            }
                            idx += 2;
                            continue;
                        }
                        TokenKind::Guillemet(raw) => {
                            return Err(Failure::new(
                                "proofs",
                                format!("unsupported 'end {raw}' identifier in proofs file"),
                            ));
                        }
                        _ => {}
                    }
                }
                if scope_stack.pop().is_none() {
                    return Err(Failure::new(
                        "proofs",
                        "unexpected 'end' with no open namespace or section",
                    ));
                }
                idx += 1;
            }
            TokenKind::Word(w) if w == "example" => {
                return Err(Failure::new(
                    "proofs",
                    "anonymous 'example' declarations in proofs file cannot be audited; use a named theorem",
                ));
            }
            TokenKind::Word(w) if matches!(w.as_str(), "theorem" | "lemma") => {
                let decl_kind = w.as_str();
                if is_private {
                    return Err(Failure::new(
                        "proofs",
                        format!(
                            "private {decl_kind} declarations in proofs file cannot be audited by name"
                        ),
                    ));
                }
                let Some(next) = tokens.get(idx + 1) else {
                    return Err(Failure::new(
                        "proofs",
                        format!("expected {decl_kind} identifier in proofs file"),
                    ));
                };
                let raw_name = match &next.kind {
                    TokenKind::Word(name) if !next.dot_prefixed && !is_lean_keyword(name) => {
                        name.as_str()
                    }
                    TokenKind::Word(name) => {
                        return Err(Failure::new(
                            "proofs",
                            format!("unsupported {decl_kind} identifier '{name}' in proofs file"),
                        ));
                    }
                    TokenKind::Guillemet(raw) => {
                        return Err(Failure::new(
                            "proofs",
                            format!("unsupported {decl_kind} identifier '{raw}' in proofs file"),
                        ));
                    }
                    _ => {
                        return Err(Failure::new(
                            "proofs",
                            format!("expected {decl_kind} identifier in proofs file"),
                        ));
                    }
                };
                let (is_root, ident) = match raw_name.strip_prefix("_root_.") {
                    Some(rest) => (true, rest),
                    None => (false, raw_name),
                };
                if !proof_commands::name_valid(ident) {
                    return Err(Failure::new(
                        "proofs",
                        format!("unsupported {decl_kind} identifier '{raw_name}' in proofs file"),
                    ));
                }
                let full = if is_root {
                    ident.to_owned()
                } else {
                    let ns_parts: Vec<&str> = scope_stack
                        .iter()
                        .filter_map(|scope| match scope {
                            Scope::Namespace(n) => Some(n.as_str()),
                            Scope::Section(_) | Scope::Mutual => None,
                        })
                        .collect();
                    if ns_parts.is_empty() {
                        ident.to_owned()
                    } else {
                        format!("{}.{}", ns_parts.join("."), ident)
                    }
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
                idx += 2;
            }
            _ => {
                is_private = false;
                idx += 1;
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
fn contract_bindings(
    contracts: &[String],
    requested: &BTreeMap<String, String>,
    witnesses: &BTreeMap<String, String>,
) -> Result<(String, Vec<String>), Failure> {
    if requested.is_empty() && witnesses.is_empty() {
        return Ok((String::new(), Vec::new()));
    }
    for function in requested.keys().chain(witnesses.keys()) {
        if !contracts.contains(function) {
            return Err(Failure::new(
                "contracts",
                format!("no emitted contract for '{function}'"),
            ));
        }
    }
    for function in contracts {
        if !requested.contains_key(function) {
            return Err(Failure::new(
                "contracts",
                format!("missing proof binding for contract '{function}'"),
            ));
        }
    }
    for function in contracts {
        if !witnesses.contains_key(function) {
            return Err(Failure::new(
                "contracts",
                format!("missing precondition witness for contract '{function}'"),
            ));
        }
    }
    let mut source = String::new();
    let mut names = Vec::new();
    for (index, (function, theorem)) in requested.iter().enumerate() {
        // Inputs are bounded ASCII names checked by options(). Quote every
        // component and use root names so proof-script namespaces/open commands
        // cannot redirect the contract or selected theorem.
        let proof = theorem
            .split('.')
            .map(|part| format!("«{part}»"))
            .collect::<Vec<_>>()
            .join(".");
        let name = format!("VesperVerify.contract_{index}");
        source.push_str(&format!(
            "\ntheorem _root_.{name} : _root_.Vesper.«{function}$contract» := _root_.{proof}\n"
        ));
        names.push(name);
        let witness = witnesses[function]
            .split('.')
            .map(|part| format!("«{part}»"))
            .collect::<Vec<_>>()
            .join(".");
        let name = format!("VesperVerify.precondition_{index}");
        source.push_str(&format!(
            "\ntheorem _root_.{name} : _root_.Vesper.«{function}$admissible» := _root_.{witness}\n"
        ));
        names.push(name);
    }
    Ok((source, names))
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
    let (bindings, binding_theorems) = contract_bindings(
        &report.contracts,
        &options.contract_proofs,
        &options.contract_witnesses,
    )?;
    let (mut bundle_text, mut audit_theorems) = if let Some(proof_text) = &proofs {
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
    bundle_text.push_str(&bindings);
    audit_theorems.extend(binding_theorems);
    audit_theorems.sort();
    audit_theorems.dedup();
    if audit_theorems.len() > 128 {
        return Err(Failure::new(
            "contracts",
            "selected theorems and contract bindings exceed 128",
        ));
    }
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
    let bundle_id = blob_id(Domain::Source, bundle_text.as_bytes());
    let deriv_inputs = opened_store.as_ref().map(|_| {
        DerivationInputs::new(Domain::Artifact, EXTRACT_BUILDER, EXTRACT_BUILDER_VERSION)
            .with_input("lean_bundle", bundle_id)
            .with_option("source_blake3", source_blake3.clone())
            .with_option("lean_model_blake3", lean_model_blake3.clone())
            .with_option("proofs_blake3", proofs_blake3.as_deref().unwrap_or(""))
            .with_option("lean_version", toolchain.version.clone())
            .with_option("theorems", theorems_csv.clone())
            .with_option("allowed_axioms", axioms_csv.clone())
    });

    // A CAS derivation records a claimed input/output association, not a proof
    // that Lean produced this artifact from this bundle. Always compile the
    // current source and keep its complete artifact closure for kernel replay.
    // A cache hit is accepted only when its bytes match that fresh result.
    compile(&toolchain, &stage.0, "VesperExtract", options.timeout)?;
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
            let fresh = read_olean(&stage.0.join("VesperExtract.olean"))?;
            if bytes != fresh {
                return Err(Failure::new(
                    "store",
                    "cached .olean does not match a fresh compilation of the current source and proofs",
                ));
            }
            cert_report = Some(OleanCertReport {
                store: dir.display().to_string(),
                object_id: cached_id.to_string(),
                cache_hit: true,
                newly_recorded: false,
            });
        }
    }

    let theorem_refs: Vec<&str> = audit_theorems.iter().map(String::as_str).collect();
    let allowed_refs: Vec<&str> = options.allowed_axioms.iter().map(String::as_str).collect();
    let expected_types = options
        .contract_proofs
        .keys()
        .enumerate()
        .flat_map(|(index, function)| {
            [
                (
                    format!("VesperVerify.contract_{index}"),
                    format!("Vesper.{function}$contract"),
                ),
                (
                    format!("VesperVerify.precondition_{index}"),
                    format!("Vesper.{function}$admissible"),
                ),
            ]
        })
        .collect();
    let audit = proof_commands::audit_generated_with_types(
        &stage.0,
        "VesperExtract",
        &theorem_refs,
        &allowed_refs,
        &expected_types,
        &options.lean,
        options.timeout,
    )
    .map_err(|e| Failure::new("kernel", e))?;

    if cert_report.is_none()
        && let (Some(store), Some(inputs), Some(dir)) = (&opened_store, &deriv_inputs, &store_dir)
    {
        let olean_path = stage.0.join("VesperExtract.olean");
        let olean_bytes = read_olean(&olean_path)?;
        store
            .put_blob(Domain::Source, bundle_text.as_bytes())
            .map_err(|e| Failure::new("store", e))?;
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
            "Lean extraction model: kernel checked ({} function(s), {} contract proposition(s), {} bound contract proof(s) with input witnesses, {} theorem(s)).\n",
            report.totality.len(),
            report.contracts.len(),
            options.contract_proofs.len(),
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
        checked_contracts: options.contract_proofs.clone(),
        checked_preconditions: options.contract_witnesses.clone(),
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

#[cfg(test)]
mod tests {
    use super::discover_theorems;

    #[test]
    fn discovers_attributed_and_modified_theorems_and_lemmas() {
        let src = r#"
theorem plain : True := trivial

@[simp] theorem simp_one : True := trivial

@[simp,
  aesop safe [constructors]]
protected theorem prot_thm (x : Nat) : x = x := rfl

noncomputable lemma helper' : True := trivial

namespace Outer.Inner
@[simp] theorem scoped_thm : True := trivial
theorem _root_.root_override : True := trivial
end Outer.Inner

theorem after_ns.{u} : True := trivial
"#;
        let found = discover_theorems(src).unwrap();
        assert_eq!(
            found,
            vec![
                "plain",
                "simp_one",
                "prot_thm",
                "helper'",
                "Outer.Inner.scoped_thm",
                "root_override",
                "after_ns",
            ]
        );
    }

    #[test]
    fn tracks_sections_and_unnamed_end_without_confusing_namespaces() {
        let src = r#"
namespace A
theorem first : True := trivial
section S
theorem in_section : True := trivial
end
theorem still_in_a : True := trivial
end
theorem second : True := trivial

namespace B
section
theorem in_anon_section : True := trivial
end
theorem still_in_b : True := trivial
end B
theorem third : True := trivial
"#;
        let found = discover_theorems(src).unwrap();
        assert_eq!(
            found,
            vec![
                "A.first",
                "A.in_section",
                "A.still_in_a",
                "second",
                "B.in_anon_section",
                "B.still_in_b",
                "third",
            ]
        );
    }

    #[test]
    fn field_projection_end_and_lemma_variable_do_not_corrupt_discovery() {
        let src = r#"
namespace VesperProofs.Structs
theorem move_start_frame (s : Segment) (lemma : Nat) :
    (move_start s 1).2.end = s.end := by
  simp [move_start]
theorem next_one : True := trivial
end VesperProofs.Structs
theorem top_after : True := trivial
"#;
        let found = discover_theorems(src).unwrap();
        assert_eq!(
            found,
            vec![
                "VesperProofs.Structs.move_start_frame",
                "VesperProofs.Structs.next_one",
                "top_after",
            ]
        );
    }

    #[test]
    fn rejects_private_theorems_and_unsupported_identifiers() {
        let priv_err =
            discover_theorems("private theorem hidden : False := by sorry\n").unwrap_err();
        assert_eq!(priv_err.stage, "proofs");
        assert!(priv_err.message.contains("private theorem"));

        let attr_priv_err =
            discover_theorems("@[simp] private lemma hidden : False := by sorry\n").unwrap_err();
        assert_eq!(attr_priv_err.stage, "proofs");
        assert!(attr_priv_err.message.contains("private lemma"));

        let guillemet_err =
            discover_theorems("theorem «weird name» : True := trivial\n").unwrap_err();
        assert_eq!(guillemet_err.stage, "proofs");
        assert!(
            guillemet_err
                .message
                .contains("unsupported theorem identifier")
        );

        let example_err = discover_theorems("example : False := by sorry\n").unwrap_err();
        assert_eq!(example_err.stage, "proofs");
        assert!(example_err.message.contains("example"));
    }
}
