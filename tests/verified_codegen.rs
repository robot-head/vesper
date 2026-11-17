//! Differential tests of the direct verified code generator (§42.1 pillar 3b)
//! against the evaluator and the Cranelift native backend.
//!
//! x86-64 Linux executables run natively on x86-64 Linux hosts. AArch64
//! Linux executables run natively on AArch64 Linux, or under
//! `qemu-aarch64`/`qemu-aarch64-static` when present; AArch64 Mach-O objects
//! are linked with `cc` and run on Apple silicon. Anything else is reported
//! as unavailable on stderr, never as a pass of that engine.
use std::path::{Path, PathBuf};
use std::process::Command;
use vesper::check::World;
use vesper::verified_codegen::{
    self as vc, Artifact, Container, Options, Outcome, ResultProtocol, Target, Unsupported,
    VerifiedError,
};
use vesper::{check, native, parser, runtime::Runtime};

fn world(source: &str) -> World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    c.world
}

fn scratch() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = std::sync::atomic::AtomicU64::new(0);
    let _ = n;
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "vesper-verified-codegen-{}-{nanos}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// How an artifact for `target` can be run here, if at all.
fn runner(target: Target) -> Option<Vec<PathBuf>> {
    let host = Target::host();
    if host == Some(target) {
        return Some(vec![]);
    }
    if target == Target::X86_64_WINDOWS && cfg!(target_os = "linux") {
        // Link with MinGW and run under Wine when both are installed.
        return match (on_path("x86_64-w64-mingw32-gcc"), on_path("wine")) {
            (Some(_), Some(wine)) => Some(vec![wine]),
            _ => None,
        };
    }
    let qemu: &[&str] = if target == Target::AARCH64_LINUX {
        &["qemu-aarch64", "qemu-aarch64-static"]
    } else if target == Target::RISCV64_LINUX {
        &["qemu-riscv64", "qemu-riscv64-static"]
    } else {
        &[]
    };
    if cfg!(target_os = "linux") {
        for &q in qemu {
            if let Some(q) = on_path(q) {
                return Some(vec![q]);
            }
        }
    }
    None
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}
#[cfg(not(unix))]
fn make_executable(_: &Path) {}

