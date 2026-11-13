use std::{
    fs::OpenOptions,
    io::Read,
    path::{Path, PathBuf},
};
use vesper::{
    doctest::{self, DocTestLimits, DocTestReport},
    tooling::doc::sources::{SourceLimits, discover},
};

const USAGE: &str = "usage: vesper test --doc [FILE|DIR] [--json]";
const HELP: &str = "Usage: vesper test --doc [FILE|DIR] [--json]\n\nExtract actual Vesper doc-comment fences, fully check examples, and run supported finite scalar entries in one owned JIT module.\nWith no path (or a directory), run every .ves source of the package that has doc examples: the nearest\nLoom.toml at or above the directory (else the directory itself); hidden, target/, bazel-* and nested-package\ndirectories are skipped and symlinks are not followed. Files without examples are listed, not run; a package\nwith no examples at all fails.\ncompile_fail(E...) must match exact example diagnostics; base errors and unsupported execution never count as success.\n`vesper,sim(seed=N,clock,net,disk)` fences run on the reference evaluator under a seeded SimPulse kernel with\nsimulated clock, network and disk effects, twice: the second run replays the first run's journal.\nSource/check/module budgets apply; this is not an OS sandbox or a hard deadline.\n";

fn read_source(file: &Path, limits: DocTestLimits) -> Result<String, String> {
    let name = file.display();
    if !std::fs::metadata(file)
        .map_err(|e| format!("{name}: {e}"))?
        .is_file()
    {
        return Err("doctest input must be a regular file".into());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A file-to-FIFO replacement must not block before handle validation.
        options.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let input = options.open(file).map_err(|e| format!("{name}: {e}"))?;
    if !input.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("doctest input must be a regular file".into());
    }
    let mut bytes = Vec::new();
    input
        .take(
            u64::try_from(
                limits
                    .max_source_bytes
                    .checked_add(1)
                    .ok_or("source limit overflow")?,
            )
            .map_err(|e| e.to_string())?,
        )
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let source = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
    Ok(source.to_string())
}

fn print_report(file: &str, report: &DocTestReport) {
    for d in &report.base_diagnostics {
        println!("{file}:{}:{}: {}: {}", d.line, d.column, d.code, d.message);
    }
    for example in &report.examples {
        println!(
            "{file}:{}:{}: {} doctest {}: {}",
            example.line,
            example.column,
            if example.passed { "PASS" } else { "FAIL" },
            example.index,
            example.message
        );
        for d in &example.diagnostics {
            println!("{file}:{}:{}: {}: {}", d.line, d.column, d.code, d.message);
        }
    }
}

fn parse(args: Vec<String>) -> Result<(Option<String>, bool), String> {
    let mut file = None;
    let mut json = false;
    let mut doc = false;
    for argument in args {
        match argument.as_str() {
            "--doc" if !doc => doc = true,
            "--json" if !json => json = true,
            value if !value.starts_with('-') && file.is_none() => file = Some(value.to_string()),
            _ => return Err(USAGE.into()),
        }
    }
    if !doc {
        return Err(USAGE.into());
    }
    Ok((file, json))
}

fn run_file(file: &str, json: bool, limits: DocTestLimits) -> Result<(), String> {
    let report = match read_source(Path::new(file), limits)
        .and_then(|source| doctest::run(&source, limits))
    {
        Ok(report) => report,
        Err(error) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"file":file,"passed":false,"error":error})
                );
            }
            return Err(error);
        }
    };
    if json {
        println!(
            "{}",
            serde_json::json!({"file":file,"passed":report.passed,"report":report})
        );
    } else {
        print_report(file, &report);
    }
    if report.passed {
        Ok(())
    } else {
        Err("documentation tests failed".into())
    }
}

/// Every discovered file is visited; the run fails after all files if any
/// file failed, and fails if no file in the package has examples.
fn run_package(start: &Path, json: bool, limits: DocTestLimits) -> Result<(), String> {
    let package = match discover(start, SourceLimits::default()) {
        Ok(package) => package,
        Err(error) => {
            if json {
                println!("{}", serde_json::json!({"passed":false,"error":error}));
            }
            return Err(error);
        }
    };
    let mut files = Vec::new();
    let (mut tested, mut failed, mut skipped) = (0usize, 0usize, 0usize);
    for path in &package.files {
        let name = package.display(path);
        let source = read_source(path, limits);
        let has = source
            .as_deref()
            .map_err(Clone::clone)
            .and_then(|s| doctest::has_examples(s, limits));
        match (source, has) {
            (Ok(_), Ok(false)) => {
                skipped += 1;
                if !json {
                    println!("{name}: no doc examples");
                }
                files.push(serde_json::json!({"file":name,"examples":false}));
            }
            (Ok(source), Ok(true)) => {
                tested += 1;
                match doctest::run(&source, limits) {
                    Ok(report) => {
                        if !report.passed {
                            failed += 1;
                        }
                        if !json {
                            print_report(&name, &report);
                        }
                        files.push(serde_json::json!({"file":name,"examples":true,"passed":report.passed,"report":report}));
                    }
                    Err(error) => {
                        failed += 1;
                        if !json {
                            println!("{name}: FAIL: {error}");
                        }
                        files.push(serde_json::json!({"file":name,"examples":true,"passed":false,"error":error}));
                    }
                }
            }
            (Err(error), _) | (Ok(_), Err(error)) => {
                tested += 1;
                failed += 1;
                if !json {
                    println!("{name}: FAIL: {error}");
                }
                files.push(serde_json::json!({"file":name,"passed":false,"error":error}));
            }
        }
    }
    let error = if tested == 0 {
        Some(format!(
            "no Vesper documentation examples found in package {}",
            package.root.display()
        ))
    } else if failed > 0 {
        Some(format!("documentation tests failed in {failed} file(s)"))
    } else {
        None
    };
    if json {
        println!(
            "{}",
            serde_json::json!({
                "root":package.root.display().to_string(),
                "manifest":package.manifest.as_ref().map(|m| m.display().to_string()),
                "passed":error.is_none(),
                "files":files,
                "error":error,
            })
        );
    } else {
        println!(
            "doctests: {tested} file(s) tested, {failed} failed, {skipped} without examples, under {}",
            package.root.display()
        );
    }
    error.map_or(Ok(()), Err)
}

pub fn run(args: Vec<String>) -> Result<(), String> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{HELP}");
        return Ok(());
    }
    let requested_json = args.iter().any(|arg| arg == "--json");
    let (file, json) = match parse(args) {
        Ok(options) => options,
        Err(error) => {
            if requested_json {
                println!("{}", serde_json::json!({"passed":false,"error":error}));
            }
            return Err(error);
        }
    };
    let limits = DocTestLimits::default();
    match file {
        None => run_package(&PathBuf::from("."), json, limits),
        Some(path) if std::fs::metadata(&path).is_ok_and(|m| m.is_dir()) => {
            run_package(Path::new(&path), json, limits)
        }
        Some(path) => run_file(&path, json, limits),
    }
}
