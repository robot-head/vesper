use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-sbom-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let f = Self(dir);
        f.write(&snapshot());
        f
    }
    fn write(&self, value: &Value) {
        fs::write(
            self.0.join("Loom.lock"),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_vesper"))
            .current_dir(&self.0)
            .arg("sbom")
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
fn package(name: &str, capabilities: &[&str], dependencies: Value) -> Value {
    json!({"manifest":{"package":{"name":name,"version":"1.0.0+build","edition":"2026","capability_ceiling":capabilities},"dependencies":dependencies}})
}
fn snapshot() -> Value {
    let mut app = package(
        "@test/app",
        &["Storage"],
        json!({
            "@test/a":{"version":"^1","effects":["Storage"],"features":["native"]},
            "@test/b":{"version":"^1","effects":[]},
            "@std/lib":{"version":"^1","effects":[]},
            "std/lib":{"version":"^1","effects":[]}
        }),
    );
    app["manifest"]["package"]["license"] = json!("custom \"license\"\nnotes");
    let mut a = package(
        "@test/a",
        &["Storage"],
        json!({"@test/c":{"version":"^1","effects":[]}}),
    );
    a["manifest"]["features"] = json!({"native":{"sys_deps":["libstdc++ ^2"]}});
    a["features"] = json!(["native"]);
    json!({"schema_version":1,"roots":[{"name":"@test/app","version":"^1"}],"packages":[
        app,a,
        package("@test/b",&[],json!({"@test/c":{"version":"^1","effects":[]}})),
        package("@test/c",&[],json!({"@test/b":{"version":"^1","effects":[]}})),
        package("@std/lib",&[],json!({})),package("std/lib",&[],json!({})),
        package("@test/unused",&["Net"],json!({}))
    ],"system_packages":[{"name":"libstdc++","version":"2.1.0+abi"},{"name":"unused_sys","version":"9.0.0"}]})
}
fn success(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn component<'a>(bom: &'a Value, name: &str) -> &'a Value {
    bom["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["name"] == name)
        .unwrap()
}
fn property<'a>(value: &'a Value, name: &str) -> &'a str {
    value["properties"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == name)
        .unwrap()["value"]
        .as_str()
        .unwrap()
}

#[test]
fn emits_reachable_components_native_dependencies_and_metadata_without_mutation() {
    let f = Fixture::new();
    let bytes = fs::read(f.0.join("Loom.lock")).unwrap();
    let bom = success(&f.run(&[]));
    assert_eq!(bom["bomFormat"], "CycloneDX");
    assert_eq!(bom["specVersion"], "1.6");
    assert_eq!(bom["version"], 1);
    assert_eq!(bom["components"].as_array().unwrap().len(), 7);
    assert_eq!(component(&bom, "@test/app")["type"], "application");
    assert_eq!(component(&bom, "libstdc++")["type"], "library");
    assert_eq!(component(&bom, "libstdc++")["version"], "2.1.0+abi");
    assert_eq!(
        property(&bom["metadata"], "vesper:inventory-basis"),
        "declared_metadata"
    );
    assert_eq!(
        serde_json::from_str::<Value>(property(&bom["metadata"], "vesper:unused-packages"))
            .unwrap(),
        json!(["@test/unused"])
    );
    assert_eq!(
        serde_json::from_str::<Value>(property(&bom["metadata"], "vesper:unused-native-packages"))
            .unwrap(),
        json!(["unused_sys"])
    );
    assert_eq!(
        property(component(&bom, "@test/app"), "vesper:declared-license"),
        "custom \"license\"\nnotes"
    );
    assert_eq!(
        serde_json::from_str::<Value>(property(
            component(&bom, "@test/app"),
            "vesper:required-capabilities"
        ))
        .unwrap(),
        json!(["Storage"])
    );
    assert_eq!(fs::read(f.0.join("Loom.lock")).unwrap(), bytes);
}

