//! Bootstrap switch slice 3d (#2659, part of #2349): @vesper/native-lib's
//! calls input made by Stage 2's own front end (@vesper/export-build
//! `stage2_calls_input`: @vesper/stage2's loader, checker and lowering,
//! written with the Vesper writers of #2677) against the one the Rust
//! compiler encodes (tests/common/codegen_calls_words.rs) for the same
//! library.
//!
//! The two inputs spell names and types in different orders, so they are
//! compared through what reads them: @vesper/codegen-calls' instances and
//! declarations (`codegen_calls_text`) and @vesper/place-ir's canonical
//! program dump (`place_ir_dump`).
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/codegen_calls_words.rs"]
mod codegen_calls_words;
#[path = "common/ir_words.rs"]
mod ir_words;

use check_harness::{Package, repo};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The driver: `driver <package> <roots file> <optimize: 0 or 1> <out>`
/// writes the Stage 2 calls input and exits 0, or exits with the reason it
/// declined (`CALLS_*` in packages/vesper-export-build/src/calls_input.ves)
/// after writing the front end's error.
const DRIVER: &str = r#"use @vesper/export-build::calls_input::{stage2_calls_input};
use @vesper-std/env::{arg_bytes};
use @vesper-std/fs::{IoError, read_path, write_path};

fn arg(i: u64) -> ChunkSeq[u8] eff [Ffi[StdEnv]] {
    match arg_bytes(i) {
        Option::Some(a) => a,
        Option::None => ChunkSeq::new[u8](),
    }
}

pub fn main() -> u64 eff [Ffi[StdFs], Ffi[StdEnv]] {
    let root = arg(1);
    let roots_path = arg(2);
    let optimize = arg(3);
    let out = arg(4);
    let mut roots: ChunkSeq[u8] = ChunkSeq::new[u8]();
    handle Fail[IoError] in {
        roots = read_path(in roots_path.as_slice());
    } with {
        return v => v,
        raise(_e, _k) => {},
    }
    let made = stage2_calls_input(
        in root.as_slice(),
        in roots.as_slice(),
        optimize.try_get(0).unwrap_or(0) == 49
    );
    let body = if made.declined == 0 { made.bytes } else { made.err };
    handle Fail[IoError] in {
        write_path(in out.as_slice(), in body.as_slice());
    } with {
        return v => v,
        raise(_e, _k) => {},
    }
    widen_u64(made.declined)
}
"#;

/// Why the driver declined, by exit status.
const DECLINES: &[&str] = &[
    "",
    "the Stage 2 front end failed",
    "dyn, fip or an expression the encoder cannot spell",
    "no library root",
];

/// The functions Stage 2 must lower as Rust does today. The rest differ
/// where Stage 2's checker records less than Rust's: overflow and partial
/// operations (`fallible`) and the unit type of statements.
const MIN_SAME_FUNCTIONS: usize = 8;

/// The functions of a `place_ir_dump`, by name: each body's lines with
/// path ids replaced by the paths they name, and the spans (which Stage 2
/// offsets by the modules assembled before the package, and leaves out of
/// parameters) left out.
fn functions(dump: &str) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut paths = std::collections::HashMap::new();
    let mut out = std::collections::BTreeMap::new();
    let mut current: Option<String> = None;
    for line in dump.lines() {
        if let Some(rest) = line.strip_prefix("path ") {
            let (id, named) = rest.split_once(' ').unwrap_or((rest, ""));
            paths.insert(id.to_string(), named.to_string());
        } else if let Some(name) = line.strip_prefix("fn ") {
            current = Some(name.to_string());
            out.insert(name.to_string(), vec![]);
        } else if let Some(name) = &current {
            let words: Vec<String> = line
                .split_whitespace()
                .map(|w| match paths.get(w) {
                    Some(p) if w.starts_with('p') => format!("[{p}]"),
                    _ => w.to_string(),
                })
                .collect();
            if let Some(body) = out.get_mut(name) {
                body.push(words.join(" "));
            }
        }
    }
    out
}

/// Whether two bodies agree once spans are left out: `@a..b` tokens and
/// every number of a header line (parameter spans sit among them).
fn same_body(a: &[String], b: &[String]) -> bool {
    let strip = |l: &String| -> String {
        let header = l.starts_with("header");
        l.split(' ')
            .filter(|w| !w.starts_with('@') && !(header && w.parse::<u64>().is_ok()))
            .collect::<Vec<_>>()
            .join(" ")
    };
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| strip(x) == strip(y))
}

/// The calls task flags (packages/vesper-codegen-calls/src/data.ves).
const OPTIMIZE_TASK: u64 = 1;
const LIBRARY_TASK: u64 = 4;
const MORE_ROOTS_TASK: u64 = 8;

/// Libraries and their export roots.
const LIBRARIES: &[(&str, &str, &[&str])] = &[
    (
        "arith",
        "pub fn add3(a: i64, b: i64, c: i64) -> i64 { wrapping_add(wrapping_add(a, b), c) }
pub fn mix(a: u32, b: u32) -> u32 { (a ^ b) | (a & 7) }",
        &["add3", "mix"],
    ),
    (
        "branches",
        "pub fn sign(x: i16) -> i16 { if x < 0 { -1 } else if x == 0 { 0 } else { 1 } }
pub fn pick(c: bool, x: u8) -> u8 { let mut y: u8 = x; if c { y = x & 15; } y }",
        &["pick", "sign"],
    ),
    (
        "calls",
        "fn square(x: i64) -> i64 { wrapping_mul(x, x) }
pub fn sum_squares(a: i64, b: i64) -> i64 { wrapping_add(square(a), square(b)) }
pub fn quad(x: i64) -> i64 { square(square(x)) }",
        &["quad", "sum_squares"],
    ),
    (
        "fails",
        "fn step(a: u64, b: u64) -> u64 eff [Fail[ArithOverflow]] { a * b + 1 }
pub fn total(a: u64, b: u64) -> u64 eff [Fail[ArithOverflow], Fail[RuntimeError]] { step(a, b) / b }",
        &["total"],
    ),
    (
        "floats",
        "pub fn lerp(a: f64, b: f64, t: f64) -> f64 { a + (b - a) * t }
pub fn half(x: f32) -> f32 { x * 0.5 }",
        &["half", "lerp"],
    ),
];

