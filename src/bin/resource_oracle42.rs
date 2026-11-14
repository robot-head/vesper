//! Run one finite resource-model request through the actual pinned Lean worker.
#![forbid(unsafe_code)]

#[path = "resource_oracle42/mod.rs"]
pub mod oracle;

use oracle::{Config, HarnessLimits, protocol};
use std::{ffi::OsString, fs::File, io::Read, io::Write, path::PathBuf, process::ExitCode};

const REPORT_CAP: usize = 4 * 1024 * 1024;

struct Options {
    lean: PathBuf,
    repo_root: PathBuf,
    request: PathBuf,
    artifacts: Option<PathBuf>,
}

fn value(args: &mut impl Iterator<Item = OsString>, flag: &str) -> Result<PathBuf, String> {
    args.next()
        .map(PathBuf::from)
        .ok_or_else(|| format!("missing value for {flag}"))
}

fn options() -> Result<Option<Options>, String> {
    let mut args = std::env::args_os().skip(1);
    let mut lean = std::env::var_os("VESPER_LEAN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("lean"));
    let mut repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut request = None;
    let mut artifacts = None;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--help" | "-h") => return Ok(None),
            Some("--lean") => lean = value(&mut args, "--lean")?,
            Some("--repo-root") => repo_root = value(&mut args, "--repo-root")?,
            Some("--request") => {
                if request.is_some() {
                    return Err("--request may be supplied once".into());
                }
                request = Some(value(&mut args, "--request")?);
            }
            Some("--artifacts") => artifacts = Some(value(&mut args, "--artifacts")?),
            _ => return Err("unknown argument; use --help".into()),
        }
    }
    Ok(Some(Options {
        lean,
        repo_root,
        request: request.ok_or("--request is required")?,
        artifacts,
    }))
}

fn run(options: Options) -> Result<(), String> {
    if !std::fs::metadata(&options.request)
        .map_err(|e| format!("request metadata: {e}"))?
        .is_file()
    {
        return Err("request must be a regular file".into());
    }
    let file = File::open(&options.request).map_err(|e| format!("request file: {e}"))?;
    if !file
        .metadata()
        .map_err(|e| format!("request metadata: {e}"))?
        .is_file()
    {
        return Err("request must be a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(protocol::REQUEST_CAP as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("request read: {e}"))?;
    if bytes.len() > protocol::REQUEST_CAP {
        return Err("request exceeds the fixed frame bound".into());
    }
    let (sequence, request) = protocol::decode_request(&bytes).map_err(|e| e.to_string())?;
    if sequence != 0 {
        return Err("a single-request session must start at sequence zero".into());
    }
    let mut session = oracle::start(&Config {
        lean: options.lean,
        repo_root: options.repo_root,
        artifact_dir: options.artifacts,
        limits: HarnessLimits::default(),
    })
    .map_err(|e| e.to_string())?;
    let response = session.evaluate(&request).map_err(|e| e.to_string())?;
    let evidence = session.finish().map_err(|e| e.to_string())?;
    let report = serde_json::json!({
        "schema_version": 1,
        "provider": protocol::PROTOCOL_ID,
        "model_only": true,
        "stage": response.stage(),
        "response": response,
        "session": evidence,
    });
    let mut output = serde_json::to_vec(&report).map_err(|e| format!("report encoding: {e}"))?;
    if output.len() > REPORT_CAP {
        return Err("observation report exceeds its fixed bound".into());
    }
    output.push(b'\n');
    std::io::stdout()
        .lock()
        .write_all(&output)
        .map_err(|e| format!("report write: {e}"))
}

fn main() -> ExitCode {
    match options() {
        Ok(None) => {
            println!(
                "resource_oracle42 --request FILE [--lean PATH] [--repo-root PATH] [--artifacts DIR]\n\
                 Serve one sequence-zero JSON request using the finite resource model.\n\
                 A model refusal is returned as data; transport/build failures exit nonzero.\n\
                 This endpoint does not execute Vesper source or a native backend."
            );
            ExitCode::SUCCESS
        }
        Ok(Some(options)) => match run(options) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("resource_oracle42: {error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("resource_oracle42: {error}");
            ExitCode::FAILURE
        }
    }
}
