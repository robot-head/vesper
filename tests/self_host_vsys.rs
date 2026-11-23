//! Differential test of `@vesper/vsys` (`packages/vesper-vsys`) against
//! `vesper_ffi::sys::parse` over every `.vsys` file in the repository and
//! negative/error cases (part of #1408, #1627).

#[path = "common/check_harness.rs"]
mod check_harness;

use check_harness::*;
use std::path::{Path, PathBuf};
use vesper_ffi::sys::{self, Limits};

const TARGETS: [(&str, &str); 4] = [
    ("x86_64-unknown-linux-gnu", "sysv64"),
    ("aarch64-unknown-linux-gnu", "aapcs64"),
    ("aarch64-apple-darwin", "darwin64"),
    ("x86_64-pc-windows-msvc", "win64"),
];

fn walk_vsys(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk_vsys(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("vsys") {
            out.push(p);
        }
    }
}

fn fill_placeholders(src: &str, target: (&str, &str)) -> String {
    src.replace("@TARGET@", target.0)
        .replace("@ABI@", target.1)
        .replace(
            "@DIGEST@",
            "0000000000000000000000000000000000000000000000000000000000000000",
        )
}

fn unpack_bytes(words: &[u64]) -> Vec<u8> {
    let len = words[0] as usize;
    let mut bytes = Vec::with_capacity(len + 8);
    for w in &words[1..] {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    bytes.truncate(len);
    bytes
}

#[test]
fn vsys_matches_rust_across_repo_and_errors() {
    let entry = Package::load("packages/vesper-vsys").entry("run_vsys_check", true);

    let mut files = vec![];
    for dir in ["packages", "crates", "tests"] {
        walk_vsys(Path::new(dir), &mut files);
    }
    files.sort();
    assert!(!files.is_empty(), "expected .vsys files in repo");

    let mut checked_ok = 0usize;
    for path in &files {
        let raw = std::fs::read_to_string(path).unwrap();
        let targets: &[(&str, &str)] = if raw.contains("@TARGET@") {
            &TARGETS
        } else {
            &[TARGETS[0]]
        };
        for &target in targets {
            let src = fill_placeholders(&raw, target);
            let rust = sys::parse(&src, Limits::default());
            let vesper = unpack_bytes(&entry.words(src.as_bytes()));
            match rust {
                Ok(lib) => {
                    assert!(
                        vesper.first() == Some(&0),
                        "{} ({}): Rust accepted but Vesper rejected: {:?}",
                        path.display(),
                        target.0,
                        vesper
                    );
                    let rb = lib.canonical_bytes();
                    let vb = &vesper[1..];
                    if vb != rb {
                        let pos = vb
                            .iter()
                            .zip(rb.iter())
                            .position(|(a, b)| a != b)
                            .unwrap_or(vb.len().min(rb.len()));
                        let lo = pos.saturating_sub(40);
                        panic!(
                            "{} ({}): canonical bytes differ at byte {pos} (vesper len={}, rust len={}):\n  vesper: {:?}\n  rust:   {:?}",
                            path.display(),
                            target.0,
                            vb.len(),
                            rb.len(),
                            String::from_utf8_lossy(&vb[lo..(pos + 60).min(vb.len())]),
                            String::from_utf8_lossy(&rb[lo..(pos + 60).min(rb.len())]),
                        );
                    }
                    checked_ok += 1;
                }
                Err(e) => {
                    assert!(
                        vesper.first() == Some(&1),
                        "{} ({}): Rust rejected ({e}) but Vesper accepted",
                        path.display(),
                        target.0
                    );
                }
            }
        }
    }
    assert!(
        checked_ok >= 10,
        "expected >= 10 valid .vsys cases, got {checked_ok}"
    );

    let negatives = [
        // Missing required header option.
        r#"sys library Bad (target = "x86_64-unknown-linux-gnu", abi = "sysv64") {}"#,
        // Mismatched ABI.
        r#"sys library Bad (target = "x86_64-unknown-linux-gnu", abi = "win64", link = "dynamic", name = "c", artifact = "blake3:0000000000000000000000000000000000000000000000000000000000000000") {}"#,
        // Negative unsigned constant.
        r#"sys library Bad (target = "x86_64-unknown-linux-gnu", abi = "sysv64", link = "dynamic", name = "c", artifact = "blake3:0000000000000000000000000000000000000000000000000000000000000000") { const X: u32 = -1; }"#,
        // Empty enum.
        r#"sys library Bad (target = "x86_64-unknown-linux-gnu", abi = "sysv64", link = "dynamic", name = "c", artifact = "blake3:0000000000000000000000000000000000000000000000000000000000000000") { enum E: u32 {} }"#,
        // Implicit padding in wire struct.
        r#"sys library Bad (target = "x86_64-unknown-linux-gnu", abi = "sysv64", link = "dynamic", name = "c", artifact = "blake3:0000000000000000000000000000000000000000000000000000000000000000") { wire struct S { a: u8, b: u32 } }"#,
        // Recursive wire struct.
        r#"sys library Bad (target = "x86_64-unknown-linux-gnu", abi = "sysv64", link = "dynamic", name = "c", artifact = "blake3:0000000000000000000000000000000000000000000000000000000000000000") { wire struct S { next: S } }"#,
        // Non-64-bit variadic parameter.
        r#"sys library Bad (target = "x86_64-unknown-linux-gnu", abi = "sysv64", link = "dynamic", name = "c", artifact = "blake3:0000000000000000000000000000000000000000000000000000000000000000") { fn f(a: i32, ..., b: u32) -> ForeignVal[i32]; }"#,
    ];
    for (i, bad) in negatives.iter().enumerate() {
        assert!(
            sys::parse(bad, Limits::default()).is_err(),
            "neg {i} unexpectedly parsed in Rust"
        );
        let vesper = unpack_bytes(&entry.words(bad.as_bytes()));
        assert_eq!(
            vesper.first(),
            Some(&1),
            "neg {i} unexpectedly accepted by Vesper"
        );
    }
}
