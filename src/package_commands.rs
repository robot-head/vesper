//! CLI adapters for the versioned interface and offline Loom libraries.
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use vesper_semver::{Interface, Report};

pub fn semver(args: Vec<String>) -> Result<(), String> {
    let mut paths = vec![];
    let mut json = false;
    let mut old_version = None;
    let mut new_version = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--old-version" => {
                if old_version.is_some() {
                    return Err("--old-version may only be specified once".into());
                }
                old_version = Some(args.next().ok_or("--old-version requires a version")?);
            }
            "--new-version" => {
                if new_version.is_some() {
                    return Err("--new-version may only be specified once".into());
                }
                new_version = Some(args.next().ok_or("--new-version requires a version")?);
            }
            "--help" | "-h" => {
                println!(
                    "Usage: vesper semver <old.vhir> <new.vhir> [--json] [--old-version X --new-version Y]"
                );
                return Ok(());
            }
            _ if arg.starts_with('-') => return Err(format!("unknown semver option '{arg}'")),
            _ => paths.push(arg),
        }
    }
    if paths.len() != 2 {
        return Err("semver requires baseline and candidate interface files".into());
    }
    if old_version.is_some() != new_version.is_some() {
        return Err("--old-version and --new-version must be specified together".into());
    }
    let read = |path: &str| -> Result<Interface, String> {
        let bytes = fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        serde_json::from_slice(&bytes).map_err(|e| format!("{path}: invalid interface: {e}"))
    };
    let old = read(&paths[0])?;
    let new = read(&paths[1])?;
    if let (Some(old_version), Some(new_version)) = (old_version, new_version) {
        let report = vesper_semver::check_release(&old, &new, &old_version, &new_version)
            .map_err(|e| e.to_string())?;
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
            );
        } else {
            print_report(&report.comparison);
            println!(
                "release {} -> {}: {}",
                report.old_version,
                report.new_version,
                if report.accepted {
                    "accepted"
                } else {
                    "rejected"
                }
            );
            for issue in &report.issues {
                println!("  {}", issue.message);
            }
        }
        return if report.accepted {
            Ok(())
        } else {
            Err("release version does not satisfy the interface/version policy".into())
        };
    }
    let report = vesper_semver::compare(&old, &new);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else {
        print_report(&report);
    }
    if report.is_compatible() {
        Ok(())
    } else {
        Err("candidate interface is breaking or cannot be certified compatible".into())
    }
}
fn print_report(report: &Report) {
    println!("{}", format!("{:?}", report.classification).to_lowercase());
    for issue in &report.issues {
        println!("{} [{}]: {}", issue.path, issue.rule, issue.message);
        if !issue.caller_witness.is_empty() {
            println!("  caller: {}", issue.caller_witness);
        }
    }
    for adapter in &report.required_adapters {
        println!(
            "{} parameter {} requires an explicit reborrow adapter",
            adapter.function_path, adapter.parameter
        );
    }
}

/// Write a new lockfile without truncating an existing one on resolution or
/// write failure. Temporary files are created exclusively beside the target.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    path.file_name().ok_or("output path must name a file")?;
    let mut temp = PathBuf::from(parent);
    // Do not append a suffix to the destination leaf: a valid NAME_MAX-sized
    // output name must still leave room for the private staging file.
    let temporary_name = format!(
        ".vesper-tmp-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    temp.push(temporary_name);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|e| format!("{}: {e}", temp.display()))?;
    let written = file.write_all(bytes).and_then(|_| file.sync_all());
    drop(file);
    let result = written.and_then(|_| fs::rename(&temp, path));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(|e| format!("{}: {e}", path.display()))
}

