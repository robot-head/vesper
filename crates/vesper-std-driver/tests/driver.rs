//! The sample driver of self-hosting SH0 lane 3 (#1125), compiled ahead of
//! time and linked into this test (feature `vesper-port`): it walks a
//! package's `src/**`, reads every `.ves` file, spawns `cc --version` and
//! writes its report atomically, through @vesper-std/fs, process and env.
//! The report must equal the one computed here with `std`. CI runs it on
//! Linux, macOS and Windows (scripts/test-port-harness.sh, ABI tier).
#![cfg(vesper_port)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn fnv(s: &[u8]) -> u64 {
    let mut h: u64 = 14695981039346656037;
    for &b in s {
        h = (h ^ u64::from(b)).wrapping_mul(1099511628211);
    }
    h
}

fn walk(root: &Path, rel: &str, out: &mut Vec<String>) {
    for e in std::fs::read_dir(root.join(rel)).unwrap() {
        let e = e.unwrap();
        let name = e.file_name().into_string().unwrap();
        let path = if rel.is_empty() {
            name
        } else {
            format!("{rel}/{name}")
        };
        let t = e.file_type().unwrap();
        if t.is_dir() {
            walk(root, &path, out);
        } else if t.is_file() && path.ends_with(".ves") {
            out.push(path);
        }
    }
}

/// The report the driver must write for package `root` and compiler `cc`.
fn model(root: &Path, cc: &str) -> String {
    let src = root.join("src");
    let mut files = vec![];
    walk(&src, "", &mut files);
    files.sort();
    let mut out = String::new();
    let mut total = 0;
    for f in &files {
        let text = std::fs::read(src.join(f)).unwrap();
        total += text.len();
        writeln!(out, "file {f} {} {:016x}", text.len(), fnv(&text)).unwrap();
    }
    writeln!(out, "files {}\nbytes {total}", files.len()).unwrap();
    match std::process::Command::new(cc)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
    {
        Ok(o) => {
            let first = o
                .stdout
                .split(|&b| b == b'\n' || b == b'\r')
                .next()
                .unwrap_or_default();
            writeln!(
                out,
                "cc {} {}",
                o.status.code().unwrap_or(0),
                String::from_utf8_lossy(first)
            )
            .unwrap();
        }
        Err(e) => writeln!(out, "cc error {}", vesper_std_host::IoKind::of(&e) as u64).unwrap(),
    }
    out
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn driver_walks_reads_spawns_and_writes_atomically() {
    let tmp = std::env::temp_dir().join(format!("vesper-std-driver-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let out = tmp.join("report.txt");
    std::fs::write(&out, "stale").unwrap();
    // Packages of increasing size: the std fs package, the self-hosted
    // compiler front end (many files).
    for package in ["packages/vesper-std/fs", "packages/vesper-compiler"] {
        let root = repo().join(package);
        // The driver reads its settings from the environment.
        // SAFETY: this is the only test in this binary, and nothing else
        // reads or writes the environment while it runs.
        unsafe {
            std::env::set_var("VESPER_STD_DRIVER_ROOT", &root);
            std::env::set_var("VESPER_STD_DRIVER_OUT", &out);
            std::env::set_var("VESPER_STD_DRIVER_CC", "cc");
        }
        let before = vesper_std_host::live_handles();
        let files = vesper_std_driver::driver::run();
        assert_eq!(vesper_std_host::live_handles(), before, "{package}");
        assert_eq!(
            vesper_std_driver::driver::live_allocations(),
            0,
            "{package}"
        );
        let want = model(&root, "cc");
        assert_eq!(std::fs::read_to_string(&out).unwrap(), want, "{package}");
        assert_eq!(
            files,
            want.lines().filter(|l| l.starts_with("file ")).count() as u64,
            "{package}"
        );
        assert!(files > 0, "{package}");
    }
    // Only the report is left: the temporary file was renamed over it.
    let left: Vec<_> = std::fs::read_dir(&tmp).unwrap().collect();
    assert_eq!(left.len(), 1);
    // A missing package: the walk fails with NotFound.
    // SAFETY: as above.
    unsafe { std::env::set_var("VESPER_STD_DRIVER_ROOT", tmp.join("missing")) };
    assert_eq!(vesper_std_driver::driver::run(), 1_000_001);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn driver_vsys_copies_match_the_packages() {
    let lib =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("vesper/src/lib.ves"))
            .unwrap()
            .replace("\r\n", "\n");
    for name in ["fs", "process", "env"] {
        let vsys =
            std::fs::read_to_string(repo().join(format!("packages/vesper-std/{name}/{name}.vsys")))
                .unwrap()
                .replace("\r\n", "\n");
        let mut block = format!("// @export vsys direct rust=crate::{name}\n");
        for line in vsys.lines().filter(|l| !l.starts_with("//")) {
            if line.is_empty() {
                block.push_str("// |\n");
            } else {
                block.push_str(&format!("// | {line}\n"));
            }
        }
        assert!(
            lib.contains(&block),
            "vesper/src/lib.ves lacks the current {name}.vsys"
        );
    }
}
