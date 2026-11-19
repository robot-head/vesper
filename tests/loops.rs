//! `loop`, `for` over ranges, `break`, `return` and `let mut` (src/loops.rs):
//! the evaluator, the Cranelift JIT and both object targets agree, mode
//! violations are rejected, and the backends without loops refuse them with
//! typed errors. `scripts/test-aarch64.sh` runs the same fixtures as AArch64
//! executables under QEMU.
use std::path::Path;
use vesper::{check, native, parser, runtime::Runtime};

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/loops")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn world(source: &str) -> vesper::check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    c
}

fn evaluate(c: &vesper::check::Checked) -> String {
    let mut runtime = Runtime::new(&c.world);
    runtime.fuel = 200_000_000;
    runtime.run("main", vec![]).unwrap().to_string()
}

/// Evaluator and JIT agree, and both object targets build.
fn differential(source: &str) -> String {
    let c = world(source);
    let interpreted = evaluate(&c);
    let jit = native::jit(&c.world, "main").unwrap().to_string();
    assert_eq!(jit, interpreted);
    let (obj, _) = native::object(&c.world, "main").unwrap();
    assert!(!obj.is_empty());
    let (arm, _) =
        native::object_for_target(&c.world, "main", Some("aarch64-unknown-linux-gnu")).unwrap();
    assert_eq!(&arm[..4], b"\x7fELF");
    assert_eq!(u16::from_le_bytes([arm[18], arm[19]]), 183);
    jit
}

#[test]
fn ten_million_iterations_in_constant_stack() {
    let source = fixture("count.ves");
    // A recursive lowering would need ten million frames; the JIT runs the
    // loop as a back edge on the test thread's ordinary stack.
    let c = world(&source);
    assert_eq!(native::jit(&c.world, "main").unwrap().to_string(), "170");
    // The evaluator agrees on a smaller bound (its step budget is finite).
    let small = source.replace("10000000", "100000");
    assert_eq!(
        differential(&small),
        ((100_000u64 * 99_999 / 2) % 251).to_string()
    );
}

#[test]
fn nested_loops_break_values_and_early_returns() {
    // triangle 55, break values 63 + 63, early returns 21 + 1020, closure
    // 8, unit returns 99 + 7, inclusive range 6; 1342 % 256 = 62.
    assert_eq!(differential(&fixture("control.ves")), "62");
}

#[test]
fn for_over_ranges() {
    let out = differential(
        "fn main() -> (i64, i64, i64, i64) {
            let mut a = 0;
            for i in 0..10 {
                a = wrapping_add(a, i);
            }
            let mut b = 0;
            for i in 3..=3 {
                b = wrapping_add(b, i);
            }
            let mut c = 0;
            for i in 5..2 {
                c = wrapping_add(c, i);
            }
            let mut d = 0;
            for _ in -2..=2 {
                d = wrapping_add(d, 2);
            }
            (a, b, c, d)
        }",
    );
    assert_eq!(out, "(45, 3, 0, 10)");
}

#[test]
fn inclusive_range_to_the_type_maximum() {
    let out = differential(
        "fn main() -> i64 {
            let lo: u8 = 254;
            let hi: u8 = 255;
            let mut n = 0;
            for b in lo..=hi {
                let byte: u8 = b;
                n = wrapping_add(n, 1);
            }
            n
        }",
    );
    assert_eq!(out, "2");
}

#[test]
fn loop_value_without_break_is_never() {
    let out = differential(
        "fn pick(n: i64) -> i64 {
            let mut i = 0;
            loop {
                if i == n {
                    return wrapping_mul(i, 10);
                }
                i = wrapping_add(i, 1);
            }
        }
        fn main() -> i64 { pick(4) }",
    );
    assert_eq!(out, "40");
}

#[test]
fn rejected_programs() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/loops/reject");
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    assert!(entries.len() >= 7);
    for path in entries {
        let source = std::fs::read_to_string(&path).unwrap();
        // Each fixture's first line names the expected code: `// E0213: ...`.
        let code = source
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("// "))
            .and_then(|l| l.split(':').next())
            .unwrap_or_else(|| panic!("{}: no expected code", path.display()));
        let p = parser::parse(&source);
        assert!(
            p.diagnostics.is_empty(),
            "{}: {:?}",
            path.display(),
            p.diagnostics
        );
        let c = check::check(&p.module);
        assert!(
            c.diagnostics.iter().any(|d| d.code == code),
            "{}: expected {code}, got {:?}",
            path.display(),
            c.diagnostics
        );
    }
}

