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

/// The token swaps of [`vsys_accepts_what_rust_accepts_under_mutation`]:
/// each one turns a declaration into a near miss Rust may reject (a linear
/// handle losing its finalizer, a drop import with the wrong signature, a
/// C string beside a consumed handle).
const SWAPS: [(&str, &str); 12] = [
    ("affine", "linear"),
    ("linear", "affine"),
    ("sink ", "in "),
    ("nocapture in s: CStr", "sink s: CStr"),
    ("ForeignVal[Unit]", "ForeignVal[i32]"),
    ("ForeignVal[i32]", "ForeignVal[Unit]"),
    ("static", "dynamic"),
    ("dynamic", "static"),
    (": u32", ": usize"),
    ("nocapture ", ""),
    (" drop ", " "),
    ("callback ", "fn "),
];

/// The `.vsys` names Rust's driver host read (`packages::driver_host`
/// before #2760): the link mode, then the extern and callback types, wire
/// structs and enums, as `run_vsys_names` prints them.
fn rust_names(lib: &sys::LibraryDescriptor) -> String {
    let mut out = String::from(if lib.link().mode() == sys::LinkMode::Static {
        "s"
    } else {
        "d"
    });
    let names = lib
        .extern_types()
        .iter()
        .map(|t| t.name())
        .chain(lib.wire_structs().iter().map(|w| w.name()))
        .chain(lib.enums().iter().map(|e| e.name()));
    for n in names {
        out.push_str(n);
        out.push('\n');
    }
    out
}

/// Whether Rust and Vesper agree on `src`: both reject it, or both accept
/// it with the same canonical bytes and names.
fn agree(check: &Entry, names: &Entry, src: &str) -> Result<bool, String> {
    let rust = sys::parse(src, Limits::default());
    let vesper = unpack_bytes(&check.words(src.as_bytes()));
    match (rust, vesper.first()) {
        (Ok(lib), Some(&0)) if vesper[1..] == *lib.canonical_bytes() => {
            let v = String::from_utf8(unpack_bytes(&names.words(src.as_bytes()))).unwrap();
            let r = rust_names(&lib);
            if v == r {
                Ok(true)
            } else {
                Err(format!("names differ: vesper {v:?}, rust {r:?}"))
            }
        }
        (Ok(_), Some(&0)) => Err("canonical bytes differ".into()),
        (Ok(_), _) => Err("Rust accepted, Vesper rejected".into()),
        (Err(_), Some(&1)) => Ok(false),
        (Err(e), _) => Err(format!("Rust rejected ({e}), Vesper accepted")),
    }
}

/// Near misses of every repository `.vsys` file (each line left out, each
/// token swap of [`SWAPS`] at each place it occurs): the driver's package
/// load reads a library's names with `@vesper/vsys` (#2760), so it must
/// reject exactly what `vesper_ffi::sys::parse` rejects.
#[test]
fn vsys_accepts_what_rust_accepts_under_mutation() {
    let package = Package::load("packages/vesper-vsys");
    let check = package.entry("run_vsys_check", true);
    let names = package.entry("run_vsys_names", true);
    let mut files = vec![];
    for dir in ["packages", "crates", "tests"] {
        walk_vsys(Path::new(dir), &mut files);
    }
    files.sort();
    let mut cases = 0usize;
    let mut rejected = 0usize;
    let mut diffs = vec![];
    for path in &files {
        let src = fill_placeholders(&std::fs::read_to_string(path).unwrap(), TARGETS[0]);
        let lines: Vec<&str> = src.lines().collect();
        let mut mutants = vec![];
        for skip in 0..lines.len() {
            let kept: Vec<&str> = lines
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != skip)
                .map(|(_, l)| *l)
                .collect();
            mutants.push(kept.join("\n"));
        }
        for (from, to) in SWAPS {
            for (at, _) in src.match_indices(from) {
                mutants.push(format!("{}{to}{}", &src[..at], &src[at + from.len()..]));
            }
        }
        for m in &mutants {
            cases += 1;
            match agree(&check, &names, m) {
                Ok(true) => {}
                Ok(false) => rejected += 1,
                Err(why) => {
                    eprintln!("DIFF {}: {why}", path.display());
                    diffs.push(format!("{}: {why}\n{m}", path.display()))
                }
            }
        }
    }
    eprintln!("{cases} near misses, {rejected} rejected by both, {} diffs", diffs.len());
    assert!(cases > 1000, "expected > 1000 near misses, got {cases}");
    assert!(diffs.is_empty(), "{} diffs; first:\n{}", diffs.len(), diffs[0]);
}
