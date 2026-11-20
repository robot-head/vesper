//! Compiler capacity (SH3 lane 7, #1246): native code has no fixed cap on
//! function instances, polymorphic recursion is reported with its cycle,
//! and word entries take up to eight words.
//!
//! The large program is a generated package (`write_large_package`): 5,200
//! chained functions in ten modules (one file would pass the parser's
//! token limit), more than 5,000 instances.
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

/// Modules of the generated package, and functions per module.
const MODULES: u32 = 10;
const PER_MODULE: u32 = 520;

/// Writes the generated package (#1246 repro) under `dir`: `MODULES` files
/// of `PER_MODULE` plain functions each, chained so that `main` reaches
/// every one.
/// `main` returns the sum of `1..=MODULES * PER_MODULE` plus `MODULES`.
fn write_large_package(dir: &std::path::Path) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Loom.toml"),
        "[package]\nname = \"@test/capacity\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
    )
    .unwrap();
    let mut lib = String::new();
    let mut body = String::from("    let mut n: u64 = 0;\n");
    for m in 0..MODULES {
        let mut s = String::new();
        for i in 0..PER_MODULE {
            let k = m * PER_MODULE + i + 1;
            let next = if i + 1 == PER_MODULE {
                "1".to_string()
            } else {
                format!("f{}(x)", i + 1)
            };
            let name = if i == 0 {
                format!("pub fn m{m}_start")
            } else {
                format!("fn f{i}")
            };
            s.push_str(&format!(
                "{name}(x: u64) -> u64 {{ wrapping_add({next}, {k}) }}\n"
            ));
        }
        std::fs::write(dir.join(format!("src/m{m}.ves")), s).unwrap();
        lib.push_str(&format!("use self::m{m}::{{m{m}_start}};\n"));
        body.push_str(&format!("    n = wrapping_add(n, m{m}_start(0));\n"));
    }
    lib.push_str(&format!("pub fn main() -> u64 {{\n{body}    n\n}}\n"));
    std::fs::write(dir.join("src/lib.ves"), lib).unwrap();
}

#[test]
fn more_than_five_thousand_instances_compile() {
    let dir = std::env::temp_dir().join(format!("vesper-capacity-{}", std::process::id()));
    write_large_package(&dir);
    let assembled = vesper::packages::load(&dir).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(assembled.ok(), "{:?}", assembled.diagnostics);
    let c = check::check(&assembled.module);
    assert!(c.diagnostics.is_empty(), "{:?}", &c.diagnostics[..1]);
    let count = native::instance_count(&c.world, "main").unwrap();
    assert!(count > 5_000, "{count} instances");
    let total = u64::from(MODULES * PER_MODULE);
    let expected = total * (total + 1) / 2 + u64::from(MODULES);
    let value = native::jit_with_options(&c.world, "main", false).unwrap();
    assert_eq!(value.to_string(), expected.to_string());
}

#[test]
fn polymorphic_recursion_reports_its_cycle() {
    // The checker accepts this (the evaluator runs it); native code would
    // need an instance per depth. The old cap reported only "exceeds 4096
    // function instances" after building 4,096 of them.
    let c = accepted(
        "fn grow[T](sink x: T, n: u64) -> T {\n\
             if n == 0 { x } else { let (y, b) = hop((x, true), wrapping_sub(n, 1)); y }\n\
         }\n\
         fn hop[T](sink x: T, n: u64) -> T { grow(x, n) }\n\
         fn main() -> u64 { let s: u64 = 7; grow(s, 3) }\n",
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
