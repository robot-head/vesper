use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

const MANIFEST: &str = r#"
[package]
name = "@test/app"
version = "1.0.0"
edition = "2026"

[dependencies]
"@test/math" = { version = "^1", effects = [] }

[features.extra]
"#;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-locked-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let fixture = Self(dir);
        fixture.write("Loom.toml", MANIFEST);
        fixture.catalog(&["1.0.0"]);
        fixture
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn write(&self, name: &str, text: &str) {
        fs::write(self.path(name), text).unwrap();
    }

    fn write_json(&self, name: &str, value: &Value) {
        self.write(name, &serde_json::to_string_pretty(value).unwrap());
    }

    fn read_json(&self, name: &str) -> Value {
        serde_json::from_slice(&fs::read(self.path(name)).unwrap()).unwrap()
    }

    fn catalog(&self, versions: &[&str]) {
        self.write_json(
            "catalog.json",
            &json!({
                "schema_version": 1,
                "packages": versions.iter().map(|version| json!({
                    "package": {
                        "name": "@test/math",
                        "version": version,
                        "edition": "2026"
                    }
                })).collect::<Vec<_>>()
            }),
        );
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_vesper"))
            .current_dir(&self.0)
            .args(args)
            .output()
            .unwrap()
    }

    fn resolve(&self, name: &str) {
        let output = self.run(&["resolve", "--catalog", "catalog.json", "-o", name]);
        assert_success(&output);
    }

    fn locked(&self, extra: &[&str]) -> Output {
        let mut args = vec!["resolve", "--catalog", "catalog.json", "--locked", "--json"];
        args.extend(extra);
        self.run(&args)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Test snapshots may be read-only, which prevents deletion on Windows.
        #[cfg(windows)]
        for entry in fs::read_dir(&self.0).unwrap().flatten() {
            if let Ok(metadata) = entry.metadata()
                && metadata.is_file()
                && metadata.permissions().readonly()
            {
                let mut permissions = metadata.permissions();
                permissions.set_readonly(false);
                let _ = fs::set_permissions(entry.path(), permissions);
            }
        }
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_error(output: &Output, kind: &str) -> Value {
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&output.stderr)));
    assert_eq!(report["status"], "error");
    assert_eq!(report["error"]["kind"], kind);
    assert_eq!(report["error"]["decisions"], 0);
    report
}

fn assert_unchanged(path: &Path, original: &[u8], before: &fs::Metadata) {
    assert_eq!(fs::read(path).unwrap(), original);
    let after = fs::metadata(path).unwrap();
    assert_eq!(after.modified().unwrap(), before.modified().unwrap());
    assert_eq!(after.permissions(), before.permissions());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(after.ino(), before.ino());
    }
}

#[test]
fn keeps_an_older_valid_selection_without_searching_or_rewriting() {
    let fixture = Fixture::new();
    fixture.resolve("Loom.lock");
    let path = fixture.path("Loom.lock");
    let bytes = fs::read(&path).unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&path, permissions).unwrap();
    let before = fs::metadata(&path).unwrap();
    fixture.catalog(&["1.1.0", "1.0.0"]);

    let locked = fixture.locked(&[]);
    assert_success(&locked);
    let report: Value = serde_json::from_slice(&locked.stdout).unwrap();
    assert_eq!(report["status"], "locked");
    assert_eq!(
        report["solution"],
        serde_json::from_slice::<Value>(&bytes).unwrap()
    );
    assert_unchanged(&path, &bytes, &before);

    let fresh = fixture.run(&["resolve", "--catalog", "catalog.json", "--json"]);
    assert_success(&fresh);
    let fresh: Value = serde_json::from_slice(&fresh.stdout).unwrap();
    let math = fresh["solution"]["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|package| package["manifest"]["package"]["name"] == "@test/math")
        .unwrap();
    assert_eq!(math["manifest"]["package"]["version"], "1.1.0");
    assert_unchanged(&path, &bytes, &before);
}

