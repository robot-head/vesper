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
const HELP: &str = "Usage: vesper test --doc [FILE|DIR] [--json]\n\nExtract actual Vesper doc-comment fences, fully check examples, and run supported finite scalar entries in one shared JIT module per file;\nexamples outside the native subset run on the reference evaluator instead, and each result names its engine.\nWith no path (or a directory), run every .ves source of the package that has doc examples: the nearest\nLoom.toml at or above the directory (else the directory itself); hidden, target/, bazel-* and nested-package\ndirectories are skipped and symlinks are not followed. Files without examples are listed, not run; a package\nwith no examples at all fails. Each file's examples are checked against that file; the runnable examples of all\nfiles then share ONE package module and ONE JIT module when the files' top-level names are disjoint, otherwise\neach file runs in its own module and the report says why.\ncompile_fail(E...) must match exact example diagnostics; base errors and unsupported execution never count as success.\n`vesper,sim(seed=N,clock,net,disk)` fences run on the reference evaluator under a seeded SimPulse kernel with\nsimulated clock, network and disk effects, twice: the second run replays the first run's journal.\nSource/check/module budgets apply; this is not an OS sandbox or a hard deadline.\n";

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
        let engine = match example.engine {
            Some(doctest::Engine::Jit) => " [jit]",
            Some(doctest::Engine::Evaluator) => " [evaluator]",
            Some(doctest::Engine::SimpulseEvaluator) => " [simpulse]",
            None => "",
        };
        println!(
            "{file}:{}:{}: {} doctest {}: {}{engine}",
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

/// Every discovered file is visited; files with examples run together in one
/// package module (one shared JIT module) when their top-level names are
/// disjoint, else each in its own module. The run fails after all files if
/// any file failed, and fails if no file in the package has examples.
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
    // (display name, Some(source) with examples | None without, or the read error).
    let mut entries: Vec<(String, Result<Option<String>, String>)> = Vec::new();
    for path in &package.files {
        let name = package.display(path);
        let entry = read_source(path, limits).and_then(|source| {
            doctest::has_examples(&source, limits).map(|has| has.then_some(source))
        });
        entries.push((name, entry));
    }
    let with_examples: Vec<(String, String)> = entries
        .iter()
        .filter_map(|(name, e)| match e {
            Ok(Some(source)) => Some((name.clone(), source.clone())),
            _ => None,
        })
        .collect();
    let mut package_report = doctest::run_package(&with_examples, limits);
    let mut files = Vec::new();
    let (mut tested, mut failed, mut skipped) = (0usize, 0usize, 0usize);
    let mut results = std::mem::take(&mut package_report.files).into_iter();
    for (name, entry) in &entries {
        match entry {
            Ok(None) => {
                skipped += 1;
                if !json {
                    println!("{name}: no doc examples");
                }
                files.push(serde_json::json!({"file":name,"examples":false}));
            }
            Ok(Some(_)) => {
                tested += 1;
                let result = results.next().ok_or("missing package doctest result")?;
                if !result.passed {
                    failed += 1;
                }
                match (&result.report, &result.error) {
                    (Some(report), _) => {
                        if !json {
                            print_report(name, report);
                        }
                        files.push(serde_json::json!({"file":name,"examples":true,"passed":result.passed,"report":report}));
                    }
                    (None, error) => {
                        let error = error.clone().unwrap_or_default();
                        if !json {
                            println!("{name}: FAIL: {error}");
                        }
                        files.push(serde_json::json!({"file":name,"examples":true,"passed":false,"error":error}));
                    }
                }
            }
            Err(error) => {
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
                "package_module":package_report.package_module,
                "package_fallback":package_report.package_fallback,
                "batch":package_report.batch,
                "coverage_probes":package_report.coverage_probes,
                "jit_examples":package_report.jit_examples,
                "evaluator_examples":package_report.evaluator_examples,
                "error":error,
            })
        );
    } else {
        match (&package_report.batch, &package_report.package_fallback) {
            (Some(batch), _) if package_report.package_module => println!(
                "package module: {} entry point(s) in one shared JIT module ({} probe module(s))",
                batch.entry_count, package_report.coverage_probes
            ),
            (_, Some(reason)) => println!("package module not used: {reason}; one module per file"),
            _ => {}
        }
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
