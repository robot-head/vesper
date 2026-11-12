//! Local content identity and materialization; no provenance/cache-trust decision.
use serde_json::json;
use std::path::PathBuf;
use vesper_loom_cas::{CopyStrategy, Domain, MaterializeOptions, ObjectId, ObjectKind, Store};

const HELP: &str = "Usage:\n  vesper cas put-blob <file> --domain source|interface|artifact --store <dir> [--json]\n  vesper cas put-tree <dir> --domain source|interface|artifact --store <dir> [--json]\n  vesper cas verify <object-id> --store <dir> [--json]\n  vesper cas materialize <tree-id> <destination> --store <dir> [--copy] [--json]\n\nIngestion initializes the local private store if needed. Verification rehashes\nonly the named object; materialization verifies the entire referenced tree and\nrequires a fresh destination. These operations establish content identity,\nnot provenance, namespace ownership, builder trust or cache eligibility.\n";

pub fn run(args: Vec<String>) -> Result<(), String> {
    let mut args = args.into_iter();
    let operation = match args.next().as_deref() {
        Some("-h" | "--help") => {
            print!("{HELP}");
            return Ok(());
        }
        Some(op @ ("put-blob" | "put-tree" | "verify" | "materialize")) => op.to_owned(),
        _ => return Err(format!("expected a cas operation; {HELP}")),
    };
    let mut store_path = None;
    let mut domain = None;
    let mut copy = false;
    let mut json_output = false;
    let mut positional_only = false;
    let mut positional = Vec::new();
    while let Some(arg) = args.next() {
        if !positional_only {
            match arg.as_str() {
                "--help" | "-h" => {
                    print!("{HELP}");
                    return Ok(());
                }
                "--" => {
                    positional_only = true;
                    continue;
                }
                "--store" => {
                    if store_path.is_some() {
                        return Err("--store may only be supplied once".into());
                    }
                    store_path = Some(PathBuf::from(
                        args.next().ok_or("--store requires a directory")?,
                    ));
                    continue;
                }
                "--domain" => {
                    if domain.is_some() {
                        return Err("--domain may only be supplied once".into());
                    }
                    domain = Some(match args.next().as_deref() {
                        Some("source") => Domain::Source,
                        Some("interface") => Domain::Interface,
                        Some("artifact") => Domain::Artifact,
                        _ => return Err("--domain requires source, interface or artifact".into()),
                    });
                    continue;
                }
                "--copy" => {
                    if copy {
                        return Err("--copy may only be supplied once".into());
                    }
                    copy = true;
                    continue;
                }
                "--json" => {
                    if json_output {
                        return Err("--json may only be supplied once".into());
                    }
                    json_output = true;
                    continue;
                }
                _ if arg.starts_with('-') => return Err(format!("unknown cas option '{arg}'")),
                _ => {}
            }
        }
        positional.push(arg);
    }
    let store_path = store_path.ok_or("cas requires --store <directory>")?;
    let ingestion = matches!(operation.as_str(), "put-blob" | "put-tree");
    if domain.is_some() != ingestion {
        return Err("--domain is required for ingestion and only valid for ingestion".into());
    }
    if copy && operation != "materialize" {
        return Err("--copy requires materialize".into());
    }
    if positional.len() != if operation == "materialize" { 2 } else { 1 } {
        return Err(format!(
            "wrong number of paths/identities for cas {operation}"
        ));
    }
    let id = if ingestion {
        None
    } else {
        let id = positional[0]
            .parse::<ObjectId>()
            .map_err(|e| e.to_string())?;
        if operation == "materialize" && id.kind != ObjectKind::Tree {
            return Err("cas materialize requires a tree identity".into());
        }
        Some(id)
    };
    if !ingestion && !store_path.join("LAYOUT").is_file() {
        return Err(
            "cas verification/materialization requires an existing initialized store".into(),
        );
    }
    let store = Store::open(&store_path).map_err(|e| e.to_string())?;
    if operation == "put-tree" {
        let source = std::fs::canonicalize(&positional[0]).map_err(|e| e.to_string())?;
        if store.root().starts_with(source) {
            return Err("CAS store must be outside the directory being ingested".into());
        }
    }
    let report = if ingestion {
        let domain = domain.expect("ingestion domain validated");
        let result = if operation == "put-blob" {
            store.put_file(domain, &positional[0])
        } else {
            store.ingest_dir(domain, &positional[0])
        }
        .map_err(|e| e.to_string())?;
        json!({"schema_version":1,"operation":operation,"id":result.id.to_string(),
            "domain":result.id.domain.as_str(),"kind":result.id.kind.as_str(),
            "object_bytes":result.size,"newly_stored":result.newly_stored,
            "verification_basis":"content_identity","cache_eligible":false})
    } else {
        let id = id.expect("non-ingestion identity validated");
        if operation == "verify" {
            let bytes = store.verify_object(&id).map_err(|e| e.to_string())?;
            if id.kind == ObjectKind::Tree {
                store.read_tree(&id).map_err(|e| e.to_string())?;
            }
            json!({"schema_version":1,"operation":operation,"id":id.to_string(),
                "object_bytes":bytes,"scope":"object_only","verification_basis":"content_identity",
                "cache_eligible":false})
        } else {
            let options = MaterializeOptions {
                strategy: if copy {
                    CopyStrategy::CopyOnly
                } else {
                    CopyStrategy::ReflinkOrCopy
                },
                verify_reflinks: true,
            };
            let materialized = store
                .materialize(&id, &positional[1], options)
                .map_err(|e| e.to_string())?;
            json!({"schema_version":1,"operation":operation,"id":id.to_string(),
                "files":materialized.files,"directories":materialized.directories,
                "bytes":materialized.bytes,"reflinked":materialized.reflinked,"copied":materialized.copied,
                "verification_basis":"content_identity","cache_eligible":false})
        }
    };
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else if ingestion || operation == "verify" {
        println!("{}", report["id"].as_str().expect("report identity"));
    } else {
        println!(
            "materialized {} files ({} bytes)",
            report["files"], report["bytes"]
        );
    }
    Ok(())
}
