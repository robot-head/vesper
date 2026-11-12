//! Filesystem/CLI adapter for pure lint tooling.
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
#[allow(unused_imports)]
pub use vesper::tooling::lint::{
    ConfigProvenance, PolicyOutcome, PolicyReport, PreparedLint, ReportedFinding, SourceReport,
    prepare_source, prepare_source_with_policy,
};
use vesper_lint::policy::{Policy, PolicyLimits};
const MAX_BYTES: usize = 262144;
pub const HELP: &str = "Usage: vesper lint FILE [--json] [--fix] [--semver-safe] [--config PATH]\n\nCheck actual resolved effects and supported inout field accesses.\nUnused effects (L0102) may be removed only after complete candidate reparse/recheck.\nProjection narrowing (L0101) is diagnostic-only: caller/signature changes need review.\n--semver-safe requires --fix and validates exported supported interface compatibility.\nHandlers, closures, indirect/open-row/recursive calls remain conservative.\nInputs: regular UTF-8 files <=256 KiB; token/nesting/work budgets also apply.\n--config loads one explicit bounded Loom.toml policy; no adjacent/default discovery.\nAllow suppresses diagnostics and fixes; warn succeeds; remaining deny exits nonzero.\nFuture warn/allow lints are reported unavailable; future deny/plugins fail closed.\nNo arena escape, mode-relaxation, FIP, secret/Cancel or plugin-pack analysis.\n";
fn read_source(path: &Path) -> Result<(String, fs::Permissions), String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !metadata.is_file() {
        return Err("lint input must be a regular file without final symlinks".into());
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
    if !metadata.is_file() || metadata.len() > MAX_BYTES as u64 {
        return Err("lint input is not a bounded regular file (256 KiB)".into());
    }
    let mut bytes = vec![];
    file.take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_BYTES {
        return Err("lint source exceeds 256 KiB".into());
    }
    Ok((
        String::from_utf8(bytes).map_err(|_| "lint input must be UTF-8")?,
        metadata.permissions(),
    ))
}
fn load_policy(path: &Path) -> Result<(Policy, ConfigProvenance), String> {
    const MAX_CONFIG_BYTES: u64 = 65536;
    if path.as_os_str().len() > 4096 {
        return Err("lint config path exceeds 4096 bytes".into());
    }
    if !fs::symlink_metadata(path)
        .map_err(|e| e.to_string())?
        .is_file()
    {
        return Err("lint config must be a regular file without final symlinks".into());
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
    if !metadata.is_file() || metadata.len() > MAX_CONFIG_BYTES {
        return Err("lint config must be a bounded regular file (64 KiB)".into());
    }
    let mut bytes = vec![];
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err("lint config exceeds 64 KiB".into());
    }
    let source = std::str::from_utf8(&bytes).map_err(|_| "lint config must be UTF-8")?;
    let policy = Policy::parse(source, PolicyLimits::default()).map_err(|e| e.to_string())?;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    };
    let provenance = ConfigProvenance::Explicit {
        path: absolute.display().to_string(),
        bytes: bytes.len(),
        blake3: blake3::hash(&bytes).to_hex().to_string(),
    };
    Ok((policy, provenance))
}
struct Staging(Option<PathBuf>);
impl Staging {
    fn publish(&mut self, destination: &Path) -> Result<(), String> {
        let path = self
            .0
            .as_deref()
            .ok_or("staging path ownership already transferred")?;
        fs::rename(path, destination).map_err(|e| e.to_string())?;
        // The vacated temporary name is no longer ours. Another process may
        // create a new file there before this guard is dropped.
        self.0 = None;
        Ok(())
    }
}
impl Drop for Staging {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}
fn replace_source(
    path: &Path,
    original: &str,
    candidate: &str,
    permissions: fs::Permissions,
) -> Result<(), String> {
    if permissions.readonly() {
        return Err("lint refuses to replace a readonly source".into());
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temporary_path = parent.join(format!(
        ".vesper-lint-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    replace_at(path, original, candidate, permissions, temporary_path)
}
fn replace_at(
    path: &Path,
    original: &str,
    candidate: &str,
    permissions: fs::Permissions,
    temporary_path: PathBuf,
) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary_path).map_err(|e| e.to_string())?;
    let mut temporary = Staging(Some(temporary_path));
    file.write_all(candidate.as_bytes())
        .map_err(|e| e.to_string())?;
    file.set_permissions(permissions)
        .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    let (current, _) = read_source(path)?;
    if current != original {
        return Err(
            "source changed while lint was validating; original path left untouched".into(),
        );
    }
    temporary.publish(path)
}
pub fn run(args: Vec<String>) -> Result<(), String> {
    let json = args.iter().any(|a| a == "--json");
    let mut config_label: Option<String> = None;
    let result = (|| {
        let mut path = None;
        let mut config = None;
        let mut fix = false;
        let mut semver_safe = false;
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--help" | "-h" => {
                    print!("{HELP}");
                    return Ok(None);
                }
                "--json" => (),
                "--fix" => fix = true,
                "--semver-safe" => semver_safe = true,
                "--config" => {
                    if config.is_some() {
                        return Err("--config may be specified only once".into());
                    }
                    let value = args.next().ok_or("--config requires a path")?;
                    if value.len() > 4096 {
                        return Err("lint config path exceeds 4096 bytes".into());
                    }
                    if value.starts_with('-') {
                        return Err("--config requires a path".into());
                    }
                    config_label = Some(value.clone());
                    config = Some(PathBuf::from(value));
                }
                _ if arg.starts_with('-') => return Err(format!("unknown lint option '{arg}'")),
                _ => {
                    if path.replace(PathBuf::from(arg)).is_some() {
                        return Err("lint accepts one source file".into());
                    }
                }
            }
        }
        let path = path.ok_or("lint requires a source file")?;
        let (policy, provenance) = if let Some(config) = config {
            load_policy(&config)?
        } else {
            (Policy::default(), ConfigProvenance::Default)
        };
        let (source, permissions) = read_source(&path)?;
        let prepared = if config_label.is_none() {
            prepare_source(&source, fix, semver_safe)?
        } else {
            prepare_source_with_policy(&source, fix, semver_safe, &policy, provenance)?
        };
        let output = serde_json::to_string_pretty(&prepared.report).map_err(|e| e.to_string())?;
        if let Some(candidate) = prepared.candidate {
            replace_source(&path, &source, &candidate, permissions)?;
        }
        Ok(Some((prepared.report, output)))
    })();
    match result {
        Ok(None) => Ok(()),
        Ok(Some((report, output))) => {
            if json {
                println!("{output}");
            } else {
                for finding in &report.diagnostics {
                    println!(
                        "{:?} {} {}:{}..{}: {}",
                        finding.severity,
                        finding.finding.code,
                        finding.finding.function,
                        finding.finding.span.start,
                        finding.finding.span.end,
                        finding.finding.message
                    );
                }
                for rule in &report.policy.unavailable {
                    println!(
                        "Unavailable lint {} ({:?}); not analyzed",
                        rule.lint.as_str(),
                        rule.severity
                    );
                }
                println!(
                    "lint: {} diagnostic(s), {} conservative skip(s), changed={}, remaining deny={}",
                    report.diagnostics.len(),
                    report.skipped.len(),
                    report.changed,
                    report.outcome.remaining_denied
                );
            }
            if report.status == "denied" {
                Err("lint policy denied remaining findings or unavailable analysis".into())
            } else {
                Ok(())
            }
        }
        Err(error) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"schema_version":1,"status":"invalid","error":error,"config":config_label})
                );
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (PathBuf, PathBuf) {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-lint-transaction-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let file = dir.join("source.ves");
        fs::write(&file, "current").unwrap();
        (dir, file)
    }
    #[test]
    fn stale_contents_and_private_staging_collisions_preserve_files() {
        let (dir, path) = fixture();
        let permissions = fs::metadata(&path).unwrap().permissions();
        let temporary = dir.join("stage");
        assert!(
            replace_at(
                &path,
                "outdated",
                "candidate",
                permissions.clone(),
                temporary.clone()
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "current");
        assert!(!temporary.exists());
        fs::write(&temporary, "preexisting staging owner").unwrap();
        assert!(
            replace_at(
                &path,
                "current",
                "candidate",
                permissions,
                temporary.clone()
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "current");
        assert_eq!(
            fs::read_to_string(&temporary).unwrap(),
            "preexisting staging owner"
        );
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn published_staging_guard_preserves_a_new_owner_at_its_vacated_path() {
        let (dir, destination) = fixture();
        let temporary = dir.join("stage");
        fs::write(&temporary, "validated candidate").unwrap();
        let mut guard = Staging(Some(temporary.clone()));
        guard.publish(&destination).unwrap();
        fs::write(&temporary, "new owner after rename").unwrap();
        drop(guard);
        assert_eq!(
            fs::read_to_string(&temporary).unwrap(),
            "new owner after rename"
        );
        assert_eq!(
            fs::read_to_string(&destination).unwrap(),
            "validated candidate"
        );
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn failed_staging_publication_keeps_cleanup_ownership() {
        let (dir, original) = fixture();
        let temporary = dir.join("stage");
        let blocked = dir.join("directory");
        fs::create_dir(&blocked).unwrap();
        fs::write(&temporary, "candidate").unwrap();
        let mut guard = Staging(Some(temporary.clone()));
        assert!(guard.publish(&blocked).is_err());
        assert!(temporary.exists());
        drop(guard);
        assert!(!temporary.exists());
        assert!(blocked.is_dir());
        assert_eq!(fs::read_to_string(&original).unwrap(), "current");
        fs::remove_dir_all(dir).unwrap();
    }
}
