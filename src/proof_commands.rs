//! Local Lean artifact replay and axiom-policy auditing; never a cache certificate.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    env,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

const AUDITOR: &str = include_str!("../tools/proof-audit/Audit.lean");
const MAX_INPUT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_FILES: usize = 4096;
const MAX_CAPTURE: usize = 8 * 1024 * 1024;
const MARKER: &str = "VESPER_PROOF_AUDIT_V1:";
const HELP: &str = "Usage: vesper proof-audit <Module> --root <artifact-directory> --theorem <Qualified.name> [--theorem ...] [--allow-axiom <Name>] [--lean <path>] [--timeout <seconds>] [--json]\n\nReplay existing local Lean 4.24.0 artifacts through the kernel and audit named theorems.\nArtifact imports disable package environment extensions and plugins.\nThe selected Lean toolchain and artifact parser remain trusted; this is not an OS sandbox, provenance certificate or reusable proof-cache approval.\nThe default axiom policy is empty; sorryAx is always forbidden.\n";

#[derive(Default)]
struct Options {
    module: Option<String>,
    root: Option<PathBuf>,
    theorems: BTreeSet<String>,
    allowed: BTreeSet<String>,
    lean: Option<String>,
    timeout: Option<u64>,
    json: bool,
}

fn name_valid(name: &str) -> bool {
    name.len() <= 1024
        && name.split('.').all(|part| {
            !part.is_empty()
                && (part.as_bytes()[0].is_ascii_alphabetic() || part.starts_with('_'))
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'\'')
        })
}

pub fn run(args: Vec<String>) -> Result<(), String> {
    let mut options = Options::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{HELP}");
                return Ok(());
            }
            "--root" => {
                if options.root.is_some() {
                    return Err("--root may only be specified once".into());
                }
                options.root = Some(args.next().ok_or("--root requires a directory")?.into());
            }
            "--theorem" | "--allow-axiom" => {
                let name = args
                    .next()
                    .ok_or_else(|| format!("{arg} requires a name"))?;
                if !name_valid(&name) {
                    return Err(format!(
                        "invalid Lean name '{name}'; use qualified ASCII identifiers"
                    ));
                }
                if arg == "--theorem" {
                    options.theorems.insert(name);
                } else {
                    if name == "sorryAx" {
                        return Err("sorryAx cannot be allowed by the audit policy".into());
                    }
                    options.allowed.insert(name);
                }
            }
            "--lean" => {
                if options.lean.is_some() {
                    return Err("--lean may only be specified once".into());
                }
                options.lean = Some(args.next().ok_or("--lean requires a path")?);
            }
            "--timeout" => {
                if options.timeout.is_some() {
                    return Err("--timeout may only be specified once".into());
                }
                let seconds = args
                    .next()
                    .ok_or("--timeout requires seconds")?
                    .parse::<u64>()
                    .map_err(|_| "invalid --timeout; expected 1 to 300 seconds")?;
                if !(1..=300).contains(&seconds) {
                    return Err("--timeout must be between 1 and 300 seconds".into());
                }
                options.timeout = Some(seconds);
            }
            "--json" => options.json = true,
            _ if arg.starts_with('-') => return Err(format!("unknown proof-audit option '{arg}'")),
            _ => {
                if options.module.replace(arg).is_some() {
                    return Err("proof-audit accepts one module".into());
                }
            }
        }
    }
    let module = options.module.as_deref().ok_or("missing Lean module")?;
    if !name_valid(module)
        || matches!(
            module.split('.').next(),
            Some("Lean" | "Std" | "Init" | "Lake")
        )
    {
        return Err(
            "module must be a qualified ASCII identifier outside trusted toolchain namespaces"
                .into(),
        );
    }
    if options.theorems.is_empty() {
        return Err("proof-audit requires at least one --theorem".into());
    }
    if options.theorems.len() > 128 {
        return Err("proof-audit supports at most 128 theorem names".into());
    }
    let root = options
        .root
        .as_deref()
        .ok_or("proof-audit requires --root")?;
    let report = match audit(&options, root, module) {
        Ok(report) => report,
        Err(message) => {
            if options.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schema_version":1,"status":"error",
                        "error":{"kind":"audit_failed","message":message}
                    }))
                    .map_err(|e| e.to_string())?
                );
            }
            return Err(message);
        }
    };
    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else {
        println!(
            "Lean artifact audit: {}",
            if report.accepted {
                "accepted"
            } else {
                "rejected"
            }
        );
        println!(
            "{}; {} kernel-replayed constants",
            report.lean_version, report.checked_constants
        );
        for theorem in &report.theorems {
            println!("{}: axioms [{}]", theorem.name, theorem.axioms.join(", "));
        }
        for axiom in &report.forbidden_axioms {
            println!("unapproved axiom: {axiom}");
        }
        println!("Local audit only; no provenance or proof-cache certificate.");
    }
    if report.accepted {
        Ok(())
    } else {
        Err("theorem axioms exceed the explicit audit policy".into())
    }
}

