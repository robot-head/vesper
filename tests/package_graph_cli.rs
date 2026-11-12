use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "vesper-graph-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let fixture = Self(path);
        fixture.write(&snapshot());
        fixture
    }
    fn write(&self, snapshot: &Value) {
        fs::write(
            self.0.join("Loom.lock"),
            serde_json::to_vec_pretty(snapshot).unwrap(),
        )
        .unwrap();
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_vesper"))
            .current_dir(&self.0)
            .args(args)
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn selection(name: &str, ceiling: &[&str], dependencies: Value) -> Value {
    json!({
        "manifest": {
            "package": {
                "name": format!("@test/{name}"),
                "version": "1.0.0",
                "edition": "2026",
                "capability_ceiling": ceiling
            },
            "dependencies": dependencies
        }
    })
}

fn snapshot() -> Value {
    // A diamond with a B <-> C cycle and a feature-selected native dependency.
    let root = selection(
        "app",
        &["Storage"],
        json!({
            "@test/a": {"version":"^1", "effects":["Storage"], "features":["native"]},
            "@test/b": {"version":"^1", "effects":[]}
        }),
    );
    let mut a = selection(
        "a",
        &["Storage"],
        json!({"@test/c": {"version":"^1", "effects":[]}}),
    );
    a["manifest"]["features"] = json!({"native":{"sys_deps":["zlib ^1.2"]}});
    a["features"] = json!(["native"]);
    json!({
        "schema_version":1,
        "roots":[{"name":"@test/app","version":"=1.0.0","features":[]}],
        "packages":[
            root, a,
            selection("b", &[], json!({"@test/c":{"version":"^1","effects":[]}})),
            selection("c", &[], json!({"@test/b":{"version":"^1","effects":[]}})),
            selection("unused", &["Net"], json!({}))
        ],
        "system_packages":[
            {"name":"zlib", "version":"1.2.13"},
            {"name":"unused_sys", "version":"9.0.0"}
        ]
    })
}

fn success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn graph(fixture: &Fixture, extra: &[&str]) -> Value {
    let mut args = vec!["graph", "--json"];
    args.extend(extra);
    let output = fixture.run(&args);
    success(&output);
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn reports_reachable_graph_features_capability_edges_and_native_versions() {
    let fixture = Fixture::new();
    let before = fs::read(fixture.0.join("Loom.lock")).unwrap();
    let report = graph(&fixture, &[]);
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["status"], "graph");
    assert_eq!(report["basis"], "declared_metadata");
    assert_eq!(report["direction"], "dependencies");
    assert!(report["focus"].is_null());
    assert_eq!(report["snapshot_roots"], json!(["@test/app"]));
    assert_eq!(report["packages"].as_object().unwrap().len(), 4);
    assert_eq!(report["packages"]["@test/a"]["features"], json!(["native"]));
    assert_eq!(
        report["packages"]["@test/app"]["required_capabilities"],
        json!(["Storage"])
    );
    assert_eq!(report["packages"]["@test/app"]["is_root"], true);
    assert_eq!(
        report["packages"]["@test/b"]["required_capabilities"],
        json!([])
    );
    assert_eq!(report["system_packages"], json!({"zlib":"1.2.13"}));
    assert_eq!(report["unused_packages"], json!(["@test/unused"]));
    assert_eq!(report["unused_system_packages"], json!(["unused_sys"]));
    assert_eq!(report["dependencies"].as_array().unwrap().len(), 5);
    let edge = report["dependencies"]
        .as_array()
        .unwrap()
        .iter()
        .find(|edge| edge["dependent"] == "@test/app" && edge["dependency"] == "@test/a")
        .unwrap();
    assert_eq!(edge["allowed_capabilities"], json!(["Storage"]));
    assert_eq!(edge["requested_features"], json!(["native"]));
    assert_eq!(
        report["system_dependencies"],
        json!([
            {"package":"@test/a","system":"zlib","requirement":"^1.2"}
        ])
    );
    assert_eq!(fs::read(fixture.0.join("Loom.lock")).unwrap(), before);
}

#[test]
fn filtered_dependency_and_dependent_views_terminate_on_cycles() {
    let fixture = Fixture::new();
    let dependencies = graph(&fixture, &["--package", "@test/b"]);
    assert_eq!(dependencies["focus"], "@test/b");
    assert_eq!(dependencies["packages"].as_object().unwrap().len(), 2);
    assert!(dependencies["packages"]["@test/b"].is_object());
    assert!(dependencies["packages"]["@test/c"].is_object());
    assert_eq!(dependencies["dependencies"].as_array().unwrap().len(), 2);
    assert_eq!(dependencies["system_packages"], json!({}));
    // These are unused in the source snapshot, not merely outside this view.
    assert_eq!(dependencies["unused_packages"], json!(["@test/unused"]));
    assert_eq!(
        dependencies["unused_system_packages"],
        json!(["unused_sys"])
    );
    assert_eq!(dependencies["snapshot_roots"], json!(["@test/app"]));

    let dependents = graph(&fixture, &["--package", "@test/b", "--invert"]);
    assert_eq!(dependents["direction"], "dependents");
    assert_eq!(dependents["packages"].as_object().unwrap().len(), 4);
    assert_eq!(dependents["dependencies"].as_array().unwrap().len(), 5);
    assert_eq!(dependents["system_packages"], json!({"zlib":"1.2.13"}));
}

#[test]
fn json_text_and_dot_are_stable_when_snapshot_selections_are_reordered() {
    let fixture = Fixture::new();
    let formats: [&[&str]; 3] = [&["graph", "--json"], &["graph"], &["graph", "--dot"]];
    let original = formats.map(|args| {
        let output = fixture.run(args);
        success(&output);
        output.stdout
    });
    let mut reordered = snapshot();
    reordered["packages"].as_array_mut().unwrap().reverse();
    reordered["system_packages"]
        .as_array_mut()
        .unwrap()
        .reverse();
    fixture.write(&reordered);
    for (args, expected) in formats.into_iter().zip(original) {
        let output = fixture.run(args);
        success(&output);
        assert_eq!(output.stdout, expected);
    }
}

#[test]
fn renders_dot_with_separate_native_nodes_and_reverses_arrows() {
    let fixture = Fixture::new();
    let normal = fixture.run(&["graph", "--dot"]);
    success(&normal);
    let normal = String::from_utf8(normal.stdout).unwrap();
    assert!(normal.contains("digraph"));
    assert!(normal.contains("\"pkg:@test/app\" -> \"pkg:@test/a\""));
    assert!(normal.contains("\"pkg:@test/a\" -> \"sys:zlib\""));
    assert!(normal.contains("Storage"));
    assert!(normal.contains("1.2.13"));
    assert!(!normal.contains("pkg:@test/unused"));
    let inverse = fixture.run(&["graph", "--dot", "--invert"]);
    success(&inverse);
    let inverse = String::from_utf8(inverse.stdout).unwrap();
    assert!(inverse.contains("\"pkg:@test/a\" -> \"pkg:@test/app\""));
    assert!(inverse.contains("\"sys:zlib\" -> \"pkg:@test/a\""));
}

#[test]
fn validates_the_entire_snapshot_before_showing_a_filtered_graph() {
    let fixture = Fixture::new();
    for change in [
        "schema",
        "capability",
        "missing_native",
        "no_roots",
        "invalid_unused",
    ] {
        let mut invalid = snapshot();
        match change {
            "schema" => invalid["schema_version"] = json!(999),
            "capability" => {
                invalid["packages"][0]["manifest"]["package"]["capability_ceiling"] = json!([])
            }
            "missing_native" => invalid["system_packages"] = json!([]),
            "no_roots" => invalid["roots"] = json!([]),
            "invalid_unused" => {
                invalid["packages"][4]["manifest"]["features"] =
                    json!({"bad":{"features":["unknown"]}})
            }
            _ => unreachable!(),
        }
        fixture.write(&invalid);
        let output = fixture.run(&["graph", "--package", "@test/b", "--json"]);
        assert!(!output.status.success(), "{change}");
        let error: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(error["status"], "error");
        assert_eq!(error["error"]["kind"], "invalid_snapshot");
        assert!(error.get("packages").is_none());
        if change != "no_roots" {
            assert!(!error["error"]["diagnostics"].as_array().unwrap().is_empty());
        }
    }
}

#[test]
fn rejects_unknown_or_unreachable_focus_and_handles_explicit_paths() {
    let fixture = Fixture::new();
    for package in ["@test/absent", "@test/unused"] {
        let output = fixture.run(&["graph", "--package", package, "--json"]);
        assert!(!output.status.success());
        let error: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(error["error"]["kind"], "unknown_package");
    }
    fs::rename(fixture.0.join("Loom.lock"), fixture.0.join("pinned.json")).unwrap();
    let explicit = fixture.run(&["graph", "pinned.json", "--json"]);
    success(&explicit);
    assert!(!fixture.0.join("Loom.lock").exists());
}

#[test]
fn rejects_bad_options_and_files_without_creating_or_overwriting_output() {
    let fixture = Fixture::new();
    let original = fs::read(fixture.0.join("Loom.lock")).unwrap();
    for args in [
        vec!["graph", "--json", "--dot"],
        vec!["graph", "--package"],
        vec!["graph", "--package", "../escape"],
        vec!["graph", "--package", "@test/a", "--package", "@test/b"],
        vec!["graph", "first.lock", "second.lock"],
        vec!["graph", "-o", "new.lock"],
        vec!["graph", "missing.lock", "--json"],
    ] {
        let output = fixture.run(&args);
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty());
    }
    assert_eq!(fs::read(fixture.0.join("Loom.lock")).unwrap(), original);
    assert!(!fixture.0.join("new.lock").exists());
    assert!(!fixture.0.join("missing.lock").exists());
    fs::write(fixture.0.join("Loom.lock"), "{bad json}").unwrap();
    let output = fixture.run(&["graph", "--json"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid"));
}
