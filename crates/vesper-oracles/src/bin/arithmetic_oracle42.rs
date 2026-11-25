//! Execute the bounded arithmetic differential suites with the real toolchains.
#![forbid(unsafe_code)]

#[path = "arithmetic_oracle42/fault_peer.rs"]
mod fault_peer;
#[path = "arithmetic_oracle42/mod.rs"]
pub mod oracle;

use oracle::{
    FailureKind, HarnessError, HarnessLimits, RunConfig, RunFailure, RunReport, RunStatus, Suite,
};
use std::{ffi::OsString, io::Write, path::PathBuf, process::ExitCode};

struct Options {
    suite: Suite,
    seed: u64,
    jobs: u8,
    lean: PathBuf,
    artifacts: Option<PathBuf>,
    limits: HarnessLimits,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            suite: Suite::Ci128,
            seed: 0,
            jobs: 1,
            lean: PathBuf::from("lean"),
            artifacts: None,
            limits: HarnessLimits::default(),
        }
    }
}
fn invalid(detail: &str) -> HarnessError {
    HarnessError::new(FailureKind::InvalidConfig, detail)
}
fn number(value: OsString) -> Result<u64, HarnessError> {
    let value = value.to_str().ok_or_else(|| invalid("non-UTF8 number"))?;
    if value.is_empty() || value.len() > 20 || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid("expected an unsigned decimal number"));
    }
    value.parse().map_err(|_| invalid("number exceeds u64"))
}
fn parse(options: &mut Options, args: impl Iterator<Item = OsString>) -> Result<(), HarnessError> {
    let mut args = args;
    let mut seen = 0u8;
    while let Some(flag) = args.next() {
        let bit = match flag.to_str() {
            Some("--suite") => 1,
            Some("--seed") => 2,
            Some("--jobs") => 4,
            Some("--lean") => 8,
            Some("--artifacts") => 16,
            Some("--shrink-attempts") => 32,
            _ => return Err(invalid("unknown option; use --help")),
        };
        if seen & bit != 0 {
            return Err(invalid("duplicate option"));
        }
        seen |= bit;
        let value = args.next().ok_or_else(|| invalid("missing option value"))?;
        match bit {
            1 => {
                options.suite = match number(value)? {
                    128 => Suite::Ci128,
                    100000 => Suite::Full100000,
                    _ => return Err(invalid("suite must be 128 or 100000")),
                };
            }
            2 => options.seed = number(value)?,
            4 => {
                let jobs = number(value)?;
                if !(1..=4).contains(&jobs) {
                    return Err(invalid("jobs must be 1 through 4"));
                }
                options.jobs = jobs as u8;
            }
            8 | 16 => {
                if value.is_empty() || value.len() > 4096 {
                    return Err(invalid("path must contain 1 through 4096 bytes"));
                }
                if bit == 8 {
                    options.lean = value.into();
                } else {
                    options.artifacts = Some(value.into());
                }
            }
            32 => {
                let attempts = number(value)?;
                if attempts > 64 {
                    return Err(invalid("shrink attempts must be 0 through 64"));
                }
                options.limits.shrink_attempts = attempts as u16;
            }
            _ => unreachable!(),
        }
    }
    Ok(())
}
fn emit(value: &impl serde::Serialize) -> ExitCode {
    let bytes = match serde_json::to_vec(value) {
        Ok(bytes) if bytes.len() < 65536 => bytes,
        _ => {
            eprintln!("could not serialize bounded oracle report");
            return ExitCode::FAILURE;
        }
    };
    let mut output = std::io::stdout().lock();
    if output
        .write_all(&bytes)
        .and_then(|()| output.write_all(b"\n"))
        .is_err()
    {
        eprintln!("could not write oracle report");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
fn preflight_failure(options: &Options, error: HarnessError) -> ExitCode {
    let mut partial = RunReport::new(options.suite, options.seed, options.jobs);
    partial.status = RunStatus::Failed;
    let failure = RunFailure {
        error,
        case_id: None,
        partial: Box::new(partial),
    };
    eprintln!("{}", failure.error);
    let _ = emit(&failure);
    ExitCode::FAILURE
}
fn private_result(result: Result<(), HarnessError>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    if first.as_deref() == Some(std::ffi::OsStr::new("--worker")) {
        return if args.next().is_none() {
            private_result(oracle::worker::worker_main())
        } else {
            private_result(Err(invalid("worker mode is exclusive")))
        };
    }
    if first.as_deref() == Some(std::ffi::OsStr::new("--fault-peer")) {
        return match (args.next(), args.next()) {
            (Some(mode), None) => match mode.to_str() {
                Some(mode) => private_result(fault_peer::run(mode)),
                None => private_result(Err(invalid("invalid fault-peer mode"))),
            },
            _ => private_result(Err(invalid("fault-peer mode requires exactly one mode"))),
        };
    }
    if first.as_deref() == Some(std::ffi::OsStr::new("--help")) && args.next().is_none() {
        println!(
            "arithmetic_oracle42 [--suite 128|100000] [--seed U64] [--jobs 1..4]\n  [--lean PATH] [--artifacts DIR] [--shrink-attempts 0..64]"
        );
        return ExitCode::SUCCESS;
    }
    let mut options = Options::default();
    if let Err(error) = parse(&mut options, first.into_iter().chain(args)) {
        return preflight_failure(&options, error);
    }
    let worker_exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(error) => return preflight_failure(&options, error.into()),
    };
    let config = RunConfig {
        suite: options.suite,
        seed: options.seed,
        jobs: options.jobs,
        lean: options.lean,
        worker_exe,
        artifacts: options.artifacts,
        limits: options.limits,
    };
    match oracle::run(&config) {
        Ok(report) => emit(&report),
        Err(failure) => {
            eprintln!("{}", failure.error);
            let _ = emit(&failure);
            ExitCode::FAILURE
        }
    }
}