#[test]
fn backends_without_loops_refuse_them() {
    let c = world(&fixture("count.ves"));
    let lean = vesper::lean::extract(&c.world).unwrap_err();
    assert_eq!(lean, vesper::loops::LEAN_REFUSAL);
    let wasm = vesper::wasm_backend::compile(&c.world, "main", &Default::default()).unwrap_err();
    assert!(
        matches!(
            &wasm,
            vesper::wasm_backend::WasmError::Unsupported {
                construct: vesper::wasm_backend::Unsupported::Operation,
                detail,
                ..
            } if detail == vesper::loops::WASM_REFUSAL
        ),
        "{wasm:?}"
    );
    let verified = vesper::verified_codegen::compile(
        &c.world,
        "main",
        &vesper::verified_codegen::Options::new(vesper::verified_codegen::Target::X86_64_LINUX),
    );
    assert!(verified.is_err());
    let ebpf = vesper::ebpf_backend::compile_source(
        "copy enum XdpAction { Aborted, Drop, Pass, Tx, Redirect, }
        fn main() -> XdpAction {
            let mut i: u64 = 0;
            loop {
                if i == 3 {
                    break;
                }
                i = wrapping_add(i, 1);
            }
            XdpAction::Pass
        }",
        &Default::default(),
    )
    .unwrap_err();
    assert!(
        ebpf.to_string()
            .contains("loop is not supported on the eBPF target"),
        "{ebpf}"
    );
}

fn diagnostics(source: &str) -> Vec<String> {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    check::check(&p.module)
        .diagnostics
        .into_iter()
        .map(|d| d.code)
        .collect()
}

#[test]
fn for_pattern_shadow_returns_after_break() {
    let out = differential(
        "fn main() -> i64 {
            let i = 42;
            for i in 0..3 {
                break;
            }
            i
        }",
    );
    assert_eq!(out, "42");
}

#[test]
fn empty_range_joins_the_loop_exit() {
    // The range may be empty, so `x` may still be zero after the loop: the
    // division needs its failure effect.
    let codes = diagnostics(
        "fn main() -> i64 {
            let mut x = 0;
            for _ in 0..0 {
                x = 1;
                break;
            }
            1 / x
        }",
    );
    assert!(codes.iter().any(|c| c == "E0401"), "{codes:?}");
}

#[test]
fn inferred_closure_returns_must_agree() {
    let codes = diagnostics(
        "fn main() -> i64 {
            let f = |b: bool| {
                if b {
                    return true;
                }
                return 2;
            };
            f(true)
        }",
    );
    assert!(codes.iter().any(|c| c == "E0200"), "{codes:?}");
}

#[test]
fn optimized_early_return_of_a_forwarded_local() {
    assert_eq!(
        differential("fn main() -> i64 { let x = 42; return x; }"),
        "42"
    );
}

#[test]
fn functions_named_like_loop_words_stay_callable() {
    let out = differential(
        "fn for(x: i64) -> i64 { x }
        fn break() -> i64 { 3 }
        fn main() -> i64 { wrapping_add(break(), for(4)) }",
    );
    assert_eq!(out, "7");
}

#[test]
fn native_reclaims_boxes_across_loops() {
    let c = world(
        "copy struct P { x: i64 }
        fn main() -> i64 {
            let mut total = 0;
            for i in 0..3 {
                let b = Box::new(P { x: i });
                let Box(p) = b;
                total = wrapping_add(total, p.x);
            }
            total
        }",
    );
    assert_eq!(evaluate(&c), "3");
    // Box drops are elaborated over the loop (#1064, tests/loop_boxes.rs).
    vesper::fip::native_runtime::reset_counts();
    assert_eq!(native::jit(&c.world, "main").unwrap().to_string(), "3");
    assert_eq!(
        vesper::fip::native_runtime::alloc_count(),
        vesper::fip::native_runtime::free_count()
    );
}

#[test]
fn loops_pass_through_the_package_loader() {
    // A `for` pattern and loop bodies are scopes for the loader's renamer:
    // `i` in `main` stays the local, `sum` resolves to `util::sum`.
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/loops/package");
    let assembled = vesper::packages::load(&dir).unwrap();
    assert!(assembled.ok(), "{:?}", assembled.diagnostics);
    let c = check::check(&assembled.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    // 10 + 21 + 100 + 3
    assert_eq!(evaluate(&c), "134");
    assert_eq!(native::jit(&c.world, "main").unwrap().to_string(), "134");
}
