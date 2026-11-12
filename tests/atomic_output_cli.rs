use std::{
    fs,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

#[test]
#[cfg_attr(
    windows,
    ignore = "POSIX NAME_MAX=255 regression; Windows path-length behavior needs separate validation"
)]
fn maximum_length_output_names_allow_atomic_lockfile_and_interface_replacement() {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "vesper-long-output-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&dir).unwrap();
    fs::write(
        dir.join("Loom.toml"),
        "[package]\nname = \"@test/app\"\nversion = \"1.0.0\"\nedition = \"2026\"\n",
    )
    .unwrap();
    fs::write(
        dir.join("catalog.json"),
        "{\"schema_version\":1,\"packages\":[]}",
    )
    .unwrap();
    fs::write(dir.join("source.ves"), "fn answer() -> i64 { 42 }").unwrap();
    let lockfile = format!("{}.lock", "l".repeat(250));
    let interface = format!("{}.vhir", "i".repeat(250));
    for (name, args) in [
        (
            &lockfile,
            vec!["resolve", "--catalog", "catalog.json", "-o", &lockfile],
        ),
        (
            &interface,
            vec![
                "emit",
                "source.ves",
                "--emit",
                "interface",
                "--package",
                "@test/app",
                "-o",
                &interface,
            ],
        ),
    ] {
        assert_eq!(name.len(), 255);
        fs::write(dir.join(name), "previous output").unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_vesper"))
            .current_dir(&dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let _: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.join(name)).unwrap()).unwrap();
    }
    assert!(!fs::read_dir(&dir).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("vesper-tmp")
    }));
    fs::remove_dir_all(dir).unwrap();
}
