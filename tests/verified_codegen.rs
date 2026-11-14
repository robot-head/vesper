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
    if target == Target::AARCH64_LINUX && cfg!(target_os = "linux") {
        for q in ["qemu-aarch64", "qemu-aarch64-static"] {
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

/// Run an artifact built with `ResultProtocol::Words`.
fn execute(artifact: &Artifact, prefix: &[PathBuf]) -> Outcome {
    let dir = scratch();
    let exe = dir.join("program");
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
    }
    let mut cmd = match prefix.split_first() {
        Some((q, rest)) => {
            let mut c = Command::new(q);
            c.args(rest).arg(&exe);
            c
        }
        None => Command::new(&exe),
    };
    let out = cmd.output().expect("run the emitted executable");
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
        other => panic!("unexpected outcome {other:?}"),
    }
}

const TARGETS: [Target; 3] = [
    Target::X86_64_LINUX,
    Target::AARCH64_LINUX,
    Target::AARCH64_MACOS,
];

/// Compile for every target (with and without the IR optimizer), check the
/// output is deterministic, run what can run here, and compare with the
/// evaluator and Cranelift. Returns the rendered outcome.
fn differential(source: &str) -> String {
    let w = world(source);
    let expected = evaluate(source, &w);
    match cranelift(source, &w) {
        Some(jit) => assert_eq!(jit, expected, "Cranelift JIT disagrees with the evaluator"),
        None => assert!(
            !matches!(expected.as_str(), "overflow" | "div0"),
            "Cranelift JIT succeeded where the evaluator failed"
        ),
    }
    for target in TARGETS {
        for optimize in [true, false] {
            let options = Options {
                protocol: ResultProtocol::Words,
                optimize,
                ..Options::new(target)
            };
            let artifact = vc::compile(&w, "main", &options)
                .unwrap_or_else(|e| panic!("{target:?}: {e}\n{source}"));
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
        ("kinds.ves", "(241, 4147483647, 1, (-3, -1, true, false))"),
        ("overflow.ves", "overflow"),
        ("divide.ves", "div0"),
        ("unit.ves", "()"),
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
    rejects("fn main() -> f64 { 1.5 }", Unsupported::Float);
    rejects("fn main() -> i64 { let s = \"x\"; 1 }", Unsupported::String);
    rejects("fn main() -> Option[i64] { Some(1) }", Unsupported::Enum);
    rejects(
        "fn main() -> i64 { match true { true => 1, false => 0, } }",
        Unsupported::Match,
    );
    rejects(
        "fn id[T](sink x: T) -> T { x } fn main() -> i64 { id[i64](4) }",
        Unsupported::Generic,
    );
    rejects(
        "fn main() -> i64 { let f = |x: i64| -> i64 { x }; f(1) }",
        Unsupported::FunctionValue,
    );
    rejects(
        "linear effect Ask { fn ask() -> i64; } fn main() -> i64 { handle Ask in { Ask::ask() } with { return x => x, ask(k) => k(21), } }",
        Unsupported::Effect,
    );
    rejects(
        "fn main() -> i64 { let xs = (1, 2, 3); let i = 1; xs[i] }",
        Unsupported::Index,
    );
    rejects(
        "copy struct P { x: i64 } fn main() -> bool { let a = P { x: 1 }; a == a }",
        Unsupported::StructuralEquality,
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
    assert!(Target::from_triple("riscv64gc-unknown-linux-gnu").is_err());
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
    for (a, machine) in [(&x86, 62u16), (&arm, 183)] {
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