fn emitted_output(cmd: &mut Command) -> std::io::Result<std::process::Output> {
    let started = std::time::Instant::now();
    loop {
        match cmd.output() {
            // A concurrent fork can briefly inherit another test's writable
            // executable descriptor before exec closes it. Retry only the
            // Linux launch error, never an executable's nonzero exit status.
            Err(error)
                if cfg!(target_os = "linux")
                    && error.raw_os_error() == Some(26)
                    && started.elapsed() < std::time::Duration::from_secs(1) =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            result => return result,
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn emitted_launch_retries_temporary_text_busy_but_preserves_failures() {
    use std::io::{Read, Write};
    use std::process::Stdio;

    let dir = scratch();
    let exe = dir.join("program");
    // Keep the writable descriptor in a separate process so concurrent test
    // forks cannot inherit it from this process and extend the busy interval.
    let mut writer = Command::new("/bin/sh")
        .args([
            "-c",
            "exec 3>\"$1\"; printf '%s' \"$2\" >&3; printf ready; read -r release",
            "held-writer",
        ])
        .arg(&exe)
        .arg("#!/bin/sh\nprintf launched\nexit 17\n")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut ready = [0; 5];
    writer
        .stdout
        .take()
        .unwrap()
        .read_exact(&mut ready)
        .unwrap();
    assert_eq!(&ready, b"ready");
    make_executable(&exe);
    let busy = emitted_output(&mut Command::new(&exe)).unwrap_err();
    assert_eq!(busy.raw_os_error(), Some(26));

    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        writer
            .stdin
            .take()
            .unwrap()
            .write_all(b"release\n")
            .unwrap();
        assert!(writer.wait().unwrap().success());
    });
    let out = emitted_output(&mut Command::new(&exe)).unwrap();
    release.join().unwrap();
    assert_eq!(out.stdout, b"launched");
    assert_eq!(out.status.code(), Some(17));
    let missing = emitted_output(&mut Command::new(dir.join("missing"))).unwrap_err();
    assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Run an artifact built with `ResultProtocol::Words`.
fn execute(artifact: &Artifact, prefix: &[PathBuf]) -> Outcome {
    let dir = scratch();
    // Windows starts executables by their `.exe` name.
    let exe = dir.join(if artifact.container == Container::CoffObject {
        "program.exe"
    } else {
        "program"
    });
    match artifact.container {
        Container::ElfExecutable => {
            std::fs::write(&exe, &artifact.bytes).unwrap();
            make_executable(&exe);
        }
        Container::MachOObject => {
            let object = dir.join("program.o");
            std::fs::write(&object, &artifact.bytes).unwrap();
            let status = Command::new("cc")
                .arg(&object)
                .arg("-o")
                .arg(&exe)
                .status()
                .expect("spawn cc");
            assert!(status.success(), "cc failed to link the Mach-O object");
        }
        Container::CoffObject => {
            let object = dir.join("program.obj");
            std::fs::write(&object, &artifact.bytes).unwrap();
            if cfg!(windows) {
                vesper::native::windows::link_executable(&object, &exe, None)
                    .expect("link.exe links the COFF object");
            } else {
                let status = Command::new("x86_64-w64-mingw32-gcc")
                    .arg(&object)
                    .arg("-o")
                    .arg(&exe)
                    .status()
                    .expect("spawn x86_64-w64-mingw32-gcc");
                assert!(status.success(), "MinGW failed to link the COFF object");
            }
        }
    }
    let mut cmd = match prefix.split_first() {
        Some((q, rest)) => {
            let mut c = Command::new(q);
            c.args(rest).arg(&exe);
            c
        }
        None => Command::new(&exe),
    };
    if artifact.container == Container::CoffObject && !prefix.is_empty() {
        // Wine: a private prefix outside the home directory, no debug noise.
        cmd.env(
            "WINEPREFIX",
            std::env::temp_dir().join("vesper-verified-wine"),
        )
        .env("WINEDEBUG", "-all");
    }
    let out = emitted_output(&mut cmd).expect("run the emitted executable");
    let _ = std::fs::remove_dir_all(&dir);
    Outcome::from_exit(out.status.code(), &out.stdout)
}

/// `source` plus an entry that runs `main` under handlers for both failures
/// and returns 0 (no failure), 1 (`ArithOverflow`) or 2 (division by zero).
fn status_world(source: &str) -> World {
    world(&format!(
        "{source}\nfn verified_codegen_status() -> u8 {{ handle Fail[ArithOverflow] in {{ handle Fail[RuntimeError] in {{ let _ = main(); 0 }} with {{ return value => value, raise(_, _) => 2, }} }} with {{ return value => value, raise(_, _) => 1, }} }}"
    ))
}
fn status_name(status: &str) -> Option<&'static str> {
    match status {
        "0" => None,
        "1" => Some("overflow"),
        "2" => Some("div0"),
        other => panic!("status {other}"),
    }
}

/// The evaluator's outcome, rendered: a value, `overflow` or `div0`.
fn evaluate(source: &str, world: &World) -> String {
    let mut runtime = Runtime::new(world);
    runtime.fuel = 2_000_000_000;
    match runtime.run("main", vec![]) {
        Ok(v) => v.to_string(),
        Err(e) if e.contains("region bound exhausted") => "region".into(),
        Err(e) if e.contains("unhandled effect Fail") => {
            let w = status_world(source);
            let mut runtime = Runtime::new(&w);
            runtime.fuel = 2_000_000_000;
            let status = runtime.run("verified_codegen_status", vec![]).unwrap();
            status_name(&status.to_string())
                .expect("the evaluator failed without a handled failure")
                .into()
        }
        Err(e) => panic!("evaluator error: {e}"),
    }
}

/// The Cranelift JIT's outcome. Cranelift requires a total entry, so an
/// entry with `Fail` effects runs under the status wrapper, and, when it
/// succeeds with an `i64`, under a wrapper that returns the value. `None`
/// when only "no failure" is known.
fn cranelift(source: &str, world: &World) -> Option<String> {
    let main = &world.functions["main"];
    if main.effects.instances.is_empty() {
        return Some(native::jit(world, "main").unwrap().to_string());
    }
    let w = status_world(source);
    let status = native::jit(&w, "verified_codegen_status")
        .unwrap()
        .to_string();
    if let Some(failure) = status_name(&status) {
        return Some(failure.into());
    }
    if main.result.name() != "i64" {
        return None;
    }
    let w = self::world(&format!(
        "{source}\nfn verified_codegen_value() -> i64 {{ handle Fail[ArithOverflow] in {{ handle Fail[RuntimeError] in {{ main() }} with {{ return value => value, raise(_, _) => 0, }} }} with {{ return value => value, raise(_, _) => 0, }} }}"
    ));
    Some(
        native::jit(&w, "verified_codegen_value")
            .unwrap()
            .to_string(),
    )
}

fn render(world: &World, outcome: &Outcome) -> String {
    match outcome {
        Outcome::Words(words) => vc::decode(world, &world.functions["main"].result, words)
            .unwrap()
            .to_string(),
        Outcome::Overflow => "overflow".into(),
        Outcome::DivisionByZero => "div0".into(),
        Outcome::Region => "region".into(),
        other => panic!("unexpected outcome {other:?}"),
    }
}

const TARGETS: [Target; 5] = [
    Target::X86_64_LINUX,
    Target::AARCH64_LINUX,
    Target::AARCH64_MACOS,
    Target::RISCV64_LINUX,
    Target::X86_64_WINDOWS,
];

/// Compile for every target (with and without the IR optimizer), check the
/// output is deterministic, run what can run here, and compare with the
/// evaluator and Cranelift. Returns the rendered outcome.
fn differential(source: &str) -> String {
    differential_with(source, true)
}

/// [`differential`], optionally without the Cranelift JIT: Cranelift
/// specializes region-polymorphic functions inline and refuses recursion
/// through them ("recursive effect specialization exceeds 64 inline
/// frames"), which the verified backend compiles as ordinary calls.
fn differential_with(source: &str, with_cranelift: bool) -> String {
    let w = world(source);
    let expected = evaluate(source, &w);
    if with_cranelift {
        match cranelift(source, &w) {
            Some(jit) => assert_eq!(jit, expected, "Cranelift JIT disagrees with the evaluator"),
            None => assert!(
                !matches!(expected.as_str(), "overflow" | "div0"),
                "Cranelift JIT succeeded where the evaluator failed"
            ),
        }
    }
    for target in TARGETS {
        for optimize in [true, false] {
            let options = Options {
                protocol: ResultProtocol::Words,
                optimize,
                ..Options::new(target)
            };
            let artifact = match vc::compile(&w, "main", &options) {
                Ok(a) => a,
                // Only the Linux ELF executable maps the region heap.
                Err(VerifiedError::Target(e))
                    if target.os != vc::Os::Linux && e.contains("program heap") =>
                {
                    continue;
                }
                Err(e) => panic!("{target:?}: {e}\n{source}"),
            };
            let again = vc::compile(&w, "main", &options).unwrap();
            assert_eq!(artifact.bytes, again.bytes, "nondeterministic output");
            match runner(target) {
                Some(prefix) => {
                    let got = render(&w, &execute(&artifact, &prefix));
                    assert_eq!(
                        got, expected,
                        "{target:?} (optimize {optimize}) disagrees with the evaluator\n{source}"
                    );
                }
                None => {
                    if optimize {
                        eprintln!("{target:?}: no native host or QEMU here; run unavailable");
                    }
                }
            }
        }
    }
    expected
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/verified_codegen")
            .join(name),
    )
    .unwrap()
}

