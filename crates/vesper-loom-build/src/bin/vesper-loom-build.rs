//! Command-line runner for one offline build step.
//!
//! ```text
//! vesper-loom-build run --cache DIR --request FILE --out DIR
//!                       [--replace] [--policy read-write|verify|uncached]
//!                       [--on-corrupt fail|rebuild]
//! vesper-loom-build key --cache DIR --request FILE
//! vesper-loom-build sweep --cache DIR [--min-age-secs N]
//! vesper-loom-build identity
//! ```
//!
//! `--cache DIR` holds `DIR/store` (the CAS) and `DIR/scratch`. `run`
//! prints the versioned `BuildOutcome` as JSON on stdout. Exit status: 0 on
//! an accepted result, 1 on a build failure, 2 on a usage error.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use vesper_loom_build::{
    BuildEngine, CachePolicy, CorruptionPolicy, DestinationPolicy, ExecutionIdentity, RequestFile,
    RunOptions, Runner,
};

struct Args {
    command: String,
    cache: Option<PathBuf>,
    request: Option<PathBuf>,
    out: Option<PathBuf>,
    options: RunOptions,
    min_age: Duration,
}

fn parse() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let command = it.next().ok_or("missing command")?;
    let mut args = Args {
        command,
        cache: None,
        request: None,
        out: None,
        options: RunOptions::default(),
        min_age: Duration::from_secs(24 * 3600),
    };
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--cache" => args.cache = Some(value()?.into()),
            "--request" => args.request = Some(value()?.into()),
            "--out" => args.out = Some(value()?.into()),
            "--replace" => args.options.destination = DestinationPolicy::Replace,
            "--policy" => {
                args.options.cache = match value()?.as_str() {
                    "read-write" => CachePolicy::ReadWrite,
                    "verify" => CachePolicy::Verify,
                    "uncached" => CachePolicy::Uncached,
                    other => return Err(format!("unknown policy `{other}`")),
                }
            }
            "--on-corrupt" => {
                args.options.on_corrupt = match value()?.as_str() {
                    "fail" => CorruptionPolicy::Fail,
                    "rebuild" => CorruptionPolicy::Rebuild,
                    other => return Err(format!("unknown corruption policy `{other}`")),
                }
            }
            "--min-age-secs" => {
                let secs: u64 = value()?.parse().map_err(|_| "bad --min-age-secs")?;
                args.min_age = Duration::from_secs(secs);
            }
            other => return Err(format!("unknown flag `{other}`")),
        }
    }
    Ok(args)
}

fn runner(args: &Args) -> Result<Runner, String> {
    let cache = args.cache.as_ref().ok_or("--cache is required")?;
    let engine = Arc::new(BuildEngine::new().map_err(|e| e.to_string())?);
    Runner::open(engine, cache.join("store"), cache.join("scratch")).map_err(|e| e.to_string())
}

fn main() -> ExitCode {
    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("usage error: {e}");
            return ExitCode::from(2);
        }
    };
    let result = match args.command.as_str() {
        "identity" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&ExecutionIdentity::current()).unwrap()
            );
            Ok(())
        }
        "run" | "key" | "sweep" => run(&args),
        other => {
            eprintln!("usage error: unknown command `{other}`");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Usage(e)) => {
            eprintln!("usage error: {e}");
            ExitCode::from(2)
        }
        Err(Failure::Build(e)) => {
            eprintln!("build failed: {e}");
            ExitCode::from(1)
        }
    }
}

enum Failure {
    Usage(String),
    Build(String),
}

fn run(args: &Args) -> Result<(), Failure> {
    let runner = runner(args).map_err(Failure::Usage)?;
    if args.command == "sweep" {
        let n = runner
            .sweep_stale(args.min_age)
            .map_err(|e| Failure::Build(e.to_string()))?;
        println!("{{\"removed\":{n}}}");
        return Ok(());
    }
    let path = args
        .request
        .as_ref()
        .ok_or(Failure::Usage("--request is required".into()))?;
    let request = RequestFile::load(path).map_err(|e| Failure::Build(e.to_string()))?;
    let prepared = runner
        .prepare(&request)
        .map_err(|e| Failure::Build(e.to_string()))?;
    if args.command == "key" {
        println!("{}", prepared.key());
        return Ok(());
    }
    let out = args
        .out
        .as_ref()
        .ok_or(Failure::Usage("--out is required".into()))?;
    let outcome = runner
        .execute(&prepared, out, args.options)
        .map_err(|e| Failure::Build(e.to_string()))?;
    println!("{}", serde_json::to_string_pretty(&outcome).unwrap());
    Ok(())
}
