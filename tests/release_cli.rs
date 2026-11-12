use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
use vesper_semver::{
    Declaration, Effects, Fields, Function, Interface, Mode, Parameter, Pass, Primitive,
    Projection, Type,
};

struct Fixture(PathBuf);
impl Fixture {
    fn new(old: &Interface, new: &Interface) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-release-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("old.vhir"), serde_json::to_vec(old).unwrap()).unwrap();
        fs::write(dir.join("new.vhir"), serde_json::to_vec(new).unwrap()).unwrap();
        Self(dir)
    }
    fn run(&self, extra: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_vesper"))
            .current_dir(&self.0)
            .args(["semver", "old.vhir", "new.vhir"])
            .args(extra)
            .output()
            .unwrap()
    }
    fn release(&self, old: &str, new: &str, accepted: bool) -> Value {
        let output = self.run(&["--old-version", old, "--new-version", new, "--json"]);
        assert_eq!(
            output.status.success(),
            accepted,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["policy_version"], 1);
        assert_eq!(report["accepted"], accepted);
        report
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn added_type() -> Interface {
    let mut interface = Interface::default();
    interface.exports.insert(
        "@test/lib::type::Added".into(),
        Declaration::Struct {
            generics: vec![],
            mode: Mode::Copy,
            fields: Fields::Closed,
        },
    );
    interface
}

fn callback(pass: Pass) -> Interface {
    let integer = Type::Primitive {
        name: Primitive::I64,
    };
    let mut interface = Interface::default();
    interface.exports.insert(
        "@test/lib::fn::touch".into(),
        Declaration::Function {
            signature: Function {
                generics: vec![],
                mode: Mode::Copy,
                parameters: vec![Parameter {
                    pass,
                    ty: integer.clone(),
                    projection: Projection::All,
                }],
                result: integer,
                result_mode: Mode::Copy,
                effects: Effects::default(),
            },
        },
    );
    interface
}

#[test]
fn checks_numeric_bump_size_and_preserves_default_comparison_behavior() {
    let empty = Interface::default();
    let patch = Fixture::new(&empty, &empty);
    let report = patch.release("1.0.0", "1.0.1", true);
    assert_eq!(report["required_bump"], "patch");
    assert_eq!(report["actual_bump"], "patch");
    patch.release("1.0.0", "2.0.0", true);
    let default = patch.run(&["--json"]);
    assert!(default.status.success());
    let default: Value = serde_json::from_slice(&default.stdout).unwrap();
    assert_eq!(default["classification"], "patch");
    assert!(default.get("policy_version").is_none());

    let added = added_type();
    let minor = Fixture::new(&empty, &added);
    let insufficient = minor.release("1.0.0", "1.0.1", false);
    assert_eq!(insufficient["required_bump"], "minor");
    assert_eq!(insufficient["issues"][0]["code"], "insufficient_bump");
    minor.release("1.0.0", "1.1.0", true);
    minor.release("1.0.0", "2.0.0", true);

    let breaking = Fixture::new(&added, &empty);
    breaking.release("1.0.0", "1.1.0", false);
    let major = breaking.release("1.0.0", "2.0.0", true);
    assert_eq!(major["comparison"]["classification"], "major");
    assert!(!major["comparison"]["issues"].as_array().unwrap().is_empty());
    assert!(!breaking.run(&["--json"]).status.success());
}

#[test]
fn major_release_retains_required_migration_adapters() {
    let fixture = Fixture::new(&callback(Pass::Inout), &callback(Pass::In));
    let report = fixture.release("1.0.0", "2.0.0", true);
    assert_eq!(report["comparison"]["classification"], "major");
    assert_eq!(
        report["comparison"]["required_adapters"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    fixture.release("1.0.0", "1.1.0", false);
    assert!(!fixture.run(&["--json"]).status.success());
}

#[test]
fn unknown_interfaces_never_pass_even_with_a_major_bump() {
    let mut interface = Interface::default();
    interface.exports.insert(
        "unknown".into(),
        Declaration::Unsupported {
            description: "unmodeled contract".into(),
        },
    );
    let fixture = Fixture::new(&interface, &interface);
    let report = fixture.release("1.0.0", "2.0.0", false);
    assert_eq!(report["comparison"]["classification"], "unknown");
    assert!(
        report["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue["code"] == "unknown_interface")
    );
}

#[test]
fn release_order_ignores_build_metadata_and_prereleases_fail_closed() {
    let empty = Interface::default();
    let fixture = Fixture::new(&empty, &empty);
    for (old, new) in [
        ("1.0.0", "1.0.0"),
        ("1.1.0", "1.0.9"),
        ("1.0.0+one", "1.0.0+two"),
    ] {
        let report = fixture.release(old, new, false);
        assert!(
            report["issues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|issue| issue["code"] == "non_increasing_version")
        );
    }
    for (old, new) in [("1.0.0-rc.1", "1.0.0"), ("1.0.0", "2.0.0-rc.1")] {
        let report = fixture.release(old, new, false);
        assert!(
            report["issues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|issue| issue["code"] == "prerelease_unsupported")
        );
    }
    fixture.release("1.0.0+old", "1.0.1+new", true);
    let breaking = Fixture::new(&added_type(), &empty);
    breaking.release("0.1.0", "0.2.0", false);
    breaking.release("0.1.0", "1.0.0", true);
}

#[test]
fn version_flags_are_paired_unique_and_well_formed() {
    let empty = Interface::default();
    let fixture = Fixture::new(&empty, &empty);
    for flags in [
        vec!["--old-version", "1.0.0"],
        vec!["--new-version", "1.0.1"],
        vec!["--old-version"],
        vec!["--old-version", "one", "--new-version", "1.0.1", "--json"],
        vec![
            "--old-version",
            "1.0.0",
            "--new-version",
            "2.0.0",
            "--new-version",
            "3.0.0",
        ],
        vec![
            "--old-version",
            "1.0.0",
            "--old-version",
            "1.1.0",
            "--new-version",
            "2.0.0",
        ],
    ] {
        let output = fixture.run(&flags);
        assert!(!output.status.success(), "{flags:?}");
        assert!(output.stdout.is_empty());
    }
    let text = fixture.run(&["--old-version", "1.0.0", "--new-version", "1.0.1"]);
    assert!(text.status.success());
    assert!(String::from_utf8_lossy(&text.stdout).contains("release 1.0.0 -> 1.0.1: accepted"));
}
