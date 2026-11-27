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
    &["--target-features", "avx2"],
    &["--target-features"],
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

/// The name lists of packages/vesper-driver/src/triple.ves: a field's code
/// is its name's index (`arm64` reads as `aarch64`).
const ARCHES: &str = "unknown amdgcn asmjs avr bpfeb bpfel hexagon loongarch64 m68k msp430 nvptx64 pulley32 pulley64 pulley32be pulley64be powerpc powerpc64 powerpc64le s390x sparc sparc64 sparcv9 wasm32 wasm64 x86_64 x86_64h xtensa arm armeb armv4 armv4t armv5t armv5te armv5tej armv6 armv6j armv6k armv6z armv6kz armv6t2 armv6m armv7 armv7a armv7k armv7ve armv7m armv7r armv7s armv8 armv8a armv8.1a armv8.2a armv8.3a armv8.4a armv8.5a armv8m.base armv8m.main armv8r thumbeb thumbv4t thumbv5te thumbv6m thumbv7a thumbv7em thumbv7m thumbv7neon thumbv8m.base thumbv8m.main armebv7r aarch64 arm64 aarch64_be riscv32 riscv32gc riscv32i riscv32im riscv32ima riscv32imac riscv32imafc riscv32imc riscv64 riscv64gc riscv64imac riscv64a23 i386 i586 i686 mips mipsel mipsisa32r6 mipsisa32r6el mips64 mips64el mipsisa64r6 mipsisa64r6el clever clever1.0";
const VENDORS: &str = "unknown amd apple espressif experimental fortanix ibm kmc nintendo nvidia pc rumprun sun uwp wrs";
const OSES: &str = "unknown aix amdhsa bitrig cloudabi cuda cygwin dragonfly emscripten freebsd fuchsia haiku hermit horizon hurd illumos l4re linux nebulet netbsd none openbsd psp redox solaris solid_asp3 uefi vxworks wasi wasip1 wasip2 windows espidf";
const DARWINS: &str = "darwin ios macosx tvos visionos watchos xros";
const ENVS: &str = "unknown amdgiz android androideabi eabi eabihf gnu gnuabi64 gnueabi gnueabihf gnuspe gnux32 gnu_ilp32 gnullvm hermitkernel hurdkernel linuxkernel macabi musl musleabi musleabihf muslabi64 msvc newlib none kernel uclibc uclibceabi uclibceabihf sgx sim softfloat spe threads ohos";
const FORMATS: &str = "unknown elf coff macho wasm xcoff";

fn code_of(name: &str, names: &str) -> u64 {
    names
        .split(' ')
        .position(|n| n == name)
        .unwrap_or_else(|| panic!("{name} is not in the list")) as u64
}

/// `target_lexicon`'s parse as triple.ves's codes.
fn lexicon_words(s: &str) -> Vec<u64> {
    use std::str::FromStr;
    let Ok(t) = target_lexicon::Triple::from_str(s) else {
        return vec![0];
    };
    let arch = t.architecture.to_string();
    let vendor = match t.vendor {
        target_lexicon::Vendor::Custom(_) => 1000,
        v => code_of(&v.to_string(), VENDORS),
    };
    let os = t.operating_system.to_string();
    let os = match DARWINS
        .split(' ')
        .position(|d| t.operating_system.is_like_darwin() && os.starts_with(d))
    {
        Some(k) => 100 + k as u64,
        None => code_of(&os, OSES),
    };
    vec![
        1,
        code_of(&arch, ARCHES),
        vendor,
        os,
        code_of(&t.environment.to_string(), ENVS),
        code_of(&t.binary_format.to_string(), FORMATS),
    ]
}

