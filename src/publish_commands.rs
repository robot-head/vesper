//! Offline package publication gate and local CAS release pipeline (§10.4).
//!
//! Validates a package manifest (`Loom.toml`), checks the source module, exports
//! its `.vhir` interface fingerprint (`mif:blake3:...`), verifies contextual SemVer
//! substitutability against a baseline (or `--initial`), optionally audits a
//! resolved `Loom.lock` graph (CycloneDX 1.6 SBOM + OSV advisory catalog),
//! kernel-replays local Lean `.olean` proofs (`0 sorryAx`), verifies an Ed25519
//! builder operator quorum, and only then ingests the release into the local CAS
//! (`--store`) and writes a deterministic publication receipt (`-o`).
use serde_json::{Value, json};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use vesper::{check, parser};
use vesper_loom_audit::{
    Outcome, audit, parse_catalog, parse_policy as parse_audit_policy, parse_solution,
};
use vesper_loom_calculus::{parse_manifest, validate_manifest, validate_solution};
use vesper_loom_cas::{Domain, ObjectId, ObjectKind, Store, blob_id};
use vesper_loom_trust::{parse_bundle, parse_identity, parse_policy, verify_attestations};

const MAX_SMALL_JSON_BYTES: u64 = 1024 * 1024;
const MAX_SOURCE_BYTES: u64 = 4 * 1024 * 1024;

const HELP: &str = "Usage:
  vesper publish <SOURCE.ves> (--initial | --baseline <old.vhir> --old-version <semver>) [options]

Required release mode (choose one):
  --initial                    First publication of this package version (skips baseline diff)
  --baseline <old.vhir>        Baseline .vhir interface JSON to compare via vesper semver
  --old-version <semver>       Baseline release SemVer version (required with --baseline)

Options:
  --manifest <Loom.toml>       Package manifest (default: Loom.toml)
  --solution <Loom.lock>       Validated Loom snapshot; enables CycloneDX 1.6 SBOM generation
  --advisories <osv.json>      Local OSV advisory catalog (requires --solution & --audit-policy)
  --audit-policy <policy.json> Local audit policy JSON (requires --solution & --advisories)
  --olean-root <dir>           Local Lean artifact directory (.olean/.ir) for kernel replay
  --olean-module <Module>      Qualified Lean module name (required with --olean-root)
  --theorem <Qualified.name>   Theorem name to kernel-audit (repeatable, required with --olean-root)
  --allow-axiom <Name>         Additional Lean axiom allowed besides empty default (never sorryAx)
  --lean <path>                Lean launcher path for kernel replay (or VESPER_LEAN)
  --timeout <seconds>          Lean kernel-replay timeout (1..=300 seconds)
  --expected <identity.json>   Expected build identity JSON for builder quorum gate
  --policy <builders.json>     Builder policy JSON (required with --expected/--bundle)
  --bundle <attestations.json> Ed25519 attestation bundle JSON (required with --expected/--policy)
  --artifact <file>            Optional built artifact binary whose BLAKE3 blob ID must match expected.output
  --at <unix_seconds>          Evaluation timestamp for attestation quorum (default: host UTC clock)
  --store <cas_dir>            Local CAS directory to ingest verified source and interface blobs
  -o <receipt.json>            Write deterministic JSON publication receipt atomically
  --json                       Emit structured JSON receipt/diagnostics on stdout
  -h, --help                   Show this help text
";

fn read_bounded(path: &str, max_bytes: u64, label: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| format!("{path}: {e}"))?
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{path}: {e}"))?;
    if bytes.len() as u64 > max_bytes {
        return Err(format!("{path}: {label} exceeds {max_bytes} byte limit"));
    }
    Ok(bytes)
}

fn read_utf8_bounded(path: &str, max_bytes: u64, label: &str) -> Result<String, String> {
    let bytes = read_bounded(path, max_bytes, label)?;
    String::from_utf8(bytes).map_err(|e| format!("{path}: invalid UTF-8: {e}"))
}

fn fail(json_out: bool, kind: &str, message: String, extra: Option<Value>) -> Result<(), String> {
    if json_out {
        let mut obj = json!({
            "schema_version": 1,
            "status": "rejected",
            "error": {
                "kind": kind,
                "message": message,
            }
        });
        if let Some(details) = extra {
            obj["details"] = details;
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&obj).map_err(|e| e.to_string())?
        );
    }
    Err(message)
}