#[test]
fn supports_custom_lock_paths_and_rejects_stale_root_or_catalog_metadata() {
    let fixture = Fixture::new();
    fixture.resolve("pinned.json");
    let path = fixture.path("pinned.json");
    let bytes = fs::read(&path).unwrap();
    let before = fs::metadata(&path).unwrap();
    assert_success(&fixture.locked(&["--lockfile", "pinned.json"]));
    assert!(!fixture.path("Loom.lock").exists());

    fixture.write(
        "Loom.toml",
        &MANIFEST.replace(
            "edition = \"2026\"",
            "edition = \"2026\"\nlicense = \"MIT\"",
        ),
    );
    assert_error(
        &fixture.locked(&["--lockfile", "pinned.json"]),
        "locked_mismatch",
    );
    fixture.write("Loom.toml", MANIFEST);

    let mut catalog = fixture.read_json("catalog.json");
    catalog["packages"][0]["package"]["license"] = json!("MIT");
    fixture.write_json("catalog.json", &catalog);
    assert_error(
        &fixture.locked(&["--lockfile", "pinned.json"]),
        "locked_mismatch",
    );
    fixture.catalog(&[]);
    assert_error(
        &fixture.locked(&["--lockfile", "pinned.json"]),
        "locked_mismatch",
    );
    assert_unchanged(&path, &bytes, &before);
}

#[test]
fn rejects_changed_requested_features_and_injected_locked_features() {
    let fixture = Fixture::new();
    fixture.resolve("Loom.lock");
    assert_error(&fixture.locked(&["--features", "extra"]), "locked_mismatch");

    let mut snapshot = fixture.read_json("Loom.lock");
    for selected in snapshot["packages"].as_array_mut().unwrap() {
        if selected["manifest"]["package"]["name"] == "@test/app" {
            selected["features"] = json!(["extra"]);
        }
    }
    fixture.write_json("Loom.lock", &snapshot);
    assert_error(&fixture.locked(&[]), "locked_mismatch");
}

#[test]
fn distinguishes_invalid_input_unsupported_policy_and_bad_snapshot_schema() {
    let fixture = Fixture::new();
    fixture.resolve("Loom.lock");
    fixture.write(
        "Loom.toml",
        &format!("{MANIFEST}\n[resolver.policy]\noptimize = [\"minimize_size\"]\n"),
    );
    assert_error(&fixture.locked(&[]), "unsupported_policy");
    fixture.write("Loom.toml", MANIFEST);

    let mut catalog = fixture.read_json("catalog.json");
    catalog["schema_version"] = json!(999);
    fixture.write_json("catalog.json", &catalog);
    assert_error(&fixture.locked(&[]), "invalid_input");
    fixture.catalog(&["1.0.0"]);

    let mut snapshot = fixture.read_json("Loom.lock");
    snapshot["schema_version"] = json!(999);
    fixture.write_json("Loom.lock", &snapshot);
    assert_error(&fixture.locked(&[]), "locked_mismatch");
}

#[test]
fn never_creates_a_missing_lock_and_reports_parse_errors_without_json_success() {
    let fixture = Fixture::new();
    let missing = fixture.locked(&[]);
    assert!(!missing.status.success());
    assert!(missing.stdout.is_empty());
    assert!(!fixture.path("Loom.lock").exists());
    fixture.write("Loom.lock", "{not json}");
    let malformed = fixture.locked(&[]);
    assert!(!malformed.status.success());
    assert!(malformed.stdout.is_empty());
    assert!(String::from_utf8_lossy(&malformed.stderr).contains("invalid lockfile"));
    assert_eq!(
        fs::read_to_string(fixture.path("Loom.lock")).unwrap(),
        "{not json}"
    );
}

#[test]
fn rejects_output_and_search_options_in_read_only_mode() {
    let fixture = Fixture::new();
    fixture.write("keep.lock", "keep this file");
    for (args, message) in [
        (
            vec!["resolve", "--locked", "-o", "keep.lock"],
            "cannot be combined with -o",
        ),
        (
            vec!["resolve", "--locked", "--max-decisions", "1"],
            "performs no search",
        ),
        (
            vec!["resolve", "--lockfile", "keep.lock"],
            "requires --locked",
        ),
        (
            vec![
                "resolve",
                "--locked",
                "--lockfile",
                "keep.lock",
                "--lockfile",
                "other.lock",
            ],
            "only be specified once",
        ),
        (vec!["resolve", "--locked", "--lockfile"], "requires a path"),
    ] {
        let output = fixture.run(&args);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains(message));
        assert!(output.stdout.is_empty());
    }
    assert_eq!(
        fs::read_to_string(fixture.path("keep.lock")).unwrap(),
        "keep this file"
    );
    assert!(!fixture.path("Loom.lock").exists());
    assert!(!fixture.path("other.lock").exists());
}
