//! Bounded source formatting with successful-only atomic replacement.
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use vesper_fmt::{FormatResult, Limits, format_recovery_source, format_source};
const MAX_BYTES: u64 = 256 * 1024;
const HELP: &str = "Usage: vesper fmt FILE [--recovery] [--check] [--json]\n\nFormat supported Vesper source and expression macros with a canonical 100-column ribbon.\nRecovery mode formats only complete functions and preserves damaged or opaque source bytes.\nCheck mode never writes and exits nonzero when formatting is needed.\nInputs are bounded to 256 KiB regular UTF-8 files without final symlinks.\nDefault mode rejects invalid/incomplete source; failures always leave input unchanged.\nToken/string/comment bytes are preserved; indivisible leaves may exceed 100 columns.\n";
struct Input {
    text: String,
    metadata: fs::Metadata,
}
fn open_input(path: &Path) -> Result<Input, String> {
    if !fs::symlink_metadata(path)
        .map_err(|e| e.to_string())?
        .is_file()
    {
        return Err("fmt requires a regular file without final symlinks".into());
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
    if !metadata.is_file() {
        return Err("fmt requires a regular file".into());
    }
    if metadata.len() > MAX_BYTES {
        return Err("fmt input exceeds 256 KiB".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("fmt input exceeds 256 KiB".into());
    }
    let text = String::from_utf8(bytes).map_err(|_| "fmt input must be UTF-8")?;
    Ok(Input { text, metadata })
}
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if a.dev() != b.dev() || a.ino() != b.ino() {
            return false;
        }
    }
    a.len() == b.len() && a.modified().ok() == b.modified().ok()
}
struct Staging {
    path: PathBuf,
    cleanup: bool,
}
impl Drop for Staging {
    fn drop(&mut self) {
        if self.cleanup {
            // The guard exists only after exclusive creation. Windows rejects
            // unlinking readonly files, including stages that inherited the
            // input's readonly bit before a later drift/rename error. Clear
            // that bit only on this invocation-owned staging path. Unix mode
            // bits are never broadened. Cleanup remains best effort on OS error.
            #[cfg(windows)]
            #[allow(
                clippy::permissions_set_readonly_false,
                reason = "Windows-only owned-stage cleanup clears the readonly attribute; Unix permission bits are untouched"
            )]
            if let Ok(metadata) = fs::metadata(&self.path) {
                let mut permissions = metadata.permissions();
                if permissions.readonly() {
                    permissions.set_readonly(false);
                    let _ = fs::set_permissions(&self.path, permissions);
                }
            }
            let _ = fs::remove_file(&self.path);
        }
    }
}
fn replace(path: &Path, input: &Input, output: &str) -> Result<(), String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    path.file_name().ok_or("fmt path must name a file")?;
    let mut current = NEXT.load(Ordering::Relaxed);
    let next = loop {
        let next = current
            .checked_add(1)
            .ok_or("fmt staging counter exhausted")?;
        match NEXT.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break current,
            Err(observed) => current = observed,
        }
    };
    let staging = parent.join(format!(".vesper-fmt-{}-{next}", std::process::id()));
    replace_at_stage(path, input, output, staging)
}
fn replace_at_stage(
    path: &Path,
    input: &Input,
    output: &str,
    staging: PathBuf,
) -> Result<(), String> {
    // Own cleanup only after exclusive creation succeeds: a colliding existing
    // file must never be deleted by a staging guard.
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staging)
        .map_err(|e| e.to_string())?;
    let mut stage = Staging {
        path: staging,
        cleanup: true,
    };
    file.write_all(output.as_bytes())
        .map_err(|e| e.to_string())?;
    file.set_permissions(input.metadata.permissions())
        .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    // Reject observed replacement/edits. This is not a filesystem compare-and-swap:
    // an external writer racing the final rename still needs caller coordination.
    let current = open_input(path)?;
    if current.text != input.text || !same_file(&current.metadata, &input.metadata) {
        return Err("fmt input changed during formatting; retry".into());
    }
    // Rename is the commit. No subsequent fallible operation reports failure.
    fs::rename(&stage.path, path).map_err(|e| e.to_string())?;
    stage.cleanup = false;
    Ok(())
}
fn execute(args: Vec<String>) -> Result<(serde_json::Value, bool), String> {
    let mut path = None;
    let mut check = false;
    let mut recovery = false;
    let mut positional = false;
    for arg in args {
        if !positional {
            match arg.as_str() {
                "--check" => {
                    check = true;
                    continue;
                }
                "--json" => continue,
                "--recovery" => {
                    recovery = true;
                    continue;
                }
                "--" => {
                    positional = true;
                    continue;
                }
                _ if arg.starts_with('-') => return Err(format!("unknown fmt option '{arg}'")),
                _ => {}
            }
        }
        if path.replace(PathBuf::from(arg)).is_some() {
            return Err("fmt accepts one file".into());
        }
    }
    let path = path.ok_or("fmt requires a file; see fmt --help")?;
    let input = open_input(&path)?;
    let (result, edits) = if recovery {
        let partial = format_recovery_source(&input.text, Limits::default())
            .map_err(|e| format!("cannot format recovery source: {e}"))?;
        let count = partial.edits().len();
        (
            FormatResult {
                changed: partial.changed(),
                output: partial.into_output(),
            },
            Some(count),
        )
    } else {
        (
            format_source(&input.text, Limits::default())
                .map_err(|e| format!("cannot format source: {e}"))?,
            None,
        )
    };
    let status = if check && result.changed {
        "needs_format"
    } else if result.changed {
        "formatted"
    } else {
        "unchanged"
    };
    let mut report = serde_json::json!({"schema_version":1,"status":status,"changed":result.changed,"check":check,"input_bytes":input.text.len(),"output_bytes":result.output.len(),"width":100,"mode":if recovery {"recovery"} else {"source"}});
    if let Some(count) = edits {
        report["edits"] = count.into();
    }
    if result.changed && !check {
        replace(&path, &input, &result.output)?;
    }
    Ok((report, check && result.changed))
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
    match execute(args) {
        Ok((report, needs_format)) => {
            if json {
                println!("{report}");
            } else {
                println!("fmt: {}", report["status"].as_str().unwrap_or("unknown"));
            }
            if needs_format {
                Err("source is not canonically formatted".into())
            } else {
                Ok(())
            }
        }
        Err(error) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"schema_version":1,"status":"error","error":error})
                );
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "vesper-fmt-stage-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        path
    }
    #[test]
    fn staging_collision_cannot_delete_an_existing_file() {
        let directory = fixture();
        let path = directory.join("input.ves");
        let stage = directory.join("existing");
        fs::write(&path, "fn main(){1}").unwrap();
        let input = open_input(&path).unwrap();
        fs::write(&stage, "do not delete").unwrap();
        assert!(replace_at_stage(&path, &input, "fn main() {\n    1\n}\n", stage.clone()).is_err());
        assert_eq!(fs::read_to_string(&stage).unwrap(), "do not delete");
        assert_eq!(fs::read_to_string(&path).unwrap(), input.text);
        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn observed_concurrent_edit_is_not_overwritten_and_owned_stage_is_removed() {
        let directory = fixture();
        let path = directory.join("input.ves");
        let stage = directory.join("owned-stage");
        fs::write(&path, "fn main(){1}").unwrap();
        let input = open_input(&path).unwrap();
        fs::write(&path, "fn main(){2}").unwrap();
        assert!(replace_at_stage(&path, &input, "fn main() {\n    1\n}\n", stage.clone()).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "fn main(){2}");
        assert!(!stage.exists());
        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn reports_describe_check_and_successful_replacement() {
        let directory = fixture();
        let path = directory.join("input.ves");
        fs::write(&path, "fn main(){1}").unwrap();
        let (check, needs) = execute(vec![
            path.to_string_lossy().into_owned(),
            "--check".into(),
            "--json".into(),
        ])
        .unwrap();
        assert!(needs);
        assert_eq!(check["status"], "needs_format");
        assert_eq!(fs::read_to_string(&path).unwrap(), "fn main(){1}");
        let (written, needs) = execute(vec![path.to_string_lossy().into_owned()]).unwrap();
        assert!(!needs);
        assert_eq!(written["status"], "formatted");
        assert_eq!(written["width"], 100);
        let (check, needs) =
            execute(vec![path.to_string_lossy().into_owned(), "--check".into()]).unwrap();
        assert!(!needs);
        assert_eq!(check["status"], "unchanged");
        assert_eq!(check["changed"], false);
        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn readonly_owned_stage_is_cleaned_on_actual_precommit_drift_error() {
        let directory = fixture();
        let path = directory.join("input.ves");
        let stage = directory.join("readonly-owned-stage");
        fs::write(&path, "fn main(){1}").unwrap();
        let writable = fs::metadata(&path).unwrap().permissions();
        let mut readonly = writable.clone();
        readonly.set_readonly(true);
        fs::set_permissions(&path, readonly).unwrap();
        let input = open_input(&path).unwrap();
        assert!(input.metadata.permissions().readonly());
        // Keep an original readonly permission snapshot, then simulate a new
        // source edit. The actual helper inherits readonly onto its own stage
        // before rejecting this observed drift, so Windows must clear it.
        fs::set_permissions(&path, writable.clone()).unwrap();
        fs::write(&path, "fn main(){2}").unwrap();
        let error = replace_at_stage(&path, &input, "candidate", stage.clone()).unwrap_err();
        assert!(error.contains("input changed"), "{error}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "fn main(){2}");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().readonly(),
            writable.readonly()
        );
        assert!(!stage.exists());
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn readonly_collision_is_not_unlinked_or_made_writable() {
        let directory = fixture();
        let path = directory.join("input.ves");
        let stage = directory.join("readonly-foreign-stage");
        fs::write(&path, "fn main(){1}").unwrap();
        let input = open_input(&path).unwrap();
        fs::write(&stage, "foreign owner").unwrap();
        let original = fs::metadata(&stage).unwrap().permissions();
        let mut readonly = original.clone();
        readonly.set_readonly(true);
        fs::set_permissions(&stage, readonly).unwrap();
        assert!(replace_at_stage(&path, &input, "candidate", stage.clone()).is_err());
        assert_eq!(fs::read_to_string(&stage).unwrap(), "foreign owner");
        assert!(fs::metadata(&stage).unwrap().permissions().readonly());
        assert_eq!(fs::read_to_string(&path).unwrap(), input.text);
        // Test cleanup restores exactly the prior permission snapshot.
        fs::set_permissions(&stage, original).unwrap();
        fs::remove_dir_all(directory).unwrap();
    }
}