/// `@vesper/driver::triple` (#1896) reads every target triple as
/// `target_lexicon` 0.13 does: rustc's targets, and triples built from
/// known, misplaced and malformed parts.
#[test]
fn triples_parse_like_target_lexicon() {
    thread_local! {
        static TRIPLE: Entry = driver().entry("triple_words", true);
    }
    let mut corpus: Vec<String> = [
        "x86_64-unknown-linux-gnu",
        "x86_64-pc-windows-msvc",
        "x86_64-pc-windows-gnu",
        "x86_64-pc-windows-gnullvm",
        "aarch64-pc-windows-msvc",
        "i686-pc-windows-msvc",
        "x86_64-uwp-windows-msvc",
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "arm64-apple-macosx11.0.0",
        "x86_64-apple-macosx10.7",
        "aarch64-apple-ios-macabi",
        "aarch64-apple-ios-sim",
        "aarch64-apple-visionos",
        "aarch64-unknown-linux-gnu",
        "riscv64gc-unknown-linux-gnu",
        "wasm32-wasip2",
        "wasm32-unknown-unknown",
        "wasm32-wasi",
        "bpf",
        "bpfel-unknown-none",
        "armv7-unknown-linux-gnueabihf",
        "thumbv7em-none-eabihf",
        "armv6k-nintendo-3ds",
        "riscv32imc-esp-espidf",
        "x86_64-fortanix-unknown-sgx",
        "x86_64-unknown-linux-gnu-elf",
        "x86_64-windows",
        "x86_64-pc-windows-msvc-coff",
        "x86_64-pc-windows-msvc-elf-extra",
        "powerpc64le-unknown-linux-musl",
        "sparcv9-sun-solaris",
        "x86_64-unknown-illumos",
        "aarch64-kmc-solid_asp3",
        "x86_64-unknown-uefi",
        "nvptx64-nvidia-cuda",
        "amdgcn-amd-amdhsa",
        "",
        "-",
        "x86_64-",
        "x86_64--windows",
        "X86_64-pc-windows-msvc",
        "not a triple",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let arches = [
        "x86_64", "aarch64", "arm64", "wasm32", "i686", "armv8.1a", "unknown", "foo", "",
    ];
    let vendors = [
        "pc", "unknown", "apple", "acme", "a.b_9", "Acme", "9x", "windows", "gnu", "",
    ];
    let oses = [
        "windows",
        "linux",
        "macosx10.7",
        "macosx",
        "darwin.",
        "ios1.2.3",
        "ios1.2.3.4",
        "tvos65535",
        "tvos65536",
        "watchos1.256",
        "xros+1",
        "visionos1..2",
        "none",
        "wasip2",
        "",
    ];
    let envs = ["msvc", "gnu", "eabi", "musl", "", "coff"];
    let formats = ["coff", "elf", "macho", "wasm", "x", ""];
    for a in arches {
        for v in vendors {
            for o in oses {
                for e in envs {
                    for f in formats {
                        let parts = [a, v, o, e, f];
                        // Every part, then with each empty part left out
                        // (the usual short forms).
                        corpus.push(parts.join("-"));
                        let short: Vec<&str> =
                            parts.iter().copied().filter(|p| !p.is_empty()).collect();
                        corpus.push(short.join("-"));
                    }
                }
            }
        }
    }
    corpus.sort();
    corpus.dedup();
    let mut differ = vec![];
    for s in &corpus {
        let got = TRIPLE.with(|e| e.words(s.as_bytes()));
        let want = lexicon_words(s);
        if got != want {
            differ.push(format!("{s:?}: vesper {got:?}, target_lexicon {want:?}"));
        }
    }
    eprintln!("triples: {} parsed, {} differ", corpus.len(), differ.len());
    assert!(
        differ.is_empty(),
        "{} of {} triples differ:\n{}",
        differ.len(),
        corpus.len(),
        differ[..differ.len().min(40)].join("\n")
    );
}

#[test]
fn windows_link_matches_rust() {
    use vesper::native::windows::{Linker, LinkerFlavor, WindowsNativeError, link_args};

    let entry = driver().entry("windows_link_check", true);
    let linkers = [
        ("link.exe", 0u64, false),
        (
            "C:\\Program Files\\MSVC\\bin\\HostX64\\x64\\LINK.EXE",
            0u64,
            true,
        ),
        ("/usr/bin/lld-link", 1u64, false),
        ("C:\\LLVM\\bin\\LLD-LINK.exe", 1u64, true),
        ("rust-lld", 2u64, false),
        ("/toolchain/bin/RUST-LLD.EXE", 2u64, false),
        ("custom-ld.exe", 0u64, false),
    ];
    let object_sets: &[&[&str]] = &[&["a.obj"], &["a.obj", "b.obj", "dir/c.obj"]];
    for (linker_path, expected_flavor, windows_backslash) in linkers {
        for objs in object_sets {
            let output = "out/app.exe";
            let mut input = Vec::new();
            input.extend_from_slice(linker_path.as_bytes());
            input.push(0);
            input.extend_from_slice(output.as_bytes());
            input.push(0);
            for (idx, obj) in objs.iter().enumerate() {
                if idx > 0 {
                    input.push(0);
                }
                input.extend_from_slice(obj.as_bytes());
            }

            let words = entry.words(&input);
            let mut at = 1;
            let got_flavor = words[0];
            let got_args = String::from_utf8(unpack(&words, &mut at)).unwrap();
            let got_err_with = String::from_utf8(unpack(&words, &mut at)).unwrap();
            let got_err_none = String::from_utf8(unpack(&words, &mut at)).unwrap();
            assert_eq!(at, words.len());

            assert_eq!(got_flavor, expected_flavor, "flavor for {linker_path}");
            // `std::path::Path::file_stem` treats `\` as a separator only on Windows.
            if !windows_backslash || cfg!(windows) {
                let rust_linker = Linker::explicit(Path::new(linker_path));
                let want_flavor = match rust_linker.flavor {
                    LinkerFlavor::Msvc => 0,
                    LinkerFlavor::LldLink => 1,
                    LinkerFlavor::RustLld => 2,
                };
                assert_eq!(got_flavor, want_flavor, "rust flavor for {linker_path}");
            }

            let obj_paths: Vec<PathBuf> = objs.iter().map(PathBuf::from).collect();
            let mut want_args: Vec<String> = Vec::new();
            if expected_flavor == 2 {
                want_args.push("-flavor".into());
                want_args.push("link".into());
            }
            want_args.extend(
                link_args(&obj_paths, Path::new(output))
                    .into_iter()
                    .map(|s| s.to_string_lossy().into_owned()),
            );
            assert_eq!(got_args, want_args.join("\0"), "args for {linker_path}");

            let want_err_with = WindowsNativeError::LinkerFailed {
                linker: linker_path.into(),
                status: Some(1120),
                output: "LNK1120: unresolved\r\n  ".into(),
            }
            .to_string();
            let want_err_none = WindowsNativeError::LinkerFailed {
                linker: linker_path.into(),
                status: None,
                output: "   \n\t ".into(),
            }
            .to_string();
            assert_eq!(got_err_with, want_err_with);
            assert_eq!(got_err_none, want_err_none);
        }
    }
}

fn encode_diagnostics_stream(ds: &[vesper::ast::Diagnostic]) -> Vec<u8> {
    let mut out = Vec::new();
    let push_u64 = |buf: &mut Vec<u8>, v: u64| buf.extend_from_slice(&v.to_le_bytes());
    let push_str = |buf: &mut Vec<u8>, s: &str| {
        buf.extend_from_slice(&(s.len() as u64).to_le_bytes());
        buf.extend_from_slice(s.as_bytes());
    };
    push_u64(&mut out, ds.len() as u64);
    for d in ds {
        push_str(&mut out, &d.code);
        push_str(&mut out, &d.message);
        push_u64(&mut out, d.span.start as u64);
        push_u64(&mut out, d.span.end as u64);
        push_u64(&mut out, d.related.len() as u64);
        for r in &d.related {
            push_u64(&mut out, r.span.start as u64);
            push_u64(&mut out, r.span.end as u64);
            push_str(&mut out, &r.message);
        }
        match &d.help {
            Some(h) => {
                push_u64(&mut out, 1);
                push_str(&mut out, h);
            }
            None => push_u64(&mut out, 0),
        }
    }
    out
}

fn rust_raw_diagnostics(reason: &str, ds: &[vesper::ast::Diagnostic]) -> String {
    const MAX_RAW_DIAGNOSTICS: usize = 128;
    const MAX_RAW_CODE_CHARS: usize = 128;
    const MAX_RAW_MESSAGE_CHARS: usize = 4096;
    let mut out = format!("diagnostic source rendering unavailable: {reason}\n");
    for d in ds.iter().take(MAX_RAW_DIAGNOSTICS) {
        let code: String = d.code.chars().take(MAX_RAW_CODE_CHARS).collect();
        let message: String = d.message.chars().take(MAX_RAW_MESSAGE_CHARS).collect();
        out.push_str(&format!(
            "{code}: {message} (UTF-8 bytes {}..{})\n",
            d.span.start, d.span.end
        ));
    }
    if ds.len() > MAX_RAW_DIAGNOSTICS {
        out.push_str(&format!(
            "{} additional compiler errors exceed the display profile\n",
            ds.len() - MAX_RAW_DIAGNOSTICS
        ));
    }
    out
}

#[test]
fn diagnostics_format_matches_rust() {
    use vesper::ast::Diagnostic;

    let json_entry = driver().entry("diagnostics_json_words", true);
    let raw_entry = driver().entry("format_raw_diagnostics_words", true);

    let cases: Vec<(&str, Vec<Diagnostic>)> = vec![
        ("empty", vec![]),
        (
            "escapes-and-fields",
            vec![
                Diagnostic::new(
                    "E0001",
                    "bad \"quote\" \\slash\n\r\t\u{0008}\u{000c}\u{0000}\u{001f} λ🚀",
                    0..42,
                )
                .with_related(5..10, "first \"note\"")
                .with_related(12..18, "second\nnote")
                .with_help("try `let x = 1;`"),
                Diagnostic::new("E0002", "plain", 100..200),
            ],
        ),
        (
            "utf8-truncation-and-overflow",
            (0..135usize)
                .map(|i| {
                    if i == 0 {
                        let long_code = "é".repeat(150);
                        let long_msg = "🚀".repeat(4120);
                        Diagnostic::new(&long_code, long_msg, 1..999)
                    } else {
                        Diagnostic::new(&format!("E{i:04}"), format!("msg {i}"), i..i + 3)
                    }
                })
                .collect(),
        ),
    ];

    for (name, ds) in &cases {
        let stream = encode_diagnostics_stream(ds);

        let json_words = json_entry.words(&stream);
        let mut at = 0;
        let got_json = String::from_utf8(unpack(&json_words, &mut at)).unwrap();
        assert_eq!(at, json_words.len(), "{name}: json words");
        let want_json = serde_json::to_string(ds).unwrap();
        assert_eq!(got_json, want_json, "{name}: json mismatch");

        let reason = "source exceeds the 1 MiB diagnostic display profile";
        let mut raw_input = Vec::new();
        raw_input.extend_from_slice(&(reason.len() as u64).to_le_bytes());
        raw_input.extend_from_slice(reason.as_bytes());
        raw_input.extend_from_slice(&stream);
        let raw_words = raw_entry.words(&raw_input);
        let mut at = 0;
        let got_raw = String::from_utf8(unpack(&raw_words, &mut at)).unwrap();
        assert_eq!(at, raw_words.len(), "{name}: raw words");
        let want_raw = rust_raw_diagnostics(reason, ds);
        assert_eq!(got_raw, want_raw, "{name}: raw mismatch");
    }
}

#[test]
fn package_diagnostics_format_matches_rust() {
    use vesper::ast::Diagnostic;

    let pkg_json_entry = driver().entry("package_diagnostics_json_words", true);
    let err_lines_entry = driver().entry("package_error_lines_words", true);

    let entries: Vec<(&str, Diagnostic)> = vec![
        (
            "Loom.toml",
            Diagnostic::new("E0901", "bad manifest \"field\"\nline 2", 0..12),
        ),
        (
            "src/lib.ves",
            Diagnostic::new("E0201", "unknown name 'x'", 10..11)
                .with_related(1..4, "defined here")
                .with_help("did you mean 'y'?"),
        ),
    ];

    let mut stream = Vec::new();
    let push_u64 = |buf: &mut Vec<u8>, v: u64| buf.extend_from_slice(&v.to_le_bytes());
    let push_str = |buf: &mut Vec<u8>, s: &str| {
        buf.extend_from_slice(&(s.len() as u64).to_le_bytes());
        buf.extend_from_slice(s.as_bytes());
    };
    push_u64(&mut stream, entries.len() as u64);
    for (file, d) in &entries {
        push_str(&mut stream, file);
        push_str(&mut stream, &d.code);
        push_str(&mut stream, &d.message);
        push_u64(&mut stream, d.span.start as u64);
        push_u64(&mut stream, d.span.end as u64);
        push_u64(&mut stream, d.related.len() as u64);
        for r in &d.related {
            push_u64(&mut stream, r.span.start as u64);
            push_u64(&mut stream, r.span.end as u64);
            push_str(&mut stream, &r.message);
        }
        match &d.help {
            Some(h) => {
                push_u64(&mut stream, 1);
                push_str(&mut stream, h);
            }
            None => push_u64(&mut stream, 0),
        }
    }

    let words = pkg_json_entry.words(&stream);
    let mut at = 0;
    let got_json = String::from_utf8(unpack(&words, &mut at)).unwrap();
    assert_eq!(at, words.len());

    let want_arr: Vec<serde_json::Value> = entries
        .iter()
        .map(|(file, d)| {
            let mut v = serde_json::to_value(d).unwrap();
            v["file"] = serde_json::Value::String((*file).to_owned());
            v
        })
        .collect();
    let want_json = serde_json::Value::Array(want_arr).to_string();
    assert_eq!(got_json, want_json);

    let mut lines_in = Vec::new();
    push_str(&mut lines_in, "Loom.toml");
    push_str(&mut lines_in, "E0901");
    push_str(&mut lines_in, "invalid version");
    let lwords = err_lines_entry.words(&lines_in);
    let mut lat = 0;
    let got_lines = String::from_utf8(unpack(&lwords, &mut lat)).unwrap();
    assert_eq!(lat, lwords.len());
    assert_eq!(
        got_lines,
        "error[E0901]: invalid version (Loom.toml)\nerror[E0901]: invalid version\n"
    );
}

#[test]
fn diagnostics_remap_matches_rust() {
    use vesper::ast::Diagnostic;
    use vesper::forge_frontend::{ByteRange, MapKind, SourceMapSegment, original_diagnostic};

    let remap_entry = driver().entry("remap_diagnostics_json_words", true);

    let map = vec![
        SourceMapSegment {
            expanded: ByteRange { start: 0, end: 10 },
            original: ByteRange { start: 0, end: 10 },
            kind: MapKind::Source,
            r#macro: None,
        },
        SourceMapSegment {
            expanded: ByteRange { start: 10, end: 40 },
            original: ByteRange { start: 10, end: 18 },
            kind: MapKind::Generated,
            r#macro: Some("expand".into()),
        },
        SourceMapSegment {
            expanded: ByteRange { start: 40, end: 60 },
            original: ByteRange { start: 18, end: 38 },
            kind: MapKind::Source,
            r#macro: None,
        },
    ];

    let ds = vec![
        // Sugar rejection codes must NOT be remapped.
        Diagnostic::new("E0143", "sugar", 12..25).with_related(42..50, "rel"),
        Diagnostic::new("E0144", "sugar2", 15..20),
        // Generated segment overlap (non-empty and empty).
        Diagnostic::new("E0201", "in gen", 15..25).with_related(10..10, "gen start"),
        // Source segment mid, boundary, clamping past original.end, and EOF fallback.
        Diagnostic::new("E0202", "in src", 42..50)
            .with_related(40..45, "boundary")
            .with_related(55..70, "clamped")
            .with_related(60..60, "eof"),
    ];

    let mut input = Vec::new();
    input.extend_from_slice(&(map.len() as u64).to_le_bytes());
    for s in &map {
        input.extend_from_slice(&(s.expanded.start as u64).to_le_bytes());
        input.extend_from_slice(&(s.expanded.end as u64).to_le_bytes());
        input.extend_from_slice(&(s.original.start as u64).to_le_bytes());
        input.extend_from_slice(&(s.original.end as u64).to_le_bytes());
        let kind: u64 = match s.kind {
            MapKind::Source => 0,
            MapKind::Generated => 1,
        };
        input.extend_from_slice(&kind.to_le_bytes());
    }
    input.extend_from_slice(&encode_diagnostics_stream(&ds));

    let words = remap_entry.words(&input);
    let mut at = 0;
    let got_json = String::from_utf8(unpack(&words, &mut at)).unwrap();
    assert_eq!(at, words.len());

    let want_ds: Vec<Diagnostic> = ds
        .into_iter()
        .map(|d| original_diagnostic(&map, d))
        .collect();
    let want_json = serde_json::to_string(&want_ds).unwrap();
    assert_eq!(got_json, want_json);
}
