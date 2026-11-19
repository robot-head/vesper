//! Read-only policy audit of a declared dependency snapshot and a local catalog.
use std::{fs::File, io::Read};
use vesper_loom_audit::{Limits, Outcome, audit, parse_catalog, parse_policy, parse_solution};

const MAX_INPUT_BYTES: u64 = 1024 * 1024;
const HELP: &str = "Usage: vesper audit [Loom.lock] --catalog advisories.json --policy audit-policy.json [--json]\n\nEmit a versioned JSON audit of the validated reachable dependency graph.\nOnly a clean result exits successfully; findings and unknown coverage fail closed.\nThe advisory catalog and policy are local caller-selected inputs, not an authenticated live feed.\nThis checks declared metadata, not actual installed binaries or source provenance.\nNo package code runs and no files are written. Each input is limited to 1 MiB.\n";

fn input(path: &str) -> Result<Vec<u8>, String> {
    let mut bytes = vec![];
    File::open(path)
        .map_err(|e| format!("{path}: {e}"))?
        .take(MAX_INPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{path}: {e}"))?;
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return Err(format!("{path}: audit input exceeds the 1 MiB limit"));
    }
    Ok(bytes)
}

fn execute(args: Vec<String>) -> Result<Option<vesper_loom_audit::AuditReport>, String> {
    let mut snapshot = None;
    let mut catalog = None;
    let mut policy = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{HELP}");
                return Ok(None);
            }
            "--catalog" | "--policy" => {
                let path = args
                    .next()
                    .ok_or_else(|| format!("{arg} requires a path"))?;
                let slot = if arg == "--catalog" {
                    &mut catalog
                } else {
                    &mut policy
                };
                if slot.replace(path).is_some() {
                    return Err(format!("{arg} may only be specified once"));
                }
            }
            "--json" => {}
            _ if arg.starts_with('-') => return Err(format!("unknown audit option '{arg}'")),
            _ => {
                if snapshot.replace(arg).is_some() {
                    return Err("audit accepts one solution snapshot".into());
                }
            }
        }
    }
    let catalog = catalog.ok_or("audit requires --catalog")?;
    let policy = policy.ok_or("audit requires --policy")?;
    let limits = Limits::default();
    let solution = parse_solution(&input(snapshot.as_deref().unwrap_or("Loom.lock"))?, &limits)
        .map_err(|e| format!("invalid snapshot: {e}"))?;
    let catalog = parse_catalog(&input(&catalog)?, &limits)
        .map_err(|e| format!("invalid advisory catalog: {e}"))?;
    let policy = parse_policy(&input(&policy)?, &limits)
        .map_err(|e| format!("invalid audit policy: {e}"))?;
    audit(&solution, &catalog, &policy, &limits)
        .map(Some)
        .map_err(|e| e.to_string())
}

pub fn run(args: Vec<String>) -> Result<(), String> {
    let report = match execute(args) {
        Ok(Some(report)) => report,
        Ok(None) => return Ok(()),
        Err(message) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "schema_version": 1, "status": "error",
                    "error": { "kind": "audit_failed", "message": message }
                }))
                .map_err(|e| e.to_string())?
            );
            return Err(message);
        }
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
    );
    match report.outcome {
        Outcome::Clean => Ok(()),
        Outcome::Findings => Err("dependency audit found policy violations".into()),
        Outcome::Unknown => Err("dependency audit has unknown coverage".into()),
    }
}
