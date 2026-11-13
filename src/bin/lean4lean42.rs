//! Developer-only fixed-model external replay; no source compiler integration.
#![forbid(unsafe_code)]
#[path = "lean4lean42/mod.rs"]
pub mod driver;
use driver::{Config, Error, ErrorKind, Failure, Limits, Report};
use std::{collections::BTreeSet, io::Write, path::PathBuf};

fn emit(value: &impl serde::Serialize, cap: usize) -> Result<(), Error> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| Error::new(ErrorKind::Protocol, "result serialization"))?;
    if bytes.len() > cap {
        return Err(Error::new(ErrorKind::OutputLimit, "result JSON byte cap"));
    }
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&bytes)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}
fn arguments() -> Result<Vec<String>, Error> {
    let mut args = Vec::new();
    args.try_reserve_exact(8)
        .map_err(|_| Error::new(ErrorKind::Allocation, "option allocation"))?;
    for arg in std::env::args_os().skip(1) {
        if args.len() >= 8 {
            return Err(Error::new(ErrorKind::InvalidConfig, "too many options"));
        }
        let arg = arg
            .into_string()
            .map_err(|_| Error::new(ErrorKind::InvalidConfig, "options require UTF-8"))?;
        if arg.len() > 4096 {
            return Err(Error::new(ErrorKind::InvalidConfig, "option byte cap"));
        }
        args.push(arg)
    }
    Ok(args)
}
fn config(args: &[String]) -> Result<Config, Error> {
    if args.len() % 2 != 0 {
        return Err(Error::new(
            ErrorKind::InvalidConfig,
            "options require values",
        ));
    }
    let mut seen = BTreeSet::new();
    let mut root = None;
    let mut lean = None;
    let mut artifacts = None;
    for pair in args.chunks_exact(2) {
        if !seen.insert(pair[0].as_str()) || pair[1].is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidConfig,
                "duplicate or empty option",
            ));
        }
        match pair[0].as_str() {
            "--tool-root" => root = Some(PathBuf::from(&pair[1])),
            "--lean" => lean = Some(PathBuf::from(&pair[1])),
            "--artifacts" => artifacts = Some(PathBuf::from(&pair[1])),
            _ => return Err(Error::new(ErrorKind::InvalidConfig, "unknown option")),
        }
    }
    let tool_root =
        root.ok_or_else(|| Error::new(ErrorKind::InvalidConfig, "--tool-root is required"))?;
    let lean = match lean {
        Some(p) => p,
        None => driver::receipt_compiler(&tool_root)?,
    };
    Ok(Config {
        tool_root,
        lean,
        artifacts,
        limits: Limits::default(),
    })
}
fn failure(error: Error) -> Failure {
    Failure {
        error,
        partial: Box::new(Report::new(Limits::default())),
    }
}
fn main() -> std::process::ExitCode {
    let args = match arguments() {
        Ok(v) => v,
        Err(e) => {
            let _ = emit(&failure(e), Limits::default().report_bytes);
            return std::process::ExitCode::FAILURE;
        }
    };
    if args.len() == 2 {
        match args[0].as_str() {
            "--fault-peer" => {
                return match driver::fault_peer::run(&args[1]) {
                    Ok(()) => std::process::ExitCode::SUCCESS,
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::ExitCode::FAILURE
                    }
                };
            }
            "--seal-setup" => {
                return match driver::seal_setup(std::path::Path::new(&args[1])) {
                    Ok(receipt) => match emit(&receipt, 4 * 1024 * 1024) {
                        Ok(()) => std::process::ExitCode::SUCCESS,
                        Err(e) => {
                            eprintln!("{e}");
                            std::process::ExitCode::FAILURE
                        }
                    },
                    Err(e) => {
                        let _ = emit(&e, 65536);
                        std::process::ExitCode::FAILURE
                    }
                };
            }
            "--verify-setup-source" => {
                return match driver::verify_setup_source(std::path::Path::new(&args[1])) {
                    Ok(hash) => match emit(
                        &serde_json::json!({"schema":1,"source_manifest_blake3":hash}),
                        65536,
                    ) {
                        Ok(()) => std::process::ExitCode::SUCCESS,
                        Err(e) => {
                            eprintln!("{e}");
                            std::process::ExitCode::FAILURE
                        }
                    },
                    Err(e) => {
                        let _ = emit(&e, 65536);
                        std::process::ExitCode::FAILURE
                    }
                };
            }
            _ => {}
        }
    }
    let config = match config(&args) {
        Ok(c) => c,
        Err(e) => {
            let _ = emit(&failure(e), Limits::default().report_bytes);
            return std::process::ExitCode::FAILURE;
        }
    };
    match driver::run(&config) {
        Ok(report) => match emit(&report, config.limits.report_bytes) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                let _ = emit(&failure(e), config.limits.report_bytes);
                std::process::ExitCode::FAILURE
            }
        },
        Err(f) => {
            let _ = emit(&f, config.limits.report_bytes);
            std::process::ExitCode::FAILURE
        }
    }
}