fn format_effect_capability(effect: &vesper::ast::Effect) -> String {
    if effect.args.is_empty() {
        effect.name.clone()
    } else {
        let args = effect
            .args
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        format!("{}[{args}]", effect.name)
    }
}

fn effect_permitted_by_ceiling(
    effect: &vesper::ast::Effect,
    ceiling: &std::collections::BTreeSet<String>,
) -> bool {
    let full = format_effect_capability(effect);
    ceiling.contains(&full) || ceiling.contains(&effect.name)
}

fn is_foreign_effect_name(name: &str) -> bool {
    let base = name.split('[').next().unwrap_or(name);
    matches!(base, "Ffi" | "UnsafeFfi" | "UnsafeFFI")
}

fn published_source_tree_id(
    source_path: &str,
    source_id: ObjectId,
    size: u64,
) -> Result<ObjectId, String> {
    let file_name = Path::new(source_path)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("{source_path}: source path must have a UTF-8 file name"))?;
    let executable = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(source_path)
                .map(|m| m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        }
        #[cfg(not(unix))]
        {
            false
        }
    };
    let mut tree = vesper_loom_cas::Tree::new(Domain::Source);
    tree.insert_file(file_name, source_id, size, executable)
        .map_err(|e| format!("{source_path}: {e}"))?;
    Ok(tree.id())
}

#[derive(Default)]
struct Options {
    source: Option<String>,
    manifest: Option<String>,
    initial: bool,
    baseline: Option<String>,
    old_version: Option<String>,
    solution: Option<String>,
    advisories: Option<String>,
    audit_policy: Option<String>,
    olean_root: Option<PathBuf>,
    olean_module: Option<String>,
    theorems: Vec<String>,
    allowed_axioms: Vec<String>,
    lean: Option<String>,
    timeout: Option<u64>,
    expected: Option<String>,
    policy: Option<String>,
    bundle: Option<String>,
    artifact: Option<String>,
    at: Option<u64>,
    store: Option<PathBuf>,
    output: Option<PathBuf>,
    json: bool,
}

fn set_once(slot: &mut Option<String>, value: String, flag: &str) -> Result<(), String> {
    if slot.replace(value).is_some() {
        return Err(format!("{flag} may only be specified once"));
    }
    Ok(())
}

