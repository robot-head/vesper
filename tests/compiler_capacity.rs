//! Compiler capacity (SH3 lane 7, #1246): native code has no fixed cap on
//! function instances, polymorphic recursion is reported with its cycle,
//! and word entries take up to eight words.
//!
//! The large program is generated (`large_program`): a generic function
//! doubling into two instances per level plus a chain of plain functions,
//! more than 5,000 instances from a few kilobytes of source.
use vesper::{check, native, parser};

fn accepted(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    let shown: Vec<_> = c
        .diagnostics
        .iter()
        .map(|d| format!("{}: {} at `{}`", d.code, d.message, &source[d.span.clone()]))
        .collect();
    assert!(c.diagnostics.is_empty(), "{shown:#?}");
    c
}

/// Levels of the doubling generic: level `k` has `2^k` instances.
const LEVELS: u32 = 12;
/// Plain functions in the chain.
const CHAIN: u32 = 1_000;

/// `main` reaches `2^LEVELS - 1` instances of the `lvl_*` generics (each
/// level instantiates the next at `(T, bool)` and `(bool, T)`) and `CHAIN`
/// plain functions, and returns `2^(LEVELS - 1) + CHAIN`.
fn large_program() -> String {
    let mut s = String::new();
    for k in 0..LEVELS - 1 {
        s.push_str(&format!(
            "fn lvl_{k}[T](x: T) -> u64 {{ lvl_{n}((x, true)) + lvl_{n}((true, x)) }}\n",
            n = k + 1
        ));
    }
    s.push_str(&format!("fn lvl_{}[T](x: T) -> u64 {{ 1 }}\n", LEVELS - 1));
    for i in 0..CHAIN {
        if i + 1 == CHAIN {
            s.push_str(&format!("fn chain_{i}() -> u64 {{ 1 }}\n"));
        } else {
            s.push_str(&format!("fn chain_{i}() -> u64 {{ chain_{}() + 1 }}\n", i + 1));
        }
    }
    s.push_str("fn main() -> u64 { lvl_0(true) + chain_0() }\n");
    s
}

#[test]
fn more_than_five_thousand_instances_compile() {
    let source = large_program();
    let c = accepted(&source);
    let count = native::instance_count(&c.world, "main").unwrap();
    assert!(count > 5_000, "{count} instances");
    let expected = (1u64 << (LEVELS - 1)) + u64::from(CHAIN);
    let value = native::jit_with_options(&c.world, "main", false).unwrap();
    assert_eq!(value.to_string(), expected.to_string());
}

#[test]
fn polymorphic_recursion_reports_its_cycle() {
    let c = accepted(
        "fn grow[T](x: T, n: u64) -> u64 { if n == 0 { 0 } else { hop((x, true), n - 1) } }\n\
         fn hop[T](x: T, n: u64) -> u64 { grow(x, n) }\n\
         fn main() -> u64 { grow(true, 3) }\n",
    );
    let error = native::jit_with_options(&c.world, "main", false).unwrap_err();
    assert!(error.contains("polymorphic recursion"), "{error}");
    assert!(
        error.contains("grow -> hop -> grow") || error.contains("hop -> grow -> hop"),
        "{error}"
    );
}

/// `fN(w1, .., wN) = sum of i * wi`, so a swapped or dropped word changes
/// the result.
fn weighted(n: usize) -> String {
    let params: Vec<String> = (1..=n).map(|i| format!("w{i}: u64")).collect();
    let mut terms: Vec<String> = (1..=n).map(|i| format!("{i} * w{i}")).collect();
    terms.push("0".into());
    format!(
        "fn f{n}({}) -> u64 {{ {} }}\n",
        params.join(", "),
        terms.join(" + ")
    )
}

#[test]
fn word_entries_take_up_to_eight_words() {
    let source: String = (0..=9).map(weighted).collect();
    let c = accepted(&source);
    for optimize in [false, true] {
        for n in 0..=native::MAX_WORD_FUNCTION_ARGS {
            let f = native::WordFunction::compile(&c.world, &format!("f{n}"), optimize)
                .unwrap_or_else(|e| panic!("f{n}: {e}"));
            let args: Vec<u64> = (1..=n as u64).map(|i| 1000 + i * i).collect();
            let expected: u64 = args.iter().zip(1..).map(|(w, i)| i * w).sum();
            // SAFETY: every parameter is a `u64` word.
            assert_eq!(unsafe { f.call(&args) }.unwrap(), expected, "f{n}");
        }
        let error = native::WordFunction::compile(&c.world, "f9", optimize)
            .err()
            .expect("nine words are refused");
        assert!(error.contains("at most 8 words"), "{error}");
    }
}

/// Measures one package (`COMPILER_CAPACITY_PACKAGE`, a directory whose
/// root defines `check_all(in input: Str) -> u64`): check time, native
/// instance count, and JIT compile time unoptimized and optimized. The
/// report's table comes from release runs of this test
/// (`cargo test --release --test compiler_capacity -- --ignored --nocapture`).
#[test]
#[ignore = "measurement; set COMPILER_CAPACITY_PACKAGE"]
fn measure_package() {
    use std::time::Instant;
    let Ok(dir) = std::env::var("COMPILER_CAPACITY_PACKAGE") else {
        return;
    };
    let t = Instant::now();
    let assembled = vesper::packages::load(std::path::Path::new(&dir)).unwrap();
    assert!(assembled.ok(), "{:?}", assembled.diagnostics);
    let load = t.elapsed();
    let t = Instant::now();
    let c = check::check(&assembled.module);
    assert!(c.diagnostics.is_empty(), "{:?}", &c.diagnostics[..1]);
    let checked = t.elapsed();
    let t = Instant::now();
    let count = native::instance_count(&c.world, "check_all").unwrap();
    let counted = t.elapsed();
    let mut compiled = vec![];
    for optimize in [false, true] {
        let t = Instant::now();
        native::WordFunction::compile(&c.world, "check_all", optimize).unwrap();
        compiled.push(t.elapsed());
    }
    println!(
        "{dir}: load {load:.2?}, check {checked:.2?}, {count} instances (counted in {counted:.2?}), JIT --no-opt {:.2?}, JIT optimized {:.2?}",
        compiled[0], compiled[1]
    );
}
