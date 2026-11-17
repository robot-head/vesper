//! `vesper build --lib` (#878): the CLI writes the object and interface, and
//! refuses flags it does not apply to libraries instead of ignoring them.
use std::process::Command;

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("vesper-lib-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("t.ves"), "pub fn one() -> i64 {\n    1\n}\n").unwrap();
    dir
}

fn build(dir: &std::path::Path, extra: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
        .args(["build", "--lib"])
        .arg(dir.join("t.ves"))
        .arg("-o")
        .arg(dir.join("t.o"))
        .args(extra)
        .output()
        .unwrap()
}

#[test]
fn library_build_writes_object_and_interface() {
    let dir = scratch("ok");
    let interface = dir.join("t.json");
    let out = build(&dir, &["--interface", interface.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let object = std::fs::read(dir.join("t.o")).unwrap();
    assert!(object.windows(12).any(|w| w == b"vesper_t_one"));
    let json = std::fs::read_to_string(&interface).unwrap();
    assert!(json.contains("\"symbol\": \"vesper_t_one\""), "{json}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn verifier_tee_and_codegen_flags_are_refused() {
    let dir = scratch("flags");
    for flag in [
        &["--verify=stack"][..],
        &["--tee"],
        &["--codegen", "verified"],
    ] {
        let out = build(&dir, flag);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{flag:?} was accepted");
        assert!(
            stderr.contains("do not apply to build --lib"),
            "{flag:?}: {stderr}"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn instance_flag_exports_a_generic_at_a_stable_symbol() {
    let dir = scratch("instance");
    std::fs::write(
        dir.join("t.ves"),
        "pub fn pick[T: mod copy](a: T, b: T, first: bool) -> T {\n    if first { a } else { b }\n}\n",
    )
    .unwrap();
    let interface = dir.join("t.json");
    let out = build(
        &dir,
        &[
            "--instance",
            "pick[u64]",
            "--instance",
            "pick_byte = pick[u8]",
            "--interface",
            interface.to_str().unwrap(),
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = std::fs::read_to_string(&interface).unwrap();
    assert!(
        json.contains("\"symbol\": \"vesper_t_pick__u64\""),
        "{json}"
    );
    assert!(
        json.contains("\"symbol\": \"vesper_t_pick_byte\""),
        "{json}"
    );
    assert!(json.contains("vesper.native.export-abi/2"), "{json}");
    // Without an instance the generic is refused by name.
    let refused = build(&dir, &[]);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("cannot export generic function 'pick'"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// `@export` directives of a dependency apply to the dependency's own
/// names, not to the root library being built (review of #921).
#[test]
fn package_builds_read_directives_from_the_root_module_only() {
    let dir = std::env::temp_dir().join(format!("vesper-lib-pkg-{}", std::process::id()));
    let write = |path: &str, text: &str| {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(
        "app/Loom.toml",
        "[package]\nname = \"@demo/app\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\n\"@demo/dep\" = { version = \"^0.1\", path = \"../dep\" }\n",
    );
    write(
        "app/src/lib.ves",
        "use @demo/dep::twice;\n\npub fn four() -> u64 {\n    twice(2)\n}\n",
    );
    write(
        "dep/Loom.toml",
        "[package]\nname = \"@demo/dep\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
    );
    write(
        "dep/src/lib.ves",
        "// @export instance id[u64]\npub fn id[T](sink x: T) -> T {\n    x\n}\n\npub fn twice(x: u64) -> u64 {\n    wrapping_add(x, x)\n}\n",
    );
    let interface = dir.join("app.json");
    let out = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .args(["build", "--lib", "--lib-name", "app"])
        .arg(dir.join("app"))
        .arg("-o")
        .arg(dir.join("app.o"))
        .arg("--interface")
        .arg(&interface)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = std::fs::read_to_string(&interface).unwrap();
    assert!(json.contains("\"symbol\": \"vesper_app_four\""), "{json}");
    assert!(!json.contains("id__u64"), "{json}");
    std::fs::remove_dir_all(&dir).ok();
}