/// Rendered by the evaluator; the tests check the other engines agree.
const ENUMS: &str = "(110, 5, 6, -1, Some(42), Rect(3, 5))";
/// `tiny / 1e17` is the subnormal 4.9e-322: 321 zeros after the point.
fn floats_expected() -> String {
    format!(
        "(5.775, 0.9999999999999999, -0, NaN, 0.{}49, 0.30000000000000004, (false, true, false, false, false, false), (true, false, false, true, false, true), true, -inf)",
        "0".repeat(321)
    )
}
const LITERALS: &str = "(100, 200, 300, 400, 500, Ok(42), Err(Halt), (-2, -1, 0))";

#[test]
fn fixtures_agree_with_the_evaluator_and_cranelift() {
    for (name, expected) in [
        ("fibonacci.ves", "6765"),
        ("loop.ves", "15007"),
        (
            "structs.ves",
            "Segment { closed: true, from: Point { x: 25, y: 6 }, to: Point { x: 1, y: 2 } }",
        ),
        ("inout.ves", "(2, 22, 42)"),
        // Disjoint projections of one place: each copy-back is partial.
        ("projections.ves", "(21, 21)"),
        ("kinds.ves", "(241, 4147483647, 1, (-3, -1, true, false))"),
        ("overflow.ves", "overflow"),
        ("divide.ves", "div0"),
        ("unit.ves", "()"),
        ("enums.ves", ENUMS),
        ("literals.ves", LITERALS),
        ("nested.ves", "(Some(Some(1)), Some(None), 9, 3, 4)"),
        ("floats.ves", floats_expected().as_str()),
    ] {
        assert_eq!(differential(&fixture(name)), expected, "{name}");
    }
}

