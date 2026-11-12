#[path = "../src/fmt_commands.rs"]
mod fmt_commands;
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "vesper-fmt-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn file(&self) -> PathBuf {
        self.0.join("source.ves")
    }
    fn write(&self, source: &[u8]) {
        fs::write(self.file(), source).unwrap();
    }
    fn run(&self, flags: &[&str]) -> bool {
        let mut args = vec![self.file().to_string_lossy().into_owned()];
        args.extend(flags.iter().map(|s| s.to_string()));
        invoke(args)
    }
    fn unchanged(&self, source: &[u8]) {
        assert_eq!(fs::read(self.file()).unwrap(), source);
        assert_eq!(fs::read_dir(&self.0).unwrap().count(), 1);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn invoke(args: Vec<String>) -> bool {
    if let Some(binary) = option_env!("CARGO_BIN_EXE_vesper") {
        std::process::Command::new(binary)
            .arg("fmt")
            .args(args)
            .output()
            .unwrap()
            .status
            .success()
    } else {
        // Standalone leaf validation exercises the identical source adapter.
        // Root integration runs these same cases against the actual binary.
        fmt_commands::run(args).is_ok()
    }
}
#[test]
fn check_is_read_only_then_format_is_atomic_and_idempotent() {
    let f = Fixture::new();
    let source = b"fn main( )->u64{let x=1;x}";
    f.write(source);
    assert!(!f.run(&["--check", "--json"]));
    f.unchanged(source);
    assert!(f.run(&["--json"]));
    let formatted = fs::read(f.file()).unwrap();
    assert_ne!(formatted, source);
    assert!(f.run(&["--check"]));
    f.unchanged(&formatted);
    assert!(f.run(&[]));
    f.unchanged(&formatted);
}
#[test]
fn syntax_utf8_macro_and_input_budget_fail_without_any_writes() {
    let f = Fixture::new();
    for source in [
        b"fn main(){let x=".as_slice(),
        b"fn main(){m!{  token /* raw */  }}",
        b"fn main(){\xff}",
        b"/* unclosed",
    ] {
        f.write(source);
        assert!(!f.run(&["--json"]));
        f.unchanged(source);
    }
    let deep = format!("fn main(){{{}true}}", "!".repeat(200));
    f.write(deep.as_bytes());
    assert!(!f.run(&[]));
    f.unchanged(deep.as_bytes());
    let large = vec![b' '; 256 * 1024 + 1];
    f.write(&large);
    assert!(!f.run(&[]));
    f.unchanged(&large);
}
#[test]
fn options_and_missing_files_fail() {
    assert!(!invoke(vec![]));
    assert!(!invoke(vec!["--bad".into()]));
    assert!(!invoke(vec!["first".into(), "second".into()]));
    let f = Fixture::new();
    assert!(!f.run(&[]));
    assert!(invoke(vec!["--help".into()]));
    assert!(!invoke(vec!["--".into(), "--help".into()]));
    assert!(!invoke(vec![f.0.to_string_lossy().into_owned()]));
}
#[cfg(unix)]
#[test]
fn symlink_input_never_changes_link_or_target() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let target = f.0.join("target.ves");
    let source = b"fn main(){1}";
    fs::write(&target, source).unwrap();
    symlink(&target, f.file()).unwrap();
    assert!(!f.run(&[]));
    assert!(
        fs::symlink_metadata(f.file())
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(target).unwrap(), source);
    assert_eq!(fs::read_dir(&f.0).unwrap().count(), 2);
}
#[cfg(unix)]
#[test]
fn replacement_preserves_unix_permissions() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let f = Fixture::new();
    f.write(b"fn main(){1}");
    fs::set_permissions(f.file(), fs::Permissions::from_mode(0o640)).unwrap();
    assert!(f.run(&[]));
    assert_eq!(fs::metadata(f.file()).unwrap().mode() & 0o777, 0o640);
}
