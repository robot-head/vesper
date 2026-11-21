//! Self-hosting SH5 lane 12 (#1335): `@vesper/driver`'s argument parser
//! and option checks (packages/vesper-driver) against the Rust CLI.
//!
//! Every argument vector of the corpus below (each compile command with
//! every option alone, every ordered pair of options, and the option sets
//! the CLI suites use) is parsed by the Vesper package in the JIT, and the
//! default `vesper` binary runs the same arguments in an empty directory.
//! A Vesper error must be exactly the Rust CLI's `vesper: <message>` on
//! stderr with exit status 1; `--help` must print the CLI's help; options
//! the Vesper parser accepts must get the Rust CLI as far as reading the
//! (missing) input. The checks that need a package directory run against a
//! real one. `with_extension` is compared with `Path::with_extension`.
//!
//! The end-to-end differential (the hybrid binary, whose `main` is the
//! Vesper driver, against the default binary) is
//! crates/vesper-hybrid/tests/driver.rs.

#[path = "common/check_harness.rs"]
mod check_harness;

use check_harness::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn driver() -> &'static Package {
    static PACKAGE: OnceLock<Package> = OnceLock::new();
    PACKAGE.get_or_init(|| Package::load("packages/vesper-driver"))
}

thread_local! {
    /// `parse_text`, compiled once per test thread.
    static PARSE: Entry = driver().entry("parse_text", true);
}

/// Unpack a `pack`ed byte string at `words[*at..]`.
fn unpack(words: &[u64], at: &mut usize) -> Vec<u8> {
    let len = words[*at] as usize;
    *at += 1;
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        out.push((words[*at + i / 8] >> ((i % 8) * 8)) as u8);
    }
    *at += len.div_ceil(8);
    out
}

#[derive(Debug)]
struct Parsed {
    status: u64,
    message: String,
    package_error: String,
    lib_error: String,
    record: Vec<u8>,
}

const OK: u64 = 0;
const ERROR: u64 = 1;
const HELP: u64 = 2;

fn parse(args: &[&str]) -> Parsed {
    let input = args.join("\0");
    let words = PARSE.with(|e| e.words(input.as_bytes()));
    let mut at = 1;
    let message = String::from_utf8(unpack(&words, &mut at)).unwrap();
    let mut p = Parsed {
        status: words[0],
        message,
        package_error: String::new(),
        lib_error: String::new(),
        record: vec![],
    };
    if p.status == OK {
        p.package_error = String::from_utf8(unpack(&words, &mut at)).unwrap();
        p.lib_error = String::from_utf8(unpack(&words, &mut at)).unwrap();
        p.record = unpack(&words, &mut at);
    }
    assert_eq!(at, words.len(), "{args:?}");
    p
}

/// The record's command, fuel and input (`record` in lib.ves).
fn record_fields(record: &[u8]) -> (u64, u64, Option<String>) {
    let word = |i: usize| u64::from_le_bytes(record[i * 8..i * 8 + 8].try_into().unwrap());
    let present = word(4) != 0;
    let len = word(5) as usize;
    let input = present.then(|| String::from_utf8(record[48..48 + len].to_vec()).unwrap());
    (word(0), word(3), input)
}

