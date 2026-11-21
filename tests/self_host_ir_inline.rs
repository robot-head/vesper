//! Self-hosting SH3 lane 11 (#1250): `@vesper/ir-inline` against
//! `src/ir_inline.rs` over the corpus.
//!
//! Every corpus world is lowered the way the native path lowers it with
//! `optimize` on (`inline_words::native_program`), then inlined by Rust and
//! by Vesper with the default budget:
//!
//! - Decisions: the whole `Report` (sites, every callee's inlined and
//!   refused counts by reason, partial inlinings, folded sites, growth,
//!   `inlined_into`, literal constants) and every function's size and cost
//!   after inlining.
//! - Programs: the inlined program as canonical text (`inline_words::dump`),
//!   Rust's against the package's `inline_dump`.
//! - The decoder: the input program read back as the same text.
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/inline_words.rs"]
mod inline_words;

use check_harness::*;
use inline_words::*;
use std::sync::OnceLock;
use vesper::ir::Program;
use vesper::ir_inline::Budget;

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-ir-inline"))
}

/// The package's one entry, compiled once per test binary (the optimized
/// JIT compile takes minutes in a debug build, almost all of it in
/// `ir_opt::optimize`).
fn entry() -> &'static Entry {
    static E: OnceLock<Entry> = OnceLock::new();
    E.get_or_init(|| package().entry("inline_entry", true))
}

const MODE_REPORT: u64 = 0;
const MODE_DUMP: u64 = 1;
const MODE_INPUT: u64 = 2;
const MODE_PROGRAM: u64 = 3;

fn with_mode(mode: u64, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 8);
    out.extend(mode.to_le_bytes());
    out.extend(bytes);
    out
}

/// Every `n`-th corpus world is prepared (`SELF_HOST_IR_INLINE_STEP`,
/// default 8: lowering plus `ir_opt` over the whole corpus takes most of an
/// hour in a debug build; set it to 1 for the full corpus).
const DEFAULT_STEP: usize = 8;

/// One corpus world's native-path program, or `None` when lowering or
/// `ir_opt` refuses it (the native path then never inlines it either).
struct Prepared {
    name: String,
    world: &'static vesper::check::World,
    program: Program,
}

fn prepared() -> &'static [Prepared] {
    static P: OnceLock<Vec<Prepared>> = OnceLock::new();
    let only = std::env::var("SELF_HOST_IR_INLINE_ONLY").ok();
    P.get_or_init(|| {
        let quiet = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let mut out = vec![];
        for c in corpus().iter().step_by(step(DEFAULT_STEP)) {
            if only.as_ref().is_some_and(|o| !c.name.contains(o.as_str())) {
                continue;
            }
            let r = std::panic::catch_unwind(|| native_program(&c.world));
            if let Ok(Some(program)) = r {
                out.push(Prepared {
                    name: c.name.clone(),
                    world: &c.world,
                    program,
                });
            }
        }
        std::panic::set_hook(quiet);
        out
    })
}

fn trace() -> bool {
    std::env::var_os("SELF_HOST_IR_INLINE_TRACE").is_some()
}

