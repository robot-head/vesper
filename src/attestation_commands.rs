//! Offline signature quorum plus actual blob-content checking.
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Read,
    time::{SystemTime, UNIX_EPOCH},
};
use vesper_loom_cas::{Domain, ObjectHasher, ObjectId, ObjectKind};

const MAX_ARTIFACT_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const HELP: &str = "Usage: vesper attest verify --expected identity.json --policy builders.json --bundle attestations.json --artifact output.bin [--at UNIX_SECONDS] [--json]\n       vesper attest import --expected identity.json --policy builders.json --bundle attestations.json --artifact output.bin --store DIRECTORY [--at UNIX_SECONDS] [--json]\n\nVerify the actual artifact blob and an Ed25519 quorum under configured keys/operator groups.\nimport repeats current checks and ingests the exact owned bytes into the CAS only after acceptance (64 MiB limit).\nverify is read-only and streams files up to 4 GiB.\n--at explicitly selects the evaluation time; the default is the host's current Unix time.\nThis does not authenticate source namespaces, prove hermeticity, implement Sigstore/TUF, or change cache trust.\n";

fn input(path: &str) -> Result<Vec<u8>, String> {
    let mut bytes = vec![];
    File::open(path)
        .map_err(|e| format!("{path}: {e}"))?
        .take(vesper_loom_trust::MAX_JSON_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{path}: {e}"))?;
    if bytes.len() > vesper_loom_trust::MAX_JSON_BYTES {
        return Err(format!("{path}: attestation JSON exceeds the size limit"));
    }
    Ok(bytes)
}

fn error(json: bool, kind: &str, message: &str) -> Result<(), String> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version":1,"status":"error","error":{"kind":kind,"message":message}
            }))
            .map_err(|e| e.to_string())?
        );
    }
    Err(message.into())
}

pub fn run(args: Vec<String>) -> Result<(), String> {
    let mut args = args.into_iter();
    let importing = match args.next().as_deref() {
        Some("--help" | "-h") => {
            print!("{HELP}");
            return Ok(());
        }
        Some("verify") => false,
        Some("import") => true,
        _ => return Err("attest requires verify or import; see attest --help".into()),
    };
    let mut paths = BTreeMap::new();
    let mut at = None;
    let mut json = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{HELP}");
                return Ok(());
            }
            "--expected" | "--policy" | "--bundle" | "--artifact" => {
                let path = args
                    .next()
                    .ok_or_else(|| format!("{arg} requires a path"))?;
                if paths.insert(arg.clone(), path).is_some() {
                    return Err(format!("{arg} may only be specified once"));
                }
            }
            "--store" if importing => {
                let path = args.next().ok_or("--store requires a directory")?;
                if paths.insert(arg.clone(), path).is_some() {
                    return Err("--store may only be specified once".into());
                }
            }
            "--at" => {
                if at.is_some() {
                    return Err("--at may only be specified once".into());
                }
                at = Some(
                    args.next()
                        .ok_or("--at requires Unix seconds")?
                        .parse::<u64>()
                        .map_err(|_| "invalid --at; expected unsigned Unix seconds")?,
                );
            }
            "--json" => json = true,
            _ => return Err(format!("unknown attest option '{arg}'")),
        }
    }
    for name in ["--expected", "--policy", "--bundle", "--artifact"] {
        if !paths.contains_key(name) {
            return Err(format!("attest requires {name}"));
        }
    }
    if importing && !paths.contains_key("--store") {
        return Err("attest import requires --store".into());
    }
    let expected_bytes = input(&paths["--expected"])?;
    let policy_bytes = input(&paths["--policy"])?;
    let bundle_bytes = input(&paths["--bundle"])?;
    let now = match at {
        Some(at) => at,
        None => SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "host clock precedes the Unix epoch")?
            .as_secs(),
    };
    if importing {
        return import_blob(
            &paths,
            &expected_bytes,
            &policy_bytes,
            &bundle_bytes,
            now,
            json,
        );
    }
    let expected = vesper_loom_trust::parse_identity(&expected_bytes)
        .map_err(|e| format!("invalid expected identity: {e}"))?;
    let policy = vesper_loom_trust::parse_policy(&policy_bytes)
        .map_err(|e| format!("invalid builder policy: {e}"))?;
    let bundle = vesper_loom_trust::parse_bundle(&bundle_bytes)
        .map_err(|e| format!("invalid attestation bundle: {e}"))?;
    let id = match expected.output.parse::<ObjectId>() {
        Ok(id) if id.domain == Domain::Artifact && id.kind == ObjectKind::Blob => id,
        _ => {
            return error(
                json,
                "unsupported_output",
                "attest CLI requires an artifact-domain blob output identity",
            );
        }
    };
    let mut file =
        File::open(&paths["--artifact"]).map_err(|e| format!("{}: {e}", paths["--artifact"]))?;
    if file.metadata().map_err(|e| e.to_string())?.len() > MAX_ARTIFACT_BYTES {
        return error(
            json,
            "artifact_too_large",
            "artifact exceeds the CLI's 4 GiB limit",
        );
    }
    let mut hasher = ObjectHasher::new(Domain::Artifact, ObjectKind::Blob);
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut chunk).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        if hasher.len() + count as u64 > MAX_ARTIFACT_BYTES {
            return error(
                json,
                "artifact_too_large",
                "artifact exceeds the CLI's 4 GiB limit",
            );
        }
        hasher.update(&chunk[..count]);
    }
    let observed = hasher.finish();
    let artifact_matches = id == observed;
    let verification = vesper_loom_trust::verify_attestations(&expected, &bundle, &policy, now);
    let accepted = artifact_matches && verification.accepted;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version":1,"status":if accepted {"verified"} else {"rejected"},
                "accepted":accepted,"cache_eligible":false,
                "basis":"configured_ed25519_keys_and_operator_groups",
                "identity":expected,
                "artifact":{"expected_id":id.to_string(),"observed_id":observed.to_string(),
                    "bytes":hasher.len(),"matches":artifact_matches},
                "verification":verification
            }))
            .map_err(|e| e.to_string())?
        );
    } else {
        println!(
            "builder attestation verification: {}",
            if accepted { "accepted" } else { "rejected" }
        );
        println!(
            "artifact: {observed} ({} bytes; matches: {artifact_matches})",
            hasher.len()
        );
        println!(
            "verified operator groups: {} / {} required",
            verification.verified_operators.len(),
            verification.minimum_operators
        );
        for diagnostic in &verification.diagnostics {
            println!(
                "{} [{}]: {}",
                diagnostic.key_id.as_deref().unwrap_or("policy"),
                diagnostic.code,
                diagnostic.message
            );
        }
        println!("Configured-key verification only; cache trust is unchanged.");
    }
    if !artifact_matches {
        Err("artifact bytes do not match the expected signed output".into())
    } else if !verification.accepted {
        Err("builder attestation verification rejected the bundle".into())
    } else {
        Ok(())
    }
}