pub(crate) struct Stage(pub(crate) PathBuf);
impl Stage {
    pub(crate) fn new() -> Result<Self, String> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..64 {
            let path = env::temp_dir().join(format!(
                "vesper-proof-audit-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let created = {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    fs::DirBuilder::new().mode(0o700).create(&path)
                }
                #[cfg(not(unix))]
                {
                    fs::create_dir(&path)
                }
            };
            match created {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("private audit staging directory: {e}")),
            }
        }
        Err("cannot allocate a private audit staging directory".into())
    }
}
impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Serialize)]
struct FileDigest {
    path: String,
    bytes: u64,
    blake3: String,
}

fn artifact_file(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        let name = name.to_string_lossy();
        [".olean", ".olean.server", ".olean.private", ".ir"]
            .iter()
            .any(|end| name.ends_with(end))
    })
}

fn copy_artifacts(root: &Path, output: &Path) -> Result<Vec<FileDigest>, String> {
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    let mut files = vec![];
    let mut bytes = 0u64;
    let mut entries = 0usize;
    while let Some((directory, depth)) = pending.pop() {
        if depth > 64 {
            return Err("artifact directory exceeds the depth limit".into());
        }
        for entry in fs::read_dir(directory).map_err(|e| e.to_string())? {
            entries += 1;
            if entries > MAX_FILES * 4 {
                return Err("artifact directory exceeds the entry limit".into());
            }
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_symlink() {
                return Err("symlinks are not allowed in the artifact directory".into());
            }
            if kind.is_dir() {
                pending.push((path, depth + 1));
            } else if kind.is_file() && artifact_file(&path) {
                if files.len() == MAX_FILES {
                    return Err("too many artifact files".into());
                }
                let mut data = vec![];
                File::open(&path)
                    .map_err(|e| e.to_string())?
                    .take(MAX_INPUT_BYTES - bytes + 1)
                    .read_to_end(&mut data)
                    .map_err(|e| e.to_string())?;
                bytes += data.len() as u64;
                if bytes > MAX_INPUT_BYTES {
                    return Err("artifact snapshot exceeds 128 MiB".into());
                }
                let relative = path.strip_prefix(root).map_err(|e| e.to_string())?;
                let dest = output.join(relative);
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                fs::write(&dest, &data).map_err(|e| e.to_string())?;
                files.push(FileDigest {
                    path: relative
                        .to_str()
                        .ok_or("artifact paths must be UTF-8")?
                        .replace('\\', "/"),
                    bytes: data.len() as u64,
                    blake3: blake3::hash(&data).to_hex().to_string(),
                });
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

fn capture(mut reader: impl Read) -> std::io::Result<(Vec<u8>, bool)> {
    let mut saved = vec![];
    let mut chunk = [0u8; 8192];
    let mut truncated = false;
    loop {
        let count = reader.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        let take = count.min(MAX_CAPTURE - saved.len());
        saved.extend_from_slice(&chunk[..take]);
        truncated |= take != count;
    }
    Ok((saved, truncated))
}

pub(crate) fn execute(command: &mut Command, timeout: Duration) -> Result<Output, String> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start Lean: {e}"))?;
    let stdout = child.stdout.take().ok_or("Lean stdout is unavailable")?;
    let stderr = child.stderr.take().ok_or("Lean stderr is unavailable")?;
    let out = thread::spawn(move || capture(stdout));
    let err = thread::spawn(move || capture(stderr));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() < timeout => thread::sleep(Duration::from_millis(10)),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(match result {
                    Err(e) => format!("waiting for Lean: {e}"),
                    _ => "Lean audit timed out".into(),
                });
            }
        }
    };
    let (stdout, out_truncated) = out
        .join()
        .map_err(|_| "Lean stdout reader failed")?
        .map_err(|e| e.to_string())?;
    let (stderr, err_truncated) = err
        .join()
        .map_err(|_| "Lean stderr reader failed")?
        .map_err(|e| e.to_string())?;
    let status = status?;
    if out_truncated || err_truncated {
        return Err("Lean output exceeded the capture limit".into());
    }
    let stdout_text = String::from_utf8_lossy(&stdout);
    let stderr_text = String::from_utf8_lossy(&stderr);
    let panic_output = stdout_text
        .lines()
        .filter(|line| !line.starts_with(MARKER))
        .any(|line| line.contains("PANIC"))
        || stderr_text.contains("PANIC");
    if !status.success() || panic_output {
        let details = format!("{stdout_text}\n{stderr_text}");
        return Err(format!(
            "Lean audit failed ({status}): {}",
            details.chars().take(4000).collect::<String>()
        ));
    }
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Theorem {
    name: String,
    axioms: Vec<String>,
    type_repr: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawModule {
    name: String,
    path: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAudit {
    schema_version: u32,
    kernel_replayed: bool,
    checked_constants: u64,
    theorems: Vec<Theorem>,
    modules: Vec<RawModule>,
}
#[derive(Serialize)]
struct ModuleDigest {
    name: String,
    scope: &'static str,
    files: Vec<FileDigest>,
}
#[derive(Serialize)]
struct AuditReport {
    schema_version: u32,
    status: &'static str,
    accepted: bool,
    kernel_replayed: bool,
    cache_eligible: bool,
    module: String,
    lean_version: String,
    lean_binary_blake3: String,
    auditor_blake3: String,
    artifact_inventory_blake3: String,
    checked_constants: u64,
    allowed_axioms: BTreeSet<String>,
    forbidden_axioms: BTreeSet<String>,
    theorems: Vec<Theorem>,
    artifact_files: Vec<FileDigest>,
    imports: Vec<ModuleDigest>,
}

fn digest_file(path: &Path, relative: &Path) -> Result<FileDigest, String> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if file.metadata().map_err(|e| e.to_string())?.len() > 2 * 1024 * 1024 * 1024 {
        return Err("an imported artifact exceeds the fingerprinting size limit".into());
    }
    let mut hasher = blake3::Hasher::new();
    let bytes = std::io::copy(&mut file.take(2 * 1024 * 1024 * 1024 + 1), &mut hasher)
        .map_err(|e| e.to_string())?;
    if bytes > 2 * 1024 * 1024 * 1024 {
        return Err("an imported artifact grew beyond the fingerprinting limit".into());
    }
    Ok(FileDigest {
        path: relative
            .to_str()
            .ok_or("artifact paths must be UTF-8")?
            .replace('\\', "/"),
        bytes,
        blake3: hasher.finalize().to_hex().to_string(),
    })
}

pub(crate) struct LeanToolchain {
    pub(crate) prefix: PathBuf,
    pub(crate) binary: PathBuf,
    pub(crate) version: String,
}
pub(crate) fn lean_toolchain(launcher: &str) -> Result<LeanToolchain, String> {
    let prefix = execute(
        Command::new(launcher).arg("--print-prefix"),
        Duration::from_secs(30),
    )?;
    let prefix = PathBuf::from(
        String::from_utf8(prefix.stdout)
            .map_err(|e| e.to_string())?
            .trim(),
    )
    .canonicalize()
    .map_err(|e| format!("Lean toolchain prefix: {e}"))?;
    let binary = prefix
        .join("bin")
        .join(if cfg!(windows) { "lean.exe" } else { "lean" });
    let version = execute(
        Command::new(&binary).arg("--version"),
        Duration::from_secs(10),
    )?;
    let version = String::from_utf8(version.stdout)
        .map_err(|e| e.to_string())?
        .trim()
        .to_string();
    if !version.contains("version 4.24.0,") {
        return Err(format!("proof-audit requires Lean 4.24.0; found {version}"));
    }
    Ok(LeanToolchain {
        prefix,
        binary,
        version,
    })
}

/// Reuse the normal kernel replay with an empty axiom policy. The caller owns
/// freshly generated artifacts; source strings and reported flags never approve.
pub(crate) fn audit_generated(
    root: &Path,
    module: &str,
    theorems: &[&str],
    lean: &str,
    timeout: u64,
) -> Result<serde_json::Value, String> {
    if !name_valid(module)
        || theorems.is_empty()
        || theorems.len() > 128
        || theorems.iter().any(|name| !name_valid(name))
        || !(1..=300).contains(&timeout)
    {
        return Err("invalid generated proof audit request".into());
    }
    let options = Options {
        theorems: theorems.iter().map(|name| (*name).to_owned()).collect(),
        lean: Some(lean.to_owned()),
        timeout: Some(timeout),
        ..Options::default()
    };
    let report = audit(&options, root, module)?;
    if !report.accepted || !report.kernel_replayed || !report.forbidden_axioms.is_empty() {
        return Err("generated theorem failed the empty-axiom kernel audit".into());
    }
    serde_json::to_value(report).map_err(|error| error.to_string())
}

fn audit(options: &Options, root: &Path, module: &str) -> Result<AuditReport, String> {
    let launcher = options
        .lean
        .clone()
        .or_else(|| env::var("VESPER_LEAN").ok())
        .unwrap_or_else(|| "lean".into());
    let LeanToolchain {
        prefix,
        binary,
        version,
    } = lean_toolchain(&launcher)?;
    let stage = Stage::new()?;
    let snapshot = stage.0.join("artifacts");
    fs::create_dir(&snapshot).map_err(|e| e.to_string())?;
    let root = root
        .canonicalize()
        .map_err(|e| format!("artifact root: {e}"))?;
    let files = copy_artifacts(&root, &snapshot)?;
    let module_file = module
        .split('.')
        .fold(snapshot.clone(), |path, part| path.join(part))
        .with_extension("olean");
    if !module_file.is_file() {
        return Err(format!("module {module} has no .olean artifact in --root"));
    }
    let helper = stage.0.join("Audit.lean");
    fs::write(&helper, AUDITOR).map_err(|e| e.to_string())?;
    let request = stage.0.join("request.json");
    fs::write(
        &request,
        serde_json::to_vec(&serde_json::json!({
            "schema_version":1,"root":snapshot,"toolchain_prefix":prefix,
            "module":module,"theorems":options.theorems
        }))
        .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let mut command = Command::new(&binary);
    command
        .env_clear()
        .env("LEAN_PATH", prefix.join("lib/lean"))
        .env("LEAN_ABORT_ON_PANIC", "1")
        .current_dir(&stage.0);
    #[cfg(windows)]
    for key in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = env::var_os(key) {
            command.env(key, value);
        }
    }
    let output = execute(
        command
            .args(["-j1", "-M4096", "--run"])
            .arg(helper)
            .arg(request),
        Duration::from_secs(options.timeout.unwrap_or(30)),
    )?;
    let text = String::from_utf8(output.stdout).map_err(|e| e.to_string())?;
    let mut records = text.lines().filter_map(|line| line.strip_prefix(MARKER));
    let raw: RawAudit = serde_json::from_str(records.next().ok_or("Lean emitted no audit result")?)
        .map_err(|e| format!("invalid Lean audit result: {e}"))?;
    if records.next().is_some() || raw.schema_version != 1 || !raw.kernel_replayed {
        return Err("Lean audit result is duplicate, unsupported or not kernel-replayed".into());
    }
    let requested = raw
        .theorems
        .iter()
        .map(|t| t.name.clone())
        .collect::<BTreeSet<_>>();
    if requested != options.theorems || raw.theorems.len() != requested.len() {
        return Err("Lean audit did not check every requested theorem exactly once".into());
    }
    if raw.modules.len() > 8192 {
        return Err("Lean audit exceeds the imported-module limit".into());
    }
    let snapshot = snapshot.canonicalize().map_err(|e| e.to_string())?;
    let mut imports = vec![];
    let mut names = BTreeSet::new();
    let mut import_bytes = 0u64;
    for module in raw.modules {
        if !names.insert(module.name.clone()) {
            return Err("Lean audit contains duplicate module identities".into());
        }
        let primary = module.path.canonicalize().map_err(|e| e.to_string())?;
        let (scope, base) = if primary.starts_with(&snapshot) {
            ("artifact", &snapshot)
        } else if primary.starts_with(&prefix) {
            ("toolchain", &prefix)
        } else {
            return Err("imported module escaped the artifact/toolchain roots".into());
        };
        let mut parts = vec![];
        let mut candidates = vec![];
        for suffix in ["", ".server", ".private"] {
            let mut part = primary.as_os_str().to_os_string();
            part.push(suffix);
            let part = PathBuf::from(part);
            if part.is_file() {
                candidates.push(part);
            }
        }
        let ir = primary.with_extension("ir");
        if ir.is_file() {
            candidates.push(ir);
        }
        for part in candidates {
            let part = part.canonicalize().map_err(|e| e.to_string())?;
            let relative = part
                .strip_prefix(base)
                .map_err(|_| "an imported artifact part escaped its root")?;
            let digest = digest_file(&part, relative)?;
            import_bytes += digest.bytes;
            if import_bytes > 4 * 1024 * 1024 * 1024 {
                return Err("imported artifact inventory exceeds 4 GiB".into());
            }
            parts.push(digest);
        }
        imports.push(ModuleDigest {
            name: module.name,
            scope,
            files: parts,
        });
    }
    imports.sort_by(|a, b| a.name.cmp(&b.name));
    let forbidden_axioms = raw
        .theorems
        .iter()
        .flat_map(|t| t.axioms.iter())
        .filter(|axiom| axiom.as_str() == "sorryAx" || !options.allowed.contains(*axiom))
        .cloned()
        .collect::<BTreeSet<_>>();
    Ok(AuditReport {
        schema_version: 1,
        status: "audited",
        accepted: forbidden_axioms.is_empty(),
        kernel_replayed: true,
        cache_eligible: false,
        module: module.into(),
        lean_version: version,
        lean_binary_blake3: digest_file(&binary, Path::new("bin/lean"))?.blake3,
        auditor_blake3: blake3::hash(AUDITOR.as_bytes()).to_hex().to_string(),
        artifact_inventory_blake3: blake3::hash(
            &serde_json::to_vec(&files).map_err(|e| e.to_string())?,
        )
        .to_hex()
        .to_string(),
        checked_constants: raw.checked_constants,
        allowed_axioms: options.allowed.clone(),
        forbidden_axioms,
        theorems: raw.theorems,
        artifact_files: files,
        imports,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn identifier_surface_is_bounded_and_cannot_inject_source() {
        assert!(name_valid("Example._theorem'"));
        for name in [
            "",
            "Example..theorem",
            "Example;run_cmd",
            "1name",
            "Éxample",
        ] {
            assert!(!name_valid(name));
        }
        assert!(!name_valid(&"a".repeat(1025)));
    }

    #[test]
    fn panic_output_cannot_be_accepted_with_a_successful_exit_status() {
        let error = execute(
            Command::new("sh").args(["-c", "printf 'PANIC: fake success\\n'; exit 0"]),
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(error.contains("Lean audit failed"));
    }

    #[test]
    fn a_success_record_may_contain_panic_text_in_an_identifier() {
        execute(
            Command::new("sh").args([
                "-c",
                r#"printf '%s\n' 'VESPER_PROOF_AUDIT_V1:{"name":"Example.PANIC"}'"#,
            ]),
            Duration::from_secs(5),
        )
        .unwrap();
    }

    #[test]
    fn an_overdue_checker_is_killed_and_reaped() {
        let started = Instant::now();
        let error = execute(
            Command::new("sh").args(["-c", "while :; do :; done"]),
            Duration::from_millis(30),
        )
        .unwrap_err();
        assert!(error.contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
