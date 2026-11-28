//! `vesper publish`'s Lean proof gate over the shipped compiler's
//! `vesper proof-audit --json` (#2670, part of #1397).
//!
//! The gate used to call `proof_commands::audit_policy` in the compiler
//! library. It now checks the same policy here and runs the audit as a
//! process. The checks and their messages are the library's.
use crate::compiler;
use serde_json::Value;
use std::{path::Path, process::Command};

/// The compiler command that kernel-replays Lean artifacts.
const PROOF_AUDIT_COMMAND: &str = "proof-audit";
/// The longest qualified Lean name accepted, in bytes.
const MAX_NAME_BYTES: usize = 1024;
/// The most theorems one audit checks.
const MAX_THEOREMS: usize = 128;
/// The longest Lean run allowed, in seconds.
const MAX_TIMEOUT_SECONDS: u64 = 300;
/// The `status` of an audit that could not run.
const ERROR_STATUS: &str = "error";

/// A qualified ASCII Lean name.
fn name_valid(name: &str) -> bool {
    name.len() <= MAX_NAME_BYTES
        && name.split('.').all(|part| {
            !part.is_empty()
                && (part.as_bytes()[0].is_ascii_alphabetic() || part.starts_with('_'))
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'\'')
        })
}

/// These axioms bypass kernel justification of the claimed result. Explicit
/// policies may admit mathematical assumptions, but never proof/compiler escapes.
pub fn forbidden_axiom(name: &str) -> bool {
    matches!(name, "sorryAx" | "Lean.ofReduceBool" | "Lean.trustCompiler")
        // Lean 4.35's Meta.nativeEqTrue creates per-theorem axioms under
        // `_native ++ tacticName ++ ax`; see Lean/Meta/Native.lean.
        || name.split('.').any(|part| part == "_native")
}

/// Replay local Lean artifacts under an explicit allowed-axiom whitelist,
/// always rejecting proof/compiler escape axioms. Returns whether the
/// release theorems are accepted, and the audit report.
pub fn audit_policy(
    root: &Path,
    module: &str,
    theorems: &[String],
    allowed_axioms: &[String],
    lean: Option<&str>,
    timeout: Option<u64>,
) -> Result<(bool, Value), String> {
    if !name_valid(module)
        || matches!(
            module.split('.').next(),
            Some("Lean" | "Std" | "Init" | "Lake")
        )
    {
        return Err(
            "module must be a qualified ASCII identifier outside trusted toolchain namespaces"
                .into(),
        );
    }
    if theorems.is_empty()
        || theorems.len() > MAX_THEOREMS
        || theorems.iter().any(|n| !name_valid(n))
    {
        return Err("invalid or empty theorem list for proof gate (1..=128 theorems)".into());
    }
    for ax in allowed_axioms {
        if forbidden_axiom(ax) {
            return Err(format!(
                "{ax} cannot be allowed by the publish proof policy"
            ));
        }
        if !name_valid(ax) {
            return Err(format!("invalid Lean axiom name '{ax}'"));
        }
    }
    if let Some(secs) = timeout
        && !(1..=MAX_TIMEOUT_SECONDS).contains(&secs)
    {
        return Err("--timeout must be between 1 and 300 seconds".into());
    }
    let vesper = compiler::locate()?;
    let mut command = Command::new(&vesper);
    command
        .arg(PROOF_AUDIT_COMMAND)
        .arg(module)
        .arg("--root")
        .arg(root)
        .arg("--json");
    for theorem in theorems {
        command.arg("--theorem").arg(theorem);
    }
    for axiom in allowed_axioms {
        command.arg("--allow-axiom").arg(axiom);
    }
    if let Some(lean) = lean {
        command.arg("--lean").arg(lean);
    }
    if let Some(secs) = timeout {
        command.arg("--timeout").arg(secs.to_string());
    }
    // A rejected audit exits 1 with its report; an audit that could not run
    // exits 1 with an error document. Both are on stdout.
    let output = command
        .output()
        .map_err(|e| format!("{}: {e}", vesper.display()))?;
    let report: Value = serde_json::from_slice(&output.stdout).map_err(|_| {
        format!(
            "{} {PROOF_AUDIT_COMMAND} failed: {}",
            vesper.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )
    })?;
    if report.get("status").and_then(Value::as_str) == Some(ERROR_STATUS) {
        return Err(report
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned());
    }
    let flag = |field: &str| report.get(field).and_then(Value::as_bool) == Some(true);
    let no_forbidden = report
        .get("forbidden_axioms")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty);
    let accepted = flag("accepted") && flag("kernel_replayed") && no_forbidden;
    Ok((accepted, report))
}