#[test]
fn references_resolve_once_preserve_cycles_and_do_not_collapse_identity_namespaces() {
    let f = Fixture::new();
    let bom = success(&f.run(&[]));
    let references = bom["components"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["bom-ref"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(references.len(), 7);
    assert_ne!(
        component(&bom, "@std/lib")["purl"],
        component(&bom, "std/lib")["purl"]
    );
    assert!(
        component(&bom, "@std/lib")["purl"]
            .as_str()
            .unwrap()
            .contains("%40std")
    );
    assert!(
        component(&bom, "@std/lib")["purl"]
            .as_str()
            .unwrap()
            .contains("%2Bbuild")
    );
    assert!(
        component(&bom, "libstdc++")["purl"]
            .as_str()
            .unwrap()
            .contains("libstdc%2B%2B")
    );
    let edges = bom["dependencies"].as_array().unwrap();
    assert_eq!(edges.len(), references.len());
    let mut edge_refs = BTreeSet::new();
    for edge in edges {
        let name = edge["ref"].as_str().unwrap();
        assert!(references.contains(name));
        assert!(edge_refs.insert(name));
        for target in edge["dependsOn"].as_array().unwrap() {
            assert!(references.contains(target.as_str().unwrap()));
        }
    }
    for (from, to) in [
        ("@test/b", "@test/c"),
        ("@test/c", "@test/b"),
        ("@test/a", "libstdc++"),
    ] {
        let source = component(&bom, from)["bom-ref"].as_str().unwrap();
        let target = &component(&bom, to)["bom-ref"];
        let edge = edges.iter().find(|e| e["ref"] == source).unwrap();
        assert!(edge["dependsOn"].as_array().unwrap().contains(target));
    }
}

#[test]
fn inventory_is_stable_under_input_reordering_and_supports_an_explicit_path() {
    let f = Fixture::new();
    let first = f.run(&[]);
    success(&first);
    let mut reversed = snapshot();
    reversed["packages"].as_array_mut().unwrap().reverse();
    reversed["system_packages"]
        .as_array_mut()
        .unwrap()
        .reverse();
    f.write(&reversed);
    let second = f.run(&[]);
    success(&second);
    assert_eq!(first.stdout, second.stdout);
    fs::rename(f.0.join("Loom.lock"), f.0.join("inventory.lock")).unwrap();
    let explicit = f.run(&["inventory.lock"]);
    success(&explicit);
    assert_eq!(first.stdout, explicit.stdout);
    assert!(!f.0.join("Loom.lock").exists());
}

#[test]
fn invalid_or_rootless_snapshots_never_emit_partial_boms() {
    let f = Fixture::new();
    for mode in [
        "schema",
        "rootless",
        "capabilities",
        "invalid_unused",
        "missing_native",
    ] {
        let mut invalid = snapshot();
        match mode {
            "schema" => invalid["schema_version"] = json!(999),
            "rootless" => invalid["roots"] = json!([]),
            "capabilities" => {
                invalid["packages"][0]["manifest"]["package"]["capability_ceiling"] = json!([])
            }
            "invalid_unused" => {
                invalid["packages"][6]["manifest"]["features"] =
                    json!({"bad":{"features":["missing"]}})
            }
            "missing_native" => invalid["system_packages"] = json!([]),
            _ => unreachable!(),
        }
        f.write(&invalid);
        let output = f.run(&[]);
        assert!(!output.status.success(), "{mode}");
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn malformed_files_options_and_missing_inputs_do_not_write_output_files() {
    let f = Fixture::new();
    for args in [
        &["--json"][..],
        &["-o", "output.json"][..],
        &["a.lock", "b.lock"][..],
        &["missing.lock"][..],
    ] {
        let output = f.run(args);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    assert!(!f.0.join("output.json").exists());
    fs::write(f.0.join("Loom.lock"), "{bad json}").unwrap();
    let output = f.run(&[]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}