/// `vesper.arith.checked/1` boundaries for every kind and operator.
#[test]
fn checked_arithmetic_boundaries() {
    let max = |k: &str| match k {
        "u8" => "255",
        "u32" => "4294967295",
        "u64" => "18446744073709551615",
        _ => "9223372036854775807",
    };
    let mut cases = vec![];
    for k in ["u8", "u32", "u64", "i64"] {
        let m = max(k);
        for (a, op, b) in [
            (m, "+", "0"),
            (m, "+", "1"),
            ("1", "-", "1"),
            ("0", "-", "1"),
            (m, "*", "1"),
            (m, "*", "2"),
            ("7", "/", "2"),
            ("7", "/", "0"),
            ("7", "%", "3"),
            ("7", "%", "0"),
        ] {
            cases.push(format!(
                "fn op(a: {k}, b: {k}) -> {k} eff [Fail[ArithOverflow], Fail[RuntimeError]] {{ a {op} b }}\nfn main() -> {k} eff [Fail[ArithOverflow], Fail[RuntimeError]] {{ op({a}, {b}) }}"
            ));
        }
        cases.push(format!(
            "fn neg(a: {k}) -> {k} eff [Fail[ArithOverflow]] {{ -a }}\nfn main() -> {k} eff [Fail[ArithOverflow]] {{ neg(0) }}"
        ));
        cases.push(format!(
            "fn neg(a: {k}) -> {k} eff [Fail[ArithOverflow]] {{ -a }}\nfn main() -> {k} eff [Fail[ArithOverflow]] {{ neg(1) }}"
        ));
        for op in ["wrapping_add", "wrapping_sub", "wrapping_mul"] {
            cases.push(format!(
                "fn op(a: {k}, b: {k}) -> {k} {{ {op}(a, b) }}\nfn main() -> {k} {{ op({m}, 3) }}"
            ));
        }
        for cmp in ["<", "<=", ">", ">=", "==", "!="] {
            cases.push(format!(
                "fn c(a: {k}, b: {k}) -> (bool, bool, bool) {{ (a {cmp} b, b {cmp} a, a {cmp} a) }}\nfn main() -> (bool, bool, bool) {{ c({m}, 1) }}"
            ));
        }
    }
    for (a, op, b) in [
        ("-9223372036854775808", "/", "-1"),
        ("-9223372036854775808", "%", "-1"),
        ("-9223372036854775808", "-", "1"),
        ("-9223372036854775808", "*", "-1"),
        ("-7", "/", "2"),
        ("-7", "%", "2"),
        ("7", "%", "-2"),
        ("-3037000500", "*", "3037000500"),
        ("3037000499", "*", "3037000499"),
    ] {
        cases.push(format!(
            "fn op(a: i64, b: i64) -> i64 eff [Fail[ArithOverflow], Fail[RuntimeError]] {{ a {op} b }}\nfn main() -> i64 eff [Fail[ArithOverflow], Fail[RuntimeError]] {{ op({a}, {b}) }}"
        ));
    }
    cases.push(
        "fn neg(a: i64) -> i64 eff [Fail[ArithOverflow]] { -a }\nfn main() -> i64 eff [Fail[ArithOverflow]] { neg(-9223372036854775808) }".into(),
    );
    cases.push(
        "fn main() -> i64 eff [Fail[ArithOverflow]] { let x = 9223372036854775806; let y = x; y += 1; y += 1; y }".into(),
    );
    assert!(cases.len() > 90);
    for case in &cases {
        differential(case);
    }
}

fn rejects(source: &str, construct: Unsupported) {
    let w = world(source);
    for target in TARGETS {
        match vc::compile(&w, "main", &Options::new(target)) {
            Err(VerifiedError::Unsupported { construct: c, .. }) if c == construct => {}
            other => panic!("expected {construct:?}, got {other:?}\n{source}"),
        }
    }
}

#[test]
fn constructs_outside_the_subset_are_typed_errors() {
    rejects(
        "fn r(a: f64, b: f64) -> f64 { a % b } fn main() -> f64 { r(1.5, 1.0) }",
        Unsupported::Float,
    );
    rejects("fn main() -> i64 { let s = \"x\"; 1 }", Unsupported::String);
    rejects(
        "enum L { Nil, Cons(i64, L) } fn main() -> i64 { match L::Nil { L::Nil => 0, L::Cons(x, _) => x, } }",
        Unsupported::Enum,
    );
    rejects(
        "fn main() -> i64 { match \"a\" { \"a\" => 1, _ => 0, } }",
        Unsupported::String,
    );
    rejects(
        "fn main() -> i64 { match 1.5 { 1.5 => 1, _ => 0, } }",
        Unsupported::Float,
    );
    rejects(
        "fn main() -> i64 { region r { let m = r.checkpoint(); r.commit(m); 1 } }",
        Unsupported::Effect,
    );
    // Nothing supplies the entry's region capabilities.
    rejects(
        "fn main() -> i64 eff [r: Region] { let b = r.box(1); r.get(b) }",
        Unsupported::Effect,
    );
    // Every `inout` argument is checked, not only the first.
    rejects(
        "fn both(inout a: i64, inout b: i64) { a = wrapping_add(a, 1); b = wrapping_add(b, 1); } \
         fn main() -> i64 { let y = 2; region r(64) { let z = 0; both(inout z, inout y); wrapping_add(z, y) } }",
        Unsupported::Effect,
    );
    rejects(
        "fn main() -> i64 { let f = |x: i64| -> i64 { x }; f(1) }",
        Unsupported::FunctionValue,
    );
    // A tail-resumptive handler compiles (milestone 11); one that resumes
    // other than in tail position does not.
    rejects(
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle Ask in { Ask::ask() } with { return x => x, ask(k) => wrapping_add(k(21), 1), } }",
        Unsupported::Effect,
    );
    rejects(
        "copy struct P { x: i64 } fn main() -> bool { let a = P { x: 1 }; a == a }",
        Unsupported::StructuralEquality,
    );
}