/// `SELF_HOST_IR_INLINE_STEP`, or `default`.
fn step(default: usize) -> usize {
    std::env::var("SELF_HOST_IR_INLINE_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

/// Sorts each function's `local` lines (the package keeps locals in
/// insertion order, Rust in name order).
fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut locals: Vec<&str> = vec![];
    let flush = |out: &mut String, locals: &mut Vec<&str>| {
        locals.sort_unstable();
        for l in locals.drain(..) {
            out.push_str(l);
            out.push('\n');
        }
    };
    for line in text.lines() {
        if line.starts_with("local ") {
            locals.push(line);
            continue;
        }
        if line.starts_with("value ") || line.starts_with("block ") || line.starts_with("fn ") {
            flush(&mut out, &mut locals);
        }
        out.push_str(line);
        out.push('\n');
    }
    flush(&mut out, &mut locals);
    out
}

fn rust_inline(c: &Prepared) -> (vesper::ir_inline::Report, Program) {
    let mut program = c.program.clone();
    let report = vesper::ir_inline::inline(c.world, &mut program, &Budget::default());
    (report, program)
}

#[test]
fn decoder_round_trips_the_corpus() {
    let t0 = std::time::Instant::now();
    let entry = entry();
    if trace() {
        eprintln!("package {:.1} s", t0.elapsed().as_secs_f64());
        let _ = prepared();
        eprintln!("prepared {:.1} s", t0.elapsed().as_secs_f64());
    }
    let mut bad = 0;
    let mut n = 0;
    for c in prepared() {
        if trace() {
            eprintln!("decode {}", c.name);
        }
        let input = inline_input(c.world, &c.program, &Budget::default());
        let rust = normalize(&dump(&input.tables, &c.program));
        let vesper = normalize(&entry.text(&with_mode(MODE_INPUT, &input.bytes)));
        n += 1;
        if let Some(d) = first_difference(&rust, &vesper) {
            bad += 1;
            if bad <= 5 {
                eprintln!("decode: {} differs at {d}", c.name);
            }
        }
    }
    eprintln!("decode: {bad} of {n} programs differ");
    assert!(n > 25, "only {n} corpus programs");
    assert_eq!(bad, 0);
}

#[test]
fn decisions_match_rust() {
    let entry = entry();
    let (mut bad, mut n, mut sites, mut refused) = (0, 0, 0, 0);
    for c in prepared() {
        let input = inline_input(c.world, &c.program, &Budget::default());
        let (report, program) = rust_inline(c);
        let rust = Decoded::of_rust(c.world, &report, &program);
        let vesper = decode_report(
            &entry.words(&with_mode(MODE_REPORT, &input.bytes)),
            &input.tables,
        );
        n += 1;
        sites += rust.sites;
        refused += rust
            .refused
            .values()
            .flat_map(|m| m.values())
            .sum::<usize>();
        if rust != vesper {
            bad += 1;
            if bad <= 5 {
                eprintln!("report: {} differs", c.name);
                let r = format!("{rust:#?}");
                let v = format!("{vesper:#?}");
                if let Some(d) = first_difference(&r, &v) {
                    eprintln!("  {d}");
                }
            }
        }
    }
    eprintln!("decisions: {bad} of {n} programs differ ({sites} sites inlined, {refused} refused)");
    assert!(sites > 500, "only {sites} inlined sites");
    assert_eq!(bad, 0);
}

#[test]
fn inlined_programs_match_rust() {
    let entry = entry();
    let (mut bad, mut n) = (0, 0);
    for c in prepared() {
        let input = inline_input(c.world, &c.program, &Budget::default());
        let (_, program) = rust_inline(c);
        let rust = normalize(&dump(&input.tables, &program));
        let vesper = normalize(&entry.text(&with_mode(MODE_DUMP, &input.bytes)));
        if let Some(dir) = std::env::var_os("SELF_HOST_IR_INLINE_DUMPS") {
            let base = std::path::Path::new(&dir).join(c.name.replace('/', "_"));
            std::fs::write(base.with_extension("rust"), &rust).unwrap();
            std::fs::write(base.with_extension("vesper"), &vesper).unwrap();
            std::fs::write(
                base.with_extension("input"),
                normalize(&dump(&input.tables, &c.program)),
            )
            .unwrap();
        }
        n += 1;
        if let Some(d) = first_difference(&rust, &vesper) {
            bad += 1;
            if bad <= 5 {
                eprintln!("program: {} differs at {d}", c.name);
            }
        }
    }
    eprintln!("programs: {bad} of {n} inlined programs differ");
    assert_eq!(bad, 0);
}

/// The words the hybrid's `inline` hook reads: the package's inlined
/// program decoded back into an `ir::Program` (`decode_program`) is
/// `Debug`-equal to Rust's, and so is the report's public part.
#[test]
fn decoded_programs_equal_rust() {
    let entry = entry();
    let (mut bad, mut n) = (0, 0);
    for c in prepared() {
        let input = inline_input(c.world, &c.program, &Budget::default());
        let (report, program) = rust_inline(c);
        let words = entry.words(&with_mode(MODE_PROGRAM, &input.bytes));
        let (vreport, vprogram) = decode_program(&words, &input.tables, &c.program);
        n += 1;
        let rust = format!("{report:?}\n{program:#?}");
        let vesper = format!("{vreport:?}\n{vprogram:#?}");
        // `Report::contents` is private and the hook leaves it empty.
        let rust = strip_contents(&rust);
        let vesper = strip_contents(&vesper);
        if let Some(d) = first_difference(&rust, &vesper) {
            bad += 1;
            if bad <= 5 {
                eprintln!("decoded: {} differs at {d}", c.name);
            }
        }
    }
    eprintln!("decoded: {bad} of {n} programs differ");
    assert!(n > 25, "only {n} corpus programs");
    assert_eq!(bad, 0);
}

/// The report's `Debug` text without its private `contents` map.
fn strip_contents(text: &str) -> String {
    match (text.find(", contents: "), text.find(" }\n")) {
        (Some(a), Some(b)) if a < b => format!("{}{}", &text[..a], &text[b..]),
        _ => text.to_owned(),
    }
}

/// Smaller budgets reach the growth caps and the single-site rule.
#[test]
fn tight_budgets_match_rust() {
    let entry = entry();
    let budgets = [
        Budget {
            small_callee: 24,
            single_site_callee: 60,
            caller_growth: 120,
            program_growth: 600,
            callee_growth: 90,
            caller_handles: 1,
        },
        Budget::none(),
    ];
    let mut bad = 0;
    for budget in &budgets {
        for c in prepared().iter().step_by(3) {
            let input = inline_input(c.world, &c.program, budget);
            let mut program = c.program.clone();
            let report = vesper::ir_inline::inline(c.world, &mut program, budget);
            let rust = Decoded::of_rust(c.world, &report, &program);
            let vesper = decode_report(
                &entry.words(&with_mode(MODE_REPORT, &input.bytes)),
                &input.tables,
            );
            if rust != vesper {
                bad += 1;
                if bad <= 5 {
                    eprintln!("budget {budget:?}: {} differs", c.name);
                }
            }
        }
    }
    assert_eq!(bad, 0);
}

/// ABBA: `ir_inline::inline` against the package (input encoded
/// beforehand, and with its encoding).
#[test]
#[ignore = "timing; CHECK_HARNESS_SPEED=1"]
fn inline_speed() {
    if !speed() {
        return;
    }
    let entry = entry();
    let cases = prepared();
    let inputs: Vec<Vec<u8>> = cases
        .iter()
        .map(|c| {
            with_mode(
                MODE_REPORT,
                &inline_input(c.world, &c.program, &Budget::default()).bytes,
            )
        })
        .collect();
    abba(
        "inline, Vesper alone (input encoded beforehand)",
        &mut || {
            time(|| {
                for c in cases {
                    std::hint::black_box(rust_inline(c));
                }
            })
        },
        &mut || {
            time(|| {
                for i in &inputs {
                    std::hint::black_box(entry.words(i));
                }
            })
        },
    );
    abba(
        "inline with encoding",
        &mut || {
            time(|| {
                for c in cases {
                    std::hint::black_box(rust_inline(c));
                }
            })
        },
        &mut || {
            time(|| {
                for c in cases {
                    let input = inline_input(c.world, &c.program, &Budget::default());
                    std::hint::black_box(entry.words(&with_mode(MODE_REPORT, &input.bytes)));
                }
            })
        },
    );
}

/// How long the package's entries take to compile (JIT, optimizer on and
/// off).
#[test]
#[ignore = "timing"]
fn compile_time() {
    let t = std::time::Instant::now();
    let _ = package();
    eprintln!("load + check {:.1} s", t.elapsed().as_secs_f64());
    let program = vesper::ir::lower_world(&package().world);
    let mut rows: Vec<(usize, &String, usize, usize)> = program
        .functions
        .iter()
        .map(|(n, f)| {
            (
                f.blocks.len() * f.values.len(),
                n,
                f.blocks.len(),
                f.values.len(),
            )
        })
        .collect();
    rows.sort();
    for r in rows.iter().rev().take(25) {
        eprintln!("{:>10} {} blocks {} values {}", r.0, r.1, r.2, r.3);
    }
    if std::env::var_os("COMPILE_TIME_SIZES_ONLY").is_some() {
        let mut times = vec![];
        for (n, f) in &program.functions {
            let mut one = Program::default();
            one.functions.insert(n.clone(), f.clone());
            let t = std::time::Instant::now();
            let _ = vesper::ir_opt::optimize(&package().world, &mut one);
            times.push((t.elapsed().as_secs_f64(), n.clone()));
        }
        times.sort_by(|a, b| b.0.total_cmp(&a.0));
        let total: f64 = times.iter().map(|t| t.0).sum();
        eprintln!("ir_opt total {total:.1} s");
        for (t, n) in times.iter().take(30) {
            eprintln!("ir_opt {t:.2} s {n}");
        }
        return;
    }
    for opt in [false, true] {
        let t = std::time::Instant::now();
        let _ = package().entry("inline_entry", opt);
        eprintln!("compile optimize={opt} {:.1} s", t.elapsed().as_secs_f64());
    }
}