pub fn resolve(args: Vec<String>) -> Result<(), String> {
    use std::collections::BTreeSet;
    let mut args = args.into_iter();
    let mut manifest_path = None;
    let mut catalog_path = None;
    let mut output = None;
    let mut locked = false;
    let mut lockfile = None;
    let mut features = BTreeSet::new();
    let mut max_decisions = 100_000usize;
    let mut explicit_budget = false;
    let mut json = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--catalog" => {
                catalog_path = Some(
                    args.next()
                        .ok_or("--catalog requires a JSON catalog path")?,
                )
            }
            "-o" => {
                output = Some(PathBuf::from(
                    args.next().ok_or("-o requires a lockfile path")?,
                ))
            }
            "--locked" => locked = true,
            "--lockfile" => {
                if lockfile.is_some() {
                    return Err("--lockfile may only be specified once".into());
                }
                lockfile = Some(PathBuf::from(
                    args.next().ok_or("--lockfile requires a path")?,
                ));
            }
            "--features" => {
                let requested = args
                    .next()
                    .ok_or("--features requires comma-separated feature names")?;
                for name in requested.split(',') {
                    if name.is_empty() {
                        return Err("--features contains an empty feature name".into());
                    }
                    features.insert(name.to_string());
                }
            }
            "--max-decisions" => {
                explicit_budget = true;
                max_decisions = args
                    .next()
                    .ok_or("--max-decisions requires a positive integer")?
                    .parse()
                    .map_err(|_| "invalid --max-decisions value")?;
                if max_decisions == 0 {
                    return Err("--max-decisions must be positive".into());
                }
            }
            "--json" => json = true,
            "--help" | "-h" => {
                println!(
                    "Usage: vesper resolve [Loom.toml] --catalog <catalog.json> [--features a,b] [--json]\n  Fresh resolution: [--max-decisions N] [-o Loom.lock]\n  Read-only validation: --locked [--lockfile Loom.lock]"
                );
                return Ok(());
            }
            _ if arg.starts_with('-') => return Err(format!("unknown resolve option '{arg}'")),
            _ => {
                if manifest_path.replace(arg).is_some() {
                    return Err("resolve accepts one root manifest".into());
                }
            }
        }
    }
    if !locked && lockfile.is_some() {
        return Err("--lockfile requires --locked; use -o for fresh resolution".into());
    }
    if locked && output.is_some() {
        return Err(
            "--locked cannot be combined with -o; use --lockfile for the input snapshot".into(),
        );
    }
    if locked && explicit_budget {
        return Err(
            "--max-decisions applies only to fresh resolution; --locked performs no search".into(),
        );
    }
    let manifest_path = manifest_path.unwrap_or_else(|| "Loom.toml".into());
    let catalog_path =
        catalog_path.ok_or("offline resolution requires --catalog <catalog.json>")?;
    let source = fs::read_to_string(&manifest_path).map_err(|e| format!("{manifest_path}: {e}"))?;
    let root = vesper_loom_calculus::parse_manifest(&source).map_err(|errors| {
        format!(
            "{manifest_path}: invalid manifest: {}",
            serde_json::to_string(&errors).unwrap_or_else(|_| "invalid manifest".into())
        )
    })?;
    let catalog_bytes = fs::read(&catalog_path).map_err(|e| format!("{catalog_path}: {e}"))?;
    let catalog = serde_json::from_slice(&catalog_bytes)
        .map_err(|e| format!("{catalog_path}: invalid catalog: {e}"))?;
    let request = vesper_loom_solver::ResolveRequest {
        root,
        features,
        catalog,
        max_decisions,
    };
    if locked {
        let path = lockfile.unwrap_or_else(|| PathBuf::from("Loom.lock"));
        let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let solution = serde_json::from_slice(&bytes)
            .map_err(|e| format!("{}: invalid lockfile: {e}", path.display()))?;
        if let Err(error) = vesper_loom_solver::validate_locked(&request, &solution) {
            return resolution_error(error, json, "lock validation");
        }
        return print_solution(&solution, json, "locked");
    }
    let solution = match vesper_loom_solver::resolve(&request) {
        Ok(solution) => solution,
        Err(error) => return resolution_error(error, json, "resolution"),
    };
    let bytes = serde_json::to_vec_pretty(&solution).map_err(|e| e.to_string())?;
    if let Some(path) = output {
        atomic_write(&path, &bytes)?;
    }
    print_solution(&solution, json, "resolved")
}

fn resolution_error(
    error: vesper_loom_solver::ResolveError,
    json: bool,
    action: &str,
) -> Result<(), String> {
    let error = serde_json::to_value(error).map_err(|e| e.to_string())?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({"status":"error","error":error}))
                .map_err(|e| e.to_string())?
        );
    }
    Err(format!("offline {action} failed: {error}"))
}

fn print_solution(
    solution: &vesper_loom_calculus::Solution,
    json: bool,
    status: &str,
) -> Result<(), String> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({"status":status,"solution":solution}))
                .map_err(|e| e.to_string())?
        );
    } else {
        println!(
            "{status} {} package(s), {} native package(s)",
            solution.packages.len(),
            solution.system_packages.len()
        );
        for selected in &solution.packages {
            println!(
                "{} {}",
                selected.manifest.package.name.as_str(),
                selected.manifest.package.version
            );
        }
    }
    Ok(())
}
