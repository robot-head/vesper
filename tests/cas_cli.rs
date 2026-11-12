use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
use vesper_loom_cas::{Domain, ObjectId, Store, blob_id};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "vesper-cas-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_vesper"))
            .current_dir(&self.0)
            .arg("cas")
            .args(args)
            .output()
            .unwrap()
    }
    fn put(&self, kind: &str, input: &str, domain: &str) -> Value {
        success(self.run(&[
            kind, input, "--domain", domain, "--store", "store", "--json",
        ]))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn failure(output: Output) -> String {
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    String::from_utf8(output.stderr).unwrap()
}

#[test]
fn ingestion_rehashing_and_dedup_preserve_content_domains() {
    let f = Fixture::new();
    let bytes = [0, 255, 13, 10, 42];
    fs::write(f.0.join("input"), bytes).unwrap();
    for domain in Domain::ALL {
        let first = f.put("put-blob", "input", domain.as_str());
        assert_eq!(first["id"], blob_id(domain, &bytes).to_string());
        assert_eq!(first["object_bytes"], bytes.len());
        assert_eq!(first["domain"], domain.as_str());
        assert_eq!(first["kind"], "blob");
        assert_eq!(first["newly_stored"], true);
        assert_eq!(first["cache_eligible"], false);
        let second = f.put("put-blob", "input", domain.as_str());
        assert_eq!(second["id"], first["id"]);
        assert_eq!(second["newly_stored"], false);
        let id = first["id"].as_str().unwrap();
        let report = success(f.run(&["verify", id, "--store", "store", "--json"]));
        assert_eq!(report["object_bytes"], bytes.len());
        assert_eq!(report["scope"], "object_only");
        assert_eq!(report["cache_eligible"], false);
        let text = f.run(&["verify", id, "--store", "store"]);
        assert!(text.status.success());
        assert_eq!(String::from_utf8(text.stdout).unwrap(), format!("{id}\n"));
    }
    assert_eq!(fs::read(f.0.join("input")).unwrap(), bytes);
}

#[test]
fn tree_materialization_is_independent_fresh_and_complete() {
    let f = Fixture::new();
    fs::create_dir_all(f.0.join("source/nested")).unwrap();
    fs::write(f.0.join("source/a"), b"original").unwrap();
    fs::write(f.0.join("source/nested/b"), [0, 255]).unwrap();
    let tree = f.put("put-tree", "source", "source");
    let id = tree["id"].as_str().unwrap();
    assert_eq!(tree["kind"], "tree");
    let report = success(f.run(&[
        "materialize",
        id,
        "workspace",
        "--store",
        "store",
        "--copy",
        "--json",
    ]));
    assert_eq!(report["files"], 2);
    assert_eq!(report["directories"], 2);
    assert_eq!(report["bytes"], 10);
    assert_eq!(report["copied"], 2);
    assert_eq!(report["reflinked"], 0);
    assert_eq!(fs::read(f.0.join("workspace/nested/b")).unwrap(), [0, 255]);
    fs::write(f.0.join("workspace/a"), b"modified").unwrap();
    failure(f.run(&["materialize", id, "workspace", "--store", "store"]));
    assert_eq!(fs::read(f.0.join("workspace/a")).unwrap(), b"modified");
    let second = f.run(&["materialize", id, "workspace2", "--store", "store"]);
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(fs::read(f.0.join("workspace2/a")).unwrap(), b"original");
    assert_eq!(fs::read(f.0.join("source/a")).unwrap(), b"original");
    assert_eq!(f.put("put-tree", "source", "source")["id"], id);
}

#[test]
fn corruption_and_missing_children_fail_without_partial_materialization() {
    let f = Fixture::new();
    fs::create_dir(f.0.join("source")).unwrap();
    fs::write(f.0.join("source/file"), b"original").unwrap();
    let tree = f.put("put-tree", "source", "artifact");
    let tree_id = tree["id"].as_str().unwrap();
    let store = Store::open(f.0.join("store")).unwrap();
    let blob = blob_id(Domain::Artifact, b"original");
    let path = store.object_path(&blob);
    #[allow(clippy::permissions_set_readonly_false)]
    {
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(false);
        fs::set_permissions(&path, permissions).unwrap();
    }
    fs::write(&path, b"tampered").unwrap();
    failure(f.run(&["verify", &blob.to_string(), "--store", "store", "--json"]));
    // Object-only verification of the tree never claims child verification.
    assert_eq!(
        success(f.run(&["verify", tree_id, "--store", "store", "--json"]))["scope"],
        "object_only"
    );
    failure(f.run(&[
        "materialize",
        tree_id,
        "workspace",
        "--store",
        "store",
        "--copy",
        "--json",
    ]));
    assert!(!f.0.join("workspace").exists());
    assert!(!fs::read_dir(&f.0).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".vesper-staging-")
    }));
    fs::remove_file(&path).unwrap();
    failure(f.run(&[
        "materialize",
        tree_id,
        "workspace",
        "--store",
        "store",
        "--json",
    ]));
    assert!(!f.0.join("workspace").exists());
    assert!(tree_id.parse::<ObjectId>().is_ok());
}

#[test]
fn invalid_arguments_and_uninitialized_reads_do_not_create_stores() {
    let f = Fixture::new();
    for args in [
        vec![],
        vec!["unknown"],
        vec!["put-blob", "input", "--store", "store"],
        vec!["put-tree", "input", "--domain", "source"],
        vec![
            "put-blob", "input", "--domain", "unknown", "--store", "store",
        ],
        vec!["verify", "../bad", "--store", "store"],
        vec![
            "put-blob", "input", "--domain", "source", "--store", "store", "--copy",
        ],
        vec![
            "put-blob", "input", "--domain", "source", "--store", "store", "--store", "other",
        ],
        vec![
            "put-blob", "input", "--domain", "source", "--store", "store", "--json", "--json",
        ],
        vec![
            "put-blob", "input", "extra", "--domain", "source", "--store", "store",
        ],
    ] {
        failure(f.run(&args));
        assert!(!f.0.join("store").exists());
    }
    let id = blob_id(Domain::Artifact, b"missing").to_string();
    failure(f.run(&["verify", &id, "--store", "store"]));
    failure(f.run(&["materialize", &id, "workspace", "--store", "store"]));
    assert!(!f.0.join("store").exists());
    assert!(f.run(&["--help"]).status.success());
}

#[test]
fn self_ingestion_and_linked_inputs_are_rejected() {
    let f = Fixture::new();
    fs::create_dir(f.0.join("source")).unwrap();
    assert!(
        failure(f.run(&[
            "put-tree",
            "source",
            "--domain",
            "source",
            "--store",
            "source/store"
        ]))
        .contains("outside")
    );
    #[cfg(unix)]
    {
        fs::write(f.0.join("outside"), b"secret").unwrap();
        std::os::unix::fs::symlink(f.0.join("outside"), f.0.join("link")).unwrap();
        failure(f.run(&["put-blob", "link", "--domain", "source", "--store", "store"]));
        assert_eq!(fs::read(f.0.join("outside")).unwrap(), b"secret");
    }
}