fn rust_cli(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("run vesper");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// One option each, with and without its value.
const ATOMS: &[&[&str]] = &[
    &["-o", "out.bin"],
    &["-o"],
    &["--emit", "ir"],
    &["--emit", "cst"],
    &["--emit", "object"],
    &["--emit", "clif"],
    &["--emit", "interface"],
    &["--emit", "bogus"],
    &["--emit"],
    &["--entry", "f"],
    &["--entry"],
    &["--include-tests"],
    &["--fuel", "5"],
    &["--fuel", "+7"],
    &["--fuel", "-1"],
    &["--fuel", ""],
    &["--fuel", "+"],
    &["--fuel", "18446744073709551615"],
    &["--fuel", "18446744073709551616"],
    &["--fuel"],
    &["--json"],
    &["--no-opt"],
    &["--verify-policy", "p.json"],
    &["--verify-policy"],
    &["--tee"],
    &["--codegen", "verified"],
    &["--codegen", "other"],
    &["--codegen"],
    &["--target", "bpf"],
    &["--target", "wasm32-wasip2"],
    &["--target"],
    &["--linker", "ld"],
    &["--linker"],
    &["--link-search", "lib"],
    &["--link-search"],
    &["--package", "@a/b"],
    &["--package"],
    &["--verify-stack"],
    &["--verify-const-time"],
    &["--verify-detfloat"],
    &["--verify-bogus"],
    &["--verify=all"],
    &["--verify=stack, realtime"],
    &["--verify=\u{3000}detfloat\u{a0}"],
    &["--verify=\u{2028}const-time\u{85},\u{1680}stack\u{202f}"],
    &["--verify="],
    &["--verify=stack,"],
    &["--verify=nope"],
    &["--verify"],
    &["--lib"],
    &["--lib-name", "x"],
    &["--lib-name"],
    &["--interface", "i.json"],
    &["--interface"],
    &["--rust-binding", "b.rs"],
    &["--rust-binding"],
    &["--rust-types", "t"],
    &["--rust-types"],
    &["--instance", "f[u64]"],
    &["--instance"],
    &["--help"],
    &["-h"],
    &["-"],
    &["-x"],
    &["--unknown"],
    &["--"],
    &["second.ves"],
];

const COMMANDS: &[&str] = &["check", "run", "jit", "build", "emit"];

/// The input; it does not exist in the Rust CLI's directory.
const INPUT: &str = "missing.ves";

/// Option sets of the CLI suites (tests/cli*.rs).
const SUITE_SETS: &[&[&str]] = &[
    &[
        "build",
        "--lib",
        "--lib-name",
        "pkg",
        "-o",
        "x.o",
        "--rust-binding",
        "b.rs",
    ],
    &[
        "build",
        "--lib",
        "-o",
        "x.o",
        "--interface",
        "i.json",
        "--rust-types",
        "crate::t",
        "--rust-binding",
        "b.rs",
    ],
    &[
        "build",
        "-o",
        "a.out",
        "--verify=const-time,stack",
        "--verify-policy",
        "p.json",
    ],
    &[
        "build",
        "--codegen",
        "verified",
        "-o",
        "bin",
        "--target",
        "x86_64-unknown-linux-gnu",
    ],
    &[
        "emit",
        "--emit",
        "object",
        "-o",
        "x.o",
        "--target",
        "aarch64-apple-darwin",
    ],
    &[
        "emit",
        "--emit",
        "interface",
        "--package",
        "@org/name",
        "-o",
        "i.json",
    ],
    &["run", "--fuel", "100", "--include-tests", "--entry", "t"],
    &["jit", "--no-opt", "--json", "--include-tests"],
    &["build", "--target", "wasm32-wasip2", "-o", "a.wasm"],
    &["build", "--tee", "-o", "a.out"],
    &[
        "build",
        "--link-search",
        "a",
        "--link-search",
        "b",
        "--linker",
        "clang",
    ],
    &[
        "build",
        "--lib",
        "-o",
        "x.o",
        "--instance",
        "f[u64]",
        "--instance",
        "g[i32]",
    ],
];

fn compare(dir: &Path, args: &[&str], help: &str) -> Result<(), String> {
    let p = parse(args);
    let (code, stdout, stderr) = rust_cli(dir, args);
    let shown = format!(
        "{args:?}: vesper parse {p:?}; rust exit {code}, stdout {stdout:?}, stderr {stderr:?}"
    );
    match p.status {
        ERROR => {
            if code != 1 || !stdout.is_empty() || stderr != format!("vesper: {}\n", p.message) {
                return Err(shown);
            }
        }
        HELP => {
            if code != 0 || stdout != help || !stderr.is_empty() {
                return Err(shown);
            }
        }
        OK => {
            let (command, _, input) = record_fields(&p.record);
            let expected = COMMANDS.iter().position(|c| *c == args[0]).unwrap() as u64 + 1;
            let named = input.as_deref().is_some_and(|i| args.contains(&i));
            if command != expected || !named {
                return Err(format!(
                    "{shown}: record command {command}, input {input:?}"
                ));
            }
            let input = input.unwrap();
            if code != 1 || !stderr.starts_with(&format!("vesper: {input}: ")) {
                return Err(shown);
            }
        }
        _ => return Err(shown),
    }
    Ok(())
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vesper-self-host-driver-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn argument_errors_match_the_rust_cli() {
    let dir = scratch("args");
    let help = rust_cli(&dir, &["--help"]).1;
    let mut cases: Vec<Vec<&str>> = vec![];
    for c in COMMANDS {
        cases.push(vec![c]);
        cases.push(vec![c, INPUT]);
        for a in ATOMS {
            let mut v = vec![*c];
            v.extend_from_slice(a);
            cases.push(v.clone());
            v.push(INPUT);
            cases.push(v);
            let mut v = vec![*c, INPUT];
            v.extend_from_slice(a);
            cases.push(v);
        }
    }
    // Every ordered pair of options, each command in turn.
    for (i, a) in ATOMS.iter().enumerate() {
        for (j, b) in ATOMS.iter().enumerate() {
            let c = COMMANDS[(i + j) % COMMANDS.len()];
            let mut v = vec![c];
            v.extend_from_slice(a);
            v.extend_from_slice(b);
            v.push(INPUT);
            cases.push(v);
        }
    }
    for s in SUITE_SETS {
        let mut v = s.to_vec();
        v.push(INPUT);
        cases.push(v);
    }
    let failures: Vec<String> = cases
        .iter()
        .filter_map(|args| compare(&dir, args, &help).err())
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        failures.is_empty(),
        "{} of {} argument vectors differ:\n{}",
        failures.len(),
        cases.len(),
        failures[..failures.len().min(20)].join("\n")
    );
    eprintln!("{} argument vectors agree", cases.len());
}

#[test]
fn records_carry_the_options() {
    for (args, fuel) in [
        (&["run", "a.ves"][..], 10_000_000),
        (&["run", "--fuel", "5", "a.ves"], 5),
        (&["run", "--fuel", "+7", "a.ves"], 7),
        (
            &[
                "run",
                "--fuel",
                "1",
                "--fuel",
                "18446744073709551615",
                "a.ves",
            ],
            u64::MAX,
        ),
    ] {
        let p = parse(args);
        assert_eq!(p.status, OK, "{args:?}");
        assert_eq!(
            record_fields(&p.record),
            (2, fuel, Some("a.ves".to_owned())),
            "{args:?}"
        );
    }
}

#[test]
fn package_checks_match_the_rust_cli() {
    let dir = scratch("package");
    std::fs::create_dir_all(dir.join("pkg/src")).unwrap();
    std::fs::write(
        dir.join("pkg/Loom.toml"),
        "[package]\nname = \"@t/pkg\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("pkg/src/lib.ves"),
        "fn main() -> i64 {\n    0\n}\n",
    )
    .unwrap();
    let cases: &[&[&str]] = &[
        &["emit", "--emit", "cst", "pkg"],
        &["build", "--target", "bpf", "pkg"],
        &["build", "--target", "bpf", "--emit", "cst", "pkg"],
    ];
    for args in cases {
        let p = parse(args);
        assert_eq!(p.status, OK, "{args:?}: {p:?}");
        assert!(!p.package_error.is_empty(), "{args:?}");
        let (code, stdout, stderr) = rust_cli(&dir, args);
        assert_eq!(
            (code, stdout.as_str(), stderr),
            (1, "", format!("vesper: {}\n", p.package_error)),
            "{args:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn object_paths_match_path_with_extension() {
    let paths = [
        "a.out",
        "a",
        "dir/a.out",
        "dir/",
        "dir//",
        "dir/.",
        "dir/./",
        "./a",
        ".",
        "./",
        "..",
        "a/..",
        "/",
        "",
        ".hidden",
        ".hidden.x",
        "a.b.c",
        "a/b.",
        "a//b",
        "/abs/x.y",
        "x/./.",
        "a./",
        "..x",
        "...",
        "a b/c d.e",
        "dir.d/file",
        "dir.d/",
        "dir.d/.",
        "dir.d//",
        "x/..x",
        "a..b",
        ".x.y",
        "x.y/./",
        "é/ü.ö",
    ];
    let entry = driver().entry("with_extension_text", true);
    for p in paths {
        let windows = cfg!(windows);
        let mut input = String::from(if windows { "w" } else { "u" });
        input.push_str(p);
        let words = entry.words(input.as_bytes());
        let mut at = 0;
        let got = String::from_utf8(unpack(&words, &mut at)).unwrap();
        let want = Path::new(p).with_extension("vesper.o");
        assert_eq!(got, want.to_str().unwrap(), "{p:?}");
    }
}
