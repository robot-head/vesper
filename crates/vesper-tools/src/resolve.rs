//! `vesper resolve`: offline Loom resolution and locked validation (moved
//! from the compiler's `package_commands`, self-hosting SH0 lane 2).
use std::{fs, path::PathBuf};
use vesper::atomic_file::atomic_write;

pub fn resolve(args: Vec<String>) -> Result<(), String> {
    use std::collections::BTreeSet;
    let mut args = args.into_iter();
    let mut manifest_path = None;
    let mut catalog_path = None;
    let mut output = None;
    let mut locked = false;
    let mut lockfile = None;
    let mut baseline = None;
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
            "--baseline" => {
                if baseline.is_some() {
                    return Err("--baseline may only be specified once".into());
                }
                baseline = Some(args.next().ok_or("--baseline requires a snapshot path")?);
            }
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
                    "Usage: vesper resolve [Loom.toml] --catalog <catalog.json> [--features a,b] [--json]\n  Fresh resolution: [--max-decisions N] [--baseline old.lock] [-o Loom.lock]\n  Read-only validation: --locked [--lockfile Loom.lock]"
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
    if locked && baseline.is_some() {
        return Err(
            "--baseline compares a fresh resolution and cannot be combined with --locked".into(),
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
    let baseline = if let Some(path) = baseline {
        let bytes = fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
        let snapshot: vesper_loom_calculus::Solution = serde_json::from_slice(&bytes)
            .map_err(|e| format!("{path}: invalid baseline snapshot: {e}"))?;
        let validation = vesper_loom_calculus::validate_solution(&snapshot);
        if !validation.diagnostics.is_empty()
            || validation.graph.as_ref().is_none_or(|g| g.roots.is_empty())
        {
            if json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "status":"error","error":{"kind":"invalid_baseline","diagnostics":validation.diagnostics}
                })).map_err(|e| e.to_string())?);
            }
            return Err(format!(
                "{path}: baseline must have a valid nonempty package graph"
            ));
        }
        Some(snapshot)
    } else {
        None
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
    let upgrade = baseline
        .as_ref()
        .map(|old| vesper_loom_calculus::check_upgrade(old, &solution));
    if let Some(report) = &upgrade
        && !report.allowed
    {
        let kind = if report.diagnostics.iter().any(|d| {
            matches!(
                d.code.as_str(),
                "capability_escalation" | "unsafe_ffi_policy_relaxed"
            )
        }) {
            "capability_escalation"
        } else {
            "upgrade_mismatch"
        };
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "status":"error","error":{"kind":kind},"upgrade":report
                }))
                .map_err(|e| e.to_string())?
            );
        }
        let messages = report
            .diagnostics
            .iter()
            .map(|diagnostic| {
                let package = diagnostic
                    .package
                    .as_ref()
                    .map(|p| p.as_str())
                    .unwrap_or("snapshot");
                format!("{package}: {}", diagnostic.message)
            })
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!(
            "declared-authority upgrade guard rejected candidate: {messages}"
        ));
    }
    let bytes = serde_json::to_vec_pretty(&solution).map_err(|e| e.to_string())?;
    if let Some(path) = output {
        atomic_write(&path, &bytes)?;
    }
    if let Some(upgrade) = upgrade {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "status":"resolved","solution":solution,"upgrade":upgrade
                }))
                .map_err(|e| e.to_string())?
            );
            return Ok(());
        }
        println!(
            "declared-authority guard passed ({} change(s))",
            upgrade.changes.len()
        );
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
