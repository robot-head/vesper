//! Self-hosting SH2 lane 7 (#1179), part 2: expression typing in Vesper
//! (`packages/vesper-check-core-typing/src/exprs.ves`) against the Rust
//! checker's expression types (`World::types`), over every checked corpus
//! world (lane 4's harness).
//!
//! The Vesper checker covers a growing subset of `Checker::expr_expected`.
//! It reports a function it cannot check yet as skipped, and the test
//! prints how many functions it checked. Every checked function must give
//! exactly the types the Rust checker recorded for its expressions.

#[path = "common/check_harness.rs"]
mod check_harness;

#[path = "common/core_typing_words.rs"]
mod core_typing_words;

use check_harness::*;
use core_typing_words::*;
use std::sync::OnceLock;
use vesper::check::World;

fn package() -> &'static Package {
    static PACKAGE: OnceLock<Package> = OnceLock::new();
    PACKAGE.get_or_init(|| Package::load("packages/vesper-check-core-typing"))
}

#[test]
fn typed_constant_pattern_checks_the_scrutinee() {
    use vesper::ffi_lowering::{Handler, Imports, plan_probe};

    let mut imports = Imports::new();
    imports
        .add(
            r#"sys library Flags (
        target = "x86_64-unknown-linux-gnu", abi = "sysv64", link = "static", name = "flags",
        artifact = "blake3:0000000000000000000000000000000000000000000000000000000000000000"
    ) { const O_CREAT: i32 = 64; }"#,
            Handler::Direct,
        )
        .unwrap();
    let source = "fn probe(x: u8) -> i32 { match x { Flags::O_CREAT => 1, 64 => 2, _ => 0 } }";
    let mut module = vesper::parser::parse(source).module;
    let generated = plan_probe::generated(&imports).unwrap();
    assert!(
        plan_probe::rewrite(&mut module, &imports, &generated, None)
            .error
            .is_none()
    );
    let (world, errors) = World::collect(&module);
    assert!(errors.is_empty());
    let result = package()
        .entry("core_typing_exprs", true)
        .text(&exprs_input(&world, &module));
    let start = source.find("Flags::O_CREAT").unwrap();
    let want = format!("D E0200 {start} {}", start + "Flags::O_CREAT".len());
    assert_eq!(
        result
            .lines()
            .filter(|l| l.starts_with("D "))
            .collect::<Vec<_>>(),
        [want]
    );
}

/// A `break` whose value breaks out of the same loop keeps the inner
/// break's result: the frame is read again after the value is checked.
#[test]
fn nested_break_keeps_the_inner_result() {
    let source = "fn f() -> u32 { loop { break { break 1u32; }; } }";
    let module = vesper::parser::parse(source).module;
    let checked = vesper::check::check(&module);
    assert!(checked.diagnostics.is_empty());
    let world = checked.world;
    let text = package()
        .entry("core_typing_exprs", true)
        .text(&exprs_input(&world, &module));
    let (v, checked, skipped) = vesper_types(&text);
    assert_eq!((checked.len(), skipped), (1, 0));
    assert_eq!(first_difference(&rust_types(&world, &checked), &v), None);
}

#[test]
fn expression_types_match_rust() {
    let f = package().entry("core_typing_exprs", true);
    let (mut total, mut skipped, mut rows) = (0, 0, 0);
    let mut bad = 0;
    let mut shown = 0;
    for c in corpus() {
        let text = f.text(&exprs_input(&c.world, &c.module));
        let (v, checked, s) = vesper_types(&text);
        total += checked.len();
        skipped += s;
        let r = rust_types(&c.world, &checked);
        rows += r.lines().filter(|l| l.starts_with('T')).count();
        if let Some(d) = first_difference(&r, &v) {
            bad += 1;
            if shown < 8 {
                shown += 1;
                eprintln!("exprs: {} differs at {d}", c.name);
            }
        }
    }
    eprintln!(
        "exprs: {bad} of {} worlds differ; {total} functions checked, {skipped} skipped, \
         {rows} expression types",
        corpus().len()
    );
    assert_eq!(bad, 0);
    // Coverage floor: a checker that skipped everything would agree too.
    // Relative, because the corpus is smaller under Bazel (its data globs)
    // than under Cargo: a quarter of the functions, with types in each.
    assert!(
        total * 4 > total + skipped && rows > total,
        "{total} functions checked, {skipped} skipped, {rows} types"
    );
}

/// Every checked function's result (`Checker::results`, the declared result
/// refined by its body) is the one the Rust checker recorded in
/// `World::function_results`.
#[test]
fn handler_effects_match_rust() {
    let f = package().entry("core_typing_exprs", true);
    let (mut total, mut bad) = (0, 0);
    for c in corpus() {
        let v = vesper_handler_effects(&f.text(&exprs_input(&c.world, &c.module)));
        let spans: Vec<(usize, usize)> = v.iter().map(|(span, _)| *span).collect();
        let r = rust_handler_effects(&c.world, &spans);
        total += v.len();
        for (a, b) in r.iter().zip(&v) {
            if a != b {
                bad += 1;
                if bad <= 8 {
                    eprintln!(
                        "handlers: {} {:?}: rust {} vesper {}",
                        c.name, a.0, a.1, b.1
                    );
                }
            }
        }
    }
    eprintln!("handlers: {bad} of {total} handler effects differ");
    assert_eq!(bad, 0);
    assert!(total > 0);
}

/// Every checked function's operations that may raise `Fail[RuntimeError]`
/// or `Fail[ArithOverflow]` (opfacts.ves) are the ones the Rust checker
/// recorded in `World::partial_ops` and `World::overflow_ops`.
#[test]
fn failure_ops_match_rust() {
    const SHOWN: usize = 12;
    let f = package().entry("core_typing_exprs", true);
    let (mut functions, mut ops, mut computed, mut bad) = (0, 0, 0, 0);
    for c in corpus() {
        let text = f.text(&exprs_input(&c.world, &c.module));
        for (span, v) in vesper_failure_ops(&text) {
            let r = rust_failure_ops(&c.world, span);
            functions += 1;
            ops += r.len();
            computed += v.len();
            for (side, op) in r
                .symmetric_difference(&v)
                .map(|op| (if r.contains(op) { "rust" } else { "vesper" }, op))
            {
                bad += 1;
                if bad <= SHOWN {
                    eprintln!(
                        "failure ops: {} fn {span:?}: only {side} has {} {:?}",
                        c.name, op.0, op.1
                    );
                }
            }
        }
    }
    eprintln!(
        "failure ops: {bad} differ over {functions} functions, \
         {ops} Rust and {computed} Vesper operations"
    );
    assert_eq!(bad, 0);
    assert!(ops > 0 && computed == ops);
}

#[test]
fn function_results_match_rust() {
    let f = package().entry("core_typing_exprs", true);
    let (mut total, mut bad) = (0, 0);
    for c in corpus() {
        let v = vesper_results(&f.text(&exprs_input(&c.world, &c.module)));
        let spans: Vec<(usize, usize)> = v.iter().map(|(span, _)| *span).collect();
        let r = rust_results(&c.world, &spans);
        total += v.len();
        for (a, b) in r.iter().zip(&v) {
            if a != b {
                bad += 1;
                if bad <= 8 {
                    eprintln!("results: {} {:?}: rust {} vesper {}", c.name, a.0, a.1, b.1);
                }
            }
        }
    }
    eprintln!("results: {bad} of {total} function results differ");
    assert_eq!(bad, 0);
    assert!(total > 0);
}
