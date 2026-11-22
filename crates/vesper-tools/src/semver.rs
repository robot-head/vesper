//! `vesper semver`: compare two versioned interface files (moved out of the
//! compiler binary by SH6 lane 3, #1398). `vesper resolve` is `vesper-resolve`.
use std::fs;
use vesper_semver::{Interface, Report};

pub fn run(args: Vec<String>) -> Result<(), String> {
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
