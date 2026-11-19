//! Bounded read-only JSON validation through the independent codec core.
use std::{
    fs::{self, OpenOptions},
    io::Read,
};
use vesper_serde::{IndexedJson, Kind, Limits};

const MAX_BYTES: u64 = 1024 * 1024;
const HELP: &str = "Usage: vesper json check FILE [--json]\n\nValidate the complete JSON document, including unknown subtrees and duplicate decoded keys.\nInputs are bounded to 1 MiB regular files; parser depth/token/string/work budgets also apply.\nNo file is modified and no source macros or SIMD code execute.\n";

fn input(path: &str) -> Result<Vec<u8>, String> {
    let regular = || format!("{path}: JSON input must be a regular file without final symlinks");
    if !fs::symlink_metadata(path)
        .map_err(|e| format!("{path}: {e}"))?
        .is_file()
    {
        return Err(regular());
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
    let file = options.open(path).map_err(|e| format!("{path}: {e}"))?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err(regular());
    }
    if meta.len() > MAX_BYTES {
        return Err("JSON input exceeds the 1 MiB CLI limit".into());
    }
    let mut bytes = vec![];
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("JSON input exceeds the 1 MiB CLI limit".into());
    }
    Ok(bytes)
}
fn check(args: Vec<String>) -> Result<Option<serde_json::Value>, String> {
    let mut args = args.into_iter();
    match args.next().as_deref() {
        Some("--help" | "-h") => {
            print!("{HELP}");
            return Ok(None);
        }
        Some("check") => {}
        _ => return Err("json requires check; see json --help".into()),
    }
    let mut path = None;
    for arg in args {
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{HELP}");
                return Ok(None);
            }
            "--json" => {}
            _ if arg.starts_with('-') => return Err(format!("unknown json option '{arg}'")),
            _ => {
                if path.replace(arg).is_some() {
                    return Err("json check accepts one file".into());
                }
            }
        }
    }
    let path = path.ok_or("json check requires a file")?;
    let bytes = input(&path)?;
    let mut limits = Limits::default();
    limits.max_input_bytes = limits.max_input_bytes.min(MAX_BYTES as usize);
    let document = IndexedJson::parse(&bytes, limits).map_err(|e| e.to_string())?;
    let kind = match document.root().kind() {
        Kind::Null => "null",
        Kind::Bool => "bool",
        Kind::Number => "number",
        Kind::String => "string",
        Kind::Array => "array",
        Kind::Object => "object",
    };
    Ok(Some(
        serde_json::json!({"schema_version":1,"status":"valid","bytes":bytes.len(),"root_kind":kind,"tokens":document.token_count(),"work_used":document.work_used(),"index_capacity_bytes":document.index_capacity_bytes()}),
    ))
}
pub fn run(args: Vec<String>) -> Result<(), String> {
    let json = args.iter().any(|arg| arg == "--json");
    match check(args) {
        Ok(None) => Ok(()),
        Ok(Some(report)) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
                );
            } else {
                println!(
                    "Valid JSON: {} root, {} bytes",
                    report["root_kind"].as_str().unwrap_or("unknown"),
                    report["bytes"]
                );
            }
            Ok(())
        }
        Err(message) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &serde_json::json!({"schema_version":1,"status":"invalid","error":message})
                    )
                    .map_err(|e| e.to_string())?
                );
            }
            Err(message)
        }
    }
}