/// Dynamic tuple indexing (the front end's byte tables): every element, the
/// first and last, out of range on both sides, multi-word elements and
/// unsigned indices.
#[test]
fn dynamic_tuple_indexing() {
    let table = "fn t(i: i64) -> i64 eff [Fail[RuntimeError]] { (7, 11, 13, 17, 19)[i] }";
    for i in [
        "0",
        "1",
        "4",
        "5",
        "-1",
        "-9223372036854775808",
        "9223372036854775807",
    ] {
        differential(&format!(
            "{table}\nfn main() -> i64 eff [Fail[RuntimeError]] {{ t({i}) }}"
        ));
    }
    assert_eq!(
        differential(&format!(
            "{table}\nfn sum(i: i64, acc: i64) -> i64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {{ if i == 5 {{ acc }} else {{ sum(i + 1, acc * 3 + t(i)) }} }}\nfn main() -> i64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {{ sum(0, 0) }}"
        )),
        "1051"
    );
    assert_eq!(
        differential(
            "copy struct P { x: i64, y: bool }\nfn pick(i: u8) -> P eff [Fail[RuntimeError]] { (P { x: 1, y: true }, P { x: 2, y: false }, P { x: 3, y: true })[i] }\nfn main() -> (P, P) eff [Fail[RuntimeError]] { (pick(2), pick(1)) }"
        ),
        "(P { x: 3, y: true }, P { x: 2, y: false })"
    );
    assert_eq!(
        differential(
            "fn pick(i: u64) -> (i64, i64) eff [Fail[RuntimeError]] { ((1, 2), (3, 4))[i] }\nfn main() -> (i64, i64) eff [Fail[RuntimeError]] { pick(2) }"
        ),
        "div0"
    );
    // A handled out-of-range index is `RuntimeError::IndexOutOfBounds`.
    assert_eq!(
        differential(&format!(
            "{table}\nfn caught(i: i64) -> i64 {{ handle Fail[RuntimeError] in {{ t(i) }} with {{ return v => v, raise(e, r) => match e {{ RuntimeError::IndexOutOfBounds => -1, _ => -2, }}, }} }}\nfn main() -> (i64, i64, i64) {{ (caught(3), caught(5), caught(-1)) }}"
        )),
        "(17, -1, -1)"
    );
}

#[test]
fn entry_and_target_errors() {
    let w = world("fn f(x: i64) -> i64 { x } fn main() -> i64 { f(1) }");
    assert!(matches!(
        vc::compile(&w, "f", &Options::new(Target::X86_64_LINUX)),
        Err(VerifiedError::Entry(_))
    ));
    assert!(matches!(
        vc::compile(&w, "nope", &Options::new(Target::X86_64_LINUX)),
        Err(VerifiedError::Entry(_))
    ));
    assert_eq!(
        Target::from_triple("riscv64gc-unknown-linux-gnu").unwrap(),
        Target::RISCV64_LINUX
    );
    assert!(Target::from_triple("riscv64gc-apple-darwin").is_err());
    assert_eq!(
        Target::from_triple("x86_64-pc-windows-msvc").unwrap(),
        Target::X86_64_WINDOWS
    );
    assert!(Target::from_triple("aarch64-pc-windows-msvc").is_err());
    assert!(Target::from_triple("powerpc64le-unknown-linux-gnu").is_err());
    assert!(Target::from_triple("x86_64-apple-darwin").is_err());
    assert_eq!(
        Target::from_triple("aarch64-apple-darwin").unwrap(),
        Target::AARCH64_MACOS
    );
    assert_eq!(
        Target::from_triple("x86_64-unknown-linux-gnu").unwrap(),
        Target::X86_64_LINUX
    );
    let small = Options {
        max_code_bytes: 64,
        ..Options::new(Target::X86_64_LINUX)
    };
    assert!(matches!(
        vc::compile(&world(&fixture("fibonacci.ves")), "main", &small),
        Err(VerifiedError::Limit(_))
    ));
}