fn build_driver(dir: &Path) -> PathBuf {
    let package = dir.join("driver");
    std::fs::create_dir_all(package.join("src")).unwrap();
    let path = |p: &str| serde_json::to_string(repo().join(p).to_str().unwrap()).unwrap();
    std::fs::write(
        package.join("Loom.toml"),
        format!(
            "[package]\nname = \"@test/stage2-calls-driver\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n\
             [dependencies]\n\
             \"@vesper/export-build\" = {{ version = \"^0.1\", path = {} }}\n\
             \"@vesper-std/env\" = {{ version = \"^0.1\", path = {} }}\n\
             \"@vesper-std/fs\" = {{ version = \"^0.1\", path = {} }}\n",
            path("packages/vesper-export-build"),
            path("packages/vesper-std/env"),
            path("packages/vesper-std/fs"),
        ),
    )
    .unwrap();
    std::fs::write(package.join("src/main.ves"), DRIVER).unwrap();
    let binary = dir.join("stage2-calls-driver");
    let out = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("build")
        .arg(&package)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    binary
}

/// The calls input the Rust compiler encodes for the package at `dir`'s
/// library task over `roots`, optimized, from its first lowering.
fn rust_input(dir: &Path, roots: &[&str]) -> Vec<u8> {
    let p = vesper::packages::load(dir).unwrap_or_else(|e| panic!("{e:?}"));
    assert!(p.ok(), "{:?} {:?}", p.diagnostics, p.manifest_errors);
    let checked = vesper::check::check(&p.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let program = vesper::ir_swap::lower(&checked.world);
    let tasks: Vec<(String, u64)> = roots
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let place = if i == 0 {
                LIBRARY_TASK
            } else {
                MORE_ROOTS_TASK
            };
            (r.to_string(), OPTIMIZE_TASK | place)
        })
        .collect();
    codegen_calls_words::calls_input_flags_wide(&checked.world, &program, &tasks)
        .expect("the library encodes")
}

#[cfg(unix)]
#[test]
fn stage2_calls_inputs_read_as_rust_ones() {
    let scratch = std::env::temp_dir().join(format!("vesper-stage2-calls-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let driver = build_driver(&scratch);
    let calls = Package::load("packages/vesper-codegen-calls").entry("codegen_calls_text", true);
    let dump = Package::load("packages/vesper-place-ir").entry("place_ir_dump", true);
    let (mut same_calls, mut same_fns, mut failures, mut differing) = (0, 0, vec![], vec![]);
    for (library, source, roots) in LIBRARIES {
        let package = scratch.join(format!("pkg-{library}"));
        std::fs::create_dir_all(package.join("src")).unwrap();
        std::fs::write(
            package.join("Loom.toml"),
            format!(
                "[package]\nname = \"@port/{library}\"\nversion = \"0.1.0\"\nedition = \"2026\"\n"
            ),
        )
        .unwrap();
        std::fs::write(package.join("src/lib.ves"), source).unwrap();
        let roots_file = scratch.join(format!("{library}.roots"));
        std::fs::write(&roots_file, roots.join("\n")).unwrap();
        let out = scratch.join(format!("{library}.calls"));
        let status = Command::new(&driver)
            .arg(&package)
            .arg(&roots_file)
            .arg("1")
            .arg(&out)
            .status()
            .unwrap();
        let written = std::fs::read(&out).unwrap_or_default();
        match status.code() {
            Some(0) => {}
            Some(c) => {
                let why = DECLINES.get(c as usize).copied().unwrap_or("?");
                failures.push(format!(
                    "{library}: declined: {why}: {}",
                    String::from_utf8_lossy(&written)
                ));
                continue;
            }
            None => panic!("{library}: the driver was killed"),
        }
        let rust = rust_input(&package, roots);
        let (rust_calls, stage2_calls) = (calls.text(&rust), calls.text(&written));
        if rust_calls != stage2_calls {
            failures.push(format!(
                "{library}: instances or declarations differ\nRust:\n{rust_calls}\nStage 2:\n{stage2_calls}"
            ));
            continue;
        }
        same_calls += 1;
        let rust_fns = functions(&dump.text(&rust));
        let stage2_fns = functions(&dump.text(&written));
        for (name, body) in &rust_fns {
            match stage2_fns.get(name) {
                Some(b) if same_body(body, b) => same_fns += 1,
                Some(_) => differing.push(format!("{library}::{name}")),
                None => failures.push(format!("{library}: Stage 2 did not lower {name}")),
            }
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    eprintln!(
        "stage2 calls inputs: {same_calls} of {} libraries with Rust's instances and \
         declarations; {same_fns} functions lowered as Rust lowers them, {} differ ({})",
        LIBRARIES.len(),
        differing.len(),
        differing.join(", ")
    );
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    assert!(
        same_fns >= MIN_SAME_FUNCTIONS,
        "only {same_fns} functions lowered as Rust lowers them"
    );
}