pub fn run(args: Vec<String>) -> Result<(), String> {
    let mut opts = Options::default();
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(());
            }
            "--initial" => opts.initial = true,
            "--json" => opts.json = true,
            "--manifest" => {
                let v = it.next().ok_or("--manifest requires a Loom.toml path")?;
                set_once(&mut opts.manifest, v, "--manifest")?;
            }
            "--baseline" => {
                let v = it.next().ok_or("--baseline requires a .vhir path")?;
                set_once(&mut opts.baseline, v, "--baseline")?;
            }
            "--old-version" => {
                let v = it.next().ok_or("--old-version requires a SemVer string")?;
                set_once(&mut opts.old_version, v, "--old-version")?;
            }
            "--solution" => {
                let v = it.next().ok_or("--solution requires a Loom.lock path")?;
                set_once(&mut opts.solution, v, "--solution")?;
            }
            "--advisories" => {
                let v = it.next().ok_or("--advisories requires a catalog path")?;
                set_once(&mut opts.advisories, v, "--advisories")?;
            }
            "--audit-policy" => {
                let v = it.next().ok_or("--audit-policy requires a policy path")?;
                set_once(&mut opts.audit_policy, v, "--audit-policy")?;
            }
            "--olean-root" => {
                if opts.olean_root.is_some() {
                    return Err("--olean-root may only be specified once".into());
                }
                let v = it.next().ok_or("--olean-root requires a directory")?;
                opts.olean_root = Some(PathBuf::from(v));
            }
            "--olean-module" => {
                let v = it.next().ok_or("--olean-module requires a Lean module")?;
                set_once(&mut opts.olean_module, v, "--olean-module")?;
            }
            "--theorem" => {
                let v = it.next().ok_or("--theorem requires a qualified name")?;
                opts.theorems.push(v);
            }
            "--allow-axiom" => {
                let v = it.next().ok_or("--allow-axiom requires an axiom name")?;
                if v == "sorryAx" {
                    return Err("sorryAx cannot be allowed by publish proof policy".into());
                }
                opts.allowed_axioms.push(v);
            }
            "--lean" => {
                let v = it.next().ok_or("--lean requires an executable path")?;
                set_once(&mut opts.lean, v, "--lean")?;
            }
            "--timeout" => {
                if opts.timeout.is_some() {
                    return Err("--timeout may only be specified once".into());
                }
                let raw = it.next().ok_or("--timeout requires seconds")?;
                let secs = raw
                    .parse::<u64>()
                    .map_err(|_| "invalid --timeout; expected 1..=300".to_string())?;
                if !(1..=300).contains(&secs) {
                    return Err("--timeout must be between 1 and 300 seconds".into());
                }
                opts.timeout = Some(secs);
            }
            "--expected" => {
                let v = it
                    .next()
                    .ok_or("--expected requires an identity JSON path")?;
                set_once(&mut opts.expected, v, "--expected")?;
            }
            "--policy" => {
                let v = it
                    .next()
                    .ok_or("--policy requires a builder policy JSON path")?;
                set_once(&mut opts.policy, v, "--policy")?;
            }
            "--bundle" => {
                let v = it
                    .next()
                    .ok_or("--bundle requires an attestation bundle path")?;
                set_once(&mut opts.bundle, v, "--bundle")?;
            }
            "--artifact" => {
                let v = it
                    .next()
                    .ok_or("--artifact requires an artifact file path")?;
                set_once(&mut opts.artifact, v, "--artifact")?;
            }
            "--at" => {
                if opts.at.is_some() {
                    return Err("--at may only be specified once".into());
                }
                let raw = it.next().ok_or("--at requires Unix seconds")?;
                let secs = raw
                    .parse::<u64>()
                    .map_err(|_| "invalid --at; expected unsigned Unix seconds".to_string())?;
                opts.at = Some(secs);
            }
            "--store" => {
                if opts.store.is_some() {
                    return Err("--store may only be specified once".into());
                }
                opts.store = Some(PathBuf::from(
                    it.next().ok_or("--store requires a CAS directory")?,
                ));
            }
            "-o" => {
                if opts.output.is_some() {
                    return Err("-o may only be specified once".into());
                }
                opts.output = Some(PathBuf::from(
                    it.next().ok_or("-o requires a receipt output path")?,
                ));
            }
            _ if arg.starts_with('-') => return Err(format!("unknown publish option '{arg}'")),
            _ => {
                if opts.source.replace(arg).is_some() {
                    return Err("publish accepts one entry .ves source file".into());
                }
            }
        }
    }

    let source_path = opts
        .source
        .as_deref()
        .ok_or("publish requires a .ves entry source file; see publish --help")?;
    if opts.initial && (opts.baseline.is_some() || opts.old_version.is_some()) {
        return Err("--initial cannot be combined with --baseline or --old-version".into());
    }
    if !opts.initial && (opts.baseline.is_none() || opts.old_version.is_none()) {
        return Err(
            "publish requires either --initial or both --baseline and --old-version".into(),
        );
    }
    if opts.advisories.is_some() != opts.audit_policy.is_some() {
        return Err("--advisories and --audit-policy must be supplied together".into());
    }
    if opts.advisories.is_some() && opts.solution.is_none() {
        return Err("--advisories/--audit-policy require --solution <Loom.lock>".into());
    }
    let has_proof_flags =
        opts.olean_root.is_some() || opts.olean_module.is_some() || !opts.theorems.is_empty();
    if has_proof_flags
        && (opts.olean_root.is_none() || opts.olean_module.is_none() || opts.theorems.is_empty())
    {
        return Err(
            "Lean proof gate requires --olean-root, --olean-module, and at least one --theorem"
                .into(),
        );
    }
    let attest_count = [
        opts.expected.is_some(),
        opts.policy.is_some(),
        opts.bundle.is_some(),
    ]
    .into_iter()
    .filter(|&b| b)
    .count();
    if attest_count != 0 && attest_count != 3 {
        return Err(
            "attestation quorum gate requires --expected, --policy, and --bundle together".into(),
        );
    }
    if (opts.artifact.is_some() || opts.at.is_some()) && attest_count == 0 {
        return Err("--artifact and --at apply only with --expected/--policy/--bundle".into());
    }

    let manifest_path = opts.manifest.as_deref().unwrap_or("Loom.toml");
    let manifest_src = read_utf8_bounded(manifest_path, MAX_SMALL_JSON_BYTES, "manifest")?;
    let manifest = parse_manifest(&manifest_src).map_err(|errors| {
        format!(
            "{manifest_path}: invalid manifest: {}",
            serde_json::to_string(&errors).unwrap_or_else(|_| "invalid manifest".into())
        )
    })?;
    let manifest_diags = validate_manifest(&manifest);
    if !manifest_diags.is_empty() {
        return fail(
            opts.json,
            "manifest_invalid",
            format!("{manifest_path}: manifest validation failed"),
            Some(serde_json::to_value(&manifest_diags).map_err(|e| e.to_string())?),
        );
    }

    let source = read_utf8_bounded(source_path, MAX_SOURCE_BYTES, "source")?;
    let parsed = parser::parse(&source);
    if !parsed.diagnostics.is_empty() {
        return fail(
            opts.json,
            "syntax_error",
            format!(
                "{source_path}: {} syntax error(s)",
                parsed.diagnostics.len()
            ),
            Some(json!({"count": parsed.diagnostics.len()})),
        );
    }
    let checked = check::check(&parsed.module);
    if !checked.diagnostics.is_empty() {
        return fail(
            opts.json,
            "typecheck_error",
            format!(
                "{source_path}: {} semantic error(s)",
                checked.diagnostics.len()
            ),
            Some(json!({"count": checked.diagnostics.len()})),
        );
    }
    if manifest.package.forbid_unsafe_ffi {
        let mut bad_ffi = Vec::new();
        for cap in &manifest.package.capability_ceiling {
            if is_foreign_effect_name(cap) {
                bad_ffi.push(format!("manifest capability_ceiling ({cap})"));
            }
        }
        for (fn_name, func) in &checked.world.functions {
            for inst in &func.effects.instances {
                if is_foreign_effect_name(&inst.name) {
                    bad_ffi.push(format!("{fn_name} ({})", format_effect_capability(inst)));
                }
            }
        }
        if !bad_ffi.is_empty() {
            return fail(
                opts.json,
                "capability_violation",
                format!(
                    "{}: forbid_unsafe_ffi = true blocks foreign authority: {}",
                    manifest.package.name.as_str(),
                    bad_ffi.join(", ")
                ),
                Some(json!({ "violations": bad_ffi })),
            );
        }
    }

    let mut ceiling_violations = Vec::new();
    for (fn_name, func) in &checked.world.functions {
        for inst in &func.effects.instances {
            if !effect_permitted_by_ceiling(inst, &manifest.package.capability_ceiling) {
                ceiling_violations.push(format!("{fn_name} ({})", format_effect_capability(inst)));
            }
        }
    }
    if !ceiling_violations.is_empty() {
        return fail(
            opts.json,
            "capability_violation",
            format!(
                "{}: checked source exceeds package capability_ceiling: {}",
                manifest.package.name.as_str(),
                ceiling_violations.join(", ")
            ),
            Some(json!({ "violations": ceiling_violations })),
        );
    }

    let interface = vesper::interface::export(&checked.world, manifest.package.name.as_str())?;
    let interface_bytes = serde_json::to_vec_pretty(&interface).map_err(|e| e.to_string())?;
    let source_id = blob_id(Domain::Source, source.as_bytes());
    let interface_id = blob_id(Domain::Interface, &interface_bytes);
    let interface_mif = format!("mif:blake3:{}", interface_id.digest.to_hex());

    let pkg_version = manifest.package.version.to_string();
    let semver_gate = if opts.initial {
        json!({
            "mode": "initial",
            "accepted": true,
            "version": pkg_version,
        })
    } else {
        let baseline_path = opts.baseline.as_deref().expect("validated");
        let old_version = opts.old_version.as_deref().expect("validated");
        let old_bytes = read_bounded(baseline_path, MAX_SOURCE_BYTES, "baseline interface")?;
        let old_iface: vesper_semver::Interface = serde_json::from_slice(&old_bytes)
            .map_err(|e| format!("{baseline_path}: invalid baseline .vhir interface: {e}"))?;
        let report =
            vesper_semver::check_release(&old_iface, &interface, old_version, &pkg_version)
                .map_err(|e| e.to_string())?;
        if !report.accepted {
            let witnesses: Vec<String> = report
                .comparison
                .issues
                .iter()
                .filter(|i| !i.caller_witness.is_empty())
                .map(|i| i.caller_witness.clone())
                .collect();
            return fail(
                opts.json,
                "semver_rejected",
                format!(
                    "SemVer release gate rejected {} -> {} (required {:?} bump)",
                    report.old_version, report.new_version, report.required_bump
                ),
                Some(json!({
                    "release_report": report,
                    "caller_witnesses": witnesses,
                })),
            );
        }
        serde_json::to_value(&report).map_err(|e| e.to_string())?
    };

    let (sbom_gate, audit_gate) = if let Some(solution_path) = opts.solution.as_deref() {
        let sol_bytes = read_bounded(solution_path, MAX_SMALL_JSON_BYTES, "solution")?;
        let solution: vesper_loom_calculus::Solution = serde_json::from_slice(&sol_bytes)
            .map_err(|e| format!("{solution_path}: invalid solution snapshot: {e}"))?;
        let sol_val = validate_solution(&solution);
        if !sol_val.diagnostics.is_empty() {
            return fail(
                opts.json,
                "solution_invalid",
                format!("{solution_path}: solution snapshot validation failed"),
                Some(serde_json::to_value(&sol_val.diagnostics).map_err(|e| e.to_string())?),
            );
        }
        let graph = sol_val
            .graph
            .as_ref()
            .ok_or("solution validation produced no graph")?;
        let root_selection = solution
            .packages
            .iter()
            .find(|sel| sel.manifest.package.name == manifest.package.name);
        if !graph.roots.contains(&manifest.package.name)
            || root_selection.is_none_or(|sel| sel.manifest != manifest)
            || graph
                .packages
                .get(&manifest.package.name)
                .is_none_or(|p| p.version != manifest.package.version)
        {
            return fail(
                opts.json,
                "solution_root_mismatch",
                format!(
                    "{solution_path}: root manifest does not match release manifest {} {}",
                    manifest.package.name.as_str(),
                    manifest.package.version
                ),
                None,
            );
        }
        let sbom = crate::sbom_commands::build(&solution)?;
        let audit_report = if let (Some(adv_path), Some(pol_path)) =
            (opts.advisories.as_deref(), opts.audit_policy.as_deref())
        {
            let limits = vesper_loom_audit::Limits::default();
            let parsed_sol =
                parse_solution(&sol_bytes, &limits).map_err(|e| format!("{solution_path}: {e}"))?;
            let catalog = parse_catalog(
                &read_bounded(adv_path, MAX_SMALL_JSON_BYTES, "advisories")?,
                &limits,
            )
            .map_err(|e| format!("{adv_path}: {e}"))?;
            let pol = parse_audit_policy(
                &read_bounded(pol_path, MAX_SMALL_JSON_BYTES, "audit policy")?,
                &limits,
            )
            .map_err(|e| format!("{pol_path}: {e}"))?;
            let report = audit(&parsed_sol, &catalog, &pol, &limits).map_err(|e| e.to_string())?;
            if report.outcome != Outcome::Clean {
                return fail(
                    opts.json,
                    "audit_rejected",
                    format!(
                        "reachable OSV dependency audit rejected publication ({:?})",
                        report.outcome
                    ),
                    Some(serde_json::to_value(&report).map_err(|e| e.to_string())?),
                );
            }
            Some(serde_json::to_value(&report).map_err(|e| e.to_string())?)
        } else {
            None
        };
        (Some(sbom), audit_report)
    } else {
        (None, None)
    };

    let proof_gate = if let (Some(root), Some(module)) =
        (opts.olean_root.as_deref(), opts.olean_module.as_deref())
    {
        let (accepted, report) = crate::proof_commands::audit_policy(
            root,
            module,
            &opts.theorems,
            &opts.allowed_axioms,
            opts.lean.as_deref(),
            opts.timeout,
        )?;
        if !accepted {
            return fail(
                opts.json,
                "proof_audit_rejected",
                "Lean kernel replay rejected release theorems (forbidden axiom or sorryAx)".into(),
                Some(report),
            );
        }
        Some(report)
    } else {
        None
    };

    let attestation_gate = if let (Some(exp_path), Some(pol_path), Some(bun_path)) = (
        opts.expected.as_deref(),
        opts.policy.as_deref(),
        opts.bundle.as_deref(),
    ) {
        let now = match opts.at {
            Some(at) => at,
            None => SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| "host clock precedes the Unix epoch")?
                .as_secs(),
        };
        let exp = parse_identity(&read_bounded(
            exp_path,
            vesper_loom_trust::MAX_JSON_BYTES as u64,
            "expected identity",
        )?)?;
        let pol = parse_policy(&read_bounded(
            pol_path,
            vesper_loom_trust::MAX_JSON_BYTES as u64,
            "builder policy",
        )?)?;
        let bun = parse_bundle(&read_bounded(
            bun_path,
            vesper_loom_trust::MAX_JSON_BYTES as u64,
            "attestation bundle",
        )?)?;
        if exp.package != manifest.package.name || exp.version != pkg_version {
            return fail(
                opts.json,
                "attestation_identity_mismatch",
                format!(
                    "expected build identity {} {} does not match manifest {} {}",
                    exp.package.as_str(),
                    exp.version,
                    manifest.package.name.as_str(),
                    pkg_version
                ),
                None,
            );
        }
        let source_tree_id = published_source_tree_id(source_path, source_id, source.len() as u64)?;
        if exp.source_tree != source_tree_id.to_string() {
            return fail(
                opts.json,
                "attestation_source_mismatch",
                format!(
                    "expected source tree {} does not match published source tree {source_tree_id}",
                    exp.source_tree
                ),
                None,
            );
        }
        if let Some(art_path) = opts.artifact.as_deref() {
            let expected_id: ObjectId = exp
                .output
                .parse()
                .map_err(|e| format!("invalid expected output identity: {e}"))?;
            if expected_id.domain != Domain::Artifact || expected_id.kind != ObjectKind::Blob {
                return Err("expected.output must be an artifact blob identity".into());
            }
            let mut hasher = vesper_loom_cas::ObjectHasher::new(Domain::Artifact, ObjectKind::Blob);
            let mut f = File::open(art_path).map_err(|e| format!("{art_path}: {e}"))?;
            let mut buf = [0u8; 64 * 1024];
            loop {
                let n = f.read(&mut buf).map_err(|e| format!("{art_path}: {e}"))?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
            let actual_id = hasher.finish();
            if actual_id != expected_id {
                return fail(
                    opts.json,
                    "artifact_hash_mismatch",
                    format!("artifact digest {actual_id} does not match expected {expected_id}"),
                    None,
                );
            }
        }
        let verif = verify_attestations(&exp, &bun, &pol, now);
        if !verif.accepted {
            return fail(
                opts.json,
                "attestation_quorum_rejected",
                "builder operator quorum rejected publication bundle".into(),
                Some(serde_json::to_value(&verif).map_err(|e| e.to_string())?),
            );
        }
        Some(serde_json::to_value(&verif).map_err(|e| e.to_string())?)
    } else {
        None
    };

    // Only ingest into the local CAS after every release gate succeeds.
    let cas_report = if let Some(store_dir) = opts.store.as_deref() {
        let store = Store::open(store_dir).map_err(|e| e.to_string())?;
        let src_ingested = store
            .put_blob(Domain::Source, source.as_bytes())
            .map_err(|e| e.to_string())?;
        let if_ingested = store
            .put_blob(Domain::Interface, &interface_bytes)
            .map_err(|e| e.to_string())?;
        Some(json!({
            "store": store_dir.display().to_string(),
            "source_id": src_ingested.id.to_string(),
            "source_newly_stored": src_ingested.newly_stored,
            "interface_id": if_ingested.id.to_string(),
            "interface_newly_stored": if_ingested.newly_stored,
        }))
    } else {
        None
    };

    let receipt = json!({
        "schema_version": 1,
        "status": "published_locally",
        "package": manifest.package.name.as_str(),
        "version": pkg_version,
        "source_id": source_id.to_string(),
        "interface_id": interface_id.to_string(),
        "interface_mif": interface_mif,
        "capability_ceiling": manifest.package.capability_ceiling,
        "forbid_unsafe_ffi": manifest.package.forbid_unsafe_ffi,
        "gates": {
            "semver": semver_gate,
            "sbom": sbom_gate,
            "audit": audit_gate,
            "proof_audit": proof_gate,
            "attestation": attestation_gate,
        },
        "cas": cas_report,
    });

    if let Some(out_path) = opts.output.as_deref() {
        let receipt_bytes = serde_json::to_vec_pretty(&receipt).map_err(|e| e.to_string())?;
        crate::package_commands::atomic_write(out_path, &receipt_bytes)?;
    }

    if opts.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&receipt).map_err(|e| e.to_string())?
        );
    } else {
        println!(
            "vesper publish accepted: {} {} ({interface_mif})",
            manifest.package.name.as_str(),
            manifest.package.version
        );
        println!("  source:    {source_id}");
        println!("  interface: {interface_id}");
        if let Some(cas) = &cas_report {
            println!("  cas store: {}", cas["store"].as_str().unwrap_or(""));
        }
    }
    let _ = Path::new(source_path);
    Ok(())
}