#[test]
fn containers_are_well_formed() {
    let w = world(&fixture("structs.ves"));
    let x86 = vc::compile(&w, "main", &Options::new(Target::X86_64_LINUX)).unwrap();
    let arm = vc::compile(&w, "main", &Options::new(Target::AARCH64_LINUX)).unwrap();
    let rv = vc::compile(&w, "main", &Options::new(Target::RISCV64_LINUX)).unwrap();
    assert_eq!(rv.code_len % 4, 0);
    for (a, machine) in [(&x86, 62u16), (&arm, 183), (&rv, 243)] {
        assert_eq!(&a.bytes[..4], b"\x7fELF");
        assert_eq!(a.bytes[4], 2, "ELFCLASS64");
        assert_eq!(u16::from_le_bytes([a.bytes[16], a.bytes[17]]), 2, "ET_EXEC");
        assert_eq!(u16::from_le_bytes([a.bytes[18], a.bytes[19]]), machine);
        assert_eq!(a.bytes.len(), 0x80 + a.code_len);
    }
    assert_eq!(arm.code_len % 4, 0);
    let mac = vc::compile(&w, "main", &Options::new(Target::AARCH64_MACOS)).unwrap();
    assert_eq!(mac.container, Container::MachOObject);
    assert_eq!(&mac.bytes[..4], &0xfeed_facfu32.to_le_bytes());
    assert_eq!(&mac.bytes[4..8], &0x0100_000cu32.to_le_bytes());
    assert!(mac.bytes.windows(6).any(|w| w == b"_main\0"));
    // The COFF object: AMD64, one RUNTIME_FUNCTION and UNWIND_INFO per
    // compiled function plus `main`, every `.pdata` field relocated.
    let win = vc::compile(&w, "main", &Options::new(Target::X86_64_WINDOWS)).unwrap();
    assert_eq!(win.container, Container::CoffObject);
    let coff = vesper::native::windows::inspect(&win.bytes).unwrap();
    assert_eq!(coff.machine, 0x8664);
    assert_eq!(coff.sections, [".text", ".xdata", ".pdata"]);
    assert_eq!(coff.runtime_functions, win.lir.len() + 1);
    assert_eq!(coff.unwind_infos, coff.runtime_functions);
    assert_eq!(coff.pdata_relocations, 3 * coff.runtime_functions);
    assert_eq!(coff.external_functions, ["main"]);
    // The slot IR and the machine instruction count are reported.
    assert!(x86.lir.iter().any(|(n, _)| n == "length2"));
    assert!(x86.machine_instructions > 50);
}

/// The x86-64 encodings disassemble, when GNU objdump is available.
#[test]
fn x86_64_code_disassembles() {
    let Some(objdump) = on_path("objdump") else {
        eprintln!("objdump unavailable; disassembly check skipped");
        return;
    };
    let w = world(&fixture("kinds.ves"));
    let a = vc::compile(&w, "main", &Options::new(Target::X86_64_LINUX)).unwrap();
    let dir = scratch();
    let raw = dir.join("code.bin");
    std::fs::write(&raw, &a.bytes[0x80..]).unwrap();
    let out = Command::new(objdump)
        .args(["-D", "-b", "binary", "-m", "i386:x86-64"])
        .arg(&raw)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() || !text.contains("Disassembly") {
        eprintln!("objdump cannot disassemble x86-64 here; check skipped");
        return;
    }
    assert!(!text.contains("(bad)"), "{text}");
    for needle in ["idiv", "mul", "syscall", "jo "] {
        assert!(text.contains(needle), "missing {needle}:\n{text}");
    }
}

/// A self call in tail position is a jump: ten million iterations run in
/// constant stack (the evaluator is not run here; it recurses).
#[test]
fn tail_self_calls_are_loops() {
    let source = fixture("loop.ves").replace("count(5000, 7)", "count(10000000, 7)");
    let w = world(&source);
    for target in TARGETS {
        let options = Options {
            protocol: ResultProtocol::Words,
            ..Options::new(target)
        };
        let artifact = vc::compile(&w, "main", &options).unwrap();
        let count = artifact
            .lir
            .iter()
            .find(|(n, _)| n == "count")
            .map(|(_, rows)| rows)
            .unwrap();
        assert!(
            !count.iter().any(|r| matches!(r, vc::Lir::Call { .. })),
            "the tail self call must not be a call"
        );
        if let Some(prefix) = runner(target) {
            assert_eq!(render(&w, &execute(&artifact, &prefix)), "30000007");
        }
    }
}