fn import_blob(
    paths: &BTreeMap<String, String>,
    expected_json: &[u8],
    policy_json: &[u8],
    bundle_json: &[u8],
    evaluated_at: u64,
    json: bool,
) -> Result<(), String> {
    let limits = vesper_loom_admission::Limits::default();
    let mut artifact = vec![];
    let file = open_import_artifact(&paths["--artifact"])?;
    if file.metadata().map_err(|e| e.to_string())?.len() > limits.max_blob_bytes as u64 {
        return error(
            json,
            "artifact_too_large",
            "artifact exceeds the admission size limit",
        );
    }
    file.take(limits.max_blob_bytes as u64 + 1)
        .read_to_end(&mut artifact)
        .map_err(|e| e.to_string())?;
    if artifact.len() > limits.max_blob_bytes {
        return error(
            json,
            "artifact_too_large",
            "artifact exceeds the admission size limit",
        );
    }
    let context = vesper_loom_admission::Context {
        expected_json,
        policy_json,
        bundle_json,
        evaluated_at,
    };
    match vesper_loom_admission::admit_blob(&paths["--store"], artifact, context, limits) {
        Ok(report) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
                );
            } else {
                println!("Signed artifact admitted to {}", paths["--store"]);
                println!("Configured-key admission only; build-cache trust is unchanged.");
            }
            Ok(())
        }
        Err(failure) => error(json, "admission_rejected", &failure.error.to_string()),
    }
}

fn open_import_artifact(path: &str) -> Result<File, String> {
    let regular = || format!("{path}: artifact import requires a regular file without symlinks");
    if !fs::symlink_metadata(path)
        .map_err(|e| format!("{path}: {e}"))?
        .is_file()
    {
        return Err(regular());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options.open(path).map_err(|e| format!("{path}: {e}"))?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err(regular());
    }
    Ok(file)
}