/// Milestone 8: generic functions by monomorphization, region blocks and
/// region-polymorphic functions over the program heap.
#[test]
fn generics_and_regions_agree_with_the_evaluator() {
    for (name, expected) in [
        (
            "generics.ves",
            "(4, true, 9, Pair { a: 20, b: 10 }, 2, true, (3, 4))",
        ),
        (
            "regions.ves",
            "(4, Point { x: 11, y: 2 }, 10, (true, false), 48, 12, 7)",
        ),
        ("region_exhausted.ves", "region"),
        ("region_unit.ves", "false"),
        ("region_fail.ves", "(-1, 8, -1, 8)"),
        // Writes to index 99 land in cell 15 (`syntax_slot`'s catch-all).
        ("syntax_pages.ves", "(30, 990, 0, true)"),
    ] {
        let with_cranelift = name == "generics.ves";
        if name != "generics.ves" {
            // Only the Linux ELF executable maps the region heap.
            let w = world(&fixture(name));
            for target in [Target::AARCH64_MACOS, Target::X86_64_WINDOWS] {
                assert!(
                    matches!(
                        vc::compile(&w, "main", &Options::new(target)),
                        Err(VerifiedError::Target(e)) if e.contains("program heap")
                    ),
                    "{name} {target:?}"
                );
            }
        }
        assert_eq!(
            differential_with(&fixture(name), with_cranelift),
            expected,
            "{name}"
        );
    }
}

// --- Milestone 7 (abort-only `Fail[E]`) ---

/// `?`, `Fail::raise` and non-resuming `handle Fail[E]` agree with the
/// evaluator and Cranelift on every target, including implicit
/// `ArithOverflow` and division by zero caught by handlers and `inout`
/// writes made before a failure.
#[test]
fn fail_effects_agree_with_the_evaluator_and_cranelift() {
    for (name, expected) in [
        ("fail_try.ves", "(1012, -3, -7, 700100)"),
        ("fail_implicit.ves", "(210, 0, 1000000001, 7, 42, -2, -99)"),
    ] {
        assert_eq!(differential(&fixture(name)), expected, "{name}");
    }
}

/// A user error type that can reach the entry has no process abort to map
/// to; only `ArithOverflow` and `RuntimeError` abort. Non-`Fail` handlers
/// stay refused.
#[test]
fn unhandled_user_failures_at_the_entry_are_typed_errors() {
    rejects(
        "copy enum E { A } fn main() -> i64 eff [Fail[E]] { let r: Result[i64, E] = Err(E::A); r? }",
        Unsupported::Effect,
    );
    rejects(
        "copy enum E { A } fn f() -> i64 eff [Fail[E]] { Fail::raise(E::A) } fn main() -> i64 eff [Fail[E]] { f() }",
        Unsupported::Effect,
    );
    // The entry aborts rather than returning a status, so it may not be
    // called (here under a handler) when it can fail.
    rejects(
        "fn main() -> u8 eff [Fail[ArithOverflow]] { let x: u8 = 200; let y = x + x; if y == 0 { handle Fail[ArithOverflow] in { main() } with { return v => v, raise(_, _) => 1, } } else { y } }",
        Unsupported::Effect,
    );
    // Handled, the same program compiles.
    let w = world(
        "copy enum E { A } fn f() -> i64 eff [Fail[E]] { Fail::raise(E::A) } fn main() -> i64 { handle Fail[E] in { f() } with { return v => v, raise(_, _) => 3, } }",
    );
    vc::compile(&w, "main", &Options::new(Target::X86_64_LINUX)).unwrap();
}

/// A raise clause that names its continuation but leaves it unused is
/// non-resuming even when it holds a nested handler (whose IR alias map lists
/// every name in scope). Capturing the continuation there is a checker
/// error (E0404), so the checker already refuses that case.
#[test]
fn nested_handlers_in_raise_clauses_capture_only_their_free_names() {
    differential(
        "copy enum E { A } copy enum G { B } fn f() -> i64 eff [Fail[E]] { Fail::raise(E::A) } fn g() -> i64 eff [Fail[G]] { Fail::raise(G::B) } fn main() -> i64 { handle Fail[E] in { f() } with { return v => v, raise(_, k) => handle Fail[G] in { g() } with { return w => w, raise(_, j) => 5, }, } }",
    );
}

/// Programs without handlers compile as before: no trap scope and no status
/// word. A function that passes a caught implicit failure on sets a scope
/// after its prologue and restores the default at its end.
#[test]
fn trap_scopes_only_where_implicit_failures_are_caught() {
    let scoped = |source: &str| {
        let w = world(source);
        let a = vc::compile(&w, "main", &Options::new(Target::X86_64_LINUX)).unwrap();
        a.lir
            .into_iter()
            .filter(|(_, rows)| rows.iter().any(|r| matches!(r, vc::Lir::Scope { .. })))
            .map(|(n, _)| n)
            .collect::<Vec<_>>()
    };
    for name in [
        "overflow.ves",
        "divide.ves",
        "fibonacci.ves",
        "fail_try.ves",
    ] {
        assert!(scoped(&fixture(name)).is_empty(), "{name}");
    }
    let names = scoped(&fixture("fail_implicit.ves"));
    for f in ["bump", "divide"] {
        assert!(names.iter().any(|n| n == f), "{f} in {names:?}");
    }
    let w = world(&fixture("fail_implicit.ves"));
    let a = vc::compile(&w, "main", &Options::new(Target::X86_64_LINUX)).unwrap();
    let (_, bump) = a.lir.iter().find(|(n, _)| n == "bump").unwrap();
    let scopes: Vec<_> = bump
        .iter()
        .filter_map(|r| match r {
            vc::Lir::Scope { ovf, div0 } => Some((*ovf, *div0)),
            _ => None,
        })
        .collect();
    let ovf = vc::TrapKind::Overflow.label();
    let div0 = vc::TrapKind::DivisionByZero.label();
    assert_eq!(scopes.len(), 2, "{bump:?}");
    assert!(scopes[0].0 != ovf && scopes[0].1 == div0);
    assert_eq!(scopes[1], (ovf, div0));
    assert!(matches!(bump.last(), Some(vc::Lir::Scope { .. })));
}

/// Milestone 11: tail-resumptive user-effect handlers by evidence passing,
/// and named functions passed as function values.
#[test]
fn tail_resumptive_handlers() {
    let mem = "linear effect Mem {\n    fn read(index: i64) -> i64;\n    fn write(index: i64, value: i64);\n}\n";
    // A capture of the handler, an operation in a callee, a generic
    // function that takes the body as a function value.
    assert_eq!(
        differential(&format!(
            "{mem}fn work() -> i64 eff [Mem] {{ Mem::write(1, 5); wrapping_add(Mem::read(1), 1) }}\nfn run[T: mod copy](sink consume: fn() -> T eff [Mem]) -> T {{ let base = 10; handle Mem in {{ consume() }} with {{ return value => value, read(i, resume) => resume(wrapping_add(base, i)), write(i, value, resume) => {{ let ignored = wrapping_add(i, value); resume(()) }}, }} }}\nfn main() -> i64 {{ run(work) }}"
        )),
        "12"
    );
    // Recursion through the effect, a `_` argument, and the `return`
    // clause (Cranelift's effect specialization does not take recursion).
    assert_eq!(
        differential_with(
            &format!(
                "{mem}fn count(n: i64, acc: i64) -> i64 eff [Mem] {{ if n == 0 {{ acc }} else {{ count(wrapping_sub(n, 1), wrapping_add(acc, Mem::read(n))) }} }}\nfn main() -> (i64, i64) {{ let k = 3; let r = handle Mem in {{ count(4, 0) }} with {{ return v => wrapping_mul(v, 2), read(i, resume) => resume(wrapping_mul(i, k)), write(_, _, resume) => resume(()), }}; (r, k) }}"
            ),
            false
        ),
        "(60, 3)"
    );
    // A clause runs in the handler's context: its failure skips the
    // `handle Fail` around the operation.
    assert_eq!(
        differential(
            "effect Boom {\n    fn boom(x: i64) -> i64;\n}\nfn inner() -> i64 eff [Boom, Fail[ArithOverflow]] { handle Fail[ArithOverflow] in { Boom::boom(1) } with { return v => v, raise(e, r) => 100, } }\nfn main() -> i64 { handle Fail[ArithOverflow] in { handle Boom in { inner() } with { return v => v, boom(x, resume) => resume(9223372036854775807 + x), } } with { return v => v, raise(e, r) => 200, } }"
        ),
        "200"
    );
    // ... while a failure in the body is still caught there.
    assert_eq!(
        differential(
            "effect Boom {\n    fn boom(x: i64) -> i64;\n}\nfn inner() -> i64 eff [Boom, Fail[ArithOverflow]] { handle Fail[ArithOverflow] in { Boom::boom(1) + 9223372036854775807 } with { return v => v, raise(e, r) => 100, } }\nfn main() -> i64 { handle Fail[ArithOverflow] in { handle Boom in { inner() } with { return v => v, boom(x, resume) => resume(x + 1), } } with { return v => v, raise(e, r) => 200, } }"
        ),
        "100"
    );
    // A clause that does not resume in tail position is refused.
    let w = world(
        "effect Boom {\n    fn boom(x: i64) -> i64;\n}\nfn main() -> i64 { handle Boom in { wrapping_add(Boom::boom(1), 1) } with { return v => v, boom(x, resume) => wrapping_add(resume(x), 1), } }",
    );
    let err = vc::compile(&w, "main", &Options::new(TARGETS[0])).unwrap_err();
    assert!(err.to_string().contains("tail-resumptive"), "{err}");
}
